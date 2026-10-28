//! Open documents. The active one lives in `PainterApp`'s own fields (so
//! every tool works on it unchanged); the others are parked here as
//! [`DocSession`]s and swapped in when their tab is chosen. Only the active
//! document is on the GPU: parking frees its tiles' GPU memory, and the
//! render pipelines, shaders and brush caches stay shared.
//!
//! Opening and saving run on worker threads, so a heavy file doesn't stall
//! the canvas. A save works on a copy of the document (tiles are shared,
//! so the copy is cheap), and editing can go on meanwhile.
//!
//! A tab left alone for a while, or any parked tab once the open documents
//! take too much memory, is suspended: its pixels and undo history go to the
//! history's compressed swap file on a worker thread, and only the layer
//! tree, names, a small thumbnail and the unsaved-changes flag stay in RAM.
//! Choosing the tab brings it back on a worker thread, then shows it.

use std::collections::HashMap;
use crate::i18n::tr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use comic_layout::{ComicPage, PanelId};
use eframe::egui;
use painter_core::{io, project, psd, Document, History, Layer, LayerId, LayerKind, TileStore, TILE_BYTES, TILE_SIZE};

use super::split_viewport::ReferenceView;
use crate::app::{file_stem, new_history, PainterApp};
use crate::scene_ui::{SceneLayer, SceneSnapshot};
use crate::view::ViewTransform;

/// A document that isn't the one being edited.
pub(crate) struct DocSession {
    pub(crate) doc: Document,
    pub(crate) history: History,
    pub(crate) view: ViewTransform,
    pub(crate) comic: Option<ComicPage>,
    pub(crate) comic_undo: Vec<Option<ComicPage>>,
    pub(crate) comic_redo: Vec<Option<ComicPage>>,
    pub(crate) selected_panel: Option<PanelId>,
    pub(crate) scenes: HashMap<LayerId, SceneLayer>,
    pub(crate) scene_undo: Vec<SceneSnapshot>,
    pub(crate) scene_redo: Vec<SceneSnapshot>,
    pub(crate) file_path: Option<PathBuf>,
    pub(crate) dirty: bool,
    pub(crate) edit_mask: bool,
    pub(crate) mask_view_mode: painter_render::MaskViewMode,
    /// When it was parked (or last brought back), for suspending tabs left alone.
    pub(crate) parked_at: Instant,
    /// Whether its pixels and undo history are in RAM.
    pub(crate) residency: Residency,
    /// A small picture of it, taken when it was suspended.
    pub(crate) thumbnail: Option<Thumbnail>,
}

/// Where a parked document's pixels and undo history are.
pub(crate) enum Residency {
    /// In RAM.
    Resident,
    /// Going to disk or coming back on a worker thread. Meanwhile the
    /// session's `doc` is a stand-in without pixels (same layers, sizes and
    /// names) and its `history` an empty one.
    Paging(Receiver<Paged>),
    /// On disk (see [`History::suspend`]); `doc` has its layers but no pixels.
    Suspended,
}

/// What paging a document out or in on a worker thread gives back.
pub(crate) struct Paged {
    pub(crate) doc: Document,
    pub(crate) history: History,
    /// Whether it ended up on disk (false: back in RAM, or the disk failed).
    pub(crate) suspended: bool,
    /// Taken before paging out.
    pub(crate) thumbnail: Option<egui::ColorImage>,
}

/// A suspended tab's picture: made on a worker thread, uploaded when first shown.
pub(crate) enum Thumbnail {
    Image(egui::ColorImage),
    Texture(egui::TextureHandle),
}

/// The longest side of a suspended tab's thumbnail.
pub(crate) const THUMBNAIL_SIZE: u32 = 256;

/// A tab parked for less than this stays in RAM even under memory pressure,
/// so flipping between tabs doesn't page them out and in.
const MIN_PARKED: Duration = Duration::from_secs(10);

/// When background tabs are suspended to disk.
pub(crate) struct SuspendPolicy {
    /// A tab parked this long is suspended (`None`: not just for being idle).
    pub(crate) idle: Option<Duration>,
    /// Above this much memory for the open documents' pixels and undo steps,
    /// the tabs parked longest are suspended until it fits (`None`: no bound).
    pub(crate) pressure_bytes: Option<usize>,
}

impl SuspendPolicy {
    /// After 5 minutes, or past 2 GiB for all documents. PAINTER_TAB_SUSPEND_MIN
    /// sets the minutes (0: never for being idle) and PAINTER_TAB_RAM_MB the
    /// memory (0: no bound); PAINTER_LOW_RAM=1 makes them 1 minute and 512 MiB.
    /// Without those, the performance profile's (see `crate::hardware::RamBudget`).
    pub(crate) fn from_env() -> Self {
        let budget = crate::hardware::RamBudget::current();
        let (minutes, mib) = (budget.idle_minutes, budget.tabs_mib);
        let number = |name: &str, default: u64| std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default);
        let minutes = number("PAINTER_TAB_SUSPEND_MIN", minutes);
        let mib = number("PAINTER_TAB_RAM_MB", mib) as usize;
        Self {
            idle: (minutes > 0).then(|| Duration::from_secs(minutes * 60)),
            pressure_bytes: (mib > 0).then(|| mib * 1024 * 1024),
        }
    }
}

impl DocSession {
    /// Whether its pixels and undo history are in RAM, to be shown or changed.
    pub(crate) fn resident(&self) -> bool {
        matches!(self.residency, Residency::Resident)
    }

    /// RAM its pixels and the tiles only its history holds take.
    fn memory_bytes(&self) -> usize {
        self.doc.tile_count() * TILE_BYTES + self.history.memory_bytes()
    }

    /// Starts paging out (`out`) or back in on a worker thread.
    fn start_paging(&mut self, out: bool) {
        let stand_in = without_pixels(&self.doc);
        let doc = std::mem::replace(&mut self.doc, stand_in);
        let history = std::mem::replace(&mut self.history, History::new(1));
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(if out { suspend_document(doc, history) } else { resume_document(doc, history) });
        });
        self.residency = Residency::Paging(rx);
    }

    /// Takes back what a worker thread paged out or in.
    fn land(&mut self, paged: Paged) {
        self.doc = paged.doc;
        self.history = paged.history;
        if paged.suspended {
            self.residency = Residency::Suspended;
            self.thumbnail = paged.thumbnail.map(Thumbnail::Image);
        } else {
            self.residency = Residency::Resident;
            self.thumbnail = None;
            // Back for a reason: not to be suspended again at once.
            self.parked_at = Instant::now();
        }
    }

    /// Brings the pixels and history back into RAM on this thread, waiting
    /// for a worker that has them. For when the document is needed now.
    pub(crate) fn wake_now(&mut self) {
        if let Residency::Paging(rx) = &self.residency {
            match rx.recv() {
                Ok(paged) => self.land(paged),
                // The worker died with them: what's left is the stand-in.
                Err(_) => self.residency = Residency::Resident,
            }
        }
        if let Residency::Suspended = self.residency {
            self.history.resume(&mut self.doc);
            self.residency = Residency::Resident;
            self.thumbnail = None;
            self.parked_at = Instant::now();
        }
    }
}

/// Pages a parked document and its history out to disk (on a worker thread),
/// after taking its thumbnail. If the disk fails it all stays in RAM.
pub(crate) fn suspend_document(mut doc: Document, mut history: History) -> Paged {
    let thumbnail = thumbnail(&doc, THUMBNAIL_SIZE);
    let suspended = history.suspend(&mut doc);
    if !suspended {
        history.resume(&mut doc);
    }
    Paged { doc, history, suspended, thumbnail: Some(thumbnail) }
}

/// Brings a document suspended by [`suspend_document`] back (on a worker thread).
pub(crate) fn resume_document(mut doc: Document, mut history: History) -> Paged {
    history.resume(&mut doc);
    Paged { doc, history, suspended: false, thumbnail: None }
}

/// `doc` with its layers, sizes and names but no pixels: what a tab shows
/// while its document is paged out or in.
fn without_pixels(doc: &Document) -> Document {
    fn clear(layers: &mut [Layer]) {
        for layer in layers {
            match &mut layer.kind {
                LayerKind::Raster(tiles) => *tiles = TileStore::new(),
                LayerKind::Vector(vector) => vector.cache = TileStore::new(),
                LayerKind::Folder(folder) => clear(&mut folder.children),
                _ => {}
            }
            if let Some(mask) = &mut layer.mask {
                mask.tiles = TileStore::new();
            }
        }
    }
    let mut out = doc.clone();
    clear(&mut out.layers);
    if let Some(selection) = &mut out.selection {
        *selection = TileStore::new();
    }
    for channel in &mut out.channels {
        channel.mask = TileStore::new();
    }
    out
}

/// A picture of `doc` at most `max` pixels on a side, averaged in square
/// blocks. It's flattened a band at a time, so a large canvas never needs a
/// full-size copy in RAM.
pub(crate) fn thumbnail(doc: &Document, max: u32) -> egui::ColorImage {
    let (w, h) = doc.canvas();
    let step = w.max(h).div_ceil(max.max(1)).max(1);
    let (ow, oh) = (w.div_ceil(step) as usize, h.div_ceil(step) as usize);
    let mut pixels = vec![egui::Color32::TRANSPARENT; ow * oh];
    // Whole rows of blocks, about a tile high.
    let band = step * TILE_SIZE.div_ceil(step);
    let mut sums = vec![[0u32; 5]; ow];
    for y0 in (0..h).step_by(band as usize) {
        let bh = band.min(h - y0);
        let flat = io::flatten_region(doc, 0, y0, w, bh);
        for oy in 0..bh.div_ceil(step) {
            sums.iter_mut().for_each(|s| *s = [0; 5]);
            for y in oy * step..((oy + 1) * step).min(bh) {
                let row = &flat[(y * w * 4) as usize..((y + 1) * w * 4) as usize];
                for (x, px) in row.as_chunks::<4>().0.iter().enumerate() {
                    let s = &mut sums[x / step as usize];
                    for c in 0..4 {
                        s[c] += px[c] as u32;
                    }
                    s[4] += 1;
                }
            }
            let out_row = ((y0 / step + oy) as usize) * ow;
            for (ox, s) in sums.iter().enumerate() {
                let n = s[4].max(1);
                pixels[out_row + ox] =
                    egui::Color32::from_rgba_premultiplied((s[0] / n) as u8, (s[1] / n) as u8, (s[2] / n) as u8, (s[3] / n) as u8);
            }
        }
    }
    egui::ColorImage { size: [ow, oh], pixels }
}

/// A document tab.
pub(crate) struct Slot {
    pub(crate) id: u64,
    /// `None` for the active document (its state is in the app).
    pub(crate) parked: Option<DocSession>,
    /// "Untitled N" numbering.
    pub(crate) untitled: u32,
    /// Name of the file an unsaved document came from (an imported image).
    pub(crate) name: Option<String>,
    /// Bumped when a parked document changes (layer transfers), for previews.
    pub(crate) revision: u64,
}

/// What opening a file on a worker thread gives back.
pub(crate) type Loaded = Result<(Document, bool, project::Extras), String>;

struct PendingLoad {
    path: PathBuf,
    rx: Receiver<Loaded>,
}

/// A failed save hands its copy back, so a closed tab can be reopened.
/// Ok: what the format couldn't keep (always empty for projects).
type Saved = Result<Vec<String>, (String, Document, project::Extras)>;

/// Extras entry carrying what opening a foreign file couldn't keep as is, one per line.
pub(crate) const OPEN_WARNINGS: &str = "painter/open-warnings";

struct PendingSave {
    slot: u64,
    path: PathBuf,
    rx: Receiver<Saved>,
}

/// Tab-close and quit confirmations.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClosePrompt {
    /// Closing one tab with unsaved changes.
    Tab(u64),
    /// Quitting with unsaved documents.
    Quit,
}

pub(crate) struct DocManager {
    pub(crate) slots: Vec<Slot>,
    pub(crate) active: usize,
    next_id: u64,
    next_untitled: u32,
    loads: Vec<PendingLoad>,
    saves: Vec<PendingSave>,
    pub(crate) prompt: Option<ClosePrompt>,
    /// Quit once the saves started by "Save all" finish.
    pub(crate) quit_after_saves: bool,
    pub(crate) reference: ReferenceView,
    /// Ctrl+Tab presses waiting to be applied (negative: backwards).
    pub(crate) pending_cycle: i32,
    /// A suspended tab that was chosen: shown once it's back in RAM.
    pub(crate) waking: Option<u64>,
    /// When background tabs go to disk.
    pub(crate) suspend: SuspendPolicy,
}

impl Default for DocManager {
    fn default() -> Self {
        Self {
            slots: vec![Slot { id: 1, parked: None, untitled: 1, name: None, revision: 0 }],
            active: 0,
            next_id: 2,
            next_untitled: 2,
            loads: Vec::new(),
            saves: Vec::new(),
            prompt: None,
            quit_after_saves: false,
            reference: ReferenceView::default(),
            pending_cycle: 0,
            waking: None,
            suspend: SuspendPolicy::from_env(),
        }
    }
}

impl DocManager {
    pub(crate) fn active_id(&self) -> u64 {
        self.slots[self.active].id
    }

    pub(crate) fn index_of(&self, id: u64) -> Option<usize> {
        self.slots.iter().position(|s| s.id == id)
    }

    pub(crate) fn loading(&self) -> impl Iterator<Item = &Path> {
        self.loads.iter().map(|l| l.path.as_path())
    }

    pub(crate) fn busy(&self) -> bool {
        !self.loads.is_empty() || !self.saves.is_empty() || self.paging()
    }

    /// Whether a tab is going to disk or coming back.
    fn paging(&self) -> bool {
        self.slots.iter().filter_map(|s| s.parked.as_ref()).any(|s| matches!(s.residency, Residency::Paging(_)))
    }

    /// Whether tab `index` is parked with its pixels on disk (or on their way).
    pub(crate) fn is_suspended(&self, index: usize) -> bool {
        self.slots.get(index).and_then(|s| s.parked.as_ref()).is_some_and(|s| !s.resident())
    }

    pub(crate) fn saving(&self) -> bool {
        !self.saves.is_empty()
    }

    fn new_slot(&mut self) -> Slot {
        let slot = Slot { id: self.next_id, parked: None, untitled: self.next_untitled, name: None, revision: 0 };
        self.next_id += 1;
        self.next_untitled += 1;
        slot
    }
}

/// Reads a file the way File → Open does (on any thread).
pub(crate) fn load_file(path: &Path) -> Loaded {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    match ext.as_str() {
        "clip" => painter_core::clip::load(path).map(|(doc, warnings)| {
            let mut extras = project::Extras::new();
            if !warnings.is_empty() {
                extras.insert(OPEN_WARNINGS.into(), warnings.join("
").into_bytes());
            }
            (doc, false, extras)
        }),
        "kra" | "ora" => {
            let read = if ext == "kra" { painter_core::kra::load(path) } else { painter_core::ora::load(path) };
            read.map(|(doc, warnings)| {
                let mut extras = project::Extras::new();
                if !warnings.is_empty() {
                    extras.insert(OPEN_WARNINGS.into(), warnings.join("\n").into_bytes());
                }
                // Saving writes the same format back.
                (doc, true, extras)
            })
        }
        "psd" => psd::load(path).map(|d| (d, false, project::Extras::new())).map_err(|e| e.to_string()),
        "png" | "jpg" | "jpeg" => io::import_png(path)
            .map(|image| {
                let mut doc = Document::new(image.width, image.height);
                // No pHYs: the usual screen assumption.
                doc.dpi = image.dpi.unwrap_or(72);
                if let Some(layer) = doc.active_layer_mut() {
                    layer.name = file_stem(path);
                    if let Some(tiles) = layer.tiles_mut() {
                        *tiles = image.tiles;
                    }
                }
                (doc, false, project::Extras::new())
            })
            .map_err(|e| e.to_string()),
        _ => project::load_with(path).map(|(d, extras)| (d, true, extras)).map_err(|e| e.to_string()),
    }
}

impl PainterApp {
    // ---- parking ------------------------------------------------------------------------

    /// Moves the active document's state out of the app.
    fn take_session(&mut self) -> DocSession {
        DocSession {
            doc: std::mem::replace(&mut self.doc, Document::new(1, 1)),
            history: std::mem::replace(&mut self.history, new_history()),
            view: std::mem::take(&mut self.view),
            comic: self.comic.take(),
            comic_undo: std::mem::take(&mut self.comic_undo),
            comic_redo: std::mem::take(&mut self.comic_redo),
            selected_panel: self.selected_panel.take(),
            scenes: std::mem::take(&mut self.scenes),
            scene_undo: std::mem::take(&mut self.scene_undo),
            scene_redo: std::mem::take(&mut self.scene_redo),
            file_path: self.file_path.take(),
            dirty: std::mem::replace(&mut self.dirty, false),
            edit_mask: std::mem::replace(&mut self.edit_mask, false),
            mask_view_mode: std::mem::take(&mut self.mask_view_mode),
            parked_at: Instant::now(),
            residency: Residency::Resident,
            thumbnail: None,
        }
    }

    /// Makes `s` the active document, with its own view.
    fn put_session(&mut self, mut s: DocSession) {
        s.wake_now();
        self.doc = s.doc;
        self.history = s.history;
        self.view = s.view;
        self.comic = s.comic;
        self.comic_undo = s.comic_undo;
        self.comic_redo = s.comic_redo;
        self.selected_panel = s.selected_panel;
        self.scenes = s.scenes;
        self.scene_undo = s.scene_undo;
        self.scene_redo = s.scene_redo;
        self.file_path = s.file_path;
        self.dirty = s.dirty;
        self.edit_mask = s.edit_mask;
        self.mask_view_mode = s.mask_view_mode;
        self.stroke = None;
        self.model_paint = None;
        self.drag = None;
        self.scene_drag = None;
        self.selected_ruler = None;
        self.ruler_drag = None;
        self.epoch += 1;
        self.resync_gpu();
        // Layer ids repeat across documents: draw this one's 3D layers afresh.
        self.refresh_scene_previews();
    }

    /// Finishes what's in progress on the active document before it's parked.
    /// `false` while a stroke is being drawn.
    fn settle_active(&mut self) -> bool {
        if self.stroke.is_some() {
            self.status = tr("Finish the stroke first").into();
            return false;
        }
        if self.transform.is_some() {
            self.apply_transform();
        }
        self.crop = None;
        self.frame.cancel();
        // The paint layer belongs to this tab's model.
        self.stop_model_paint();
        self.commit_scenes(true);
        true
    }

    /// Shows tab `index` now: a suspended one is brought back on this thread
    /// (see [`PainterApp::show_document`] for doing it in the background).
    pub(crate) fn switch_document(&mut self, index: usize) {
        if index == self.docs.active || index >= self.docs.slots.len() || !self.settle_active() {
            return;
        }
        if self.docs.waking == Some(self.docs.slots[index].id) {
            self.docs.waking = None;
        }
        let current = self.take_session();
        let from = self.docs.active;
        self.docs.slots[from].parked = Some(current);
        self.docs.slots[from].revision += 1;
        let next = self.docs.slots[index].parked.take().expect("inactive tabs are parked");
        self.docs.active = index;
        self.put_session(next);
        // The transform tool shouldn't lift pixels of the new document by itself.
        self.transform_blocked = true;
    }

    /// Shows tab `index`; a suspended one first comes back from disk on a
    /// worker thread, without holding up the canvas.
    pub(crate) fn show_document(&mut self, index: usize) {
        if index == self.docs.active || index >= self.docs.slots.len() {
            return;
        }
        if !self.docs.is_suspended(index) {
            self.docs.waking = None;
            self.switch_document(index);
            return;
        }
        self.docs.waking = Some(self.docs.slots[index].id);
        self.wake_document(index);
        self.status = format!("Restoring {}...", self.tab_title(index));
    }

    /// Starts bringing parked tab `index` back into RAM, if suspended.
    pub(crate) fn wake_document(&mut self, index: usize) {
        let Some(session) = self.docs.slots.get_mut(index).and_then(|s| s.parked.as_mut()) else { return };
        if let Residency::Suspended = session.residency {
            session.start_paging(false);
        }
    }

    pub(crate) fn cycle_document(&mut self, step: i32) {
        let n = self.docs.slots.len() as i32;
        if n > 1 {
            self.show_document((self.docs.active as i32 + step).rem_euclid(n) as usize);
        }
    }

    /// Lands finished paging, shows a tab chosen while suspended once it's
    /// back, and suspends the tabs the policy says to. Called once per frame.
    fn poll_suspension(&mut self, ctx: &egui::Context) {
        let mut lost = false;
        for slot in &mut self.docs.slots {
            let Some(session) = slot.parked.as_mut() else { continue };
            let Residency::Paging(rx) = &session.residency else { continue };
            match rx.try_recv() {
                Ok(paged) => session.land(paged),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    session.residency = Residency::Resident;
                    lost = true;
                }
            }
        }
        if lost {
            self.status = tr("A background tab couldn't be paged: its pixels were lost").into();
        }
        if let Some(id) = self.docs.waking {
            match self.docs.index_of(id) {
                Some(i) if i != self.docs.active => {
                    let session = self.docs.slots[i].parked.as_ref().expect("inactive tabs are parked");
                    match session.residency {
                        Residency::Resident => self.switch_document(i),
                        // Chosen while still going to disk: bring it straight back.
                        Residency::Suspended => self.wake_document(i),
                        Residency::Paging(_) => {}
                    }
                }
                _ => self.docs.waking = None,
            }
        }
        if let Some(wait) = self.suspend_background_tabs() {
            ctx.request_repaint_after(wait);
        }
    }

    /// Suspends the parked tabs left alone past the policy's idle time and,
    /// while the documents take more memory than it allows, the ones parked
    /// longest. Returns when to look again.
    fn suspend_background_tabs(&mut self) -> Option<Duration> {
        let now = Instant::now();
        let waking = self.docs.waking;
        let mut parked: Vec<(Instant, usize)> = (self.docs.slots.iter().enumerate())
            .filter(|(_, s)| Some(s.id) != waking)
            .filter_map(|(i, s)| s.parked.as_ref().filter(|p| p.resident()).map(|p| (p.parked_at, i)))
            .collect();
        parked.sort();
        let mut memory = self.doc.tile_count() * TILE_BYTES
            + self.history.memory_bytes()
            + self.docs.slots.iter().filter_map(|s| s.parked.as_ref()).map(DocSession::memory_bytes).sum::<usize>();
        let policy = &self.docs.suspend;
        let mut next: Option<Duration> = None;
        let mut go = Vec::new();
        for (at, i) in parked {
            let age = now.saturating_duration_since(at);
            let idle = policy.idle.is_some_and(|d| age >= d);
            let pressed = policy.pressure_bytes.is_some_and(|b| memory > b);
            let wait = if (idle || pressed) && age >= MIN_PARKED {
                memory = memory.saturating_sub(self.docs.slots[i].parked.as_ref().map_or(0, DocSession::memory_bytes));
                go.push(i);
                None
            } else if pressed {
                Some(MIN_PARKED - age)
            } else {
                policy.idle.map(|d| d.saturating_sub(age))
            };
            next = match (next, wait) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
        for i in go {
            if let Some(session) = self.docs.slots[i].parked.as_mut() {
                session.start_paging(true);
            }
        }
        next
    }

    /// An untitled, unchanged document: replacing it loses nothing.
    pub(crate) fn active_is_pristine(&self) -> bool {
        self.file_path.is_none() && !self.dirty && !self.history.can_undo() && self.comic.is_none() && self.scenes.is_empty()
    }

    /// Makes room for a new document: a new tab after the current one,
    /// unless the current document is an untouched blank one.
    pub(crate) fn open_tab_for_new_document(&mut self) -> bool {
        if self.active_is_pristine() {
            return true;
        }
        if !self.settle_active() {
            return false;
        }
        let current = self.take_session();
        let from = self.docs.active;
        self.docs.slots[from].parked = Some(current);
        self.docs.slots[from].revision += 1;
        let slot = self.docs.new_slot();
        self.docs.slots.insert(from + 1, slot);
        self.docs.active = from + 1;
        true
    }

    // ---- closing ------------------------------------------------------------------------

    /// Closes a tab, asking first if it has unsaved changes.
    pub(crate) fn request_close_tab(&mut self, id: u64) {
        let Some(i) = self.docs.index_of(id) else { return };
        if self.slot_dirty(i) {
            self.docs.prompt = Some(ClosePrompt::Tab(id));
        } else {
            self.close_tab_now(id);
        }
    }

    pub(crate) fn slot_dirty(&self, i: usize) -> bool {
        match &self.docs.slots[i].parked {
            Some(s) => s.dirty,
            None => self.dirty,
        }
    }

    pub(crate) fn dirty_count(&self) -> usize {
        (0..self.docs.slots.len()).filter(|&i| self.slot_dirty(i)).count()
    }

    /// Closes without asking. The last tab becomes a fresh blank canvas.
    pub(crate) fn close_tab_now(&mut self, id: u64) {
        let Some(i) = self.docs.index_of(id) else { return };
        if self.docs.slots.len() == 1 {
            let size = self.doc.canvas();
            let dpi = self.doc.dpi;
            let mut doc = Document::new(size.0, size.1);
            doc.dpi = dpi;
            self.stroke = None;
            self.set_document_here(doc, None);
            let slot = self.docs.new_slot();
            self.docs.slots[0] = slot;
            return;
        }
        if i == self.docs.active {
            self.stroke = None;
            let neighbour = if i + 1 < self.docs.slots.len() { i + 1 } else { i - 1 };
            // Drop the active state rather than parking it.
            self.transform = None;
            self.crop = None;
            let next = self.docs.slots[neighbour].parked.take().expect("inactive tabs are parked");
            self.put_session(next);
            self.docs.slots.remove(i);
            self.docs.active = if neighbour > i { neighbour - 1 } else { neighbour };
        } else {
            self.docs.slots.remove(i);
            if i < self.docs.active {
                self.docs.active -= 1;
            }
        }
    }

    pub(crate) fn close_all_documents(&mut self) {
        if self.dirty_count() > 0 {
            self.status = tr("Some documents have unsaved changes: save them or close their tabs one by one").into();
            let first = (0..self.docs.slots.len()).find(|&i| self.slot_dirty(i)).unwrap_or(0);
            let id = self.docs.slots[first].id;
            self.switch_document(first);
            self.docs.prompt = Some(ClosePrompt::Tab(id));
            return;
        }
        let ids: Vec<u64> = self.docs.slots.iter().map(|s| s.id).collect();
        for id in ids {
            self.close_tab_now(id);
        }
    }

    /// Saves every document with changes (untitled ones ask for a name).
    /// Returns false if one was left unsaved (a dialog was cancelled).
    pub(crate) fn save_all(&mut self) -> bool {
        let start = self.docs.active;
        let mut all = true;
        for i in 0..self.docs.slots.len() {
            if !self.slot_dirty(i) {
                continue;
            }
            self.switch_document(i);
            if self.docs.active != i {
                all = false;
                continue;
            }
            self.save();
            all &= !self.dirty;
        }
        self.switch_document(start.min(self.docs.slots.len() - 1));
        all
    }

    // ---- background open and save ---------------------------------------------------

    /// File → Open and dropped files: loads on a worker thread; the document
    /// gets its own tab when it arrives.
    pub(crate) fn open_path(&mut self, path: PathBuf) {
        if let Some(i) = self.docs.slots.iter().position(|s| {
            let p = match &s.parked {
                Some(p) => p.file_path.as_ref(),
                None => self.file_path.as_ref(),
            };
            p == Some(&path)
        }) {
            self.show_document(i);
            self.status = format!("{} is already open", path.display());
            return;
        }
        let (tx, rx) = channel();
        let job = path.clone();
        std::thread::spawn(move || {
            let _ = tx.send(load_file(&job));
        });
        self.status = format!("Opening {}...", path.display());
        self.docs.loads.push(PendingLoad { path, rx });
    }

    /// Writes the project on a worker thread from a copy of the document.
    pub(crate) fn save_in_background(&mut self, path: PathBuf, extras: project::Extras) {
        let doc = self.doc.clone();
        let (tx, rx) = channel();
        let job = path.clone();
        std::thread::spawn(move || {
            let result = save_by_extension(&doc, &extras, &job).map_err(|e| (e.to_string(), doc, extras));
            let _ = tx.send(result);
        });
        self.docs.saves.push(PendingSave { slot: self.docs.active_id(), path: path.clone(), rx });
        self.status = format!("Saving {}...", path.display());
        self.file_path = Some(path);
        self.dirty = false;
    }

    /// Collects finished loads and saves. Called once per frame.
    pub(crate) fn poll_documents(&mut self, ctx: &egui::Context) {
        let mut done = Vec::new();
        self.docs.loads.retain(|l| match l.rx.try_recv() {
            Ok(result) => {
                done.push((l.path.clone(), result));
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => {
                done.push((l.path.clone(), Err("the loader stopped".into())));
                false
            }
        });
        for (path, result) in done {
            self.finish_open(path, result);
        }

        let mut saved = Vec::new();
        self.docs.saves.retain(|s| match s.rx.try_recv() {
            Ok(result) => {
                saved.push((s.slot, s.path.clone(), result));
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => false,
        });
        for (slot, path, result) in saved {
            match result {
                Ok(notes) => {
                    self.status = if notes.is_empty() {
                        format!("Saved {}", path.display())
                    } else {
                        format!("Saved {} — {}", path.display(), summarize(&notes))
                    };
                    // iPad Save As → "Save and copy to...": now the file is complete.
                    if self.export_after_save.as_ref() == Some(&path) {
                        self.export_after_save = None;
                        crate::files::offer_export(&path);
                    }
                }
                Err((err, doc, extras)) => {
                    self.status = format!("Save failed: {err}");
                    match self.docs.index_of(slot) {
                        Some(i) if i == self.docs.active => self.dirty = true,
                        Some(i) => {
                            if let Some(s) = &mut self.docs.slots[i].parked {
                                s.dirty = true;
                            }
                        }
                        // The tab was closed meanwhile: bring the document back.
                        None => {
                            self.finish_open(path, Ok((doc, true, extras)));
                            self.dirty = true;
                        }
                    }
                    self.docs.quit_after_saves = false;
                }
            }
        }
        if self.docs.quit_after_saves && !self.docs.saving() && self.dirty_count() == 0 {
            self.allow_quit(ctx);
        }
        self.poll_suspension(ctx);
        self.poll_reference_previews(ctx);
        if self.docs.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    /// A file arrived from a worker thread: give it a tab.
    pub(crate) fn finish_open(&mut self, path: PathBuf, result: Loaded) {
        match result {
            Ok((doc, is_project, extras)) => {
                // Every opened file is a recent one (projects also through their path).
                if !is_project {
                    self.recent.touch(&path, &doc);
                }
                self.set_document(doc, is_project.then(|| path.clone()));
                if !is_project {
                    let active = self.docs.active;
                    self.docs.slots[active].name = path.file_name().map(|n| n.to_string_lossy().into_owned());
                }
                self.status = format!("Opened {}", path.display());
                self.apply_project_extras(&path, &extras);
                if let Some(notes) = extras.get(OPEN_WARNINGS) {
                    let notes: Vec<String> = String::from_utf8_lossy(notes).lines().map(str::to_string).collect();
                    self.status = format!("Opened {} — {}", path.display(), summarize(&notes));
                }
            }
            Err(err) => self.status = format!("Couldn't open {}: {err}", path.display()),
        }
    }

    /// The memory the open documents' tiles take, all tabs together.
    pub(crate) fn documents_memory(&self) -> usize {
        let parked: usize = self.docs.slots.iter().filter_map(|s| s.parked.as_ref()).map(|s| s.doc.tile_count()).sum();
        (parked + self.doc.tile_count()) * painter_core::TILE_BYTES
    }
}

/// Writes a project, or a Krita / OpenRaster file when the name says so.
fn save_by_extension(doc: &Document, extras: &project::Extras, path: &Path) -> Result<Vec<String>, String> {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("kra") => painter_core::kra::save(doc, path),
        Some("ora") => painter_core::ora::save(doc, path),
        _ => project::save_with(doc, extras, path).map(|()| Vec::new()).map_err(|e| e.to_string()),
    }
}

/// A status-bar line for a format's notes: the first two, and how many more.
pub(crate) fn summarize(notes: &[String]) -> String {
    let mut s = notes.iter().take(2).cloned().collect::<Vec<_>>().join("; ");
    if notes.len() > 2 {
        s += &format!(" (+{} more)", notes.len() - 2);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_file_reads_projects_and_images() {
        let dir = std::env::temp_dir().join(format!("painter-docs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut doc = Document::new(70, 40);
        doc.dpi = 350;
        let project_path = dir.join("a.painter");
        project::save(&doc, &project_path).unwrap();
        let (back, is_project, _) = load_file(&project_path).unwrap();
        assert!(is_project);
        assert_eq!((back.width, back.height, back.dpi), (70, 40, 350));

        let png = dir.join("b.png");
        io::export_png(&doc, &png).unwrap();
        let (img, is_project, _) = load_file(&png).unwrap();
        assert!(!is_project);
        assert_eq!((img.width, img.dpi), (70, 350), "resolution from pHYs");
        assert!(load_file(&dir.join("missing.painter")).is_err());

        // Krita and OpenRaster: saved by name, opened back as documents that save in place.
        for ext in ["kra", "ora"] {
            let path = dir.join(format!("c.{ext}"));
            assert!(save_by_extension(&doc, &project::Extras::new(), &path).unwrap().is_empty());
            let (back, saves_in_place, extras) = load_file(&path).unwrap();
            assert!(saves_in_place);
            assert!(!extras.contains_key(OPEN_WARNINGS));
            assert_eq!((back.width, back.height, back.dpi), (70, 40, 350), "{ext}");
            assert_eq!(crate::files::kind_of(&path), Some(crate::files::FileKind::Document));
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Every tile of the document, by layer and plane: pixels, masks, the
    /// selection and the saved channels.
    fn planes(doc: &Document) -> Vec<(String, painter_core::TileCoord, Vec<u8>)> {
        fn walk(layers: &[Layer], out: &mut Vec<(String, painter_core::TileCoord, Vec<u8>)>) {
            for layer in layers {
                let mut add = |plane: String, tiles: &TileStore| {
                    out.extend(tiles.iter().map(|(c, t)| (plane.clone(), c, t.bytes().to_vec())));
                };
                if let Some(tiles) = layer.tiles() {
                    add(format!("{} pixels", layer.id.0), tiles);
                }
                if let Some(mask) = &layer.mask {
                    add(format!("{} mask", layer.id.0), &mask.tiles);
                }
                if let LayerKind::Folder(folder) = &layer.kind {
                    walk(&folder.children, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&doc.layers, &mut out);
        let selection = doc.selection.iter().map(|s| ("selection".to_string(), s));
        let channels = doc.channels.iter().map(|c| (format!("channel {}", c.name), &c.mask));
        for (plane, tiles) in selection.chain(channels) {
            out.extend(tiles.iter().map(|(c, t)| (plane.clone(), c, t.bytes().to_vec())));
        }
        out.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        out
    }

    /// `w` × `h` of one colour, premultiplied, as tiles.
    fn solid(w: u32, h: u32, px: [u8; 4]) -> TileStore {
        TileStore::from_rgba(w, h, &px.repeat((w * h) as usize))
    }

    /// Waits for the worker thread and takes its result back.
    fn finish_paging(session: &mut DocSession) {
        let Residency::Paging(rx) = &session.residency else { panic!("not paging") };
        let paged = rx.recv().expect("the worker answers");
        session.land(paged);
    }

    #[test]
    fn park_suspend_resume_keeps_every_layer_mask_tile_and_undo_step() {
        let swap = std::env::temp_dir().join(format!("painter-tab-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&swap);
        let mut doc = Document::new(300, 200);
        let mut history = History::with_ram_budget(50, 1 << 20, &swap);
        let base = doc.active;
        // A folder with a masked layer inside, a selection and a saved channel.
        let (folder_id, inner) = (doc.alloc_id(), doc.alloc_id());
        let mut folder = Layer::folder(folder_id, "Inks");
        let mut child = Layer::raster(inner, "Lines");
        child.mask = Some(painter_core::Mask { tiles: solid(100, 100, [0, 0, 0, 128]), enabled: true });
        if let LayerKind::Folder(f) = &mut folder.kind {
            f.children.push(child);
        }
        doc.layers.push(folder);
        doc.channels.push(painter_core::SavedChannel { name: "Alpha 1".into(), mask: solid(64, 64, [0, 0, 0, 255]) });
        let mut states = Vec::new();
        for step in 0..8u8 {
            states.push(planes(&doc));
            history.push_labelled(&doc, &format!("Step {step}"));
            let px = [step * 20, 255 - step * 20, step * 7, 255];
            let target = if step % 2 == 0 { base } else { inner };
            doc.find_mut(target).unwrap().kind = LayerKind::Raster(solid(300, 200, px));
            doc.selection = Some(solid(64 * (1 + step as u32 % 3), 64, [0, 0, 0, 255]));
        }
        states.push(planes(&doc));
        // Two steps undone, so there's something to redo too.
        assert!(history.undo(&mut doc) && history.undo(&mut doc));
        let parked_planes = planes(&doc);
        assert_eq!(parked_planes, states[6]);
        let (done, undone): (Vec<String>, Vec<String>) =
            (history.done().map(str::to_string).collect(), history.undone().map(str::to_string).collect());

        // Park.
        let mut session = DocSession {
            doc,
            history,
            view: ViewTransform::default(),
            comic: None,
            comic_undo: Vec::new(),
            comic_redo: Vec::new(),
            selected_panel: None,
            scenes: HashMap::new(),
            scene_undo: Vec::new(),
            scene_redo: Vec::new(),
            file_path: Some(PathBuf::from("page 12.painter")),
            dirty: true,
            edit_mask: false,
            mask_view_mode: painter_render::MaskViewMode::Off,
            parked_at: Instant::now(),
            residency: Residency::Resident,
            thumbnail: None,
        };
        assert!(session.memory_bytes() > 0);

        // Suspend: meanwhile the tab still knows its layers and size.
        session.start_paging(true);
        assert!(!session.resident());
        assert_eq!((session.doc.canvas(), session.doc.find(inner).unwrap().name.as_str()), ((300, 200), "Lines"));
        finish_paging(&mut session);
        assert!(matches!(session.residency, Residency::Suspended));
        assert!(planes(&session.doc).is_empty(), "no pixels left in RAM");
        assert_eq!(session.memory_bytes(), 0);
        assert!(session.history.swap_bytes() > 0);
        assert!(session.dirty && session.file_path.is_some());
        assert!(session.history.done().eq(done.iter().map(String::as_str)));
        let Some(Thumbnail::Image(thumb)) = &session.thumbnail else { panic!("a thumbnail") };
        assert_eq!(thumb.size, [150, 100]);

        // Resume.
        session.start_paging(false);
        finish_paging(&mut session);
        assert!(session.resident());
        assert!(session.thumbnail.is_none());
        assert_eq!(planes(&session.doc), parked_planes);
        assert!(session.history.done().eq(done.iter().map(String::as_str)));
        assert!(session.history.undone().eq(undone.iter().map(String::as_str)));
        let (doc, history) = (&mut session.doc, &mut session.history);
        for want in states[..6].iter().rev() {
            assert!(history.undo(doc));
            assert_eq!(planes(doc), *want);
        }
        assert!(!history.can_undo());
        for want in &states[1..] {
            assert!(history.redo(doc));
            assert_eq!(planes(doc), *want);
        }

        // Suspended and needed at once (a tab closed next to it): back on this thread.
        session.start_paging(true);
        session.wake_now();
        assert!(session.resident());
        assert_eq!(planes(&session.doc), states[8]);
        drop(session);
        assert_eq!(std::fs::read_dir(&swap).map_or(0, |d| d.count()), 0, "closing the tab deletes its swap file");
        std::fs::remove_dir_all(&swap).ok();
    }

    #[test]
    fn thumbnails_average_the_flattened_canvas() {
        let mut doc = Document::new(1000, 130);
        let active = doc.active;
        doc.find_mut(active).unwrap().kind = LayerKind::Raster(solid(500, 130, [200, 0, 0, 255]));
        let thumb = thumbnail(&doc, THUMBNAIL_SIZE);
        // 4 × 4 blocks: 250 × 33, the left half red over the white paper.
        assert_eq!(thumb.size, [250, 33]);
        assert_eq!(thumb.pixels[0], egui::Color32::from_rgba_premultiplied(200, 0, 0, 255));
        assert_eq!(thumb.pixels[32 * 250 + 249], egui::Color32::WHITE);
    }

    #[test]
    fn idle_and_memory_limits_come_from_the_environment_defaults() {
        let policy = SuspendPolicy::from_env();
        if std::env::var_os("PAINTER_TAB_SUSPEND_MIN").is_none() && std::env::var_os("PAINTER_LOW_RAM").is_none() {
            assert_eq!(policy.idle, Some(Duration::from_secs(300)));
        }
        if std::env::var_os("PAINTER_TAB_RAM_MB").is_none() && std::env::var_os("PAINTER_LOW_RAM").is_none() {
            assert_eq!(policy.pressure_bytes, Some(2048 << 20));
        }
    }
}
