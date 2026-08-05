//! append-only 审计写入。
//!
//! 每月一个文件，权限 0600。写入时先读回最后一条记录的哈希作为
//! 本条的 prev_hash —— 这保证即使进程重启，链也是连续的。

use crate::record::{AuditRecord, GENESIS_HASH};
use ares_core::{paths, AresError, Result};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub struct AuditWriter {
    path: PathBuf,
    last_hash: String,
}

impl AuditWriter {
    /// 打开默认位置的当月审计文件。
    pub fn open() -> Result<Self> {
        paths::ensure_dirs()?;
        Self::open_at(paths::audit_dir())
    }

    /// 打开指定目录下的当月审计文件。测试用。
    pub fn open_at(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;

        let now = OffsetDateTime::now_utc();
        let path = dir.join(format!("{:04}-{:02}.jsonl", now.year(), now.month() as u8));

        let last_hash = read_last_hash(&path)?;
        Ok(Self { path, last_hash })
    }

    /// 当前链尾哈希。
    pub fn last_hash(&self) -> &str {
        &self.last_hash
    }

    /// 追加一条记录，返回本条的哈希。
    pub fn append(&mut self, mut rec: AuditRecord) -> Result<String> {
        rec.prev_hash = self.last_hash.clone();
        rec.hash = rec.compute_hash(&self.last_hash);

        let line = serde_json::to_string(&rec)?;

        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)
            .map_err(|e| {
                AresError::Config(format!("无法打开审计文件 {}: {e}", self.path.display()))
            })?;
        writeln!(f, "{line}")?;
        f.sync_all()?;

        self.last_hash = rec.hash.clone();
        Ok(rec.hash)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// 读取文件中最后一条记录的哈希。文件不存在或为空时返回创世哈希。
///
/// **尾行损坏容错**：审计文件最后一行被截断（崩溃写一半很现实）时，
/// 跳过损坏行并继续 —— 若这里整体 `?` 失败，任何能写该文件的进程
/// 都能靠写坏尾行瘫痪整个审计（审计 DoS）。损坏行在 verify 时会被
/// 报告为断点（见 Task 9），这里只保证系统可用。
fn read_last_hash(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok(GENESIS_HASH.to_string());
    }
    let f = File::open(path)?;
    let mut last = GENESIS_HASH.to_string();
    let mut skipped_corrupt = false;
    for line in BufReader::new(f).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<AuditRecord>(&line) {
            Ok(rec) => last = rec.hash,
            Err(_) => {
                // 跳过损坏行（只可能是尾部截断；中间损坏由 verify 报告）
                skipped_corrupt = true;
            }
        }
    }
    if skipped_corrupt {
        tracing::warn!("审计文件中存在损坏行，已跳过（请运行 ares audit verify 检查链完整性）");
    }
    Ok(last)
}

/// 当前时间的 RFC3339 表示。
pub fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .expect("RFC3339 formatting cannot fail for a valid OffsetDateTime")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

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

    #[test]
    fn first_record_links_to_genesis() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = AuditWriter::open_at(tmp.path()).unwrap();
        assert_eq!(w.last_hash(), GENESIS_HASH);

        let h = w.append(rec("df -P")).unwrap();
        assert_eq!(h.len(), 64);
        assert_eq!(w.last_hash(), h);
    }

    #[test]
    fn records_form_a_chain() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = AuditWriter::open_at(tmp.path()).unwrap();

        let h1 = w.append(rec("uptime")).unwrap();
        let h2 = w.append(rec("vm_stat")).unwrap();
        assert_ne!(h1, h2);

        let content = std::fs::read_to_string(w.path()).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);

        let r1: AuditRecord = serde_json::from_str(lines[0]).unwrap();
        let r2: AuditRecord = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(r1.prev_hash, GENESIS_HASH);
        assert_eq!(r2.prev_hash, r1.hash);
    }

    #[test]
    fn chain_survives_reopen() {
        let tmp = tempfile::tempdir().unwrap();

        let h1 = {
            let mut w = AuditWriter::open_at(tmp.path()).unwrap();
            w.append(rec("uptime")).unwrap()
        };

        // 模拟进程重启
        let mut w2 = AuditWriter::open_at(tmp.path()).unwrap();
        assert_eq!(w2.last_hash(), h1);

        let h2 = w2.append(rec("df -P")).unwrap();
        let content = std::fs::read_to_string(w2.path()).unwrap();
        let r2: AuditRecord = serde_json::from_str(content.lines().nth(1).unwrap()).unwrap();
        assert_eq!(r2.prev_hash, h1);
        assert_eq!(r2.hash, h2);
    }

    #[test]
    fn file_permissions_are_owner_only() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = AuditWriter::open_at(tmp.path()).unwrap();
        w.append(rec("uptime")).unwrap();

        let meta = std::fs::metadata(w.path()).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn appended_record_is_redacted_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = AuditWriter::open_at(tmp.path()).unwrap();
        w.append(AuditRecord::new(
            now_rfc3339(),
            "localhost",
            "terminal_execute",
            "curl -H 'Authorization: Bearer sk-abcdefghij1234567890'",
            Some(0),
            "ok",
            "confirm",
            "agent",
            "s",
        ))
        .unwrap();

        let content = std::fs::read_to_string(w.path()).unwrap();
        assert!(!content.contains("sk-abcdefghij1234567890"));
    }
}
