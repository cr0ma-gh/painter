//! File dialogs. On the desktop these are the system's (`rfd`). iPadOS apps
//! only see their own sandbox, so there "save" writes into the app's
//! Documents folder (shown under Painter in the Files app) and "open" points
//! at File → Documents, which lists what is in that folder.

use std::path::PathBuf;
use std::sync::Mutex;

#[cfg(not(target_os = "ios"))]
pub(crate) use rfd::FileDialog;

#[cfg(target_os = "ios")]
pub(crate) use sandbox::FileDialog;

/// A message for the status bar from a dialog that couldn't open (iPadOS).
static NOTICE: Mutex<Option<String>> = Mutex::new(None);

pub(crate) fn take_notice() -> Option<String> {
    NOTICE.lock().ok().and_then(|mut n| n.take())
}

#[allow(dead_code)]
fn notice(text: String) {
    if let Ok(mut n) = NOTICE.lock() {
        *n = Some(text);
    }
}

/// The app's own documents folder: iPadOS's Documents, `None` on the
/// desktop (where the system dialogs reach everything).
pub(crate) fn documents_dir() -> Option<PathBuf> {
    if cfg!(target_os = "ios") {
        let dir = PathBuf::from(std::env::var_os("HOME")?).join("Documents");
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    } else {
        None
    }
}

/// Files in the documents folder Painter can open, newest first.
pub(crate) fn documents() -> Vec<PathBuf> {
    let Some(dir) = documents_dir() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && kind_of(p).is_some())
        .map(|p| (std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), p))
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    files.into_iter().map(|(_, p)| p).collect()
}

/// What opening a file does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileKind {
    /// A project or an image: opens as a document.
    Document,
    /// A 3D model: a new 3D layer.
    Model,
    /// Brushes to import.
    Brushes,
}

pub(crate) fn kind_of(path: &std::path::Path) -> Option<FileKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "painter" | "psd" | "png" | "jpg" | "jpeg" => Some(FileKind::Document),
        "blend" | "obj" => Some(FileKind::Model),
        _ if brush_io::is_brush_file(path) => Some(FileKind::Brushes),
        _ => None,
    }
}

#[cfg(target_os = "ios")]
mod sandbox {
    use std::path::PathBuf;

    /// The subset of `rfd::FileDialog` the app uses, for the sandbox.
    #[derive(Default)]
    pub(crate) struct FileDialog {
        name: Option<String>,
        extensions: Vec<String>,
    }

    impl FileDialog {
        pub(crate) fn new() -> Self {
            Self::default()
        }

        pub(crate) fn add_filter(mut self, _name: impl Into<String>, extensions: &[impl ToString]) -> Self {
            self.extensions.extend(extensions.iter().map(|e| e.to_string()));
            self
        }

        pub(crate) fn set_file_name(mut self, name: impl Into<String>) -> Self {
            self.name = Some(name.into());
            self
        }

        /// Saving goes into Documents, under the suggested name.
        pub(crate) fn save_file(self) -> Option<PathBuf> {
            let dir = super::documents_dir()?;
            let name = self.name.unwrap_or_else(|| format!("untitled.{}", self.extensions.first().map_or("painter", |e| e.as_str())));
            Some(dir.join(name))
        }

        pub(crate) fn pick_file(self) -> Option<PathBuf> {
            super::notice("Copy files into Painter's folder in the Files app, then open them from File → Documents".into());
            None
        }

        pub(crate) fn pick_files(self) -> Option<Vec<PathBuf>> {
            self.pick_file().map(|p| vec![p])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(kind_of(std::path::Path::new("a/b.PNG")), Some(FileKind::Document));
        assert_eq!(kind_of(std::path::Path::new("model.blend")), Some(FileKind::Model));
        assert_eq!(kind_of(std::path::Path::new("x.abr")), Some(FileKind::Brushes));
        assert_eq!(kind_of(std::path::Path::new("notes.txt")), None);
        if !cfg!(target_os = "ios") {
            assert!(documents_dir().is_none());
        }
    }
}
