//! 防篡改审计。
//!
//! 审计的价值不在于「记录了什么」，而在于「能证明没被改过」。
//! hash chain 的成本几乎为零，却让日志从记录变成证据。

pub mod record;
pub mod writer;

pub use record::{AuditRecord, GENESIS_HASH};
pub use writer::{now_rfc3339, AuditWriter};

pub mod verify;
pub use verify::{verify_all, verify_file, BrokenLink, VerifyReport};
