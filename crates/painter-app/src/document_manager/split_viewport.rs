//! The Reference View: a reference board like PureRef (see [`super::board`]).
//! Pictures dropped, pasted or added from files, and live pictures of other
//! open documents, are laid out freely: moved, scaled from a corner, turned
//! from the handle above, mirrored, faded, shown in grey, stacked, arranged.
//! The pen draws marks over them to point things out (a mark drawn on a
//! picture follows it); the eraser takes marks away. The board has its own
//! undo, pan and zoom, and keeps itself between launches; it can also be
//! saved to and opened from a `.refboard` file.
//!
//! Keys while the pointer is over the board: Delete removes the selection,
//! Ctrl+Z / Ctrl+Shift+Z undo and redo, Ctrl+A selects all, Ctrl+D
//! duplicates, Ctrl+V pastes a picture, F fits, H mirrors the selection,
//! P / E / V pick the pen, the eraser and the selection tool, Esc deselects.
//! Layers dragged onto a document's picture are copied into that document.

use std::collections::HashMap;
use crate::i18n::tr;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, ColorImage, Key, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions, Vec2};
use painter_core::io;
use ui_workspace::tiles::{EditorType, SplitKind};

use super::board::{Board, ItemId, Source, EXTENSION};
use super::layer_transfer::LayerDrag;
use crate::app::PainterApp;
use crate::theme;

/// Longest side of a picture's texture.
const TEXTURE_SIZE: u32 = 4096;
/// Longest side of a document's live picture.
const PREVIEW_SIZE: u32 = 2048;
const UNDO_LIMIT: usize = 100;
/// The board kept between launches, in the app's data folder.
const AUTOSAVE_FILE: &str = "reference-board.refboard";
const AUTOSAVE_EVERY: Duration = Duration::from_secs(3);
/// Screen size of the corner and turn handles.
const HANDLE: f32 = 7.0;
/// Smooth when shrunk, square pixels when enlarged (pixel art stays sharp).
const SHARP_WHEN_ENLARGED: TextureOptions = TextureOptions { magnification: egui::TextureFilter::Nearest, ..TextureOptions::LINEAR };
/// Image files the board takes.
pub(crate) const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "bmp", "gif"];
/// Documents the board shows as a picture (flattened): Photoshop, Krita,
/// OpenRaster, Painter projects, Clip Studio.
pub(crate) const DOCUMENT_EXTENSIONS: [&str; 5] = ["psd", "kra", "ora", "painter", "clip"];

/// Files the board takes: pictures and documents.
pub(crate) fn is_board_file(path: &std::path::Path) -> bool {
    is_image_file(path) || super::video::is_video_file(path) || path.extension().and_then(|e| e.to_str()).is_some_and(|e| DOCUMENT_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

pub(crate) fn is_image_file(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| IMAGE_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// A rendered picture of an open document.
struct Preview {
    key: u64,
    texture: TextureHandle,
}

struct PreviewJob {
    slot: u64,
    key: u64,
    doc_size: Vec2,
    rx: Receiver<ColorImage>,
}

/// A picture's texture (and its grey version, when shown in grey).
struct Texture {
    texture: TextureHandle,
    grey: bool,
    /// The bytes it was made from.
    source: usize,
}

struct DecodeJob {
    id: ItemId,
    grey: bool,
    source: usize,
    rx: Receiver<Option<ColorImage>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum BoardTool {
    #[default]
    Select,
    Pen,
    Eraser,
}

/// What a drag on the board does.
enum Drag {
    Pan,
    Move { start: [f32; 2], origins: Vec<(ItemId, [f32; 2])> },
    Scale { id: ItemId, start: f32, scale: f32 },
    Rotate { id: ItemId, start: f32, rotation: f32 },
    Draw,
    Erase,
}

/// Writes the board to the app's data folder a moment after it changes.
#[derive(Default)]
struct Autosave {
    path: Option<PathBuf>,
    saved: Board,
    checked: Option<Instant>,
    writing: Option<JoinHandle<Result<(), String>>>,
}

pub(crate) struct ReferenceView {
    pub(crate) board: Board,
    undo: Vec<Board>,
    redo: Vec<Board>,
    selection: Vec<ItemId>,
    pub(crate) tool: BoardTool,
    pen_color: Color32,
    /// Screen pixels at the zoom it's drawn at.
    pen_width: f32,
    /// Board point at the middle of the view, and screen points per board unit.
    center: Vec2,
    zoom: f32,
    /// Fit the board on the next frame (first show, after loading).
    fit_pending: bool,
    /// The view mirrored left-right (to check a drawing against references).
    mirror: bool,
    drag: Option<Drag>,
    drawing: Vec<[f32; 2]>,
    textures: HashMap<ItemId, Texture>,
    decoding: Vec<DecodeJob>,
    previews: HashMap<u64, Preview>,
    jobs: Vec<PreviewJob>,
    /// The board's area last frame, and whether the pointer was over it:
    /// where dropped files and keys go.
    pub(crate) rect: Option<Rect>,
    pub(crate) hovered: bool,
    /// Keys taken from the canvas while the pointer was over the board.
    pub(crate) keys: Vec<egui::Event>,
    autosave: Autosave,
    /// What the status bar should say (taken once).
    pub(crate) message: Option<String>,
    /// Ctrl+V was down last frame (see [`ctrl_v_down`]).
    paste_down: bool,
    /// GIFs and videos playing.
    pub(crate) media: super::board_media::Media,
}

impl Default for ReferenceView {
    fn default() -> Self {
        Self {
            board: Board::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            selection: Vec::new(),
            tool: BoardTool::Select,
            pen_color: Color32::from_rgb(235, 59, 48),
            pen_width: 4.0,
            center: Vec2::ZERO,
            zoom: 1.0,
            fit_pending: true,
            mirror: false,
            drag: None,
            drawing: Vec::new(),
            textures: HashMap::new(),
            decoding: Vec::new(),
            previews: HashMap::new(),
            jobs: Vec::new(),
            rect: None,
            hovered: false,
            keys: Vec::new(),
            autosave: Autosave::default(),
            message: None,
            paste_down: false,
            media: Default::default(),
        }
    }
}

/// `pixels` (premultiplied RGBA8, `w` × `h`) shrunk by an integer box filter
/// so the longest side is at most `max`.
pub(crate) fn downscale(pixels: &[u8], w: u32, h: u32, max: u32) -> ColorImage {
    let step = w.max(h).div_ceil(max).max(1) as usize;
    let (ow, oh) = ((w as usize).div_ceil(step), (h as usize).div_ceil(step));
    let mut out = vec![Color32::TRANSPARENT; ow * oh];
    for oy in 0..oh {
        for ox in 0..ow {
            let mut sum = [0u32; 4];
            let mut n = 0;
            for y in oy * step..((oy + 1) * step).min(h as usize) {
                for x in ox * step..((ox + 1) * step).min(w as usize) {
                    let i = (y * w as usize + x) * 4;
                    for c in 0..4 {
                        sum[c] += pixels[i + c] as u32;
                    }
                    n += 1;
                }
            }
            let n = n.max(1);
            out[oy * ow + ox] =
                Color32::from_rgba_premultiplied((sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8, (sum[3] / n) as u8);
        }
    }
    ColorImage { size: [ow, oh], pixels: out }
}

/// An image file decoded for the screen: premultiplied, at most
/// [`TEXTURE_SIZE`] on a side, in grey if asked.
fn decode(bytes: &[u8], grey: bool) -> Option<ColorImage> {
    // GIFs: their first frame (animations play through `board_media`).
    let (w, h, mut raw) = if painter_core::gif::is_gif(bytes) {
        let gif = painter_core::gif::decode(bytes).ok()?;
        let first = gif.frames.into_iter().next()?;
        (gif.width, gif.height, first.rgba)
    } else {
        let img = image::load_from_memory(bytes).ok()?.to_rgba8();
        let (w, h) = img.dimensions();
        (w, h, img.into_raw())
    };
    for px in raw.as_chunks_mut::<4>().0 {
        if grey {
            let l = ((px[0] as u32 * 299 + px[1] as u32 * 587 + px[2] as u32 * 114) / 1000) as u8;
            *px = [l, l, l, px[3]];
        }
        *px = painter_core::blend::premultiply(*px);
    }
    Some(downscale(&raw, w, h, TEXTURE_SIZE))
}

/// A picture's size in pixels from its file's header.
fn image_size(bytes: &[u8]) -> Option<[f32; 2]> {
    if painter_core::gif::is_gif(bytes) && bytes.len() >= 10 {
        let (w, h) = (u16::from_le_bytes([bytes[6], bytes[7]]), u16::from_le_bytes([bytes[8], bytes[9]]));
        return (w > 0 && h > 0).then_some([w as f32, h as f32]);
    }
    let (w, h) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok()?;
    Some([w as f32, h as f32])
}

/// A fingerprint of a document's pixels and layer look, to notice changes
/// to the document being edited (tiles are copied on write).
fn doc_fingerprint(doc: &painter_core::Document) -> u64 {
    fn walk(layers: &[painter_core::Layer], h: &mut u64) {
        for l in layers {
            let mut mix = |v: u64| *h = (*h ^ v).wrapping_mul(0x0100_0000_01b3);
            mix(l.id.0);
            mix(l.visible as u64 | (l.opacity.to_bits() as u64) << 1);
            if let Some(t) = l.tiles() {
                for (c, tile) in t.iter() {
                    mix(Arc::as_ptr(tile) as u64 ^ ((c.x as u64) << 32) ^ c.y as u64);
                }
            }
            if let painter_core::LayerKind::Folder(f) = &l.kind {
                walk(&f.children, h);
            }
        }
    }
    let mut h = 0xcbf2_9ce4_8422_2325;
    walk(&doc.layers, &mut h);
    h
}

impl ReferenceView {
    /// The board saved at the last launch.
    pub(crate) fn load_saved(&mut self) {
        let path = crate::files::app_data_dir().map(|d| d.join(AUTOSAVE_FILE));
        if let Some(p) = path.as_ref().filter(|p| p.is_file()) {
            match std::fs::read(p).map_err(|e| e.to_string()).and_then(Board::from_bytes) {
                Ok(board) => {
                    self.board = board.clone();
                    self.autosave.saved = board;
                }
                Err(err) => {
                    let _ = std::fs::rename(p, p.with_extension("refboard.bad"));
                    self.message = Some(format!("The reference board couldn't be read ({err}); starting with an empty one"));
                }
            }
        }
        self.autosave.path = path;
    }

    /// Writes the board if it changed: at most every few seconds, or now
    /// (quitting), waiting for the write.
    pub(crate) fn autosave(&mut self, now: bool) {
        let a = &mut self.autosave;
        let Some(path) = a.path.clone() else { return };
        if let Some(handle) = a.writing.take_if(|h| now || h.is_finished()) {
            if let Ok(Err(err)) = handle.join() {
                self.message = Some(format!("Couldn't save the reference board: {err}"));
                a.saved = Board::default();
            }
        }
        if a.writing.is_some() || (!now && a.checked.is_some_and(|t| t.elapsed() < AUTOSAVE_EVERY)) {
            return;
        }
        a.checked = Some(Instant::now());
        if a.saved == self.board {
            return;
        }
        a.saved = self.board.clone();
        let board = self.board.clone();
        let handle = std::thread::spawn(move || -> Result<(), String> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, board.to_bytes()).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
        });
        if now {
            if let Ok(Err(err)) = handle.join() {
                self.message = Some(format!("Couldn't save the reference board: {err}"));
            }
        } else {
            a.writing = Some(handle);
        }
    }

    /// Remembers the board before a change, for undo.
    fn checkpoint(&mut self) {
        self.undo.push(self.board.clone());
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn undo(&mut self) {
        if let Some(b) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.board, b));
            self.selection.retain(|id| self.board.find(*id).is_some());
        }
    }

    fn redo(&mut self) {
        if let Some(b) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.board, b));
            self.selection.retain(|id| self.board.find(*id).is_some());
        }
    }

    /// Adds an image file's bytes at `at` (board units; the view's centre
    /// when `None`), selected. Pictures bigger than the view are scaled to fit it.
    pub(crate) fn add_image(&mut self, name: &str, ext: &str, bytes: Vec<u8>, at: Option<[f32; 2]>) -> Result<ItemId, String> {
        let size = image_size(&bytes).ok_or_else(|| format!("{name} isn't an image Painter can read (PNG, JPEG, BMP)"))?;
        self.checkpoint();
        let at = at.unwrap_or([self.center.x, self.center.y]);
        let source = Source::Image { name: name.to_string(), ext: ext.to_ascii_lowercase(), bytes: Arc::new(bytes) };
        let id = self.board.add(source, size, at);
        if let Some(rect) = self.rect {
            let view = rect.size() / self.zoom;
            let fit = (view.x * 0.8 / size[0]).min(view.y * 0.8 / size[1]);
            if fit < 1.0 {
                self.board.find_mut(id).expect("just added").scale = fit;
            }
        }
        self.selection = vec![id];
        self.tool = BoardTool::Select;
        Ok(id)
    }

    pub(crate) fn add_image_file(&mut self, path: &std::path::Path, at: Option<[f32; 2]>) -> Result<ItemId, String> {
        if super::video::is_video_file(path) {
            return self.add_video_file(path, at);
        }
        if !is_image_file(path) && is_board_file(path) {
            // A layered document: its picture, flattened (draft layers left out).
            let (doc, _, _) = crate::document_manager::session_store::load_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let mut pixels = painter_core::io::flatten(&doc.without_drafts());
            for px in pixels.as_chunks_mut::<4>().0 {
                *px = painter_core::blend::unpremultiply(*px);
            }
            let mut png = Vec::new();
            image::RgbaImage::from_raw(doc.width, doc.height, pixels)
                .ok_or("image size mismatch")?
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .map_err(|e| e.to_string())?;
            let name = path.file_name().map_or_else(|| "Document".into(), |s| s.to_string_lossy().into_owned());
            return self.add_image(&name, "png", png, at);
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let name = path.file_stem().map_or_else(|| "Image".into(), |s| s.to_string_lossy().into_owned());
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("png").to_string();
        self.add_image(&name, &ext, bytes, at)
    }

    /// A video, played in a loop (kept by its path, not copied into the board).
    fn add_video_file(&mut self, path: &std::path::Path, at: Option<[f32; 2]>) -> Result<ItemId, String> {
        let info = super::video::probe(path)?;
        self.checkpoint();
        let name = path.file_stem().map_or_else(|| "Video".into(), |s| s.to_string_lossy().into_owned());
        let source = Source::Video { name, path: path.to_string_lossy().into_owned() };
        let size = [info.width as f32, info.height as f32];
        let id = self.board.add(source, size, at.unwrap_or([self.center.x, self.center.y]));
        if let Some(rect) = self.rect {
            let view = rect.size() / self.zoom;
            let fit = (view.x * 0.8 / size[0]).min(view.y * 0.8 / size[1]);
            if fit < 1.0 {
                self.board.find_mut(id).expect("just added").scale = fit;
            }
        }
        self.selection = vec![id];
        self.tool = BoardTool::Select;
        Ok(id)
    }

    /// A picture from the clipboard.
    fn paste(&mut self, at: Option<[f32; 2]>) -> Result<ItemId, String> {
        if let Some(img) = crate::clipboard_image::read() {
            return self.add_image("Pasted", "png", img.to_png()?, at);
        }
        match crate::clipboard_image::copied_image_path() {
            Some(path) => self.add_image_file(&path, at),
            None => Err("No picture in the clipboard".into()),
        }
    }

    /// Shows everything (or nothing to show: 100 % at the origin).
    fn fit(&mut self, rect: Rect) {
        match self.board.bounds() {
            Some((lo, hi)) => {
                let size = Vec2::new(hi[0] - lo[0], hi[1] - lo[1]).max(Vec2::splat(1.0));
                self.zoom = ((rect.width() - 32.0) / size.x).min((rect.height() - 32.0) / size.y).clamp(0.01, 8.0);
                self.center = Vec2::new((lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0);
            }
            None => {
                self.zoom = 1.0;
                self.center = Vec2::ZERO;
            }
        }
    }

    fn to_screen(&self, rect: Rect, [x, y]: [f32; 2]) -> Pos2 {
        let dx = (x - self.center.x) * self.zoom;
        rect.center() + Vec2::new(if self.mirror { -dx } else { dx }, (y - self.center.y) * self.zoom)
    }

    fn to_board(&self, rect: Rect, p: Pos2) -> [f32; 2] {
        let d = (p - rect.center()) / self.zoom;
        [self.center.x + if self.mirror { -d.x } else { d.x }, self.center.y + d.y]
    }

    /// The single selected picture's corner and turn handles, on screen.
    fn handles(&self, rect: Rect) -> Option<(ItemId, [Pos2; 4], Pos2)> {
        let [id] = self.selection[..] else { return None };
        let item = self.board.find(id)?;
        let corners = item.corners().map(|c| self.to_screen(rect, c));
        let top = corners[0] + (corners[1] - corners[0]) / 2.0;
        let middle = corners[0] + (corners[2] - corners[0]) / 2.0;
        let up = (top - middle).normalized();
        let up = if up.is_finite() { up } else { -Vec2::Y };
        Some((id, corners, top + up * 22.0))
    }

    fn select_all(&mut self) {
        self.selection = self.board.items.iter().filter(|i| !i.locked).map(|i| i.id).collect();
    }

    fn delete_selection(&mut self) {
        if !self.selection.is_empty() {
            self.checkpoint();
            self.board.remove(&self.selection);
            self.selection.clear();
        }
    }

    fn duplicate_selection(&mut self) {
        if !self.selection.is_empty() {
            self.checkpoint();
            let offset = 24.0 / self.zoom;
            self.selection = self.board.duplicate(&self.selection, [offset, offset]);
        }
    }

    /// Changes every selected picture (one undo step).
    fn edit_selection(&mut self, f: impl Fn(&mut super::board::Item)) {
        if self.selection.is_empty() {
            return;
        }
        self.checkpoint();
        for id in self.selection.clone() {
            if let Some(item) = self.board.find_mut(id) {
                f(item);
            }
        }
    }

    /// Starts decoding pictures without a texture; collects finished ones;
    /// forgets textures of pictures that are gone.
    fn update_textures(&mut self, ctx: &egui::Context) {
        let mut done = Vec::new();
        self.decoding.retain(|j| match j.rx.try_recv() {
            Ok(image) => {
                done.push((j.id, j.grey, j.source, image));
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(_) => false,
        });
        for (id, grey, source, image) in done {
            match image {
                Some(image) => {
                    let texture = ctx.load_texture(format!("board-{id}"), image, SHARP_WHEN_ENLARGED);
                    self.textures.insert(id, Texture { texture, grey, source });
                }
                None => self.message = Some("A picture on the reference board couldn't be decoded".into()),
            }
        }
        self.textures.retain(|id, _| self.board.find(*id).is_some());
        for item in &self.board.items {
            let Source::Image { bytes, .. } = &item.source else { continue };
            let source = Arc::as_ptr(bytes) as usize;
            let current = self.textures.get(&item.id).is_some_and(|t| t.grey == item.grey && t.source == source);
            let running = self.decoding.iter().any(|j| j.id == item.id && j.grey == item.grey && j.source == source);
            if current || running {
                continue;
            }
            let (bytes, grey) = (bytes.clone(), item.grey);
            let (tx, rx) = channel();
            std::thread::spawn(move || {
                let _ = tx.send(decode(&bytes, grey));
            });
            self.decoding.push(DecodeJob { id: item.id, grey, source, rx });
        }
        if !self.decoding.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(60));
        }
    }

    /// Handles a key or paste taken from the canvas (or read while hovered).
    fn key(&mut self, event: &egui::Event, pointer: Option<[f32; 2]>) {
        match event {
            egui::Event::Paste(_) => self.paste_report(pointer),
            egui::Event::Key { key, pressed: true, modifiers, .. } => {
                let cmd = modifiers.command || modifiers.ctrl;
                match key {
                    Key::Delete | Key::Backspace => self.delete_selection(),
                    Key::Escape => self.selection.clear(),
                    Key::Z if cmd && modifiers.shift => self.redo(),
                    Key::Z if cmd => self.undo(),
                    Key::Y if cmd => self.redo(),
                    Key::A if cmd => self.select_all(),
                    Key::D if cmd => self.duplicate_selection(),
                    Key::V if cmd => self.paste_report(pointer),
                    Key::F if !cmd => self.fit_pending = true,
                    Key::H if !cmd => self.edit_selection(|i| i.flip = !i.flip),
                    Key::P if !cmd => self.tool = BoardTool::Pen,
                    Key::E if !cmd => self.tool = BoardTool::Eraser,
                    Key::V if !cmd => self.tool = BoardTool::Select,
                    // GIFs and videos.
                    Key::Space => self.media.toggle(&self.selection),
                    Key::ArrowLeft => self.media.seek(&self.selection, None, -5000),
                    Key::ArrowRight => self.media.seek(&self.selection, None, 5000),
                    Key::Home => self.media.seek(&self.selection, Some(0), 0),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn paste_report(&mut self, at: Option<[f32; 2]>) {
        if let Err(err) = self.paste(at) {
            self.message = Some(err);
        }
    }
}

/// Ctrl+V held, read from the keyboard: egui only reports Ctrl+V as a text
/// paste, and not at all when the clipboard holds just a picture.
#[cfg(windows)]
fn ctrl_v_down() -> bool {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetAsyncKeyState(key: i32) -> i16;
    }
    // SAFETY: reads the keyboard state; no pointers involved.
    let down = |k: i32| unsafe { GetAsyncKeyState(k) } as u16 & 0x8000 != 0;
    down(0x11) && down(0x56) // VK_CONTROL, V
}

#[cfg(not(windows))]
fn ctrl_v_down() -> bool {
    false
}

/// Keys the board takes from the canvas while the pointer is over it.
pub(crate) fn board_key(event: &egui::Event) -> bool {
    match event {
        egui::Event::Paste(_) => true,
        egui::Event::Key { key, modifiers, .. } => {
            let cmd = modifiers.command || modifiers.ctrl;
            matches!(key, Key::Delete | Key::Backspace | Key::Escape)
                || (cmd && matches!(key, Key::Z | Key::Y | Key::A | Key::D | Key::V))
                || (!cmd && !modifiers.alt && matches!(key, Key::F | Key::H | Key::P | Key::E | Key::V))
        }
        _ => false,
    }
}

impl PainterApp {
    /// Puts tab `id`'s live picture on the board, adding a Reference View left
    /// of the canvas if the workspace has none.
    pub(crate) fn show_in_reference(&mut self, id: u64) {
        let r = &mut self.docs.reference;
        let existing = r.board.items.iter().find(|i| i.source == Source::Document { slot: id }).map(|i| i.id);
        let item = match existing {
            Some(item) => item,
            None => {
                r.checkpoint();
                let size = self.docs.slots.iter().find(|s| s.id == id).and_then(|s| s.parked.as_ref()).map(|s| s.doc.canvas());
                let size = size.unwrap_or(self.doc.canvas());
                let at = r.board.bounds().map_or([0.0, 0.0], |(_, hi)| [hi[0] + 40.0 + size.0 as f32 / 2.0, 0.0]);
                r.board.add(Source::Document { slot: id }, [size.0 as f32, size.1 as f32], at)
            }
        };
        r.selection = vec![item];
        r.fit_pending = true;
        let tiles = &mut self.ws.set.current_mut().tiles;
        if let Some(area) = tiles.find(EditorType::Reference) {
            tiles.add_tab(area, EditorType::Reference);
        } else if let Some(canvas) = tiles.find(EditorType::Canvas) {
            tiles.split(canvas, SplitKind::SideBySide, 0.5, EditorType::Reference, true);
        }
    }

    /// Takes the board's keys out of the input while the pointer is over it.
    pub(crate) fn intercept_board_keys(&mut self, raw: &mut egui::RawInput) {
        if !self.docs.reference.hovered {
            return;
        }
        let keys = &mut self.docs.reference.keys;
        raw.events.retain(|e| {
            if board_key(e) {
                keys.push(e.clone());
                false
            } else {
                true
            }
        });
    }

    /// Image files dropped on the board; returns those it didn't take.
    pub(crate) fn drop_on_board(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>) -> Vec<PathBuf> {
        let r = &mut self.docs.reference;
        let Some(rect) = r.rect.filter(|_| r.hovered) else { return paths };
        let pointer = ctx.input(|i| i.pointer.hover_pos()).unwrap_or(rect.center());
        let mut at = r.to_board(rect, pointer);
        let mut rest = Vec::new();
        for path in paths {
            if !is_board_file(&path) {
                rest.push(path);
                continue;
            }
            match r.add_image_file(&path, Some(at)) {
                Ok(_) => at = [at[0] + 32.0 / r.zoom, at[1] + 32.0 / r.zoom],
                Err(err) => r.message = Some(err),
            }
        }
        if rest.is_empty() && r.message.is_none() {
            self.status = tr("Added to the reference board").into();
        }
        rest
    }

    /// Picks image files for the board.
    pub(crate) fn add_board_images_dialog(&mut self) {
        // Videos are only offered where they can play (not on Android / iOS yet).
        let dialog = if super::video::PLAYBACK_SUPPORTED {
            crate::files::FileDialog::new()
                .add_filter(
                    "Pictures, videos and documents",
                    &[IMAGE_EXTENSIONS.as_slice(), super::video::VIDEO_EXTENSIONS.as_slice(), DOCUMENT_EXTENSIONS.as_slice()].concat(),
                )
                .add_filter("Images", &IMAGE_EXTENSIONS)
                .add_filter("Videos", &super::video::VIDEO_EXTENSIONS)
        } else {
            crate::files::FileDialog::new()
                .add_filter("Pictures and documents", &[IMAGE_EXTENSIONS.as_slice(), DOCUMENT_EXTENSIONS.as_slice()].concat())
                .add_filter("Images", &IMAGE_EXTENSIONS)
        };
        if let Some(paths) = dialog
            .add_filter("Documents (PSD, Krita, OpenRaster, Painter, Clip Studio)", &DOCUMENT_EXTENSIONS)
            .purpose(crate::files::Purpose::ReferenceImage)
            .pick_files()
        {
            for path in paths {
                self.add_board_image(&path);
            }
        }
    }

    pub(crate) fn add_board_image(&mut self, path: &std::path::Path) {
        if let Err(err) = self.docs.reference.add_image_file(path, None) {
            self.status = err;
        }
    }

    fn open_board_dialog(&mut self) {
        let Some(path) = crate::files::FileDialog::new().add_filter("Reference board", &[EXTENSION]).pick_file() else { return };
        match std::fs::read(&path).map_err(|e| e.to_string()).and_then(Board::from_bytes) {
            Ok(board) => {
                let r = &mut self.docs.reference;
                r.checkpoint();
                r.board = board;
                r.selection.clear();
                r.fit_pending = true;
                self.status = format!("Opened the reference board {}", path.display());
            }
            Err(err) => self.status = format!("Couldn't open {}: {err}", path.display()),
        }
    }

    fn save_board_dialog(&mut self) {
        let Some(path) = crate::files::FileDialog::new()
            .add_filter("Reference board", &[EXTENSION])
            .set_file_name(format!("references.{EXTENSION}"))
            .save_file()
        else {
            return;
        };
        self.status = match std::fs::write(&path, self.docs.reference.board.to_bytes()) {
            Ok(()) => {
                crate::files::offer_export(&path);
                format!("Saved the reference board to {}", path.display())
            }
            Err(err) => format!("Couldn't save the board: {err}"),
        };
    }

    /// Renders live pictures of the documents on the board that changed;
    /// collects finished ones; drops pictures of closed documents.
    pub(crate) fn poll_reference_previews(&mut self, ctx: &egui::Context) {
        let r = &mut self.docs.reference;
        let mut finished = Vec::new();
        r.jobs.retain(|job| match job.rx.try_recv() {
            Ok(image) => {
                finished.push((job.slot, job.key, job.doc_size, image));
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(_) => false,
        });
        for (slot, key, doc_size, image) in finished {
            let texture = ctx.load_texture(format!("reference-{slot}"), image, SHARP_WHEN_ENLARGED);
            // The document may have been resized.
            for item in r.board.items.iter_mut().filter(|i| i.source == Source::Document { slot }) {
                item.size = [doc_size.x, doc_size.y];
            }
            r.previews.insert(slot, Preview { key, texture });
        }
        let open: Vec<u64> = self.docs.slots.iter().map(|s| s.id).collect();
        r.board.items.retain(|i| !matches!(i.source, Source::Document { slot } if !open.contains(&slot)));
        let slots: Vec<u64> =
            r.board.items.iter().filter_map(|i| if let Source::Document { slot } = i.source { Some(slot) } else { None }).collect();
        r.previews.retain(|slot, _| slots.contains(slot));
        for slot in slots {
            let Some(i) = self.docs.index_of(slot) else { continue };
            // A suspended tab keeps the picture it had; with none, it comes back to draw one.
            if self.docs.is_suspended(i) {
                if !self.docs.reference.previews.contains_key(&slot) {
                    self.wake_document(i);
                }
                continue;
            }
            let s = &self.docs.slots[i];
            // A parked document changes only through transfers (its revision);
            // the one being edited is checked by its pixels.
            let (doc, key) = match &s.parked {
                Some(session) => (&session.doc, s.revision),
                None => (&self.doc, doc_fingerprint(&self.doc)),
            };
            let r = &self.docs.reference;
            let fresh = r.previews.get(&slot).is_some_and(|p| p.key == key);
            let running = r.jobs.iter().any(|j| j.slot == slot);
            if fresh || running {
                continue;
            }
            let doc = doc.clone();
            let doc_size = Vec2::new(doc.width as f32, doc.height as f32);
            let (tx, rx) = channel();
            std::thread::spawn(move || {
                let pixels = io::flatten(&doc);
                let _ = tx.send(downscale(&pixels, doc.width, doc.height, PREVIEW_SIZE));
            });
            self.docs.reference.jobs.push(PreviewJob { slot, key, doc_size, rx });
        }
        if !self.docs.reference.jobs.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        self.docs.reference.update_textures(ctx);
        let r = &mut self.docs.reference;
        if let Some(msg) = r.media.update(ctx, &r.board) {
            r.message = Some(msg);
        }
        self.docs.reference.autosave(false);
        if let Some(msg) = self.docs.reference.message.take() {
            self.status = msg;
        }
    }

    fn board_toolbar(&mut self, ui: &mut egui::Ui) {
        let others: Vec<(u64, String)> = (0..self.docs.slots.len()).map(|i| (self.docs.slots[i].id, self.tab_title(i))).collect();
        ui.horizontal_wrapped(|ui| {
            ui.add_space(4.0);
            let r = &mut self.docs.reference;
            for (tool, label, tip) in [
                (BoardTool::Select, "Select", "Move, scale (corners) and turn (handle above) pictures · V"),
                (BoardTool::Pen, "Pen", "Draw marks over the pictures · P"),
                (BoardTool::Eraser, "Eraser", "Rub out marks · E"),
            ] {
                if ui.selectable_label(r.tool == tool, label).on_hover_text(tip).clicked() {
                    r.tool = tool;
                }
            }
            if r.tool != BoardTool::Select {
                ui.color_edit_button_srgba(&mut r.pen_color).on_hover_text(tr("Pen colour"));
                ui.add(egui::DragValue::new(&mut r.pen_width).range(1.0..=60.0).speed(0.2).suffix(" px"))
                    .on_hover_text(tr("Pen width on screen"));
            }
            ui.separator();
            let mut add_images = false;
            let mut paste = false;
            let mut add_doc = None;
            ui.menu_button(tr("+"), |ui| {
                if ui.button(tr("Pictures or documents...")).on_hover_text(tr("PNG, JPEG, BMP; PSD, Krita, OpenRaster, Painter and Clip Studio files as pictures")).clicked() {
                    ui.close_menu();
                    add_images = true;
                }
                if ui.button(tr("Paste picture (Ctrl+V)")).clicked() {
                    ui.close_menu();
                    paste = true;
                }
                ui.separator();
                ui.label(tr("Live picture of a document:"));
                for (id, name) in &others {
                    if ui.button(name).clicked() {
                        ui.close_menu();
                        add_doc = Some(*id);
                    }
                }
            })
            .response
            .on_hover_text(tr("Add pictures (you can also drop image files here)"));
            let r = &mut self.docs.reference;
            if ui.button(tr("Fit")).on_hover_text(tr("Show everything · F")).clicked() {
                r.fit_pending = true;
            }
            if ui.button(tr("Arrange")).on_hover_text(tr("Lay the pictures (or the selected ones) out in rows")).clicked() {
                r.checkpoint();
                let ids = (!r.selection.is_empty()).then(|| r.selection.clone());
                r.board.arrange(ids.as_deref(), 16.0);
                r.fit_pending = true;
            }
            ui.toggle_value(&mut r.mirror, tr("Mirror")).on_hover_text(tr("Mirror the view"));
            ui.add_enabled_ui(!r.undo.is_empty(), |ui| {
                if ui.small_button(tr("⟲")).on_hover_text(tr("Undo · Ctrl+Z")).clicked() {
                    r.undo();
                }
            });
            ui.add_enabled_ui(!r.redo.is_empty(), |ui| {
                if ui.small_button(tr("⟳")).on_hover_text(tr("Redo · Ctrl+Shift+Z")).clicked() {
                    r.redo();
                }
            });
            let mut open = false;
            let mut save = false;
            ui.menu_button(tr("Board"), |ui| {
                if ui.button(tr("Open board...")).clicked() {
                    ui.close_menu();
                    open = true;
                }
                if ui.button(tr("Save board as...")).clicked() {
                    ui.close_menu();
                    save = true;
                }
                ui.separator();
                let r = &mut self.docs.reference;
                if ui.add_enabled(!r.board.marks.is_empty(), egui::Button::new(tr("Clear all marks"))).clicked() {
                    ui.close_menu();
                    r.checkpoint();
                    r.board.marks.clear();
                }
                if ui.add_enabled(r.board.items.iter().any(|i| i.locked), egui::Button::new(tr("Unlock all"))).clicked() {
                    ui.close_menu();
                    r.checkpoint();
                    r.board.items.iter_mut().for_each(|i| i.locked = false);
                }
                if ui.add_enabled(r.board != Board::default(), egui::Button::new(tr("Clear the board"))).clicked() {
                    ui.close_menu();
                    r.checkpoint();
                    r.board = Board::default();
                    r.selection.clear();
                }
            });
            if add_images {
                self.add_board_images_dialog();
            }
            if paste {
                self.docs.reference.paste_report(None);
            }
            if let Some(id) = add_doc {
                self.show_in_reference(id);
            }
            if open {
                self.open_board_dialog();
            }
            if save {
                self.save_board_dialog();
            }
        });
    }

    /// The Reference View editor: the board.
    pub(crate) fn reference_view_ui(&mut self, ui: &mut egui::Ui) {
        self.board_toolbar(ui);
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let ctx = ui.ctx().clone();
        let pointer = response.hover_pos().or(response.interact_pointer_pos());
        {
            let r = &mut self.docs.reference;
            r.rect = Some(rect);
            r.hovered = response.hovered() || response.dragged() || response.context_menu_opened();
            if r.fit_pending {
                r.fit(rect);
                r.fit_pending = false;
            }
        }

        // Keys: those taken from the canvas, and any read here while hovered.
        let mut keys = std::mem::take(&mut self.docs.reference.keys);
        if response.hovered() {
            keys.extend(ui.input(|i| i.events.iter().filter(|e| board_key(e)).cloned().collect::<Vec<_>>()));
        }
        let at = pointer.map(|p| self.docs.reference.to_board(rect, p));
        for e in &keys {
            self.docs.reference.key(e, at);
        }
        // Ctrl+V with a picture in the clipboard, which egui doesn't report.
        let down = response.hovered() && ui.input(|i| i.focused) && ctrl_v_down();
        let r = &mut self.docs.reference;
        let reported = keys.iter().any(|e| {
            matches!(e, egui::Event::Paste(_))
                || matches!(e, egui::Event::Key { key: Key::V, modifiers, .. } if modifiers.command || modifiers.ctrl)
        });
        if down && !r.paste_down && !reported {
            r.paste_report(at);
        }
        r.paste_down = down;

        // Layers dropped on a document's picture are copied into that document.
        if let Some(payload) = response.dnd_release_payload::<LayerDrag>() {
            let r = &self.docs.reference;
            let target = pointer.map(|p| r.to_board(rect, p)).and_then(|p| r.board.any_item_at(p)).and_then(|id| r.board.find(id));
            match target.map(|i| &i.source) {
                Some(Source::Document { slot }) => {
                    if let Some(i) = self.docs.index_of(*slot) {
                        self.transfer_layer_to(i, payload.layer);
                    }
                }
                _ => self.status = tr("Drop the layer on a document's picture to copy it there").into(),
            }
        }
        let layer_hover = response.dnd_hover_payload::<LayerDrag>().is_some();
        let files_hover =
            response.hovered() && ctx.input(|i| i.raw.hovered_files.iter().any(|f| f.path.as_deref().is_some_and(is_board_file)));

        self.board_input(ui, rect, &response, pointer);
        let edit = self.board_context_menu(&response);
        if let Some(slot) = edit {
            if let Some(i) = self.docs.index_of(slot) {
                self.switch_document(i);
            }
        }

        // Drawing.
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme::canvas_backdrop());
        let r = &self.docs.reference;
        for item in &r.board.items {
            let corners = item.corners().map(|c| r.to_screen(rect, c));
            let tex = match &item.source {
                Source::Image { .. } => r.media.texture(item).or_else(|| r.textures.get(&item.id).map(|t| t.texture.id())),
                Source::Document { slot } => r.previews.get(slot).map(|p| p.texture.id()),
                Source::Video { .. } => r.media.texture(item),
            };
            match tex {
                Some(tex) => {
                    // Local corners (unmirrored) TL, TR, BR, BL carry UVs (0,0) (1,0) (1,1) (0,1).
                    let [w, h] = [item.size[0] / 2.0, item.size[1] / 2.0];
                    let local = [[-w, -h], [w, -h], [w, h], [-w, h]].map(|p| r.to_screen(rect, item.to_board(p)));
                    let uv = [Pos2::new(0.0, 0.0), Pos2::new(1.0, 0.0), Pos2::new(1.0, 1.0), Pos2::new(0.0, 1.0)];
                    let tint = Color32::WHITE.gamma_multiply(item.opacity);
                    let mut mesh = egui::Mesh::with_texture(tex);
                    for k in 0..4 {
                        mesh.vertices.push(egui::epaint::Vertex { pos: local[k], uv: uv[k], color: tint });
                    }
                    mesh.add_triangle(0, 1, 2);
                    mesh.add_triangle(0, 2, 3);
                    painter.add(mesh);
                }
                None => {
                    painter.add(egui::Shape::convex_polygon(corners.to_vec(), Color32::from_gray(60), Stroke::NONE));
                    let label = if matches!(item.source, Source::Document { .. }) { "Rendering..." } else { "Loading..." };
                    let c = corners[0] + (corners[2] - corners[0]) / 2.0;
                    painter.text(c, egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(12.0), theme::text_dim());
                }
            }
            if item.locked {
                painter.text(
                    corners[0] + Vec2::new(4.0, 4.0),
                    egui::Align2::LEFT_TOP,
                    "🔒",
                    egui::FontId::proportional(11.0),
                    theme::text_dim(),
                );
            }
            // GIFs and videos: how far through the loop, along the bottom edge.
            if let Some((t, playing)) = r.media.progress(item) {
                let (a, b) = (corners[3], corners[2]);
                painter.line_segment([a, b], Stroke::new(3.0_f32, Color32::from_black_alpha(140)));
                painter.line_segment([a, a + (b - a) * t], Stroke::new(3.0_f32, theme::accent()));
                if !playing {
                    let c = corners[0] + (corners[2] - corners[0]) / 2.0;
                    painter.circle_filled(c, 16.0, Color32::from_black_alpha(140));
                    painter.text(c, egui::Align2::CENTER_CENTER, "⏸", egui::FontId::proportional(16.0), Color32::WHITE);
                }
            }
        }
        // Marks over everything.
        let line = |pts: &[[f32; 2]], width: f32, color: Color32| {
            let screen: Vec<Pos2> = pts.iter().map(|p| r.to_screen(rect, *p)).collect();
            let width = (width * r.zoom).max(1.0);
            match screen.as_slice() {
                [] => {}
                [p] => {
                    painter.circle_filled(*p, width / 2.0, color);
                }
                _ => {
                    painter.add(egui::Shape::line(screen.clone(), Stroke::new(width, color)));
                    // Round ends.
                    painter.circle_filled(screen[0], width / 2.0, color);
                    painter.circle_filled(*screen.last().expect("not empty"), width / 2.0, color);
                }
            }
        };
        for m in &r.board.marks {
            let [cr, cg, cb, ca] = m.color;
            line(&r.board.mark_points(m), r.board.mark_width(m), Color32::from_rgba_unmultiplied(cr, cg, cb, ca));
        }
        if !r.drawing.is_empty() {
            line(&r.drawing, r.pen_width / r.zoom, r.pen_color);
        }
        // Selection.
        for id in &r.selection {
            if let Some(item) = r.board.find(*id) {
                let c = item.corners().map(|c| r.to_screen(rect, c));
                painter.add(egui::Shape::closed_line(c.to_vec(), Stroke::new(1.5_f32, theme::accent())));
            }
        }
        if let Some((_, corners, turn)) = r.handles(rect) {
            let top = corners[0] + (corners[1] - corners[0]) / 2.0;
            painter.line_segment([top, turn], Stroke::new(1.0_f32, theme::accent()));
            painter.circle(turn, HANDLE * 0.8, Color32::WHITE, Stroke::new(1.5_f32, theme::accent()));
            for c in corners {
                painter.rect(
                    Rect::from_center_size(c, Vec2::splat(HANDLE * 1.4)),
                    1.0,
                    Color32::WHITE,
                    Stroke::new(1.5_f32, theme::accent()),
                    egui::StrokeKind::Middle,
                );
            }
        }
        if let (BoardTool::Eraser, Some(p)) = (r.tool, response.hover_pos()) {
            painter.circle_stroke(p, 8.0, Stroke::new(1.0_f32, theme::text_dim()));
        }
        if r.board.items.is_empty() && r.board.marks.is_empty() {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Drop pictures here, paste one (Ctrl+V)\nor add them with +",
                egui::FontId::proportional(13.0),
                theme::text_dim(),
            );
        }
        if layer_hover || files_hover {
            painter.rect_stroke(rect.shrink(2.0), 0.0, Stroke::new(2.0_f32, theme::accent()), egui::StrokeKind::Inside);
        }
        painter.text(
            rect.left_bottom() + Vec2::new(6.0, -4.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{:.0}%{}", r.zoom * 100.0, if r.mirror { " · mirrored" } else { "" }),
            egui::FontId::proportional(11.0),
            theme::text_dim(),
        );
        if r.drag.is_some() || !r.decoding.is_empty() {
            ctx.request_repaint();
        }
    }

    /// Pan, zoom, select, move, scale, turn, draw and erase.
    fn board_input(&mut self, ui: &egui::Ui, rect: Rect, response: &egui::Response, pointer: Option<Pos2>) {
        let r = &mut self.docs.reference;
        // Zoom with the wheel (or pinch) about the pointer; pan with two fingers.
        if response.hovered() {
            let (scroll, touch) = ui.input(|i| (i.raw_scroll_delta.y, i.multi_touch()));
            let mut factor = (scroll * 0.0015).exp();
            if let Some(t) = &touch {
                factor *= t.zoom_delta;
                let d = t.translation_delta / r.zoom;
                r.center -= Vec2::new(if r.mirror { -d.x } else { d.x }, d.y);
            }
            if (factor - 1.0).abs() > 1e-4 {
                if let Some(p) = pointer {
                    let before = r.to_board(rect, p);
                    r.zoom = (r.zoom * factor).clamp(0.01, 32.0);
                    let after = r.to_board(rect, p);
                    r.center += Vec2::new(before[0] - after[0], before[1] - after[1]);
                }
            }
        }
        let (primary_pressed, middle, shift, cmd, released) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.middle_down(),
                i.modifiers.shift,
                i.modifiers.command || i.modifiers.ctrl,
                i.pointer.any_released(),
            )
        });
        let space = ui.input(|i| i.key_down(Key::Space));
        // Start of a drag (or a click).
        if response.hovered() && (primary_pressed || (middle && r.drag.is_none())) && r.drag.is_none() {
            let Some(p) = pointer else { return };
            let b = r.to_board(rect, p);
            r.drag = Some(if middle || space {
                Drag::Pan
            } else {
                match r.tool {
                    BoardTool::Pen => {
                        r.drawing = vec![b];
                        Drag::Draw
                    }
                    BoardTool::Eraser => {
                        r.checkpoint();
                        r.board.erase_at(b, 8.0 / r.zoom);
                        Drag::Erase
                    }
                    BoardTool::Select => {
                        let handle = r.handles(rect).and_then(|(id, corners, turn)| {
                            let item = r.board.find(id)?;
                            let centre = r.to_screen(rect, item.pos);
                            if p.distance(turn) <= HANDLE * 1.5 {
                                let angle = (p - centre).angle();
                                return Some(Drag::Rotate { id, start: angle, rotation: item.rotation });
                            }
                            corners.iter().any(|c| p.distance(*c) <= HANDLE * 1.5).then(|| Drag::Scale {
                                id,
                                start: p.distance(centre).max(1.0),
                                scale: item.scale,
                            })
                        });
                        match handle {
                            Some(d) => {
                                r.checkpoint();
                                d
                            }
                            None => match r.board.item_at(b) {
                                Some(id) => {
                                    if shift || cmd {
                                        if let Some(k) = r.selection.iter().position(|s| *s == id) {
                                            r.selection.remove(k);
                                        } else {
                                            r.selection.push(id);
                                        }
                                    } else if !r.selection.contains(&id) {
                                        r.selection = vec![id];
                                    }
                                    r.checkpoint();
                                    let origins =
                                        r.selection.iter().filter_map(|id| r.board.find(*id).map(|i| (*id, i.pos))).collect();
                                    Drag::Move { start: b, origins }
                                }
                                None => {
                                    if !shift && !cmd {
                                        r.selection.clear();
                                    }
                                    Drag::Pan
                                }
                            },
                        }
                    }
                }
            });
        }
        // During the drag.
        if let (Some(drag), Some(p)) = (&r.drag, pointer) {
            let b = r.to_board(rect, p);
            match drag {
                Drag::Pan => {
                    let d = response.drag_delta() / r.zoom;
                    r.center -= Vec2::new(if r.mirror { -d.x } else { d.x }, d.y);
                }
                Drag::Move { start, origins } => {
                    let (dx, dy) = (b[0] - start[0], b[1] - start[1]);
                    for (id, o) in origins.clone() {
                        if let Some(item) = r.board.find_mut(id) {
                            item.pos = [o[0] + dx, o[1] + dy];
                        }
                    }
                }
                Drag::Scale { id, start, scale } => {
                    let (id, start, scale) = (*id, *start, *scale);
                    if let Some(centre) = r.board.find(id).map(|i| r.to_screen(rect, i.pos)) {
                        let s = (scale * p.distance(centre) / start).clamp(0.005, 200.0);
                        r.board.find_mut(id).expect("found").scale = s;
                    }
                }
                Drag::Rotate { id, start, rotation } => {
                    let (id, start, rotation) = (*id, *start, *rotation);
                    if let Some(centre) = r.board.find(id).map(|i| r.to_screen(rect, i.pos)) {
                        let turn = (p - centre).angle() - start;
                        // The view may be mirrored: a turn on screen is the opposite on the board.
                        let mut a = rotation + if r.mirror { -turn } else { turn };
                        if shift {
                            let step = 15f32.to_radians();
                            a = (a / step).round() * step;
                        }
                        r.board.find_mut(id).expect("found").rotation = a;
                    }
                }
                Drag::Draw => {
                    let last = *r.drawing.last().expect("started with a point");
                    if ((b[0] - last[0]).powi(2) + (b[1] - last[1]).powi(2)).sqrt() * r.zoom >= 1.5 {
                        r.drawing.push(b);
                    }
                }
                Drag::Erase => {
                    r.board.erase_at(b, 8.0 / r.zoom);
                }
            }
        }
        // End.
        if released && r.drag.is_some() && !ui.input(|i| i.pointer.any_down()) {
            match r.drag.take() {
                Some(Drag::Draw) => {
                    let points = std::mem::take(&mut r.drawing);
                    r.checkpoint();
                    let width = r.pen_width / r.zoom;
                    let [cr, cg, cb, ca] = r.pen_color.to_srgba_unmultiplied();
                    r.board.add_mark(points, [cr, cg, cb, ca], width);
                }
                // A click or a drag that changed nothing leaves no undo step.
                Some(Drag::Move { .. } | Drag::Scale { .. } | Drag::Rotate { .. } | Drag::Erase) if r.undo.last() == Some(&r.board) => {
                    r.undo.pop();
                }
                _ => {}
            }
        }
    }

    /// The right-click menu; returns a document to switch to.
    fn board_context_menu(&mut self, response: &egui::Response) -> Option<u64> {
        let rect = self.docs.reference.rect?;
        if response.secondary_clicked() {
            let r = &mut self.docs.reference;
            if let Some(p) = response.interact_pointer_pos() {
                match r.board.item_at(r.to_board(rect, p)) {
                    Some(id) if !r.selection.contains(&id) => r.selection = vec![id],
                    Some(_) => {}
                    None => r.selection.clear(),
                }
            }
        }
        let mut edit = None;
        response.context_menu(|ui| {
            let r = &mut self.docs.reference;
            if r.selection.is_empty() {
                if ui.button(tr("Paste picture")).clicked() {
                    ui.close_menu();
                    r.paste_report(None);
                }
                if ui.button(tr("Select all")).clicked() {
                    ui.close_menu();
                    r.select_all();
                }
                if ui.button(tr("Fit")).clicked() {
                    ui.close_menu();
                    r.fit_pending = true;
                }
                return;
            }
            let first = r.selection[0];
            let item = r.board.find(first).cloned();
            if let Some(Source::Document { slot }) = item.as_ref().map(|i| &i.source) {
                if ui.button(tr("Edit this document")).clicked() {
                    ui.close_menu();
                    edit = Some(*slot);
                }
                ui.separator();
            }
            if r.media.is_moving(first) {
                let label = if r.media.is_paused(first) { "Play  (Space)" } else { "Pause  (Space)" };
                if ui.button(label).clicked() {
                    ui.close_menu();
                    r.media.toggle(&r.selection.clone());
                }
                if ui.button(tr("Restart  (Home)")).clicked() {
                    ui.close_menu();
                    r.media.seek(&r.selection.clone(), Some(0), 0);
                }
                ui.separator();
            }
            if ui.button(tr("Bring to front")).clicked() {
                ui.close_menu();
                r.checkpoint();
                r.board.restack(&r.selection.clone(), true);
            }
            if ui.button(tr("Send to back")).clicked() {
                ui.close_menu();
                r.checkpoint();
                r.board.restack(&r.selection.clone(), false);
            }
            if ui.button(tr("Mirror  (H)")).clicked() {
                ui.close_menu();
                r.edit_selection(|i| i.flip = !i.flip);
            }
            let grey = item.as_ref().is_some_and(|i| i.grey);
            if ui.selectable_label(grey, tr("Grey")).clicked() {
                ui.close_menu();
                r.edit_selection(|i| i.grey = !grey);
            }
            if let Some(item) = &item {
                let mut opacity = item.opacity * 100.0;
                if ui.add(egui::Slider::new(&mut opacity, 5.0..=100.0).text(tr("Opacity")).suffix("%")).changed() {
                    if !r.undo.last().is_some_and(|b| b.find(first).is_some_and(|i| i.opacity != item.opacity)) {
                        r.checkpoint();
                    }
                    let o = opacity / 100.0;
                    for id in r.selection.clone() {
                        if let Some(i) = r.board.find_mut(id) {
                            i.opacity = o;
                        }
                    }
                }
            }
            if ui.button(tr("Reset size and turn")).clicked() {
                ui.close_menu();
                r.edit_selection(|i| {
                    i.scale = 1.0;
                    i.rotation = 0.0;
                });
            }
            if ui.button(tr("Duplicate  (Ctrl+D)")).clicked() {
                ui.close_menu();
                r.duplicate_selection();
            }
            if ui.button(tr("Lock")).on_hover_text(tr("Clicks go through it; unlock from Board → Unlock all")).clicked() {
                ui.close_menu();
                r.edit_selection(|i| i.locked = true);
                r.selection.clear();
            }
            let has_marks = r.board.marks.iter().any(|m| m.on.is_some_and(|on| r.selection.contains(&on)));
            if ui.add_enabled(has_marks, egui::Button::new(tr("Clear its marks"))).clicked() {
                ui.close_menu();
                r.checkpoint();
                let sel = r.selection.clone();
                r.board.marks.retain(|m| m.on.is_none_or(|on| !sel.contains(&on)));
            }
            ui.separator();
            if ui.button(tr("Remove  (Delete)")).clicked() {
                ui.close_menu();
                r.delete_selection();
            }
        });
        edit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscale_averages_blocks() {
        // 4 × 2: left half opaque red, right half transparent.
        let mut px = vec![0u8; 4 * 2 * 4];
        for y in 0..2 {
            for x in 0..2 {
                let i = (y * 4 + x) * 4;
                px[i] = 255;
                px[i + 3] = 255;
            }
        }
        let img = downscale(&px, 4, 2, 2);
        assert_eq!(img.size, [2, 1]);
        assert_eq!(img.pixels[0], Color32::from_rgba_premultiplied(255, 0, 0, 255));
        assert_eq!(img.pixels[1], Color32::TRANSPARENT);
        assert_eq!(downscale(&px, 4, 2, 100).size, [4, 2], "small pictures stay as they are");
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::RgbaImage::from_pixel(w, h, image::Rgba([200, 100, 50, 255]))
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn view_maps_both_ways_even_mirrored() {
        let mut r = ReferenceView::default();
        let rect = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(400.0, 300.0));
        r.center = Vec2::new(50.0, -20.0);
        r.zoom = 2.5;
        for mirror in [false, true] {
            r.mirror = mirror;
            let p = [123.0, 45.0];
            let back = r.to_board(rect, r.to_screen(rect, p));
            assert!((back[0] - p[0]).abs() < 1e-3 && (back[1] - p[1]).abs() < 1e-3);
        }
        assert!(r.to_screen(rect, [60.0, -20.0]).x < rect.center().x, "mirrored: right of centre shows left");
    }

    #[test]
    fn adds_pictures_with_undo_and_keys() {
        let mut r = ReferenceView { rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 200.0))), ..Default::default() };
        let id = r.add_image("a", "PNG", png(40, 20), Some([5.0, 5.0])).unwrap();
        let item = r.board.find(id).unwrap();
        assert_eq!((item.size, item.scale), ([40.0, 20.0], 1.0));
        assert!(matches!(&item.source, Source::Image { ext, .. } if ext == "png"));
        // Bigger than the view: scaled to fit it.
        let big = r.add_image("b", "png", png(1000, 100), None).unwrap();
        assert!((r.board.find(big).unwrap().scale - 0.16).abs() < 1e-4);
        assert!(r.add_image("c", "png", b"nope".to_vec(), None).is_err());
        assert_eq!(r.selection, [big]);
        let key = |k: Key, cmd: bool| egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers { ctrl: cmd, command: cmd, ..Default::default() },
        };
        assert!(board_key(&key(Key::Delete, false)) && board_key(&key(Key::Z, true)) && !board_key(&key(Key::S, true)));
        r.key(&key(Key::H, false), None);
        assert!(r.board.find(big).unwrap().flip);
        r.key(&key(Key::Delete, false), None);
        assert_eq!(r.board.items.len(), 1);
        r.key(&key(Key::Z, true), None);
        assert_eq!(r.board.items.len(), 2);
        r.key(&key(Key::A, true), None);
        r.key(&key(Key::D, true), None);
        assert_eq!(r.board.items.len(), 4);
        r.key(&key(Key::P, false), None);
        assert_eq!(r.tool, BoardTool::Pen);
        assert!(decode(&png(3, 2), true).is_some_and(|i| i.size == [3, 2] && i.pixels[0].r() == i.pixels[0].g()));
    }

    #[test]
    fn documents_land_as_pictures() {
        let dir = std::env::temp_dir().join(format!("painter-board-docs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sheet.kra");
        painter_core::kra::save(&painter_core::Document::new(120, 80), &path).unwrap();
        let mut r = ReferenceView::default();
        let id = r.add_image_file(&path, None).unwrap();
        let item = r.board.find(id).unwrap();
        assert_eq!(item.size, [120.0, 80.0]);
        assert!(matches!(&item.source, Source::Image { name, ext, .. } if name == "sheet.kra" && ext == "png"));
        assert!(is_board_file(std::path::Path::new("a.PSD")) && !is_board_file(std::path::Path::new("a.txt")));
        std::fs::remove_dir_all(&dir).ok();
    }
}
