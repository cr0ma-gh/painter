//! Layer thumbnails for the Layers panel, as Clip Studio's and Photoshop's:
//! each layer's content, small, over a checkerboard. They frame either the
//! area the layer has content in (enlarged, so a small sketch on a big canvas
//! still shows) or the whole canvas, in its shape (`LayerThumbMode`).
//! Drawn again only when a layer's pixels change (and not while a stroke is
//! being painted), a few per frame; folders and corrections keep their icon.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use eframe::egui::{self, Color32};
use painter_core::tile::{TileStore, TILE_SIZE};
use painter_core::{Layer, LayerId, LayerKind};
use ui_workspace::LayerThumbMode;

use crate::app::PainterApp;

/// Thumbnails drawn again at most this many per frame.
const PER_FRAME: usize = 3;

/// Room left around a layer's content in `Content` mode, as a share of its size.
const CONTENT_MARGIN: f32 = 0.08;

/// The smallest canvas area a `Content` thumbnail frames, in pixels, so a
/// single dot isn't blown up into a blur.
const MIN_CONTENT_SIDE: f32 = 16.0;

#[derive(Default)]
pub(crate) struct LayerThumbs {
    /// Layer → what it was drawn from, and the picture.
    cache: HashMap<LayerId, (u64, egui::TextureHandle)>,
}

impl LayerThumbs {
    pub(crate) fn get(&self, id: LayerId) -> Option<&egui::TextureHandle> {
        self.cache.get(&id).map(|(_, t)| t)
    }
}

/// What a layer's thumbnail depends on: its pixels (shared tiles change
/// their address when painted) or its settings, the canvas size and the
/// framing mode. A raster layer's content bounds follow from its tiles, so
/// any change to them changes the key too.
fn content_key(layer: &Layer, canvas: (u32, u32), mode: LayerThumbMode) -> Option<u64> {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    canvas.hash(&mut h);
    mode.hash(&mut h);
    match &layer.kind {
        LayerKind::Raster(_) | LayerKind::Vector(_) => {
            let tiles = layer.tiles()?;
            tiles.len().hash(&mut h);
            for (coord, tile) in tiles.iter() {
                (coord.x, coord.y).hash(&mut h);
                (std::sync::Arc::as_ptr(tile) as usize).hash(&mut h);
            }
        }
        LayerKind::Fill { color } => color.hash(&mut h),
        LayerKind::Gradient(g) => format!("{g:?}").hash(&mut h),
        LayerKind::Tone(t) => format!("{t:?}").hash(&mut h),
        LayerKind::Folder(_) | LayerKind::Adjustment(_) => return None,
    }
    Some(h.finish())
}

/// The painted pixels' bounds `[x0, y0, x1, y1]` (inclusive), if any.
///
/// Only the tiles on the edge of the painted tiles' box are scanned: a tile
/// further in can't hold a pixel beyond them, so a full layer on a huge
/// canvas costs its perimeter, not its area.
pub(crate) fn content_bounds(tiles: &TileStore) -> Option<[i32; 4]> {
    let painted: Vec<_> = tiles.iter().filter(|(_, t)| !t.is_transparent()).collect();
    let first = painted.first()?.0;
    let edges = painted.iter().fold([first.x, first.y, first.x, first.y], |b, (c, _)| {
        [b[0].min(c.x), b[1].min(c.y), b[2].max(c.x), b[3].max(c.y)]
    });
    let mut b: Option<[i32; 4]> = None;
    for (coord, tile) in &painted {
        if coord.x != edges[0] && coord.x != edges[2] && coord.y != edges[1] && coord.y != edges[3] {
            continue;
        }
        let (ox, oy) = coord.origin();
        for (i, px) in tile.bytes().as_chunks::<4>().0.iter().enumerate() {
            if px[3] > 0 {
                let (x, y) = (ox + (i as u32 % TILE_SIZE) as i32, oy + (i as u32 / TILE_SIZE) as i32);
                b = Some(b.map_or([x, y, x, y], |b| [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)]));
            }
        }
    }
    b
}

/// The canvas area `[x, y, w, h]` a layer's thumbnail shows. `Content`: a
/// square around what's painted on a raster or vector layer, with a margin;
/// the whole canvas for empty layers and full-canvas ones (fill, gradient, tone).
fn frame(layer: &Layer, canvas: (u32, u32), mode: LayerThumbMode) -> [f32; 4] {
    let whole = [0.0, 0.0, canvas.0.max(1) as f32, canvas.1.max(1) as f32];
    if mode == LayerThumbMode::Canvas || !matches!(layer.kind, LayerKind::Raster(_) | LayerKind::Vector(_)) {
        return whole;
    }
    let Some(b) = layer.tiles().and_then(content_bounds) else { return whole };
    let (w, h) = ((b[2] - b[0] + 1) as f32, (b[3] - b[1] + 1) as f32);
    let side = (w.max(h) * (1.0 + 2.0 * CONTENT_MARGIN)).max(MIN_CONTENT_SIDE);
    let (cx, cy) = (b[0] as f32 + w / 2.0, b[1] as f32 + h / 2.0);
    [cx - side / 2.0, cy - side / 2.0, side, side]
}

/// The thumbnail's size in pixels for a framed area `[x, y, w, h]`, fitting `side` × `side`.
fn frame_size(area: [f32; 4], side: f32) -> [usize; 2] {
    let k = side / area[2].max(area[3]).max(1.0);
    [((area[2] * k).round() as usize).max(4), ((area[3] * k).round() as usize).max(4)]
}

/// The layer's `area` (`[x, y, w, h]` on the canvas) drawn small
/// (straight colour over a light checkerboard).
fn render(layer: &Layer, area: [f32; 4], size: [usize; 2]) -> egui::ColorImage {
    let (w, h) = (size[0], size[1]);
    let (sx, sy) = (area[2] / w as f32, area[3] / h as f32);
    let mut pixels = Vec::with_capacity(w * h);
    for ty in 0..h {
        for tx in 0..w {
            // A few samples per thumbnail pixel, so thin lines still show.
            let mut acc = [0.0f32; 4];
            for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                let (x, y) = (area[0] + (tx as f32 + ox) * sx, area[1] + (ty as f32 + oy) * sy);
                let p = match &layer.kind {
                    LayerKind::Raster(_) | LayerKind::Vector(_) => {
                        let q = layer.tiles().map_or([0; 4], |t| t.pixel(x.floor() as i32, y.floor() as i32));
                        q.map(|v| v as f32 / 255.0)
                    }
                    LayerKind::Fill { color } => [color[0] as f32 / 255.0, color[1] as f32 / 255.0, color[2] as f32 / 255.0, 1.0],
                    LayerKind::Gradient(g) => g.sample(x, y),
                    LayerKind::Tone(t) => t.sample(x, y),
                    _ => [0.0; 4],
                };
                for k in 0..4 {
                    acc[k] += p[k] / 4.0;
                }
            }
            // Premultiplied colour over the checkerboard.
            let check = if ((tx / 4) + (ty / 4)) % 2 == 0 { 1.0 } else { 0.8 };
            let c = [0, 1, 2].map(|k| ((acc[k] + check * (1.0 - acc[3])).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            pixels.push(Color32::from_rgb(c[0], c[1], c[2]));
        }
    }
    egui::ColorImage { size, pixels }
}

impl PainterApp {
    /// Brings the thumbnails of `ids` up to date (a few per frame), at `side` pixels.
    pub(crate) fn update_layer_thumbs(&mut self, ctx: &egui::Context, ids: &[LayerId], side: f32) {
        // Not while painting: the stroke isn't in the tiles yet, and it would cost frames.
        if self.stroke.is_some() {
            return;
        }
        let canvas = self.doc.canvas();
        let mode = self.ws.chrome.layer_thumb_mode;
        let mut drawn = 0;
        for &id in ids {
            let Some(layer) = self.doc.find(id) else { continue };
            let Some(key) = content_key(layer, canvas, mode) else { continue };
            let key = key ^ (side.round() as u64) << 40;
            if self.layer_thumbs.cache.get(&id).is_some_and(|(k, _)| *k == key) {
                continue;
            }
            if drawn == PER_FRAME {
                ctx.request_repaint();
                break;
            }
            // The content's bounds are found only here, when the key changed.
            let area = frame(layer, canvas, mode);
            let image = render(layer, area, frame_size(area, side));
            match self.layer_thumbs.cache.get_mut(&id) {
                Some((k, tex)) => {
                    tex.set(image, egui::TextureOptions::LINEAR);
                    *k = key;
                }
                None => {
                    let tex = ctx.load_texture(format!("layer-thumb-{}", id.0), image, egui::TextureOptions::LINEAR);
                    self.layer_thumbs.cache.insert(id, (key, tex));
                }
            }
            drawn += 1;
        }
        // Layers that are gone.
        let live: std::collections::HashSet<LayerId> = ids.iter().copied().collect();
        self.layer_thumbs.cache.retain(|id, _| live.contains(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A raster layer on a `w` × `h` canvas, opaque red over `[x0, y0, x1, y1)`.
    fn red_rect(w: u32, h: u32, r: [u32; 4]) -> Layer {
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in r[1]..r[3] {
            for x in r[0]..r[2] {
                let i = ((y * w + x) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(&[255, 0, 0, 255]);
            }
        }
        Layer::raster_with(LayerId(1), "a", TileStore::from_rgba(w, h, &rgba))
    }

    #[test]
    fn thumbnails_show_the_layer_in_the_canvas_shape() {
        assert_eq!(frame_size([0.0, 0.0, 2000.0, 1000.0], 40.0), [40, 20]);
        assert_eq!(frame_size([0.0, 0.0, 1000.0, 3000.0], 40.0), [13, 40]);
        // A red layer half covered: red on the left, checkerboard on the right.
        let layer = red_rect(64, 64, [0, 0, 32, 64]);
        let whole = frame(&layer, (64, 64), LayerThumbMode::Canvas);
        assert_eq!(whole, [0.0, 0.0, 64.0, 64.0]);
        let img = render(&layer, whole, [16, 16]);
        assert_eq!(img.pixels[0], Color32::from_rgb(255, 0, 0));
        let right = img.pixels[15];
        assert!(right.r() == right.g() && right.g() == right.b() && right.r() > 190, "{right:?}");
        // The key follows the pixels.
        let a = content_key(&layer, (64, 64), LayerThumbMode::Canvas);
        let mut other = layer.clone();
        if let Some(t) = other.tiles_mut() {
            t.insert(painter_core::TileCoord::new(0, 0), painter_core::tile::Tile::empty());
        }
        assert_ne!(a, content_key(&other, (64, 64), LayerThumbMode::Canvas));
        assert_eq!(a, content_key(&layer.clone(), (64, 64), LayerThumbMode::Canvas), "copies share tiles: same picture");
        // ...and the mode.
        assert_ne!(a, content_key(&layer, (64, 64), LayerThumbMode::Content));
    }

    #[test]
    fn content_bounds_are_exact_across_tiles() {
        // Spans three tile columns and two rows, starting and ending mid-tile.
        let layer = red_rect(256, 256, [50, 70, 170, 100]);
        assert_eq!(content_bounds(layer.tiles().unwrap()), Some([50, 70, 169, 99]));
        // Nothing painted, or only blank tiles stored: no bounds.
        let mut empty = red_rect(64, 64, [0, 0, 0, 0]);
        assert_eq!(content_bounds(empty.tiles().unwrap()), None);
        empty.tiles_mut().unwrap().insert(painter_core::TileCoord::new(3, 3), painter_core::tile::Tile::empty());
        assert_eq!(content_bounds(empty.tiles().unwrap()), None);
    }

    #[test]
    fn content_mode_enlarges_a_small_detail() {
        // A 40 × 20 detail on a 4000 × 3000 canvas.
        let layer = red_rect(4000, 3000, [2000, 1500, 2040, 1520]);
        // Whole canvas: at 38 px the detail is under one thumbnail pixel, gone.
        let whole = frame(&layer, (4000, 3000), LayerThumbMode::Canvas);
        let size = frame_size(whole, 38.0);
        assert_eq!(size, [38, 29]);
        let img = render(&layer, whole, size);
        assert!(img.pixels.iter().all(|p| p.g() > 150), "no red pixel shows");
        // Content: a square around the detail, which now fills most of it.
        let area = frame(&layer, (4000, 3000), LayerThumbMode::Content);
        assert_eq!(area[2], area[3]);
        assert!(area[0] < 2000.0 && area[0] + area[2] > 2040.0 && area[1] < 1500.0 && area[1] + area[3] > 1520.0, "{area:?}");
        let size = frame_size(area, 38.0);
        assert_eq!(size, [38, 38]);
        let img = render(&layer, area, size);
        let red = img.pixels.iter().filter(|p| *p == &Color32::from_rgb(255, 0, 0)).count();
        assert!(red > 38 * 38 / 4, "{red} red pixels");
        // Empty layers and fills keep the whole canvas.
        let empty = red_rect(64, 64, [0, 0, 0, 0]);
        assert_eq!(frame(&empty, (64, 64), LayerThumbMode::Content), [0.0, 0.0, 64.0, 64.0]);
        let fill = Layer { kind: LayerKind::Fill { color: [1, 2, 3] }, ..empty };
        assert_eq!(frame(&fill, (64, 64), LayerThumbMode::Content), [0.0, 0.0, 64.0, 64.0]);
    }
}
