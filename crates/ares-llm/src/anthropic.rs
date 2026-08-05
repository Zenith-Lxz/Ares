//! Anthropic Messages API。
//!
//! 与 OpenAI 协议的差异集中在四处：system 独立成顶层字段、
//! 工具调用是 content 数组中的 tool_use 块、工具结果以 user 消息
//! 承载 tool_result 块、认证用 x-api-key 头。

use crate::{CompletionRequest, CompletionResponse, Message, Provider, Role, ToolCall, Usage};
use ares_core::{AresError, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

const API_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    base_url: String,
    api_key: String,
    client: reqwest::Client,
    name: String,
}

impl AnthropicProvider {
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

    /// 拆出 system 提示与其余消息。
    pub(crate) fn split_system(messages: &[Message]) -> (String, Vec<Value>) {
        let system = messages
            .iter()
            .filter(|m| m.role == Role::System)
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n\n");

        let rest = messages
            .iter()
            .filter(|m| m.role != Role::System)
            .map(Self::encode_message)
            .collect();

        (system, rest)
    }

    fn encode_message(m: &Message) -> Value {
        match m.role {
            Role::User => json!({"role": "user", "content": m.content}),

            Role::Assistant => {
                let mut blocks: Vec<Value> = Vec::new();
                if !m.content.is_empty() {
                    blocks.push(json!({"type": "text", "text": m.content}));
                }
                for tc in &m.tool_calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": tc.arguments,
                    }));
                }
                json!({"role": "assistant", "content": blocks})
            }

            // 工具结果在 Anthropic 中以 user 消息承载
            Role::Tool => json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.content,
                }]
            }),

            Role::System => unreachable!("system 已在 split_system 中剥离"),
        }
    }

    /// 从响应 content 数组中解析文本与工具调用。
    pub(crate) fn decode_content(content: &Value) -> (String, Vec<ToolCall>) {
        let mut text = String::new();
        let mut calls = Vec::new();

        if let Some(blocks) = content.as_array() {
            for b in blocks {
                match b["type"].as_str() {
                    Some("text") => text.push_str(b["text"].as_str().unwrap_or_default()),
                    Some("tool_use") => {
                        if let (Some(id), Some(name)) = (b["id"].as_str(), b["name"].as_str()) {
                            calls.push(ToolCall {
                                id: id.to_string(),
                                name: name.to_string(),
                                arguments: b["input"].clone(),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        (text, calls)
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        let (system, messages) = Self::split_system(&req.messages);

        let mut body = json!({
            "model": req.model,
            "messages": messages,
            "max_tokens": req.max_tokens,
            "temperature": req.temperature,
        });
        if !system.is_empty() {
            body["system"] = json!(system);
        }
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(
                req.tools
                    .iter()
                    .map(|t| {
                        json!({
                            "name": t.name,
                            "description": t.description,
                            "input_schema": t.parameters,
                        })
                    })
                    .collect(),
            );
        }

        let resp = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
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
            return Err(AresError::Llm(format!(
                "HTTP {status}: {}",
                ares_core::redact::redact(&text)
            )));
        }

        let v: Value = serde_json::from_str(&text)
            .map_err(|e| AresError::Llm(format!("响应不是合法 JSON：{e}")))?;

        let (content, tool_calls) = Self::decode_content(&v["content"]);
        Ok(CompletionResponse {
            content,
            tool_calls,
            usage: Usage {
                input: v["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32,
                output: v["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32,
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

    #[test]
    fn system_messages_are_hoisted_out() {
        let msgs = vec![Message::system("你是运维助手"), Message::user("检查磁盘")];
        let (system, rest) = AnthropicProvider::split_system(&msgs);
        assert_eq!(system, "你是运维助手");
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0]["role"], "user");
    }

    #[test]
    fn multiple_system_messages_are_joined() {
        let msgs = vec![Message::system("A"), Message::system("B")];
        let (system, rest) = AnthropicProvider::split_system(&msgs);
        assert_eq!(system, "A\n\nB");
        assert!(rest.is_empty());
    }

    #[test]
    fn assistant_tool_call_becomes_tool_use_block() {
        let msgs = vec![Message::assistant(
            "我来看看",
            vec![ToolCall {
                id: "toolu_1".into(),
                name: "terminal_execute".into(),
                arguments: json!({"command": "df -P"}),
            }],
        )];
        let (_, rest) = AnthropicProvider::split_system(&msgs);
        let blocks = rest[0]["content"].as_array().unwrap();

        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["id"], "toolu_1");
        // input 是对象而非字符串 —— 与 OpenAI 相反
        assert!(blocks[1]["input"].is_object());
    }

    #[test]
    fn tool_result_is_carried_by_user_message() {
        let msgs = vec![Message::tool("toolu_1", "exit_code: 0")];
        let (_, rest) = AnthropicProvider::split_system(&msgs);

        assert_eq!(rest[0]["role"], "user");
        assert_eq!(rest[0]["content"][0]["type"], "tool_result");
        assert_eq!(rest[0]["content"][0]["tool_use_id"], "toolu_1");
    }

    #[test]
    fn assistant_with_no_text_omits_text_block() {
        let msgs = vec![Message::assistant(
            "",
            vec![ToolCall {
                id: "t1".into(),
                name: "n".into(),
                arguments: json!({}),
            }],
        )];
        let (_, rest) = AnthropicProvider::split_system(&msgs);
        let blocks = rest[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_use");
    }

    #[test]
    fn decodes_mixed_content_blocks() {
        let content = json!([
            {"type": "text", "text": "我来检查一下。"},
            {"type": "tool_use", "id": "toolu_x", "name": "terminal_execute", "input": {"command": "uptime"}}
        ]);
        let (text, calls) = AnthropicProvider::decode_content(&content);

        assert_eq!(text, "我来检查一下。");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "toolu_x");
        assert_eq!(calls[0].arguments["command"], "uptime");
    }

    #[test]
    fn decodes_text_only_response() {
        let content = json!([{"type": "text", "text": "磁盘使用率 44%。"}]);
        let (text, calls) = AnthropicProvider::decode_content(&content);
        assert_eq!(text, "磁盘使用率 44%。");
        assert!(calls.is_empty());
    }

    #[test]
    fn unknown_block_types_are_ignored() {
        let content = json!([
            {"type": "thinking", "thinking": "内部推理"},
            {"type": "text", "text": "结论"}
        ]);
        let (text, _) = AnthropicProvider::decode_content(&content);
        assert_eq!(text, "结论");
    }
}
