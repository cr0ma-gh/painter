//! Copying between open documents: a layer (with its mask, a folder with
//! everything in it, a 3D layer with its scene) or the selection, from the
//! active document into another tab. When the two documents have different
//! resolutions and "match print size" is on, the copy is resampled so it
//! keeps its physical size (a 300 dpi sketch dropped onto a 600 dpi page
//! comes out twice as many pixels wide). The clipboard (Ctrl+C / Ctrl+V)
//! is shared by all tabs and does the same on paste.

use painter_core::{vector, Document, Layer, LayerId, LayerKind, TileStore};
use transform_tools::commit::composite_over;
use transform_tools::crop::copy;
use transform_tools::{resample, FloatingSelection, Image, Interpolation, Mat3};

use crate::app::PainterApp;

/// A layer row being dragged (onto a document tab or a reference view).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LayerDrag {
    pub(crate) layer: LayerId,
}

/// How much a copy from `from_dpi` to `to_dpi` is scaled to keep its size on paper.
pub(crate) fn print_scale(from_dpi: u32, to_dpi: u32, match_print_size: bool) -> f64 {
    if !match_print_size || from_dpi == to_dpi || from_dpi == 0 || to_dpi == 0 {
        1.0
    } else {
        to_dpi as f64 / from_dpi as f64
    }
}

/// `tiles` scaled by `factor` about the canvas origin (bicubic).
pub(crate) fn rescale_tiles(tiles: &TileStore, canvas: (u32, u32), factor: f64) -> TileStore {
    if (factor - 1.0).abs() < 1e-9 {
        return tiles.clone();
    }
    let Some(floating) = copy(tiles, None, canvas) else { return TileStore::new() };
    match rescale_floating(&floating, factor) {
        Some(f) => {
            let image = Image { width: f.width, height: f.height, rgba: f.rgba };
            composite_over(&TileStore::new(), &image, f.origin)
        }
        None => TileStore::new(),
    }
}

/// A floating selection scaled by `factor`, its origin too.
pub(crate) fn rescale_floating(floating: &FloatingSelection, factor: f64) -> Option<FloatingSelection> {
    let width = ((floating.width as f64 * factor).round() as u32).max(1);
    let height = ((floating.height as f64 * factor).round() as u32).max(1);
    let src = Image { width: floating.width, height: floating.height, rgba: floating.rgba.clone() };
    let filter = if factor < 1.0 { Interpolation::Lanczos3 } else { Interpolation::Bicubic };
    let image = resample(&src, &Mat3::scale(factor, factor), width, height, filter)?;
    let origin = [(floating.origin[0] as f64 * factor).round() as i32, (floating.origin[1] as f64 * factor).round() as i32];
    Some(FloatingSelection { origin, width: image.width, height: image.height, rgba: image.rgba })
}

/// A copy of `layer` for `dst`: fresh ids from `dst`, pixels and vector
/// strokes scaled by `factor`. Returns the copy and (old id, new id) pairs.
pub(crate) fn copy_layer(layer: &Layer, src_canvas: (u32, u32), dst: &mut Document, factor: f64) -> (Layer, Vec<(LayerId, LayerId)>) {
    let mut ids = Vec::new();
    let copy = copy_rec(layer, src_canvas, dst, factor, &mut ids);
    (copy, ids)
}

fn copy_rec(layer: &Layer, src_canvas: (u32, u32), dst: &mut Document, factor: f64, ids: &mut Vec<(LayerId, LayerId)>) -> Layer {
    let mut out = layer.clone();
    out.id = dst.alloc_id();
    ids.push((layer.id, out.id));
    let dst_canvas = dst.canvas();
    out.kind = match &layer.kind {
        LayerKind::Raster(tiles) => LayerKind::Raster(rescale_tiles(tiles, src_canvas, factor)),
        LayerKind::Vector(v) => {
            let mut v = v.clone();
            if (factor - 1.0).abs() > 1e-9 {
                for stroke in &mut v.strokes {
                    for p in &mut stroke.points {
                        p.x = (p.x as f64 * factor) as f32;
                        p.y = (p.y as f64 * factor) as f32;
                    }
                    stroke.settings.size = (stroke.settings.size as f64 * factor) as f32;
                }
                let k = factor as f32;
                for t in &mut v.texts {
                    t.transform(|[x, y]| [x * k, y * k], k);
                }
            }
            vector::rasterize(&mut v, dst_canvas);
            LayerKind::Vector(v)
        }
        LayerKind::Folder(f) => {
            let mut f = f.clone();
            f.children = f.children.iter().map(|c| copy_rec(c, src_canvas, dst, factor, ids)).collect();
            LayerKind::Folder(f)
        }
        // Procedural layers cover the canvas they land on.
        other => other.clone(),
    };
    if let Some(mask) = &mut out.mask {
        mask.tiles = rescale_tiles(&mask.tiles, src_canvas, factor);
    }
    out
}

impl PainterApp {
    /// Copies layer `id` of the active document into tab `index`.
    pub(crate) fn transfer_layer_to(&mut self, index: usize, id: LayerId) {
        if index == self.docs.active || index >= self.docs.slots.len() {
            return;
        }
        self.commit_scenes(true);
        let Some(layer) = self.doc.find(id).cloned() else { return };
        let src_canvas = self.doc.canvas();
        let src_dpi = self.doc.dpi;
        let match_size = self.tools.paste_match_dpi;
        let rulers: Vec<painter_core::Ruler> = self.doc.rulers.rulers.iter().filter(|r| r.layer.is_some()).cloned().collect();
        let scenes: Vec<(LayerId, crate::scene_ui::SceneLayer)> = self.scenes.iter().map(|(k, v)| (*k, v.clone())).collect();
        let title = self.tab_title(index);
        let slot = &mut self.docs.slots[index];
        let Some(target) = slot.parked.as_mut() else { return };
        let factor = print_scale(src_dpi, target.doc.dpi, match_size);
        target.history.push(&target.doc);
        let (copy, ids) = copy_layer(&layer, src_canvas, &mut target.doc, factor);
        let name = copy.name.clone();
        let new_id = target.doc.add_layer(copy);
        // 3D layers bring their scenes along.
        for (old, new) in &ids {
            if let Some((_, scene)) = scenes.iter().find(|(k, _)| k == old) {
                target.scenes.insert(*new, scene.clone());
            }
        }
        // The layers' rulers come along, scaled like the pixels.
        for (old, new) in &ids {
            for r in rulers.iter().filter(|r| r.layer == Some(old.0)) {
                let mut copy = r.scaled(factor as f32);
                copy.layer = Some(new.0);
                target.doc.rulers.rulers.push(copy);
            }
        }
        target.doc.active = new_id;
        target.dirty = true;
        slot.revision += 1;
        self.status = if (factor - 1.0).abs() > 1e-9 {
            format!("Copied {name} to {title}, scaled ×{factor:.2} to keep its print size")
        } else {
            format!("Copied {name} to {title}")
        };
    }

    /// Copies the active document's selection into tab `index`.
    pub(crate) fn transfer_selection_to(&mut self, index: usize) {
        if index == self.docs.active || index >= self.docs.slots.len() {
            return;
        }
        let Some(selection) = self.doc.selection.clone() else { return };
        let src_canvas = self.doc.canvas();
        let src_dpi = self.doc.dpi;
        let match_size = self.tools.paste_match_dpi;
        let slot = &mut self.docs.slots[index];
        let Some(target) = slot.parked.as_mut() else { return };
        let factor = print_scale(src_dpi, target.doc.dpi, match_size);
        target.history.push(&target.doc);
        target.doc.selection = Some(rescale_tiles(&selection, src_canvas, factor));
        target.dirty = true;
        slot.revision += 1;
        self.status = "Selection copied".into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use painter_core::{Mask, TileCoord};

    fn square(x0: i32, y0: i32, side: i32) -> TileStore {
        let mut tiles = TileStore::new();
        for y in y0..y0 + side {
            for x in x0..x0 + side {
                let c = TileCoord::containing(x, y);
                let (ox, oy) = c.origin();
                tiles.get_or_create_mut(c).set_pixel((x - ox) as u32, (y - oy) as u32, [200, 0, 0, 255]);
            }
        }
        tiles
    }

    #[test]
    fn rescaling_keeps_the_print_size() {
        assert_eq!(print_scale(300, 600, true), 2.0);
        assert_eq!(print_scale(300, 600, false), 1.0);
        let tiles = square(10, 20, 30);
        let big = rescale_tiles(&tiles, (100, 100), 2.0);
        // 10..40 becomes 20..80.
        assert_eq!(big.pixel(25, 45)[3], 255);
        assert_eq!(big.pixel(78, 98)[3], 255);
        assert_eq!(big.pixel(15, 45)[3], 0);
        assert_eq!(big.pixel(85, 50)[3], 0);
        assert_eq!(rescale_tiles(&tiles, (100, 100), 1.0).pixel(10, 20), [200, 0, 0, 255]);
    }

    #[test]
    fn folders_copy_with_fresh_ids_and_masks() {
        let src = Document::new(100, 100);
        let mut dst = Document::new(200, 200);
        let mut folder = Layer::folder(LayerId(50), "Inks");
        if let LayerKind::Folder(f) = &mut folder.kind {
            let mut inner = Layer::raster_with(LayerId(51), "Lines", square(0, 0, 10));
            inner.mask = Some(Mask { enabled: true, tiles: square(0, 0, 5) });
            f.children.push(inner);
        }
        let before = dst.next_id();
        let (copy, ids) = copy_layer(&folder, src.canvas(), &mut dst, 2.0);
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().all(|(_, new)| new.0 >= before), "ids come from the target");
        let LayerKind::Folder(f) = &copy.kind else { panic!("still a folder") };
        let inner = &f.children[0];
        assert_eq!(inner.tiles().unwrap().pixel(18, 18)[3], 255, "scaled ×2");
        assert_eq!(inner.mask.as_ref().unwrap().tiles.pixel(8, 8)[3], 255);
        assert_eq!(inner.mask.as_ref().unwrap().tiles.pixel(12, 12)[3], 0);
    }
}
