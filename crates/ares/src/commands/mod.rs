//! Tauri command 薄层（迁移 Phase 1：签名与类型对齐，函数体为 mock）。
//!
//! ★ 这一层必须薄：只做参数校验和转发，业务逻辑留在 ares-* crate。
//! Phase 2 起逐步接通真实实现。
//!
//! 本模块仅在 `tauri` feature 下编译（主仓库 egui 版不链接 tauri）。

pub mod agent;
pub mod audit;
pub mod config;
pub mod guard;
pub mod host;
pub mod session;
pub mod vault;

/// command 统一错误（Tauri 序列化为字符串）。
pub type CmdError = String;

/// 全局状态：会话池 / agent 句柄 / 配置（方案任务 4）。
/// Phase 1：sessions 池真实可用（Phase 2 直接接），agent 为占位。
#[derive(Default)]
pub struct AppState {
    pub sessions: std::sync::Mutex<std::collections::HashMap<u32, crate::pty::Session>>,
    pub next_id: std::sync::Mutex<u32>,
    /// Agent 句柄占位（Phase 4 接 ares-agent）
    pub agent: std::sync::Mutex<Option<serde_json::Value>>,
    pub config: std::sync::Mutex<config::AppConfig>,
}

/// 前端调试页遍历用的 command 清单（Phase 1 验收）。
pub const COMMAND_INVENTORY: &[(&str, &str)] = &[
    ("session_create", "创建会话（mock 返回 1）"),
    ("session_subscribe", "订阅 PTY 输出流（mock 接受 Channel）"),
    ("session_write", "写入 PTY（mock）"),
    ("session_resize", "调整尺寸（mock）"),
    ("session_close", "关闭会话（mock）"),
    ("session_list", "会话列表（mock 空）"),
    ("command_check", "命令拦截判定（mock allow）"),
    ("command_authorize", "TouchID 授权（mock true）"),
    ("host_list", "主机列表（mock 两条示例）"),
    ("host_get", "主机详情（mock）"),
    ("host_probe", "连通性探测（mock）"),
    ("agent_subscribe", "Agent 事件订阅（mock Channel）"),
    ("agent_send", "Agent 消息（mock）"),
    ("agent_interrupt", "中断（mock）"),
    ("agent_approve", "审批回应（mock）"),
    ("agent_set_scope", "设置操作范围（mock）"),
    ("audit_query", "审计查询（mock 空）"),
    ("audit_verify", "审计链校验（mock ok）"),
    ("config_get", "读取配置（mock 默认）"),
    ("config_set", "写入配置（mock）"),
    ("theme_list", "主题列表（mock 内置两条）"),
    ("vault_has", "凭据存在性（mock false）"),
    ("vault_set", "写入凭据（mock）"),
];
