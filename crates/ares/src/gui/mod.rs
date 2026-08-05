//! 简易 iTerm2 GUI（eframe/egui + vt100 + portable-pty）。

pub mod app;
pub mod approver;
pub mod exec;
pub mod russh_exec;
pub mod session;
pub mod sftp;
pub mod term;

pub use app::GuiApp;
