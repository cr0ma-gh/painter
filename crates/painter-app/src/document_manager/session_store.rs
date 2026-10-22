//! Open documents. The active one lives in `PainterApp`'s own fields (so
//! every tool works on it unchanged); the others are parked here as
//! [`DocSession`]s and swapped in when their tab is chosen. Only the active
//! document is on the GPU: parking frees its tiles' GPU memory, and the
//! render pipelines, shaders and brush caches stay shared.
//!
//! Opening and saving run on worker threads, so a heavy file doesn't stall
//! the canvas. A save works on a copy of the document (tiles are shared,
//! so the copy is cheap), and editing can go on meanwhile.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, TryRecvError};

use comic_layout::{ComicPage, PanelId};
use eframe::egui;
use painter_core::{io, project, psd, Document, History, LayerId};

use super::split_viewport::ReferenceView;
use crate::app::{file_stem, PainterApp, UNDO_LIMIT};
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
        !self.loads.is_empty() || !self.saves.is_empty()
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
            history: std::mem::replace(&mut self.history, History::new(UNDO_LIMIT)),
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
        }
    }

    /// Makes `s` the active document, with its own view.
    fn put_session(&mut self, s: DocSession) {
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
        self.stroke = None;
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
            self.status = "Finish the stroke first".into();
            return false;
        }
        if self.transform.is_some() {
            self.apply_transform();
        }
        self.crop = None;
        self.frame.cancel();
        self.commit_scenes(true);
        true
    }

    /// Shows tab `index`.
    pub(crate) fn switch_document(&mut self, index: usize) {
        if index == self.docs.active || index >= self.docs.slots.len() || !self.settle_active() {
            return;
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

    pub(crate) fn cycle_document(&mut self, step: i32) {
        let n = self.docs.slots.len() as i32;
        if n > 1 {
            self.switch_document((self.docs.active as i32 + step).rem_euclid(n) as usize);
        }
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
            self.status = "Some documents have unsaved changes: save them or close their tabs one by one".into();
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
            self.switch_document(i);
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
        self.poll_reference_previews(ctx);
        if self.docs.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    /// A file arrived from a worker thread: give it a tab.
    pub(crate) fn finish_open(&mut self, path: PathBuf, result: Loaded) {
        match result {
            Ok((doc, is_project, extras)) => {
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
}
