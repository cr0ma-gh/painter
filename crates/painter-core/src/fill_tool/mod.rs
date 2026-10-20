//! Fill bucket: which pixels a click fills, and that area as a stroke.
//!
//! The pipeline works on canvas-sized byte masks ([`Bitmap`]), so every step
//! is a linear pass that runs in tens of milliseconds even on 600 dpi pages:
//!
//! 1. [`reference_mask`]: the pixels the fill may cover ("open") are those
//!    of the reference image (the edited layer, all visible layers, or the
//!    layers marked as reference) close enough to the clicked colour, and
//!    inside the selection.
//! 2. [`gap_detection`]: small gaps in the line art are bridged in a working
//!    copy of the boundary (a morphological closing); the line art itself is
//!    never touched.
//! 3. [`flood_fill`]: a scanline flood from the click over the open pixels,
//!    then the fill grows into the bridges and corners it is entitled to,
//!    and finally expands under the line art (or shrinks) by a few pixels so
//!    no white halo is left between ink and colour.

pub mod flood_fill;
pub mod gap_detection;
pub mod reference_mask;

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::raster::{RasterizedStroke, StrokeBuffer};
use crate::tile::{Tile, TileCoord, TileStore, TILE_SIZE};

pub use flood_fill::{fill, Fill};
pub use gap_detection::{close_gaps, GapPreset};
pub use reference_mask::{
    open_mask_from_pixels, open_mask_from_tiles, reference_pixels, restrict_to_selection, ReferenceOptions, ReferenceSource,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FillOptions {
    /// Maximum per-channel difference from the clicked colour, 0..=255.
    pub tolerance: u8,
    /// Only the connected area around the click, or every matching pixel.
    pub contiguous: bool,
    /// Pixels to grow the fill under the surrounding line art (anti-halo);
    /// negative values pull the fill back from the lines instead.
    pub expand: i32,
    /// Close gaps in the line art up to this many pixels wide (0 = off).
    pub gap: u32,
}

impl Default for FillOptions {
    fn default() -> Self {
        Self { tolerance: 16, contiguous: true, expand: 1, gap: 0 }
    }
}

/// A canvas-sized mask, one byte per pixel (0 or 1), rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bitmap {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl Bitmap {
    pub fn new(width: usize, height: usize) -> Self {
        Self { width, height, data: vec![0; width * height] }
    }

    pub fn filled(width: usize, height: usize) -> Self {
        Self { width, height, data: vec![1; width * height] }
    }

    pub fn get(&self, x: usize, y: usize) -> bool {
        self.data[y * self.width + x] != 0
    }

    pub fn set(&mut self, x: usize, y: usize, on: bool) {
        self.data[y * self.width + x] = on as u8;
    }

    pub fn count(&self) -> usize {
        self.data.iter().filter(|&&v| v != 0).count()
    }

    pub fn inverted(&self) -> Bitmap {
        Bitmap { width: self.width, height: self.height, data: self.data.iter().map(|&v| (v == 0) as u8).collect() }
    }

    /// `self & other`, in place.
    pub fn and(&mut self, other: &Bitmap) {
        for (a, b) in self.data.iter_mut().zip(&other.data) {
            *a &= *b;
        }
    }

    /// `self & !other`, in place.
    pub fn and_not(&mut self, other: &Bitmap) {
        for (a, b) in self.data.iter_mut().zip(&other.data) {
            *a &= (*b == 0) as u8;
        }
    }

    /// Tile mask (alpha 255 where set), sharing one tile for fully covered
    /// tiles and leaving empty ones out.
    pub fn to_mask(&self) -> TileStore {
        let mut out = TileStore::new();
        let full = Arc::new({
            let mut t = Tile::empty();
            t.bytes_mut().fill(255);
            t
        });
        let ts = TILE_SIZE as usize;
        for ty in 0..self.height.div_ceil(ts) {
            for tx in 0..self.width.div_ceil(ts) {
                let (x0, y0) = (tx * ts, ty * ts);
                let (x1, y1) = ((x0 + ts).min(self.width), (y0 + ts).min(self.height));
                let mut covered = 0;
                for y in y0..y1 {
                    covered += self.data[y * self.width + x0..y * self.width + x1].iter().filter(|&&v| v != 0).count();
                }
                if covered == 0 {
                    continue;
                }
                let coord = TileCoord::new(tx as i32, ty as i32);
                if covered == ts * ts {
                    out.insert_shared(coord, full.clone());
                    continue;
                }
                let mut tile = Tile::empty();
                for y in y0..y1 {
                    for x in x0..x1 {
                        if self.data[y * self.width + x] != 0 {
                            tile.set_pixel((x - x0) as u32, (y - y0) as u32, [255; 4]);
                        }
                    }
                }
                out.insert(coord, tile);
            }
        }
        out
    }
}

/// Runs `f(first_row, rows)` over bands of rows of `data` (`width` bytes
/// per row) on all cores.
pub(crate) fn par_rows<T: Send>(data: &mut [T], width: usize, f: impl Fn(usize, &mut [T]) + Sync) {
    let height = data.len() / width.max(1);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(height.max(1));
    let band = height.div_ceil(threads.max(1)).max(1);
    if threads <= 1 || data.len() < 1 << 16 {
        f(0, data);
        return;
    }
    std::thread::scope(|s| {
        for (i, chunk) in data.chunks_mut(band * width).enumerate() {
            let f = &f;
            s.spawn(move || f(i * band, chunk));
        }
    });
}

/// Turns a coverage mask into a stroke of `color`, ready for `raster::commit_stroke`.
pub fn region_to_stroke(region: &TileStore, color: [f32; 3]) -> RasterizedStroke {
    let mut buffer = StrokeBuffer::new();
    let mut dirty = BTreeSet::new();
    for (coord, tile) in region.iter() {
        let pixels = buffer.tile_mut(coord);
        for (dst, src) in pixels.iter_mut().zip(tile.bytes().as_chunks::<4>().0) {
            let a = src[3] as f32 / 255.0;
            *dst = [color[0] * a, color[1] * a, color[2] * a, a];
        }
        dirty.insert(coord);
    }
    RasterizedStroke { buffer, dirty }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_shares_full_tiles_and_skips_empty_ones() {
        let mut b = Bitmap::new(150, 70);
        for y in 0..64 {
            for x in 0..64 {
                b.set(x, y, true);
            }
        }
        b.set(140, 66, true);
        let mask = b.to_mask();
        assert_eq!(mask.len(), 2);
        assert_eq!(mask.pixel(10, 10)[3], 255);
        assert_eq!(mask.pixel(140, 66)[3], 255);
        assert_eq!(mask.pixel(141, 66)[3], 0);
        let stroke = region_to_stroke(&mask, [1.0, 0.0, 0.0]);
        assert_eq!(stroke.buffer.pixel(3, 3), [1.0, 0.0, 0.0, 1.0]);
    }
}
