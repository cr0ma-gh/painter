//! Strokes that paint pixels from somewhere else instead of one colour: the
//! clone stamp (another spot of the image), the pattern stamp (a repeated
//! tile) and the history brush (the same spot at an earlier state).
//!
//! The stroke is stamped as usual, so its coverage keeps the brush's tip,
//! dynamics and opacity; [`merge_from_source`] then takes the colour from the
//! source under each pixel.

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::blend::{to_f32, to_u8, Rgba};
use crate::brush::{BrushBlend, BrushMode};
use crate::raster::{merge_stroke, RasterizedStroke, StrokeBuffer};
use crate::tile::{Tile, TileCoord, TileStore, TILE_PIXELS, TILE_SIZE};

/// A repeating image for the pattern stamp and pattern fills.
#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Premultiplied RGBA8, row-major.
    pub rgba: Arc<Vec<u8>>,
}

impl Pattern {
    pub fn new(name: &str, width: u32, height: u32, rgba: Vec<u8>) -> Self {
        assert_eq!(rgba.len(), (width * height * 4) as usize, "pattern size mismatch");
        Self { name: name.into(), width: width.max(1), height: height.max(1), rgba: Arc::new(rgba) }
    }

    /// The pixel at canvas `(x, y)`, the pattern repeated from the origin.
    pub fn pixel(&self, x: i32, y: i32) -> [u8; 4] {
        let (px, py) = (x.rem_euclid(self.width as i32) as usize, y.rem_euclid(self.height as i32) as usize);
        let i = (py * self.width as usize + px) * 4;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2], self.rgba[i + 3]]
    }

    /// Built-in screentone-like patterns in `ink` on transparency, `size` pixels a cell.
    pub fn builtin(size: u32, ink: [u8; 3]) -> Vec<Pattern> {
        let s = size.max(4);
        let make = |name: &str, f: &dyn Fn(f32, f32) -> f32| {
            let mut rgba = vec![0u8; (s * s * 4) as usize];
            for y in 0..s {
                for x in 0..s {
                    // Cell coordinates in 0..1 at pixel centres; 4×4 supersampling for clean edges.
                    let mut cover = 0.0;
                    for sy in 0..4 {
                        for sx in 0..4 {
                            let u = (x as f32 + (sx as f32 + 0.5) / 4.0) / s as f32;
                            let v = (y as f32 + (sy as f32 + 0.5) / 4.0) / s as f32;
                            cover += f(u, v);
                        }
                    }
                    let a = cover / 16.0;
                    let i = ((y * s + x) * 4) as usize;
                    let q = |c: u8| (c as f32 * a + 0.5) as u8;
                    rgba[i..i + 4].copy_from_slice(&[q(ink[0]), q(ink[1]), q(ink[2]), (a * 255.0 + 0.5) as u8]);
                }
            }
            Pattern::new(name, s, s, rgba)
        };
        let inside = |b: bool| if b { 1.0 } else { 0.0 };
        vec![
            make("Dots", &|u, v| inside((u - 0.5).powi(2) + (v - 0.5).powi(2) < 0.09)),
            make("Lines", &|_, v| inside(v < 0.35)),
            make("Diagonal lines", &|u, v| inside((u + v).fract() < 0.35)),
            make("Cross-hatch", &|u, v| inside((u + v).fract() < 0.25 || (u - v + 1.0).fract() < 0.25)),
            make("Checker", &|u, v| inside((u < 0.5) != (v < 0.5))),
            make("Grid", &|u, v| inside(u < 0.15 || v < 0.15)),
        ]
    }
}

/// Where a source stroke takes its colours.
#[derive(Clone, Debug)]
pub enum StrokeSource {
    /// `image` read `offset` pixels away: the clone stamp (`[0, 0]`: the history brush).
    Image { image: TileStore, offset: [i32; 2] },
    /// A pattern repeated over the canvas, `scale` times its size.
    Pattern { pattern: Pattern, scale: f32 },
}

impl StrokeSource {
    /// The source colour (premultiplied) that lands on canvas pixel `(x, y)`.
    pub fn sample(&self, x: i32, y: i32) -> [u8; 4] {
        match self {
            StrokeSource::Image { image, offset } => image.pixel(x + offset[0], y + offset[1]),
            StrokeSource::Pattern { pattern, scale } => {
                let s = scale.max(0.01);
                pattern.pixel(((x as f32 + 0.5) / s).floor() as i32, ((y as f32 + 0.5) / s).floor() as i32)
            }
        }
    }
}

/// How a source stroke lays its colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceMerge {
    /// Over what's there, like paint (the stamps): transparent source pixels leave the layer alone.
    Over,
    /// Towards the source, transparency included (the history brush restores erased pixels).
    Replace,
}

/// One tile of a source stroke merged onto `before`. `stroke` is the stamped
/// coverage (its alpha); `opacity` and the selection scale it, as for paint.
#[allow(clippy::too_many_arguments)]
pub fn merge_from_source(
    before: Option<&Tile>,
    stroke: &[Rgba],
    coord: TileCoord,
    source: &StrokeSource,
    merge: SourceMerge,
    opacity: f32,
    blend: BrushBlend,
    alpha_locked: bool,
    selection: Option<&Tile>,
) -> Tile {
    let (ox, oy) = coord.origin();
    let colour = |i: usize| to_f32(source.sample(ox + (i as u32 % TILE_SIZE) as i32, oy + (i as u32 / TILE_SIZE) as i32));
    match merge {
        SourceMerge::Over => {
            let recoloured: Vec<Rgba> =
                stroke.iter().enumerate().map(|(i, s)| if s[3] <= 0.0 { [0.0; 4] } else { colour(i).map(|v| v * s[3]) }).collect();
            merge_stroke(before, &recoloured, opacity, BrushMode::Paint, blend, alpha_locked, selection)
        }
        SourceMerge::Replace => {
            let mut out = before.cloned().unwrap_or_else(Tile::empty);
            for (i, (o, s)) in out.bytes_mut().as_chunks_mut::<4>().0.iter_mut().zip(stroke).enumerate() {
                let selected = selection.map_or(1.0, |t| t.bytes()[i * 4 + 3] as f32 / 255.0);
                let c = s[3] * opacity * selected;
                if c <= 0.0 {
                    continue;
                }
                let (b, src) = (to_f32(*o), colour(i));
                let mut mixed = [0, 1, 2, 3].map(|k| b[k] + (src[k] - b[k]) * c.min(1.0));
                if alpha_locked {
                    // Keep the layer's alpha: the restored colour at the old coverage.
                    let k = if mixed[3] > 0.0 { b[3] / mixed[3] } else { 0.0 };
                    mixed = [mixed[0] * k, mixed[1] * k, mixed[2] * k, b[3]];
                }
                *o = to_u8(mixed);
            }
            out
        }
    }
}

/// Merges the `coords` tiles of a source stroke into `tiles`, rebuilding each from `before`.
#[allow(clippy::too_many_arguments)]
pub fn commit_source_stroke<'a>(
    tiles: &mut TileStore,
    before: &TileStore,
    buffer: &StrokeBuffer,
    coords: impl IntoIterator<Item = &'a TileCoord>,
    source: &StrokeSource,
    merge: SourceMerge,
    opacity: f32,
    blend: BrushBlend,
    alpha_locked: bool,
    selection: Option<&TileStore>,
) {
    for &coord in coords {
        let Some(pixels) = buffer.get(coord) else { continue };
        let selection_tile = match selection {
            None => None,
            Some(sel) => match sel.get(coord) {
                Some(tile) => Some(&**tile),
                None => continue,
            },
        };
        let merged = merge_from_source(
            before.get(coord).map(|t| &**t),
            pixels,
            coord,
            source,
            merge,
            opacity,
            blend,
            alpha_locked,
            selection_tile,
        );
        if merged.bytes().iter().all(|&b| b == 0) {
            tiles.remove(coord);
        } else {
            tiles.insert(coord, merged);
        }
    }
}

/// An empty stroke buffer and dirty set, for stamping a source stroke on the UI thread.
pub fn new_stroke() -> RasterizedStroke {
    RasterizedStroke { buffer: StrokeBuffer::new(), dirty: BTreeSet::new() }
}

const _: () = assert!(TILE_PIXELS == (TILE_SIZE * TILE_SIZE) as usize);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::BrushSettings;
    use crate::raster::stamp_dab;
    use crate::Dab;

    fn solid(rgba: [u8; 4]) -> TileStore {
        TileStore::from_rgba(64, 64, &rgba.repeat(64 * 64))
    }

    fn dab_at(x: f32, y: f32, r: f32) -> Dab {
        Dab { x, y, radius: r, alpha: 1.0, angle: 0.0, thickness: 1.0, mirror: false }
    }

    #[test]
    fn clone_copies_from_the_offset() {
        // Left half red, right half blue; clone from 32 px to the right.
        let mut img = vec![0u8; 64 * 64 * 4];
        for y in 0..64 {
            for x in 0..64 {
                let c = if x < 32 { [255, 0, 0, 255] } else { [0, 0, 255, 255] };
                img[(y * 64 + x) * 4..][..4].copy_from_slice(&c);
            }
        }
        let layer = TileStore::from_rgba(64, 64, &img);
        let settings = BrushSettings { hardness: 1.0, ..BrushSettings::default() };
        let mut stroke = new_stroke();
        stamp_dab(&mut stroke.buffer, &dab_at(16.0, 32.0, 6.0), [1.0; 3], &settings, (64, 64), &mut stroke.dirty);
        let source = StrokeSource::Image { image: layer.clone(), offset: [32, 0] };
        let mut tiles = layer.clone();
        commit_source_stroke(
            &mut tiles,
            &layer,
            &stroke.buffer,
            &stroke.dirty,
            &source,
            SourceMerge::Over,
            1.0,
            BrushBlend::Normal,
            false,
            None,
        );
        assert_eq!(tiles.pixel(16, 32), [0, 0, 255, 255]);
        assert_eq!(tiles.pixel(2, 2), [255, 0, 0, 255]);
    }

    #[test]
    fn history_brush_restores_transparency() {
        let now = solid([0, 255, 0, 255]);
        let then = TileStore::new();
        let settings = BrushSettings { hardness: 1.0, ..BrushSettings::default() };
        let mut stroke = new_stroke();
        stamp_dab(&mut stroke.buffer, &dab_at(32.0, 32.0, 8.0), [1.0; 3], &settings, (64, 64), &mut stroke.dirty);
        let source = StrokeSource::Image { image: then, offset: [0, 0] };
        let mut tiles = now.clone();
        commit_source_stroke(
            &mut tiles,
            &now,
            &stroke.buffer,
            &stroke.dirty,
            &source,
            SourceMerge::Replace,
            1.0,
            BrushBlend::Normal,
            false,
            None,
        );
        assert_eq!(tiles.pixel(32, 32), [0, 0, 0, 0]);
        assert_eq!(tiles.pixel(2, 2), [0, 255, 0, 255]);
        // Over: a transparent source changes nothing.
        let mut tiles = now.clone();
        commit_source_stroke(
            &mut tiles,
            &now,
            &stroke.buffer,
            &stroke.dirty,
            &source,
            SourceMerge::Over,
            1.0,
            BrushBlend::Normal,
            false,
            None,
        );
        assert_eq!(tiles.pixel(32, 32), [0, 255, 0, 255]);
    }

    #[test]
    fn patterns_repeat() {
        let p = Pattern::builtin(8, [0, 0, 0]).into_iter().find(|p| p.name == "Checker").unwrap();
        assert_eq!(p.pixel(1, 1), p.pixel(9, 17));
        assert_ne!(p.pixel(1, 1)[3], p.pixel(5, 1)[3]);
        let s = StrokeSource::Pattern { pattern: p.clone(), scale: 2.0 };
        assert_eq!(s.sample(2, 2), p.pixel(1, 1));
    }
}
