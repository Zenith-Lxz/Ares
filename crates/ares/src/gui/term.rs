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

/// 行渲染缓存：记录每行签名（文本+颜色），未变化的行跳过重画。
/// 终端静止时（提示符等待输入）零 text 绘制 —— 卡顿的主要来源。
#[derive(Default)]
pub struct TermCache {
    row_sigs: Vec<u64>,
}

/// 把 vt100 屏幕画进 ui 区域。返回实际渲染的（行数, 列数）。
pub fn draw_terminal(
    ui: &mut egui::Ui,
    screen: &vt100::Screen,
    font: FontId,
    cache: &mut TermCache,
) -> (u16, u16) {
    let (rows, cols) = screen.size();
    let painter = ui.painter();

    let cell_w = ui.fonts(|f| f.glyph_width(&font, 'M'));
    let cell_h = ui.fonts(|f| f.row_height(&font));
    if cell_w <= 0.0 || cell_h <= 0.0 {
        return (rows, cols);
    }

    // 扩展缓存
    if cache.row_sigs.len() < rows as usize {
        cache.row_sigs.resize(rows as usize, u64::MAX);
    }

    let origin = ui.min_rect().min;
    let cursor = screen.cursor_position();

    for r in 0..rows {
        let sig = row_signature(screen, r, cols);
        if cache.row_sigs[r as usize] == sig {
            continue; // 此行未变化
        }
        cache.row_sigs[r as usize] = sig;
        let lay = RowLayout {
            origin,
            cell_w,
            cell_h,
            font: font.clone(),
        };
        draw_row(painter, screen, r, cols, &lay);
    }

    // 光标：反色块（光标行总是重画一次，保证光标可见）
    let (cr, cc) = cursor;
    if cr < rows && cc < cols {
        // 光标行强制重画（签名可能没变化但光标位置变了）
        let sig = row_signature(screen, cr, cols);
        cache.row_sigs[cr as usize] = u64::MAX;
        let _ = sig;
        let lay = RowLayout {
            origin,
            cell_w,
            cell_h,
            font: font.clone(),
        };
        draw_row(painter, screen, cr, cols, &lay);
        cache.row_sigs[cr as usize] = row_signature(screen, cr, cols);
        let pos = pos2(origin.x + cc as f32 * cell_w, origin.y + cr as f32 * cell_h);
        if let Some(cell) = screen.cell(cr, cc) {
            let fg = color(cell.fgcolor());
            painter.rect_filled(Rect::from_min_size(pos, Vec2::new(cell_w, cell_h)), 0.0, fg);
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
fn draw_row(painter: &egui::Painter, screen: &vt100::Screen, r: u16, cols: u16, lay: &RowLayout) {
    let base_y = lay.origin.y + r as f32 * lay.cell_h;
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
        let bg = cell.bgcolor();
        if bg != vt100::Color::Default {
            painter.rect_filled(
                Rect::from_min_size(
                    pos,
                    Vec2::new(lay.cell_w * contents.chars().count() as f32, lay.cell_h),
                ),
                0.0,
                color(bg),
            );
        }
        let fg = color(cell.fgcolor());
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

/// 行签名：文本 + 前景/背景色的 FNV 哈希。颜色或内容任何变化都触发重画。
fn row_signature(screen: &vt100::Screen, r: u16, cols: u16) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for c in 0..cols {
        if let Some(cell) = screen.cell(r, c) {
            for b in cell.contents().bytes() {
                h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
            }
            h = (h ^ color_key(cell.fgcolor())).wrapping_mul(0x100_0000_01b3);
            h = (h ^ color_key(cell.bgcolor())).wrapping_mul(0x100_0000_01b3);
        }
        h = (h ^ 0x1f).wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn color_key(c: vt100::Color) -> u64 {
    match c {
        vt100::Color::Default => 0,
        vt100::Color::Idx(i) => 1 + i as u64,
        vt100::Color::Rgb(r, g, b) => {
            0x0100_0000 | ((r as u64) << 16) | ((g as u64) << 8) | b as u64
        }
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

    #[test]
    fn row_signature_differs_on_content_and_color() {
        let mut p = vt100::Parser::new(3, 40, 0);
        p.process(b"plain text");
        let s1 = p.screen();
        let sig_a = row_signature(s1, 0, 40);
        let _ = &s1;
        // 改颜色（同文本）
        p.process(b"\x1b[H\x1b[31mplain text");
        let sig_b = row_signature(p.screen(), 0, 40);
        assert_ne!(sig_a, sig_b, "颜色变化必须改变签名");
        // 清空重写不同文本
        p.process(b"\x1b[2J\x1b[Hother text");
        let sig_c = row_signature(p.screen(), 0, 40);
        assert_ne!(sig_a, sig_c, "内容变化必须改变签名");
        // 相同内容签名稳定
        let mut p2 = vt100::Parser::new(3, 40, 0);
        p2.process(b"plain text");
        assert_eq!(
            sig_a,
            row_signature(p2.screen(), 0, 40),
            "相同内容签名应一致"
        );
    }
}
