//! 工具注册表与执行上下文。
//!
//! 工具规格由 Rust 类型单一来源生成 JSON Schema。
//! M5 的 MCP server 会复用同一份 `specs()` 输出 —— 内置 Agent
//! 与外部 Agent 看到的工具定义必须完全一致，否则行为会漂移。

use ares_audit::AuditWriter;
use ares_core::{AresError, HostId, Result, ToolCategory};
use ares_exec::Executor;
use ares_policy::PolicyEngine;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::budget::OutputBudget;

/// 一次工具调用的产出。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolOutput {
    /// 回传给 LLM 的文本（已脱敏、已过预算）
    pub content: String,
    /// 展示给人看的文本。通常比 content 更简洁
    pub display: String,
    /// 若输出落盘，此处为引用 ID
    pub stored_ref: Option<String>,
}

impl ToolOutput {
    /// 内容与展示相同的简单输出。
    pub fn text(s: impl Into<String>) -> Self {
        let s = s.into();
        Self {
            content: s.clone(),
            display: s,
            stored_ref: None,
        }
    }

    pub fn with_display(mut self, d: impl Into<String>) -> Self {
        self.display = d.into();
        self
    }
}

/// 工具执行上下文。所有工具共享同一份，由 harness 装配。
pub struct ToolContext {
    executor: Arc<dyn Executor>,
    policy: Arc<PolicyEngine>,
    audit: Arc<Mutex<AuditWriter>>,
    budget: OutputBudget,
    /// 当前允许操作的主机集合。空集表示不允许任何主机
    scope: Vec<HostId>,
    session_id: String,
    /// `agent` 或 MCP client id
    caller: String,
}

impl ToolContext {
    pub fn new(
        executor: Arc<dyn Executor>,
        policy: Arc<PolicyEngine>,
        audit: Arc<Mutex<AuditWriter>>,
        scope: Vec<HostId>,
        session_id: impl Into<String>,
        caller: impl Into<String>,
    ) -> Self {
        Self {
            executor,
            policy,
            audit,
            budget: OutputBudget::default(),
            scope,
            session_id: session_id.into(),
            caller: caller.into(),
        }
    }

    pub fn executor(&self) -> &Arc<dyn Executor> {
        &self.executor
    }

    pub fn policy(&self) -> &Arc<PolicyEngine> {
        &self.policy
    }

    pub fn audit(&self) -> &Arc<Mutex<AuditWriter>> {
        &self.audit
    }

    pub fn budget(&self) -> OutputBudget {
        self.budget
    }

    pub fn scope(&self) -> &[HostId] {
        &self.scope
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn caller(&self) -> &str {
        &self.caller
    }

    /// scope 强制。任何工具在触碰主机前必须调用。
    ///
    /// 这是外部 MCP client 无法越权的关键：未授权的主机
    /// 连出现在 get_environment 结果里的机会都没有，
    /// 即使 Agent 凭空猜出主机名，也会在这里被挡下。
    pub fn check_scope(&self, host: &HostId) -> Result<()> {
        if self.scope.contains(host) {
            Ok(())
        } else {
            Err(AresError::OutOfScope(host.to_string()))
        }
    }
}

/// 工具的静态规格。用于生成 LLM 的 tools 定义与 MCP 的 tools 列表。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub category: ToolCategory,
    /// JSON Schema（object 类型）
    pub parameters: serde_json::Value,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn category(&self) -> ToolCategory;
    /// 给 LLM 看的说明。要写清楚何时该用、何时不该用。
    fn description(&self) -> &'static str;
    /// 参数的 JSON Schema，必须是 `{"type":"object", ...}`
    fn parameters(&self) -> serde_json::Value;

    async fn call(&self, ctx: &ToolContext, args: serde_json::Value) -> Result<ToolOutput>;

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            category: self.category(),
            parameters: self.parameters(),
        }
    }
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<&'static str, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name(), tool);
    }

    pub fn get(&self, name: &str) -> Result<Arc<dyn Tool>> {
        self.tools
            .get(name)
            .cloned()
            .ok_or_else(|| AresError::UnknownTool(name.to_string()))
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.tools.keys().copied().collect()
    }

    /// 全部工具规格。顺序稳定（BTreeMap），保证 prompt 可缓存。
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ares_core::config::HostsConfig;
    use ares_exec::LocalExecutor;
    use ares_policy::PolicyConfig;
    use serde_json::json;

    struct DummyTool;

    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &'static str {
            "dummy"
        }
        fn category(&self) -> ToolCategory {
            ToolCategory::Read
        }
        fn description(&self) -> &'static str {
            "测试用工具"
        }
        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object", "properties": {}, "required": []})
        }
        async fn call(&self, _ctx: &ToolContext, _args: serde_json::Value) -> Result<ToolOutput> {
            Ok(ToolOutput::text("ok"))
        }
    }

    fn test_ctx(scope: Vec<HostId>) -> ToolContext {
        let tmp = tempfile::tempdir().unwrap();
        let policy = PolicyEngine::new(
            PolicyConfig::load_from("/nonexistent").unwrap(),
            HostsConfig::default(),
        )
        .unwrap();
        let audit = AuditWriter::open_at(tmp.path()).unwrap();
        // tempdir 在此泄漏，测试进程结束时由系统清理
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

    #[test]
    fn registry_lookup_and_listing() {
        let mut r = ToolRegistry::new();
        assert!(r.is_empty());
        r.register(Arc::new(DummyTool));

        assert_eq!(r.names(), vec!["dummy"]);
        assert!(r.get("dummy").is_ok());
        assert!(r.get("nope").is_err());
    }

    #[test]
    fn spec_carries_schema_and_category() {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(DummyTool));
        let specs = r.specs();

        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "dummy");
        assert_eq!(specs[0].category, ToolCategory::Read);
        assert_eq!(specs[0].parameters["type"], "object");
    }

    #[test]
    fn scope_check_allows_listed_host() {
        let ctx = test_ctx(vec![HostId::localhost()]);
        assert!(ctx.check_scope(&HostId::localhost()).is_ok());
    }

    #[test]
    fn scope_check_rejects_unlisted_host() {
        let ctx = test_ctx(vec![HostId::localhost()]);
        let err = ctx.check_scope(&HostId::new("prod-web-01")).unwrap_err();
        assert!(err.to_string().contains("scope"));
    }

    #[test]
    fn empty_scope_rejects_everything() {
        // 未授权的调用方（如未在 mcp.toml 中列出的 client）看到的就是这个
        let ctx = test_ctx(vec![]);
        assert!(ctx.check_scope(&HostId::localhost()).is_err());
    }

    #[tokio::test]
    async fn tool_can_be_called_through_registry() {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(DummyTool));
        let ctx = test_ctx(vec![HostId::localhost()]);

        let out = r.get("dummy").unwrap().call(&ctx, json!({})).await.unwrap();
        assert_eq!(out.content, "ok");
    }
}
