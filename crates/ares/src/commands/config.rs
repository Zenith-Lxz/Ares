//! 配置 commands（方案 §5.5，Phase 1 mock）。

use tauri::State;

use super::AppState;
use super::CmdError;

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct AppConfig {
    pub font_size: f32,
    pub line_height: f32,
    pub theme: String,
    pub scrollback: u32,
    pub command_guard: bool,
    pub glass_blur: f32,
    pub glass_opacity: f32,
}

#[derive(serde::Serialize)]
pub struct ThemeInfo {
    pub name: String,
    pub dark: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            line_height: 1.4,
            theme: "Doric".into(),
            scrollback: 5000,
            command_guard: true,
            glass_blur: 24.0,
            glass_opacity: 0.72,
        }
    }
}

/// 读取配置。Phase 1 mock：默认值。
#[tauri::command]
pub async fn config_get(_state: State<'_, AppState>) -> Result<AppConfig, CmdError> {
    Ok(AppConfig::default())
}

/// 写入配置。Phase 1 mock：接受即成功。
#[tauri::command]
pub async fn config_set(_state: State<'_, AppState>, _config: AppConfig) -> Result<(), CmdError> {
    Ok(())
}

/// 主题列表。Phase 1 mock：内置两条。
#[tauri::command]
pub async fn theme_list(_state: State<'_, AppState>) -> Result<Vec<ThemeInfo>, CmdError> {
    Ok(vec![
        ThemeInfo {
            name: "Doric".into(),
            dark: true,
        },
        ThemeInfo {
            name: "Light".into(),
            dark: false,
        },
    ])
}
