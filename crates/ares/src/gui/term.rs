//! egui 终端渲染：把 vt100::Screen 画到界面上。
//!
//! 只画非空 cell（终端大部分区域是空的，跳过即可获得可接受的帧率）；
//! 256 色映射用标准 ANSI 算法；光标用反色块表示。
//! 主题化（2026-08-05 批次8）：Default 色 → 主题 fg/bg，ANSI 0-15 → 主题调色板。

use egui::{pos2, Color32, FontId, Rect, Vec2};

use super::themes::Theme;

/// 标准 16 色（ANSI）。顺序：黑红绿黄蓝紫青白 + 亮色。
/// 16 色段在渲染层被主题调色板拦截（fg_color/bg_color），此处保持
/// 标准表供测试与兜底。
const STANDARD: [Color32; 16] = [
    Color32::from_rgb(0, 0, 0),
    Color32::from_rgb(128, 0, 0),
    Color32::from_rgb(0, 128, 0),
    Color32::from_rgb(128, 128, 0),
    Color32::from_rgb(0, 0, 128),
    Color32::from_rgb(128, 0, 128),
    Color32::from_rgb(0, 128, 128),
    Color32::from_rgb(192, 192, 192),
    Color32::from_rgb(128, 128, 128),
    Color32::from_rgb(255, 0, 0),
    Color32::from_rgb(0, 255, 0),
    Color32::from_rgb(255, 255, 0),
    Color32::from_rgb(0, 0, 255),
    Color32::from_rgb(255, 0, 255),
    Color32::from_rgb(0, 255, 255),
    Color32::from_rgb(255, 255, 255),
];

/// 标准 256 色映射（0-15 用 STANDARD 表；渲染层 0-15 由主题调色板拦截）。
fn color_256(i: u8) -> Color32 {
    match i {
        0..=15 => STANDARD[i as usize],
        16..=231 => {
            let v = i - 16;
            let (r, g, b) = (v / 36, (v / 6) % 6, v % 6);
            let x = |c: u8| if c == 0 { 0 } else { 55 + c * 40 };
            Color32::from_rgb(x(r), x(g), x(b))
        }
        232..=255 => {
            let v = 8 + (i - 232) * 10;
            Color32::from_rgb(v, v, v)
        }
    }
}

/// 前景色：Default → 主题 fg；Idx 0-15 → 主题调色板；其余标准 256。
pub fn fg_color(vt: vt100::Color, theme: &Theme) -> Color32 {
    match vt {
        vt100::Color::Default => theme.fg,
        vt100::Color::Idx(i) if i < 16 => theme.palette[i as usize],
        vt100::Color::Idx(i) => color_256(i),
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

/// 背景色：Default → 主题 bg；其余同前景逻辑。
pub fn bg_color(vt: vt100::Color, theme: &Theme) -> Color32 {
    match vt {
        vt100::Color::Default => theme.bg,
        vt100::Color::Idx(i) if i < 16 => theme.palette[i as usize],
        vt100::Color::Idx(i) => color_256(i),
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

/// 终端选区（绘制行坐标，含滚动偏移；row0<=row1；若同行为单格）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectRange {
    pub row0: u16,
    pub col0: u16,
    pub row1: u16,
    pub col1: u16,
}

impl SelectRange {
    pub fn normalized(row0: u16, col0: u16, row1: u16, col1: u16) -> Self {
        if (row0, col0) <= (row1, col1) {
            Self {
                row0,
                col0,
                row1,
                col1,
            }
        } else {
            Self {
                row0: row1,
                col0: col1,
                row1: row0,
                col1: col0,
            }
        }
    }

    pub fn contains(&self, r: u16, c: u16) -> bool {
        if r < self.row0 || r > self.row1 {
            return false;
        }
        if r == self.row0 && c < self.col0 {
            return false;
        }
        if r == self.row1 && c > self.col1 {
            return false;
        }
        true
    }
}

/// 在文本中查找第一个 URL（http/https/ftp），返回 (start, end, url)。
pub fn find_url(text: &str) -> Option<(usize, usize, String)> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 7 <= bytes.len() {
        let scheme = bytes[i..].starts_with(b"http://")
            || bytes[i..].starts_with(b"https://")
            || bytes[i..].starts_with(b"ftp://");
        if scheme {
            let mut j = i + 7;
            while j < bytes.len() {
                let b = bytes[j];
                if b.is_ascii_whitespace()
                    || b == b'"'
                    || b == b'\''
                    || b == b'<'
                    || b == b'>'
                    || b == b')'
                    || b == b']'
                {
                    break;
                }
                j += 1;
            }
            return Some((i, j - 1, text[i..j].to_string()));
        }
        i += 1;
    }
    None
}

pub fn draw_terminal(
    ui: &mut egui::Ui,
    screen: &vt100::Screen,
    font: FontId,
    theme: &Theme,
    selection: Option<&SelectRange>,
    cursor_style: &str,
    cursor_blink: bool,
    images: &[(u16, u16, Vec<u8>)],
    ligs: Option<&crate::gui::ligatures::LigatureEngine>,
) -> (u16, u16) {
    let (rows, cols) = screen.size();
    let painter = ui.painter();

    let cell_w = ui.fonts(|f| f.glyph_width(&font, 'M'));
    let cell_h = ui.fonts(|f| f.row_height(&font));
    if cell_w <= 0.0 || cell_h <= 0.0 {
        return (rows, cols);
    }

    let origin = ui.min_rect().min;
    let cursor = screen.cursor_position();
    let lay = RowLayout {
        origin,
        cell_w,
        cell_h,
        font: font.clone(),
    };

    // 内联图片（M6）：解码 → texture → 绘制；记录遮挡区（图片覆盖的 cell 不再画）
    let mut blocked: Vec<std::collections::HashSet<u16>> =
        vec![std::collections::HashSet::new(); rows as usize];
    for (irow, icol, data) in images {
        let img = match image::load_from_memory(data.as_slice()) {
            Ok(img) => img.to_rgba8(),
            Err(_) => continue,
        };
        let (w, h) = (img.width(), img.height());
        let w_cells = (w as f32 / cell_w).ceil().max(1.0) as u16;
        let h_cells = (h as f32 / cell_h).ceil().max(1.0) as u16;
        if *irow >= rows || *icol >= cols {
            continue;
        }
        let tex = ui.ctx().load_texture(
            format!("inline-{irow}-{icol}"),
            egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw()),
            egui::TextureOptions::LINEAR,
        );
        let pos = pos2(
            origin.x + *icol as f32 * cell_w,
            origin.y + *irow as f32 * cell_h,
        );
        let rect = Rect::from_min_size(
            pos,
            Vec2::new(w_cells as f32 * cell_w, h_cells as f32 * cell_h),
        );
        painter.image(
            tex.id(),
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        for br in *irow..(*irow + h_cells).min(rows) {
            for bc in *icol..(*icol + w_cells).min(cols) {
                blocked[br as usize].insert(bc);
            }
        }
    }

    // egui 即时模式：每帧全画（上一帧不保留）。静止时由数据驱动
    // repaint 控制（读线程无数据不触发重画 → egui 不重画帧 → 画面保留）。
    for r in 0..rows {
        draw_row(
            painter, screen, r, cols, &lay, theme, selection, &blocked, ligs,
        );
    }

    // 光标（M3）：block / beam / underline，可选闪烁（0.5s 周期）
    let (cr, cc) = cursor;
    if cr < rows && cc < cols {
        let blink_off = cursor_blink && (ui.input(|i| i.time) as u64) % 2 == 1;
        if !blink_off {
            let pos = pos2(origin.x + cc as f32 * cell_w, origin.y + cr as f32 * cell_h);
            let cursor_color = theme.cursor;
            match cursor_style {
                "beam" => {
                    painter.rect_filled(
                        Rect::from_min_size(pos, Vec2::new(2.0, cell_h)),
                        0.0,
                        cursor_color,
                    );
                }
                "underline" => {
                    painter.rect_filled(
                        Rect::from_min_size(
                            pos + egui::vec2(0.0, cell_h - 2.0),
                            Vec2::new(cell_w, 2.0),
                        ),
                        0.0,
                        cursor_color,
                    );
                }
                _ => {
                    // block：主题色反色块
                    if let Some(cell) = screen.cell(cr, cc) {
                        let fg = fg_color(cell.fgcolor(), theme);
                        painter.rect_filled(
                            Rect::from_min_size(pos, Vec2::new(cell_w, cell_h)),
                            0.0,
                            fg,
                        );
                    } else {
                        painter.rect_filled(
                            Rect::from_min_size(pos, Vec2::new(cell_w, cell_h)),
                            0.0,
                            cursor_color,
                        );
                    }
                }
            }
        }
    }

    (rows, cols)
}

/// 行绘制布局参数。
struct RowLayout {
    origin: egui::Pos2,
    cell_w: f32,
    cell_h: f32,
    font: FontId,
}

/// 一行内按「同色连续段」合并绘制（减少 text 调用）。
#[allow(clippy::too_many_arguments)]
fn draw_row(
    painter: &egui::Painter,
    screen: &vt100::Screen,
    r: u16,
    cols: u16,
    lay: &RowLayout,
    theme: &Theme,
    selection: Option<&SelectRange>,
    blocked: &[std::collections::HashSet<u16>],
    ligs: Option<&crate::gui::ligatures::LigatureEngine>,
) {
    let base_y = lay.origin.y + r as f32 * lay.cell_h;
    // 段：(起始列, 文本, 前景色)
    let mut segments: Vec<(u16, String, Color32)> = Vec::new();
    for c in 0..cols {
        if blocked.get(r as usize).map_or(false, |s| s.contains(&c)) {
            continue; // 图片遮挡区（M6）
        }
        let Some(cell) = screen.cell(r, c) else {
            continue;
        };
        if cell.is_wide_continuation() {
            continue;
        }
        let contents = cell.contents();
        if contents.is_empty() {
            continue;
        }
        let pos = pos2(lay.origin.x + c as f32 * lay.cell_w, base_y);
        let selected = selection.map_or(false, |s| s.contains(r, c));
        let bg = cell.bgcolor();
        if bg != vt100::Color::Default {
            painter.rect_filled(
                Rect::from_min_size(
                    pos,
                    Vec2::new(lay.cell_w * contents.chars().count() as f32, lay.cell_h),
                ),
                0.0,
                bg_color(bg, theme),
            );
        }
        let fg = fg_color(cell.fgcolor(), theme);
        // 选中：前景/背景反色（macOS 终端风格高亮）
        let (fg, bg) = if selected {
            (bg_color(bg, theme), fg)
        } else {
            (fg, bg_color(bg, theme))
        };
        if selected {
            painter.rect_filled(
                Rect::from_min_size(
                    pos,
                    Vec2::new(lay.cell_w * contents.chars().count() as f32, lay.cell_h),
                ),
                0.0,
                bg,
            );
        }
        match segments.last_mut() {
            Some((_, text, last_fg)) if *last_fg == fg => text.push_str(&contents),
            _ => segments.push((c, contents, fg)),
        }
    }
    for (start_col, text, fg) in segments {
        let pos = pos2(lay.origin.x + start_col as f32 * lay.cell_w, base_y);
        draw_text_segment(painter, pos, &text, &lay, fg, ligs);
    }
}

/// 绘制一段文本：URL 下划线（M5）+ 连字位图（M8）+ 普通文本。
fn draw_text_segment(
    painter: &egui::Painter,
    pos: egui::Pos2,
    text: &str,
    lay: &RowLayout,
    fg: Color32,
    ligs: Option<&crate::gui::ligatures::LigatureEngine>,
) {
    let link_col = Color32::from_rgb(80, 160, 255);
    let mut x = pos.x;
    let mut i = 0usize;
    let text_len = text.len();
    while i < text_len {
        // 连字优先（最长匹配）
        let lig_hit = ligs.and_then(|l| l.find_at(text, i));
        if let Some((ls, le, lig)) = lig_hit {
            if ls > i {
                let part = &text[i..ls];
                x += paint_plain(painter, egui::pos2(x, pos.y), part, lay, fg);
            }
            painter.image(
                lig.tex.id(),
                egui::Rect::from_min_size(
                    egui::pos2(x, pos.y),
                    egui::vec2(lig.width_px, lig.height_px),
                ),
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            x += lig.width_px;
            i = le;
            continue;
        }
        // URL 检测（在当前剩余部分找）
        if let Some((s, e, _)) = find_url(&text[i..]) {
            let (s, e) = (s + i, e + i);
            if s > i {
                let part = &text[i..s];
                x += paint_plain(painter, egui::pos2(x, pos.y), part, lay, fg);
            }
            let mut job = egui::text::LayoutJob::default();
            let fmt = |color: Color32, underline: bool| egui::text::TextFormat {
                font_id: lay.font.clone(),
                color,
                underline: if underline {
                    egui::Stroke::new(1.0_f32, link_col)
                } else {
                    egui::Stroke::NONE
                },
                ..Default::default()
            };
            job.append(&text[s..=e], 0.0, fmt(link_col, true));
            let galley = painter.layout_job(job);
            let w = galley.size().x;
            painter.galley(egui::pos2(x, pos.y), galley, fg);
            x += w;
            i = e + 1;
            continue;
        }
        // 普通余段
        let part = &text[i..];
        x += paint_plain(painter, egui::pos2(x, pos.y), part, lay, fg);
        break;
    }
}

/// 绘制普通文本并返回像素宽度。
fn paint_plain(
    painter: &egui::Painter,
    pos: egui::Pos2,
    text: &str,
    lay: &RowLayout,
    fg: Color32,
) -> f32 {
    let galley = painter.layout(text.to_string(), lay.font.clone(), fg, f32::INFINITY);
    let w = galley.size().x;
    painter.galley(pos, galley, fg);
    w
}

/// 从 ui 区域推导终端行列数（等宽字体）。
pub fn size_for(ui: &egui::Ui, font: &FontId) -> (u16, u16) {
    let cell_w = ui.fonts(|f| f.glyph_width(font, 'M'));
    let cell_h = ui.fonts(|f| f.row_height(font));
    let area = ui.available_size();
    let cols = (area.x / cell_w.max(1.0)).floor().max(20.0) as u16;
    let rows = (area.y / cell_h.max(1.0)).floor().max(5.0) as u16;
    (rows, cols)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_16_colors_match_ansi() {
        assert_eq!(color_256(0), Color32::from_rgb(0, 0, 0));
        assert_eq!(color_256(1), Color32::from_rgb(128, 0, 0));
        assert_eq!(color_256(9), Color32::from_rgb(255, 0, 0));
        assert_eq!(color_256(15), Color32::from_rgb(255, 255, 255));
    }

    #[test]
    fn cube_colors_are_smooth() {
        // 16 + 36*5 + 6*5 + 5 = 231 → rgb(255, 255, 255)
        assert_eq!(color_256(231), Color32::from_rgb(255, 255, 255));
        // 16 → rgb(0,0,0)
        assert_eq!(color_256(16), Color32::from_rgb(0, 0, 0));
    }

    #[test]
    fn gray_ramp_monotonic() {
        assert_eq!(color_256(232), Color32::from_rgb(8, 8, 8));
        assert_eq!(color_256(255), Color32::from_rgb(238, 238, 238));
    }
}
