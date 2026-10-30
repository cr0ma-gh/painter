//! Layer management on a document, for several layers at once: moving them
//! above, below or into another layer (drag and drop), duplicating them
//! (folders with their contents, fresh ids), gathering them into a new
//! folder, deleting them, and a copy of the document without its draft layers
//! (for exports).
//!
//! A selection may name a folder and layers inside it; the operations act on
//! its "roots" (the selected layers not inside another selected folder), in
//! the panel's top-down order, and keep that order.

use crate::document::{find_in, locate_mut, Document};
use crate::layer::{Layer, LayerId, LayerKind};

/// Where dropped layers go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerDrop {
    /// Just above this layer, in its folder.
    Above(LayerId),
    /// Just below this layer, in its folder.
    Below(LayerId),
    /// At the top of this folder.
    Into(LayerId),
}

impl Document {
    /// Whether `id` is `ancestor` or inside it.
    pub fn contains_in(&self, ancestor: LayerId, id: LayerId) -> bool {
        ancestor == id
            || self.find(ancestor).is_some_and(|a| match &a.kind {
                LayerKind::Folder(f) => find_in(&f.children, id).is_some(),
                _ => false,
            })
    }

    /// The selected layers not inside another selected folder, top first.
    pub fn selection_roots(&self, ids: &[LayerId]) -> Vec<LayerId> {
        self.rows_top_down()
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| ids.contains(id) && !ids.iter().any(|other| other != id && self.contains_in(*other, *id)))
            .collect()
    }

    /// Detaches a layer (with its contents) from the tree.
    fn take_layer(&mut self, id: LayerId) -> Option<Layer> {
        let (list, i) = locate_mut(&mut self.layers, id)?;
        Some(list.remove(i))
    }

    /// Puts a detached layer back at `at`; gives it back if `at` doesn't exist.
    fn place_layer(&mut self, layer: Layer, at: LayerDrop) -> Result<(), Box<Layer>> {
        match at {
            LayerDrop::Above(t) | LayerDrop::Below(t) => {
                let Some((list, i)) = locate_mut(&mut self.layers, t) else { return Err(Box::new(layer)) };
                let i = if matches!(at, LayerDrop::Above(_)) { i + 1 } else { i };
                list.insert(i, layer);
                Ok(())
            }
            LayerDrop::Into(f) => match self.find_mut(f).map(|l| &mut l.kind) {
                Some(LayerKind::Folder(folder)) => {
                    folder.children.push(layer);
                    Ok(())
                }
                _ => Err(Box::new(layer)),
            },
        }
    }

    /// Moves `ids` (see [`Document::selection_roots`]) to `to`, keeping
    /// their order. Refused when the target is one of them or inside one.
    pub fn move_layers(&mut self, ids: &[LayerId], to: LayerDrop) -> bool {
        let roots = self.selection_roots(ids);
        let target = match to {
            LayerDrop::Above(t) | LayerDrop::Below(t) | LayerDrop::Into(t) => t,
        };
        if roots.is_empty() || roots.iter().any(|r| self.contains_in(*r, target)) || self.find(target).is_none() {
            return false;
        }
        if matches!(to, LayerDrop::Into(_)) && !self.find(target).is_some_and(Layer::is_folder) {
            return false;
        }
        let taken: Vec<Layer> = roots.iter().filter_map(|id| self.take_layer(*id)).collect();
        // `taken` is top first. Above / into: place the lowest first, each next
        // one above it. Below: the highest first, each next one below it.
        let mut anchor = to;
        let order: Vec<Layer> = match to {
            LayerDrop::Below(_) => taken,
            _ => taken.into_iter().rev().collect(),
        };
        for layer in order {
            let id = layer.id;
            if let Err(layer) = self.place_layer(layer, anchor) {
                // Can't happen (checked above); keep the layer rather than lose it.
                self.layers.push(*layer);
            }
            anchor = match to {
                LayerDrop::Below(_) => LayerDrop::Below(id),
                _ => LayerDrop::Above(id),
            };
            if let LayerDrop::Into(_) = to {
                // Inside the folder now: the next one goes above it.
                anchor = LayerDrop::Above(id);
            }
        }
        true
    }

    /// A deep copy of `layer` with fresh ids from this document; records old → new ids.
    fn copy_with_new_ids(&mut self, layer: &Layer, ids: &mut Vec<(LayerId, LayerId)>) -> Layer {
        let mut copy = layer.clone();
        copy.id = self.alloc_id();
        ids.push((layer.id, copy.id));
        if let (LayerKind::Folder(src), LayerKind::Folder(dst)) = (&layer.kind, &mut copy.kind) {
            dst.children = src.children.iter().map(|c| self.copy_with_new_ids(c, ids)).collect();
        }
        copy
    }

    /// Duplicates `ids` (folders with their contents), each copy just above
    /// its original and named "… copy". Returns every old → new id (contents
    /// included); the copy of the active layer becomes active.
    pub fn duplicate_layers(&mut self, ids: &[LayerId]) -> Vec<(LayerId, LayerId)> {
        let mut map = Vec::new();
        for id in self.selection_roots(ids) {
            let Some(original) = self.find(id).cloned() else { continue };
            let mut copy = self.copy_with_new_ids(&original, &mut map);
            copy.name = format!("{} copy", original.name);
            let _ = self.place_layer(copy, LayerDrop::Above(id));
        }
        if let Some(&(_, new)) = map.iter().find(|(old, _)| *old == self.active) {
            self.active = new;
        }
        map
    }

    /// Gathers `ids` into a new folder where the topmost of them was; the
    /// folder becomes active. `None` when nothing is selected.
    pub fn group_layers(&mut self, ids: &[LayerId], name: &str) -> Option<LayerId> {
        let roots = self.selection_roots(ids);
        let top = *roots.first()?;
        let folder = self.alloc_id();
        self.place_layer(Layer::folder(folder, name), LayerDrop::Above(top)).ok()?;
        self.move_layers(&roots, LayerDrop::Into(folder));
        self.active = folder;
        Some(folder)
    }

    /// Photoshop's and Clip Studio's Merge Down (Ctrl+E): layer `id` is
    /// composited into the raster layer just below it, with its blend mode,
    /// opacity, mask, clipping and effects, and goes away. The lower layer's
    /// own mask is applied first; it keeps its name, blend mode and opacity.
    /// Returns the merged layer, or why it can't be done.
    pub fn merge_down(&mut self, id: LayerId) -> Result<LayerId, &'static str> {
        use crate::io::{composite_layers, Area};
        use crate::tile::{Tile, TileCoord, TileStore, TILE_SIZE};
        let canvas = self.canvas();
        let (list, i) = locate_mut(&mut self.layers, id).ok_or("the layer is gone")?;
        if i == 0 {
            return Err("there is no layer below it in its folder");
        }
        let (lower, upper) = (&list[i - 1], &list[i]);
        if upper.is_folder() {
            return Err("folders can't be merged down: merge the layers inside it");
        }
        if !matches!(lower.kind, LayerKind::Raster(_)) || lower.file_object.is_some() {
            return Err("the layer below isn't a raster layer");
        }
        if lower.locked {
            return Err("the layer below is locked");
        }
        if !upper.visible || !lower.visible {
            return Err("both layers must be visible");
        }
        // The pair alone, the lower one as plain pixels (its mask still hiding).
        let mut base = lower.clone();
        base.blend = crate::blend::BlendMode::Normal;
        base.opacity = 1.0;
        base.clip = false;
        base.effects = Default::default();
        base.layer_color = None;
        let mut top = upper.clone();
        top.layer_color = None;
        let whole_canvas = !top.has_pixels() || top.effects.is_active();
        if top.effects.is_active() {
            // Drawn once here, not once per band below.
            let shown = crate::effects::render(top.tiles().expect("pixel layer"), &top.effects, canvas);
            top.kind = LayerKind::Raster(shown);
            top.effects = Default::default();
        }
        let in_canvas = |c: &TileCoord| {
            c.x >= 0 && c.y >= 0 && (c.x as u32) < canvas.0.div_ceil(TILE_SIZE) && (c.y as u32) < canvas.1.div_ceil(TILE_SIZE)
        };
        let mut coords: Vec<TileCoord> =
            base.tiles().into_iter().chain(top.tiles()).flat_map(|t| t.iter().map(|(c, _)| c)).filter(in_canvas).collect();
        if whole_canvas {
            let (tx, ty) = (canvas.0.div_ceil(TILE_SIZE) as i32, canvas.1.div_ceil(TILE_SIZE) as i32);
            coords.extend((0..ty).flat_map(|y| (0..tx).map(move |x| TileCoord::new(x, y))));
        }
        coords.sort_by_key(|c| (c.y, c.x));
        coords.dedup();
        // Pixels off the canvas the upper layer can't reach stay as they were.
        let mut merged = TileStore::new();
        if let Some(tiles) = base.tiles() {
            for (c, t) in tiles.iter().filter(|(c, _)| !in_canvas(c)) {
                merged.insert_shared(c, t.clone());
            }
        }
        let pair = [base, top];
        // One band of tile rows at a time, so a large canvas never needs a full-size buffer.
        let mut row_start = 0;
        while row_start < coords.len() {
            let ty = coords[row_start].y;
            let row_end = coords[row_start..].iter().position(|c| c.y != ty).map_or(coords.len(), |n| row_start + n);
            let y0 = ty as u32 * TILE_SIZE;
            let area = Area { canvas, x0: 0, y0, w: canvas.0, h: TILE_SIZE.min(canvas.1 - y0) };
            let mut buf = vec![0u8; (area.w * area.h * 4) as usize];
            composite_layers(&pair, &mut buf, area);
            for c in &coords[row_start..row_end] {
                let mut tile = Tile::empty();
                let x0 = c.x as u32 * TILE_SIZE;
                for ly in 0..area.h {
                    for lx in 0..TILE_SIZE.min(canvas.0 - x0) {
                        let i = ((ly * area.w + x0 + lx) * 4) as usize;
                        tile.set_pixel(lx, ly, [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]);
                    }
                }
                merged.insert(*c, tile);
            }
            row_start = row_end;
        }
        let lower_id = list[i - 1].id;
        list.remove(i);
        let lower = &mut list[i - 1];
        lower.kind = LayerKind::Raster(merged);
        lower.mask = None;
        self.active = lower_id;
        Ok(lower_id)
    }

    /// Deletes `ids` (folders with their contents). The last root layer stays.
    pub fn delete_layers(&mut self, ids: &[LayerId]) -> usize {
        self.selection_roots(ids).into_iter().filter(|id| self.delete_layer(*id)).count()
    }

    /// This document without its draft layers (and their contents), for
    /// exporting and printing. Layer data is shared, so this is cheap.
    pub fn without_drafts(&self) -> Document {
        fn strip(list: &mut Vec<Layer>) {
            list.retain(|l| !l.draft);
            for layer in list {
                if let LayerKind::Folder(f) = &mut layer.kind {
                    strip(&mut f.children);
                }
            }
        }
        let mut doc = self.clone();
        strip(&mut doc.layers);
        // Files show Duotone's inks, not the grey underneath.
        crate::color_mode::ink_duotone(&mut doc);
        doc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paper, a folder "F" with "a" (bottom) and "b", then "c" and "d" on top.
    fn doc() -> (Document, [LayerId; 5]) {
        let mut doc = Document::new(64, 64);
        let ink = doc.active;
        doc.find_mut(ink).unwrap().name = "c".into();
        let f = doc.alloc_id();
        let (a, b, d) = (doc.alloc_id(), doc.alloc_id(), doc.alloc_id());
        let mut folder = Layer::folder(f, "F");
        if let LayerKind::Folder(x) = &mut folder.kind {
            x.children.push(Layer::raster(a, "a"));
            x.children.push(Layer::raster(b, "b"));
        }
        // Folder below "c".
        let i = doc.layers.iter().position(|l| l.id == ink).unwrap();
        doc.layers.insert(i, folder);
        doc.layers.push(Layer::raster(d, "d"));
        (doc, [f, a, b, ink, d])
    }

    fn names(doc: &Document) -> Vec<String> {
        doc.rows_top_down().iter().map(|(id, depth)| format!("{}{}", "-".repeat(*depth), doc.find(*id).unwrap().name)).collect()
    }

    #[test]
    fn moves_keep_order_and_refuse_cycles() {
        let (mut d, [f, a, b, c, top]) = doc();
        assert_eq!(names(&d), ["d", "c", "F", "-b", "-a", "Paper"]);
        // "d" and "c" into the folder: on top of it, still d over c.
        assert!(d.move_layers(&[c, top], LayerDrop::Into(f)));
        assert_eq!(names(&d), ["F", "-d", "-c", "-b", "-a", "Paper"]);
        // "a" above "d", "b" below "a".
        assert!(d.move_layers(&[a], LayerDrop::Above(top)));
        assert!(d.move_layers(&[b], LayerDrop::Below(a)));
        assert_eq!(names(&d), ["F", "-a", "-b", "-d", "-c", "Paper"]);
        // Two layers out, below the folder, keeping their order.
        assert!(d.move_layers(&[a, c], LayerDrop::Below(f)));
        assert_eq!(names(&d), ["F", "-b", "-d", "a", "c", "Paper"]);
        // A folder can't go into itself or next to something inside it.
        assert!(!d.move_layers(&[f], LayerDrop::Into(f)));
        assert!(!d.move_layers(&[f], LayerDrop::Above(b)));
        assert!(!d.move_layers(&[a], LayerDrop::Into(c)), "not a folder");
        // A folder and something inside it: the folder moves with its contents.
        assert!(d.move_layers(&[f, b], LayerDrop::Below(c)));
        assert_eq!(names(&d), ["a", "c", "F", "-b", "-d", "Paper"]);
    }

    #[test]
    fn duplicates_group_delete_and_drafts() {
        let (mut d, [f, a, _b, c, _top]) = doc();
        d.active = f;
        let map = d.duplicate_layers(&[f, a]);
        assert_eq!(map.len(), 3, "the folder and both children, once");
        assert_eq!(names(&d), ["d", "c", "F copy", "-b", "-a", "F", "-b", "-a", "Paper"]);
        let copy = map[0].1;
        assert_eq!(d.active, copy);
        assert!(map.iter().all(|(old, new)| old != new && d.find(*new).is_some()));
        // Copies share pixels until painted.
        assert_eq!(d.find(map[1].1).unwrap().tiles().unwrap().len(), d.find(map[1].0).unwrap().tiles().unwrap().len());

        let g = d.group_layers(&[c, copy], "Group").unwrap();
        assert_eq!(names(&d), ["d", "Group", "-c", "-F copy", "--b", "--a", "F", "-b", "-a", "Paper"]);
        assert_eq!(d.active, g);

        d.find_mut(c).unwrap().draft = true;
        d.find_mut(f).unwrap().draft = true;
        let out = d.without_drafts();
        assert_eq!(names(&out), ["d", "Group", "-F copy", "--b", "--a", "Paper"]);
        assert!(d.find(c).is_some(), "the document itself keeps them");

        assert_eq!(d.delete_layers(&[g, c]), 1, "the folder goes with its contents");
        assert_eq!(names(&d), ["d", "F", "-b", "-a", "Paper"]);
    }

    #[test]
    fn merge_down_keeps_the_picture() {
        use crate::layer::Mask;
        use crate::tile::{TileCoord, TileStore};
        let (w, h) = (100u32, 70u32);
        let solid = |px: [u8; 4], x0: u32, x1: u32| {
            let mut rgba = vec![0u8; (w * h * 4) as usize];
            for y in 0..h {
                for x in x0..x1 {
                    let i = ((y * w + x) * 4) as usize;
                    rgba[i..i + 4].copy_from_slice(&px);
                }
            }
            TileStore::from_rgba(w, h, &rgba)
        };
        let mut lower = Layer::raster_with(LayerId(1), "Lower", solid([200, 40, 40, 255], 0, 80));
        let mut mask = Mask { enabled: true, ..Mask::default() };
        mask.tiles.get_or_create_mut(TileCoord::new(1, 0)).set_pixel(3, 3, [255; 4]);
        lower.mask = Some(mask.clone());
        let mut upper = Layer::raster_with(LayerId(2), "Upper", solid([0, 60, 120, 160], 30, 100));
        upper.blend = crate::blend::BlendMode::Multiply;
        upper.opacity = 0.6;
        mask.tiles.get_or_create_mut(TileCoord::new(0, 0)).set_pixel(40, 10, [128; 4]);
        upper.mask = Some(mask);
        let mut doc = Document::from_parts(w, h, vec![lower, upper], LayerId(2), 3);
        let before = crate::io::flatten(&doc);
        assert_eq!(doc.merge_down(LayerId(2)), Ok(LayerId(1)));
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.active, LayerId(1));
        assert!(doc.layers[0].mask.is_none(), "masks are applied");
        assert_eq!(crate::io::flatten(&doc), before);
        assert!(doc.merge_down(LayerId(1)).is_err(), "nothing below");
    }
}
