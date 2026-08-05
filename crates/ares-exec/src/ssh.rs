//! 最小 SSH 执行器：每次调用 spawn `ssh <alias> <cmd>`。
//!
//! M1.5 入口改造的最小实现 —— 支持 Agent 面板对远端主机执行命令。
//! 完整能力（密钥管理 / askpass / vault 密码）在 M2 补齐；
//! 本实现用 `BatchMode=yes` 保证无交互会话（不会卡在密码提示），
//! 密钥代理 / 跳板等能力天然继承 `~/.ssh/config` 与用户 ssh-agent。

use crate::{ExecOutcome, ExecRequest, Executor};
use ares_core::{AresError, HostId, Result};
use async_trait::async_trait;
use std::process::Stdio;
use std::time::Instant;
use tokio::process::Command;

#[derive(Debug, Clone, Default)]
pub struct SshExecutor;

impl SshExecutor {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Executor for SshExecutor {
    async fn execute(&self, req: ExecRequest) -> Result<ExecOutcome> {
        if !self.supports(&req.host) {
            return Err(AresError::Exec(format!(
                "SshExecutor 不支持主机 {}（本机请用 LocalExecutor）",
                req.host
            )));
        }

        let started = Instant::now();

        let child = Command::new("ssh")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("ConnectTimeout=10")
            .arg(req.host.as_str())
            .arg(&req.command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| AresError::Exec(format!("无法启动 ssh：{e}")))?;

        let result = tokio::time::timeout(req.timeout, child.wait_with_output()).await;

        let duration_ms = started.elapsed().as_millis() as u64;

        match result {
            Ok(Ok(out)) => Ok(ExecOutcome {
                host: req.host,
                exit_code: out.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                duration_ms,
                timed_out: false,
            }),
            Ok(Err(e)) => Err(AresError::Exec(format!("等待 ssh 失败：{e}"))),
            Err(_) => Ok(ExecOutcome {
                host: req.host,
                exit_code: -1,
                stdout: String::new(),
                stderr: format!("ssh 超时（{}s）", req.timeout.as_secs()),
                duration_ms,
                timed_out: true,
            }),
        }
    }

    /// 本机（localhost/127.0.0.1/::1）走 LocalExecutor，其余走 ssh。
    fn supports(&self, host: &HostId) -> bool {
        !host.is_local()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_support_localhost() {
        let ex = SshExecutor::new();
        assert!(!ex.supports(&HostId::localhost()));
        assert!(!ex.supports(&HostId::new("127.0.0.1")));
        assert!(ex.supports(&HostId::new("prod-web-01")));
    }

    #[test]
    fn rejects_local_hosts() {
        let ex = SshExecutor::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt
            .block_on(ex.execute(ExecRequest::local("echo hi")))
            .unwrap_err();
        assert!(err.to_string().contains("本机请用 LocalExecutor"));
    }

    #[test]
    fn remote_command_timeout_is_reported() {
        // BatchMode + 不可达主机 + 短超时 → timed_out 或非 0 退出，两者都接受，
        // 但不能 panic、不能无限等待。
        let ex = SshExecutor::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let req = ExecRequest::new(HostId::new("nonexistent-host.invalid"), "echo hi")
            .with_timeout(std::time::Duration::from_secs(3));
        let out = rt.block_on(ex.execute(req)).unwrap();
        // 不可达主机：要么超时要么连接失败（非 0）
        assert!(out.timed_out || out.exit_code != 0);
    }
}
