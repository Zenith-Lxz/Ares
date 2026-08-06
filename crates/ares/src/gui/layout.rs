//! 会话布局持久化（M7）：tab/分屏布局保存与恢复。
//!
//! `~/.local/share/ares/layout.json`（仅恢复终端 tab；SFTP/主机页不恢复）。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutFile {
    pub tabs: Vec<LayoutTab>,
    pub active: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutTab {
    /// 主机别名（hosts.toml 键）。
    pub host: String,
    /// 分屏方向：None=单 pane；"v"=垂直（左右）；"h"=水平（上下）。
    pub split: Option<String>,
}

impl LayoutFile {
    pub fn path() -> std::path::PathBuf {
        ares_core::paths::data_dir().join("layout.json")
    }

    pub fn load() -> Option<Self> {
        let text = std::fs::read_to_string(Self::path()).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save(&self) {
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(Self::path(), text);
        }
    }
}
