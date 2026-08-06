//! SFTP 文件浏览（Netcatty 的 SFTP 功能，russh 纯 Rust 实现）。
//!
//! 连接复用主机簿参数（hostname/user/port）；认证走默认私钥
//! （id_ed25519 / id_rsa / id_ecdsa）。异步操作经 channel 桥接
//! 到 GUI 主线程（与 Agent 面板同样的模式）。

use crate::gui::session::ConnTarget;
use russh::client;
use russh_sftp::client::SftpSession;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};

/// 远程目录条目：(名称, 是否目录, 大小)
pub type Entry = (String, bool, u64);

/// SFTP 面板（一个 tab）。
pub struct SftpPanel {
    pub title: String,
    pub remote_path: String,
    pub entries: Vec<Entry>,
    pub local_path: String,
    pub local_entries: Vec<Entry>,
    pub selected: Option<String>,
    pub busy: bool,
    pub error: Option<String>,
    sftp: std::sync::Arc<tokio::sync::Mutex<SftpSession>>,
    rx: Receiver<SftpResult>,
    tx: Sender<SftpResult>,
}

enum SftpResult {
    Listed(String, Vec<Entry>),
    Done(Result<String, String>),
}

impl SftpPanel {
    /// 建立连接（阻塞式，GUI 在 rt 上调用）；返回面板。
    pub fn connect(
        title: &str,
        target: &ConnTarget,
        user: &str,
        auth: &str,
        rt: &tokio::runtime::Runtime,
    ) -> Result<Self, String> {
        let sftp = rt.block_on(connect_sftp(target, user, title, auth))?;
        let (tx, rx) = std::sync::mpsc::channel();
        let mut panel = Self {
            title: title.into(),
            remote_path: "/".into(),
            entries: Vec::new(),
            local_path: std::env::var("HOME").unwrap_or_else(|_| "/".into()),
            local_entries: Vec::new(),
            selected: None,
            busy: false,
            error: None,
            sftp: std::sync::Arc::new(tokio::sync::Mutex::new(sftp)),
            rx,
            tx,
        };
        panel.list_remote(rt, "/");
        panel.list_local(rt);
        Ok(panel)
    }

    // ── 操作（GUI 按钮/双击触发，spawn 到 rt）──

    pub fn list_remote(&mut self, rt: &tokio::runtime::Runtime, path: &str) {
        self.remote_path = path.to_string();
        self.busy = true;
        let tx = self.tx.clone();
        let sftp = self.sftp.clone();
        let path = path.to_string();
        rt.spawn(async move {
            let sftp = sftp.lock().await;
            match sftp.read_dir(&path).await {
                Ok(dir) => {
                    let mut entries: Vec<Entry> = dir
                        .map(|e| {
                            let meta = e.metadata();
                            (e.file_name(), meta.is_dir(), meta.size.unwrap_or(0))
                        })
                        .collect();
                    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                    let _ = tx.send(SftpResult::Listed(path, entries));
                }
                Err(e) => {
                    let _ = tx.send(SftpResult::Done(Err(format!("读取目录失败：{e}"))));
                }
            }
        });
    }

    pub fn list_local(&mut self, rt: &tokio::runtime::Runtime) {
        self.busy = true;
        let tx = self.tx.clone();
        let path = self.local_path.clone();
        rt.spawn_blocking(move || {
            let mut entries = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&path) {
                for de in rd.flatten() {
                    let name = de.file_name().to_string_lossy().to_string();
                    let is_dir = de.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    let size = de.metadata().map(|m| m.len()).unwrap_or(0);
                    entries.push((name, is_dir, size));
                }
            }
            entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let _ = tx.send(SftpResult::Listed(path, entries));
        });
    }

    pub fn enter_remote(&mut self, rt: &tokio::runtime::Runtime, name: &str) {
        let path = format!("{}/{}", self.remote_path.trim_end_matches('/'), name);
        self.list_remote(rt, &path);
    }

    pub fn go_up(&mut self, rt: &tokio::runtime::Runtime) {
        let p = Path::new(&self.remote_path);
        let parent = p.parent().map(|p| p.to_string_lossy().to_string());
        if let Some(parent) = parent {
            if parent.is_empty() {
                self.list_remote(rt, "/");
            } else {
                self.list_remote(rt, &parent);
            }
        }
    }

    pub fn enter_local(&mut self, rt: &tokio::runtime::Runtime, name: &str) {
        let path = format!("{}/{}", self.local_path.trim_end_matches('/'), name);
        self.local_path = path;
        self.list_local(rt);
    }

    pub fn go_up_local(&mut self, rt: &tokio::runtime::Runtime) {
        let p = Path::new(&self.local_path);
        if let Some(parent) = p.parent() {
            self.local_path = parent.to_string_lossy().to_string();
            self.list_local(rt);
        }
    }

    /// 下载远程文件到本地当前目录。
    pub fn download(&mut self, rt: &tokio::runtime::Runtime, name: &str) {
        self.busy = true;
        let tx = self.tx.clone();
        let sftp = self.sftp.clone();
        let name = name.to_string();
        let remote = format!("{}/{}", self.remote_path.trim_end_matches('/'), name);
        let local = PathBuf::from(&self.local_path).join(&name);
        rt.spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let sftp = sftp.lock().await;
            let res = async {
                let mut file = sftp
                    .open(&remote)
                    .await
                    .map_err(|e| format!("打开远程文件失败：{e}"))?;
                let mut data = Vec::new();
                file.read_to_end(&mut data)
                    .await
                    .map_err(|e| format!("读取失败：{e}"))?;
                let _ = file.shutdown().await;
                std::fs::write(&local, &data).map_err(|e| format!("写本地失败：{e}"))?;
                Ok::<String, String>(format!("已下载 {name}（{} 字节）", data.len()))
            }
            .await;
            let _ = tx.send(SftpResult::Done(res));
        });
    }

    /// 上传本地文件到远程当前目录。
    pub fn upload(&mut self, rt: &tokio::runtime::Runtime, name: &str) {
        self.busy = true;
        let tx = self.tx.clone();
        let sftp = self.sftp.clone();
        let name = name.to_string();
        let local = PathBuf::from(&self.local_path).join(&name);
        let remote = format!("{}/{}", self.remote_path.trim_end_matches('/'), name);
        rt.spawn(async move {
            let sftp = sftp.lock().await;
            use tokio::io::AsyncWriteExt;
            let res = async {
                use russh_sftp::protocol::OpenFlags;
                let data = std::fs::read(&local).map_err(|e| format!("读本地失败：{e}"))?;
                let mut file = sftp
                    .open_with_flags(
                        &remote,
                        OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
                    )
                    .await
                    .map_err(|e| format!("打开远程文件失败：{e}"))?;
                file.write_all(&data)
                    .await
                    .map_err(|e| format!("写入失败：{e}"))?;
                let _ = file.shutdown().await;
                Ok::<String, String>(format!("已上传 {name}（{} 字节）", data.len()))
            }
            .await;
            let _ = tx.send(SftpResult::Done(res));
        });
    }

    /// GUI 每帧调用：处理结果队列。
    pub fn poll(&mut self) {
        while let Ok(r) = self.rx.try_recv() {
            match r {
                SftpResult::Listed(path, entries) => {
                    if path == self.remote_path {
                        self.entries = entries;
                    } else if path == self.local_path {
                        self.local_entries = entries;
                    }
                    self.busy = false;
                }
                SftpResult::Done(res) => {
                    self.busy = false;
                    self.error = Some(res.unwrap_or_else(|e| e));
                }
            }
        }
    }
}

/// 建立 russh 连接并认证（私钥优先；密码主机走 keychain vault），返回 SFTP 会话。
async fn connect_sftp(
    target: &ConnTarget,
    user: &str,
    alias: &str,
    auth: &str,
) -> Result<SftpSession, String> {
    let config = std::sync::Arc::new(client::Config::default());
    let port = target.port.unwrap_or(22);
    let addr = format!("{}:{port}", target.hostname);
    let mut session = client::connect(config, addr, SftpHandler)
        .await
        .map_err(|e| format!("连接失败：{e}"))?;

    // 认证：密码主机走本地加密 vault（ssh-pw:<alias>），否则依次尝试默认私钥
    let mut authed = false;
    if auth == "password" {
        match crate::vault::get(&format!("ssh-pw:{alias}")) {
            Some(pw) => {
                if let Ok(res) = session.authenticate_password(user, pw).await {
                    authed = res.success();
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
        for key_name in ["id_ed25519", "id_rsa", "id_ecdsa"] {
            let path = PathBuf::from(&home).join(".ssh").join(key_name);
            if !path.exists() {
                continue;
            }
            let Ok(key) = russh::keys::load_secret_key(&path, None) else {
                continue;
            };
            let key = russh::keys::PrivateKeyWithHashAlg::new(std::sync::Arc::new(key), None);
            if let Ok(auth_res) = session.authenticate_publickey(user, key).await {
                if auth_res.success() {
                    authed = true;
                    break;
                }
            }
        }
    }
    if !authed {
        return Err(
            "认证失败：未找到可用的私钥（尝试了 ~/.ssh/id_ed25519 / id_rsa / id_ecdsa）".into(),
        );
    }

    // 打开 sftp 子系统通道（russh-sftp 官方示例用法）
    let channel = session
        .channel_open_session()
        .await
        .map_err(|e| format!("打开会话通道失败：{e}"))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| format!("请求 sftp 子系统失败：{e}"))?;
    SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| format!("SFTP 会话建立失败：{e}"))
}

/// russh 客户端 handler：接受任何服务器密钥（信任由用户连接行为负责）。
struct SftpHandler;

impl client::Handler for SftpHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}
