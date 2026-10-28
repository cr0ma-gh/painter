//! Brush preset import and export.
//!
//! - [`sut`]: Clip Studio Paint sub tools (`.sut`, SQLite) and tool databases.
//! - [`abr`]: Photoshop brushes (`.abr`, v1/v2 and v6+).
//! - [`krita`]: Krita presets (`.kpp`) and bundles (`.bundle`).
//! - [`mypaint`]: MyPaint brushes (`.myb`).
//! - [`gimp`]: GIMP brushes (`.gbr`, `.gih`, `.vbr`).
//! - [`native`]: Painter's own `.rustbrush` (JSON) format.
//!
//! Every reader produces [`BrushPreset`]s: one unified representation holding
//! our engine's [`painter_core::BrushSettings`] plus everything the source
//! carried that the engine can't use yet (tip bitmaps, textures, dynamics,
//! raw parameters), so nothing is lost when re-exporting.

pub mod abr;
pub mod abr_write;
pub mod cache;
mod descriptor;
pub mod gimp;
pub mod krita;
pub mod mypaint;
pub mod native;
pub mod sut;
pub mod types;

use std::path::Path;

pub use abr::clean_brush_name;
pub use types::{BrushIoError, BrushPreset, BrushTexture, BrushTip, DualBrush, Dynamic, Dynamics, ImportReport, PresetSource};

/// File extensions [`import`] understands.
pub const IMPORT_EXTENSIONS: [&str; 10] = ["sut", "abr", "rustbrush", "todb", "kpp", "bundle", "myb", "gbr", "gih", "vbr"];

/// Imports a brush file, reporting fallbacks taken (see [`ImportReport`]).
/// Clip Studio materials are resolved from this user's installed library.
pub fn import_report(path: &Path) -> Result<ImportReport, BrushIoError> {
    import_report_with(path, &sut::MaterialLibrary::for_current_user())
}

/// [`import_report`], resolving Clip Studio materials through `library`.
pub fn import_report_with(path: &Path, library: &sut::MaterialLibrary) -> Result<ImportReport, BrushIoError> {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    match ext.as_str() {
        "sut" | "todb" => sut::read_report(path, library),
        "abr" => abr::read_report(&std::fs::read(path)?),
        "rustbrush" => Ok(ImportReport { presets: native::read(&std::fs::read(path)?)?, ..ImportReport::default() }),
        "kpp" => krita::read_kpp(&std::fs::read(path)?, &|name| sibling(path, name)),
        "bundle" => krita::read_bundle(std::fs::read(path)?),
        "myb" => mypaint::read(&std::fs::read(path)?, &stem(path)),
        "gbr" => gimp::read_gbr(&std::fs::read(path)?),
        "gih" => gimp::read_gih(&std::fs::read(path)?),
        "vbr" => gimp::read_vbr(&std::fs::read(path)?),
        other => Err(BrushIoError::Unsupported(format!("brush file type .{other}"))),
    }
}

/// Imports every preset in a brush file, picking the reader by extension.
pub fn import(path: &Path) -> Result<Vec<BrushPreset>, BrushIoError> {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    match ext.as_str() {
        "sut" | "todb" => sut::read(path),
        "abr" => abr::read(&std::fs::read(path)?),
        "rustbrush" => native::read(&std::fs::read(path)?),
        "kpp" | "bundle" | "myb" | "gbr" | "gih" | "vbr" => import_report(path).map(|r| r.presets),
        other => Err(BrushIoError::Unsupported(format!("brush file type .{other}"))),
    }
}

/// A file name without its extension (MyPaint names brushes by file).
fn stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// A tip file a Krita preset refers to, looked up next to the preset.
fn sibling(path: &Path, name: &str) -> Option<Vec<u8>> {
    let dir = path.parent()?;
    [dir.join(name), dir.join("..").join("brushes").join(name)].into_iter().find_map(|p| std::fs::read(p).ok())
}

/// Whether `path` looks like a brush file [`import`] can read.
pub fn is_brush_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMPORT_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}
