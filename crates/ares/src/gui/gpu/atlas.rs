//! 字形 atlas（etagere 打包 + wgpu 纹理，dirty 区批量上传）。
//!
//! 所有字体的 glyph 位图（含彩色 emoji）共享一张 RGBA 纹理；
//! 渲染前调用 `upload` 把新栅格化的区域写进 GPU。

use etagere::{AllocId, AtlasAllocator, Rectangle as ERect, Size as ESize};
use std::collections::HashMap;

pub struct GlyphAtlas {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
    allocator: AtlasAllocator,
    /// (font_id, glyph_id, px) → 打包分配（px=栅格化字号，不同字号独立缓存，
    /// 避免窗口 resize 后旧尺寸位图被拉伸导致字形扭曲）
    cache: HashMap<(u32, u16, u32), AllocId>,
    /// 待上传：x, y, w, h, rgba
    dirty: Vec<(u32, u32, u32, u32, Vec<u8>)>,
}

impl GlyphAtlas {
    pub fn new(device: &wgpu::Device, size: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ares-glyph-atlas"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ares-glyph-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            texture,
            view,
            sampler,
            allocator: AtlasAllocator::new(ESize::new(size as i32, size as i32)),
            cache: HashMap::new(),
            dirty: Vec::new(),
        }
    }

    /// 获取或插入 glyph 位图；返回其在 atlas 中的像素矩形。
    pub fn get_or_insert(
        &mut self,
        font_id: u32,
        glyph_id: u16,
        px: u32,
        w: u32,
        h: u32,
        rgba: &[u8],
    ) -> Option<ERect> {
        let key = (font_id, glyph_id, px);
        if let Some(id) = self.cache.get(&key) {
            return Some(self.allocator.get(*id));
        }
        if w == 0 || h == 0 || rgba.is_empty() {
            return None;
        }
        let alloc = self.allocator.allocate(ESize::new(w as i32, h as i32))?;
        let rect = alloc.rectangle;
        self.cache.insert(key, alloc.id);
        self.dirty
            .push((rect.min.x as u32, rect.min.y as u32, w, h, rgba.to_vec()));
        Some(rect)
    }

    /// 上传所有新 glyph 到 GPU。
    pub fn upload(&mut self, queue: &wgpu::Queue) {
        for (x, y, w, h, data) in self.dirty.drain(..) {
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &data,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 4),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
        }
    }
}

/// 坐标工具：把 atlas 矩形转 UV。
pub fn rect_to_uv(rect: &ERect, atlas_size: f32) -> [f32; 4] {
    [
        rect.min.x as f32 / atlas_size,
        rect.min.y as f32 / atlas_size,
        (rect.min.x + rect.width()) as f32 / atlas_size,
        (rect.min.y + rect.height()) as f32 / atlas_size,
    ]
}
