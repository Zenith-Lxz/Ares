//! 审计记录定义。
//!
//! 字段顺序即 JSON 序列化顺序，且参与哈希计算 —— 不要随意调整。

use serde::{Deserialize, Serialize};

/// 一条审计记录。
///
/// `prev_hash` 与 `hash` 构成链：任何中间记录被修改或删除，
/// 后续记录的哈希都对不上，`verify` 会立即发现。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditRecord {
    /// RFC3339 时间戳
    pub ts: String,
    /// 主机标识
    pub host: String,
    /// 工具名
    pub tool: String,
    /// 完整命令（已脱敏）
    pub command: String,
    /// 退出码。非执行类操作为 None
    pub exit_code: Option<i32>,
    /// 输出摘要（已脱敏，截断到 512 字符）
    pub output_digest: String,
    /// 审批决定的稳定标签：deny / confirm / auto / observer / rejected / timeout
    pub decision: String,
    /// 调用方：`agent` 或 MCP client id
    pub caller: String,
    /// 会话 ID
    pub session_id: String,
    /// 命中的策略规则描述
    pub policy_hit: Option<String>,
    /// 回滚命令（若该操作可回滚）
    pub rollback: Option<String>,
    /// 使用的模型
    pub model: Option<String>,
    /// token 用量：(input, output)
    pub tokens: Option<(u32, u32)>,
    /// 前一条记录的哈希。链首为全 0
    pub prev_hash: String,
    /// 本条哈希 = blake3(prev_hash + 本条除 hash 外的 JSON)
    pub hash: String,
}

/// 链首的哨兵哈希。
pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// 输出摘要的最大长度。超出部分截断 —— 审计记录的价值在于可检索，
/// 不在于存全量输出（全量输出由 outputs/ 落盘负责）。
const DIGEST_MAX: usize = 512;

impl AuditRecord {
    /// 构造一条尚未计算哈希的记录。
    ///
    /// `command` 与 `output` 会在此处完成脱敏，调用方无需预先处理 ——
    /// 把脱敏放在唯一入口，避免任何一处遗漏。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ts: String,
        host: impl Into<String>,
        tool: impl Into<String>,
        command: &str,
        exit_code: Option<i32>,
        output: &str,
        decision: impl Into<String>,
        caller: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        let mut digest = ares_core::redact::redact(output);
        if digest.chars().count() > DIGEST_MAX {
            digest = digest.chars().take(DIGEST_MAX).collect::<String>() + "…（已截断）";
        }
        Self {
            ts,
            host: host.into(),
            tool: tool.into(),
            command: ares_core::redact::redact(command),
            exit_code,
            output_digest: digest,
            decision: decision.into(),
            caller: caller.into(),
            session_id: session_id.into(),
            policy_hit: None,
            rollback: None,
            model: None,
            tokens: None,
            prev_hash: String::new(),
            hash: String::new(),
        }
    }

    pub fn with_policy_hit(mut self, hit: impl Into<String>) -> Self {
        self.policy_hit = Some(hit.into());
        self
    }

    pub fn with_rollback(mut self, cmd: impl Into<String>) -> Self {
        self.rollback = Some(cmd.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>, input: u32, output: u32) -> Self {
        self.model = Some(model.into());
        self.tokens = Some((input, output));
        self
    }

    /// 计算本条记录的哈希。
    ///
    /// 输入是 `prev_hash` 拼上「hash 字段置空后的完整 JSON」——
    /// 置空而非省略，是为了让序列化结构稳定。
    pub(crate) fn compute_hash(&self, prev_hash: &str) -> String {
        let mut probe = self.clone();
        probe.prev_hash = prev_hash.to_string();
        probe.hash = String::new();
        let json = serde_json::to_string(&probe).expect("audit record must serialize");

        let mut hasher = blake3::Hasher::new();
        hasher.update(prev_hash.as_bytes());
        hasher.update(json.as_bytes());
        hasher.finalize().to_hex().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> AuditRecord {
        AuditRecord::new(
            "2026-08-05T10:00:00Z".into(),
            "localhost",
            "terminal_execute",
            "df -P",
            Some(0),
            "Filesystem 1024-blocks",
            "observer",
            "agent",
            "sess-1",
        )
    }

    #[test]
    fn new_redacts_command_and_output() {
        let rec = AuditRecord::new(
            "2026-08-05T10:00:00Z".into(),
            "localhost",
            "terminal_execute",
            "mysql --password=hunter2",
            Some(0),
            "token: ghp_1234567890abcdefghij",
            "confirm",
            "agent",
            "sess-1",
        );
        assert!(!rec.command.contains("hunter2"));
        assert!(!rec.output_digest.contains("ghp_1234567890abcdefghij"));
    }

    #[test]
    fn long_output_is_truncated() {
        let long = "x".repeat(2000);
        let rec = AuditRecord::new(
            "t".into(),
            "h",
            "tool",
            "cmd",
            None,
            &long,
            "auto",
            "agent",
            "s",
        );
        assert!(rec.output_digest.chars().count() <= DIGEST_MAX + 10);
        assert!(rec.output_digest.ends_with("（已截断）"));
    }

    #[test]
    fn hash_is_deterministic() {
        let rec = sample();
        let h1 = rec.compute_hash(GENESIS_HASH);
        let h2 = rec.compute_hash(GENESIS_HASH);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn hash_changes_with_prev_hash() {
        let rec = sample();
        let h1 = rec.compute_hash(GENESIS_HASH);
        let h2 = rec.compute_hash("ffff");
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_changes_with_content() {
        let a = sample();
        let mut b = sample();
        b.command = "rm -rf /tmp/x".into();
        assert_ne!(a.compute_hash(GENESIS_HASH), b.compute_hash(GENESIS_HASH));
    }

    #[test]
    fn builders_set_optional_fields() {
        let rec = sample()
            .with_policy_hit("builtin:observer:df")
            .with_rollback("true")
            .with_model("claude-opus-5", 100, 50);
        assert_eq!(rec.policy_hit.as_deref(), Some("builtin:observer:df"));
        assert_eq!(rec.rollback.as_deref(), Some("true"));
        assert_eq!(rec.tokens, Some((100, 50)));
    }
}
