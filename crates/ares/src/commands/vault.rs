//! 凭据 commands（方案 §5.5，Phase 1 mock）。
//!
//! ★ 安全铁律：不提供 vault_get —— 凭据永不进前端进程。
//! 密码只在 Rust 侧从 vault 取出直接喂给 ssh askpass。

use tauri::State;

use super::AppState;
use super::CmdError;

/// 凭据存在性（前端只允许问「有没有」，不允许读值）。
#[tauri::command]
pub async fn vault_has(_state: State<'_, AppState>, alias: String) -> Result<bool, CmdError> {
    Ok(crate::vault::get(&format!("ssh-pw:{alias}")).is_some())
}

/// 写入凭据。
#[tauri::command]
pub async fn vault_set(
    _state: State<'_, AppState>,
    alias: String,
    secret: String,
) -> Result<(), CmdError> {
    crate::vault::set(&format!("ssh-pw:{alias}"), &secret).map_err(|e| e.to_string())
}
