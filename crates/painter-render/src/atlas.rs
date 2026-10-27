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

/// Page edge of the standard atlas profile.
pub(crate) const PAGE_SIZE: u32 = 2048;
/// Page edge of the compact (low-VRAM) atlas profile.
pub(crate) const COMPACT_PAGE_SIZE: u32 = 1024;
/// Pages the atlas starts with. Two, not one: OpenGL (wgpu's GL backend, used
/// on Android's Mali GPUs) makes a one-layer texture a plain 2D texture, and
/// shaders reading it as an array (`texture_2d_array`) then read nothing.
/// Also the floor `shrink` never goes below.
const FIRST_PAGES: u32 = 2;
/// `shrink` gives pages back once fewer than this percentage of slots is used.
const SHRINK_BELOW_PERCENT: u64 = 35;

/// How big the atlas pages are, i.e. the VRAM step it grows and shrinks by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AtlasProfile {
    /// 2048² pages: 16 MiB each, 32 MiB to start.
    Standard,
    /// 1024² pages for integrated or legacy GPUs: 4 MiB each, 8 MiB to start.
    Compact,
}

impl AtlasProfile {
    /// `PAINTER_ATLAS=compact|standard` when set, otherwise compact when
    /// `compact` asks for it (see [`crate::RenderProfile::compact_atlas`]) or on
    /// devices limited to downlevel texture sizes (2D textures under 4096 px).
    pub fn choose(device: &wgpu::Device, compact: bool) -> Self {
        match std::env::var("PAINTER_ATLAS").ok().as_deref().map(str::trim) {
            Some(v) if v.eq_ignore_ascii_case("compact") => return Self::Compact,
            Some(v) if v.eq_ignore_ascii_case("standard") => return Self::Standard,
            _ => {}
        }
        if compact || device.limits().max_texture_dimension_2d < 2 * PAGE_SIZE {
            Self::Compact
        } else {
            Self::Standard
        }
    }

    /// Page edge in pixels.
    pub fn page_size(self) -> u32 {
        match self {
            Self::Standard => PAGE_SIZE,
            Self::Compact => COMPACT_PAGE_SIZE,
        }
    }
}

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
    /// Page edge in pixels (see [`AtlasProfile`]).
    page_size: u32,
    pages: u32,
    free: Vec<u32>,
    next: u32,
    /// Shared slots of uniform tiles: value → (slot, references), and back.
    shared: HashMap<[u8; 4], (u32, u32)>,
    shared_slots: HashMap<u32, [u8; 4]>,
}

impl Atlas {
    /// An atlas with `profile`'s page size, starting at [`FIRST_PAGES`] pages.
    pub fn with_profile(device: &wgpu::Device, profile: AtlasProfile) -> Self {
        let page_size = profile.page_size();
        let (texture, view) = Self::create(device, page_size, FIRST_PAGES);
        Self {
            texture,
            view,
            page_size,
            pages: FIRST_PAGES,
            free: Vec::new(),
            next: 0,
            shared: HashMap::new(),
            shared_slots: HashMap::new(),
        }
    }

    fn create(device: &wgpu::Device, page_size: u32, pages: u32) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tile atlas"),
            size: wgpu::Extent3d { width: page_size, height: page_size, depth_or_array_layers: pages },
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

    fn tiles_per_row(&self) -> u32 {
        self.page_size / TILE_SIZE
    }

    fn tiles_per_page(&self) -> u32 {
        self.tiles_per_row() * self.tiles_per_row()
    }

    /// Bytes of GPU memory the atlas occupies.
    pub fn memory_bytes(&self) -> u64 {
        self.pages as u64 * (self.page_size * self.page_size * 4) as u64
    }

    pub fn used_slots(&self) -> u32 {
        self.next - self.free.len() as u32
    }

    fn alloc(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> u32 {
        if let Some(slot) = self.free.pop() {
            return slot;
        }
        if self.next == self.pages * self.tiles_per_page() {
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
        let (texture, view) = Self::create(device, self.page_size, pages);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("atlas grow") });
        encoder.copy_texture_to_texture(
            self.texture.as_image_copy(),
            texture.as_image_copy(),
            wgpu::Extent3d { width: self.page_size, height: self.page_size, depth_or_array_layers: self.pages },
        );
        queue.submit([encoder.finish()]);
        self.texture = texture;
        self.view = view;
        self.pages = pages;
    }

    /// Page count `shrink` settles on: halved while fewer than
    /// [`SHRINK_BELOW_PERCENT`] of the slots are used, never below [`FIRST_PAGES`].
    fn shrink_target(&self) -> u32 {
        let used = self.used_slots() as u64;
        let mut pages = self.pages;
        while pages > FIRST_PAGES && used * 100 < SHRINK_BELOW_PERCENT * pages as u64 * self.tiles_per_page() as u64 {
            pages = (pages / 2).max(FIRST_PAGES);
        }
        pages
    }

    /// Gives VRAM back after many slots were released: while fewer than 35% of
    /// the slots are in use, halves the page count (down to [`FIRST_PAGES`]),
    /// packing the live slots to the front of a new texture with
    /// `copy_texture_to_texture`.
    ///
    /// Live slots below the used count stay put; the others move into the
    /// holes. Returns the moves as `old slot → new slot` (possibly empty), or
    /// `None` when the atlas kept its size. Callers must repoint every
    /// reference to a moved slot. As with `grow`, the copies are submitted
    /// right away so tile writes queued earlier land before them.
    pub fn shrink(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Option<HashMap<u32, u32>> {
        let pages = self.shrink_target();
        if pages >= self.pages {
            return None;
        }
        let used = self.used_slots();
        let mut dead = vec![false; self.next as usize];
        for &slot in &self.free {
            dead[slot as usize] = true;
        }
        let mut holes = (0..used).filter(|&s| dead[s as usize]);
        let moves: HashMap<u32, u32> = (used..self.next)
            .filter(|&s| !dead[s as usize])
            .map(|s| (s, holes.next().expect("as many holes below the used count as live slots above it")))
            .collect();

        let (texture, view) = Self::create(device, self.page_size, pages);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("atlas shrink") });
        // Pages holding slots below the used count keep their place.
        let kept = used.div_ceil(self.tiles_per_page());
        if kept > 0 {
            encoder.copy_texture_to_texture(
                self.texture.as_image_copy(),
                texture.as_image_copy(),
                wgpu::Extent3d { width: self.page_size, height: self.page_size, depth_or_array_layers: kept },
            );
        }
        for (&from, &to) in &moves {
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: self.origin(from),
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: self.origin(to), aspect: wgpu::TextureAspect::All },
                wgpu::Extent3d { width: TILE_SIZE, height: TILE_SIZE, depth_or_array_layers: 1 },
            );
        }
        queue.submit([encoder.finish()]);

        for entry in self.shared.values_mut() {
            if let Some(&to) = moves.get(&entry.0) {
                entry.0 = to;
            }
        }
        self.shared_slots = self.shared.iter().map(|(&value, &(slot, _))| (slot, value)).collect();
        self.texture = texture;
        self.view = view;
        self.pages = pages;
        self.next = used;
        self.free.clear();
        Some(moves)
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

    /// Top-left texel of `slot` in the texture array.
    fn origin(&self, slot: u32) -> wgpu::Origin3d {
        let (row, page) = (self.tiles_per_row(), self.tiles_per_page());
        let i = slot % page;
        wgpu::Origin3d { x: (i % row) * TILE_SIZE, y: (i / row) * TILE_SIZE, z: slot / page }
    }

    fn write(&self, queue: &wgpu::Queue, slot: u32, bytes: &[u8]) {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: self.origin(slot),
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
    /// An empty residency whose atlas has `profile`'s page size.
    pub fn new(device: &wgpu::Device, profile: AtlasProfile) -> Self {
        Self { atlas: Atlas::with_profile(device, profile), planes: HashMap::new(), grid: (0, 0) }
    }

    /// Drops everything for a new canvas size, with a fresh atlas of `profile`.
    pub fn reset(&mut self, device: &wgpu::Device, canvas: (u32, u32), profile: AtlasProfile) {
        self.atlas = Atlas::with_profile(device, profile);
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

    /// Compacts the atlas when most of it went unused (see [`Atlas::shrink`])
    /// and repoints every plane at the moved slots. Returns whether it shrank.
    pub fn shrink(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> bool {
        let Some(moves) = self.atlas.shrink(device, queue) else {
            return false;
        };
        for tiles in self.planes.values_mut() {
            for s in tiles.slots.iter_mut().filter(|s| **s != 0) {
                if let Some(&to) = moves.get(&(*s - 1)) {
                    *s = to + 1;
                    tiles.dirty = true;
                }
            }
        }
        true
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

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use super::*;

    /// One device for every atlas test, used by one test at a time.
    fn gpu() -> Option<(&'static wgpu::Device, &'static wgpu::Queue, MutexGuard<'static, ()>)> {
        static DEVICE: OnceLock<Option<(wgpu::Device, wgpu::Queue)>> = OnceLock::new();
        static TURN: Mutex<()> = Mutex::new(());
        let (device, queue) = DEVICE
            .get_or_init(|| {
                let backends = wgpu::Backends::from_env().unwrap_or(wgpu::Backends::all());
                let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends, ..Default::default() });
                let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))?;
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None)).ok()
            })
            .as_ref()?;
        let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        Some((device, queue, turn))
    }

    fn residency(device: &wgpu::Device, profile: AtlasProfile, grid: (u32, u32)) -> Residency {
        Residency { atlas: Atlas::with_profile(device, profile), planes: HashMap::new(), grid }
    }

    /// A non-uniform tile unique to `seed`.
    fn tile_bytes(seed: u32) -> Vec<u8> {
        (0..TILE_SIZE * TILE_SIZE)
            .flat_map(|i| {
                let v = seed.wrapping_mul(2_654_435_761).wrapping_add(i * 7);
                [v as u8, (v >> 8) as u8, (v >> 16) as u8, 255]
            })
            .collect()
    }

    /// The bytes of one atlas slot, read back from the GPU.
    fn read_slot(device: &wgpu::Device, queue: &wgpu::Queue, atlas: &Atlas, slot: u32) -> Vec<u8> {
        let row = TILE_SIZE * 4;
        let padded = row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("atlas slot readback"),
            size: (padded * TILE_SIZE) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &atlas.texture,
                mip_level: 0,
                origin: atlas.origin(slot),
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(TILE_SIZE) },
            },
            wgpu::Extent3d { width: TILE_SIZE, height: TILE_SIZE, depth_or_array_layers: 1 },
        );
        queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |result| result.expect("failed to map readback buffer"));
        device.poll(wgpu::Maintain::Wait);
        let mapped = slice.get_mapped_range();
        (0..TILE_SIZE as usize).flat_map(|y| mapped[y * padded as usize..][..row as usize].to_vec()).collect()
    }

    #[test]
    fn compact_profile_starts_at_8_mib() {
        let Some((device, _queue, _turn)) = gpu() else { return };
        assert_eq!(Atlas::with_profile(device, AtlasProfile::Compact).memory_bytes(), 8 << 20);
        assert_eq!(Atlas::with_profile(device, AtlasProfile::Standard).memory_bytes(), 32 << 20);
    }

    #[test]
    fn shrink_compacts_live_slots_and_frees_pages() {
        let Some((device, queue, _turn)) = gpu() else { return };
        // Compact pages hold 256 tiles: 1600 tiles grow the atlas to 8 pages.
        let grid = (40, 40);
        let mut res = residency(device, AtlasProfile::Compact, grid);
        let (doomed, kept) = (Plane::Color(LayerId(1)), Plane::Color(LayerId(2)));
        let uniform = [9u8, 8, 7, 255].repeat((TILE_SIZE * TILE_SIZE) as usize);
        let mut expected = HashMap::new();
        for y in 0..grid.1 as i32 {
            for x in 0..grid.0 as i32 {
                let n = (y * grid.0 as i32 + x) as u32;
                let coord = TileCoord::new(x, y);
                // Every fifth tile survives on the kept plane, a few of them uniform.
                let plane = if n.is_multiple_of(5) { kept } else { doomed };
                let bytes = if plane == kept && n.is_multiple_of(35) { uniform.clone() } else { tile_bytes(n) };
                res.write_tile(device, queue, plane, coord, Some(&bytes));
                if plane == kept {
                    expected.insert(coord, bytes);
                }
            }
        }
        res.flush(queue);
        assert_eq!(res.atlas.pages, 8);
        let grown = res.atlas.memory_bytes();
        assert!(!res.shrink(device, queue), "a full atlas keeps its pages");

        // Drop 80% of the tiles.
        res.retain(|p| *p == kept);
        let used = res.atlas.used_slots();
        assert!(res.shrink(device, queue));
        res.flush(queue);
        assert_eq!(res.atlas.pages, FIRST_PAGES);
        assert!(res.atlas.memory_bytes() < grown);
        assert_eq!(res.atlas.used_slots(), used);
        assert!(!res.shrink(device, queue), "already compact");

        let read_all = |res: &Residency| {
            let tiles = res.get(kept).expect("kept plane");
            for (coord, bytes) in &expected {
                let s = tiles.slots[(coord.y as u32 * grid.0 + coord.x as u32) as usize];
                assert!(s != 0 && s - 1 < res.atlas.used_slots(), "{coord:?} points past the live slots");
                assert!(read_slot(device, queue, &res.atlas, s - 1) == *bytes, "{coord:?} lost its pixels");
            }
        };
        read_all(&res);

        // New tiles allocate after the compacted slots and leave old ones alone.
        let fresh = tile_bytes(9999);
        res.write_tile(device, queue, Plane::Selection, TileCoord::new(0, 0), Some(&fresh));
        res.write_tile(device, queue, Plane::Selection, TileCoord::new(1, 0), Some(&uniform));
        assert_eq!(res.atlas.used_slots(), used + 1, "the uniform tile reuses the shared slot");
        read_all(&res);
        assert!(read_slot(device, queue, &res.atlas, used) == fresh);
    }
}
