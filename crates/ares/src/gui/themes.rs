//! 终端主题系统（2026-08-05 批次8：iTerm2 化）。
//!
//! - 内置主题：Default / Snazzy / Dracula / Solarized Dark / One Dark / Gruvbox
//! - 导入 .itermcolors（XML plist，mbadolato/iTerm2-Color-Schemes 450+ 主题）
//! - 存储：`~/.config/ares/themes/<name>.toml`（导入时转换落盘，内置不可覆盖）
//!
//! ANSI 16 色替换 vt100 的 `Color::Idx`（0-15）；256 色保持标准表；
//! Default 色替换为主题 fg/bg；TrueColor 直用。

use egui::Color32;

/// 主题：名称 + 前景/背景/光标 + ANSI 16 色调色板。
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub fg: Color32,
    pub bg: Color32,
    pub cursor: Color32,
    /// ANSI 0-15
    pub palette: [Color32; 16],
}

impl Theme {
    pub fn new(
        name: &str,
        fg: (u8, u8, u8),
        bg: (u8, u8, u8),
        cursor: (u8, u8, u8),
        palette: [(u8, u8, u8); 16],
    ) -> Self {
        Self {
            name: name.into(),
            fg: rgb(fg),
            bg: rgb(bg),
            cursor: rgb(cursor),
            palette: palette.map(rgb),
        }
    }
}

fn rgb(c: (u8, u8, u8)) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

/// 内置主题（经典 iTerm2 配色 + 流行主题）。
pub fn builtin_themes() -> Vec<Theme> {
    vec![
        // iTerm2 默认：绿黑
        Theme::new(
            "Default",
            (204, 204, 204),
            (0, 0, 0),
            (204, 204, 204),
            [
                (0, 0, 0),
                (204, 0, 0),
                (0, 204, 0),
                (204, 204, 0),
                (0, 0, 204),
                (204, 0, 204),
                (0, 204, 204),
                (204, 204, 204),
                (128, 128, 128),
                (255, 0, 0),
                (0, 255, 0),
                (255, 255, 0),
                (0, 0, 255),
                (255, 0, 255),
                (0, 255, 255),
                (255, 255, 255),
            ],
        ),
        // Snazzy
        Theme::new(
            "Snazzy",
            (239, 240, 235),
            (40, 42, 54),
            (239, 240, 235),
            [
                (40, 42, 54),
                (255, 85, 85),
                (165, 255, 132),
                (255, 232, 95),
                (90, 144, 255),
                (255, 117, 255),
                (165, 255, 255),
                (239, 240, 235),
                (94, 95, 110),
                (255, 85, 85),
                (165, 255, 132),
                (255, 232, 95),
                (90, 144, 255),
                (255, 117, 255),
                (165, 255, 255),
                (239, 240, 235),
            ],
        ),
        // Dracula
        Theme::new(
            "Dracula",
            (248, 248, 242),
            (40, 42, 54),
            (248, 248, 242),
            [
                (0, 0, 0),
                (255, 85, 85),
                (80, 250, 123),
                (241, 250, 140),
                (189, 147, 249),
                (255, 121, 198),
                (139, 233, 253),
                (248, 248, 242),
                (98, 114, 164),
                (255, 85, 85),
                (80, 250, 123),
                (241, 250, 140),
                (189, 147, 249),
                (255, 121, 198),
                (139, 233, 253),
                (255, 255, 255),
            ],
        ),
        // Solarized Dark
        Theme::new(
            "Solarized Dark",
            (131, 148, 150),
            (0, 43, 54),
            (131, 148, 150),
            [
                (0, 43, 54),
                (220, 50, 47),
                (133, 153, 0),
                (181, 137, 0),
                (38, 139, 210),
                (211, 54, 130),
                (42, 161, 152),
                (238, 232, 213),
                (0, 83, 94),
                (203, 75, 22),
                (88, 110, 117),
                (101, 123, 131),
                (131, 148, 150),
                (108, 113, 196),
                (147, 161, 161),
                (253, 246, 227),
            ],
        ),
        // One Dark
        Theme::new(
            "One Dark",
            (171, 178, 191),
            (40, 44, 52),
            (171, 178, 191),
            [
                (40, 44, 52),
                (224, 108, 117),
                (152, 195, 121),
                (229, 192, 123),
                (97, 175, 239),
                (198, 120, 221),
                (86, 182, 194),
                (171, 178, 191),
                (92, 99, 112),
                (224, 108, 117),
                (152, 195, 121),
                (229, 192, 123),
                (97, 175, 239),
                (198, 120, 221),
                (86, 182, 194),
                (220, 223, 228),
            ],
        ),
        // Gruvbox Dark
        Theme::new(
            "Gruvbox Dark",
            (235, 219, 178),
            (40, 40, 40),
            (235, 219, 178),
            [
                (40, 40, 40),
                (204, 36, 29),
                (152, 151, 26),
                (215, 153, 33),
                (69, 133, 136),
                (177, 98, 134),
                (104, 157, 106),
                (235, 219, 178),
                (146, 131, 116),
                (204, 36, 29),
                (152, 151, 26),
                (215, 153, 33),
                (69, 133, 136),
                (177, 98, 134),
                (104, 157, 106),
                (251, 241, 199),
            ],
        ),
    ]
}

/// 主题目录（用户导入的主题存这里，toml 格式）。
pub fn themes_dir() -> std::path::PathBuf {
    ares_core::paths::config_dir().join("themes")
}

/// 全部可用主题名：内置 + 用户导入。
pub fn available_themes() -> Vec<String> {
    let mut names: Vec<String> = builtin_themes().iter().map(|t| t.name.clone()).collect();
    if let Ok(rd) = std::fs::read_dir(themes_dir()) {
        for de in rd.flatten() {
            let name = de.file_name().to_string_lossy().to_string();
            if name.ends_with(".toml") {
                let stem = name.trim_end_matches(".toml").to_string();
                if !names.contains(&stem) {
                    names.push(stem);
                }
            }
        }
    }
    names.sort();
    names
}

/// 按名取主题（内置优先，否则读用户目录）。
pub fn load_theme(name: &str) -> Theme {
    if let Some(t) = builtin_themes().into_iter().find(|t| t.name == name) {
        return t;
    }
    let path = themes_dir().join(format!("{name}.toml"));
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(t) = parse_theme_toml(&text) {
            return t;
        }
    }
    builtin_themes().into_iter().next().unwrap()
}

/// 解析 .itermcolors（XML plist）→ Theme。
pub fn parse_itermcolors(path: &std::path::Path) -> Result<Theme, String> {
    let data = std::fs::read(path).map_err(|e| format!("读取失败：{e}"))?;
    let plist = plist::Value::from_reader_xml(std::io::Cursor::new(data))
        .map_err(|e| format!("plist 解析失败（不是有效的 .itermcolors）：{e}"))?;
    let dict = plist.as_dictionary().ok_or("格式错误：根节点不是字典")?;

    let color = |key: &str| -> Option<(u8, u8, u8)> {
        let d = dict.get(key)?.as_dictionary()?;
        let r = d.get("Red Component")?.as_real()?;
        let g = d.get("Green Component")?.as_real()?;
        let b = d.get("Blue Component")?.as_real()?;
        Some((
            (r * 255.0).round() as u8,
            (g * 255.0).round() as u8,
            (b * 255.0).round() as u8,
        ))
    };

    let mut palette = [(0u8, 0u8, 0u8); 16];
    for (i, slot) in palette.iter_mut().enumerate() {
        if let Some(c) = color(&format!("Ansi {i} Color")) {
            *slot = c;
        }
    }
    let fg = color("Foreground Color").unwrap_or((204, 204, 204));
    let bg = color("Background Color").unwrap_or((0, 0, 0));
    let cursor = color("Cursor Color").unwrap_or(fg);

    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "Imported".into());
    Ok(Theme::new(&name, fg, bg, cursor, palette))
}

/// 导入 .itermcolors 到主题目录（toml），返回主题名。
pub fn import_itermcolors(path: &std::path::Path) -> Result<String, String> {
    let theme = parse_itermcolors(path)?;
    let dir = themes_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建主题目录失败：{e}"))?;
    let out = dir.join(format!("{}.toml", theme.name));
    std::fs::write(&out, serialize_theme_toml(&theme)).map_err(|e| format!("写入失败：{e}"))?;
    Ok(theme.name)
}

/// 主题 → toml（round-trip 持久化）。
fn serialize_theme_toml(t: &Theme) -> String {
    let mut s = String::new();
    s.push_str(&format!("name = \"{}\"\n", t.name));
    let c = |c: Color32| format!("[{}, {}, {}]", c.r(), c.g(), c.b());
    s.push_str(&format!("fg = {}\n", c(t.fg)));
    s.push_str(&format!("bg = {}\n", c(t.bg)));
    s.push_str(&format!("cursor = {}\n", c(t.cursor)));
    s.push_str("palette = [\n");
    for p in &t.palette {
        s.push_str(&format!("    {},\n", c(*p)));
    }
    s.push_str("]\n");
    s
}

/// 解析主题 toml（与 serialize 对称；也可手写）。
fn parse_theme_toml(text: &str) -> Result<Theme, String> {
    let v: toml::Value = toml::from_str(text).map_err(|e| format!("主题解析失败：{e}"))?;
    let name = v
        .get("name")
        .and_then(|x| x.as_str())
        .unwrap_or("Imported")
        .to_string();
    let arr = |key: &str| -> Option<(u8, u8, u8)> {
        let a = v.get(key)?.as_array()?;
        Some((
            a.first()?.as_integer()? as u8,
            a.get(1)?.as_integer()? as u8,
            a.get(2)?.as_integer()? as u8,
        ))
    };
    let fg = arr("fg").unwrap_or((204, 204, 204));
    let bg = arr("bg").unwrap_or((0, 0, 0));
    let cursor = arr("cursor").unwrap_or(fg);
    let mut palette = [(0u8, 0u8, 0u8); 16];
    if let Some(p) = v.get("palette").and_then(|x| x.as_array()) {
        for (i, item) in p.iter().enumerate().take(16) {
            if let Some(a) = item.as_array() {
                palette[i] = (
                    a.first().and_then(|x| x.as_integer()).unwrap_or(0) as u8,
                    a.get(1).and_then(|x| x.as_integer()).unwrap_or(0) as u8,
                    a.get(2).and_then(|x| x.as_integer()).unwrap_or(0) as u8,
                );
            }
        }
    }
    Ok(Theme::new(&name, fg, bg, cursor, palette))
}
