//! Sparse tile storage for raster layers.
//!
//! Pixels live in fixed 64×64 tiles keyed by tile coordinate. Tiles that were
//! never painted take no memory, and tiles are reference counted so cloning a
//! store (for undo snapshots) shares data until one side writes to a tile.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

pub const TILE_SIZE: u32 = 64;
pub const TILE_PIXELS: usize = (TILE_SIZE * TILE_SIZE) as usize;
pub const TILE_BYTES: usize = TILE_PIXELS * 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileCoord {
    pub x: i32,
    pub y: i32,
}

impl TileCoord {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// Tile containing the pixel at `(px, py)`.
    pub fn containing(px: i32, py: i32) -> Self {
        let s = TILE_SIZE as i32;
        Self::new(px.div_euclid(s), py.div_euclid(s))
    }

    /// Pixel coordinate of the tile's top-left corner.
    pub fn origin(self) -> (i32, i32) {
        let s = TILE_SIZE as i32;
        (self.x * s, self.y * s)
    }
}

/// 64×64 premultiplied RGBA8 pixels, row-major.
#[derive(Clone, PartialEq, Eq)]
pub struct Tile {
    data: Box<[u8]>,
}

impl Tile {
    pub fn empty() -> Self {
        Self { data: vec![0; TILE_BYTES].into_boxed_slice() }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    fn index(x: u32, y: u32) -> usize {
        debug_assert!(x < TILE_SIZE && y < TILE_SIZE);
        ((y * TILE_SIZE + x) * 4) as usize
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = Self::index(x, y);
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }

    pub fn set_pixel(&mut self, x: u32, y: u32, px: [u8; 4]) {
        let i = Self::index(x, y);
        self.data[i..i + 4].copy_from_slice(&px);
    }

    /// Premultiplied storage means alpha 0 implies colour 0, so alpha alone decides.
    pub fn is_transparent(&self) -> bool {
        // The alpha bytes of two pixels read as one little-endian u64.
        const ALPHA: u64 = u64::from_le_bytes([0, 0, 0, 0xFF, 0, 0, 0, 0xFF]);
        // A row at a time: OR-ing its words has no branch (so it vectorises),
        // and the row test still stops early on the first painted row.
        self.data
            .as_chunks::<{ TILE_SIZE as usize * 4 }>()
            .0
            .iter()
            .all(|row| row.as_chunks::<8>().0.iter().fold(0, |acc, w| acc | u64::from_le_bytes(*w)) & ALPHA == 0)
    }
}

impl Default for Tile {
    fn default() -> Self {
        Self::empty()
    }
}

impl fmt::Debug for Tile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Tile(64x64)")
    }
}

#[derive(Clone, Default, Debug)]
pub struct TileStore {
    tiles: HashMap<TileCoord, Arc<Tile>>,
}

impl TileStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether both hold the very same tiles (shared, unchanged since one was
    /// copied from the other): a cheap "nothing was painted" test.
    pub fn same_tiles(&self, other: &TileStore) -> bool {
        self.tiles.len() == other.tiles.len()
            && self.tiles.iter().all(|(k, t)| other.tiles.get(k).is_some_and(|o| Arc::ptr_eq(t, o)))
    }

    pub fn get(&self, coord: TileCoord) -> Option<&Arc<Tile>> {
        self.tiles.get(&coord)
    }

    /// Mutable access to a tile, allocating it if absent. A tile still shared
    /// with a snapshot is copied first.
    pub fn get_or_create_mut(&mut self, coord: TileCoord) -> &mut Tile {
        Arc::make_mut(self.tiles.entry(coord).or_insert_with(|| Arc::new(Tile::empty())))
    }

    /// Stores `tile`, dropping the entry instead when the tile is fully transparent.
    pub fn insert(&mut self, coord: TileCoord, tile: Tile) {
        if tile.is_transparent() {
            self.tiles.remove(&coord);
        } else {
            self.tiles.insert(coord, Arc::new(tile));
        }
    }

    /// Stores an already shared tile (many coordinates may hold the same one;
    /// writes copy it first). Transparent tiles are dropped like in insert.
    pub fn insert_shared(&mut self, coord: TileCoord, tile: Arc<Tile>) {
        if tile.is_transparent() {
            self.tiles.remove(&coord);
        } else {
            self.tiles.insert(coord, tile);
        }
    }

    /// Stores `tile` as is, transparent or not (the history paging a step back in).
    pub(crate) fn put_shared(&mut self, coord: TileCoord, tile: Arc<Tile>) {
        self.tiles.insert(coord, tile);
    }

    /// Drops every fully transparent tile (left behind by in-place edits
    /// through [`TileStore::get_or_create_mut`] that erased all their pixels),
    /// so it takes neither RAM nor a GPU atlas slot. Returns the coordinates
    /// dropped, for the caller to free on the GPU.
    pub fn prune_transparent(&mut self) -> Vec<TileCoord> {
        self.prune_where(|_| true)
    }

    /// [`TileStore::prune_transparent`] limited to the tiles no clone of the
    /// store shares: those written since the last snapshot. A tile still
    /// shared was there when that snapshot was taken (and pruned then), so
    /// this skips scanning an unchanged layer.
    pub fn prune_unshared_transparent(&mut self) -> Vec<TileCoord> {
        self.prune_where(|t| Arc::strong_count(t) == 1)
    }

    fn prune_where(&mut self, check: impl Fn(&Arc<Tile>) -> bool) -> Vec<TileCoord> {
        let mut pruned = Vec::new();
        self.tiles.retain(|c, t| {
            let dead = check(t) && t.is_transparent();
            if dead {
                pruned.push(*c);
            }
            !dead
        });
        pruned
    }

    pub fn remove(&mut self, coord: TileCoord) -> Option<Arc<Tile>> {
        self.tiles.remove(&coord)
    }

    pub fn iter(&self) -> impl Iterator<Item = (TileCoord, &Arc<Tile>)> + '_ {
        self.tiles.iter().map(|(c, t)| (*c, t))
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// Deletes every pixel outside a `width` × `height` canvas: tiles wholly
    /// outside go, tiles on the edge are cleared past it. Returns whether
    /// anything was removed.
    pub fn clip_to_canvas(&mut self, width: u32, height: u32) -> bool {
        let (cols, rows) = (width.div_ceil(TILE_SIZE) as i32, height.div_ceil(TILE_SIZE) as i32);
        let before = self.tiles.len();
        self.tiles.retain(|c, _| c.x >= 0 && c.y >= 0 && c.x < cols && c.y < rows);
        let mut changed = self.tiles.len() != before;
        let edges: Vec<TileCoord> = self
            .tiles
            .keys()
            .copied()
            .filter(|c| (c.x as u32 + 1) * TILE_SIZE > width || (c.y as u32 + 1) * TILE_SIZE > height)
            .collect();
        for coord in edges {
            let (ox, oy) = coord.origin();
            let (keep_w, keep_h) = ((width as i32 - ox).min(TILE_SIZE as i32) as u32, (height as i32 - oy).min(TILE_SIZE as i32) as u32);
            let tile = &self.tiles[&coord];
            let dirty = (0..TILE_SIZE).any(|y| (0..TILE_SIZE).any(|x| (x >= keep_w || y >= keep_h) && tile.pixel(x, y)[3] != 0));
            if !dirty {
                continue;
            }
            changed = true;
            let tile = self.get_or_create_mut(coord);
            for y in 0..TILE_SIZE {
                for x in 0..TILE_SIZE {
                    if x >= keep_w || y >= keep_h {
                        tile.set_pixel(x, y, [0; 4]);
                    }
                }
            }
            if tile.is_transparent() {
                self.tiles.remove(&coord);
            }
        }
        changed
    }

    pub fn memory_bytes(&self) -> usize {
        self.tiles.len() * TILE_BYTES
    }

    pub fn pixel(&self, x: i32, y: i32) -> [u8; 4] {
        let coord = TileCoord::containing(x, y);
        let (ox, oy) = coord.origin();
        self.get(coord).map_or([0; 4], |t| t.pixel((x - ox) as u32, (y - oy) as u32))
    }

    /// Builds a store from a premultiplied RGBA8 image, skipping empty tiles.
    pub fn from_rgba(width: u32, height: u32, rgba: &[u8]) -> Self {
        assert_eq!(rgba.len(), (width * height * 4) as usize, "image buffer size mismatch");
        let mut store = Self::new();
        for ty in 0..height.div_ceil(TILE_SIZE) {
            for tx in 0..width.div_ceil(TILE_SIZE) {
                let x0 = tx * TILE_SIZE;
                let row_bytes = ((width - x0).min(TILE_SIZE) * 4) as usize;
                let mut tile = Tile::empty();
                for y in 0..TILE_SIZE {
                    let py = ty * TILE_SIZE + y;
                    if py >= height {
                        break;
                    }
                    let src = ((py * width + x0) * 4) as usize;
                    let dst = (y * TILE_SIZE * 4) as usize;
                    tile.bytes_mut()[dst..dst + row_bytes].copy_from_slice(&rgba[src..src + row_bytes]);
                }
                store.insert(TileCoord::new(tx as i32, ty as i32), tile);
            }
        }
        store
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coords_handle_negative_pixels() {
        assert_eq!(TileCoord::containing(-1, 0), TileCoord::new(-1, 0));
        assert_eq!(TileCoord::containing(64, 63), TileCoord::new(1, 0));
    }

    #[test]
    fn transparent_tiles_are_not_stored() {
        let mut store = TileStore::new();
        store.insert(TileCoord::new(0, 0), Tile::empty());
        assert!(store.is_empty());
        store.get_or_create_mut(TileCoord::new(0, 0)).set_pixel(3, 4, [10, 20, 30, 255]);
        assert_eq!(store.pixel(3, 4), [10, 20, 30, 255]);
    }

    #[test]
    fn transparency_reads_only_alpha() {
        let mut tile = Tile::empty();
        tile.bytes_mut().fill(0x7F);
        for i in (3..TILE_BYTES).step_by(4) {
            tile.bytes_mut()[i] = 0;
        }
        assert!(tile.is_transparent());
        for (x, y) in [(0, 0), (1, 0), (63, 17), (63, 63)] {
            let mut t = Tile::empty();
            t.set_pixel(x, y, [0, 0, 0, 1]);
            assert!(!t.is_transparent(), "alpha at ({x}, {y})");
        }
    }

    #[test]
    fn erased_tiles_are_pruned() {
        let mut store = TileStore::new();
        let tile = store.get_or_create_mut(TileCoord::new(2, -1));
        tile.set_pixel(5, 6, [40, 50, 60, 255]);
        tile.set_pixel(5, 6, [0; 4]);
        store.get_or_create_mut(TileCoord::new(0, 0)).set_pixel(0, 0, [1, 1, 1, 255]);
        assert_eq!(store.len(), 2);
        assert_eq!(store.prune_transparent(), vec![TileCoord::new(2, -1)]);
        assert_eq!(store.len(), 1);
        store.get_or_create_mut(TileCoord::new(0, 0)).set_pixel(0, 0, [0; 4]);
        store.prune_transparent();
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn pruning_unshared_skips_snapshot_tiles() {
        let mut store = TileStore::new();
        store.put_shared(TileCoord::new(0, 0), Arc::new(Tile::empty()));
        let snapshot = store.clone();
        store.get_or_create_mut(TileCoord::new(1, 0));
        assert_eq!(store.prune_unshared_transparent(), vec![TileCoord::new(1, 0)]);
        assert_eq!(store.len(), 1, "the tile the snapshot shares is left alone");
        drop(snapshot);
        assert_eq!(store.prune_unshared_transparent(), vec![TileCoord::new(0, 0)]);
        assert!(store.is_empty());
    }

    #[test]
    fn clones_share_until_written() {
        let mut a = TileStore::new();
        a.get_or_create_mut(TileCoord::new(0, 0)).set_pixel(0, 0, [1, 1, 1, 255]);
        let snapshot = a.clone();
        a.get_or_create_mut(TileCoord::new(0, 0)).set_pixel(0, 0, [2, 2, 2, 255]);
        assert_eq!(snapshot.pixel(0, 0), [1, 1, 1, 255]);
        assert_eq!(a.pixel(0, 0), [2, 2, 2, 255]);
    }

    #[test]
    fn from_rgba_splits_into_tiles() {
        let (w, h) = (70, 3);
        let mut img = vec![0u8; (w * h * 4) as usize];
        let i = ((2 * w + 69) * 4) as usize;
        img[i..i + 4].copy_from_slice(&[9, 8, 7, 255]);
        let store = TileStore::from_rgba(w, h, &img);
        assert_eq!(store.len(), 1);
        assert_eq!(store.pixel(69, 2), [9, 8, 7, 255]);
    }

    #[test]
    fn clipping_to_the_canvas_deletes_what_is_outside() {
        // 150 × 100 of paint on a 100 × 70 canvas.
        let rgba: Vec<u8> = [10, 20, 30, 255].repeat(150 * 100);
        let mut store = TileStore::from_rgba(150, 100, &rgba);
        assert!(store.clip_to_canvas(100, 70));
        assert_eq!(store.pixel(99, 69), [10, 20, 30, 255]);
        assert_eq!(store.pixel(100, 10)[3], 0);
        assert_eq!(store.pixel(10, 70)[3], 0);
        assert!(store.iter().all(|(c, _)| c.x < 2 && c.y < 2), "tiles past the canvas are gone");
        assert!(!store.clip_to_canvas(100, 70), "nothing left to remove");
        let mut neg = TileStore::new();
        neg.get_or_create_mut(TileCoord::new(-1, 0)).set_pixel(0, 0, [1, 1, 1, 255]);
        assert!(neg.clip_to_canvas(100, 70) && neg.is_empty());
    }
}
