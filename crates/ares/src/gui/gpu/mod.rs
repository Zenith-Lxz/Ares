//! 自研 wgpu 终端渲染器（α 架构：Alacritty 式 GPU 渲染）。
//!
//! - 字形 atlas：etagere 打包 + swash 栅格化（含彩色 emoji）
//! - 连字：Fira Code（rustybuzz calt shaping）
//! - 行缓存：内容 hash 变化才重建 shaping（静态行零 CPU）
//! - 合成：终端渲染到 offscreen 纹理 → egui 原生纹理注册 → painter.image
//!
//! 与 egui 渲染路径可切换（`ARES_RENDER=egui` 回退旧实现）。

pub mod atlas;
pub mod fonts;

use crate::gui::term::SelectRange;
use crate::gui::themes::Theme;
use atlas::GlyphAtlas;
use egui::{Color32, Rect};
use fonts::{FontKind, FontSet, FONT_CJK, FONT_EMOJI, FONT_FIRA};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// 顶点（位置 NDC / UV / 颜色）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vert {
    pos: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
}

/// 一行内一个绘制单元（背景 quad 或字形 quad）。
#[derive(Clone)]
struct RowGlyph {
    /// 目标矩形（逻辑像素，相对终端区域原点）
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    /// atlas UV（整纹理 UV）
    uv: [f32; 4],
    /// 顶点色（premultiplied 前）
    color: [f32; 4],
}

/// 文本段（连续同 fg/bg/字体 的 cells）。
struct Seg {
    start: u16,
    text: String,
    fg: Color32,
    bg: Option<Color32>,
    kind: FontKind,
    bold: bool,
    underline: bool,
}

pub struct GpuTerminalRenderer {
    device: std::sync::Arc<wgpu::Device>,
    queue: std::sync::Arc<wgpu::Queue>,
    atlas: GlyphAtlas,
    fonts: FontSet,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    /// 终端渲染目标（物理像素尺寸，resize 重建）
    target: Option<(wgpu::Texture, wgpu::TextureView, u32, u32)>,
    /// 注册给 egui 的纹理 id（target 重建后需重注册）
    egui_tex: Option<egui::TextureId>,
    /// 行内容 hash（跳过未变化行的 shaping）
    row_hashes: Vec<u64>,
    /// 行渲染数据缓存
    row_glyphs: Vec<Vec<RowGlyph>>,
    /// pixels per point（高分屏）
    scale: f32,
    /// 行高（逻辑像素，变化时重建字体尺寸缓存）
    cell_h: f32,
    /// 主题快照（变化时全行失效）
    theme_key: u64,
    /// swash 栅格化上下文
    raster: swash::scale::ScaleContext,
    /// 1x1 白色像素在 atlas 的位置（背景 quad 采样用）
    white_px: [f32; 4],
}

/// 索引缓冲：每个 quad 6 索引（固定 64k quad 上限）。
const MAX_QUADS: usize = 65535;

impl GpuTerminalRenderer {
    pub fn new(
        device: std::sync::Arc<wgpu::Device>,
        queue: std::sync::Arc<wgpu::Queue>,
        scale: f32,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ares-terminal-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ares-terminal-pipeline"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vert>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let mut atlas = GlyphAtlas::new(&device, 2048);
        // 预置 1x1 白像素（背景 quad 采样）
        let white_rect = atlas
            .get_or_insert(0, 0, 0, 1, 1, &[255, 255, 255, 255])
            .unwrap();
        let white_uv = atlas::rect_to_uv(&white_rect, 2048.0);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ares-terminal-bind"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&atlas.sampler),
                },
            ],
        });
        let vbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ares-term-vbuf"),
            size: (MAX_QUADS * 4 * std::mem::size_of::<Vert>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let ibuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ares-term-ibuf"),
            size: (MAX_QUADS * 6 * std::mem::size_of::<u32>()) as u64,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // 索引序列固定
        let indices: Vec<u32> = (0..MAX_QUADS as u32)
            .flat_map(|q| [q * 4, q * 4 + 1, q * 4 + 2, q * 4, q * 4 + 2, q * 4 + 3])
            .collect();
        queue.write_buffer(&ibuf, 0, bytemuck::cast_slice(&indices));

        Self {
            device,
            queue,
            atlas,
            fonts: FontSet::new(),
            pipeline,
            bind_group,
            vbuf,
            ibuf,
            target: None,
            egui_tex: None,
            row_hashes: Vec::new(),
            row_glyphs: Vec::new(),
            scale,
            cell_h: 18.0,
            theme_key: 0,
            raster: swash::scale::ScaleContext::new(),
            white_px: white_uv,
        }
    }

    /// 渲染终端到纹理；返回 egui TextureId（调用方 painter.image 绘制）。
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        screen: &vt100::Screen,
        theme: &Theme,
        selection: Option<&SelectRange>,
        cursor_style: &str,
        cursor_blink: bool,
        rect: Rect,
        renderer: &mut egui_wgpu::Renderer,
    ) -> Option<egui::TextureId> {
        let (rows, cols) = screen.size();
        if rows == 0 || cols == 0 || rect.width() <= 1.0 || rect.height() <= 1.0 {
            return self.egui_tex;
        }
        let phys_w = (rect.width() * self.scale).round().max(1.0) as u32;
        let phys_h = (rect.height() * self.scale).round().max(1.0) as u32;
        let cell_w = rect.width() / cols as f32;
        let cell_h = rect.height() / rows as f32;

        // 主题/字号变化 → 全部行失效
        let mut th = DefaultHasher::new();
        theme.bg.hash(&mut th);
        theme.fg.hash(&mut th);
        (cell_h as u32).hash(&mut th);
        let tkey = th.finish();
        if tkey != self.theme_key || self.row_hashes.len() != rows as usize {
            self.theme_key = tkey;
            self.row_hashes = vec![u64::MAX; rows as usize];
            self.row_glyphs = vec![Vec::new(); rows as usize];
            self.cell_h = cell_h;
        }

        // 目标纹理（resize 重建 + egui 重注册）
        if self.target.as_ref().map(|t| (t.2, t.3)) != Some((phys_w, phys_h)) {
            self.target = Some(create_target(&self.device, phys_w, phys_h));
            self.egui_tex = None;
            if let Some((_, view, _, _)) = &self.target {
                let id =
                    renderer.register_native_texture(&self.device, view, wgpu::FilterMode::Linear);
                self.egui_tex = Some(id);
            }
        }

        // 逐行：hash 变化才重建 shaping
        for r in 0..rows {
            let h = row_hash(
                screen,
                r,
                cols,
                selection,
                cursor_style,
                cursor_blink,
                r == screen.cursor_position().0,
            );
            if self.row_hashes[r as usize] == h {
                continue;
            }
            self.row_hashes[r as usize] = h;
            self.row_glyphs[r as usize] = self.build_row(
                screen,
                r,
                cols,
                cell_w,
                cell_h,
                theme,
                selection,
                cursor_style,
                cursor_blink,
            );
        }

        // 顶点组装（全量重建，~百 μs 级）
        let mut verts: Vec<Vert> = Vec::with_capacity(rows as usize * 64 * 4);
        for glyphs in self.row_glyphs.iter() {
            for g in glyphs {
                push_quad(&mut verts, g, phys_w, phys_h, self.scale);
            }
        }
        if !verts.is_empty() {
            self.queue
                .write_buffer(&self.vbuf, 0, bytemuck::cast_slice(&verts));
        }

        // atlas 新字形上传
        self.atlas.upload(&self.queue);

        // 渲染 pass
        if let Some((_, view, _, _)) = &self.target {
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("ares-term-encoder"),
                });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("ares-term-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 0.0,
                                g: 0.0,
                                b: 0.0,
                                a: 0.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, self.vbuf.slice(..));
                pass.set_index_buffer(self.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                let quads = (verts.len() / 4).min(MAX_QUADS) as u32;
                if quads > 0 {
                    pass.draw_indexed(0..quads * 6, 0, 0..1);
                }
            }
            self.queue.submit(Some(encoder.finish()));
        }

        self.egui_tex
    }

    /// 重建一行：扫描 cells → 段（fg/bg/font 分组）→ shaping → atlas → RowGlyph。
    #[allow(clippy::too_many_arguments)]
    fn build_row(
        &mut self,
        screen: &vt100::Screen,
        r: u16,
        cols: u16,
        cell_w: f32,
        cell_h: f32,
        theme: &Theme,
        selection: Option<&SelectRange>,
        cursor_style: &str,
        cursor_blink: bool,
    ) -> Vec<RowGlyph> {
        let mut out = Vec::new();
        let cursor = screen.cursor_position();
        let is_cursor_row = r == cursor.0;
        let blink_off = cursor_blink
            && (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64 / 500 % 2 == 1)
                .unwrap_or(false));

        // 收集段：(start_col, text, fg, bg, kind, bold, underline)
        let mut segs: Vec<Seg> = Vec::new();
        let mut c = 0u16;
        while c < cols {
            let Some(cell) = screen.cell(r, c) else {
                c += 1;
                continue;
            };
            if cell.is_wide_continuation() {
                c += 1;
                continue;
            }
            let contents = cell.contents();
            let is_selected = selection.is_some_and(|s| s.contains(r, c));
            // 光标块：反色
            let mut fg = color_of(cell.fgcolor(), theme);
            let mut bg: Option<Color32> = match cell.bgcolor() {
                vt100::Color::Default => None,
                b => Some(color_of(b, theme)),
            };
            if is_cursor_row && c == cursor.1 && cursor_style == "block" && !blink_off {
                // 块光标反色：字符 = 原背景色（Default → 终端底色），块底 = 原前景色
                let orig_bg = match cell.bgcolor() {
                    vt100::Color::Default => theme.bg,
                    b => color_of(b, theme),
                };
                fg = orig_bg;
                bg = Some(color_of(cell.fgcolor(), theme));
            }
            if is_selected {
                // 选区反色
                let b = bg.unwrap_or(theme.bg);
                bg = Some(fg);
                fg = b;
            }
            if cell.bold() {
                // 粗体提亮
                fg = brighten(fg, 1.25);
            }
            let kind = self.fonts.char_kind(contents.chars().next().unwrap_or(' '));
            let prev = segs.last_mut();
            if let Some(p) = prev {
                if p.fg == fg
                    && p.bg == bg
                    && p.kind == kind
                    && p.bold == cell.bold()
                    && p.underline == cell.underline()
                {
                    p.text.push_str(&contents);
                    c += 1;
                    continue;
                }
            }
            segs.push(Seg {
                start: c,
                text: contents,
                fg,
                bg,
                kind,
                bold: cell.bold(),
                underline: cell.underline(),
            });
            c += 1;
        }

        // 背景 quad
        for s in &segs {
            if let Some(bg) = s.bg {
                let x = s.start as f32 * cell_w;
                out.push(RowGlyph {
                    x,
                    y: 0.0,
                    w: s.text.chars().count() as f32 * cell_w,
                    h: cell_h,
                    uv: self.white_px,
                    color: to_rgba(bg),
                });
            }
        }
        // 字形（含 emoji / 连字）
        for s in &segs {
            match s.kind {
                FontKind::Emoji => self.push_emoji(&mut out, s, cell_w, cell_h),
                _ => self.push_shaped(&mut out, s, cell_w, cell_h),
            }
        }
        // 光标 beam/underline
        if is_cursor_row && !blink_off {
            let cc = cursor.1;
            match cursor_style {
                "beam" => out.push(RowGlyph {
                    x: cc as f32 * cell_w,
                    y: 0.0,
                    w: 2.0,
                    h: cell_h,
                    uv: self.white_px,
                    color: to_rgba(theme.cursor),
                }),
                "underline" => out.push(RowGlyph {
                    x: cc as f32 * cell_w,
                    y: cell_h - 2.0,
                    w: cell_w,
                    h: 2.0,
                    uv: self.white_px,
                    color: to_rgba(theme.cursor),
                }),
                _ => {}
            }
        }
        out
    }

    /// 普通文本段 shaping + 栅格化（连字/Fira/CJK）。
    fn push_shaped(&mut self, out: &mut Vec<RowGlyph>, s: &Seg, cell_w: f32, cell_h: f32) {
        let upem = match s.kind {
            FontKind::Cjk => self.fonts.cjk.as_ref().map(|f| f.units_per_em() as f32),
            _ => Some(self.fonts.fira.units_per_em() as f32),
        };
        let Some(upem) = upem else { return };
        let px = (cell_h * self.scale).round().max(4.0);
        let scale_px = px / upem;
        let glyphs = self.fonts.shape(s.kind, &s.text);
        let (font_id, swash_font) = match s.kind {
            FontKind::Cjk => (FONT_CJK, self.fonts.cjk_swash.as_ref()),
            _ => (FONT_FIRA, Some(&self.fonts.fira_swash)),
        };
        let Some(swash_font) = swash_font else { return };
        let y_base = self.fonts.baseline_ratio(s.kind) * cell_h;
        let mut x = s.start as f32 * cell_w;
        for (gid, advance) in glyphs {
            let w_px = advance as f32 * scale_px;
            if w_px <= 0.0 {
                continue;
            }
            // 真实位图尺寸 + placement 偏移（不拉伸，字形原样绘制）
            if let Some((gw, gh, lx, ty, rgba)) =
                raster_glyph(&mut self.raster, swash_font, gid, px)
            {
                if let Some(rect) = self
                    .atlas
                    .get_or_insert(font_id, gid, px as u32, gw, gh, &rgba)
                {
                    let uv = atlas::rect_to_uv(&rect, 2048.0);
                    out.push(RowGlyph {
                        x: x + lx as f32 / self.scale,
                        y: y_base + ty as f32 / self.scale,
                        w: gw as f32 / self.scale,
                        h: gh as f32 / self.scale,
                        uv,
                        color: to_rgba(s.fg),
                    });
                }
            }
            x += w_px / self.scale;
        }
        // 下划线
        if s.underline {
            out.push(RowGlyph {
                x: s.start as f32 * cell_w,
                y: cell_h - 1.5,
                w: s.text.chars().count() as f32 * cell_w,
                h: 1.5,
                uv: self.white_px,
                color: to_rgba(s.fg),
            });
        }
    }

    /// 彩色 emoji：swash 彩色位图直接入 atlas。
    fn push_emoji(&mut self, out: &mut Vec<RowGlyph>, s: &Seg, cell_w: f32, cell_h: f32) {
        let Some(swash_font) = &self.fonts.emoji_swash else {
            return;
        };
        let px = (cell_h * self.scale).round().max(8.0);
        let mut scaler = self.raster.builder(*swash_font).size(px).build();
        let mut x = s.start as f32 * cell_w;
        for ch in s.text.chars() {
            let gid = swash_font.charmap().map(ch);
            if gid == 0 {
                x += cell_w;
                continue;
            };
            if let Some(bitmap) = scaler.scale_color_bitmap(gid, swash::scale::StrikeWith::BestFit)
            {
                let (gw, gh) = (bitmap.placement.width, bitmap.placement.height);
                if gw > 0 && gh > 0 {
                    let rgba: Vec<u8> = bitmap
                        .data
                        .chunks(4)
                        .flat_map(|p| [p[0], p[1], p[2], p[3]])
                        .collect();
                    if let Some(rect) = self
                        .atlas
                        .get_or_insert(FONT_EMOJI, gid, px as u32, gw, gh, &rgba)
                    {
                        let uv = atlas::rect_to_uv(&rect, 2048.0);
                        // 按位图真实尺寸绘制（不拉伸），垂直按基线对齐
                        let y_base = cell_h * 0.78;
                        out.push(RowGlyph {
                            x: x + bitmap.placement.left as f32 / self.scale,
                            y: y_base + bitmap.placement.top as f32 / self.scale,
                            w: gw as f32 / self.scale,
                            h: gh as f32 / self.scale,
                            uv,
                            color: [1.0, 1.0, 1.0, 1.0],
                        });
                    }
                }
            }
            x += cell_w;
        }
    }
}

/// 创建终端渲染目标纹理。
fn create_target(
    device: &wgpu::Device,
    w: u32,
    h: u32,
) -> (wgpu::Texture, wgpu::TextureView, u32, u32) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ares-terminal-target"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view, w, h)
}

/// 行内容 hash（内容 + 颜色 + 选区 + 光标状态）。
fn row_hash(
    screen: &vt100::Screen,
    r: u16,
    cols: u16,
    selection: Option<&SelectRange>,
    cursor_style: &str,
    cursor_blink: bool,
    is_cursor_row: bool,
) -> u64 {
    let mut h = DefaultHasher::new();
    (r as u64).hash(&mut h);
    for c in 0..cols {
        if let Some(cell) = screen.cell(r, c) {
            cell.contents().hash(&mut h);
            let fg = cell.fgcolor();
            let bg = cell.bgcolor();
            format!("{fg:?}{bg:?}").hash(&mut h);
            cell.bold().hash(&mut h);
            cell.underline().hash(&mut h);
            cell.is_wide_continuation().hash(&mut h);
        } else {
            0u8.hash(&mut h);
        }
        selection.is_some_and(|s| s.contains(r, c)).hash(&mut h);
    }
    if is_cursor_row {
        cursor_style.hash(&mut h);
        if cursor_blink {
            let phase = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64 / 500 % 2)
                .unwrap_or(0);
            phase.hash(&mut h);
        }
        screen.cursor_position().1.hash(&mut h);
    }
    h.finish()
}

/// swash 灰度字形 → RGBA 位图（白字 + coverage alpha）+ 位图偏移。
fn raster_glyph(
    ctx: &mut swash::scale::ScaleContext,
    font: &swash::FontRef,
    glyph_id: u16,
    px: f32,
) -> Option<(u32, u32, i32, i32, Vec<u8>)> {
    let mut scaler = ctx.builder(*font).size(px).build();
    let render = swash::scale::Render::new(&[swash::scale::Source::Outline]);
    let image = render.render(&mut scaler, glyph_id)?;
    let (w, h) = (image.placement.width, image.placement.height);
    if w == 0 || h == 0 {
        return None;
    }
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for &cov in image.data.iter() {
        rgba.extend_from_slice(&[255, 255, 255, cov]);
    }
    Some((w, h, image.placement.left, image.placement.top, rgba))
}

/// 压入一个 quad 的 4 个顶点（NDC 转换）。
fn push_quad(verts: &mut Vec<Vert>, g: &RowGlyph, phys_w: u32, phys_h: u32, scale: f32) {
    // 逻辑像素 → 物理像素（对齐整数像素，消除字形模糊）→ NDC
    let x0 = (g.x * scale).round();
    let y0 = (g.y * scale).round();
    let w = (g.w * scale).round();
    let h = (g.h * scale).round();
    let nx0 = x0 / phys_w as f32 * 2.0 - 1.0;
    let nx1 = (x0 + w) / phys_w as f32 * 2.0 - 1.0;
    let ny0 = 1.0 - y0 / phys_h as f32 * 2.0;
    let ny1 = 1.0 - (y0 + h) / phys_h as f32 * 2.0;
    let (u0, v0, u1, v1) = (g.uv[0], g.uv[1], g.uv[2], g.uv[3]);
    verts.extend_from_slice(&[
        Vert {
            pos: [nx0, ny0],
            uv: [u0, v0],
            color: g.color,
        },
        Vert {
            pos: [nx1, ny0],
            uv: [u1, v0],
            color: g.color,
        },
        Vert {
            pos: [nx1, ny1],
            uv: [u1, v1],
            color: g.color,
        },
        Vert {
            pos: [nx0, ny1],
            uv: [u0, v1],
            color: g.color,
        },
    ]);
}

/// vt100 Color → egui Color32（与 term.rs 同逻辑）。
fn color_of(color: vt100::Color, theme: &Theme) -> Color32 {
    match color {
        vt100::Color::Default => theme.fg,
        vt100::Color::Idx(i) if i < 16 => theme.palette[i as usize],
        vt100::Color::Idx(i) => term_color_256(i),
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

/// 256 色标准映射（与 term.rs 一致）。
fn term_color_256(i: u8) -> Color32 {
    if i < 16 {
        [
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
        ][i as usize]
    } else if i < 232 {
        let n = i - 16;
        let r = n / 36;
        let g = (n % 36) / 6;
        let b = n % 6;
        let v = |x: u8| [0, 95, 135, 175, 215, 255][x as usize];
        Color32::from_rgb(v(r), v(g), v(b))
    } else {
        let v = 8 + (i - 232) * 10;
        Color32::from_rgb(v, v, v)
    }
}

/// 颜色提亮（粗体模拟）。
fn brighten(c: Color32, f: f32) -> Color32 {
    Color32::from_rgb(
        (c.r() as f32 * f).min(255.0) as u8,
        (c.g() as f32 * f).min(255.0) as u8,
        (c.b() as f32 * f).min(255.0) as u8,
    )
}

/// Color32 → RGBA 顶点色。
fn to_rgba(c: Color32) -> [f32; 4] {
    [
        c.r() as f32 / 255.0,
        c.g() as f32 / 255.0,
        c.b() as f32 / 255.0,
        c.a() as f32 / 255.0,
    ]
}
