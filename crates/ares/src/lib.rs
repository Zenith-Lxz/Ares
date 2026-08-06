//! ARES 库入口（Tauri 迁移版）。
//!
//! 分支 feat/tauri-migration：主仓库仍是 egui 版（main.rs 保留），
//! 本 lib 暴露 Tauri 壳（src-tauri/）所需的模块：
//! - `pty`：PTY 会话（从 gui/session.rs 提取，删 vt100，双线程推送）
//! - `commands`：Tauri command 薄层（Phase 1 为 mock 签名）
//! - `vault`：本地加密凭据库（原样，未改动一行）

#[cfg(feature = "tauri")]
pub mod commands;
#[cfg(feature = "tauri")]
pub mod pty;
pub mod vault;
