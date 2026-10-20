//! Panels as layers: each panel gets a masked folder (its art can't leak
//! past the frame) and a border layer, kept in sync with the vector shape.
//!
//! ## Clipping mask pipeline
//!
//! No new GPU pass is needed. A panel folder carries a layer mask built from
//! its polygon ([`panel_mask`]: anti-aliased polygon coverage, inverted,
//! because a `painter_core::Mask` stores the *hidden* amount). Masked folders
//! are composited in isolation, so the existing compositor applies the mask
//! to everything drawn inside, on the GPU (`Plane::Mask` in the tile atlas,
//! sampled by `composite.wgsl`) and on the CPU (`painter_core::io::flatten`)
//! alike. Reshaping a panel regenerates the mask and border tiles
//! ([`ComicPage::refresh`]); the app re-uploads those two planes.
//!
//! ```text
//! Comic page            (folder, pass-through)
//! ├─ Panel 2 border     (raster, the frame line)
//! ├─ Panel 2            (folder, mask = outside the polygon hidden)
//! │  └─ Panel 2 art     (raster: draw here)
//! ├─ Panel 1 border
//! └─ Panel 1
//!    └─ Panel 1 art
//! ```

use std::collections::BTreeMap;

use painter_core::{selection, Document, Layer, LayerId, LayerKind, Mask, TileCoord, TileStore};

use crate::frame_tool::{reading_order, segment_distance, split, Cut, Panel, PanelId};
use crate::page_preset::{GutterMetrics, PageGeometry};

/// Version of the saved page format.
const COMIC_FORMAT: u32 = 1;

#[derive(serde::Serialize, serde::Deserialize)]
struct ComicFile {
    format: u32,
    geometry: PageGeometry,
    gutter: GutterMetrics,
    root: Option<u64>,
    next_panel: u32,
    panels: Vec<PanelFile>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PanelFile {
    panel: Panel,
    folder: u64,
    art: u64,
    border: u64,
}

/// The layers that make up one panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelLayers {
    /// Masked folder holding the panel's art.
    pub folder: LayerId,
    /// The raster layer to draw on.
    pub art: LayerId,
    /// The frame line, above the folder so the mask doesn't cut it in half.
    pub border: LayerId,
}

/// The mask hiding everything outside `panel` (mask alpha = hidden).
pub fn panel_mask(panel: &Panel, canvas: (u32, u32)) -> TileStore {
    let inside = selection::polygon_mask(&panel.shape_f32(), canvas);
    selection::invert(Some(&inside), canvas)
}

/// The panel's frame line, premultiplied RGBA, centred on the polygon edges.
/// Dashes follow the outline's length; borderless panels give no tiles.
pub fn border_tiles(panel: &Panel, canvas: (u32, u32)) -> TileStore {
    let style = &panel.border;
    let mut out = TileStore::new();
    if !style.visible || style.width <= 0.0 || panel.shape.len() < 2 {
        return out;
    }
    let half = style.width * 0.5;
    let n = panel.shape.len();
    // Arc length at the start of each edge, for dashes.
    let mut starts = vec![0.0; n];
    for i in 1..n {
        let (a, b) = (panel.shape[i - 1], panel.shape[i]);
        starts[i] = starts[i - 1] + (b[0] - a[0]).hypot(b[1] - a[1]);
    }
    let [bx, by, bw, bh] = panel.bounds();
    let x0 = (bx - half - 1.0).floor().max(0.0) as i32;
    let y0 = (by - half - 1.0).floor().max(0.0) as i32;
    let x1 = ((bx + bw + half + 1.0).ceil() as i32).min(canvas.0 as i32);
    let y1 = ((by + bh + half + 1.0).ceil() as i32).min(canvas.1 as i32);
    for y in y0..y1 {
        for x in x0..x1 {
            let p = [x as f64 + 0.5, y as f64 + 0.5];
            // Nearest edge and how far along the outline that point is.
            let (d, s) = (0..n)
                .map(|i| {
                    let (a, b) = (panel.shape[i], panel.shape[(i + 1) % n]);
                    let d = segment_distance(p, a, b);
                    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                    let len2 = (dx * dx + dy * dy).max(1e-12);
                    let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0);
                    (d, starts[i] + t * len2.sqrt())
                })
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .unwrap_or((f64::MAX, 0.0));
            let mut cover = if style.antialias {
                (half - d + 0.5).clamp(0.0, 1.0)
            } else if d <= half {
                1.0
            } else {
                0.0
            };
            if let Some([on, off]) = style.dash {
                if on > 0.0 && (s % (on + off.max(0.0))) >= on {
                    cover = 0.0;
                }
            }
            if cover <= 0.0 {
                continue;
            }
            let a = (cover * 255.0).round() as u32;
            let px = [style.color[0], style.color[1], style.color[2]].map(|c| ((c as u32 * a + 127) / 255) as u8);
            let coord = TileCoord::containing(x, y);
            let (ox, oy) = coord.origin();
            out.get_or_create_mut(coord).set_pixel((x - ox) as u32, (y - oy) as u32, [px[0], px[1], px[2], a as u8]);
        }
    }
    out
}

/// A comic page in a document: its geometry, the vector panels and the
/// layers each one owns. (Kept by the app; not yet saved in `.painter` files.)
#[derive(Debug, Clone)]
pub struct ComicPage {
    pub geometry: PageGeometry,
    pub gutter: GutterMetrics,
    pub panels: Vec<Panel>,
    pub layers: BTreeMap<PanelId, PanelLayers>,
    /// The "Comic page" folder holding every panel.
    pub root: Option<LayerId>,
    next_panel: u32,
}

impl ComicPage {
    pub fn new(geometry: PageGeometry, gutter: GutterMetrics) -> Self {
        Self { geometry, gutter, panels: Vec::new(), layers: BTreeMap::new(), root: None, next_panel: 1 }
    }

    pub fn next_id(&mut self) -> PanelId {
        let id = PanelId(self.next_panel);
        self.next_panel += 1;
        id
    }

    pub fn panel(&self, id: PanelId) -> Option<&Panel> {
        self.panels.iter().find(|p| p.id == id)
    }

    fn root_children<'d>(&self, doc: &'d mut Document) -> Option<&'d mut Vec<Layer>> {
        match &mut doc.find_mut(self.root?)?.kind {
            LayerKind::Folder(f) => Some(&mut f.children),
            _ => None,
        }
    }

    /// Adds a panel with its folder, art and border layers (on top of the
    /// page folder, which is created on first use). Returns its layers.
    pub fn add_panel(&mut self, doc: &mut Document, mut panel: Panel) -> PanelLayers {
        if self.root.is_none_or(|id| doc.find(id).is_none()) {
            let id = doc.alloc_id();
            doc.layers.push(Layer::folder(id, "Comic page"));
            self.root = Some(id);
        }
        if self.panel(panel.id).is_some() {
            panel.id = self.next_id();
        }
        self.next_panel = self.next_panel.max(panel.id.0 + 1);
        let canvas = doc.canvas();
        let (folder_id, art_id, border_id) = (doc.alloc_id(), doc.alloc_id(), doc.alloc_id());
        let n = panel.id.0;
        let mut folder = Layer::folder(folder_id, format!("Panel {n}"));
        if let LayerKind::Folder(f) = &mut folder.kind {
            f.pass_through = false;
            f.children.push(Layer::raster(art_id, format!("Panel {n} art")));
        }
        folder.mask = Some(Mask { tiles: panel_mask(&panel, canvas), enabled: true });
        let border = Layer::raster_with(border_id, format!("Panel {n} border"), border_tiles(&panel, canvas));
        if let Some(children) = self.root_children(doc) {
            children.push(folder);
            children.push(border);
        }
        let layers = PanelLayers { folder: folder_id, art: art_id, border: border_id };
        self.layers.insert(panel.id, layers);
        self.panels.push(panel);
        layers
    }

    /// Regenerates a panel's mask and border after its shape or border style
    /// changed. Returns the layers whose planes need re-uploading.
    pub fn refresh(&self, doc: &mut Document, id: PanelId) -> Option<PanelLayers> {
        let panel = self.panel(id)?;
        let layers = *self.layers.get(&id)?;
        let canvas = doc.canvas();
        if let Some(folder) = doc.find_mut(layers.folder) {
            folder.mask = Some(Mask { tiles: panel_mask(panel, canvas), enabled: true });
        }
        if let Some(border) = doc.find_mut(layers.border) {
            border.kind = LayerKind::Raster(border_tiles(panel, canvas));
        }
        Some(layers)
    }

    /// Replaces a panel's shape (frame tool edits) and refreshes its layers.
    pub fn reshape(&mut self, doc: &mut Document, id: PanelId, shape: Vec<[f64; 2]>) -> Option<PanelLayers> {
        self.panels.iter_mut().find(|p| p.id == id)?.shape = shape;
        self.refresh(doc, id)
    }

    /// Splits a panel along `cut` with the page gutter. The first half keeps
    /// the panel's layers (and its art); the second gets new ones.
    pub fn split_panel(&mut self, doc: &mut Document, id: PanelId, cut: Cut) -> Option<(PanelLayers, PanelLayers)> {
        let panel = self.panel(id)?.clone();
        let new_id = PanelId(self.next_panel);
        let gap = self.gutter.for_cut([cut.b[0] - cut.a[0], cut.b[1] - cut.a[1]]);
        let (first, second) = split(&panel, cut, gap, new_id)?;
        self.next_panel += 1;
        let kept = self.reshape(doc, id, first.shape)?;
        let added = self.add_panel(doc, second);
        Some((kept, added))
    }

    /// Removes a panel and its layers.
    pub fn remove_panel(&mut self, doc: &mut Document, id: PanelId) {
        if let Some(layers) = self.layers.remove(&id) {
            if let Some(children) = self.root_children(doc) {
                children.retain(|l| l.id != layers.folder && l.id != layers.border);
            }
        }
        self.panels.retain(|p| p.id != id);
    }

    /// Changes a panel's border and redraws only the border layer.
    pub fn set_border(&mut self, doc: &mut Document, id: PanelId, border: crate::frame_tool::BorderStyle) -> Option<PanelLayers> {
        let canvas = doc.canvas();
        let panel = self.panels.iter_mut().find(|p| p.id == id)?;
        panel.border = border;
        let tiles = border_tiles(panel, canvas);
        let layers = *self.layers.get(&id)?;
        doc.find_mut(layers.border)?.kind = LayerKind::Raster(tiles);
        Some(layers)
    }

    /// Drops panels whose layers were deleted from the Layers panel.
    pub fn prune(&mut self, doc: &Document) {
        let gone: Vec<PanelId> = self.layers.iter().filter(|(_, l)| doc.find(l.folder).is_none()).map(|(id, _)| *id).collect();
        for id in gone {
            self.layers.remove(&id);
            self.panels.retain(|p| p.id != id);
        }
    }

    /// Turns the panel's clipping on or off (off: its art can spill out).
    pub fn set_clip(&self, doc: &mut Document, id: PanelId, clip: bool) -> Option<PanelLayers> {
        let layers = *self.layers.get(&id)?;
        doc.find_mut(layers.folder)?.mask.as_mut()?.enabled = clip;
        Some(layers)
    }

    pub fn clip_enabled(&self, doc: &Document, id: PanelId) -> bool {
        self.layers.get(&id).and_then(|l| doc.find(l.folder)).and_then(|f| f.mask.as_ref()).is_some_and(|m| m.enabled)
    }

    /// Moves a panel's folder and border one step up (towards the top of the
    /// stack) or down among the panels. Returns whether it moved.
    pub fn move_panel(&self, doc: &mut Document, id: PanelId, up: bool) -> bool {
        let Some(layers) = self.layers.get(&id).copied() else {
            return false;
        };
        let Some(children) = self.root_children(doc) else {
            return false;
        };
        // Each panel is a pair: folder then border right above it.
        let Some(at) = children.iter().position(|l| l.id == layers.folder) else {
            return false;
        };
        let pair_end = (at + 2).min(children.len());
        let pair: Vec<Layer> = children.drain(at..pair_end).collect();
        let target = if up { (at + 2).min(children.len()) } else { at.saturating_sub(2) };
        let moved = target != at;
        for (k, layer) in pair.into_iter().enumerate() {
            children.insert(target + k, layer);
        }
        moved
    }

    /// Panel ids from the top of the layer stack down.
    pub fn stack_order(&self, doc: &Document) -> Vec<PanelId> {
        let Some(LayerKind::Folder(root)) = self.root.and_then(|id| doc.find(id)).map(|l| &l.kind) else {
            return Vec::new();
        };
        root.children.iter().rev().filter_map(|l| self.layers.iter().find(|(_, p)| p.folder == l.id).map(|(id, _)| *id)).collect()
    }

    /// The page as JSON for a project file (`comic.json` in a `.painter`).
    /// Masks and borders are ordinary layers, saved with the document.
    pub fn to_json(&self) -> Vec<u8> {
        let file = ComicFile {
            format: COMIC_FORMAT,
            geometry: self.geometry.clone(),
            gutter: self.gutter,
            root: self.root.map(|id| id.0),
            next_panel: self.next_panel,
            panels: self
                .panels
                .iter()
                .filter_map(|p| {
                    let l = self.layers.get(&p.id)?;
                    Some(PanelFile { panel: p.clone(), folder: l.folder.0, art: l.art.0, border: l.border.0 })
                })
                .collect(),
        };
        serde_json::to_vec_pretty(&file).unwrap_or_default()
    }

    /// Reads a page saved by [`Self::to_json`] for `doc`. Panels whose layers
    /// are gone (deleted, or a file edited elsewhere) are dropped.
    pub fn from_json(bytes: &[u8], doc: &Document) -> Result<ComicPage, String> {
        let file: ComicFile = serde_json::from_slice(bytes).map_err(|e| format!("invalid comic page: {e}"))?;
        if file.format > COMIC_FORMAT {
            return Err(format!("comic page made by a newer version (format {})", file.format));
        }
        let root = file.root.map(LayerId).filter(|id| doc.find(*id).is_some_and(Layer::is_folder));
        let mut page = ComicPage {
            geometry: file.geometry,
            gutter: file.gutter,
            panels: Vec::new(),
            layers: BTreeMap::new(),
            root,
            next_panel: file.next_panel.max(1),
        };
        for p in file.panels {
            let layers = PanelLayers { folder: LayerId(p.folder), art: LayerId(p.art), border: LayerId(p.border) };
            if root.is_none() || doc.find(layers.folder).is_none() {
                continue;
            }
            page.next_panel = page.next_panel.max(p.panel.id.0 + 1);
            page.layers.insert(p.panel.id, layers);
            page.panels.push(p.panel);
        }
        Ok(page)
    }

    /// Panel ids in reading order for the page's binding.
    pub fn reading_order(&self) -> Vec<PanelId> {
        reading_order(&self.panels, self.geometry.binding)
    }

    /// The panel whose art layer is `layer` (for "draw in the panel under the pointer").
    pub fn panel_of_layer(&self, layer: LayerId) -> Option<PanelId> {
        self.layers.iter().find(|(_, l)| l.art == layer || l.folder == layer || l.border == layer).map(|(id, _)| *id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame_tool::BorderStyle;
    use crate::page_preset::Binding;
    use painter_core::io;

    fn page(size: u32) -> (Document, ComicPage) {
        let doc = Document::new(size, size);
        let full = [0.0, 0.0, size as f64, size as f64];
        let geometry =
            PageGeometry { canvas: (size, size), bleed: full, trim: full, inner: full, binding: Binding::LeftToRight, webtoon: None };
        (doc, ComicPage::new(geometry, GutterMetrics::uniform(8.0)))
    }

    #[test]
    fn panel_art_never_leaks_outside_the_frame() {
        let (mut doc, mut page) = page(64);
        let id = page.next_id();
        let mut panel = Panel::rect(id, [10.0, 10.0, 30.0, 20.0]);
        panel.border.visible = false;
        let layers = page.add_panel(&mut doc, panel);
        // Paint the whole art layer red.
        let red: Vec<u8> = (0..64 * 64).flat_map(|_| [255, 0, 0, 255]).collect();
        doc.find_mut(layers.art).unwrap().kind = LayerKind::Raster(TileStore::from_rgba(64, 64, &red));
        let flat = io::flatten(&doc);
        let px = |x: usize, y: usize| &flat[(y * 64 + x) * 4..][..4];
        assert_eq!(px(20, 20), [255, 0, 0, 255], "inside");
        assert_eq!(px(5, 5), [255, 255, 255, 255], "outside shows the paper");
        assert_eq!(px(45, 20), [255, 255, 255, 255]);
    }

    #[test]
    fn borders_dashes_and_splits() {
        let (mut doc, mut page) = page(64);
        let id = page.next_id();
        let layers = page.add_panel(&mut doc, Panel::rect(id, [8.0, 8.0, 48.0, 48.0]));
        let border = doc.find(layers.border).unwrap().tiles().unwrap().clone();
        assert_eq!(border.pixel(8, 30)[3], 255, "on the edge");
        assert_eq!(border.pixel(30, 30)[3], 0, "not inside");

        let mut dashed = Panel::rect(PanelId(9), [8.0, 8.0, 48.0, 48.0]);
        dashed.border = BorderStyle { width: 2.0, dash: Some([4.0, 4.0]), ..BorderStyle::default() };
        let tiles = border_tiles(&dashed, (64, 64));
        let along: Vec<u8> = (8..40).map(|x| tiles.pixel(x, 8)[3]).collect();
        assert!(along.iter().any(|&a| a > 200) && along.contains(&0), "{along:?}");

        let (kept, added) = page.split_panel(&mut doc, id, Cut::vertical(page.panel(id).unwrap(), 0.5)).unwrap();
        assert_eq!(kept, layers, "the first half keeps its layers");
        assert_eq!(page.panels.len(), 2);
        let order = page.reading_order();
        assert_eq!(order[0], id);
        let mask = &doc.find(added.folder).unwrap().mask.as_ref().unwrap().tiles;
        assert_eq!(mask.pixel(50, 30)[3], 0, "right half visible in the new panel");
        assert_eq!(mask.pixel(20, 30)[3], 255, "left half hidden in it");
        assert_eq!(page.panel_of_layer(added.art), Some(order[1]));

        // Stacking: the second panel was added last, so it's on top; move it down.
        assert_eq!(page.stack_order(&doc), [order[1], order[0]]);
        assert!(page.move_panel(&mut doc, order[1], false));
        assert_eq!(page.stack_order(&doc), [order[0], order[1]]);
        assert!(!page.move_panel(&mut doc, order[1], false), "already at the bottom");
        assert!(page.clip_enabled(&doc, id));
        page.set_clip(&mut doc, id, false);
        assert!(!page.clip_enabled(&doc, id));

        // Saving and loading with the document keeps panels, borders and their layers.
        let bytes = painter_core::project::to_bytes(&doc).unwrap();
        let loaded_doc = painter_core::project::from_bytes(&bytes).unwrap();
        let loaded = ComicPage::from_json(&page.to_json(), &loaded_doc).unwrap();
        assert_eq!(loaded.panels, page.panels);
        assert_eq!(loaded.layers, page.layers);
        assert_eq!(loaded.reading_order(), page.reading_order());
        assert_eq!(loaded.geometry, page.geometry);
        assert!(loaded.clip_enabled(&loaded_doc, id) == page.clip_enabled(&doc, id));
        // A panel whose layers were deleted elsewhere is dropped on load.
        let mut trimmed = loaded_doc.clone();
        if let Some(LayerKind::Folder(f)) = trimmed.find_mut(page.root.unwrap()).map(|l| &mut l.kind) {
            f.children.retain(|l| l.id != added.folder);
        }
        assert_eq!(ComicPage::from_json(&page.to_json(), &trimmed).unwrap().panels.len(), 1);
        assert!(ComicPage::from_json(b"not json", &trimmed).is_err());

        page.remove_panel(&mut doc, order[1]);
        assert!(doc.find(added.folder).is_none() && page.panels.len() == 1);
    }
}
