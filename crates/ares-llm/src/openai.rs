//! OpenAI 兼容协议。
//!
//! 适用于 DeepSeek、豆包/火山方舟、Kimi、Qwen、OpenRouter、
//! 本地 vLLM 与 Ollama —— 它们都实现了 /chat/completions。

use crate::{CompletionRequest, CompletionResponse, Message, Provider, Role, ToolCall, Usage};
use ares_core::{AresError, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct OpenAiProvider {
    base_url: String,
    api_key: String,
    client: reqwest::Client,
    name: String,
}

impl OpenAiProvider {
    pub fn new(
        name: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            client: reqwest::Client::new(),
            name: name.into(),
        }
    }

    /// 把内部消息转换为 OpenAI 的 messages 数组。
    pub(crate) fn encode_messages(messages: &[Message]) -> Vec<Value> {
        messages
            .iter()
            .map(|m| match m.role {
                Role::System => json!({"role": "system", "content": m.content}),
                Role::User => json!({"role": "user", "content": m.content}),
                Role::Assistant => {
                    let mut v = json!({"role": "assistant", "content": m.content});
                    if !m.tool_calls.is_empty() {
                        v["tool_calls"] = Value::Array(
                            m.tool_calls
                                .iter()
                                .map(|tc| {
                                    json!({
                                        "id": tc.id,
                                        "type": "function",
                                        "function": {
                                            "name": tc.name,
                                            // OpenAI 要求 arguments 是 JSON 字符串而非对象
                                            "arguments": tc.arguments.to_string(),
                                        }
                                    })
                                })
                                .collect(),
                        );
                    }
                    v
                }
                Role::Tool => json!({
                    "role": "tool",
                    "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.content,
                }),
            })
            .collect()
    }

    /// 从响应中解析工具调用。
    pub(crate) fn decode_tool_calls(message: &Value) -> Vec<ToolCall> {
        message["tool_calls"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|tc| {
                        let id = tc["id"].as_str()?.to_string();
                        let name = tc["function"]["name"].as_str()?.to_string();
                        let raw = tc["function"]["arguments"].as_str().unwrap_or("{}");
                        // 模型偶尔会返回空串或畸形 JSON，退化为空对象而非丢弃整个调用 ——
                        // 丢弃会让 Agent 陷入「我调用了但没反应」的困惑
                        let arguments = serde_json::from_str(raw).unwrap_or_else(|_| json!({}));
                        Some(ToolCall {
                            id,
                            name,
                            arguments,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        let mut body = json!({
            "model": req.model,
            "messages": Self::encode_messages(&req.messages),
            "max_tokens": req.max_tokens,
            "temperature": req.temperature,
        });

        if !req.tools.is_empty() {
            body["tools"] = Value::Array(
                req.tools
                    .iter()
                    .map(|t| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": t.parameters,
                            }
                        })
                    })
                    .collect(),
            );
            body["tool_choice"] = json!("auto");
        }

        let resp = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| AresError::Llm(format!("请求失败：{e}")))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| AresError::Llm(format!("读取响应失败：{e}")))?;

        if !status.is_success() {
            // 错误体可能含有 key 片段，脱敏后再进错误链
            return Err(AresError::Llm(format!(
                "HTTP {status}: {}",
                ares_core::redact::redact(&text)
            )));
        }

        let v: Value = serde_json::from_str(&text)
            .map_err(|e| AresError::Llm(format!("响应不是合法 JSON：{e}")))?;

        let message = &v["choices"][0]["message"];
        Ok(CompletionResponse {
            content: message["content"].as_str().unwrap_or_default().to_string(),
            tool_calls: Self::decode_tool_calls(message),
            usage: Usage {
                input: v["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                output: v["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32,
            },
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Message;

    #[test]
    fn encodes_tool_call_arguments_as_string() {
        // OpenAI 协议要求 arguments 是字符串，传对象会 400
        let msgs = vec![Message::assistant(
            "",
            vec![ToolCall {
                id: "c1".into(),
                name: "terminal_execute".into(),
                arguments: json!({"command": "df -P"}),
            }],
        )];
        let encoded = OpenAiProvider::encode_messages(&msgs);
        let args = &encoded[0]["tool_calls"][0]["function"]["arguments"];
        assert!(args.is_string());
        assert!(args.as_str().unwrap().contains("df -P"));
    }

    #[test]
    fn encodes_tool_result_with_call_id() {
        let msgs = vec![Message::tool("c1", "exit_code: 0")];
        let encoded = OpenAiProvider::encode_messages(&msgs);
        assert_eq!(encoded[0]["role"], "tool");
        assert_eq!(encoded[0]["tool_call_id"], "c1");
    }

    #[test]
    fn decodes_tool_calls() {
        let msg = json!({
            "content": null,
            "tool_calls": [{
                "id": "call_abc",
                "type": "function",
                "function": {"name": "terminal_execute", "arguments": "{\"command\":\"uptime\"}"}
            }]
        });
        let calls = OpenAiProvider::decode_tool_calls(&msg);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "terminal_execute");
        assert_eq!(calls[0].arguments["command"], "uptime");
    }

    #[test]
    fn malformed_arguments_degrade_to_empty_object() {
        let msg = json!({
            "tool_calls": [{
                "id": "c1",
                "function": {"name": "t", "arguments": "not json at all"}
            }]
        });
        let calls = OpenAiProvider::decode_tool_calls(&msg);
        assert_eq!(calls.len(), 1, "畸形参数不应导致整个调用被丢弃");
        assert_eq!(calls[0].arguments, json!({}));
    }

    #[test]
    fn no_tool_calls_yields_empty_vec() {
        assert!(OpenAiProvider::decode_tool_calls(&json!({"content": "hi"})).is_empty());
    }

    #[test]
    fn base_url_trailing_slash_is_normalized() {
        let p = OpenAiProvider::new("x", "https://api.example.com/v1/", "k");
        assert_eq!(p.base_url, "https://api.example.com/v1");
    }
}
