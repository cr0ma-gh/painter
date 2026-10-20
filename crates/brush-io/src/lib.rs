//! Brush preset import and export.
//!
//! - [`sut`]: Clip Studio Paint sub tools (`.sut`, SQLite) and tool databases.
//! - [`abr`]: Photoshop brushes (`.abr`, v1/v2 and v6+).
//! - [`native`]: Painter's own `.rustbrush` (JSON) format.
//!
//! Every reader produces [`BrushPreset`]s: one unified representation holding
//! our engine's [`painter_core::BrushSettings`] plus everything the source
//! carried that the engine can't use yet (tip bitmaps, textures, dynamics,
//! raw parameters), so nothing is lost when re-exporting.

pub mod abr;
pub mod cache;
mod descriptor;
pub mod native;
pub mod sut;
pub mod types;

use std::path::Path;

pub use types::{BrushIoError, BrushPreset, BrushTexture, BrushTip, DualBrush, Dynamic, Dynamics, ImportReport, PresetSource};

/// File extensions [`import`] understands.
pub const IMPORT_EXTENSIONS: [&str; 4] = ["sut", "abr", "rustbrush", "todb"];

/// Imports a brush file, reporting fallbacks taken (see [`ImportReport`]).
/// Clip Studio materials are resolved from this user's installed library.
pub fn import_report(path: &Path) -> Result<ImportReport, BrushIoError> {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    match ext.as_str() {
        "sut" | "todb" => sut::read_report(path, &sut::MaterialLibrary::for_current_user()),
        "abr" => abr::read_report(&std::fs::read(path)?),
        "rustbrush" => Ok(ImportReport { presets: native::read(&std::fs::read(path)?)?, ..ImportReport::default() }),
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
        other => Err(BrushIoError::Unsupported(format!("brush file type .{other}"))),
    }
}

/// Whether `path` looks like a brush file [`import`] can read.
pub fn is_brush_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMPORT_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}
