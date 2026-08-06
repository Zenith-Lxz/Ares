//! PTY 会话（Tauri 迁移版）。
//!
//! 从 `gui/session.rs` 提取，**删除 vt100 解析、\n→\r\n 转换、scrollback 管理**
//! （全部由 xterm.js 负责，交接文档第 4 号坑「裸 LF 不回车」自动消失）。
//!
//! 数据流为 Spike 实测修正后的**双线程模型**（方案 §6.1）：
//! - 读线程：阻塞读 PTY → 共享缓冲（不在此处 flush）
//! - flush 线程：每 16ms 取走缓冲批量 base64 → Channel
//!
//! 单线程「读后检查 flush」会让末批数据永久卡死（方案坑 #14）。

mod session;

pub use session::{PtyChunk, Session, SessionId, SessionKind};
