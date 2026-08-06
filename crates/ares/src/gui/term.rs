//! egui 终端渲染：把 vt100::Screen 画到界面上。
//!
//! 只画非空 cell（终端大部分区域是空的，跳过即可获得可接受的帧率）；
//! 256 色映射用标准 ANSI 算法；光标用反色块表示。
//! 主题化（2026-08-05 批次8）：Default 色 → 主题 fg/bg，ANSI 0-15 → 主题调色板。

use egui::{pos2, Align2, Color32, FontId, Rect, Vec2};

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

pub fn draw_terminal(
    ui: &mut egui::Ui,
    screen: &vt100::Screen,
    font: FontId,
    theme: &Theme,
    selection: Option<&SelectRange>,
    cursor_style: &str,
    cursor_blink: bool,
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

    // egui 即时模式：每帧全画（上一帧不保留）。静止时由数据驱动
    // repaint 控制（读线程无数据不触发重画 → egui 不重画帧 → 画面保留）。
    for r in 0..rows {
        draw_row(painter, screen, r, cols, &lay, theme, selection);
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
fn draw_row(
    painter: &egui::Painter,
    screen: &vt100::Screen,
    r: u16,
    cols: u16,
    lay: &RowLayout,
    theme: &Theme,
    selection: Option<&SelectRange>,
) {
    let base_y = lay.origin.y + r as f32 * lay.cell_h;
    // 整行空（无内容、无背景）快速跳过 —— 终端大部分行是空的
    let mut has_any = false;
    for c in 0..cols {
        if let Some(cell) = screen.cell(r, c) {
            if !cell.contents().is_empty() || cell.bgcolor() != vt100::Color::Default {
                has_any = true;
                break;
            }
        }
    }
    if !has_any {
        return;
    }
    // 段：(起始列, 文本, 前景色)
    let mut segments: Vec<(u16, String, Color32)> = Vec::new();
    for c in 0..cols {
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
        painter.text(
            pos2(lay.origin.x + start_col as f32 * lay.cell_w, base_y),
            Align2::LEFT_TOP,
            text,
            lay.font.clone(),
            fg,
        );
    }
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
