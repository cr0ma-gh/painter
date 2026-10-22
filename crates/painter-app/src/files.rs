//! File dialogs and the app's own folders.
//!
//! On the desktop the dialogs are the system's (`rfd`) and answer at once.
//! On iPadOS they are UIKit's document picker (`native/ios_files.m`): opening
//! is asynchronous (the chosen files arrive a frame or more later through
//! [`take_picked`], tagged with the [`Purpose`] the dialog was opened for),
//! saving writes into the app's Documents folder (shown as "Painter" in the
//! Files app), and [`offer_export`] lets the user put a copy anywhere.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// What a file is being opened for, so a file picked later lands in the
/// right place.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Purpose {
    /// A document (or anything File → Open takes).
    #[default]
    Open,
    /// An image as a new layer.
    ImageLayer,
    Brushes,
    /// A model as a new 3D layer.
    NewModelLayer,
    /// A model added to the active 3D layer.
    AddModel,
    Theme,
    /// Pictures for the reference board.
    ReferenceImage,
    /// Photos or scans of pages for the scanner dialog.
    ScanPage,
}

/// The subset of `rfd::FileDialog` the app uses, plus the purpose.
/// (The name, extensions and purpose matter only on iPadOS.)
#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
pub(crate) struct FileDialog {
    #[cfg(not(target_os = "ios"))]
    inner: rfd::FileDialog,
    name: Option<String>,
    extensions: Vec<String>,
    purpose: Purpose,
}

impl FileDialog {
    pub(crate) fn new() -> Self {
        Self {
            #[cfg(not(target_os = "ios"))]
            inner: rfd::FileDialog::new(),
            name: None,
            extensions: Vec::new(),
            purpose: Purpose::Open,
        }
    }

    pub(crate) fn add_filter(mut self, name: impl Into<String>, extensions: &[impl ToString]) -> Self {
        let extensions: Vec<String> = extensions.iter().map(|e| e.to_string()).collect();
        #[cfg(not(target_os = "ios"))]
        {
            self.inner = self.inner.add_filter(name, &extensions);
        }
        #[cfg(target_os = "ios")]
        let _ = name.into();
        self.extensions.extend(extensions);
        self
    }

    pub(crate) fn set_file_name(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        #[cfg(not(target_os = "ios"))]
        {
            self.inner = self.inner.set_file_name(name.clone());
        }
        self.name = Some(name);
        self
    }

    /// What the chosen file is for (matters on iPadOS, where it arrives later).
    pub(crate) fn purpose(mut self, purpose: Purpose) -> Self {
        self.purpose = purpose;
        self
    }

    /// The chosen file; on iPadOS `None` now and the file comes through [`take_picked`].
    pub(crate) fn pick_file(self) -> Option<PathBuf> {
        #[cfg(not(target_os = "ios"))]
        return self.inner.pick_file();
        #[cfg(target_os = "ios")]
        {
            start_pick(self.purpose, false);
            None
        }
    }

    pub(crate) fn pick_files(self) -> Option<Vec<PathBuf>> {
        #[cfg(not(target_os = "ios"))]
        return self.inner.pick_files();
        #[cfg(target_os = "ios")]
        {
            start_pick(self.purpose, true);
            None
        }
    }

    /// Where to save; on iPadOS a file in Documents under the suggested name.
    pub(crate) fn save_file(self) -> Option<PathBuf> {
        #[cfg(not(target_os = "ios"))]
        return self.inner.save_file();
        #[cfg(target_os = "ios")]
        {
            let dir = documents_dir()?;
            let ext = self.extensions.first().map_or("painter", |e| e.as_str());
            Some(dir.join(self.name.unwrap_or_else(|| format!("untitled.{ext}"))))
        }
    }
}

/// What the open document picker is for.
#[cfg(target_os = "ios")]
static PENDING: Mutex<Purpose> = Mutex::new(Purpose::Open);

#[cfg(target_os = "ios")]
fn start_pick(purpose: Purpose, multiple: bool) {
    if let Ok(mut p) = PENDING.lock() {
        *p = purpose;
    }
    native::pick(multiple);
}

/// A file the user picked since the last call (iPadOS), with what it is for.
/// Projects opened for editing are copied into Documents first, so saving
/// them keeps the changes where the Files app can see them.
pub(crate) fn take_picked() -> Option<(Purpose, PathBuf)> {
    #[cfg(target_os = "ios")]
    {
        let path = native::take()?;
        let purpose = PENDING.lock().map(|p| *p).unwrap_or_default();
        let path = if purpose == Purpose::Open && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("painter")) {
            adopt(&path).unwrap_or(path)
        } else {
            path
        };
        Some((purpose, path))
    }
    #[cfg(not(target_os = "ios"))]
    None
}

/// Copies `path` into Documents under a free name.
#[allow(dead_code)]
fn adopt(path: &Path) -> Option<PathBuf> {
    let dir = documents_dir()?;
    if path.parent() == Some(dir.as_path()) {
        return Some(path.to_path_buf());
    }
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let ext = path.extension()?.to_string_lossy().into_owned();
    let mut target = dir.join(format!("{stem}.{ext}"));
    let mut k = 2;
    while target.exists() {
        target = dir.join(format!("{stem} {k}.{ext}"));
        k += 1;
    }
    std::fs::copy(path, &target).ok()?;
    Some(target)
}

/// Whether the document picker is on screen.
pub(crate) fn picking() -> bool {
    #[cfg(target_os = "ios")]
    return native::active();
    #[cfg(not(target_os = "ios"))]
    false
}

/// After writing an export: on iPadOS, offers to put a copy anywhere (Files,
/// iCloud Drive, another app). Nothing on the desktop, where the save dialog
/// already chose the place.
pub(crate) fn offer_export(path: &Path) {
    #[cfg(target_os = "ios")]
    native::export(path);
    #[cfg(not(target_os = "ios"))]
    let _ = path;
}

/// A message for the status bar from the file layer.
static NOTICE: Mutex<Option<String>> = Mutex::new(None);

pub(crate) fn take_notice() -> Option<String> {
    NOTICE.lock().ok().and_then(|mut n| n.take())
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

/// Where Clip Studio materials copied from a computer go (iPadOS:
/// Documents/Materials, created so it shows in the Files app). Brushes from
/// `.sut` files find their textures and tips there.
pub(crate) fn materials_dir() -> Option<PathBuf> {
    let dir = documents_dir()?.join("Materials");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Where settings are kept when eframe has no storage: iPadOS's
/// Library/Application Support (not shown in the Files app).
pub(crate) fn settings_file() -> Option<PathBuf> {
    if cfg!(target_os = "ios") {
        Some(app_data_dir()?.join("settings.json"))
    } else {
        None
    }
}

/// The app's own data folder (the brush library): `%APPDATA%\Painter` on
/// Windows, `Library/Application Support/Painter` on iPadOS and macOS,
/// `$XDG_DATA_HOME/Painter` (or `~/.local/share/Painter`) elsewhere.
pub(crate) fn app_data_dir() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(windows) {
        var("APPDATA")?
    } else if cfg!(any(target_os = "ios", target_os = "macos")) {
        var("HOME")?.join("Library/Application Support")
    } else {
        var("XDG_DATA_HOME").or_else(|| var("HOME").map(|h| h.join(".local/share")))?
    };
    Some(base.join("Painter"))
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

pub(crate) fn kind_of(path: &Path) -> Option<FileKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "painter" | "psd" | "kra" | "ora" | "png" | "jpg" | "jpeg" => Some(FileKind::Document),
        "blend" | "obj" => Some(FileKind::Model),
        _ if brush_io::is_brush_file(path) => Some(FileKind::Brushes),
        _ => None,
    }
}

/// The Objective-C side (`native/ios_files.m`).
#[cfg(target_os = "ios")]
mod native {
    use std::ffi::{c_char, c_int, CStr, CString};
    use std::path::{Path, PathBuf};

    extern "C" {
        fn painter_pick_documents(multiple: c_int);
        fn painter_export_document(path: *const c_char);
        fn painter_take_picked() -> *mut c_char;
        fn painter_free_string(s: *mut c_char);
        fn painter_picker_active() -> c_int;
    }

    pub(super) fn active() -> bool {
        // SAFETY: reads a flag.
        unsafe { painter_picker_active() != 0 }
    }

    pub(super) fn pick(multiple: bool) {
        // SAFETY: plain call; the picker is presented on the main queue.
        unsafe { painter_pick_documents(multiple as c_int) }
    }

    pub(super) fn export(path: &Path) {
        let Ok(path) = CString::new(path.to_string_lossy().as_bytes()) else { return };
        // SAFETY: the string outlives the call (the picker copies the path).
        unsafe { painter_export_document(path.as_ptr()) }
    }

    pub(super) fn take() -> Option<PathBuf> {
        // SAFETY: a malloc'd NUL-terminated string or null, freed by its owner below.
        unsafe {
            let p = painter_take_picked();
            if p.is_null() {
                return None;
            }
            let path = CStr::from_ptr(p).to_string_lossy().into_owned();
            painter_free_string(p);
            Some(PathBuf::from(path))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_desktop_paths() {
        assert_eq!(kind_of(Path::new("a/b.PNG")), Some(FileKind::Document));
        assert_eq!(kind_of(Path::new("model.blend")), Some(FileKind::Model));
        assert_eq!(kind_of(Path::new("x.abr")), Some(FileKind::Brushes));
        assert_eq!(kind_of(Path::new("notes.txt")), None);
        if !cfg!(target_os = "ios") {
            assert!(documents_dir().is_none());
            assert!(settings_file().is_none());
            assert!(take_picked().is_none());
        }
    }
}
