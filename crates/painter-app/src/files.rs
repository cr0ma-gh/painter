//! File dialogs and the app's own folders.
//!
//! On the desktop the dialogs are the system's (`rfd`) and answer at once.
//! On iPadOS they are UIKit's document picker (`native/ios_files.m`): opening
//! is asynchronous (the chosen files arrive a frame or more later through
//! [`take_picked`], tagged with the [`Purpose`] the dialog was opened for),
//! saving writes into the app's Documents folder (shown as "Painter" in the
//! Files app), and [`offer_export`] lets the user put a copy anywhere.

use std::path::{Path, PathBuf};
#[allow(unused_imports)]
use crate::i18n::tr;
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
#[cfg_attr(desktop, allow(dead_code))]
pub(crate) struct FileDialog {
    #[cfg(desktop)]
    inner: rfd::FileDialog,
    name: Option<String>,
    extensions: Vec<String>,
    purpose: Purpose,
}

impl FileDialog {
    pub(crate) fn new() -> Self {
        Self {
            #[cfg(desktop)]
            inner: rfd::FileDialog::new(),
            name: None,
            extensions: Vec::new(),
            purpose: Purpose::Open,
        }
    }

    pub(crate) fn add_filter(mut self, name: impl Into<String>, extensions: &[impl ToString]) -> Self {
        let extensions: Vec<String> = extensions.iter().map(|e| e.to_string()).collect();
        #[cfg(desktop)]
        {
            self.inner = self.inner.add_filter(name, &extensions);
        }
        #[cfg(not(desktop))]
        let _ = name.into();
        self.extensions.extend(extensions);
        self
    }

    pub(crate) fn set_file_name(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        #[cfg(desktop)]
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
        #[cfg(desktop)]
        return self.inner.pick_file();
        #[cfg(not(desktop))]
        {
            start_pick(self.purpose, false, &self.extensions);
            None
        }
    }

    pub(crate) fn pick_files(self) -> Option<Vec<PathBuf>> {
        #[cfg(desktop)]
        return self.inner.pick_files();
        #[cfg(not(desktop))]
        {
            start_pick(self.purpose, true, &self.extensions);
            None
        }
    }

    /// Where to save; on iPadOS a file in Documents under the suggested name.
    pub(crate) fn save_file(self) -> Option<PathBuf> {
        #[cfg(desktop)]
        return self.inner.save_file();
        #[cfg(not(desktop))]
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
fn start_pick(purpose: Purpose, multiple: bool, _extensions: &[String]) {
    if let Ok(mut p) = PENDING.lock() {
        *p = purpose;
    }
    native::pick(multiple);
}

#[cfg(target_os = "android")]
fn start_pick(purpose: Purpose, multiple: bool, extensions: &[String]) {
    android::open(purpose, multiple, extensions);
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
    #[cfg(target_os = "android")]
    return android::take();
    #[cfg(desktop)]
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
    #[cfg(target_os = "android")]
    return android::open_now();
    #[cfg(desktop)]
    false
}

/// After writing an export: on iPadOS, offers to put a copy anywhere (Files,
/// iCloud Drive, another app). Nothing on the desktop, where the save dialog
/// already chose the place.
pub(crate) fn offer_export(path: &Path) {
    #[cfg(target_os = "ios")]
    native::export(path);
    #[cfg(target_os = "android")]
    if let Ok(mut n) = NOTICE.lock() {
        *n = Some(android::share(path));
    }
    #[cfg(desktop)]
    let _ = path;
}

/// A message for the status bar from the file layer.
static NOTICE: Mutex<Option<String>> = Mutex::new(None);

pub(crate) fn take_notice() -> Option<String> {
    NOTICE.lock().ok().and_then(|mut n| n.take())
}

/// A message for the status bar.
#[allow(dead_code)]
pub(crate) fn notice(message: impl Into<String>) {
    if let Ok(mut n) = NOTICE.lock() {
        *n = Some(message.into());
    }
}

/// Android: the picker on the camera folder, newest photos first (they arrive through [`take_picked`]).
#[cfg(target_os = "android")]
pub(crate) fn pick_camera_photo() {
    android::camera();
}

/// The app's own documents folder: iPadOS's Documents, Android's
/// app-specific storage (Android/data/<app>/files, reachable over USB),
/// `None` on the desktop (where the system dialogs reach everything).
pub(crate) fn documents_dir() -> Option<PathBuf> {
    #[cfg(target_os = "android")]
    {
        let (internal, external) = ANDROID_FOLDERS.get()?;
        let dir = external.clone().or_else(|| internal.as_ref().map(|d| d.join("Documents")))?;
        std::fs::create_dir_all(&dir).ok()?;
        return Some(dir);
    }
    #[allow(unreachable_code)]
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
    // Desktop: the copy the Windows installer makes (`painter --copy-csp-materials`), if any.
    if documents_dir().is_none() {
        return app_data_dir().map(|d| d.join("Materials")).filter(|d| d.is_dir());
    }
    let dir = documents_dir()?.join("Materials");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Where settings are kept when eframe has no storage: iPadOS's
/// Library/Application Support (not shown in the Files app).
pub(crate) fn settings_file() -> Option<PathBuf> {
    if cfg!(any(target_os = "ios", target_os = "android")) {
        Some(app_data_dir()?.join("settings.json"))
    } else {
        None
    }
}

/// The app's own data folder (the brush library): `%APPDATA%\Painter` on
/// Windows, `Library/Application Support/Painter` on iPadOS and macOS,
/// `$XDG_DATA_HOME/Painter` (or `~/.local/share/Painter`) elsewhere; on
/// Android, the app's internal storage.
pub(crate) fn app_data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "android")]
    return ANDROID_FOLDERS.get().and_then(|(internal, _)| internal.clone()).map(|d| d.join("Painter"));
    #[allow(unreachable_code)]
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
        "painter" | "psd" | "kra" | "ora" | "clip" | "png" | "jpg" | "jpeg" => Some(FileKind::Document),
        "blend" | "obj" | "gltf" | "glb" | "fbx" => Some(FileKind::Model),
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

/// Android: the app's internal and external (app-specific) storage, from the activity.
#[cfg(target_os = "android")]
static ANDROID_FOLDERS: std::sync::OnceLock<(Option<PathBuf>, Option<PathBuf>)> = std::sync::OnceLock::new();

#[cfg(target_os = "android")]
pub(crate) fn set_android_folders(internal: Option<PathBuf>, external: Option<PathBuf>) {
    let _ = ANDROID_FOLDERS.set((internal, external));
}

/// The in-app file picker where there's no system one to call (Android): the
/// files of the app's folder that suit what's being opened.
pub(crate) fn picker_window(ctx: &eframe::egui::Context) {
    #[cfg(target_os = "android")]
    android::window(ctx);
    #[cfg(not(target_os = "android"))]
    let _ = ctx;
}

#[cfg(target_os = "android")]
mod android {
    use super::{documents_dir, Purpose};
    use crate::i18n::tr;
    use eframe::egui;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    struct Picker {
        purpose: Purpose,
        multiple: bool,
        extensions: Vec<String>,
        chosen: Vec<PathBuf>,
        /// The folder shown.
        dir: Option<PathBuf>,
        /// Newest files first (the camera folder).
        newest_first: bool,
    }

    static PICKER: Mutex<Option<Picker>> = Mutex::new(None);
    static PICKED: Mutex<Vec<(Purpose, PathBuf)>> = Mutex::new(Vec::new());
    /// The folder last opened from, for the next time.
    static LAST_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

    pub(super) fn open(purpose: Purpose, multiple: bool, extensions: &[String]) {
        let dir = LAST_DIR.lock().ok().and_then(|d| d.clone()).filter(|d| d.is_dir()).or_else(documents_dir);
        if let Ok(mut p) = PICKER.lock() {
            *p = Some(Picker {
                purpose,
                multiple,
                extensions: extensions.iter().map(|e| e.to_ascii_lowercase()).collect(),
                chosen: Vec::new(),
                dir,
                newest_first: false,
            });
        }
    }

    /// The picker on the camera folder, newest photos first, for a page photographed just now.
    pub(super) fn camera() {
        let dir = crate::android::shared_root().map(|r| r.join("DCIM")).map(|d| if d.join("Camera").is_dir() { d.join("Camera") } else { d });
        let extensions = ["jpg", "jpeg", "png", "heic"].map(String::from).to_vec();
        if let Ok(mut p) = PICKER.lock() {
            *p = Some(Picker { purpose: Purpose::ScanPage, multiple: true, extensions, chosen: Vec::new(), dir, newest_first: true });
        }
    }

    pub(super) fn open_now() -> bool {
        PICKER.lock().is_ok_and(|p| p.is_some())
    }

    pub(super) fn take() -> Option<(Purpose, PathBuf)> {
        let mut picked = PICKED.lock().ok()?;
        (!picked.is_empty()).then(|| picked.remove(0))
    }

    /// Shortcuts: the app's own folder, then the device's usual folders when allowed.
    fn places(access: bool) -> Vec<(&'static str, PathBuf)> {
        let mut out = Vec::new();
        if let Some(d) = documents_dir() {
            out.push(("Painter", d));
        }
        if access {
            if let Some(root) = crate::android::shared_root() {
                for (name, sub) in [("Download", "Download"), ("Documents", "Documents"), ("Pictures", "Pictures"), ("Camera", "DCIM")] {
                    let p = root.join(sub);
                    if p.is_dir() {
                        out.push((name, p));
                    }
                }
                out.push(("Device", root));
            }
        }
        out
    }

    /// Folders (not hidden) and the files that suit, each sorted by name.
    fn list(dir: &Path, extensions: &[String], newest_first: bool) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let (mut dirs, mut files) = (Vec::new(), Vec::new());
        if let Ok(entries) = std::fs::read_dir(dir) {
            for p in entries.filter_map(|e| e.ok().map(|e| e.path())) {
                let hidden = p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'));
                if hidden {
                    continue;
                }
                if p.is_dir() {
                    dirs.push(p);
                } else {
                    let ext = p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
                    if extensions.is_empty() || extensions.contains(&ext) {
                        files.push(p);
                    }
                }
            }
        }
        let key = |p: &PathBuf| p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        dirs.sort_by_key(key);
        files.sort_by_key(key);
        if newest_first {
            files.sort_by_key(|p| std::cmp::Reverse(std::fs::metadata(p).and_then(|m| m.modified()).ok()));
        }
        (dirs, files)
    }

    pub(super) fn window(ctx: &egui::Context) {
        let Ok(mut guard) = PICKER.lock() else { return };
        let Some(picker) = guard.as_mut() else { return };
        let access = crate::android::has_storage_access();
        let places = places(access);
        if picker.dir.is_none() {
            picker.dir = places.first().map(|(_, p)| p.clone());
        }
        let Some(dir) = picker.dir.clone() else {
            *guard = None;
            return;
        };
        let (dirs, files) = list(&dir, &picker.extensions, picker.newest_first);
        let mut done = None;
        let mut go = None;
        let row = egui::vec2(0.0, 40.0);
        egui::Window::new(tr("Open")).collapsible(false).resizable(true).default_width(460.0).show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (name, path) in &places {
                    if ui.selectable_label(dir.starts_with(path) && *name != "Device" || dir == *path, *name).clicked() {
                        go = Some(path.clone());
                    }
                }
            });
            if !access {
                ui.horizontal_wrapped(|ui| {
                    ui.small(tr("To open files from Download, Documents or your photos, allow Painter to see the device's files."));
                    if ui.button(tr("Allow access…")).clicked() {
                        crate::android::request_storage_access();
                    }
                });
            }
            ui.separator();
            ui.horizontal(|ui| {
                let up = dir.parent().filter(|p| p.starts_with("/storage") || p.starts_with("/sdcard")).map(Path::to_path_buf);
                if ui.add_enabled(up.is_some(), egui::Button::new(tr("⬆ Up"))).clicked() {
                    go = up;
                }
                ui.label(egui::RichText::new(dir.display().to_string()).small());
            });
            egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                let width = ui.available_width();
                for d in &dirs {
                    let name = d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    if ui.add(egui::Button::new(format!("📁 {name}")).min_size(egui::vec2(width, row.y))).clicked() {
                        go = Some(d.clone());
                    }
                }
                if dirs.is_empty() && files.is_empty() {
                    ui.label(tr("Nothing here that this can open."));
                }
                for f in &files {
                    let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let on = picker.chosen.contains(f);
                    if ui.add(egui::Button::new(name).selected(on).min_size(egui::vec2(width, row.y))).clicked() {
                        if picker.multiple {
                            if on {
                                picker.chosen.retain(|c| c != f);
                            } else {
                                picker.chosen.push(f.clone());
                            }
                        } else {
                            done = Some(vec![f.clone()]);
                        }
                    }
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                if picker.multiple && ui.add_enabled(!picker.chosen.is_empty(), egui::Button::new(tr("Open"))).clicked() {
                    done = Some(picker.chosen.clone());
                }
                if ui.button(tr("Cancel")).clicked() {
                    done = Some(Vec::new());
                }
            });
        });
        if let Some(path) = go {
            picker.dir = Some(path);
        }
        if let Some(chosen) = done {
            let purpose = picker.purpose;
            if let Ok(mut last) = LAST_DIR.lock() {
                *last = Some(dir);
            }
            *guard = None;
            if let Ok(mut picked) = PICKED.lock() {
                picked.extend(chosen.into_iter().map(|p| (purpose, p)));
            }
        }
    }

    /// After an export: a copy in the device's Documents/Painter folder when
    /// Painter may write there, so other apps and computers see it.
    pub(super) fn share(path: &Path) -> String {
        if crate::android::has_storage_access() {
            if let (Some(root), Some(name)) = (crate::android::shared_root(), path.file_name()) {
                let dir = root.join("Documents").join("Painter");
                let target = dir.join(name);
                if std::fs::create_dir_all(&dir).is_ok() && std::fs::copy(path, &target).is_ok() {
                    return format!("Saved, with a copy in {}", target.display());
                }
            }
        }
        format!("Saved in the app's folder: {}", path.display())
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
