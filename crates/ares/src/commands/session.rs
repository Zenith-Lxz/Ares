//! 会话管理 commands（方案 §5.1，Phase 1 mock）。

use crate::pty::{PtyChunk, SessionId};
use tauri::ipc::Channel;
use tauri::State;

use super::AppState;
use super::CmdError;

#[derive(serde::Serialize)]
pub struct SessionInfo {
    pub id: SessionId,
    /// "local" | "ssh"
    pub kind: String,
    pub host_alias: Option<String>,
    pub title: String,
    pub connected: bool,
    pub cols: u16,
    pub rows: u16,
}

/// 创建会话。host_alias 为 None 时起本地 shell。
/// Phase 1 mock：不真正创建，返回固定 id。
#[tauri::command]
pub async fn session_create(
    _state: State<'_, AppState>,
    host_alias: Option<String>,
    _cols: u16,
    _rows: u16,
) -> Result<SessionInfo, CmdError> {
    Ok(SessionInfo {
        id: 1,
        kind: if host_alias.is_some() { "ssh" } else { "local" }.into(),
        host_alias,
        title: "mock".into(),
        connected: false,
        cols: 100,
        rows: 30,
    })
}

/// 订阅 PTY 输出流。Phase 1 mock：接收 Channel 但不推流。
#[tauri::command]
pub async fn session_subscribe(
    _state: State<'_, AppState>,
    _id: SessionId,
    _channel: Channel<PtyChunk>,
) -> Result<(), CmdError> {
    Ok(())
}

/// 键盘输入写入 PTY。Phase 1 mock。
#[tauri::command]
pub async fn session_write(
    _state: State<'_, AppState>,
    _id: SessionId,
    _data: String,
) -> Result<(), CmdError> {
    Ok(())
}

/// 窗口尺寸变化。Phase 1 mock。
#[tauri::command]
pub async fn session_resize(
    _state: State<'_, AppState>,
    _id: SessionId,
    _cols: u16,
    _rows: u16,
) -> Result<(), CmdError> {
    Ok(())
}

/// 关闭会话。Phase 1 mock。
#[tauri::command]
pub async fn session_close(_state: State<'_, AppState>, _id: SessionId) -> Result<(), CmdError> {
    Ok(())
}

/// 会话列表。Phase 1 mock：空列表。
#[tauri::command]
pub async fn session_list(_state: State<'_, AppState>) -> Result<Vec<SessionInfo>, CmdError> {
    Ok(Vec::new())
}
