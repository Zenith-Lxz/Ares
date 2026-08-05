//! 统一错误类型。
//!
//! 库代码返回 `AresError`，二进制入口用 `anyhow` 做上下文附加。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AresError {
    #[error("配置错误：{0}")]
    Config(String),

    #[error("主机 {0} 不在当前 scope 内")]
    OutOfScope(String),

    #[error("操作被策略禁止：{0}")]
    Denied(String),

    #[error("用户拒绝了审批")]
    ApprovalRejected,

    #[error("审批超时")]
    ApprovalTimeout,

    #[error("执行失败：{0}")]
    Exec(String),

    #[error("审计链校验失败：第 {index} 条记录哈希不匹配")]
    AuditChainBroken { index: usize },

    #[error("工具 {0} 不存在")]
    UnknownTool(String),

    #[error("工具参数无效：{0}")]
    InvalidArgs(String),

    #[error("LLM 调用失败：{0}")]
    Llm(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    /// 来自 anyhow 链的错误透传。
    ///
    /// 用于跨 crate 边界：`ares-darwin` 的 Keychain 模块整体使用
    /// anyhow（见「职责边界」），而 `ares-agent` 使用 `ares_core::Result`。
    /// 本变体让 `anyhow::Result<T>` 可以 `?` 进 `ares_core::Result<T>`，
    /// 避免在每处调用点手工 map_err。
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, AresError>;
