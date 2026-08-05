//! 输出预算与落盘。
//!
//! 超过阈值的输出落盘保存，只把头尾若干行连同一个引用 ID 交给 Agent。
//! Agent 需要细节时用 read_stored_output 按需分段取回。

use ares_core::{paths, redact, AresError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy)]
pub struct OutputBudget {
    /// 超过此字节数即落盘
    pub max_bytes: usize,
    /// 保留的头部行数
    pub head_lines: usize,
    /// 保留的尾部行数
    pub tail_lines: usize,
}

impl Default for OutputBudget {
    fn default() -> Self {
        Self {
            max_bytes: 4096,
            head_lines: 20,
            tail_lines: 20,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BudgetedOutput {
    /// 交给 Agent 的文本（已脱敏）
    pub text: String,
    /// 落盘引用 ID。未落盘时为 None
    pub stored_ref: Option<String>,
    pub total_lines: usize,
    pub truncated: bool,
}

impl OutputBudget {
    /// 对原始输出应用预算。
    ///
    /// 无论是否截断，返回的文本都已脱敏 —— 脱敏发生在落盘之前，
    /// 所以落盘文件里也不含明文凭据。
    pub fn apply(&self, raw: &str) -> Result<BudgetedOutput> {
        let clean = redact::redact(raw);
        let total_lines = clean.lines().count();

        if clean.len() <= self.max_bytes {
            return Ok(BudgetedOutput {
                text: clean,
                stored_ref: None,
                total_lines,
                truncated: false,
            });
        }

        let ref_id = store(&clean)?;

        let lines: Vec<&str> = clean.lines().collect();
        let head: Vec<&str> = lines.iter().take(self.head_lines).copied().collect();
        let tail: Vec<&str> = lines
            .iter()
            .skip(lines.len().saturating_sub(self.tail_lines))
            .copied()
            .collect();
        let omitted = total_lines.saturating_sub(head.len() + tail.len());

        let text = format!(
            "{}\n\n… 省略 {} 行（共 {} 行）。完整输出已保存，用 read_stored_output 取回：ref={} …\n\n{}",
            head.join("\n"),
            omitted,
            total_lines,
            ref_id,
            tail.join("\n"),
        );

        Ok(BudgetedOutput {
            text,
            stored_ref: Some(ref_id),
            total_lines,
            truncated: true,
        })
    }
}

/// 落盘保存，返回引用 ID（内容哈希前 16 位）。
///
/// 用内容哈希做 ID 而非随机数：相同输出天然去重，
/// 50 台机器返回同样结果时只占一份磁盘。
fn store(content: &str) -> Result<String> {
    paths::ensure_dirs()?;
    let id = blake3::hash(content.as_bytes()).to_hex()[..16].to_string();
    let path = outputs_path(&id);
    if !path.exists() {
        std::fs::write(&path, content)?;
        // 与审计文件一致：0600。outputs 可能含脱敏后的敏感输出
        //（如 config 差异、日志片段），不能让同机其他用户可读。
        let mut perms = std::fs::metadata(&path)?.permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o600);
        std::fs::set_permissions(&path, perms)?;
    }
    Ok(id)
}

fn outputs_path(ref_id: &str) -> PathBuf {
    paths::outputs_dir().join(format!("{ref_id}.txt"))
}

/// 按行区间取回落盘输出。`start` 为 0 起的行号。
pub fn load_stored(ref_id: &str, start: usize, count: usize) -> Result<String> {
    // 防路径穿越：ref_id 必须是纯十六进制
    if ref_id.is_empty() || !ref_id.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(AresError::InvalidArgs(format!("无效的 ref: {ref_id}")));
    }
    let path = outputs_path(ref_id);
    if !path.exists() {
        return Err(AresError::InvalidArgs(format!(
            "ref {ref_id} 不存在或已被清理"
        )));
    }
    let content = std::fs::read_to_string(path)?;
    let selected: Vec<&str> = content.lines().skip(start).take(count).collect();
    Ok(selected.join("\n"))
}

/// 清理过期落盘。保留 30 天内且总量不超过 2GB，先到先删。
///
/// 返回删除的文件数。
pub fn gc() -> Result<usize> {
    const MAX_AGE_SECS: u64 = 30 * 24 * 3600;
    const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;

    let dir = paths::outputs_dir();
    if !dir.exists() {
        return Ok(0);
    }

    let mut entries: Vec<(PathBuf, std::time::SystemTime, u64)> = Vec::new();
    for e in std::fs::read_dir(&dir)? {
        let e = e?;
        let meta = e.metadata()?;
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified()?;
        entries.push((e.path(), modified, meta.len()));
    }

    // 旧的在前
    entries.sort_by_key(|(_, t, _)| *t);

    let now = std::time::SystemTime::now();
    let mut total: u64 = entries.iter().map(|(_, _, s)| *s).sum();
    let mut removed = 0usize;

    for (path, modified, size) in &entries {
        let too_old = now
            .duration_since(*modified)
            .map(|d| d.as_secs() > MAX_AGE_SECS)
            .unwrap_or(false);
        let over_quota = total > MAX_TOTAL_BYTES;

        if too_old || over_quota {
            std::fs::remove_file(path)?;
            total = total.saturating_sub(*size);
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试用独立的数据目录，避免相互干扰。
    fn with_temp_data<T>(f: impl FnOnce() -> T) -> T {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("ARES_DATA_DIR", tmp.path());
        let r = f();
        std::env::remove_var("ARES_DATA_DIR");
        r
    }

    #[test]
    fn short_output_passes_through() {
        with_temp_data(|| {
            let b = OutputBudget::default();
            let out = b.apply("hello\nworld").unwrap();
            assert_eq!(out.text, "hello\nworld");
            assert!(out.stored_ref.is_none());
            assert!(!out.truncated);
            assert_eq!(out.total_lines, 2);
        });
    }

    #[test]
    fn long_output_is_truncated_and_stored() {
        with_temp_data(|| {
            let raw: String = (0..1000).map(|i| format!("line {i}\n")).collect();
            let b = OutputBudget::default();
            let out = b.apply(&raw).unwrap();

            assert!(out.truncated);
            assert_eq!(out.total_lines, 1000);
            let ref_id = out.stored_ref.clone().unwrap();

            // 头尾都在
            assert!(out.text.contains("line 0"));
            assert!(out.text.contains("line 999"));
            // 中间被省略
            assert!(!out.text.contains("line 500"));
            assert!(out.text.contains("省略"));
            assert!(out.text.contains(&ref_id));
        });
    }

    #[test]
    fn stored_output_can_be_read_back_by_range() {
        with_temp_data(|| {
            let raw: String = (0..1000).map(|i| format!("line {i}\n")).collect();
            let out = OutputBudget::default().apply(&raw).unwrap();
            let ref_id = out.stored_ref.unwrap();

            let chunk = load_stored(&ref_id, 500, 3).unwrap();
            assert_eq!(chunk, "line 500\nline 501\nline 502");
        });
    }

    #[test]
    fn output_is_redacted_before_storing() {
        with_temp_data(|| {
            let raw = format!("{}\ntoken: ghp_1234567890abcdefghij\n", "x".repeat(5000));
            let out = OutputBudget::default().apply(&raw).unwrap();
            let ref_id = out.stored_ref.unwrap();

            let stored = std::fs::read_to_string(outputs_path(&ref_id)).unwrap();
            assert!(
                !stored.contains("ghp_1234567890abcdefghij"),
                "落盘文件中不得含有明文凭据"
            );
        });
    }

    #[test]
    fn identical_outputs_share_one_file() {
        with_temp_data(|| {
            let raw: String = (0..1000).map(|i| format!("line {i}\n")).collect();
            let a = OutputBudget::default().apply(&raw).unwrap();
            let b = OutputBudget::default().apply(&raw).unwrap();
            assert_eq!(a.stored_ref, b.stored_ref);

            let count = std::fs::read_dir(paths::outputs_dir()).unwrap().count();
            assert_eq!(count, 1, "相同内容应只落盘一份");
        });
    }

    #[test]
    fn invalid_ref_is_rejected() {
        with_temp_data(|| {
            assert!(load_stored("../../etc/passwd", 0, 10).is_err());
            assert!(load_stored("", 0, 10).is_err());
            assert!(load_stored("not-hex-!!", 0, 10).is_err());
        });
    }

    #[test]
    fn missing_ref_reports_clearly() {
        with_temp_data(|| {
            let err = load_stored("abcdef0123456789", 0, 10).unwrap_err();
            assert!(err.to_string().contains("不存在"));
        });
    }

    #[test]
    fn gc_on_empty_dir_is_noop() {
        with_temp_data(|| {
            paths::ensure_dirs().unwrap();
            assert_eq!(gc().unwrap(), 0);
        });
    }
}
