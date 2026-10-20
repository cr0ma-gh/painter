//! GPU texture assets for brushes: tip bitmaps, dual tips and paper textures,
//! uploaded once as `R8Unorm` textures with their full mip chain and reused
//! across strokes. Least-recently-used entries are evicted past a memory budget.
//!
//! Shaders read them with `textureLoad` and do their own bilinear filtering
//! (clamped for tips, wrapped for textures), so the GPU matches the CPU
//! rasteriser exactly; mips stand in for minification filtering.

use std::sync::Arc;

use painter_core::TipBitmap;

/// Upper bound on cached texture bytes (all mips).
const BUDGET_BYTES: u64 = 256 * 1024 * 1024;

struct Entry {
    bitmap: Arc<TipBitmap>,
    view: wgpu::TextureView,
    bytes: u64,
    last_used: u64,
}

pub(crate) struct TextureAssets {
    entries: Vec<Entry>,
    clock: u64,
    uploads: u64,
    /// 1×1 transparent texture bound where a slot is unused.
    pub empty: wgpu::TextureView,
}

fn create(device: &wgpu::Device, (width, height): (u32, u32), mip_level_count: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("brush asset"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

impl TextureAssets {
    pub fn new(device: &wgpu::Device) -> Self {
        let empty = create(device, (1, 1), 1).create_view(&wgpu::TextureViewDescriptor::default());
        Self { entries: Vec::new(), clock: 0, uploads: 0, empty }
    }

    /// Makes sure `bitmap` is resident, uploading it (and evicting old entries) if needed.
    pub fn ensure(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, bitmap: &Arc<TipBitmap>) {
        self.clock += 1;
        if let Some(entry) = self.entries.iter_mut().find(|e| Arc::ptr_eq(&e.bitmap, bitmap)) {
            entry.last_used = self.clock;
            return;
        }
        let texture = create(device, (bitmap.width(), bitmap.height()), bitmap.level_count());
        let mut bytes = 0;
        for level in 0..bitmap.level_count() {
            let (width, height) = bitmap.level_size(level);
            queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: level, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                bitmap.level(level),
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width), rows_per_image: Some(height) },
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
            bytes += (width * height) as u64;
        }
        self.uploads += 1;
        self.entries.push(Entry {
            bitmap: bitmap.clone(),
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            bytes,
            last_used: self.clock,
        });
        // Evict least recently used, but never what was just requested.
        while self.resident_bytes() > BUDGET_BYTES && self.entries.len() > 1 {
            let oldest = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.last_used != self.clock)
                .min_by_key(|(_, e)| e.last_used)
                .map(|(i, _)| i);
            match oldest {
                Some(i) => {
                    self.entries.swap_remove(i);
                }
                None => break,
            }
        }
    }

    /// The view of a resident bitmap, or the empty texture.
    pub fn view(&self, bitmap: Option<&Arc<TipBitmap>>) -> &wgpu::TextureView {
        bitmap
            .and_then(|b| self.entries.iter().find(|e| Arc::ptr_eq(&e.bitmap, b)))
            .map_or(&self.empty, |e| &e.view)
    }

    pub fn resident_bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.bytes).sum()
    }

    /// (resident textures, resident bytes, uploads so far).
    pub fn stats(&self) -> (usize, u64, u64) {
        (self.entries.len(), self.resident_bytes(), self.uploads)
    }
}
