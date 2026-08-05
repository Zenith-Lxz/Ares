//! 主力执行工具。
//!
//! 刻意保持「原样执行 shell 命令」而不做任何语义包装：
//! 基准数据显示原始 CLI 比等价的工具封装省 10-32 倍 token
//! 且可靠性更高。语义层的价值在于返回多次往返才能拼出的状态切片，
//! 而不是替 Agent 拼一条它自己会写的命令。

use crate::registry::{Tool, ToolContext, ToolOutput};
use ares_core::{AresError, Decision, HostId, Result, ToolCategory};
use ares_exec::{ExecRequest, DEFAULT_TIMEOUT};
use ares_policy::PolicyQuery;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct Args {
    command: String,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

/// 判定结果与执行请求的打包。Agent loop 拿到后先走审批，再调 `execute`。
#[derive(Debug, Clone)]
pub struct PreparedExec {
    pub request: ExecRequest,
    pub decision: Decision,
}

pub struct TerminalExecuteTool;

impl TerminalExecuteTool {
    pub fn new() -> Self {
        Self
    }

    /// 解析参数并做策略判定，**不执行**。
    ///
    /// 拆出这一步是为了让审批呈现方式可替换：TUI 用弹窗，
    /// MCP server 用 MRTR 或系统对话框，两者共用同一套判定。
    pub fn prepare(&self, ctx: &ToolContext, args: &serde_json::Value) -> Result<PreparedExec> {
        let a: Args = serde_json::from_value(args.clone())
            .map_err(|e| AresError::InvalidArgs(format!("terminal_execute 参数无效：{e}")))?;

        if a.command.trim().is_empty() {
            return Err(AresError::InvalidArgs("command 不能为空".into()));
        }

        let host = a.host.map(HostId::new).unwrap_or_else(HostId::localhost);
        ctx.check_scope(&host)?;

        let decision = ctx.policy().evaluate(&PolicyQuery {
            host: host.clone(),
            tool: "terminal_execute".to_string(),
            category: ToolCategory::Exec,
            command: Some(a.command.clone()),
            host_count: 1,
        });

        let timeout = a
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_TIMEOUT);

        Ok(PreparedExec {
            request: ExecRequest::new(host, a.command).with_timeout(timeout),
            decision,
        })
    }

    /// 执行已通过审批的请求。
    pub async fn execute(&self, ctx: &ToolContext, req: ExecRequest) -> Result<ToolOutput> {
        let outcome = ctx.executor().execute(req).await?;
        let budgeted = ctx.budget().apply(&outcome.combined())?;

        let status = if outcome.timed_out {
            "超时".to_string()
        } else if outcome.is_success() {
            "成功".to_string()
        } else {
            format!("退出码 {}", outcome.exit_code)
        };

        let content = format!(
            "exit_code: {}\nduration_ms: {}\n---\n{}",
            outcome.exit_code, outcome.duration_ms, budgeted.text
        );
        let display = format!("{} · {}ms\n{}", status, outcome.duration_ms, budgeted.text);

        Ok(ToolOutput {
            content,
            display,
            stored_ref: budgeted.stored_ref,
        })
    }
}

impl Default for TerminalExecuteTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for TerminalExecuteTool {
    fn name(&self) -> String {
        "terminal_execute".into()
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Exec
    }

    fn description(&self) -> String {
        "在指定主机上执行一条 shell 命令，返回退出码、标准输出与标准错误。\
         这是你的主要手段 —— 直接写 shell 命令，不要期待存在更高层的封装工具。\
         优先使用机器可读输出（ip -j addr / systemctl show / journalctl -o json / df -P / lsblk -J / ss -H），\
         它们更省 token 也更好解析。需要长时间运行的任务不要用本工具。\
         命令会经过策略判定：部分命令自动执行，部分需要用户确认或指纹，\
         少数不可逆的命令被硬禁止 —— 被禁止时不要尝试变形绕过，直接告诉用户需要人工执行。".into()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "要执行的 shell 命令"
                },
                "host": {
                    "type": "string",
                    "description": "目标主机，省略则为 localhost。必须在 get_environment 返回的列表内"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "超时秒数，默认 60"
                }
            },
            "required": ["command"]
        })
    }

    /// 直接调用路径 —— 仅在判定为无需交互（Observer / Auto）时可用。
    /// 需要交互的判定由 Agent loop 走 prepare + 审批 + execute。
    async fn call(&self, ctx: &ToolContext, args: serde_json::Value) -> Result<ToolOutput> {
        let prepared = self.prepare(ctx, &args)?;
        match &prepared.decision {
            Decision::Deny { reason } => Err(AresError::Denied(reason.clone())),
            d if d.needs_interaction() => Err(AresError::Exec(
                "该命令需要审批，必须通过 prepare + 审批流程调用".into(),
            )),
            _ => self.execute(ctx, prepared.request).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_ctx;

    #[tokio::test]
    async fn readonly_command_runs_directly() {
        let ctx = test_ctx(vec![HostId::localhost()]);
        let t = TerminalExecuteTool::new();
        let out = t.call(&ctx, json!({"command": "echo hi"})).await;

        // echo 不在 observer 白名单里，应被判为 Confirm 而拒绝直接调用
        assert!(out.is_err());

        let out = t.call(&ctx, json!({"command": "uptime"})).await.unwrap();
        assert!(out.content.contains("exit_code: 0"));
    }

    #[tokio::test]
    async fn denied_command_returns_denied_error() {
        let ctx = test_ctx(vec![HostId::localhost()]);
        let t = TerminalExecuteTool::new();
        let err = t
            .call(&ctx, json!({"command": "rm -rf /"}))
            .await
            .unwrap_err();
        assert!(matches!(err, AresError::Denied(_)));
    }

    #[tokio::test]
    async fn prepare_returns_decision_without_executing() {
        let ctx = test_ctx(vec![HostId::localhost()]);
        let t = TerminalExecuteTool::new();

        let p = t
            .prepare(
                &ctx,
                &json!({"command": "touch /tmp/ares-should-not-exist"}),
            )
            .unwrap();
        assert!(matches!(p.decision, Decision::Confirm { .. }));
        // prepare 不执行，文件不应存在
        assert!(!std::path::Path::new("/tmp/ares-should-not-exist").exists());
    }

    #[tokio::test]
    async fn out_of_scope_host_is_rejected() {
        let ctx = test_ctx(vec![HostId::localhost()]);
        let t = TerminalExecuteTool::new();
        let err = t
            .prepare(&ctx, &json!({"command": "uptime", "host": "prod-web-01"}))
            .unwrap_err();
        assert!(matches!(err, AresError::OutOfScope(_)));
    }

    #[tokio::test]
    async fn empty_command_is_rejected() {
        let ctx = test_ctx(vec![HostId::localhost()]);
        let t = TerminalExecuteTool::new();
        assert!(t.prepare(&ctx, &json!({"command": "   "})).is_err());
    }

    #[tokio::test]
    async fn missing_command_field_is_rejected() {
        let ctx = test_ctx(vec![HostId::localhost()]);
        let t = TerminalExecuteTool::new();
        assert!(t.prepare(&ctx, &json!({})).is_err());
    }

    #[tokio::test]
    async fn large_output_is_budgeted() {
        let ctx = test_ctx(vec![HostId::localhost()]);
        let t = TerminalExecuteTool::new();
        // seq 输出上万行，必然超预算
        let p = t.prepare(&ctx, &json!({"command": "seq 1 20000"})).unwrap();
        let out = t.execute(&ctx, p.request).await.unwrap();

        assert!(out.stored_ref.is_some());
        assert!(out.content.contains("省略"));
    }
}
