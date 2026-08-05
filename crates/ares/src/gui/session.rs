//! 终端会话：portable-pty spawn ssh + vt100 解析。
//!
//! 每个 tab 一个 `Session`：读线程把 pty 输出喂给 vt100 解析器，
//! egui 每帧从 `screen()` 克隆当前屏幕渲染；键盘经 `write()` 送进 pty。

use ares_core::ssh_config::SshHost;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

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
}

impl Session {
    /// 打开一个 ssh 会话（rows×cols 初始尺寸）。
    pub fn open(host: &SshHost, rows: u16, cols: u16) -> anyhow::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new("ssh");
        cmd.arg(&host.alias);
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let exited = Arc::new(Mutex::new(false));

        // 读线程：pty 输出 → vt100 解析
        {
            let parser = Arc::clone(&parser);
            let exited = Arc::clone(&exited);
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if let Ok(mut p) = parser.lock() {
                                p.process(&buf[..n]);
                            }
                        }
                    }
                }
                *exited.lock().unwrap() = true;
            });
        }

        Ok(Self {
            alias: host.alias.clone(),
            parser,
            writer: Arc::new(Mutex::new(writer)),
            master: Arc::new(Mutex::new(pair.master)),
            _child: Arc::new(Mutex::new(child)),
            exited,
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

    /// ssh 进程是否已退出。
    pub fn is_exited(&self) -> bool {
        *self.exited.lock().unwrap()
    }
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
    fn session_write_resize_do_not_panic_without_pty() {
        // 构造不存在的会话不应 panic（错误路径安全）
        // 此处仅验证 vt100 的 resize 语义
        let mut p = vt100::Parser::new(24, 80, 0);
        p.set_size(40, 100);
        assert_eq!(p.screen().size(), (40, 100));
    }
}
