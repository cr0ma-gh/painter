//! What the Windows installer asks Painter to bring in, from the command line:
//! - `painter --import-brushes FILE…`: Photoshop, Clip Studio (and other)
//!   brushes added to the brush list, those already there skipped;
//! - `painter --copy-csp-materials`: Clip Studio's installed materials copied
//!   into Painter's settings folder, so brushes keep their tips and textures
//!   without Clip Studio.

use std::path::{Path, PathBuf};

use brush_io::sut::MaterialLibrary;
use brush_io::BrushPreset;

/// Clip Studio's installed materials (`…\Material\Install`).
pub fn csp_materials() -> Option<PathBuf> {
    MaterialLibrary::for_current_user().roots.into_iter().next()
}

/// Copies `from` into `to`, folder by folder; returns the bytes copied.
/// Files already there with the same size are skipped.
fn copy_tree(from: &Path, to: &Path) -> Result<u64, String> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let mut bytes = 0;
    for entry in std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?.flatten() {
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if src.is_dir() {
            bytes += copy_tree(&src, &dst)?;
        } else {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if dst.metadata().map(|m| m.len()).ok() != Some(size) {
                std::fs::copy(&src, &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
            }
            bytes += size;
        }
    }
    Ok(bytes)
}

/// `--copy-csp-materials`: Clip Studio's materials into Painter's Materials folder.
pub fn copy_csp_materials() -> Result<String, String> {
    let from = csp_materials().ok_or("Clip Studio's materials aren't on this computer")?;
    let to = crate::files::app_data_dir().ok_or("no settings folder")?.join("Materials").join("Install");
    let bytes = copy_tree(&from, &to)?;
    Ok(format!("Copied {:.1} MB of Clip Studio materials to {}", bytes as f64 / 1_048_576.0, to.display()))
}

/// Adds the presets in `files` to the brush list at `list` (the built-in
/// brushes when it doesn't exist yet), skipping ones already in it.
fn import_into(list: &Path, files: &[PathBuf], library: &MaterialLibrary, cache: &Path) -> Result<(usize, Vec<String>), String> {
    let mut brushes: Vec<BrushPreset> = match std::fs::read(list) {
        Ok(bytes) => brush_io::native::read(&bytes).map_err(|e| format!("{}: {e}", list.display()))?,
        Err(_) => crate::app::default_brushes(),
    };
    let (mut added, mut problems) = (0, Vec::new());
    for file in files {
        match brush_io::cache::import_cached_with(file, cache, library) {
            Ok(report) => {
                for preset in report.presets {
                    if !brushes.contains(&preset) {
                        brushes.push(preset);
                        added += 1;
                    }
                }
            }
            Err(e) => problems.push(format!("{}: {e}", file.display())),
        }
    }
    if added > 0 {
        if let Some(dir) = list.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        // Through a temporary file, as the app writes it.
        let tmp = list.with_extension("tmp");
        brush_io::native::save(&brushes, &tmp).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, list).map_err(|e| e.to_string())?;
    }
    Ok((added, problems))
}

/// `--import-brushes FILE…`.
pub fn import_brushes(files: &[PathBuf]) -> Result<String, String> {
    let list = crate::files::app_data_dir().ok_or("no settings folder")?.join("brushes.rustbrush");
    let mut library = MaterialLibrary::for_current_user();
    if let Some(dir) = crate::files::materials_dir() {
        library.add_copied(&dir);
    }
    let (added, problems) = import_into(&list, files, &library, &brush_io::cache::default_dir())?;
    for p in &problems {
        eprintln!("{p}");
    }
    Ok(format!("Added {added} brushes ({} files couldn't be read)", problems.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brushes_are_added_once() {
        let dir = std::env::temp_dir().join(format!("painter-import-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut preset = crate::app::default_brushes().remove(0);
        preset.settings.name = "From Photoshop".into();
        let abr = dir.join("set.abr");
        std::fs::write(&abr, brush_io::abr_write::write(&[preset]).unwrap()).unwrap();
        let list = dir.join("brushes.rustbrush");
        let library = MaterialLibrary::default();
        let defaults = crate::app::default_brushes().len();
        let (added, problems) = import_into(&list, &[abr.clone(), dir.join("missing.sut")], &library, &dir.join("cache")).unwrap();
        assert_eq!((added, problems.len()), (1, 1));
        let saved = brush_io::native::read(&std::fs::read(&list).unwrap()).unwrap();
        assert_eq!(saved.len(), defaults + 1, "the built-in brushes and the new one");
        assert!(saved.iter().any(|b| b.settings.name == "From Photoshop"));
        // A second run adds nothing.
        assert_eq!(import_into(&list, &[abr], &library, &dir.join("cache")).unwrap().0, 0);
        // Folders copy, sizes and all.
        let (src, dst) = (dir.join("src/Paint001/abc"), dir.join("dst"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("m.png"), [1u8; 100]).unwrap();
        assert_eq!(copy_tree(&dir.join("src"), &dst).unwrap(), 100);
        assert!(dst.join("Paint001/abc/m.png").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
