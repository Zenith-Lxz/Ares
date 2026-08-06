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
        let cjk = load_system_fonts(&[
            "PingFang SC",
            "Microsoft YaHei",
            "Noto Sans CJK SC",
            "WenQuanYi Micro Hei",
        ])
        .map(|data| {
            let data: &'static [u8] = Box::leak(data.into_boxed_slice());
            (Face::from_slice(data, 0), FontRef::from_index(data, 0))
        });
        let (cjk, cjk_swash) = cjk.unwrap_or((None, None));
        let emoji_swash =
            load_system_fonts(&["Apple Color Emoji", "Segoe UI Emoji", "Noto Color Emoji"])
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

/// 从系统字体源加载第一个匹配家族的字体字节。
fn load_system_fonts(families: &[&str]) -> Option<Vec<u8>> {
    let source = font_kit::source::SystemSource::new();
    for fam in families {
        if let Ok(family) = source.select_family_by_name(fam) {
            for handle in family.fonts() {
                if let Ok(data) = handle.load() {
                    return Some(data.copy_font_data()?.to_vec());
                }
            }
        }
    }
    None
}
