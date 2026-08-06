//! russh 原生 SSH 直连执行器（2026-08-05 批次5：多机直连）。
//!
//! 用户要求：agent 直接控制多机，**不依赖本机 ssh 二进制**（当前机器
//! 没装 ssh 也能操作）。russh 纯 Rust 实现：主机簿参数 → TCP 连接 →
//! 私钥认证 → channel exec 执行命令 → 收集输出。
//!
//! 与 TerminalSessionExecutor（终端注入，当前 pane）的区别：
//! 本执行器是**独立直连**，用于 scope 内非当前 pane 的主机。

use ares_core::config::HostsConfig;
use ares_core::{AresError, HostId, Result};
use ares_exec::{ExecOutcome, ExecRequest, Executor};
use async_trait::async_trait;
use russh::client;
use russh::keys::PrivateKeyWithHashAlg;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

/// 认证私钥候选（与 SFTP 一致）。
const KEY_NAMES: [&str; 3] = ["id_ed25519", "id_rsa", "id_ecdsa"];

pub struct RusshExecutor {
    hosts: Arc<HostsConfig>,
}

impl RusshExecutor {
    pub fn new(hosts: Arc<HostsConfig>) -> Self {
        Self { hosts }
    }
}

/// russh handler：接受任何服务器密钥（信任由用户添加主机时负责）。
struct ExecHandler;

impl client::Handler for ExecHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> std::result::Result<bool, Self::Error> {
        Ok(true)
    }
}

#[async_trait]
impl Executor for RusshExecutor {
    async fn execute(&self, req: ExecRequest) -> Result<ExecOutcome> {
        let started = Instant::now();
        let alias = req.host.as_str();

        // 主机簿 → 连接参数
        let entry = self
            .hosts
            .hosts
            .get(alias)
            .cloned()
            .ok_or_else(|| AresError::OutOfScope(format!("{alias} 不在主机簿中")))?;
        let hostname = if entry.hostname.is_empty() {
            alias.to_string()
        } else {
            entry.hostname.clone()
        };
        let user = if entry.user.is_empty() {
            std::env::var("USER").unwrap_or_else(|_| "root".into())
        } else {
            entry.user.clone()
        };
        let port = entry.port.unwrap_or(22);
        let auth = if entry.auth.is_empty() {
            "key"
        } else {
            entry.auth.as_str()
        }
        .to_string();

        let timeout = req.timeout;
        let (stdout, timed_out) = tokio::time::timeout(timeout, async {
            exec_remote(alias, &hostname, port, &user, &req.command, &auth).await
        })
        .await
        .map_err(|_| AresError::Exec(format!("直连 {alias} 执行超时（{}s）", timeout.as_secs())))?
        .map_err(|e| AresError::Exec(format!("直连 {alias} 失败：{e}")))?;

        Ok(ExecOutcome {
            host: req.host,
            exit_code: if timed_out { -1 } else { 0 },
            stdout,
            stderr: String::new(),
            duration_ms: started.elapsed().as_millis() as u64,
            timed_out,
        })
    }

    /// 支持任意主机（由路由层决定何时用它）。
    fn supports(&self, _host: &HostId) -> bool {
        true
    }
}

/// russh 直连执行一条命令，返回 (stdout, timed_out)。
async fn exec_remote(
    alias: &str,
    hostname: &str,
    port: u16,
    user: &str,
    command: &str,
    auth: &str,
) -> std::result::Result<(String, bool), String> {
    let config = Arc::new(client::Config::default());
    let addr = format!("{hostname}:{port}");
    let mut session = client::connect(config, addr, ExecHandler)
        .await
        .map_err(|e| format!("连接失败：{e}"))?;

    // 认证：密码主机走本地加密 vault（ssh-pw:<alias>），否则默认私钥依次尝试
    let mut authed = false;
    if auth == "password" {
        match crate::vault::get(&format!("ssh-pw:{alias}")) {
            Some(pw) => {
                if let Ok(auth_res) = session.authenticate_password(user, pw).await {
                    authed = auth_res.success();
                }
            }
            _ => {
                return Err(format!(
                    "认证失败：{alias} 配置了密码认证但 vault 中没有密码（ssh-pw:{alias}）"
                ));
            }
        }
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        for key_name in KEY_NAMES {
            let path = PathBuf::from(&home).join(".ssh").join(key_name);
            if !path.exists() {
                continue;
            }
            let Ok(key) = russh::keys::load_secret_key(&path, None) else {
                continue;
            };
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), None);
            if let Ok(auth_res) = session.authenticate_publickey(user, key).await {
                if auth_res.success() {
                    authed = true;
                    break;
                }
            }
        }
    }
    if !authed {
        return Err("认证失败：未找到可用的私钥".into());
    }

    // 执行命令
    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|e| format!("打开通道失败：{e}"))?;
    channel
        .exec(true, command)
        .await
        .map_err(|e| format!("执行失败：{e}"))?;

    // 读取输出直到 EOF（Channel 无 AsyncRead impl，用 wait() 收消息）
    let mut output = Vec::new();
    let mut stderr = Vec::new();
    loop {
        match channel.wait().await {
            Some(russh::ChannelMsg::Data { data }) => output.extend_from_slice(&data),
            Some(russh::ChannelMsg::ExtendedData { data, .. }) => stderr.extend_from_slice(&data),
            Some(russh::ChannelMsg::Eof) | None => break,
            _ => {}
        }
    }
    let _ = channel.close().await;

    let mut out = String::from_utf8_lossy(&output).to_string();
    if !stderr.is_empty() {
        out.push_str(&format!("\n[stderr] {}", String::from_utf8_lossy(&stderr)));
    }
    Ok((out, false))
}
