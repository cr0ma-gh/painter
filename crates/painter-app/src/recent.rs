//! Recent files: what was opened or saved lately, newest first, each with a
//! small picture (made from the document on a thread of its own and kept in
//! the app's data folder). The welcome screen shows them as cards; File →
//! Open Recent lists them.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui;
use serde::{Deserialize, Serialize};

/// How many are kept.
const KEEP: usize = 30;
/// The pictures' longest side, in pixels.
const THUMB: u32 = 256;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct RecentFile {
    pub path: String,
    /// Seconds since 1970.
    pub when: u64,
}

impl RecentFile {
    pub(crate) fn name(&self) -> String {
        Path::new(&self.path).file_name().map_or_else(|| self.path.clone(), |n| n.to_string_lossy().into_owned())
    }

    pub(crate) fn folder(&self) -> String {
        Path::new(&self.path).parent().map_or_else(String::new, |p| p.to_string_lossy().into_owned())
    }

    pub(crate) fn exists(&self) -> bool {
        Path::new(&self.path).exists()
    }
}

#[derive(Default)]
pub(crate) struct Recent {
    pub files: Vec<RecentFile>,
    /// Pictures loaded for the screen, by path (`None`: there's none).
    textures: HashMap<String, Option<egui::TextureHandle>>,
}

fn data_dir() -> Option<PathBuf> {
    crate::files::app_data_dir()
}

fn list_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join("recent.json"))
}

/// Where a file's picture is kept.
fn thumb_path(path: &str) -> Option<PathBuf> {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    data_dir().map(|d| d.join("recent-thumbnails").join(format!("{:016x}.png", h.finish())))
}

/// Inserts `path` at the front of `files` (once), keeping at most `KEEP`.
pub(crate) fn add_front(files: &mut Vec<RecentFile>, path: &str, when: u64) {
    files.retain(|f| f.path != path);
    files.insert(0, RecentFile { path: path.to_string(), when });
    files.truncate(KEEP);
}

impl Recent {
    pub(crate) fn load() -> Self {
        let files = list_path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { files, textures: HashMap::new() }
    }

    fn save(&self) {
        if let Some(path) = list_path() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(json) = serde_json::to_vec_pretty(&self.files) {
                let _ = std::fs::write(path, json);
            }
        }
    }

    /// `path` was opened or saved, showing `doc`: first in the list, its picture redone.
    pub(crate) fn touch(&mut self, path: &Path, doc: &painter_core::Document) {
        let text = path.to_string_lossy().into_owned();
        let when = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        add_front(&mut self.files, &text, when);
        self.save();
        self.textures.remove(&text);
        let Some(out) = thumb_path(&text) else { return };
        // From a copy (tiles are shared, so it's cheap), off the UI thread.
        let doc = doc.without_drafts();
        std::thread::spawn(move || {
            let Ok(png) = painter_core::project::thumbnail(&doc, THUMB) else { return };
            if let Some(dir) = out.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(out, png);
        });
    }

    pub(crate) fn remove(&mut self, path: &str) {
        self.files.retain(|f| f.path != path);
        self.textures.remove(path);
        if let Some(t) = thumb_path(path) {
            let _ = std::fs::remove_file(t);
        }
        self.save();
    }

    pub(crate) fn clear(&mut self) {
        for f in std::mem::take(&mut self.files) {
            if let Some(t) = thumb_path(&f.path) {
                let _ = std::fs::remove_file(t);
            }
        }
        self.textures.clear();
        self.save();
    }

    /// The picture of `path` for the screen, loaded once (made when it was last opened or saved).
    pub(crate) fn texture(&mut self, ctx: &egui::Context, path: &str) -> Option<egui::TextureId> {
        if let Some(t) = self.textures.get(path) {
            return t.as_ref().map(|t| t.id());
        }
        let picture =
            thumb_path(path).and_then(|p| std::fs::read(p).ok()).and_then(|png| image::load_from_memory(&png).ok()).map(|img| {
                let img = img.to_rgba8();
                let size = [img.width() as usize, img.height() as usize];
                ctx.load_texture(
                    format!("recent-{path}"),
                    egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()),
                    egui::TextureOptions::LINEAR,
                )
            });
        // Not made yet (just opened): look again in a moment.
        let found = picture.is_some();
        self.textures.insert(path.to_string(), picture);
        if !found {
            self.textures.remove(path);
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        self.textures.get(path).and_then(|t| t.as_ref().map(|t| t.id()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_first_once_and_capped() {
        let mut files = Vec::new();
        add_front(&mut files, "a.painter", 1);
        add_front(&mut files, "b.psd", 2);
        add_front(&mut files, "a.painter", 3);
        assert_eq!(files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["a.painter", "b.psd"]);
        assert_eq!(files[0].when, 3);
        for i in 0..50 {
            add_front(&mut files, &format!("{i}.png"), i);
        }
        assert_eq!(files.len(), KEEP);
        assert_eq!(files[0].path, "49.png");
        assert_eq!(RecentFile { path: "/x/y/z.clip".into(), when: 0 }.name(), "z.clip");
    }
}
