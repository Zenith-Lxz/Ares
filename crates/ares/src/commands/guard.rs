//! 命令拦截 commands（方案 §5.2，Phase 1 mock）。

use crate::pty::SessionId;
use tauri::State;

use super::AppState;
use super::CmdError;

#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CommandVerdict {
    /// 直接放行
    Allow,
    /// 需要回车确认
    Confirm { reason: String },
    /// 需要 Touch ID
    TouchId { reason: String },
    /// 禁止，附拒绝理由
    Deny { reason: String },
}

/// 用户按下回车时调用。Phase 1 mock：一律放行。
#[tauri::command]
pub async fn command_check(
    _state: State<'_, AppState>,
    _id: SessionId,
    _command: String,
) -> Result<CommandVerdict, CmdError> {
    Ok(CommandVerdict::Allow)
}

/// TouchId 判定后由前端调用。Phase 1 mock：通过。
#[tauri::command]
pub async fn command_authorize(
    _state: State<'_, AppState>,
    _id: SessionId,
    _command: String,
) -> Result<bool, CmdError> {
    Ok(true)
}
