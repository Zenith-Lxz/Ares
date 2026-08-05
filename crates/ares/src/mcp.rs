//! MCP 客户端集成（2026-08-05 批次4：Agent 可调用外部 MCP server 工具）。
//!
//! 配置 `~/.config/ares/mcp.json`：
//! ```json
//! {
//!   "servers": {
//!     "fs": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."] }
//!   }
//! }
//! ```
//!
//! GUI 启动时连接各 server（stdio 子进程）→ `tools/list` → 注册为 ares 工具
//! `mcp_<server>_<tool>`。MCP 工具归类 `Write`（默认 confirm 审批，安全优先
//! —— MCP server 可能是外部系统，副作用操作必须可见可批）。

use ares_core::ToolCategory;
use ares_tools::{Tool, ToolContext, ToolOutput};
use rmcp::model::{CallToolRequestParams, ContentBlock};
use rmcp::transport::child_process::TokioChildProcess;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;
use std::sync::Arc;

/// MCP 管理器：加载配置并连接全部 server。
pub struct McpManager {
    /// 已注册的 MCP 工具（注入 ares 工具注册表）。
    pub tools: Vec<Arc<dyn Tool>>,
    /// 连接失败信息（GUI toast 展示）。
    pub errors: Vec<String>,
}

impl McpManager {
    /// 连接所有配置的 server（阻塞；GUI 启动时调用）。
    pub fn load_and_connect(rt: &tokio::runtime::Runtime) -> Self {
        let mut m = Self {
            tools: Vec::new(),
            errors: Vec::new(),
        };
        let Some(cfg) = load_config() else {
            return m; // 未配置 mcp.json，静默
        };
        for (name, server) in &cfg.servers {
            let result = rt.block_on(connect_server(name, server));
            match result {
                Ok(tools) => m.tools.extend(tools),
                Err(e) => m.errors.push(format!("MCP {name}：{e}")),
            }
        }
        m
    }
}

#[derive(serde::Deserialize)]
struct McpConfig {
    servers: std::collections::BTreeMap<String, ServerConfig>,
}

#[derive(serde::Deserialize)]
struct ServerConfig {
    /// 远程 HTTP(S) MCP endpoint（streamable HTTP，含 SSE 流式）——
    /// 配置 url 时忽略 command/args（远程 server，不启动子进程）
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    command: String,
    #[serde(default)]
    args: Vec<String>,
    /// 环境变量（可选）
    #[serde(default)]
    env: std::collections::BTreeMap<String, String>,
}

fn load_config() -> Option<McpConfig> {
    let path = ares_core::paths::config_dir().join("mcp.json");
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 连接一个 server 并列出其工具。
async fn connect_server(name: &str, cfg: &ServerConfig) -> Result<Vec<Arc<dyn Tool>>, String> {
    // 远程 HTTP(S) MCP（streamable HTTP + SSE 流式）；否则 stdio 子进程
    let client = if let Some(url) = &cfg.url {
        ().serve(StreamableHttpClientTransport::from_uri(url.clone()))
            .await
            .map_err(|e| format!("握手失败：{e}"))?
    } else {
        let mut cmd = tokio::process::Command::new(&cfg.command);
        cmd.args(&cfg.args);
        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }
        let transport = TokioChildProcess::new(cmd).map_err(|e| format!("启动失败：{e}"))?;
        ().serve(transport)
            .await
            .map_err(|e| format!("握手失败：{e}"))?
    };
    let peer = client.peer().clone();
    let tools = peer
        .list_all_tools()
        .await
        .map_err(|e| format!("列出工具失败：{e}"))?;

    let mut out = Vec::new();
    for info in tools {
        let description = info.description.as_deref().unwrap_or("").to_string();
        let schema = (*info.input_schema).clone();
        let tool: Arc<dyn Tool> = Arc::new(McpTool {
            peer: Arc::new(peer.clone()),
            server: name.to_string(),
            mcp_name: info.name.as_ref().to_string(),
            display_name: format!("mcp_{name}_{}", info.name.as_ref()),
            description,
            schema: serde_json::Value::Object(schema),
        });
        out.push(tool);
    }
    Ok(out)
}

/// 一个 MCP 工具的 ares 包装。
pub struct McpTool {
    peer: Arc<rmcp::service::Peer<rmcp::RoleClient>>,
    server: String,
    mcp_name: String,
    display_name: String,
    description: String,
    schema: serde_json::Value,
}

#[async_trait::async_trait]
impl Tool for McpTool {
    fn name(&self) -> String {
        self.display_name.clone()
    }
    fn category(&self) -> ToolCategory {
        // MCP 工具可能有副作用（写文件/外部系统）→ 默认 confirm 审批
        ToolCategory::Write
    }
    fn description(&self) -> String {
        format!(
            "[MCP {server}] {desc}",
            server = self.server,
            desc = self.description
        )
    }
    fn parameters(&self) -> serde_json::Value {
        if self.schema.is_null() {
            serde_json::json!({"type": "object", "properties": {}, "required": []})
        } else {
            self.schema.clone()
        }
    }

    async fn call(
        &self,
        _ctx: &ToolContext,
        args: serde_json::Value,
    ) -> ares_core::Result<ToolOutput> {
        let mut params = CallToolRequestParams::new(self.mcp_name.clone());
        params.arguments = args.as_object().cloned();
        let result = self
            .peer
            .call_tool(params)
            .await
            .map_err(|e| ares_core::AresError::Config(format!("MCP 调用失败：{e}")))?;

        // 提取文本内容
        let mut text = String::new();
        for block in &result.content {
            match block {
                ContentBlock::Text(t) => text.push_str(&t.text),
                _ => text.push_str("（非文本内容）\n"),
            }
        }
        if result.is_error.unwrap_or(false) {
            return Err(ares_core::AresError::Config(format!(
                "MCP 工具错误：{text}"
            )));
        }
        Ok(ToolOutput::text(text))
    }
}
