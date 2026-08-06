//! 会话管理 commands（方案 §5.1，Phase 2 真实实现）。
//!
//! 密码链路（★ 前端无任何读凭据 command）：
//! 1. session_create 查 vault（`ssh-pw:<alias>`）
//! 2. 有密码 → askpass 走 vault-get 直接连
//! 3. 无密码 → 返回 `NeedPassword { alias }`（不是报错）
//! 4. 前端弹窗 → session_provide_password 写入 vault → 重新 session_create

use crate::commands::AppState;
use crate::commands::CmdError;
use crate::pty::{PtyChunk, Session, SessionId};
use tauri::ipc::Channel;
use tauri::State;

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

/// session_create 的结果：直接建好或需要密码。
#[derive(serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SessionCreateOutcome {
    Ok(SessionInfo),
    NeedPassword { alias: String },
}

fn session_info(s: &Session) -> SessionInfo {
    SessionInfo {
        id: s.id(),
        kind: match s.kind() {
            crate::pty::SessionKind::Local => "local",
            crate::pty::SessionKind::Ssh => "ssh",
        }
        .into(),
        host_alias: s.host_alias().map(str::to_string),
        title: s
            .host_alias()
            .map(str::to_string)
            .unwrap_or_else(|| "本地".into()),
        connected: true,
        cols: 0,
        rows: 0,
    }
}

/// 创建会话。host_alias = None → 本地 shell；Some(alias) → hosts.toml 取连接参数 + ssh。
#[tauri::command]
pub async fn session_create(
    state: State<'_, AppState>,
    host_alias: Option<String>,
    cols: u16,
    rows: u16,
) -> Result<SessionCreateOutcome, CmdError> {
    // 会话 id（AppState 单调递增）
    let id = {
        let mut n = state.next_id.lock().map_err(|e| e.to_string())?;
        *n += 1;
        *n
    };

    let session = match host_alias.as_deref() {
        None => Session::spawn_local(cols, rows, id).map_err(|e| e.to_string())?,
        Some(alias) => {
            // 主机簿（hosts.toml 唯一事实源）
            let config =
                ares_core::config::HostsConfig::load().map_err(|e| format!("hosts 配置加载失败: {e}"))?;
            let entry = config
                .hosts
                .get(alias)
                .ok_or_else(|| format!("主机 {alias} 不在 hosts.toml"))?;
            // 密码检查：无密码 → NeedPassword（不创建会话）
            if crate::vault::get(&format!("ssh-pw:{alias}")).is_none() {
                return Ok(SessionCreateOutcome::NeedPassword {
                    alias: alias.to_string(),
                });
            }
            let target = format!("{}@{}", entry.user, entry.hostname);
            Session::spawn_ssh(&target, alias, entry.port.unwrap_or(22), cols, rows, id)
                .map_err(|e| e.to_string())?
        }
    };

    let info = session_info(&session);
    state
        .sessions
        .lock()
        .map_err(|e| e.to_string())?
        .insert(id, session);
    Ok(SessionCreateOutcome::Ok(info))
}

/// 订阅 PTY 输出流（双线程推送，方案 §6.1）。
#[tauri::command]
pub async fn session_subscribe(
    state: State<'_, AppState>,
    id: SessionId,
    channel: Channel<PtyChunk>,
) -> Result<(), CmdError> {
    let s = state.sessions.lock().map_err(|e| e.to_string())?;
    let session = s.get(&id).ok_or_else(|| format!("session {id} 不存在"))?;
    session.spawn_reader(channel).map_err(|e| e.to_string())
}

/// 键盘输入写入 PTY。
#[tauri::command]
pub async fn session_write(
    state: State<'_, AppState>,
    id: SessionId,
    data: String,
) -> Result<(), CmdError> {
    let s = state.sessions.lock().map_err(|e| e.to_string())?;
    let session = s
        .get(&id)
        .ok_or_else(|| format!("session {id} 不存在"))?;
    session.write(&data).map_err(|e| e.to_string())
}

/// 窗口尺寸变化。
#[tauri::command]
pub async fn session_resize(
    state: State<'_, AppState>,
    id: SessionId,
    cols: u16,
    rows: u16,
) -> Result<(), CmdError> {
    let s = state.sessions.lock().map_err(|e| e.to_string())?;
    let session = s.get(&id).ok_or_else(|| format!("session {id} 不存在"))?;
    session.resize(cols, rows).map_err(|e| e.to_string())
}

/// 关闭会话（kill 子进程 + 移除）。
#[tauri::command]
pub async fn session_close(state: State<'_, AppState>, id: SessionId) -> Result<(), CmdError> {
    let mut s = state.sessions.lock().map_err(|e| e.to_string())?;
    if let Some(session) = s.remove(&id) {
        session.kill();
    }
    Ok(())
}

/// 会话列表。
#[tauri::command]
pub async fn session_list(state: State<'_, AppState>) -> Result<Vec<SessionInfo>, CmdError> {
    let s = state.sessions.lock().map_err(|e| e.to_string())?;
    Ok(s.values().map(session_info).collect())
}

/// 写入 SSH 密码到 vault（★ 只写不读；前端永远拿不到明文）。
#[tauri::command]
pub async fn session_provide_password(
    _state: State<'_, AppState>,
    alias: String,
    secret: String,
) -> Result<(), CmdError> {
    crate::vault::set(&format!("ssh-pw:{alias}"), &secret).map_err(|e| e.to_string())
}
