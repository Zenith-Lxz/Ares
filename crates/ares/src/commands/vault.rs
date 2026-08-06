//! 凭据 commands（方案 §5.5，Phase 1 mock）。
//!
//! ★ 安全铁律：不提供 vault_get —— 凭据永不进前端进程。
//! 密码只在 Rust 侧从 vault 取出直接喂给 ssh askpass。

use tauri::State;

use super::AppState;
use super::CmdError;

/// 凭据存在性。Phase 1 mock：false。
#[tauri::command]
pub async fn vault_has(_state: State<'_, AppState>, _alias: String) -> Result<bool, CmdError> {
    Ok(false)
}

/// 写入凭据。Phase 1 mock：接受即成功。
#[tauri::command]
pub async fn vault_set(
    _state: State<'_, AppState>,
    _alias: String,
    _secret: String,
) -> Result<(), CmdError> {
    Ok(())
}
