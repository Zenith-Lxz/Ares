//! 测试辅助。仅在 test 构建中编译。

use crate::registry::ToolContext;
use ares_audit::AuditWriter;
use ares_core::config::HostsConfig;
use ares_core::HostId;
use ares_exec::LocalExecutor;
use ares_policy::{PolicyConfig, PolicyEngine};
use std::sync::Arc;
use tokio::sync::Mutex;

/// 构造一个使用临时数据目录的测试上下文。
pub fn test_ctx(scope: Vec<HostId>) -> ToolContext {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("ARES_DATA_DIR", tmp.path());

    let policy = PolicyEngine::new(
        PolicyConfig::load_from("/nonexistent").unwrap(),
        HostsConfig::default(),
    )
    .unwrap();
    let audit = AuditWriter::open_at(tmp.path().join("audit")).unwrap();

    // 让临时目录活到进程结束
    std::mem::forget(tmp);

    ToolContext::new(
        Arc::new(LocalExecutor::new()),
        Arc::new(policy),
        Arc::new(Mutex::new(audit)),
        scope,
        "sess-test",
        "agent",
    )
}
