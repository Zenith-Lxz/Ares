//! ARES 领域类型。
//!
//! 这些类型贯穿全部 crate，是各模块之间的通用语言。

use serde::{Deserialize, Serialize};
use std::fmt;

/// 主机标识符。
///
/// 取值为 `~/.ssh/config` 中的 Host 别名，或本机的 `localhost`。
/// 不使用 IP 作为标识 —— IP 会变，别名是稳定的。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HostId(pub String);

impl HostId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// 本机伪主机。M1 的全部操作都发生在这上面。
    pub fn localhost() -> Self {
        Self("localhost".to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HostId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `&str` → `HostId`（Task 12 测试与配置加载用 `.into()` 构造）。
impl From<&str> for HostId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// 主机环境等级。决定权限策略的严格程度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Env {
    Prod,
    Staging,
    Dev,
    /// 本机
    Local,
    /// 未登记主机（在 hosts.toml 中没有条目，或条目未标注 env）。
    /// 安全默认：**未知即最严** —— 变更操作强制确认（auto 不放行），
    /// 与 spec §7.1「未知主机按最严保护」一致。
    #[default]
    Unknown,
}

impl Env {
    /// 该环境下的变更操作是否强制确认（auto 规则不放行）。
    ///
    /// prod 与 unknown（未登记主机）返回 true —— 未知环境没有理由
    /// 比显式标注的 prod 宽松。其他环境的默认级别仍是 Confirm，
    /// 但显式 auto 规则可以放行，见 PolicyEngine。
    pub fn requires_confirm_for_mutation(self) -> bool {
        matches!(self, Env::Prod | Env::Unknown)
    }
}

impl fmt::Display for Env {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Env::Prod => "prod",
            Env::Staging => "staging",
            Env::Dev => "dev",
            Env::Local => "local",
            Env::Unknown => "unknown",
        };
        f.write_str(s)
    }
}

/// 工具类别。决定默认权限判定路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCategory {
    /// 无副作用的读取
    Read,
    /// 执行任意命令 —— 副作用取决于命令内容
    Exec,
    /// 明确的写入
    Write,
}

impl ToolCategory {
    /// 是否为变更类操作（需要走审批路径）。
    pub fn is_mutating(self) -> bool {
        matches!(self, ToolCategory::Exec | ToolCategory::Write)
    }
}

/// 策略判定结果。四个级别，优先级由高到低。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Decision {
    /// 硬禁止 —— 不执行，且不提供任何确认途径
    Deny { reason: String },
    /// 需要显式确认（终端 y/n / 页面确认按钮）。这是兜底的默认级别。
    /// `rule` 说明确认原因（命中危险模式 / prod 环境 / 无放行规则），
    /// `critical` 标记极高危 —— 确认之外还要求输入完整主机名（spec §14.2）。
    Confirm { rule: String, critical: bool },
    /// 自动执行 —— 仅来自 policy.toml 中显式配置的规则
    Auto { rule: String },
    /// 自动执行 —— 无副作用的只读操作
    Observer,
}

impl Decision {
    /// 是否被拒绝执行。
    pub fn is_blocked(&self) -> bool {
        matches!(self, Decision::Deny { .. })
    }

    /// 是否需要人工介入（确认或拒绝）。
    pub fn needs_interaction(&self) -> bool {
        matches!(self, Decision::Confirm { .. })
    }

    /// 用于审计记录与界面显示的稳定标签。
    pub fn label(&self) -> &'static str {
        match self {
            Decision::Deny { .. } => "deny",
            Decision::Confirm { .. } => "confirm",
            Decision::Auto { .. } => "auto",
            Decision::Observer => "observer",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostid_displays_as_plain_string() {
        assert_eq!(HostId::new("web-01").to_string(), "web-01");
        assert_eq!(HostId::localhost().as_str(), "localhost");
    }

    #[test]
    fn only_prod_forces_confirm_for_mutation() {
        assert!(Env::Prod.requires_confirm_for_mutation());
        assert!(!Env::Staging.requires_confirm_for_mutation());
        assert!(!Env::Dev.requires_confirm_for_mutation());
        assert!(!Env::Local.requires_confirm_for_mutation());
    }

    #[test]
    fn env_defaults_to_unknown() {
        // 未标注 env 的主机回落到 Unknown（未知即最严）：
        // 不可能是 dev（会放行变更），也不可能是 local（会绕过远程检查）
        assert_eq!(Env::default(), Env::Unknown);
        assert!(Env::Unknown.requires_confirm_for_mutation());
        assert!(Env::Prod.requires_confirm_for_mutation());
        assert!(!Env::Dev.requires_confirm_for_mutation());
        assert!(!Env::Local.requires_confirm_for_mutation());
    }

    #[test]
    fn exec_and_write_are_mutating() {
        assert!(!ToolCategory::Read.is_mutating());
        assert!(ToolCategory::Exec.is_mutating());
        assert!(ToolCategory::Write.is_mutating());
    }

    #[test]
    fn decision_classification() {
        let deny = Decision::Deny {
            reason: "test".into(),
        };
        assert!(deny.is_blocked());
        assert!(!deny.needs_interaction());
        assert_eq!(deny.label(), "deny");

        let confirm = Decision::Confirm {
            rule: "builtin".into(),
            critical: false,
        };
        assert!(!confirm.is_blocked());
        assert!(confirm.needs_interaction());

        assert!(Decision::Confirm {
            rule: "default".into(),
            critical: false
        }
        .needs_interaction());
        assert!(!Decision::Observer.needs_interaction());
        assert!(!Decision::Auto { rule: "r1".into() }.needs_interaction());
    }

    #[test]
    fn decision_serializes_with_kind_tag() {
        let json = serde_json::to_string(&Decision::Confirm {
            rule: "r".into(),
            critical: false,
        })
        .unwrap();
        assert!(json.contains(r#""kind":"confirm""#));
        assert!(json.contains(r#""rule":"r""#));
        assert!(json.contains(r#""critical":false"#));

        let json = serde_json::to_string(&Decision::Deny { reason: "x".into() }).unwrap();
        assert!(json.contains(r#""kind":"deny""#));
        assert!(json.contains(r#""reason":"x""#));
    }
}
