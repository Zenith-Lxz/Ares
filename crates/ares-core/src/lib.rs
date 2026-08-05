//! ARES 核心类型与配置。
//!
//! 本 crate 不依赖任何其他 ares crate，是依赖图的根。

pub mod config;
pub mod display;
pub mod error;
pub mod paths;
pub mod redact;
pub mod ssh_config;
pub mod types;

pub use error::{AresError, Result};
pub use types::{Decision, Env, HostId, ToolCategory};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_not_empty() {
        assert!(!version().is_empty());
    }
}
