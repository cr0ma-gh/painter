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
        self.data.as_chunks::<4>().0.iter().all(|p| p[3] == 0)
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
}
