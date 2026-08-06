//! 主机管理 commands（方案 §5.3，Phase 1 mock）。

use tauri::State;

use super::AppState;
use super::CmdError;

#[derive(serde::Serialize)]
pub struct HostEntry {
    pub alias: String,
    pub hostname: String,
    pub port: u16,
    pub user: String,
    pub env: String, // prod | staging | dev | local
    pub tags: Vec<String>,
    pub note: String,
    pub reachable: Option<bool>,
}

/// 主机列表。Phase 1 mock：两条示例。
#[tauri::command]
pub async fn host_list(_state: State<'_, AppState>) -> Result<Vec<HostEntry>, CmdError> {
    Ok(vec![
        HostEntry {
            alias: "测试".into(),
            hostname: "10.8.8.34".into(),
            port: 22,
            user: "root".into(),
            env: "dev".into(),
            tags: vec!["spike".into()],
            note: "Phase 1 mock".into(),
            reachable: None,
        },
        HostEntry {
            alias: "生产A".into(),
            hostname: "10.8.8.151".into(),
            port: 22,
            user: "root".into(),
            env: "prod".into(),
            tags: vec!["web".into()],
            note: "Phase 1 mock".into(),
            reachable: None,
        },
    ])
}

/// 主机详情。Phase 1 mock：返回第一条。
#[tauri::command]
pub async fn host_get(_state: State<'_, AppState>, alias: String) -> Result<HostEntry, CmdError> {
    host_list(_state)
        .await?
        .into_iter()
        .find(|h| h.alias == alias)
        .ok_or_else(|| format!("host {alias} not found"))
}

/// 触发后台连通性探测。Phase 1 mock。
#[tauri::command]
pub async fn host_probe(
    _state: State<'_, AppState>,
    _aliases: Vec<String>,
) -> Result<(), CmdError> {
    Ok(())
}
