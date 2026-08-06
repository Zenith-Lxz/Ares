//! Agent commands（方案 §5.4，Phase 1 mock）。

use tauri::ipc::Channel;
use tauri::State;

use super::AppState;
use super::CmdError;

#[derive(Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Token {
        text: String,
    },
    ToolStart {
        tool: String,
        summary: String,
    },
    ToolResult {
        tool: String,
        display: String,
        success: bool,
    },
    ApprovalRequired {
        approval_id: u32,
        host: String,
        env: String,
        command: String,
        decision: String,
        host_count: usize,
        reason: String,
    },
    TurnEnd {
        input_tokens: u32,
        output_tokens: u32,
    },
    Error {
        message: String,
    },
}

/// 订阅 Agent 事件流。Phase 1 mock：接收 Channel 不推事件。
#[tauri::command]
pub async fn agent_subscribe(
    _state: State<'_, AppState>,
    _channel: Channel<AgentEvent>,
) -> Result<(), CmdError> {
    Ok(())
}

/// 发送消息给 Agent。Phase 1 mock。
#[tauri::command]
pub async fn agent_send(_state: State<'_, AppState>, _message: String) -> Result<(), CmdError> {
    Ok(())
}

/// 中断当前 Agent 回合。Phase 1 mock。
#[tauri::command]
pub async fn agent_interrupt(_state: State<'_, AppState>) -> Result<(), CmdError> {
    Ok(())
}

/// 审批回应。Phase 1 mock。
#[tauri::command]
pub async fn agent_approve(
    _state: State<'_, AppState>,
    _approval_id: u32,
    _approved: bool,
) -> Result<(), CmdError> {
    Ok(())
}

/// Agent 操作范围（多选主机）。Phase 1 mock。
#[tauri::command]
pub async fn agent_set_scope(
    _state: State<'_, AppState>,
    _aliases: Vec<String>,
) -> Result<(), CmdError> {
    Ok(())
}
