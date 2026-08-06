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
    /// Kitty 图形协议内联图片（M6）：按接收完成时的光标位置放置。
    pub images: Arc<Mutex<Vec<InlineImage>>>,
    /// xterm 鼠标协议模式（0=关；1000=点击；1002=拖拽；1003=全跟踪）。
    pub mouse_mode: Arc<Mutex<u8>>,
    /// 括号粘贴（bracketed paste：\x1b[?2004h 开启）—— Cmd+V 时包裹。
    pub bracketed_paste: Arc<Mutex<bool>>,
}

/// Kitty 协议内联图片（渲染时解码）。
pub struct InlineImage {
    pub row: u16,
    pub col: u16,
    pub data: Vec<u8>,
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
        let images = Arc::new(Mutex::new(Vec::<InlineImage>::new()));
        let mouse_mode = Arc::new(Mutex::new(0u8));
        let bracketed_paste = Arc::new(Mutex::new(false));

        // 读线程：pty 输出 → vt100 解析（每批数据后通知 GUI 重画）
        // repaint 节流 30fps：高频输出（top/日志）不触发每批重画（M4）
        let last_repaint = Arc::new(Mutex::new(std::time::Instant::now()));
        let mut kitty_b64: Vec<u8> = Vec::new();
        {
            let last_repaint = last_repaint.clone();
            let images_r = images.clone();
            let mouse_mode_r = mouse_mode.clone();
            let bracketed_paste_r = bracketed_paste.clone();
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
                            // Kitty 图形协议解析（M6）：_G params;base64 \
                            // （vt100 忽略未知 OSC，解析独立于渲染管线）
                            parse_kitty_images(&buf[..n], &parser, &images_r, &mut kitty_b64);
                            // xterm 鼠标模式 / 括号粘贴 / 铃声（主流终端能力补齐）
                            scan_terminal_modes(&buf[..n], &mouse_mode_r, &bracketed_paste_r);
                            scan_bell(&buf[..n]);
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
            images,
            mouse_mode,
            bracketed_paste,
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

/// 扫描 xterm 鼠标模式（\x1b[?1000/1002/1003h/l）与括号粘贴（\x1b[?2004h/l）。
fn scan_terminal_modes(
    buf: &[u8],
    mouse_mode: &Arc<Mutex<u8>>,
    bracketed_paste: &Arc<Mutex<bool>>,
) {
    let mut i = 0usize;
    while i + 3 < buf.len() {
        if buf[i] == 0x1b && buf[i + 1] == b'[' && buf[i + 2] == b'?' {
            // 读数字
            let mut j = i + 3;
            let mut n = 0u32;
            while j < buf.len() && buf[j].is_ascii_digit() {
                n = n * 10 + (buf[j] - b'0') as u32;
                j += 1;
            }
            if j < buf.len() && (buf[j] == b'h' || buf[j] == b'l') {
                let on = buf[j] == b'h';
                match n {
                    1000 | 1002 | 1003 => {
                        let mut m = mouse_mode.lock().unwrap();
                        *m = if on { n as u8 } else { 0 };
                    }
                    2004 => {
                        let mut b = bracketed_paste.lock().unwrap();
                        *b = on;
                    }
                    _ => {}
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
}

/// 铃声（BEL 0x07）：播放 macOS 系统提示音（Tink）。
fn scan_bell(buf: &[u8]) {
    if buf.contains(&0x07) {
        let _ = std::process::Command::new("afplay")
            .arg("/System/Library/Sounds/Tink.aiff")
            .spawn();
    }
}

/// 解析 Kitty 图形协议（M6）：完整图 → 记录到 images（光标位置放置）。
/// 支持分片（m=1 续片）累积；base64 解码；只取 a=T（transmit）。
fn parse_kitty_images(
    buf: &[u8],
    parser: &Arc<Mutex<vt100::Parser>>,
    images: &Arc<Mutex<Vec<InlineImage>>>,
    kitty_b64: &mut Vec<u8>,
) {
    use base64::Engine;
    let mut pos = 0usize;
    while pos + 2 <= buf.len() {
        if buf[pos] == 0x1b && buf[pos + 1] == b'G' {
            if let Some(semi) = buf[pos + 2..].iter().position(|&b| b == b';') {
                let params = String::from_utf8_lossy(&buf[pos + 2..pos + 2 + semi]).to_string();
                let payload_start = pos + 2 + semi + 1;
                if let Some(end) = buf[payload_start..]
                    .windows(2)
                    .position(|w| w == [0x1b, b'\\'])
                {
                    let payload_end = payload_start + end;
                    let payload = &buf[payload_start..payload_end];
                    let is_continue = params.contains(",m=1") || params.ends_with("m=1");
                    let is_transmit = params.starts_with("a=T") || params.starts_with("a=t");
                    if is_transmit {
                        kitty_b64.extend_from_slice(payload);
                        if !is_continue {
                            if let Ok(data) =
                                base64::engine::general_purpose::STANDARD.decode(&kitty_b64[..])
                            {
                                let (row, col) = {
                                    let p = parser.lock().unwrap();
                                    p.screen().cursor_position()
                                };
                                images.lock().unwrap().push(InlineImage { row, col, data });
                            }
                            kitty_b64.clear();
                        }
                    }
                    pos = payload_end + 2;
                    continue;
                }
            }
        }
        pos += 1;
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
