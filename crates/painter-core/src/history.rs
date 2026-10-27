//! Snapshot undo/redo.
//!
//! A snapshot clones the layer tree. Tiles are `Arc`-shared, so a snapshot
//! costs one pointer per tile plus layer metadata, not pixel data. The canvas
//! size is part of it, so cropping undoes too. Each step carries the name of
//! the action it undoes, for a History panel.
//!
//! With a RAM budget ([`History::set_ram_budget`]), the steps farthest from
//! the current state page their tiles out to a deflated swap file once the
//! pixels only the history holds outgrow it, and page them back in when undone
//! or redone. Tiles the document still shows stay in RAM: they cost nothing more.

mod swap;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use crate::document::{Document, SavedChannel};
use crate::layer::{Layer, LayerId, LayerKind};
use crate::tile::{Tile, TileCoord, TileStore, TILE_BYTES};

use swap::{RecordId, SwapFile};

struct Snapshot {
    size: (u32, u32),
    /// Print resolution, so a resolution change undoes with the canvas size.
    dpi: u32,
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
    /// Set when the step's tiles are paged out (its stores keep only the pinned ones).
    cold: Option<Cold>,
}

/// What a paged-out step needs to get its tiles back.
#[derive(Default)]
struct Cold {
    /// `(store, coordinate, record)` of each tile on disk; stores are numbered by [`stores_mut`].
    pages: Vec<(u32, TileCoord, RecordId)>,
    /// Tiles kept in RAM because the document holds them too, by tile.
    pins: HashMap<usize, Vec<(u32, TileCoord)>>,
    /// The layers read back for [`History::source`], until the next step.
    thawed: OnceLock<Vec<Layer>>,
}

/// Every tile store of a state, in a fixed order: each layer's pixels (a
/// folder's children in its place) then its mask, the selection, the channels.
fn stores_mut<'a>(
    layers: &'a mut [Layer],
    selection: &'a mut Option<TileStore>,
    channels: &'a mut [SavedChannel],
) -> Vec<&'a mut TileStore> {
    fn walk<'a>(layers: &'a mut [Layer], out: &mut Vec<&'a mut TileStore>) {
        for layer in layers {
            let Layer { kind, mask, .. } = layer;
            match kind {
                LayerKind::Raster(tiles) => out.push(tiles),
                LayerKind::Vector(vector) => out.push(&mut vector.cache),
                LayerKind::Folder(folder) => walk(&mut folder.children, out),
                _ => {}
            }
            if let Some(mask) = mask {
                out.push(&mut mask.tiles);
            }
        }
    }
    let mut out = Vec::new();
    walk(layers, &mut out);
    out.extend(selection.as_mut());
    out.extend(channels.iter_mut().map(|c| &mut c.mask));
    out
}

/// Calls `f` on every tile of a state (once per coordinate).
fn each_tile(
    layers: &[Layer],
    selection: &Option<TileStore>,
    channels: &[SavedChannel],
    f: &mut impl FnMut(&Arc<Tile>),
) {
    fn store(tiles: &TileStore, f: &mut impl FnMut(&Arc<Tile>)) {
        tiles.iter().for_each(|(_, t)| f(t));
    }
    for layer in layers {
        match &layer.kind {
            LayerKind::Raster(tiles) => store(tiles, f),
            LayerKind::Vector(vector) => store(&vector.cache, f),
            LayerKind::Folder(folder) => each_tile(&folder.children, &None, &[], f),
            _ => {}
        }
        if let Some(mask) = &layer.mask {
            store(&mask.tiles, f);
        }
    }
    selection.iter().for_each(|s| store(s, f));
    channels.iter().for_each(|c| store(&c.mask, f));
}

/// The distinct tiles of a state, by address.
fn tile_keys(layers: &[Layer], selection: &Option<TileStore>, channels: &[SavedChannel]) -> HashSet<usize> {
    let mut keys = HashSet::new();
    each_tile(layers, selection, channels, &mut |t| {
        keys.insert(swap::key(t));
    });
    keys
}

impl Snapshot {
    fn of(doc: &Document, label: String, hint: bool) -> Self {
        Self {
            size: doc.canvas(),
            dpi: doc.dpi,
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
            cold: None,
        }
    }

    fn tile_keys(&self) -> HashSet<usize> {
        tile_keys(&self.layers, &self.selection, &self.channels)
    }

    /// Restores the state into `doc`; its tiles must be paged in.
    fn restore(self, doc: &mut Document) {
        debug_assert!(self.cold.is_none());
        (doc.width, doc.height) = self.size;
        doc.dpi = self.dpi;
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
/// Undo/redo steps: at most `limit`, and with a RAM budget, at most that much
/// pixel memory of their own (see the module docs).
pub struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    limit: usize,
    /// The state the history brush paints from (its name and layers), when chosen.
    source: Option<(String, Vec<Layer>)>,
    /// The most RAM the tiles only the history holds may take; `None`: no bound.
    budget: Option<usize>,
    swap: SwapFile,
    /// How many steps in RAM hold each tile, by tile.
    held: HashMap<usize, usize>,
    /// The document's tiles when last seen, by tile.
    doc_tiles: HashSet<usize>,
    /// RAM the history alone holds, when last counted.
    resident: usize,
}

/// Where swap files go unless told otherwise.
fn default_swap_dir() -> PathBuf {
    std::env::temp_dir().join("painter").join("history_swap")
}

/// Counts `s`'s tiles as held by one more step in RAM.
fn hold(held: &mut HashMap<usize, usize>, s: &Snapshot) {
    for k in s.tile_keys() {
        *held.entry(k).or_default() += 1;
    }
}

/// Counts `s`'s tiles as held by one step fewer.
fn unhold(held: &mut HashMap<usize, usize>, s: &Snapshot) {
    for k in s.tile_keys() {
        if let Some(n) = held.get_mut(&k) {
            *n -= 1;
            if *n == 0 {
                held.remove(&k);
            }
        }
    }
}

/// Pages `s`'s tiles out, but for those `doc_tiles` holds (pinned instead).
fn freeze(swap: &mut SwapFile, s: &mut Snapshot, doc_tiles: &HashSet<usize>) {
    let mut cold = Cold::default();
    let Snapshot { layers, selection, channels, .. } = s;
    for (n, store) in stores_mut(layers, selection, channels).into_iter().enumerate() {
        let tiles: Vec<(TileCoord, Arc<Tile>)> = store.iter().map(|(c, t)| (c, t.clone())).collect();
        for (coord, tile) in tiles {
            let k = swap::key(&tile);
            if !doc_tiles.contains(&k) {
                if let Some(id) = swap.store(&tile) {
                    store.remove(coord);
                    cold.pages.push((n as u32, coord, id));
                    continue;
                }
            }
            cold.pins.entry(k).or_default().push((n as u32, coord));
        }
    }
    s.cold = Some(cold);
}

/// Pages out the tiles a paged-out step pinned that the document let go of.
fn unpin(swap: &mut SwapFile, s: &mut Snapshot, gone: &[usize]) {
    let Snapshot { layers, selection, channels, cold, .. } = s;
    let Some(cold) = cold.as_mut() else { return };
    if !gone.iter().any(|k| cold.pins.contains_key(k)) {
        return;
    }
    let mut stores = stores_mut(layers, selection, channels);
    for k in gone {
        for (n, coord) in cold.pins.remove(k).unwrap_or_default() {
            let store = &mut stores[n as usize];
            let Some(tile) = store.get(coord).cloned() else { continue };
            match swap.store(&tile) {
                Some(id) => {
                    store.remove(coord);
                    cold.pages.push((n, coord, id));
                }
                None => cold.pins.entry(*k).or_default().push((n, coord)),
            }
        }
    }
}

/// Pages `s`'s tiles back in. A tile the disk fails to give back stays out
/// (it reads as transparent).
fn thaw(swap: &mut SwapFile, s: &mut Snapshot) {
    let Snapshot { layers, selection, channels, cold, .. } = s;
    let Some(cold) = cold.take() else { return };
    let mut stores = stores_mut(layers, selection, channels);
    for (n, coord, id) in cold.pages {
        if let Some(tile) = swap.take(id) {
            stores[n as usize].put_shared(coord, tile);
        }
    }
}

impl History {
    pub fn new(limit: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            limit: limit.max(1),
            source: None,
            budget: None,
            swap: SwapFile::new(default_swap_dir()),
            held: HashMap::new(),
            doc_tiles: HashSet::new(),
            resident: 0,
        }
    }

    /// A history of at most `limit` steps whose own tiles take at most
    /// `max_ram_bytes` of RAM: older steps page out to a swap file in `swap_dir`.
    pub fn with_ram_budget(limit: usize, max_ram_bytes: usize, swap_dir: impl Into<PathBuf>) -> Self {
        let mut history = Self::new(limit);
        history.set_swap_dir(swap_dir);
        history.set_ram_budget(Some(max_ram_bytes));
        history
    }

    /// Bounds the RAM the history's own tiles take (`None`: no bound); applies
    /// from the next step recorded, undone or redone.
    pub fn set_ram_budget(&mut self, max_ram_bytes: Option<usize>) {
        self.budget = max_ram_bytes;
    }

    /// The RAM budget, if any (see [`History::set_ram_budget`]).
    pub fn ram_budget(&self) -> Option<usize> {
        self.budget
    }

    /// The folder for the swap file (the system's temporary folder by
    /// default). A swap file already in use stays where it is.
    pub fn set_swap_dir(&mut self, dir: impl Into<PathBuf>) {
        self.swap.set_dir(dir.into());
    }

    /// RAM taken by the tiles the history holds and the document doesn't, as
    /// of the last step recorded, undone or redone. Within the budget then,
    /// unless the disk failed; editing adds the replaced tiles until the next step.
    pub fn memory_bytes(&self) -> usize {
        self.resident
    }

    /// Disk taken by paged-out tiles (compressed).
    pub fn swap_bytes(&self) -> u64 {
        self.swap.bytes()
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
        // A hinted step is never paged out, so its tiles are there to compare.
        if let Some(top) = self.undo.last_mut().filter(|t| t.hint) {
            top.label = describe(top, doc, &top.label);
            top.hint = false;
        }
    }

    fn record(&mut self, doc: &Document, label: &str, hint: bool) {
        self.name_last(doc);
        let snapshot = Snapshot::of(doc, label.to_string(), hint);
        hold(&mut self.held, &snapshot);
        self.undo.push(snapshot);
        if self.undo.len() > self.limit {
            let oldest = self.undo.remove(0);
            self.forget(oldest);
        }
        for s in std::mem::take(&mut self.redo) {
            self.forget(s);
        }
        self.fit(doc);
    }

    /// Lets go of a step for good.
    fn forget(&mut self, s: Snapshot) {
        match &s.cold {
            Some(cold) => cold.pages.iter().for_each(|&(_, _, id)| self.swap.release(id)),
            None => unhold(&mut self.held, &s),
        }
    }

    /// Readies a step taken off the stacks to be restored: all its tiles in RAM.
    fn wake(&mut self, s: &mut Snapshot) {
        match s.cold {
            Some(_) => thaw(&mut self.swap, s),
            None => unhold(&mut self.held, s),
        }
    }

    /// The state `doc` is in, as a step in RAM.
    fn snapshot(&mut self, doc: &Document, label: String) -> Snapshot {
        let snapshot = Snapshot::of(doc, label, false);
        hold(&mut self.held, &snapshot);
        snapshot
    }

    /// Pages out the steps farthest from `doc`'s state until the tiles only
    /// the history holds fit the budget, and recounts them.
    fn fit(&mut self, doc: &Document) {
        for s in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            if let Some(cold) = &mut s.cold {
                cold.thawed.take();
            }
        }
        self.swap.prune();
        let doc_tiles = tile_keys(&doc.layers, &doc.selection, &doc.channels);
        // Tiles pinned for the document that it painted over go to disk now.
        let gone: Vec<usize> = self.doc_tiles.iter().filter(|k| !doc_tiles.contains(k)).copied().collect();
        self.doc_tiles = doc_tiles;
        if !gone.is_empty() {
            for s in self.undo.iter_mut().chain(self.redo.iter_mut()) {
                unpin(&mut self.swap, s, &gone);
            }
        }
        let mut own = self.held.keys().filter(|k| !self.doc_tiles.contains(k)).count() * TILE_BYTES;
        while self.budget.is_some_and(|b| own > b) && !self.swap.is_broken() {
            // The step in RAM farthest from the current state, done or undone.
            let undo = self.undo.iter().position(|s| s.cold.is_none() && !s.hint).map(|i| (self.undo.len() - i, i));
            let redo = self.redo.iter().position(|s| s.cold.is_none()).map(|j| (self.redo.len() - j, j));
            let victim = match (undo, redo) {
                (Some(u), Some(r)) if r.0 > u.0 => &mut self.redo[r.1],
                (Some(u), _) => &mut self.undo[u.1],
                (None, Some(r)) => &mut self.redo[r.1],
                (None, None) => break,
            };
            for k in victim.tile_keys() {
                let Some(n) = self.held.get_mut(&k) else { continue };
                *n -= 1;
                if *n == 0 {
                    self.held.remove(&k);
                    if !self.doc_tiles.contains(&k) {
                        own -= TILE_BYTES;
                    }
                }
            }
            freeze(&mut self.swap, victim, &self.doc_tiles);
        }
        if self.swap.is_broken() {
            // Tiles that failed to page out stayed in RAM.
            let mut stuck = HashSet::new();
            for s in self.undo.iter().chain(&self.redo).filter(|s| s.cold.is_some()) {
                each_tile(&s.layers, &s.selection, &s.channels, &mut |t| {
                    let k = swap::key(t);
                    if !self.doc_tiles.contains(&k) && !self.held.contains_key(&k) {
                        stuck.insert(k);
                    }
                });
            }
            own += stuck.len() * TILE_BYTES;
        }
        self.resident = own;
    }

    pub fn undo(&mut self, doc: &mut Document) -> bool {
        let Some(mut snapshot) = self.undo.pop() else {
            return false;
        };
        self.wake(&mut snapshot);
        if snapshot.hint {
            snapshot.label = describe(&snapshot, doc, &snapshot.label);
            snapshot.hint = false;
        }
        let after = self.snapshot(doc, snapshot.label.clone());
        self.redo.push(after);
        snapshot.restore(doc);
        self.fit(doc);
        true
    }

    /// Takes back the last step as if it never happened: `doc` returns to the
    /// state before it and, unlike [`History::undo`], nothing is left to redo
    /// (a stroke a resting palm started, rolled back).
    pub fn rollback(&mut self, doc: &mut Document) -> bool {
        let Some(mut snapshot) = self.undo.pop() else {
            return false;
        };
        self.wake(&mut snapshot);
        snapshot.restore(doc);
        self.fit(doc);
        true
    }

    pub fn redo(&mut self, doc: &mut Document) -> bool {
        let Some(mut snapshot) = self.redo.pop() else {
            return false;
        };
        self.wake(&mut snapshot);
        let before = self.snapshot(doc, snapshot.label.clone());
        self.undo.push(before);
        snapshot.restore(doc);
        self.fit(doc);
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
        for s in self.undo.drain(..extra).collect::<Vec<_>>() {
            self.forget(s);
        }
    }

    /// A step's layers with all their tiles, read back from disk if paged out.
    fn layers_of(&self, s: &Snapshot) -> Vec<Layer> {
        let Some(cold) = &s.cold else {
            return s.layers.clone();
        };
        let (mut layers, mut selection, mut channels) = (s.layers.clone(), s.selection.clone(), s.channels.clone());
        let mut stores = stores_mut(&mut layers, &mut selection, &mut channels);
        for &(n, coord, id) in &cold.pages {
            if let Some(tile) = self.swap.read(id) {
                stores[n as usize].put_shared(coord, tile);
            }
        }
        layers
    }

    /// The layers at state `k` (after `k` of the listed steps; 0 = the oldest kept).
    /// `doc` is the current state.
    pub fn state_layers(&self, k: usize, doc: &Document) -> Option<Vec<Layer>> {
        let current = self.undo.len();
        match k.cmp(&current) {
            std::cmp::Ordering::Less => Some(self.layers_of(&self.undo[k])),
            std::cmp::Ordering::Equal => Some(doc.layers.clone()),
            std::cmp::Ordering::Greater => self.redo.get(self.redo.len().checked_sub(k - current)?).map(|s| self.layers_of(s)),
        }
    }

    /// Applies `f` to the layers of every kept state, done and undone: in a
    /// shared session, changes made by others reach the past states too, so
    /// undoing your own step doesn't take theirs away. Paged-out steps come
    /// back into RAM first (until the next step).
    pub fn edit_snapshots(&mut self, mut f: impl FnMut(&mut Vec<Layer>)) {
        for s in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            thaw(&mut self.swap, s);
            f(&mut s.layers);
        }
        self.held.clear();
        for s in self.undo.iter().chain(&self.redo) {
            hold(&mut self.held, s);
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
            None => self.undo.first().map(|s| {
                let layers = match &s.cold {
                    None => s.layers.as_slice(),
                    Some(cold) => cold.thawed.get_or_init(|| self.layers_of(s)).as_slice(),
                };
                ("Opened", layers)
            }),
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
        self.held.clear();
        self.doc_tiles.clear();
        self.resident = 0;
        self.swap.close();
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
    fn dpi_comes_back_through_undo_and_redo() {
        let mut doc = Document::new(10, 10);
        doc.dpi = 300;
        let mut history = History::new(10);
        history.push_labelled(&doc, "Resolution");
        doc.dpi = 600;
        assert!(history.undo(&mut doc));
        assert_eq!(doc.dpi, 300);
        assert!(history.redo(&mut doc));
        assert_eq!(doc.dpi, 600);
    }

    #[test]
    fn rollback_leaves_nothing_to_redo() {
        let mut doc = Document::new(10, 10);
        let mut history = History::new(10);
        history.push_labelled(&doc, "Pen");
        doc.find_mut(doc.active).unwrap().name = "Stray".into();
        assert!(history.rollback(&mut doc));
        assert_eq!(doc.active_layer().unwrap().name, "Layer 1");
        assert!(!history.can_undo());
        assert!(!history.can_redo());
        assert!(!history.rollback(&mut doc));
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
mod budget_tests {
    use super::*;
    use crate::layer::LayerId;

    const MIB: usize = 1024 * 1024;

    /// A swap folder of the test's own, removed when done.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("painter-history-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }

        fn files(&self) -> usize {
            std::fs::read_dir(&self.0).map_or(0, |d| d.count())
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Pixels that differ with every step and every tile.
    fn paint(doc: &mut Document, id: LayerId, step: u32) {
        let (w, h) = doc.canvas();
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = (i as u32 % w, i as u32 / w);
            let v = (x / 8 + y / 8 + step * 7) as u8;
            px.copy_from_slice(&[v, v / 2, v / 3, 255]);
        }
        doc.find_mut(id).unwrap().kind = LayerKind::Raster(TileStore::from_rgba(w, h, &rgba));
    }

    fn pixels(doc: &Document, id: LayerId) -> Vec<(TileCoord, Vec<u8>)> {
        let layer = doc.layers.iter().find(|l| l.id == id).unwrap();
        let mut tiles: Vec<_> = layer.tiles().unwrap().iter().map(|(c, t)| (c, t.bytes().to_vec())).collect();
        tiles.sort_by_key(|(c, _)| *c);
        tiles
    }

    #[test]
    fn fifty_steps_stay_within_the_budget_and_all_undo() {
        let dir = TempDir::new("budget");
        // 512 × 512: 64 tiles, 1 MiB a step; 50 steps would hold 50 MiB.
        let mut doc = Document::new(512, 512);
        let id = doc.active;
        let budget = 5 * MIB;
        let mut history = History::with_ram_budget(100, budget, &dir.0);
        let mut states = Vec::new();
        for step in 0..50 {
            states.push(pixels(&doc, id));
            history.push_labelled(&doc, &format!("Step {step}"));
            assert!(history.memory_bytes() <= budget, "step {step}: {} bytes", history.memory_bytes());
            paint(&mut doc, id, step);
        }
        let last = pixels(&doc, id);
        assert!(history.swap_bytes() > 0, "older steps went to disk");
        assert_eq!(dir.files(), 1);

        for step in (0..50).rev() {
            assert!(history.undo(&mut doc));
            assert!(history.memory_bytes() <= budget, "undo to {step}: {} bytes", history.memory_bytes());
            assert!(pixels(&doc, id) == states[step], "undo to step {step} restores its pixels");
        }
        assert!(!history.can_undo());
        assert_eq!(history.done().count(), 0);
        assert_eq!(history.undone().next(), Some("Step 0"));

        // And all the way back.
        for (step, want) in states.iter().skip(1).chain([&last]).enumerate() {
            assert!(history.redo(&mut doc));
            assert!(history.memory_bytes() <= budget, "redo {step}: {} bytes", history.memory_bytes());
            assert!(pixels(&doc, id) == *want, "redo {step} restores its pixels");
        }
        assert!(!history.can_redo());

        // Forgetting the steps frees the disk; dropping the history deletes the file.
        history.clear();
        assert_eq!(history.swap_bytes(), 0);
        drop(history);
        assert_eq!(dir.files(), 0);
    }

    #[test]
    fn tiles_the_document_shows_stay_in_ram_until_painted_over() {
        let dir = TempDir::new("pinned");
        let mut doc = Document::new(256, 256);
        let id = doc.active;
        paint(&mut doc, id, 0);
        let opened = pixels(&doc, id);
        // A budget of nothing: every step but the newest pages out.
        let mut history = History::with_ram_budget(10, 0, &dir.0);
        history.push_labelled(&doc, "Rename");
        doc.find_mut(id).unwrap().name = "Inks".into();
        history.push_labelled(&doc, "Rename again");
        doc.find_mut(id).unwrap().name = "Lines".into();
        // The steps only share the document's tiles: nothing is written.
        assert_eq!(history.swap_bytes(), 0);
        assert_eq!(history.memory_bytes(), 0);

        history.push_labelled(&doc, "Paint");
        paint(&mut doc, id, 1);
        history.push_labelled(&doc, "Paint again");
        // The tiles painted over now live only in the history: on disk.
        assert!(history.swap_bytes() > 0);
        assert_eq!(history.memory_bytes(), 0);

        // The history brush reads the oldest step back from disk.
        let (_, layers) = history.source().unwrap();
        let tiles = layers.iter().find(|l| l.id == id).unwrap().tiles().unwrap();
        assert_eq!(tiles.len(), opened.len());
        assert!(opened.iter().all(|(c, bytes)| tiles.get(*c).unwrap().bytes() == bytes.as_slice()));
        assert!(history.state_layers(0, &doc).is_some());

        while history.undo(&mut doc) {}
        assert!(pixels(&doc, id) == opened);
        assert_eq!(doc.layers.iter().find(|l| l.id == id).unwrap().name, "Layer 1");
    }

    #[test]
    fn rollback_and_forgotten_steps_free_their_pages() {
        let dir = TempDir::new("forget");
        let mut doc = Document::new(128, 128);
        let id = doc.active;
        let mut history = History::with_ram_budget(3, 0, &dir.0);
        for step in 0..6 {
            history.push_labelled(&doc, "Paint");
            paint(&mut doc, id, step);
        }
        let before = pixels(&doc, id);
        history.push_labelled(&doc, "Stray");
        paint(&mut doc, id, 99);
        assert!(history.rollback(&mut doc));
        assert!(pixels(&doc, id) == before);
        // Fewer steps, fewer pages.
        let swapped = history.swap_bytes();
        history.set_limit(1);
        history.push_labelled(&doc, "Paint");
        assert!(history.swap_bytes() < swapped);
        assert_eq!(history.done().count(), 1);
    }

    #[test]
    fn edits_to_past_states_reach_paged_out_steps() {
        let dir = TempDir::new("edit");
        let mut doc = Document::new(128, 128);
        let id = doc.active;
        let mut history = History::with_ram_budget(10, 0, &dir.0);
        for step in 0..4 {
            history.push_labelled(&doc, "Paint");
            paint(&mut doc, id, step);
        }
        let mut states = Vec::new();
        history.edit_snapshots(|layers| {
            for l in layers.iter_mut().filter(|l| l.id == id) {
                l.name = "Shared".into();
                states.push(l.tiles().unwrap().len());
            }
        });
        // Every step's tiles were back in RAM for `f` (the first had none yet).
        assert_eq!(states, [0, 4, 4, 4]);
        while history.undo(&mut doc) {
            assert_eq!(doc.layers.iter().find(|l| l.id == id).unwrap().name, "Shared");
        }
    }

    #[test]
    fn no_budget_keeps_everything_in_ram() {
        let mut doc = Document::new(128, 128);
        let id = doc.active;
        let mut history = History::new(10);
        assert_eq!(history.ram_budget(), None);
        for step in 0..3 {
            history.push_labelled(&doc, "Paint");
            paint(&mut doc, id, step);
        }
        history.push_labelled(&doc, "Paint");
        assert_eq!(history.swap_bytes(), 0);
        // Two replaced versions of four tiles (the layer started blank).
        assert_eq!(history.memory_bytes(), 2 * 4 * TILE_BYTES);
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
