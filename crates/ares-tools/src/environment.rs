//! 入口工具。
//!
//! Agent 的第一步调用。运行时返回当前 scope 的主机与可用工具，
//! 而不是把全部工具定义塞进 system prompt —— 既省 token，
//! 也让 scope 边界天然体现在返回值里：不在 scope 的主机根本不出现。

use crate::registry::{Tool, ToolContext, ToolOutput};
use ares_core::{Result, ToolCategory};
use async_trait::async_trait;
use serde_json::json;

pub struct GetEnvironmentTool {
    /// 注册表中全部工具名。构造时注入，避免循环依赖
    tool_names: Vec<String>,
}

impl GetEnvironmentTool {
    pub fn new(tool_names: Vec<String>) -> Self {
        Self { tool_names }
    }
}

#[async_trait]
impl Tool for GetEnvironmentTool {
    fn name(&self) -> String {
        "get_environment".into()
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn description(&self) -> String {
        "获取当前运行环境：可操作的主机清单、每台主机的环境等级、可用工具列表。\
         这应该是你的第一个调用 —— 在此之前不要假设任何主机存在。\
         若返回的主机列表为空，说明当前没有被授权的主机，应告知用户而非继续尝试。"
            .into()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object", "properties": {}, "required": []})
    }

    async fn call(&self, ctx: &ToolContext, _args: serde_json::Value) -> Result<ToolOutput> {
        let hosts: Vec<serde_json::Value> = ctx
            .scope()
            .iter()
            .map(|h| {
                json!({
                    "host": h.as_str(),
                    "env": ctx.policy().env_of(h).to_string(),
                })
            })
            .collect();

        let payload = json!({
            "environment": "ares-local",
            "host_count": hosts.len(),
            "hosts": hosts,
            "tools": self.tool_names,
            "constraints": {
                "approval": "所有变更操作默认需要审批（确认或拒绝）。部分操作被硬禁止且不提供确认途径。",
                "output": "优先使用命令的机器可读输出模式（ip -j / systemctl show / journalctl -o json / df -P / lsblk -J / ss -H），自由文本输出仅用于需要给人看的场合。",
                "scope": "只能操作上述 hosts 列表中的主机。",
            }
        });

        let content = serde_json::to_string_pretty(&payload)?;
        let display = format!(
            "{} 台主机在 scope 内，{} 个可用工具",
            hosts.len(),
            self.tool_names.len()
        );
        Ok(ToolOutput {
            content,
            display,
            stored_ref: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_ctx;
    use ares_core::HostId;

    #[tokio::test]
    async fn returns_hosts_in_scope() {
        let ctx = test_ctx(vec![HostId::localhost()]);
        let t = GetEnvironmentTool::new(vec!["terminal_execute".into()]);
        let out = t.call(&ctx, json!({})).await.unwrap();

        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["host_count"], 1);
        assert_eq!(v["hosts"][0]["host"], "localhost");
        assert_eq!(v["hosts"][0]["env"], "local");
    }

    #[tokio::test]
    async fn empty_scope_returns_zero_hosts() {
        // 未授权的调用方看到的就是这个 —— 不是报错，是「什么都没有」
        let ctx = test_ctx(vec![]);
        let t = GetEnvironmentTool::new(vec![]);
        let out = t.call(&ctx, json!({})).await.unwrap();

        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["host_count"], 0);
        assert!(v["hosts"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn constraints_mention_structured_output() {
        let ctx = test_ctx(vec![HostId::localhost()]);
        let t = GetEnvironmentTool::new(vec![]);
        let out = t.call(&ctx, json!({})).await.unwrap();
        assert!(out.content.contains("ip -j"));
        assert!(out.content.contains("审批"));
    }
}
