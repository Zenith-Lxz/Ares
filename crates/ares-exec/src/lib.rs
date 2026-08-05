//! 命令执行抽象。
//!
//! `Executor` 是本地与远程执行的统一接口。M1 只有 `LocalExecutor`，
//! M2 会加入 `SshExecutor`，工具层以上完全不感知差异。
//! 因此这个 trait 不得出现任何本地或远程专有的概念。

pub mod local;
pub mod ssh;

use ares_core::{HostId, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub use local::LocalExecutor;
pub use ssh::SshExecutor;

/// 默认命令超时。运维命令很少需要超过这个时长；
/// 真正的长任务应走 terminal_start/poll 而非 execute。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct ExecRequest {
    pub host: HostId,
    pub command: String,
    pub timeout: Duration,
}

impl ExecRequest {
    /// 构造一个本机执行请求。
    pub fn local(command: impl Into<String>) -> Self {
        Self {
            host: HostId::localhost(),
            command: command.into(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn new(host: HostId, command: impl Into<String>) -> Self {
        Self {
            host,
            command: command.into(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecOutcome {
    pub host: HostId,
    /// 超时或被信号终止时为 -1
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub timed_out: bool,
}

impl ExecOutcome {
    pub fn is_success(&self) -> bool {
        self.exit_code == 0 && !self.timed_out
    }

    /// 合并后的输出，用于展示与摘要。stderr 在后，因为错误信息通常更重要，
    /// 截断时应优先保留。
    pub fn combined(&self) -> String {
        match (self.stdout.trim().is_empty(), self.stderr.trim().is_empty()) {
            (true, true) => String::new(),
            (false, true) => self.stdout.clone(),
            (true, false) => self.stderr.clone(),
            (false, false) => format!("{}\n{}", self.stdout, self.stderr),
        }
    }
}

#[async_trait]
pub trait Executor: Send + Sync {
    /// 执行一条命令。
    ///
    /// 注意：本方法**不做任何权限判定** —— 判定由 harness 在调用前完成。
    /// 把执行与授权分开，是为了让执行器保持可替换。
    async fn execute(&self, req: ExecRequest) -> Result<ExecOutcome>;

    /// 该执行器是否能处理指定主机。
    fn supports(&self, host: &HostId) -> bool;
}
