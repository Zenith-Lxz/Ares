//! egui 终端渲染：把 vt100::Screen 画到界面上。
//!
//! 只画非空 cell（终端大部分区域是空的，跳过即可获得可接受的帧率）；
//! 256 色映射用标准 ANSI 算法；光标用反色块表示。

use egui::{pos2, Align2, Color32, FontId, Rect, Vec2};

/// 标准 16 色（ANSI）。顺序：黑红绿黄蓝紫青白 + 亮色。
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

/// vt100 颜色 → egui Color32。
pub fn color(vt: vt100::Color) -> Color32 {
    match vt {
        vt100::Color::Default => Color32::GRAY,
        vt100::Color::Idx(i) => color_256(i),
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

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

/// 把 vt100 屏幕画进 ui 区域。返回实际渲染的（行数, 列数）。
pub fn draw_terminal(ui: &mut egui::Ui, screen: &vt100::Screen, font: FontId) -> (u16, u16) {
    let (rows, cols) = screen.size();
    let painter = ui.painter();

    let cell_w = ui.fonts(|f| f.glyph_width(&font, 'M'));
    let cell_h = ui.fonts(|f| f.row_height(&font));
    if cell_w <= 0.0 || cell_h <= 0.0 {
        return (rows, cols);
    }

    let origin = ui.min_rect().min;
    let cursor = screen.cursor_position();

    for r in 0..rows {
        let base_y = origin.y + r as f32 * cell_h;
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
            let x = origin.x + c as f32 * cell_w;
            let pos = pos2(x, base_y);

            let bg = cell.bgcolor();
            if bg != vt100::Color::Default {
                painter.rect_filled(
                    Rect::from_min_size(
                        pos,
                        Vec2::new(cell_w * contents.chars().count() as f32, cell_h),
                    ),
                    0.0,
                    color(bg),
                );
            }
            painter.text(
                pos,
                Align2::LEFT_TOP,
                contents,
                font.clone(),
                color(cell.fgcolor()),
            );
        }
    }

    // 光标：反色块
    let (cr, cc) = cursor;
    if cr < rows && cc < cols {
        let pos = pos2(origin.x + cc as f32 * cell_w, origin.y + cr as f32 * cell_h);
        if let Some(cell) = screen.cell(cr, cc) {
            let fg = color(cell.fgcolor());
            painter.rect_filled(Rect::from_min_size(pos, Vec2::new(cell_w, cell_h)), 0.0, fg);
        }
    }

    (rows, cols)
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
