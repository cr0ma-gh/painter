//! The brush list, kept between launches.
//!
//! Every brush (built-in ones, imported ones, edits, order, deletions) lives
//! in one `.rustbrush` file in the app's data folder, tips and textures
//! included. It's loaded at startup and rewritten on a worker thread a moment
//! after the list changes.

use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use brush_io::BrushPreset;

const FILE_NAME: &str = "brushes.rustbrush";
/// How often the list is compared with what was last written.
const CHECK_EVERY: Duration = Duration::from_secs(2);

pub(crate) struct BrushLibrary {
    path: Option<PathBuf>,
    /// The list as last written (or loaded).
    saved: Vec<BrushPreset>,
    checked: Option<Instant>,
    writing: Option<JoinHandle<Result<(), String>>>,
    /// Why the last write failed, for the status bar (taken once).
    pub(crate) error: Option<String>,
}

impl BrushLibrary {
    /// The saved brushes, if there are any.
    pub(crate) fn load() -> (Self, Option<Vec<BrushPreset>>) {
        let path = crate::files::app_data_dir().map(|d| d.join(FILE_NAME));
        let mut error = None;
        let brushes = path.as_ref().filter(|p| p.is_file()).and_then(|p| {
            match std::fs::read(p).map_err(|e| e.to_string()).and_then(|b| brush_io::native::read(&b).map_err(|e| e.to_string())) {
                Ok(list) if !list.is_empty() => Some(list),
                Ok(_) => None,
                Err(err) => {
                    // Keep the unreadable file for a look, instead of overwriting it.
                    let _ = std::fs::rename(p, p.with_extension("rustbrush.bad"));
                    error = Some(format!("The saved brushes couldn't be read ({err}); starting with the default ones"));
                    None
                }
            }
        });
        let saved = brushes.clone().unwrap_or_default();
        (Self { path, saved, checked: None, writing: None, error }, brushes)
    }

    /// Writes the list if it changed. Checks at most every couple of seconds
    /// unless `now` (quitting), which also waits for the write to finish.
    pub(crate) fn autosave(&mut self, brushes: &[BrushPreset], now: bool) {
        let Some(path) = self.path.clone() else { return };
        if let Some(handle) = self.writing.take_if(|h| now || h.is_finished()) {
            if let Ok(Err(err)) = handle.join() {
                self.error = Some(format!("Couldn't save the brushes: {err}"));
                // Written again at the next check.
                self.saved.clear();
            }
        }
        if self.writing.is_some() {
            return;
        }
        if !now && self.checked.is_some_and(|t| t.elapsed() < CHECK_EVERY) {
            return;
        }
        self.checked = Some(Instant::now());
        if self.saved.as_slice() == brushes {
            return;
        }
        self.saved = brushes.to_vec();
        let list = self.saved.clone();
        let handle = std::thread::spawn(move || -> Result<(), String> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            // Through a temporary file, so a crash mid-write keeps the old list.
            let tmp = path.with_extension("tmp");
            brush_io::native::save(&list, &tmp).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
        });
        if now {
            if let Ok(Err(err)) = handle.join() {
                self.error = Some(format!("Couldn't save the brushes: {err}"));
            }
        } else {
            self.writing = Some(handle);
        }
    }
}
