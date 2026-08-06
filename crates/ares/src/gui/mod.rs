//! 简易 iTerm2 GUI（eframe/egui + vt100 + portable-pty）。

pub mod app;
pub mod approver;
pub mod exec;
pub mod layout;
pub mod ligatures;
pub mod plan_approver;
pub mod russh_exec;
pub mod session;
pub mod settings;
pub mod sftp;
pub mod term;
pub mod themes;

pub use app::GuiApp;
