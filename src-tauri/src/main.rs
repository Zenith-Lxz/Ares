//! ARES Tauri 壳（迁移 Phase 1）：注册全部 mock commands + AppState。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use ares::commands::AppState;

fn main() {
    // SSH_ASKPASS 子命令：与主仓库 `ares vault-get` 同构。
    // askpass 脚本用 current_exe() 生成（pty/session.rs），运行时必须是本二进制，
    // 否则认证阶段拿不到密码（表现：握手成功后被服务器 Connection closed）。
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("vault-get") {
        if let Some(alias) = args.get(2) {
            if let Some(secret) = ares::vault::get(alias) {
                print!("{secret}");
                return;
            }
        }
        std::process::exit(1);
    }
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            ares::commands::session::session_create,
            ares::commands::session::session_subscribe,
            ares::commands::session::session_write,
            ares::commands::session::session_resize,
            ares::commands::session::session_close,
            ares::commands::session::session_provide_password,
            ares::commands::session::session_list,
            ares::commands::guard::command_check,
            ares::commands::guard::command_authorize,
            ares::commands::host::host_list,
            ares::commands::host::host_get,
            ares::commands::host::host_probe,
            ares::commands::agent::agent_subscribe,
            ares::commands::agent::agent_send,
            ares::commands::agent::agent_interrupt,
            ares::commands::agent::agent_approve,
            ares::commands::agent::agent_set_scope,
            ares::commands::audit::audit_query,
            ares::commands::audit::audit_verify,
            ares::commands::config::config_get,
            ares::commands::config::config_set,
            ares::commands::config::theme_list,
            ares::commands::vault::vault_has,
            ares::commands::vault::vault_set,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ARES tauri application");
}
