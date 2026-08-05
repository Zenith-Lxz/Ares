//! 审计链完整性校验。
//!
//! 逐条重算哈希并检查链接关系。任何中间记录被修改或删除，
//! 都会导致该处及之后的链接断裂。

use crate::record::{AuditRecord, GENESIS_HASH};
use ares_core::{paths, Result};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenLink {
    pub file: PathBuf,
    /// 0 起的记录序号
    pub index: usize,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifyReport {
    pub total: usize,
    pub files: usize,
    /// 第一处断点（兼容 CLI 单点输出）
    pub broken_at: Option<BrokenLink>,
    /// 全部断点。校验必须继续扫描而非在第一处就停 ——
    /// 否则攻击者只需在文件开头制造一处噪声，后面的问题就不再被报告。
    pub broken_links: Vec<BrokenLink>,
}

impl VerifyReport {
    pub fn is_intact(&self) -> bool {
        self.broken_at.is_none()
    }
}

/// 校验单个审计文件。
///
/// **不提前返回**：发现断点后继续扫描，收集全部断点，
/// 否则一处伪造的噪声会掩盖后面的所有篡改。
pub fn verify_file(path: impl AsRef<Path>) -> Result<VerifyReport> {
    let path = path.as_ref();
    let mut report = VerifyReport {
        files: 1,
        ..Default::default()
    };

    if !path.exists() {
        report.files = 0;
        return Ok(report);
    }

    let f = File::open(path)?;
    let mut expected_prev = GENESIS_HASH.to_string();

    for (index, line) in BufReader::new(f).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let rec: AuditRecord = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let link = BrokenLink {
                    file: path.to_path_buf(),
                    index,
                    reason: format!("JSON 解析失败：{e}"),
                };
                if report.broken_at.is_none() {
                    report.broken_at = Some(link.clone());
                }
                report.broken_links.push(link);
                // 无法解析的记录无法参与链校验，跳过该条继续
                continue;
            }
        };

        if rec.prev_hash != expected_prev {
            let link = BrokenLink {
                file: path.to_path_buf(),
                index,
                reason: format!(
                    "prev_hash 不匹配：期望 {}，实际 {}（说明前面有记录被删除或改动）",
                    &expected_prev[..16.min(expected_prev.len())],
                    &rec.prev_hash[..16.min(rec.prev_hash.len())]
                ),
            };
            if report.broken_at.is_none() {
                report.broken_at = Some(link.clone());
            }
            report.broken_links.push(link);
            // 链条断了：后续记录的 prev_hash 都对不上，逐条记录但不重复刷屏
            expected_prev = rec.hash.clone();
            continue;
        }

        let recomputed = rec.compute_hash(&rec.prev_hash);
        if recomputed != rec.hash {
            let link = BrokenLink {
                file: path.to_path_buf(),
                index,
                reason: "本条内容与其哈希不符（说明这条记录被改动过）".to_string(),
            };
            if report.broken_at.is_none() {
                report.broken_at = Some(link.clone());
            }
            report.broken_links.push(link);
            expected_prev = rec.hash.clone();
            continue;
        }

        expected_prev = rec.hash.clone();
        report.total += 1;
    }

    Ok(report)
}

/// 校验审计目录下的全部文件，按文件名排序依次校验。
///
/// 注意：跨月文件之间不构成链（每月文件独立以创世哈希起始），
/// 这是有意为之 —— 单月文件损坏不应导致后续所有月份都无法校验。
pub fn verify_all() -> Result<VerifyReport> {
    let dir = paths::audit_dir();
    if !dir.exists() {
        return Ok(VerifyReport::default());
    }

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    files.sort();

    let mut total = VerifyReport::default();
    for f in files {
        let r = verify_file(&f)?;
        total.total += r.total;
        total.files += r.files;
        if total.broken_at.is_none() {
            total.broken_at = r.broken_at;
        }
        total.broken_links.extend(r.broken_links);
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::{now_rfc3339, AuditWriter};

    fn rec(cmd: &str) -> AuditRecord {
        AuditRecord::new(
            now_rfc3339(),
            "localhost",
            "terminal_execute",
            cmd,
            Some(0),
            "ok",
            "observer",
            "agent",
            "sess-1",
        )
    }

    fn write_three(dir: &Path) -> PathBuf {
        let mut w = AuditWriter::open_at(dir).unwrap();
        w.append(rec("uptime")).unwrap();
        w.append(rec("df -P")).unwrap();
        w.append(rec("vm_stat")).unwrap();
        w.path().to_path_buf()
    }

    #[test]
    fn intact_chain_verifies() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_three(tmp.path());

        let report = verify_file(&path).unwrap();
        assert!(report.is_intact());
        assert_eq!(report.total, 3);
    }

    #[test]
    fn missing_file_is_intact_and_empty() {
        let report = verify_file("/nonexistent/audit.jsonl").unwrap();
        assert!(report.is_intact());
        assert_eq!(report.total, 0);
        assert_eq!(report.files, 0);
    }

    #[test]
    fn tampered_content_is_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_three(tmp.path());

        // 篡改第二条的命令内容，但保留其原有 hash
        let content = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = content.lines().map(String::from).collect();
        let mut r: AuditRecord = serde_json::from_str(&lines[1]).unwrap();
        r.command = "rm -rf /".into();
        lines[1] = serde_json::to_string(&r).unwrap();
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        let report = verify_file(&path).unwrap();
        assert!(!report.is_intact());
        let broken = report.broken_at.unwrap();
        assert_eq!(broken.index, 1);
        assert!(broken.reason.contains("被改动过"));
    }

    #[test]
    fn deleted_record_is_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_three(tmp.path());

        // 删掉中间一条
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        std::fs::write(&path, format!("{}\n{}\n", lines[0], lines[2])).unwrap();

        let report = verify_file(&path).unwrap();
        assert!(!report.is_intact());
        let broken = report.broken_at.unwrap();
        assert_eq!(broken.index, 1);
        assert!(broken.reason.contains("prev_hash 不匹配"));
    }

    #[test]
    fn appended_forged_record_is_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_three(tmp.path());

        // 伪造一条追加记录，prev_hash 随便填
        let mut forged = rec("whoami");
        forged.prev_hash = "deadbeef".repeat(8);
        forged.hash = "cafebabe".repeat(8);
        let line = serde_json::to_string(&forged).unwrap();
        let mut content = std::fs::read_to_string(&path).unwrap();
        content.push_str(&line);
        content.push('\n');
        std::fs::write(&path, content).unwrap();

        let report = verify_file(&path).unwrap();
        assert!(!report.is_intact());
        assert_eq!(report.broken_at.unwrap().index, 3);
    }

    #[test]
    fn corrupt_json_is_reported_not_panicked() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("2026-08.jsonl");
        std::fs::write(&path, "{not json}\n").unwrap();

        let report = verify_file(&path).unwrap();
        assert!(!report.is_intact());
        assert!(report.broken_at.unwrap().reason.contains("JSON 解析失败"));
    }
}
