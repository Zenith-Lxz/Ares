//! GUI 设置持久化（2026-08-05 批次8b：iTerm2 化设置）。
//!
//! `~/.config/ares/settings.toml`：
//! ```toml
//! theme_name = "Snazzy"
//! font_size = 14.0
//! hide_tabs = false
//! undecorated = false          # 隐藏红绿灯（重启生效）
//! background_image = ""        # 终端背景图路径
//! ```

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiSettings {
    pub theme_name: String,
    pub font_size: f32,
    /// 隐藏顶部 Tab 栏（iTerm2 极简模式；Ctrl-T/W 仍可用）
    pub hide_tabs: bool,
    /// 隐藏窗口红绿灯（无边框；需重启生效）
    pub undecorated: bool,
    /// 终端背景图路径（空 = 无）
    pub background_image: String,
}

impl Default for GuiSettings {
    fn default() -> Self {
        Self {
            theme_name: "Default".into(),
            font_size: 14.0,
            hide_tabs: false,
            undecorated: false,
            background_image: String::new(),
        }
    }
}

impl GuiSettings {
    pub fn path() -> std::path::PathBuf {
        ares_core::paths::config_dir().join("settings.toml")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(text) = toml::to_string(self) {
            let _ = std::fs::write(Self::path(), text);
        }
    }
}
