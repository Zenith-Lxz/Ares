//! 本机执行器。
//!
//! 通过 `sh -c` 执行，保留 shell 语义（管道、重定向、复合命令），
//! 与远程 `ssh host 'cmd'` 的行为一致 —— 两者的差异越小，
//! M1 验证过的逻辑在 M2 就越可靠。

use crate::{ExecOutcome, ExecRequest, Executor};
use ares_core::{AresError, HostId, Result};
use async_trait::async_trait;
use std::process::Stdio;
use std::time::Instant;
use tokio::process::Command;

#[derive(Debug, Clone, Default)]
pub struct LocalExecutor;

impl LocalExecutor {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Executor for LocalExecutor {
    async fn execute(&self, req: ExecRequest) -> Result<ExecOutcome> {
        if !self.supports(&req.host) {
            return Err(AresError::Exec(format!(
                "LocalExecutor 不支持主机 {}",
                req.host
            )));
        }

        let started = Instant::now();

        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg(&req.command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| AresError::Exec(format!("无法启动 shell：{e}")))?;

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
            Ok(Err(e)) => Err(AresError::Exec(format!("等待进程失败：{e}"))),
            Err(_) => Ok(ExecOutcome {
                host: req.host,
                exit_code: -1,
                stdout: String::new(),
                stderr: format!("命令超时（{}秒）后被终止", req.timeout.as_secs()),
                duration_ms,
                timed_out: true,
            }),
        }
    }

    fn supports(&self, host: &HostId) -> bool {
        host.as_str() == "localhost"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn captures_stdout_and_exit_code() {
        let e = LocalExecutor::new();
        let out = e.execute(ExecRequest::local("echo hello")).await.unwrap();
        assert!(out.is_success());
        assert_eq!(out.exit_code, 0);
        assert_eq!(out.stdout.trim(), "hello");
        assert!(out.stderr.is_empty());
    }

    #[tokio::test]
    async fn captures_nonzero_exit_and_stderr() {
        let e = LocalExecutor::new();
        let out = e
            .execute(ExecRequest::local("echo oops >&2; exit 3"))
            .await
            .unwrap();
        assert!(!out.is_success());
        assert_eq!(out.exit_code, 3);
        assert_eq!(out.stderr.trim(), "oops");
    }

    #[tokio::test]
    async fn shell_semantics_are_preserved() {
        let e = LocalExecutor::new();
        // 管道
        let out = e
            .execute(ExecRequest::local("echo a b c | wc -w"))
            .await
            .unwrap();
        assert_eq!(out.stdout.trim(), "3");
        // 复合命令
        let out = e
            .execute(ExecRequest::local("true && echo yes"))
            .await
            .unwrap();
        assert_eq!(out.stdout.trim(), "yes");
    }

    #[tokio::test]
    async fn timeout_is_enforced() {
        let e = LocalExecutor::new();
        let req = ExecRequest::local("sleep 5").with_timeout(Duration::from_millis(200));
        let out = e.execute(req).await.unwrap();
        assert!(out.timed_out);
        assert_eq!(out.exit_code, -1);
        assert!(
            out.duration_ms < 3000,
            "超时后应立即返回，实际耗时 {}ms",
            out.duration_ms
        );
    }

    #[tokio::test]
    async fn stdin_is_closed_so_commands_never_hang() {
        // 若 stdin 未关闭，等待输入的命令会永久挂起
        let e = LocalExecutor::new();
        let req = ExecRequest::local("cat").with_timeout(Duration::from_secs(2));
        let out = e.execute(req).await.unwrap();
        assert!(!out.timed_out, "stdin 应为 null，cat 应立即结束");
        assert!(out.is_success());
    }

    #[tokio::test]
    async fn rejects_non_local_host() {
        let e = LocalExecutor::new();
        let req = ExecRequest::new(HostId::new("prod-web-01"), "uptime");
        assert!(e.execute(req).await.is_err());
        assert!(!e.supports(&HostId::new("prod-web-01")));
        assert!(e.supports(&HostId::localhost()));
    }

    #[tokio::test]
    async fn combined_output_puts_stderr_last() {
        let e = LocalExecutor::new();
        let out = e
            .execute(ExecRequest::local("echo out; echo err >&2"))
            .await
            .unwrap();
        let c = out.combined();
        assert!(c.find("out").unwrap() < c.find("err").unwrap());
    }

    #[tokio::test]
    async fn non_utf8_output_does_not_panic() {
        let e = LocalExecutor::new();
        let out = e
            .execute(ExecRequest::local(r#"printf '\xff\xfe'"#))
            .await
            .unwrap();
        assert!(out.is_success());
    }
}
