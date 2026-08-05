//! 命令行接口。

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ares", version, about = "Autonomous Remote Engineering System")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// 初始化配置目录，写入默认的 SOUL.md 与 USER.md
    Init,

    /// 进入本机纯对话模式（M1.5 起；默认入口是 TUI 主机列表）
    Chat,

    /// 审计相关操作
    Audit {
        #[command(subcommand)]
        action: AuditAction,
    },

    /// 供应商凭据管理
    Provider {
        #[command(subcommand)]
        action: ProviderAction,
    },
}

#[derive(Subcommand)]
pub enum AuditAction {
    /// 校验审计链完整性
    Verify,
}

#[derive(Subcommand)]
pub enum ProviderAction {
    /// 把某个供应商的 API key 写入 Keychain
    Add {
        /// providers.toml 中的供应商名
        name: String,
    },
}
