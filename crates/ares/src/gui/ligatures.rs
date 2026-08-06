//! 字体连字引擎（M8）：egui/ab_glyph 不支持 GSUB contextual ligatures，
//! 方案：rustybuzz 对连字序列做 shaping（启用 calt）→ 得到 glyph 序列 →
//! fontdue 按 glyph id 栅格化 → 合成一张位图 → 渲染时以图像绘制。
//!
//! 覆盖 Fira Code 最常用连字（内置字体文件，OFL 许可）。

use egui::{Color32, ColorImage, TextureHandle, TextureOptions};

const FIRA_CODE: &[u8] = include_bytes!("../../../../assets/fonts/FiraCode-Regular.ttf");

/// 常用连字列表（按长度降序，贪心最长匹配）。
const LIGATURES: &[&str] = &[
    "===", "!==", "===", "=>", "->", "<-", "<=", ">=", "==", "!=", "&&", "||", "::", "##", "**",
    "//", "..", "...", ":=", "=~", "!~", "++", "--", "-=", "+=", "*=", "/=", "^=", "%=", "<<",
    ">>", "<>", "<=>", "->>", "<-<", ">>>", "<<<", "|>", "<|", "..<", ">..", "~>", "?=", "/=",
    "//=", "&&=", "||=", "%%", "%%=",
];

/// 一个连字位图（含纹理句柄，直接绘制）。
pub struct Ligature {
    pub text: &'static str,
    pub tex: TextureHandle,
    pub width_px: f32,
    pub height_px: f32,
}

pub struct LigatureEngine {
    pub ligs: Vec<Ligature>,
}

impl LigatureEngine {
    /// 构建连字表（按像素高度栅格化）。
    pub fn build(ctx: &egui::Context, height_px: f32) -> Self {
        let mut ligs = Vec::new();
        let Some(face) = rustybuzz::Face::from_slice(FIRA_CODE, 0) else {
            return Self { ligs };
        };
        let upem = face.units_per_em() as f32;
        let scale = height_px / upem;
        let Some(font) =
            fontdue::Font::from_bytes(FIRA_CODE, fontdue::FontSettings::default()).ok()
        else {
            return Self { ligs };
        };
        // 去重排序：长串优先
        let mut list: Vec<&'static str> = LIGATURES.to_vec();
        list.sort_by_key(|s| std::cmp::Reverse(s.len()));
        list.dedup();
        for text in list {
            // shaping（默认特性含 calt → 连字生效）
            let mut buffer = rustybuzz::UnicodeBuffer::new();
            buffer.push_str(text);
            let glyphs = rustybuzz::shape(&face, &[], buffer);
            let infos = glyphs.glyph_infos();
            let positions = glyphs.glyph_positions();
            if infos.is_empty() {
                continue;
            }
            // 总宽（units → 像素）
            let total_units: f32 = positions.iter().map(|p| p.x_advance as f32).sum();
            let total_px = total_units * scale;
            if total_px <= 0.0 {
                continue;
            }
            // 按 glyph 栅格化合成（以 cell 高度为画布，glyph 顶部对齐）
            let px = height_px as usize;
            let w = (total_px.ceil() as usize).max(2);
            let mut img = vec![0u8; w * px * 4];
            let mut x_off: f32 = 0.0;
            for (info, pos) in infos.iter().zip(positions.iter()) {
                let gid = info.glyph_id as u16;
                let (metrics, bitmap) = font.rasterize_indexed(gid, height_px);
                if bitmap.is_empty() {
                    x_off += pos.x_advance as f32 * scale;
                    continue;
                }
                let start_x = (x_off + metrics.xmin as f32).round() as i32;
                let target_y = px as i32 - metrics.height as i32 - metrics.ymin as i32;
                for (by, row) in bitmap.chunks(metrics.width).enumerate() {
                    let ty = target_y + by as i32;
                    if ty < 0 || ty >= px as i32 {
                        continue;
                    }
                    for (bx, &a) in row.iter().enumerate() {
                        let tx = start_x + bx as i32;
                        if tx < 0 || tx >= w as i32 {
                            continue;
                        }
                        let idx = (ty as usize * w + tx as usize) * 4;
                        img[idx] = 230;
                        img[idx + 1] = 230;
                        img[idx + 2] = 230;
                        img[idx + 3] = a;
                    }
                }
                x_off += pos.x_advance as f32 * scale;
            }
            let color_img = ColorImage {
                size: [w, px],
                pixels: img
                    .chunks(4)
                    .map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
                    .collect(),
            };
            let tex = ctx.load_texture(format!("lig-{text}"), color_img, TextureOptions::LINEAR);
            ligs.push(Ligature {
                text,
                tex,
                width_px: total_px,
                height_px,
            });
        }
        Self { ligs }
    }

    /// 从 text[pos..] 找最长匹配连字；返回 (start, end, lig)。
    pub fn find_at<'a>(&'a self, text: &str, pos: usize) -> Option<(usize, usize, &'a Ligature)> {
        let rest = &text[pos..];
        for lig in &self.ligs {
            if rest.starts_with(lig.text) {
                return Some((pos, pos + lig.text.len(), lig));
            }
        }
        None
    }
}
