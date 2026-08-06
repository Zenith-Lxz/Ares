//! PTY 会话（Tauri 迁移版，Spike 双线程实现）。
//!
//! 三条硬要求（方案 §6.1）：
//! 1. 读线程必须 `std::thread::spawn`（阻塞 IO 会饿死 Tauri IPC 线程池）
//! 2. 载荷传 base64 字节（不转 String —— PTY 输出任意字节处截断多字节 UTF-8）
//! 3. Rust 侧不做任何 vt100 解析 / 换行转换（xterm.js 全包）

use anyhow::Result;
use base64::Engine;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::time::Duration;
use tauri::ipc::Channel;

/// 会话唯一标识。
pub type SessionId = u32;

const FLUSH_INTERVAL: Duration = Duration::from_millis(16); // 对齐 60fps
const FLUSH_THRESHOLD: usize = 64 * 1024;

/// 推给前端的载荷（base64 原始字节）。
#[derive(Clone, serde::Serialize)]
pub struct PtyChunk {
    pub id: SessionId,
    /// base64 编码的原始 PTY 字节
    pub data: String,
}

#[derive(Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    Local,
    Ssh,
}

pub struct Session {
    id: SessionId,
    kind: SessionKind,
    /// 主机别名（SSH）或 None（本地）
    host_alias: Option<String>,
    pair: portable_pty::PtyPair,
    writer: std::sync::Mutex<Box<dyn Write + Send>>,
    child: std::sync::Mutex<Option<Box<dyn portable_pty::Child + Send + Sync>>>,
}

impl Session {
    /// 起本地 shell 会话。
    pub fn spawn_local(cols: u16, rows: u16, id: SessionId) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let cmd = CommandBuilder::new(if cfg!(target_os = "macos") {
            "zsh"
        } else {
            "bash"
        });
        let child = pair.slave.spawn_command(cmd)?;
        let writer = pair.master.take_writer()?;
        eprintln!("[pty#{id}] local session created");
        Ok(Session {
            id,
            kind: SessionKind::Local,
            host_alias: None,
            pair,
            writer: std::sync::Mutex::new(writer),
            child: std::sync::Mutex::new(Some(child)),
        })
    }

    /// 起 ssh 会话。`target` 形如 `root@10.8.8.34`，alias 为主机配置键。
    /// 密码走 SSH_ASKPASS → 主仓库 `ares vault-get`（本地加密 vault，零弹窗）。
    pub fn spawn_ssh(
        target: &str,
        alias: &str,
        port: u16,
        cols: u16,
        rows: u16,
        id: SessionId,
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new("ssh");
        // 连接加固参数（与 gui/session.rs 一致：-t 缺失会非交互；accept-new 免 host key 卡点）
        cmd.arg("-t");
        if port != 22 {
            cmd.arg("-p");
            cmd.arg(port.to_string());
        }
        cmd.arg("-o");
        cmd.arg("StrictHostKeyChecking=accept-new");
        cmd.arg("-o");
        cmd.arg("ConnectTimeout=10");
        cmd.arg("-o");
        cmd.arg("ServerAliveInterval=15");
        cmd.arg("-o");
        cmd.arg("NumberOfPasswordPrompts=1");
        cmd.arg(target);

        // SSH_ASKPASS：临时脚本调主仓库 ares vault-get（免 keychain 弹窗）。
        // 路径取当前可执行文件（vault-get 子命令），不硬编码任何绝对路径。
        let script = write_askpass_script(alias)?;
        cmd.env("SSH_ASKPASS", &script);
        cmd.env("SSH_ASKPASS_REQUIRE", "force");
        cmd.env("DISPLAY", ":0");

        let child = pair.slave.spawn_command(cmd)?;
        let writer = pair.master.take_writer()?;
        eprintln!("[pty#{id}] ssh session created ({target}, port {port})");
        Ok(Session {
            id,
            kind: SessionKind::Ssh,
            host_alias: Some(alias.to_string()),
            pair,
            writer: std::sync::Mutex::new(writer),
            child: std::sync::Mutex::new(Some(child)),
        })
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn kind(&self) -> &SessionKind {
        &self.kind
    }

    pub fn host_alias(&self) -> Option<&str> {
        self.host_alias.as_deref()
    }

    /// 双线程读 PTY：读线程阻塞读 → 共享缓冲；flush 线程每 16ms 取走批量 base64 → Channel。
    ///
    /// 为什么不能单线程「读后检查 flush」：最后一批数据如果在 16ms 窗口内到达，
    /// 之后 read 阻塞等新数据，flush 检查永远不会再执行（ls 输出卡死根因，方案坑 #14）。
    pub fn spawn_reader(&self, channel: Channel<PtyChunk>) -> Result<()> {
        let id = self.id;
        let mut reader = self.pair.master.try_clone_reader()?;
        let shared: std::sync::Arc<std::sync::Mutex<Vec<u8>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::with_capacity(FLUSH_THRESHOLD)));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        // 读线程：阻塞读 → 共享缓冲（不在此处做任何 flush）
        let s2 = shared.clone();
        let st2 = stop.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break, // EOF
                    Ok(n) => s2.lock().unwrap().extend_from_slice(&chunk[..n]),
                    Err(_) => break,
                }
            }
            st2.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        // flush 线程：16ms 轮询 → base64 → Channel → 停止后收尾退出
        let s3 = shared.clone();
        let st3 = stop.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(FLUSH_INTERVAL);
            let data: Vec<u8> = {
                let mut b = s3.lock().unwrap();
                if b.is_empty() {
                    if st3.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                    continue;
                }
                std::mem::take(&mut *b)
            };
            let payload = base64::engine::general_purpose::STANDARD.encode(&data);
            let _ = channel.send(PtyChunk { id, data: payload });
        });
        Ok(())
    }

    pub fn write(&self, data: &str) -> Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(data.as_bytes())?;
        w.flush()?;
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.pair
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    pub fn kill(&self) {
        if let Some(c) = self.child.lock().unwrap().as_mut() {
            let _ = c.kill();
        }
    }
}

/// 写 SSH_ASKPASS 脚本：调当前可执行文件的 `vault-get "ssh-pw:<alias>"`。
/// 脚本内容不含密码明文；700 权限。
fn write_askpass_script(alias: &str) -> Result<std::path::PathBuf> {
    let dir = ares_core::paths::data_dir().join("askpass");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("askpass-{alias}.sh"));
    let self_exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("ares"));
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nexec \"{}\" vault-get \"ssh-pw:{}\" 2>/dev/null\n",
            self_exe.display(),
            alias
        ),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(path)
}
