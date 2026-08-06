//! 字体集管理：Fira Code（连字等宽）+ 系统 CJK + 系统彩色 emoji。
//!
//! shaping 用 rustybuzz（Fira Code 开 calt 连字；CJK 用系统字体）；
//! 栅格化用 swash（支持彩色 emoji：COLR/CBDT/sbix）。

use rustybuzz::{Face, UnicodeBuffer};
use swash::FontRef;

pub const FONT_FIRA: u32 = 0;
pub const FONT_CJK: u32 = 1;
pub const FONT_EMOJI: u32 = 2;

const FIRA_CODE: &[u8] = include_bytes!("../../../../../assets/fonts/FiraCode-Regular.ttf");

pub struct FontSet {
    /// Fira Code（连字 shaping + 栅格化）
    pub fira: Face<'static>,
    pub fira_swash: FontRef<'static>,
    /// 系统 CJK 字体（PingFang SC / Microsoft YaHei / Noto Sans CJK）
    pub cjk: Option<Face<'static>>,
    pub cjk_swash: Option<FontRef<'static>>,
    /// 系统彩色 emoji 字体
    pub emoji_swash: Option<FontRef<'static>>,
}

/// 字体种类（按字符范围路由）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontKind {
    Mono,
    Cjk,
    Emoji,
}

impl FontSet {
    pub fn new() -> Self {
        let fira = Face::from_slice(FIRA_CODE, 0).expect("Fira Code 内嵌字体损坏");
        let fira_swash = FontRef::from_index(FIRA_CODE, 0).expect("Fira Code swash 加载失败");
        // 系统字体数据 leak 到 'static（进程生命周期内有效，~10-20MB）
        // font-kit 拿不到时直接读系统字体文件兜底（macOS 路径）
        let cjk = load_system_fonts(
            &[
                "PingFang SC",
                "Microsoft YaHei",
                "Noto Sans CJK SC",
                "WenQuanYi Micro Hei",
            ],
            &[
                "/System/Library/Fonts/PingFang.ttc",
                "/System/Library/Fonts/STHeiti Light.ttc",
                "/System/Library/Fonts/Hiragino Sans GB.ttc",
                "/System/Library/Fonts/Supplemental/Songti.ttc",
            ],
        )
        .map(|data| {
            let data: &'static [u8] = Box::leak(data.into_boxed_slice());
            (Face::from_slice(data, 0), FontRef::from_index(data, 0))
        });
        let (cjk, cjk_swash) = cjk.unwrap_or((None, None));
        let emoji_swash = load_system_fonts(
            &["Apple Color Emoji", "Segoe UI Emoji", "Noto Color Emoji"],
            &["/System/Library/Fonts/Apple Color Emoji.ttc"],
        )
        .and_then(|data| {
            let data: &'static [u8] = Box::leak(data.into_boxed_slice());
            FontRef::from_index(data, 0)
        });
        Self {
            fira,
            fira_swash,
            cjk,
            cjk_swash,
            emoji_swash,
        }
    }

    pub fn face_for(&self, kind: FontKind) -> Option<&Face<'static>> {
        match kind {
            FontKind::Mono => Some(&self.fira),
            FontKind::Cjk => self.cjk.as_ref(),
            FontKind::Emoji => None, // emoji 不走 shaping（直接查 charmap）
        }
    }

    /// 字符 → 字体种类（emoji 判定：emoji 字体 charmap 有映射）。
    pub fn char_kind(&self, c: char) -> FontKind {
        if c.is_ascii() {
            return FontKind::Mono;
        }
        // emoji 区（含 VS16 / ZWJ / 肤色）
        let cp = c as u32;
        if (0x1F000..=0x1FAFF).contains(&cp)
            || (0x2600..=0x27BF).contains(&cp)
            || (0x2B00..=0x2BFF).contains(&cp)
            || cp == 0xFE0F
            || cp == 0x200D
            || (0x1F3FB..=0x1F3FF).contains(&cp)
        {
            return FontKind::Emoji;
        }
        // 其余非 ASCII → CJK / 符号 fallback
        FontKind::Cjk
    }

    /// 基线在 cell 内的比例（ascent/(ascent+descent)），字形垂直定位用。
    pub fn baseline_ratio(&self, kind: FontKind) -> f32 {
        let m = match kind {
            FontKind::Cjk => self.cjk_swash.as_ref().map(|f| f.metrics(&[])),
            _ => Some(self.fira_swash.metrics(&[])),
        };
        match m {
            Some(m) => {
                let asc = m.ascent;
                let desc = m.descent;
                if asc - desc > 0.0 {
                    asc / (asc - desc)
                } else {
                    0.8
                }
            }
            None => 0.8,
        }
    }

    /// 文本段 shaping（连字等宽字体）；返回 (glyph_id, x_advance_units)。
    pub fn shape(&self, kind: FontKind, text: &str) -> Vec<(u16, i32)> {
        let Some(face) = self.face_for(kind) else {
            return Vec::new();
        };
        let mut buffer = UnicodeBuffer::new();
        buffer.push_str(text);
        let glyphs = rustybuzz::shape(face, &[], buffer);
        glyphs
            .glyph_infos()
            .iter()
            .zip(glyphs.glyph_positions().iter())
            .map(|(info, pos)| (info.glyph_id as u16, pos.x_advance))
            .collect()
    }
}

/// 从系统字体源加载字体字节：先 font-kit 家族查询，失败直接读系统字体文件。
fn load_system_fonts(families: &[&str], file_fallbacks: &[&str]) -> Option<Vec<u8>> {
    let source = font_kit::source::SystemSource::new();
    for fam in families {
        if let Ok(family) = source.select_family_by_name(fam) {
            for handle in family.fonts() {
                if let Ok(data) = handle.load() {
                    if let Some(bytes) = data.copy_font_data() {
                        return Some(bytes.to_vec());
                    }
                }
            }
        }
    }
    for path in file_fallbacks {
        if let Ok(bytes) = std::fs::read(path) {
            return Some(bytes);
        }
    }
    None
}
