//! File object layers (Clip Studio's file objects, Photoshop's linked smart
//! objects): Layer → New File Object Layer places an outside picture or
//! document (PNG, JPEG, BMP, GIF, PSD, Krita, OpenRaster, Clip Studio,
//! Painter) on the canvas. The layer shows the file, flattened; the file stays
//! where it is and the project keeps a link to it.
//!
//! - Moving, scaling, turning or distorting it with the Transform tool
//!   draws it again from the file, so it never loses quality.
//! - When the file changes (saved from another app or another Painter tab),
//!   the layer follows within a couple of seconds; "Update" does it at once.
//! - "Open original" edits the file (in Painter for documents it opens, in
//!   the system's app otherwise); "Replace file" links another one in the
//!   same place; "Rasterize" turns the layer into ordinary pixels to paint on.
//!
//! The layer's pixels are saved with the project too, so it opens (and
//! exports) even where the file isn't.

use std::collections::HashMap;
use crate::i18n::tr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use eframe::egui;
use painter_core::{FileObject, Layer, LayerId, LayerKind, TileStore};
use painter_render::Plane;
use transform_tools::{commit, Image, Interpolation, Mat3, Placement};

use crate::app::PainterApp;

/// Files a file object can show.
pub(crate) const FILE_OBJECT_EXTENSIONS: [&str; 10] = ["png", "jpg", "jpeg", "bmp", "gif", "psd", "kra", "ora", "clip", "painter"];

/// How often linked files are looked at for changes.
const CHECK_EVERY: Duration = Duration::from_secs(2);

/// Decoded pictures of linked files, by path and modification time.
#[derive(Default)]
pub(crate) struct FileObjectCache {
    pictures: HashMap<PathBuf, (u64, Arc<Image>)>,
    checked: Option<Instant>,
}

/// The full path, without the `\\?\` prefix `canonicalize` adds on Windows.
fn absolute(path: &Path) -> PathBuf {
    let full = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = full.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => full,
    }
}

/// The file's modification time, in seconds.
fn modified(path: &Path) -> u64 {
    std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())
}

/// The picture a file shows: premultiplied RGBA at its own size.
fn load_picture(path: &Path) -> Result<Image, String> {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    let (width, height, mut rgba, premultiplied) = match ext.as_str() {
        "psd" | "kra" | "ora" | "clip" | "painter" => {
            let (doc, _, _) = crate::document_manager::session_store::load_file(path)?;
            (doc.width, doc.height, painter_core::io::flatten(&doc.without_drafts()), true)
        }
        _ => {
            let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            if painter_core::gif::is_gif(&bytes) {
                let gif = painter_core::gif::decode(&bytes)?;
                let first = gif.frames.into_iter().next().ok_or("empty GIF")?;
                (gif.width, gif.height, first.rgba, false)
            } else {
                let img = image::load_from_memory(&bytes).map_err(|e| format!("{}: {e}", path.display()))?.to_rgba8();
                (img.width(), img.height(), img.into_raw(), false)
            }
        }
    };
    if !premultiplied {
        for px in rgba.as_chunks_mut::<4>().0 {
            *px = painter_core::blend::premultiply(*px);
        }
    }
    Ok(Image { width, height, rgba })
}

/// The picture drawn where `placement` puts it on a `canvas`-sized layer.
fn render(picture: &Image, placement: &[[f64; 3]; 3], canvas: (u32, u32)) -> TileStore {
    commit(&TileStore::new(), picture, &Placement::Matrix(Mat3(*placement)), Interpolation::Bicubic, canvas)
}

/// Where a picture of `size` goes at first: centred, shrunk to fit the canvas if bigger.
pub(crate) fn initial_placement(size: [u32; 2], canvas: (u32, u32)) -> [[f64; 3]; 3] {
    let (w, h) = (size[0].max(1) as f64, size[1].max(1) as f64);
    let s = (canvas.0 as f64 / w).min(canvas.1 as f64 / h).min(1.0);
    let (tx, ty) = ((canvas.0 as f64 - w * s) / 2.0, (canvas.1 as f64 - h * s) / 2.0);
    [[s, 0.0, tx], [0.0, s, ty], [0.0, 0.0, 1.0]]
}

impl FileObjectCache {
    /// The file's picture, decoded once per version of the file.
    fn picture(&mut self, path: &Path) -> Result<(u64, Arc<Image>), String> {
        let stamp = modified(path);
        if let Some((m, img)) = self.pictures.get(path) {
            if *m == stamp {
                return Ok((stamp, img.clone()));
            }
        }
        let img = Arc::new(load_picture(path)?);
        self.pictures.insert(path.to_path_buf(), (stamp, img.clone()));
        Ok((stamp, img))
    }
}

impl PainterApp {
    /// Layer → New File Object Layer.
    pub(crate) fn new_file_object_dialog(&mut self) {
        if let Some(path) = crate::files::FileDialog::new().add_filter("Pictures and documents", &FILE_OBJECT_EXTENSIONS).pick_file() {
            self.add_file_object(&path);
        }
    }

    /// A new file object layer showing `path`, above the active layer.
    pub(crate) fn add_file_object(&mut self, path: &Path) {
        let path = absolute(path);
        let (stamp, picture) = match self.file_objects.picture(&path) {
            Ok(p) => p,
            Err(e) => {
                self.status = format!("Couldn't place {}: {e}", path.display());
                return;
            }
        };
        let canvas = self.doc.canvas();
        let size = [picture.width, picture.height];
        let placement = initial_placement(size, canvas);
        self.checkpoint();
        let id = self.doc.alloc_id();
        let name = path.file_name().map_or_else(|| "File object".into(), |n| n.to_string_lossy().into_owned());
        let mut layer = Layer::raster(id, name);
        layer.kind = LayerKind::Raster(render(&picture, &placement, canvas));
        layer.file_object = Some(FileObject { path: path.to_string_lossy().into_owned(), placement, size, modified: stamp });
        self.doc.add_layer(layer);
        self.doc.active = id;
        self.upload_plane(Plane::Color(id));
        self.status = format!("File object: {} (linked; the Transform tool keeps it sharp)", path.display());
    }

    /// Draws file object `id` again from its file (newly read if it changed).
    pub(crate) fn redraw_file_object(&mut self, id: LayerId) -> Result<(), String> {
        let canvas = self.doc.canvas();
        let Some(fo) = self.doc.find(id).and_then(|l| l.file_object.clone()) else { return Ok(()) };
        let (stamp, picture) = self.file_objects.picture(Path::new(&fo.path))?;
        // A changed file may have a new size: keep its place, scaled to the old box.
        let mut placement = fo.placement;
        if [picture.width, picture.height] != fo.size && picture.width > 0 && picture.height > 0 {
            let fit = Mat3::scale(fo.size[0] as f64 / picture.width as f64, fo.size[1] as f64 / picture.height as f64);
            placement = (Mat3(placement) * fit).0;
        }
        let tiles = render(&picture, &placement, canvas);
        if let Some(layer) = self.doc.find_mut(id) {
            layer.kind = LayerKind::Raster(tiles);
            layer.file_object = Some(FileObject { modified: stamp, placement, ..fo });
        }
        self.upload_plane(Plane::Color(id));
        self.dirty = true;
        Ok(())
    }

    /// Every couple of seconds: file objects whose file changed are drawn again.
    pub(crate) fn file_objects_tick(&mut self) {
        if self.file_objects.checked.is_some_and(|t| t.elapsed() < CHECK_EVERY) {
            return;
        }
        self.file_objects.checked = Some(Instant::now());
        let mut stale = Vec::new();
        fn walk(layers: &[Layer], out: &mut Vec<LayerId>) {
            for l in layers {
                if let Some(fo) = &l.file_object {
                    let now = modified(Path::new(&fo.path));
                    if now != 0 && now != fo.modified {
                        out.push(l.id);
                    }
                }
                if let LayerKind::Folder(f) = &l.kind {
                    walk(&f.children, out);
                }
            }
        }
        walk(&self.doc.layers, &mut stale);
        for id in stale {
            let name = self.doc.find(id).map(|l| l.name.clone()).unwrap_or_default();
            self.status = match self.redraw_file_object(id) {
                Ok(()) => format!("“{name}” updated from its file"),
                Err(e) => format!("“{name}”: {e}"),
            };
        }
    }

    /// The Transform tool applied to a file object: its new place, drawn from the file.
    /// `canvas_map` takes the layer's old canvas pixels to the new ones. False for
    /// a distortion a single matrix can't hold (then the layer becomes pixels).
    pub(crate) fn transform_file_object(&mut self, id: LayerId, canvas_map: Option<Mat3>) -> bool {
        let Some(fo) = self.doc.find(id).and_then(|l| l.file_object.clone()) else { return false };
        let Some(map) = canvas_map else {
            if let Some(layer) = self.doc.find_mut(id) {
                layer.file_object = None;
            }
            self.status = tr("The mesh warp turned the file object into pixels (it's no longer linked)").into();
            return false;
        };
        if let Some(layer) = self.doc.find_mut(id) {
            layer.file_object = Some(FileObject { placement: (map * Mat3(fo.placement)).0, ..fo });
        }
        if let Err(e) = self.redraw_file_object(id) {
            self.status = format!("File object: {e}");
            return false;
        }
        true
    }

    /// The Layers panel's File object section for the active layer.
    pub(crate) fn file_object_panel(&mut self, ui: &mut egui::Ui) {
        let Some(layer) = self.doc.active_layer() else { return };
        let Some(fo) = layer.file_object.clone() else { return };
        let id = layer.id;
        ui.separator();
        ui.label(egui::RichText::new(tr("File object")).strong());
        let exists = Path::new(&fo.path).exists();
        ui.small(&fo.path).on_hover_text(tr("The linked file. Its pixels are in the project too"));
        if !exists {
            ui.colored_label(
                egui::Color32::from_rgb(240, 190, 90),
                "The file isn't there: showing the last version saved in the project",
            );
        }
        ui.small(format!("{} × {} px · Transform tool: moves and scales it without losing quality", fo.size[0], fo.size[1]));
        let (mut update, mut open, mut replace, mut rasterize) = (false, false, false, false);
        ui.horizontal_wrapped(|ui| {
            update = ui.add_enabled(exists, egui::Button::new(tr("Update"))).on_hover_text(tr("Read the file again now")).clicked();
            open = ui.add_enabled(exists, egui::Button::new(tr("Open original"))).clicked();
            replace = ui.button(tr("Replace file...")).clicked();
            rasterize = ui.button(tr("Rasterize")).on_hover_text(tr("Ordinary pixels, to paint on (no longer linked)")).clicked();
        });
        if update {
            self.status = match self.redraw_file_object(id) {
                Ok(()) => "Updated from the file".into(),
                Err(e) => e,
            };
        }
        if open {
            self.open_file_object_original(Path::new(&fo.path));
        }
        if replace {
            if let Some(path) =
                crate::files::FileDialog::new().add_filter("Pictures and documents", &FILE_OBJECT_EXTENSIONS).pick_file()
            {
                let path = absolute(&path);
                self.checkpoint();
                if let Some(layer) = self.doc.find_mut(id) {
                    layer.file_object = Some(FileObject { path: path.to_string_lossy().into_owned(), modified: 0, ..fo });
                }
                if let Err(e) = self.redraw_file_object(id) {
                    self.status = format!("Couldn't use {}: {e}", path.display());
                }
            }
        }
        if rasterize {
            self.checkpoint();
            if let Some(layer) = self.doc.find_mut(id) {
                layer.file_object = None;
            }
            self.status = tr("Rasterized: an ordinary layer now").into();
        }
    }

    /// Opens the linked file: in Painter for its documents, else in the system's app.
    fn open_file_object_original(&mut self, path: &Path) {
        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
        if matches!(ext.as_str(), "psd" | "kra" | "ora" | "clip" | "painter") {
            self.open_path(path.to_path_buf());
            self.status = tr("Save it and the file object follows").into();
            return;
        }
        let program = if cfg!(windows) {
            ("cmd", vec!["/C".to_string(), "start".into(), String::new(), path.to_string_lossy().into_owned()])
        } else if cfg!(target_os = "macos") {
            ("open", vec![path.to_string_lossy().into_owned()])
        } else {
            ("xdg-open", vec![path.to_string_lossy().into_owned()])
        };
        self.status = match std::process::Command::new(program.0).args(&program.1).spawn() {
            Ok(_) => "Opened in its app: save it there and the file object follows".into(),
            Err(e) => format!("Couldn't open {}: {e}", path.display()),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_are_placed_fitted_and_redrawn_sharp() {
        // A 400 × 200 picture on a 200 × 200 canvas: halved, centred.
        let p = initial_placement([400, 200], (200, 200));
        assert_eq!(Mat3(p).apply([0.0, 0.0]), [0.0, 50.0]);
        assert_eq!(Mat3(p).apply([400.0, 200.0]), [200.0, 150.0]);
        // Small pictures keep their size.
        assert_eq!(Mat3(initial_placement([50, 50], (200, 200))).apply([0.0, 0.0]), [75.0, 75.0]);

        // A 2 × 2 checker scaled ×8, drawn from the source (bicubic): light and dark quarters.
        let mut rgba = Vec::new();
        for i in 0..4 {
            rgba.extend(if i == 0 || i == 3 { [255, 255, 255, 255] } else { [0, 0, 0, 255] });
        }
        let picture = Image { width: 2, height: 2, rgba };
        let tiles = render(&picture, &[[8.0, 0.0, 10.0], [0.0, 8.0, 10.0], [0.0, 0.0, 1.0]], (64, 64));
        assert!(tiles.pixel(13, 13)[0] > 200, "white quarter");
        assert!(tiles.pixel(21, 13)[0] < 60, "black quarter");
        assert_eq!(tiles.pixel(40, 40)[3], 0, "outside: empty");
    }
}
