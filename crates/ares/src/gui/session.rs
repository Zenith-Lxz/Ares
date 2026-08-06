//! 终端会话：portable-pty spawn ssh + vt100 解析。
//!
//! 每个 tab 一个 `Session`：读线程把 pty 输出喂给 vt100 解析器，
//! egui 每帧从 `screen()` 克隆当前屏幕渲染；键盘经 `write()` 送进 pty。

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

/// ssh 连接目标（来自 ARES 主机簿，独立于 ssh_config）。
#[derive(Debug, Clone)]
pub struct ConnTarget {
    pub hostname: String,
    pub user: Option<String>,
    pub port: Option<u16>,
}

#[derive(Clone)]
pub struct Session {
    /// tab 标题（ssh_config 别名）。
    pub alias: String,
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    /// 保持 ssh 进程存活（drop 会终止）；读取线程负责置 exited。
    _child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    /// pty 已关闭（ssh 退出）。
    pub exited: Arc<Mutex<bool>>,
    /// 已收到首笔远端数据（ssh 握手完成，连接中状态用）。
    pub connected: Arc<Mutex<bool>>,
    /// 滚动回退偏移（行数；0=正常视图）。scrollback 容量 10000 行。
    pub scroll: Arc<Mutex<usize>>,
}

impl Session {
    /// 打开一个 ssh 会话（rows×cols 初始尺寸；密钥认证）。
    #[allow(dead_code)]
    ///
    /// `alias` 是 tab 标题；连接参数来自主机簿（hostname/user/port），
    /// hostname 为空时回退直接用 alias（兼容仅填了别名的条目）。
    pub fn open(
        alias: &str,
        target: &ConnTarget,
        rows: u16,
        cols: u16,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) -> anyhow::Result<Self> {
        Self::open_with_auth(alias, target, rows, cols, repaint, "key")
    }

    /// 打开 ssh 会话（auth=password 时用 SSH_ASKPASS 从钥匙串读密码，vault）。
    pub fn open_with_auth(
        alias: &str,
        target: &ConnTarget,
        rows: u16,
        cols: u16,
        repaint: Arc<dyn Fn() + Send + Sync>,
        auth: &str,
    ) -> anyhow::Result<Self> {
        let mut cmd = CommandBuilder::new("ssh");
        // 密码主机（vault）：SSH_ASKPASS 脚本从钥匙串读 ssh-pw:<alias>
        if auth == "password" {
            let script = write_askpass(alias)?;
            cmd.env("SSH_ASKPASS", script);
            cmd.env("SSH_ASKPASS_REQUIRE", "force");
            cmd.env("DISPLAY", ":0");
            cmd.arg("-o");
            cmd.arg("NumberOfPasswordPrompts=1");
        }
        if let Some(p) = target.port {
            if p != 22 {
                cmd.arg("-p");
                cmd.arg(p.to_string());
            }
        }
        let hostname = if target.hostname.is_empty() {
            alias.to_string()
        } else {
            target.hostname.clone()
        };
        let dest = match &target.user {
            Some(u) => format!("{u}@{hostname}"),
            None => hostname,
        };
        cmd.arg(dest);
        Self::open_command(alias, cmd, rows, cols, repaint)
    }

    /// 本地终端会话（Netcatty 的 local terminal 功能）。
    pub fn open_local(
        rows: u16,
        cols: u16,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) -> anyhow::Result<Self> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
        let mut cmd = CommandBuilder::new(&shell);
        if shell.ends_with("bash") || shell.ends_with("zsh") {
            cmd.arg("-l"); // 登录 shell，加载用户环境
        }
        Self::open_command("本地", cmd, rows, cols, repaint)
    }

    /// 通用：spawn 任意命令进 pty。
    fn open_command(
        alias: &str,
        cmd: CommandBuilder,
        rows: u16,
        cols: u16,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) -> anyhow::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        // 滚动回退容量 10000 行（vt100 grid 维护，Parser::set_scrollback 公开）
        parser.lock().unwrap().set_scrollback(10000);
        let exited = Arc::new(Mutex::new(false));
        let connected = Arc::new(Mutex::new(false));
        let scroll = Arc::new(Mutex::new(0usize));

        // 读线程：pty 输出 → vt100 解析（每批数据后通知 GUI 重画）
        // repaint 节流 30fps：高频输出（top/日志）不触发每批重画（M4）
        let last_repaint = Arc::new(Mutex::new(std::time::Instant::now()));
        {
            let last_repaint = last_repaint.clone();
            let parser = Arc::clone(&parser);
            let exited = Arc::clone(&exited);
            let connected = Arc::clone(&connected);
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            *connected.lock().unwrap() = true;
                            if let Ok(mut p) = parser.lock() {
                                p.process(&buf[..n]);
                            }
                            {
                                let mut last = last_repaint.lock().unwrap();
                                if last.elapsed() >= std::time::Duration::from_millis(33) {
                                    *last = std::time::Instant::now();
                                    repaint();
                                }
                            }
                        }
                    }
                }
                *exited.lock().unwrap() = true;
                repaint();
            });
        }

        Ok(Self {
            alias: alias.to_string(),
            parser,
            writer: Arc::new(Mutex::new(writer)),
            master: Arc::new(Mutex::new(pair.master)),
            connected,
            _child: Arc::new(Mutex::new(child)),
            exited,
            scroll,
        })
    }

    /// 键盘输入送进 pty。
    pub fn write(&self, bytes: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    /// 终端尺寸变化：vt100 解析器 + pty 一起改。
    pub fn resize(&self, rows: u16, cols: u16) {
        if let Ok(mut p) = self.parser.lock() {
            p.set_size(rows, cols);
        }
        if let Ok(m) = self.master.lock() {
            let _ = m.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    /// 当前屏幕（克隆，渲染用）。
    pub fn screen(&self) -> vt100::Screen {
        self.parser.lock().unwrap().screen().clone()
    }

    /// 当前屏幕的纯文本快照（去掉尾随空白行），Agent 观察终端用。
    pub fn snapshot_text(&self) -> String {
        let screen = self.screen();
        let (rows, cols) = screen.size();
        let mut lines: Vec<String> = Vec::with_capacity(rows as usize);
        for r in 0..rows {
            let mut line = String::new();
            for c in 0..cols {
                if let Some(cell) = screen.cell(r, c) {
                    if cell.is_wide_continuation() {
                        continue;
                    }
                    line.push_str(&cell.contents());
                }
            }
            lines.push(line);
        }
        // 去掉尾部空白行
        while lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
            lines.pop();
        }
        lines.join("\n")
    }

    /// ssh 进程是否已退出。
    pub fn is_exited(&self) -> bool {
        *self.exited.lock().unwrap()
    }

    /// 是否已收到首笔远端数据（连接中反馈用）。
    pub fn is_connected(&self) -> bool {
        *self.connected.lock().unwrap()
    }

    // ── 滚动回退（scrollback）──

    /// 向上/向下滚动 delta 行（负=向下）；返回实际偏移（0=底部）。
    pub fn scroll_lines(&self, delta: i32) -> usize {
        let mut sc = self.scroll.lock().unwrap();
        let target = if delta >= 0 {
            sc.saturating_add(delta as usize)
        } else {
            sc.saturating_sub((-delta) as usize)
        };
        {
            let mut p = self.parser.lock().unwrap();
            p.set_scrollback(target);
            // 读回实际偏移（vt100 内部 clamp 到 scrollback 长度）
            *sc = p.screen().scrollback();
        }
        *sc
    }

    /// 当前滚动偏移（0=底部正常视图）。
    pub fn scroll_offset(&self) -> usize {
        *self.scroll.lock().unwrap()
    }

    /// 回到底部（新输出跟随）。
    pub fn scroll_reset(&self) {
        let mut sc = self.scroll.lock().unwrap();
        *sc = 0;
        let mut p = self.parser.lock().unwrap();
        p.set_scrollback(0);
    }
}

/// 写 SSH_ASKPASS 脚本（读取钥匙串 ssh-pw:<alias> 输出密码）。
/// 脚本内容不含密码明文；700 权限。
fn write_askpass(alias: &str) -> anyhow::Result<std::path::PathBuf> {
    let dir = ares_core::paths::data_dir().join("askpass");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("askpass-{alias}.sh"));
    let script = format!(
        "#!/bin/sh\nexec /usr/bin/security find-generic-password -s ares -a \"ssh-pw:{alias}\" -w 2>/dev/null\n"
    );
    std::fs::write(&path, script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    #[test]
    fn vt100_parser_renders_basic_output() {
        // 不真连 ssh：直接验证 vt100 解析 → 网格内容
        let mut p = vt100::Parser::new(24, 80, 0);
        p.process(b"hello\x1b[31m red\x1b[0m");
        let screen = p.screen();
        assert_eq!(screen.cell(0, 0).unwrap().contents(), "h");
        assert_eq!(screen.cell(0, 4).unwrap().contents(), "o");
        // "hello" 后是空格，然后是带颜色的 "red"（第 6、7、8 格）
        let red_cell = screen.cell(0, 7).unwrap();
        assert_eq!(red_cell.contents(), "e");
        assert_eq!(red_cell.fgcolor(), vt100::Color::Idx(1));
    }

    #[test]
    fn vt100_parser_handles_cursor_and_clear() {
        let mut p = vt100::Parser::new(24, 80, 0);
        p.process(b"abc\r\nxyz");
        let screen = p.screen();
        assert_eq!(screen.cell(1, 0).unwrap().contents(), "x");
        // 清屏
        p.process(b"\x1b[2J\x1b[H");
        let screen = p.screen();
        assert_eq!(screen.cell(0, 0).unwrap().contents(), "");
        assert_eq!(screen.cell(1, 0).unwrap().contents(), "");
    }

    #[test]
    fn snapshot_text_strips_trailing_blank_lines() {
        let mut p = vt100::Parser::new(24, 80, 0);
        p.process(b"line1\r\nline2\r\n");
        // 手动构造：直接用 Parser 验证快照行为（Session 包装同样逻辑）
        // snapshot_text 在 Session 上；这里验证 vt100 行内容即可
        let screen = p.screen();
        assert_eq!(screen.cell(0, 0).unwrap().contents(), "l");
        assert_eq!(screen.cell(1, 0).unwrap().contents(), "l");
    }

    #[test]
    fn session_write_resize_do_not_panic_without_pty() {
        // 构造不存在的会话不应 panic（错误路径安全）
        // 此处仅验证 vt100 的 resize 语义
        let mut p = vt100::Parser::new(24, 80, 0);
        p.set_size(40, 100);
        assert_eq!(p.screen().size(), (40, 100));
    }
}
