//! A canvas: its size and layer tree.

use crate::layer::{Layer, LayerId, LayerKind};
use crate::tile::{TileCoord, TileStore, TILE_SIZE};

/// Matches wgpu's default `max_texture_dimension_2d`.
pub const MAX_CANVAS_SIZE: u32 = 8192;

/// Resolution of a canvas made without one (pixels per inch).
pub const DEFAULT_DPI: u32 = 300;

/// A selection kept under a name (an alpha channel in Photoshop).
#[derive(Clone, Debug)]
pub struct SavedChannel {
    pub name: String,
    pub mask: TileStore,
}

#[derive(Clone, Debug)]
pub struct Document {
    pub width: u32,
    pub height: u32,
    /// Print resolution, pixels per inch: how big the canvas is on paper.
    /// Written into exports; it doesn't change any pixels.
    pub dpi: u32,
    /// Rulers, guides and the grid.
    pub rulers: crate::rulers::RulerSet,
    /// Root layers, bottom to top.
    pub layers: Vec<Layer>,
    pub active: LayerId,
    /// Saved selections (Photoshop's alpha channels), oldest first.
    pub channels: Vec<SavedChannel>,
    /// `None`: no selection, everything is editable (see `selection`).
    pub selection: Option<TileStore>,
    next_id: u64,
}

impl Document {
    /// A canvas with a white paper layer and one empty raster layer.
    pub fn new(width: u32, height: u32) -> Self {
        let mut doc = Self {
            width: width.clamp(1, MAX_CANVAS_SIZE),
            height: height.clamp(1, MAX_CANVAS_SIZE),
            dpi: DEFAULT_DPI,
            rulers: crate::rulers::RulerSet::default(),
            layers: Vec::new(),
            active: LayerId(0),
            selection: None,
            channels: Vec::new(),
            next_id: 1,
        };
        let paper = doc.alloc_id();
        doc.layers.push(Layer::fill(paper, "Paper", [255, 255, 255]));
        let first = doc.alloc_id();
        doc.layers.push(Layer::raster(first, "Layer 1"));
        doc.active = first;
        doc
    }

    pub fn canvas(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The canvas size on paper, in millimetres, at [Self::dpi].
    pub fn size_mm(&self) -> (f64, f64) {
        let mm = |px: u32| px as f64 * 25.4 / self.dpi.max(1) as f64;
        (mm(self.width), mm(self.height))
    }

    /// The id the next `alloc_id` returns; restored by project loading.
    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    /// Rebuilds a document from saved parts.
    pub fn from_parts(width: u32, height: u32, layers: Vec<Layer>, active: LayerId, next_id: u64) -> Self {
        let max_id = all_ids(&layers).into_iter().map(|id| id.0).max().unwrap_or(0);
        Self {
            width: width.clamp(1, MAX_CANVAS_SIZE),
            height: height.clamp(1, MAX_CANVAS_SIZE),
            dpi: DEFAULT_DPI,
            rulers: crate::rulers::RulerSet::default(),
            layers,
            active,
            selection: None,
            channels: Vec::new(),
            next_id: next_id.max(max_id + 1),
        }
    }

    /// The folder holding layer `id`: `Some(None)` at the top level, `None`
    /// if there's no such layer.
    pub fn parent_of(&self, id: LayerId) -> Option<Option<LayerId>> {
        fn walk(list: &[Layer], parent: Option<LayerId>, id: LayerId) -> Option<Option<LayerId>> {
            for layer in list {
                if layer.id == id {
                    return Some(parent);
                }
                if let LayerKind::Folder(f) = &layer.kind {
                    if let Some(found) = walk(&f.children, Some(layer.id), id) {
                        return Some(found);
                    }
                }
            }
            None
        }
        walk(&self.layers, None, id)
    }

    /// Whether `ruler` acts on (and shows for) the layer being edited. Rulers
    /// of deleted layers are kept, inactive, in case the deletion is undone.
    pub fn ruler_applies(&self, ruler: &crate::rulers::Ruler) -> bool {
        use crate::rulers::RulerScope;
        let Some(owner) = ruler.layer.map(LayerId) else { return true };
        let Some(owner_parent) = self.parent_of(owner) else { return false };
        match ruler.scope {
            RulerScope::AllLayers => true,
            RulerScope::EditingLayer => self.active == owner,
            RulerScope::SameFolder => self.active == owner || self.parent_of(self.active) == Some(owner_parent),
        }
    }

    /// The rulers acting on the layer being edited (with the grid and the
    /// snapping switch).
    pub fn rulers_in_effect(&self) -> crate::rulers::RulerSet {
        crate::rulers::RulerSet {
            rulers: self.rulers.rulers.iter().filter(|r| self.ruler_applies(r)).cloned().collect(),
            snapping: self.rulers.snapping,
            grid: self.rulers.grid,
        }
    }

    /// Layers marked as fill references (counting a marked folder once).
    pub fn reference_layer_count(&self) -> usize {
        fn count(layers: &[Layer]) -> usize {
            layers
                .iter()
                .map(|l| match &l.kind {
                    _ if l.reference => 1,
                    LayerKind::Folder(f) => count(&f.children),
                    _ => 0,
                })
                .sum()
        }
        count(&self.layers)
    }

    pub fn alloc_id(&mut self) -> LayerId {
        let id = LayerId(self.next_id);
        self.next_id += 1;
        id
    }

    pub fn find(&self, id: LayerId) -> Option<&Layer> {
        find_in(&self.layers, id)
    }

    pub fn find_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
        find_in_mut(&mut self.layers, id)
    }

    pub fn active_layer(&self) -> Option<&Layer> {
        self.find(self.active)
    }

    pub fn active_layer_mut(&mut self) -> Option<&mut Layer> {
        let id = self.active;
        self.find_mut(id)
    }

    /// Inserts `layer` at the top of the active folder, or just above the
    /// active layer, and makes it active.
    pub fn add_layer(&mut self, layer: Layer) -> LayerId {
        let id = layer.id;
        let active = self.active;
        let into_folder = self.find(active).is_some_and(Layer::is_folder);
        if into_folder {
            if let Some(LayerKind::Folder(folder)) = self.find_mut(active).map(|l| &mut l.kind) {
                folder.children.push(layer);
            }
        } else if let Some((list, i)) = locate_mut(&mut self.layers, active) {
            list.insert(i + 1, layer);
        } else {
            self.layers.push(layer);
        }
        self.active = id;
        id
    }

    /// Removes a layer (and a folder's contents). The last root layer is kept.
    pub fn delete_layer(&mut self, id: LayerId) -> bool {
        if self.layers.len() == 1 && self.layers[0].id == id {
            return false;
        }
        let Some((list, i)) = locate_mut(&mut self.layers, id) else {
            return false;
        };
        list.remove(i);
        // Select the layer below, as painting apps usually do.
        let next = (!list.is_empty()).then(|| list[i.saturating_sub(1).min(list.len() - 1)].id);
        self.active =
            next.or_else(|| self.raster_layer_ids().last().copied()).unwrap_or_else(|| self.layers[self.layers.len() - 1].id);
        true
    }

    /// Swaps a layer with its neighbour inside the same parent.
    pub fn move_layer(&mut self, id: LayerId, up: bool) -> bool {
        let Some((list, i)) = locate_mut(&mut self.layers, id) else {
            return false;
        };
        let j = if up { i + 1 } else { i.wrapping_sub(1) };
        if j >= list.len() {
            return false;
        }
        list.swap(i, j);
        true
    }

    /// Layers with pixel tiles (raster and vector), depth first.
    pub fn raster_layer_ids(&self) -> Vec<LayerId> {
        fn collect(list: &[Layer], out: &mut Vec<LayerId>) {
            for layer in list {
                if layer.has_pixels() {
                    out.push(layer.id);
                }
                if let LayerKind::Folder(f) = &layer.kind {
                    collect(&f.children, out);
                }
            }
        }
        let mut out = Vec::new();
        collect(&self.layers, &mut out);
        out
    }

    /// Layers as shown in a layer panel: top first, with nesting depth.
    pub fn rows_top_down(&self) -> Vec<(LayerId, usize)> {
        fn collect(list: &[Layer], depth: usize, out: &mut Vec<(LayerId, usize)>) {
            for layer in list.iter().rev() {
                out.push((layer.id, depth));
                if let LayerKind::Folder(f) = &layer.kind {
                    collect(&f.children, depth + 1, out);
                }
            }
        }
        let mut out = Vec::new();
        collect(&self.layers, 0, &mut out);
        out
    }

    /// Layers that carry a mask (enabled or not).
    pub fn masked_layer_ids(&self) -> Vec<LayerId> {
        all_ids(&self.layers).into_iter().filter(|&id| self.find(id).is_some_and(|l| l.mask.is_some())).collect()
    }

    pub fn tile_count(&self) -> usize {
        self.raster_layer_ids().into_iter().filter_map(|id| self.find(id).and_then(|l| l.tiles())).map(|t| t.len()).sum()
    }

    pub fn tile_in_bounds(&self, coord: TileCoord) -> bool {
        coord.x >= 0
            && coord.y >= 0
            && (coord.x as u32) < self.width.div_ceil(TILE_SIZE)
            && (coord.y as u32) < self.height.div_ceil(TILE_SIZE)
    }
}

fn all_ids(list: &[Layer]) -> Vec<LayerId> {
    let mut out = Vec::new();
    for layer in list {
        out.push(layer.id);
        if let LayerKind::Folder(f) = &layer.kind {
            out.extend(all_ids(&f.children));
        }
    }
    out
}

pub(crate) fn find_in(list: &[Layer], id: LayerId) -> Option<&Layer> {
    for layer in list {
        if layer.id == id {
            return Some(layer);
        }
        if let LayerKind::Folder(f) = &layer.kind {
            if let Some(found) = find_in(&f.children, id) {
                return Some(found);
            }
        }
    }
    None
}

fn find_in_mut(list: &mut [Layer], id: LayerId) -> Option<&mut Layer> {
    for layer in list.iter_mut() {
        if layer.id == id {
            return Some(layer);
        }
        if let LayerKind::Folder(f) = &mut layer.kind {
            if let Some(found) = find_in_mut(&mut f.children, id) {
                return Some(found);
            }
        }
    }
    None
}

/// The sibling list holding `id`, and its index there.
pub(crate) fn locate_mut(list: &mut Vec<Layer>, id: LayerId) -> Option<(&mut Vec<Layer>, usize)> {
    if let Some(i) = list.iter().position(|l| l.id == id) {
        return Some((list, i));
    }
    for layer in list.iter_mut() {
        if let LayerKind::Folder(f) = &mut layer.kind {
            if let Some(found) = locate_mut(&mut f.children, id) {
                return Some(found);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn rulers_follow_their_layer() {
        use crate::rulers::{Ruler, RulerKind, RulerScope};
        let mut doc = Document::new(100, 100);
        let ink = doc.active;
        let folder = doc.alloc_id();
        let mut group = Layer::folder(folder, "Panel");
        let (a, b) = (doc.alloc_id(), doc.alloc_id());
        if let LayerKind::Folder(f) = &mut group.kind {
            f.children.push(Layer::raster(a, "A"));
            f.children.push(Layer::raster(b, "B"));
        }
        doc.layers.push(group);
        let guide = RulerKind::Guide { vertical: true, position: 10.0 };
        doc.rulers.rulers.push(Ruler::on_layer(guide.clone(), ink.0));
        doc.rulers.rulers.push(Ruler { scope: RulerScope::SameFolder, ..Ruler::on_layer(guide.clone(), a.0) });
        doc.rulers.rulers.push(Ruler { scope: RulerScope::AllLayers, ..Ruler::on_layer(guide.clone(), b.0) });
        doc.rulers.rulers.push(Ruler::new(guide));

        assert_eq!(doc.parent_of(a), Some(Some(folder)));
        assert_eq!(doc.parent_of(ink), Some(None));
        // Editing the ink layer: its own ruler, the all-layers one, the document one.
        assert_eq!(doc.rulers_in_effect().rulers.len(), 3);
        doc.active = b;
        // In the folder: A's same-folder ruler, B's, the document one.
        let in_effect = doc.rulers_in_effect();
        assert_eq!(in_effect.rulers.len(), 3);
        assert!(in_effect.rulers.iter().all(|r| r.layer != Some(ink.0)));
        // A deleted owner switches its rulers off.
        doc.active = ink;
        doc.delete_layer(folder);
        assert_eq!(doc.rulers_in_effect().rulers.len(), 2);
        assert_eq!(doc.rulers.rulers.len(), 4, "kept for undo");
    }
    use super::*;

    #[test]
    fn new_document_has_paper_and_layer() {
        let doc = Document::new(100, 100);
        assert_eq!(doc.layers.len(), 2);
        assert!(doc.active_layer().unwrap().can_paint());
    }

    #[test]
    fn layers_go_inside_active_folder() {
        let mut doc = Document::new(10, 10);
        let folder = doc.alloc_id();
        doc.add_layer(Layer::folder(folder, "Folder"));
        let inner = doc.alloc_id();
        doc.add_layer(Layer::raster(inner, "Inner"));
        let rows = doc.rows_top_down();
        assert_eq!(rows[0], (folder, 0));
        assert_eq!(rows[1], (inner, 1));
    }

    #[test]
    fn delete_and_move() {
        let mut doc = Document::new(10, 10);
        let a = doc.active;
        let b = doc.alloc_id();
        doc.add_layer(Layer::raster(b, "B"));
        assert!(doc.delete_layer(b));
        assert_eq!(doc.active, a, "deleting selects the layer below");
        assert!(doc.find(b).is_none());

        let c = doc.alloc_id();
        doc.add_layer(Layer::raster(c, "C"));
        assert!(doc.move_layer(c, false));
        assert_eq!(doc.layers[1].id, c);
        assert!(doc.move_layer(c, true));
        assert_eq!(doc.layers[2].id, c);
        assert!(!doc.move_layer(c, true), "already on top");
    }
}
