//! 审计 commands（方案 §5.5，Phase 1 mock）。

use tauri::State;

use super::AppState;
use super::CmdError;

#[derive(serde::Serialize)]
pub struct AuditRecord {
    pub seq: u64,
    pub ts: String,
    pub actor: String,
    pub action: String,
    pub host: String,
    pub summary: String,
}

#[derive(serde::Serialize)]
pub struct VerifyReport {
    pub ok: bool,
    pub checked: usize,
    pub broken: Vec<u64>,
}

/// 审计查询。Phase 1 mock：空列表。
#[tauri::command]
pub async fn audit_query(
    _state: State<'_, AppState>,
    _filter: Option<String>,
) -> Result<Vec<AuditRecord>, CmdError> {
    Ok(Vec::new())
}

/// 审计链校验。Phase 1 mock：通过。
#[tauri::command]
pub async fn audit_verify(_state: State<'_, AppState>) -> Result<VerifyReport, CmdError> {
    Ok(VerifyReport {
        ok: true,
        checked: 0,
        broken: Vec::new(),
    })
}
