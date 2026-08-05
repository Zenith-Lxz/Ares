//! 工具层。
//!
//! 工具设计准则：能用一条 shell 命令拿到的，交给 terminal_execute。
//! 只有当 Agent 需要多次往返才能重建某个状态时，才做成独立工具。

pub mod budget;
pub mod environment;
pub mod memory_tools;
pub mod registry;
pub mod stored;
pub mod terminal;

// 供下游 crate 的测试使用。生产构建中不包含。
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

#[cfg(any(test, feature = "test-support"))]
pub use test_support::test_ctx as test_support_ctx;

pub use budget::{BudgetedOutput, OutputBudget};
pub use environment::GetEnvironmentTool;
pub use memory_tools::{
    MemoryListTool, MemorySearchTool, MemoryWriteTool, SkillCreateTool, SkillListTool,
    SkillViewTool,
};
pub use registry::{Tool, ToolContext, ToolOutput, ToolRegistry, ToolSpec};
pub use stored::ReadStoredOutputTool;
pub use terminal::{PreparedExec, TerminalExecuteTool};

use std::sync::Arc;

/// 装配 M1 的默认工具集。
///
/// get_environment 需要知道全部工具名，因此最后注册。
pub fn default_registry() -> ToolRegistry {
    let mut r = ToolRegistry::new();
    r.register(Arc::new(TerminalExecuteTool::new()));
    r.register(Arc::new(ReadStoredOutputTool));
    // 记忆与技能（2026-08-05 批次1：持久记忆 / 自进化 / skill）
    r.register(Arc::new(MemoryWriteTool));
    r.register(Arc::new(MemorySearchTool));
    r.register(Arc::new(MemoryListTool));
    r.register(Arc::new(SkillListTool));
    r.register(Arc::new(SkillViewTool));
    r.register(Arc::new(SkillCreateTool));

    let mut names: Vec<String> = r.names().iter().map(|s| s.to_string()).collect();
    names.push("get_environment".to_string());
    names.sort();

    r.register(Arc::new(GetEnvironmentTool::new(names)));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_has_full_toolset() {
        let r = default_registry();
        let mut names = r.names();
        names.sort();
        assert_eq!(
            names,
            vec![
                "get_environment",
                "memory_list",
                "memory_search",
                "memory_write",
                "read_stored_output",
                "skill_create",
                "skill_list",
                "skill_view",
                "terminal_execute"
            ]
        );
    }

    #[test]
    fn all_specs_have_object_schema() {
        for spec in default_registry().specs() {
            assert_eq!(
                spec.parameters["type"], "object",
                "工具 {} 的 schema 顶层必须是 object",
                spec.name
            );
            assert!(!spec.description.is_empty());
        }
    }
}
