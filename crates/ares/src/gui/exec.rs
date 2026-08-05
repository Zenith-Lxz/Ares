//! 终端注入执行器：agent 的命令**真实注入当前终端会话**执行。
//!
//! 用户心智：「agent 在终端里干活」—— 命令像人打字一样出现在终端、
//! 输出直接可见；agent 通过屏幕快照的增量观察结果。
//! 与 Local/SshExecutor 的差异：无独立进程、无结构化 stderr、
//! 无可靠 exit code —— 用「输出稳定检测」判断命令结束，超时视为失败。

use crate::gui::session::Session;
use ares_core::{HostId, Result};
use ares_exec::{ExecOutcome, ExecRequest, Executor};
use async_trait::async_trait;
use std::time::Instant;

/// 采样间隔与稳定判定阈值：连续两次采样相同即认为命令输出结束。
const SAMPLE: std::time::Duration = std::time::Duration::from_millis(250);

pub struct TerminalSessionExecutor {
    session: Session,
}

impl TerminalSessionExecutor {
    pub fn new(session: Session) -> Self {
        Self { session }
    }
}

#[async_trait]
impl Executor for TerminalSessionExecutor {
    async fn execute(&self, req: ExecRequest) -> Result<ExecOutcome> {
        let started = Instant::now();

        // 注入前基线（终端已显示的内容）
        let baseline = self.session.snapshot_text();

        // 注入命令：像人打字 + 回车
        // （清洗尾部换行 —— LLM 生成的命令可能带 \n，若原样注入
        //   \n 执行命令 + \r 触发空命令 → 终端多一个提示符）
        let cmd = req.command.trim_end_matches(['\r', '\n']);
        self.session.write(cmd.as_bytes());
        self.session.write(b"\r");

        // 等待「变化出现 → 稳定」或超时。
        //
        // 注意：注入后立即采样是旧屏幕（ssh 网络延迟，输出还没到），
        // 不能把「还没变化」误判为「已稳定」—— 必须等到屏幕
        // 相对注入前发生变化，且之后连续两次采样相同才算完成。
        let deadline = started + req.timeout;
        let mut last = self.session.snapshot_text();
        let mut changed = false;
        let mut timed_out = false;
        loop {
            tokio::time::sleep(SAMPLE).await;
            let now = self.session.snapshot_text();
            if now != last {
                changed = true;
                last = now;
            } else if changed {
                break; // 变化之后稳定：命令结束（含提示符）
            }
            if Instant::now() >= deadline {
                timed_out = true;
                break;
            }
        }

        let final_text = self.session.snapshot_text();
        let stdout = diff_since(&baseline, &final_text);

        Ok(ExecOutcome {
            host: req.host,
            // 终端注入模式拿不到真实退出码：稳定完成记 0，超时记 -1
            exit_code: if timed_out { -1 } else { 0 },
            stdout,
            stderr: String::new(),
            duration_ms: started.elapsed().as_millis() as u64,
            timed_out,
        })
    }

    /// 绑定当前终端会话 —— 由 GUI 决定用哪个会话，此执行器支持任意主机。
    fn supports(&self, _host: &HostId) -> bool {
        true
    }
}

/// baseline 之后的新增内容（命令回显 + 输出 + 提示符）。
fn diff_since(baseline: &str, final_text: &str) -> String {
    let b: Vec<&str> = baseline.lines().collect();
    let f: Vec<&str> = final_text.lines().collect();
    let mut i = 0;
    while i < b.len() && i < f.len() && b[i] == f[i] {
        i += 1;
    }
    f[i..].join("\n")
}

/// 空会话保护：不 panic 的稳定判定。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_returns_only_new_content() {
        let base = "line1\nline2\n$ ";
        let final_ = "line1\nline2\n$ df -P\nFilesystem\n/dev/disk1\n$ ";
        assert_eq!(
            diff_since(base, final_),
            "$ df -P\nFilesystem\n/dev/disk1\n$ "
        );
    }

    #[test]
    fn diff_with_identical_content_is_empty() {
        let text = "same\ncontent";
        assert_eq!(diff_since(text, text), "");
    }

    #[test]
    fn diff_handles_short_baseline() {
        assert_eq!(diff_since("", "new\noutput"), "new\noutput");
    }
}

/// 多主机路由执行器（2026-08-05 多主机编排）：
/// 当前 pane 主机 → 终端注入（用户看得见）；其他主机 → SshExecutor 独立通道。
pub struct RoutedExecutor {
    current: TerminalSessionExecutor,
    current_host: HostId,
    ssh: ares_exec::SshExecutor,
}

impl RoutedExecutor {
    pub fn new(current: TerminalSessionExecutor, current_host: HostId) -> Self {
        Self {
            current,
            current_host,
            ssh: ares_exec::SshExecutor::new(),
        }
    }
}

#[async_trait]
impl Executor for RoutedExecutor {
    async fn execute(&self, req: ExecRequest) -> Result<ExecOutcome> {
        if req.host == self.current_host {
            self.current.execute(req).await
        } else {
            self.ssh.execute(req).await
        }
    }

    fn supports(&self, _host: &HostId) -> bool {
        true
    }
}
