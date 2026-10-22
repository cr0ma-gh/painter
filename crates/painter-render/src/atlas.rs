//! Sparse GPU residency: every painted 64×64 tile of every plane (layer
//! colour, layer mask, selection) lives in one shared texture-array atlas.
//! Each plane has a small `R32Uint` indirection texture with one texel per
//! canvas tile: `0` = empty, otherwise `slot + 1`. GPU memory follows the
//! painted area, not the number of layers.
//!
//! Uniform tiles (every pixel the same, e.g. the fully hidden outside of a
//! comic panel's mask) share one slot per value, reference-counted, so a
//! canvas-sized mask costs one slot instead of thousands.
//!
//! Shaders read planes through `atlas_load` in `shaders/atlas.wgsl`.

use std::collections::HashMap;

use painter_core::{LayerId, TileCoord, TILE_SIZE};

use crate::gpu::CANVAS_FORMAT;

pub(crate) const PAGE_SIZE: u32 = 2048;
/// Pages the atlas starts with. Two, not one: OpenGL (wgpu's GL backend, used
/// on Android's Mali GPUs) makes a one-layer texture a plain 2D texture, and
/// shaders reading it as an array (`texture_2d_array`) then read nothing.
const FIRST_PAGES: u32 = 2;
const TILES_PER_ROW: u32 = PAGE_SIZE / TILE_SIZE;
const TILES_PER_PAGE: u32 = TILES_PER_ROW * TILES_PER_ROW;

/// A tiled image the GPU mirrors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Plane {
    /// Raster pixels or a vector layer's cache.
    Color(LayerId),
    /// Hidden amount of a layer mask.
    Mask(LayerId),
    /// The document selection.
    Selection,
}

pub(crate) struct Atlas {
    texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pages: u32,
    free: Vec<u32>,
    next: u32,
    /// Shared slots of uniform tiles: value → (slot, references), and back.
    shared: HashMap<[u8; 4], (u32, u32)>,
    shared_slots: HashMap<u32, [u8; 4]>,
}

impl Atlas {
    pub fn new(device: &wgpu::Device) -> Self {
        let (texture, view) = Self::create(device, FIRST_PAGES);
        Self { texture, view, pages: FIRST_PAGES, free: Vec::new(), next: 0, shared: HashMap::new(), shared_slots: HashMap::new() }
    }

    fn create(device: &wgpu::Device, pages: u32) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tile atlas"),
            size: wgpu::Extent3d { width: PAGE_SIZE, height: PAGE_SIZE, depth_or_array_layers: pages },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: CANVAS_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        (texture, view)
    }

    /// Bytes of GPU memory the atlas occupies.
    pub fn memory_bytes(&self) -> u64 {
        self.pages as u64 * (PAGE_SIZE * PAGE_SIZE * 4) as u64
    }

    pub fn used_slots(&self) -> u32 {
        self.next - self.free.len() as u32
    }

    fn alloc(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> u32 {
        if let Some(slot) = self.free.pop() {
            return slot;
        }
        if self.next == self.pages * TILES_PER_PAGE {
            self.grow(device, queue);
        }
        self.next += 1;
        self.next - 1
    }

    /// Doubles the page count, copying existing pages. The copy is submitted
    /// right away so it lands after this frame's earlier tile writes and
    /// before later ones (queue writes apply at the next submit).
    fn grow(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let pages = (self.pages * 2).min(device.limits().max_texture_array_layers);
        assert!(pages > self.pages, "tile atlas is full ({} pages)", self.pages);
        let (texture, view) = Self::create(device, pages);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("atlas grow") });
        encoder.copy_texture_to_texture(
            self.texture.as_image_copy(),
            texture.as_image_copy(),
            wgpu::Extent3d { width: PAGE_SIZE, height: PAGE_SIZE, depth_or_array_layers: self.pages },
        );
        queue.submit([encoder.finish()]);
        self.texture = texture;
        self.view = view;
        self.pages = pages;
    }

    /// Gives a slot back: a shared one only when its last user lets go.
    fn release(&mut self, slot: u32) {
        if let Some(value) = self.shared_slots.get(&slot).copied() {
            let entry = self.shared.get_mut(&value).expect("shared slot without an entry");
            entry.1 -= 1;
            if entry.1 > 0 {
                return;
            }
            self.shared.remove(&value);
            self.shared_slots.remove(&slot);
        }
        self.free.push(slot);
    }

    fn is_shared(&self, slot: u32) -> bool {
        self.shared_slots.contains_key(&slot)
    }

    /// The shared slot for a uniform tile of alue, written on first use.
    fn acquire_shared(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, value: [u8; 4], bytes: &[u8]) -> u32 {
        if let Some(entry) = self.shared.get_mut(&value) {
            entry.1 += 1;
            return entry.0;
        }
        let slot = self.alloc(device, queue);
        self.write(queue, slot, bytes);
        self.shared.insert(value, (slot, 1));
        self.shared_slots.insert(slot, value);
        slot
    }

    fn write(&self, queue: &wgpu::Queue, slot: u32, bytes: &[u8]) {
        let page = slot / TILES_PER_PAGE;
        let i = slot % TILES_PER_PAGE;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: (i % TILES_PER_ROW) * TILE_SIZE, y: (i / TILES_PER_ROW) * TILE_SIZE, z: page },
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(TILE_SIZE * 4), rows_per_image: Some(TILE_SIZE) },
            wgpu::Extent3d { width: TILE_SIZE, height: TILE_SIZE, depth_or_array_layers: 1 },
        );
    }
}

/// One plane's indirection table, mirrored on the CPU and uploaded when dirty.
pub(crate) struct PlaneTiles {
    texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    slots: Vec<u32>,
    dirty: bool,
}

/// Every plane the GPU knows about, backed by one atlas.
pub(crate) struct Residency {
    pub atlas: Atlas,
    planes: HashMap<Plane, PlaneTiles>,
    /// Canvas size in tiles.
    grid: (u32, u32),
}

impl Residency {
    pub fn new(device: &wgpu::Device) -> Self {
        Self { atlas: Atlas::new(device), planes: HashMap::new(), grid: (0, 0) }
    }

    /// Drops everything for a new canvas size.
    pub fn reset(&mut self, device: &wgpu::Device, canvas: (u32, u32)) {
        self.atlas = Atlas::new(device);
        self.planes.clear();
        self.grid = (canvas.0.div_ceil(TILE_SIZE), canvas.1.div_ceil(TILE_SIZE));
    }

    pub fn get(&self, plane: Plane) -> Option<&PlaneTiles> {
        self.planes.get(&plane)
    }

    /// Frees planes not in `keep`.
    pub fn retain(&mut self, keep: impl Fn(&Plane) -> bool) {
        let atlas = &mut self.atlas;
        self.planes.retain(|plane, tiles| {
            let kept = keep(plane);
            if !kept {
                for &s in tiles.slots.iter().filter(|&&s| s != 0) {
                    atlas.release(s - 1);
                }
            }
            kept
        });
    }

    /// Removes a plane's tiles (it reads as empty afterwards).
    pub fn clear(&mut self, plane: Plane) {
        if let Some(tiles) = self.planes.get_mut(&plane) {
            for &s in tiles.slots.iter().filter(|&&s| s != 0) {
                self.atlas.release(s - 1);
            }
            tiles.slots.iter_mut().for_each(|s| *s = 0);
            tiles.dirty = true;
        }
    }

    /// Stores (or with `None` drops) one tile of a plane.
    pub fn write_tile(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, plane: Plane, coord: TileCoord, bytes: Option<&[u8]>) {
        let (gw, gh) = self.grid;
        if coord.x < 0 || coord.y < 0 || coord.x as u32 >= gw || coord.y as u32 >= gh {
            return;
        }
        let index = (coord.y as u32 * gw + coord.x as u32) as usize;
        let tiles = self.planes.entry(plane).or_insert_with(|| PlaneTiles::new(device, (gw, gh)));
        let current = tiles.slots[index];
        let uniform = bytes.and_then(|b| {
            let first: [u8; 4] = b[..4].try_into().ok()?;
            b.as_chunks::<4>().0.iter().all(|p| *p == first).then_some(first)
        });
        match (bytes, uniform) {
            (Some(bytes), Some(value)) => {
                // Already pointing at this value's shared slot: nothing to do.
                if current != 0 && self.atlas.shared.get(&value).is_some_and(|(slot, _)| *slot == current - 1) {
                    return;
                }
                if current != 0 {
                    self.atlas.release(current - 1);
                }
                let slot = self.atlas.acquire_shared(device, queue, value, bytes);
                tiles.slots[index] = slot + 1;
                tiles.dirty = true;
            }
            (Some(bytes), None) => {
                // A shared slot can't be overwritten in place.
                let own = current != 0 && !self.atlas.is_shared(current - 1);
                if current != 0 && !own {
                    self.atlas.release(current - 1);
                }
                let slot = if own { current - 1 } else { self.atlas.alloc(device, queue) };
                self.atlas.write(queue, slot, bytes);
                if !own {
                    tiles.slots[index] = slot + 1;
                    tiles.dirty = true;
                }
            }
            (None, _) if current != 0 => {
                self.atlas.release(current - 1);
                tiles.slots[index] = 0;
                tiles.dirty = true;
            }
            (None, _) => {}
        }
    }
    /// Uploads changed indirection tables.
    pub fn flush(&mut self, queue: &wgpu::Queue) {
        let (gw, gh) = self.grid;
        for tiles in self.planes.values_mut().filter(|t| t.dirty) {
            queue.write_texture(
                tiles.texture.as_image_copy(),
                bytemuck::cast_slice(&tiles.slots),
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(gw * 4), rows_per_image: Some(gh) },
                wgpu::Extent3d { width: gw, height: gh, depth_or_array_layers: 1 },
            );
            tiles.dirty = false;
        }
    }
}

impl PlaneTiles {
    fn new(device: &wgpu::Device, (width, height): (u32, u32)) -> Self {
        let texture = index_texture(device, (width, height), "plane index");
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { texture, view, slots: vec![0; (width * height) as usize], dirty: true }
    }
}

pub(crate) fn index_texture(device: &wgpu::Device, (width, height): (u32, u32), label: &str) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: width.max(1), height: height.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Uint,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}
