//! policy.toml 加载。
//!
//! 配置只能**追加**规则，不能删除内置项。这是刻意的不对称：
//! 让护栏更严容易，让护栏更松必须显式且有据可查。

use ares_core::{paths, AresError, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// 硬禁止段。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DenySection {
    /// 追加到内置清单的全局禁止命令
    #[serde(default)]
    pub commands: Vec<String>,
    /// 按主机集合限定的禁止规则
    #[serde(default)]
    pub rules: Vec<DenyRule>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DenyRule {
    /// 主机选择器：主机名、`@tag`、或 `*`
    pub hosts: String,
    pub commands: Vec<String>,
    /// 拒绝时展示给 Agent 与用户的理由。必填 ——
    /// 没有理由的禁令，半年后没人记得为什么加的
    pub reason: String,
}

// 危险命令无需配置段 —— 默认级别 confirm 已覆盖（2026-08-05 决策）。
// 原 `[touchid]` 段（commands + host_count_threshold）已废弃；配置中出现
// `[touchid]` 表会因 deny_unknown_fields 解析失败（显式报错，不静默忽略）。

/// 只读白名单段。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObserverSection {
    #[serde(default)]
    pub commands: Vec<String>,
    /// 设为 true 则整体关闭 observer 白名单，只读也逐条确认
    #[serde(default)]
    pub strict: bool,
}

/// 显式放行规则。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutoRule {
    /// 主机选择器
    pub hosts: String,
    /// 适用的工具名
    #[serde(default)]
    pub tools: Vec<String>,
    /// 命令模式
    #[serde(default)]
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyConfig {
    #[serde(default)]
    pub deny: DenySection,
    #[serde(default)]
    pub observer: ObserverSection,
    #[serde(default)]
    pub auto: Vec<AutoRule>,
}

impl PolicyConfig {
    pub fn load() -> Result<Self> {
        Self::load_from(paths::config_dir().join("policy.toml"))
    }

    pub fn load_from(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            // 无配置文件时，内置清单依然生效 —— 这是安全默认值
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| AresError::Config(format!("无法读取 {}: {e}", path.display())))?;
        let cfg: Self = toml::from_str(&text)
            .map_err(|e| AresError::Config(format!("解析 {} 失败：{e}", path.display())))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// 配置校验（加载后立即执行）：
    /// - auto 规则必须至少声明 tools 或 commands 之一 —— 空规则 = 全放行，
    ///   漏写字段的 `[[auto]] hosts = "*"` 会静默变成「该主机一切命令自动执行」
    fn validate(&self) -> Result<()> {
        for (i, rule) in self.auto.iter().enumerate() {
            if rule.tools.is_empty() && rule.commands.is_empty() {
                return Err(AresError::Config(format!(
                    "auto 规则 #{}（hosts={:?}）未声明任何 tools 或 commands —— \
                     空规则会把匹配主机的一切命令设为自动执行，请显式声明至少一项",
                    i + 1,
                    rule.hosts
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn missing_file_yields_safe_defaults() {
        let cfg = PolicyConfig::load_from("/nonexistent/policy.toml").unwrap();
        assert!(cfg.deny.commands.is_empty());
        assert!(!cfg.observer.strict);
    }

    #[test]
    fn parses_full_policy() {
        let f = write_temp(
            r#"
[deny]
commands = ["fdisk *"]

[[deny.rules]]
hosts = "@prod"
commands = ["systemctl stop *", "reboot"]
reason = "生产停机必须走变更流程"

[observer]
commands = ["mycli status"]
strict = false

[[auto]]
hosts = "@dev"
tools = ["terminal_execute"]
commands = ["docker restart *"]
"#,
        );
        let cfg = PolicyConfig::load_from(f.path()).unwrap();

        assert_eq!(cfg.deny.commands, vec!["fdisk *"]);
        assert_eq!(cfg.deny.rules.len(), 1);
        assert_eq!(cfg.deny.rules[0].hosts, "@prod");
        assert_eq!(cfg.deny.rules[0].reason, "生产停机必须走变更流程");
        assert_eq!(cfg.auto.len(), 1);
        assert_eq!(cfg.auto[0].commands, vec!["docker restart *"]);
    }

    #[test]
    fn strict_observer_can_be_enabled() {
        let f = write_temp("[observer]\nstrict = true\n");
        let cfg = PolicyConfig::load_from(f.path()).unwrap();
        assert!(cfg.observer.strict);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        // deny_unknown_fields：spec 示例里的 `builtin = true`、
        // `conditions = [...]` 等不存在的字段必须解析失败，
        // 不能静默丢弃（静默丢弃 = 用户以为护栏生效，实际没有）。
        let f = write_temp("[deny]\nbuiltin = true\ncommands = [\"fdisk *\"]\n");
        assert!(PolicyConfig::load_from(f.path()).is_err());

        // 已废弃的 `[touchid]` 段必须解析失败（显式报错，而不是静默忽略后失去预期护栏）
        let f = write_temp("[touchid]\ncommands = [\"x\"]\n");
        assert!(PolicyConfig::load_from(f.path()).is_err());
    }

    #[test]
    fn deny_rule_requires_reason() {
        // reason 无默认值，缺失时解析必须失败
        let f = write_temp(
            r#"
[[deny.rules]]
hosts = "@prod"
commands = ["reboot"]
"#,
        );
        assert!(PolicyConfig::load_from(f.path()).is_err());
    }
}
