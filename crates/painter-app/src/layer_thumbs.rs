//! Layer thumbnails for the Layers panel, as Clip Studio's and Photoshop's:
//! each layer's content, small, over a checkerboard, in the canvas's shape.
//! Drawn again only when a layer's pixels change (and not while a stroke is
//! being painted), a few per frame; folders and corrections keep their icon.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use eframe::egui::{self, Color32};
use painter_core::{Layer, LayerId, LayerKind};

use crate::app::PainterApp;

/// Thumbnails drawn again at most this many per frame.
const PER_FRAME: usize = 3;

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
/// their address when painted) or its settings, and the canvas size.
fn content_key(layer: &Layer, canvas: (u32, u32)) -> Option<u64> {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    canvas.hash(&mut h);
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

/// The thumbnail's size in pixels for a canvas, fitting `side` × `side`.
pub(crate) fn thumb_size(canvas: (u32, u32), side: f32) -> [usize; 2] {
    let k = side / canvas.0.max(canvas.1).max(1) as f32;
    [((canvas.0 as f32 * k).round() as usize).max(4), ((canvas.1 as f32 * k).round() as usize).max(4)]
}

/// The layer drawn small (straight colour over a light checkerboard).
fn render(layer: &Layer, canvas: (u32, u32), size: [usize; 2]) -> egui::ColorImage {
    let (w, h) = (size[0], size[1]);
    let (sx, sy) = (canvas.0 as f32 / w as f32, canvas.1 as f32 / h as f32);
    let mut pixels = Vec::with_capacity(w * h);
    for ty in 0..h {
        for tx in 0..w {
            // A few samples per thumbnail pixel, so thin lines still show.
            let mut acc = [0.0f32; 4];
            for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                let (x, y) = ((tx as f32 + ox) * sx, (ty as f32 + oy) * sy);
                let p = match &layer.kind {
                    LayerKind::Raster(_) | LayerKind::Vector(_) => {
                        let q = layer.tiles().map_or([0; 4], |t| t.pixel(x as i32, y as i32));
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
        let size = thumb_size(canvas, side);
        let mut drawn = 0;
        for &id in ids {
            let Some(layer) = self.doc.find(id) else { continue };
            let Some(key) = content_key(layer, canvas) else { continue };
            let key = key ^ (size[0] as u64) << 48 ^ (size[1] as u64) << 32;
            if self.layer_thumbs.cache.get(&id).is_some_and(|(k, _)| *k == key) {
                continue;
            }
            if drawn == PER_FRAME {
                ctx.request_repaint();
                break;
            }
            let image = render(layer, canvas, size);
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
    use painter_core::TileStore;

    #[test]
    fn thumbnails_show_the_layer_in_the_canvas_shape() {
        assert_eq!(thumb_size((2000, 1000), 40.0), [40, 20]);
        assert_eq!(thumb_size((1000, 3000), 40.0), [13, 40]);
        // A red layer half covered: red on the left, checkerboard on the right.
        let mut rgba = vec![0u8; 64 * 64 * 4];
        for y in 0..64 {
            for x in 0..32 {
                rgba[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4].copy_from_slice(&[255, 0, 0, 255]);
            }
        }
        let layer = Layer::raster_with(LayerId(1), "a", TileStore::from_rgba(64, 64, &rgba));
        let img = render(&layer, (64, 64), [16, 16]);
        assert_eq!(img.pixels[0], Color32::from_rgb(255, 0, 0));
        let right = img.pixels[15];
        assert!(right.r() == right.g() && right.g() == right.b() && right.r() > 190, "{right:?}");
        // The key follows the pixels.
        let a = content_key(&layer, (64, 64));
        let mut other = layer.clone();
        if let Some(t) = other.tiles_mut() {
            t.insert(painter_core::TileCoord::new(0, 0), painter_core::tile::Tile::empty());
        }
        assert_ne!(a, content_key(&other, (64, 64)));
        assert_eq!(a, content_key(&layer.clone(), (64, 64)), "copies share tiles: same picture");
    }
}
