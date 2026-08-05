//! 简易 iTerm2 GUI（eframe/egui + vt100 + portable-pty）。

pub mod app;
pub mod session;
pub mod term;

pub use app::GuiApp;
