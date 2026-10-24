//! Snapshot undo/redo.
//!
//! A snapshot clones the layer tree. Tiles are `Arc`-shared, so a snapshot
//! costs one pointer per tile plus layer metadata, not pixel data. The canvas
//! size is part of it, so cropping undoes too. Each step carries the name of
//! the action it undoes, for a History panel.

use crate::document::Document;
use crate::layer::{Layer, LayerId};
use crate::tile::TileStore;

#[derive(Clone)]
struct Snapshot {
    size: (u32, u32),
    layers: Vec<Layer>,
    active: LayerId,
    selection: Option<TileStore>,
    channels: Vec<crate::document::SavedChannel>,
    mode: crate::color_mode::ColorMode,
    profile: crate::color_profile::ColorProfile,
    cmyk: Option<crate::cmyk::CmykSetup>,
    duotone: Option<crate::duotone::Duotone>,
    palette: Vec<[u8; 3]>,
    /// The action between this state and the next one.
    label: String,
    /// `label` is only what was in hand: name the step from what it changed.
    hint: bool,
}

impl Snapshot {
    fn of(doc: &Document, label: String, hint: bool) -> Self {
        Self {
            size: doc.canvas(),
            layers: doc.layers.clone(),
            active: doc.active,
            selection: doc.selection.clone(),
            channels: doc.channels.clone(),
            mode: doc.mode,
            profile: doc.profile,
            cmyk: doc.cmyk.clone(),
            duotone: doc.duotone.clone(),
            palette: doc.palette.clone(),
            label,
            hint,
        }
    }

    fn restore(self, doc: &mut Document) {
        (doc.width, doc.height) = self.size;
        doc.layers = self.layers;
        doc.active = self.active;
        doc.selection = self.selection;
        doc.channels = self.channels;
        doc.mode = self.mode;
        doc.profile = self.profile;
        doc.cmyk = self.cmyk;
        doc.duotone = self.duotone;
        doc.palette = self.palette;
    }
}


/// Every layer in the tree, folders and their contents, top-level order first.
fn flat(layers: &[Layer]) -> Vec<&Layer> {
    let mut out = Vec::new();
    for l in layers {
        out.push(l);
        if let crate::layer::LayerKind::Folder(f) = &l.kind {
            out.extend(flat(&f.children));
        }
    }
    out
}

/// Whether the layer's own content changed (pixels, mask, or a procedural layer's settings).
fn content_changed(a: &Layer, b: &Layer) -> bool {
    let tiles = match (a.tiles(), b.tiles()) {
        (Some(x), Some(y)) => !x.same_tiles(y),
        (None, None) => match (&a.kind, &b.kind) {
            (crate::layer::LayerKind::Folder(_), crate::layer::LayerKind::Folder(_)) => false,
            _ => format!("{:?}", a.kind) != format!("{:?}", b.kind),
        },
        _ => true,
    };
    let mask = match (&a.mask, &b.mask) {
        (Some(x), Some(y)) => !x.tiles.same_tiles(&y.tiles),
        (None, None) => false,
        _ => true,
    };
    tiles || mask
}

fn properties_changed(a: &Layer, b: &Layer) -> bool {
    a.name != b.name
        || a.blend != b.blend
        || a.opacity != b.opacity
        || a.visible != b.visible
        || a.locked != b.locked
        || a.alpha_locked != b.alpha_locked
        || a.clip != b.clip
        || a.reference != b.reference
        || a.draft != b.draft
        || a.layer_color != b.layer_color
        || a.effects != b.effects
        || a.mask.as_ref().map(|m| m.enabled) != b.mask.as_ref().map(|m| m.enabled)
}

/// Names a step from what changed between `before` and `after`; `hint`
/// says what was in hand (the tool or brush).
fn describe(before: &Snapshot, after: &Document, hint: &str) -> String {
    if before.size != after.canvas() {
        return "Canvas Size".into();
    }
    let (old, new) = (flat(&before.layers), flat(&after.layers));
    fn find<'a>(list: &[&'a Layer], id: LayerId) -> Option<&'a Layer> {
        list.iter().find(|l| l.id == id).copied()
    }
    let added: Vec<&Layer> = new.iter().copied().filter(|l| find(&old, l.id).is_none()).collect();
    let removed: Vec<&Layer> = old.iter().copied().filter(|l| find(&new, l.id).is_none()).collect();
    let quoted = |l: &Layer| format!("“{}”", l.name);
    match (added.len(), removed.len()) {
        (0, 0) => {}
        (1, 0) => return format!("New Layer {}", quoted(added[0])),
        (n, 0) => return format!("{n} New Layers"),
        (0, 1) => return format!("Delete Layer {}", quoted(removed[0])),
        (0, n) => return format!("Delete {n} Layers"),
        _ => return "Merge Layers".into(),
    }
    let pairs: Vec<(&Layer, &Layer)> = old.iter().filter_map(|a| find(&new, a.id).map(|b| (*a, b))).collect();
    let painted: Vec<&Layer> = pairs.iter().filter(|(a, b)| content_changed(a, b)).map(|(_, b)| *b).collect();
    match painted.len() {
        0 => {}
        1 => return format!("{hint} · {}", quoted(painted[0])),
        n => return format!("{hint} · {n} layers"),
    }
    let props: Vec<&Layer> = pairs.iter().filter(|(a, b)| properties_changed(a, b)).map(|(_, b)| *b).collect();
    match props.len() {
        0 => {}
        1 => return format!("Layer Properties {}", quoted(props[0])),
        n => return format!("Layer Properties · {n} layers"),
    }
    if old.iter().map(|l| l.id).ne(new.iter().map(|l| l.id)) {
        return "Move Layers".into();
    }
    let selection_changed = match (&before.selection, &after.selection) {
        (Some(a), Some(b)) => !a.same_tiles(b),
        (None, None) => false,
        _ => true,
    };
    if selection_changed {
        return if after.selection.is_some() { "Selection".into() } else { "Deselect".into() };
    }
    hint.to_string()
}
pub struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    limit: usize,
    /// The state the history brush paints from (its name and layers), when chosen.
    source: Option<(String, Vec<Layer>)>,
}

impl History {
    pub fn new(limit: usize) -> Self {
        Self { undo: Vec::new(), redo: Vec::new(), limit: limit.max(1), source: None }
    }

    /// Records the state *before* an edit.
    pub fn push(&mut self, doc: &Document) {
        self.push_hinted(doc, "Edit");
    }

    /// Records the state before an edit called `label` ("Duplicate Layer"…).
    pub fn push_labelled(&mut self, doc: &Document, label: &str) {
        self.record(doc, label, false);
    }

    /// Records the state before an edit made with `hint` in hand (a tool or
    /// brush); the step is named from what it changes ("G-Pen · “Inks”").
    pub fn push_hinted(&mut self, doc: &Document, hint: &str) {
        self.record(doc, hint, true);
    }

    /// Names the last step from the document it led to (when only hinted).
    pub fn name_last(&mut self, doc: &Document) {
        if let Some(top) = self.undo.last_mut().filter(|t| t.hint) {
            top.label = describe(top, doc, &top.label);
            top.hint = false;
        }
    }

    fn record(&mut self, doc: &Document, label: &str, hint: bool) {
        self.name_last(doc);
        self.undo.push(Snapshot::of(doc, label.to_string(), hint));
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self, doc: &mut Document) -> bool {
        let Some(snapshot) = self.undo.pop() else {
            return false;
        };
        let mut snapshot = snapshot;
        if snapshot.hint {
            snapshot.label = describe(&snapshot, doc, &snapshot.label);
            snapshot.hint = false;
        }
        self.redo.push(Snapshot::of(doc, snapshot.label.clone(), false));
        snapshot.restore(doc);
        true
    }

    pub fn redo(&mut self, doc: &mut Document) -> bool {
        let Some(snapshot) = self.redo.pop() else {
            return false;
        };
        self.undo.push(Snapshot::of(doc, snapshot.label.clone(), false));
        snapshot.restore(doc);
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The steps done, oldest first (the last one is the current state's).
    pub fn done(&self) -> impl Iterator<Item = &str> {
        self.undo.iter().map(|s| s.label.as_str())
    }

    /// The steps undone, the next to redo first.
    pub fn undone(&self) -> impl Iterator<Item = &str> {
        self.redo.iter().rev().map(|s| s.label.as_str())
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Keeps at most `limit` steps (the oldest go first).
    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit.max(1);
        let extra = self.undo.len().saturating_sub(self.limit);
        self.undo.drain(..extra);
    }

    /// The layers at state `k` (after `k` of the listed steps; 0 = the oldest kept).
    /// `doc` is the current state.
    pub fn state_layers(&self, k: usize, doc: &Document) -> Option<Vec<Layer>> {
        let current = self.undo.len();
        match k.cmp(&current) {
            std::cmp::Ordering::Less => Some(self.undo[k].layers.clone()),
            std::cmp::Ordering::Equal => Some(doc.layers.clone()),
            std::cmp::Ordering::Greater => self.redo.get(self.redo.len().checked_sub(k - current)?).map(|s| s.layers.clone()),
        }
    }

    /// Applies `f` to the layers of every kept state, done and undone: in a
    /// shared session, changes made by others reach the past states too, so
    /// undoing your own step doesn't take theirs away.
    pub fn edit_snapshots(&mut self, mut f: impl FnMut(&mut Vec<Layer>)) {
        for s in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            f(&mut s.layers);
        }
    }

    /// Makes `layers` (a state called `label`) what the history brush paints from.
    pub fn set_source(&mut self, label: &str, layers: Vec<Layer>) {
        self.source = Some((label.to_string(), layers));
    }

    /// What the history brush paints from: the chosen state, else the oldest kept.
    pub fn source(&self) -> Option<(&str, &[Layer])> {
        match &self.source {
            Some((label, layers)) => Some((label.as_str(), layers.as_slice())),
            None => self.undo.first().map(|s| ("Opened", s.layers.as_slice())),
        }
    }

    /// The name of the history brush's chosen state, if one was chosen.
    pub fn source_label(&self) -> Option<&str> {
        self.source.as_ref().map(|(l, _)| l.as_str())
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.source = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_redo_round_trip() {
        let mut doc = Document::new(10, 10);
        let mut history = History::new(10);
        history.push(&doc);
        doc.find_mut(doc.active).unwrap().name = "Renamed".into();
        assert!(history.undo(&mut doc));
        assert_eq!(doc.active_layer().unwrap().name, "Layer 1");
        assert!(history.redo(&mut doc));
        assert_eq!(doc.active_layer().unwrap().name, "Renamed");
        assert!(!history.can_redo());

        // The canvas size comes back too (crop).
        history.push(&doc);
        doc.width = 4;
        assert!(history.undo(&mut doc));
        assert_eq!(doc.canvas(), (10, 10));
    }

    #[test]
    fn steps_keep_their_names_through_undo_and_redo() {
        let mut doc = Document::new(10, 10);
        let mut history = History::new(10);
        for name in ["Pen", "Fill", "Rename"] {
            history.push_labelled(&doc, name);
        }
        assert!(history.undo(&mut doc) && history.undo(&mut doc));
        assert_eq!(history.done().collect::<Vec<_>>(), ["Pen"]);
        assert_eq!(history.undone().collect::<Vec<_>>(), ["Fill", "Rename"]);
        assert!(history.redo(&mut doc));
        assert_eq!(history.done().collect::<Vec<_>>(), ["Pen", "Fill"]);
        assert_eq!(history.undone().collect::<Vec<_>>(), ["Rename"]);
        history.set_limit(1);
        assert_eq!(history.done().collect::<Vec<_>>(), ["Fill"]);
    }
}

#[cfg(test)]
mod naming_tests {
    use super::*;
    use crate::layer::Layer;

    #[test]
    fn hinted_steps_are_named_from_what_changed() {
        let mut doc = Document::new(64, 64);
        let mut history = History::new(10);
        // Painting the active layer.
        history.push_hinted(&doc, "G-Pen");
        let rgba = vec![255u8; 64 * 64 * 4];
        let active = doc.active;
        doc.find_mut(active).unwrap().kind = crate::layer::LayerKind::Raster(TileStore::from_rgba(64, 64, &rgba));
        // A new layer.
        history.push_hinted(&doc, "G-Pen");
        let id = doc.alloc_id();
        doc.add_layer(Layer::raster(id, "Inks"));
        // A property.
        history.push_hinted(&doc, "G-Pen");
        doc.find_mut(id).unwrap().opacity = 0.5;
        // A named command keeps its name.
        history.push_labelled(&doc, "Flip Horizontal");
        history.name_last(&doc);
        let names: Vec<&str> = history.done().collect();
        assert_eq!(names, ["G-Pen · “Layer 1”", "New Layer “Inks”", "Layer Properties “Inks”", "Flip Horizontal"]);
    }
}
