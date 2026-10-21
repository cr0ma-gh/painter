//! On-disk cache of imported brush files.
//!
//! Parsing a `.sut` (SQLite + material thumbnails) or a large `.abr` (RLE tips,
//! patterns, descriptors) is slower than reading Painter's own `.rustbrush`, so
//! each successful import is stored as `.rustbrush` under a key made from the
//! source's path, size and modification time. Imports that needed fallbacks
//! (warnings) are not cached, so fixing the cause (e.g. installing a missing
//! material) takes effect on the next import.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::types::{BrushIoError, ImportReport};
use crate::{import_report_with, native, sut::MaterialLibrary};

/// Bumped when readers change what they produce, invalidating old entries.
const CACHE_VERSION: &str = "brush-cache-6";

/// `%LOCALAPPDATA%\Painter\brush-cache`, or a temp folder elsewhere.
pub fn default_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Painter")
        .join("brush-cache")
}

/// FNV-1a: stable across builds, unlike `DefaultHasher`.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

fn cache_file(path: &Path, cache_dir: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos();
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let key = format!("{CACHE_VERSION}|{}|{}|{modified}", canonical.display(), meta.len());
    Some(cache_dir.join(format!("{:016x}.{}", fnv1a(key.as_bytes()), native::EXTENSION)))
}

/// Imports `path`, using and filling the cache in `cache_dir`.
/// `.rustbrush` files are read directly; cache write failures are ignored.
pub fn import_cached(path: &Path, cache_dir: &Path) -> Result<ImportReport, BrushIoError> {
    import_cached_with(path, cache_dir, &MaterialLibrary::for_current_user())
}

/// [`import_cached`], resolving Clip Studio materials through `library`.
pub fn import_cached_with(path: &Path, cache_dir: &Path, library: &MaterialLibrary) -> Result<ImportReport, BrushIoError> {
    let is_native = path.extension().is_some_and(|e| e.eq_ignore_ascii_case(native::EXTENSION));
    let entry = if is_native { None } else { cache_file(path, cache_dir) };
    if let Some(entry) = &entry {
        if let Ok(presets) = std::fs::read(entry).map_err(BrushIoError::from).and_then(|b| native::read(&b)) {
            return Ok(ImportReport { presets, warnings: Vec::new(), from_cache: true });
        }
    }
    let report = import_report_with(path, library)?;
    if let Some(entry) = entry.filter(|_| report.warnings.is_empty() && !report.presets.is_empty()) {
        let _ = (|| -> Result<(), BrushIoError> {
            std::fs::create_dir_all(cache_dir)?;
            let tmp = entry.with_extension("tmp");
            native::save(&report.presets, &tmp)?;
            std::fs::rename(&tmp, &entry)?;
            Ok(())
        })();
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A v1 `.abr` with one computed brush of `diameter`.
    fn abr(diameter: u16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(1u16.to_be_bytes()); // version
        out.extend(1u16.to_be_bytes()); // count
        out.extend(1u16.to_be_bytes()); // computed
        out.extend(14u32.to_be_bytes());
        out.extend(0u32.to_be_bytes());
        for v in [25u16, diameter, 100, 0, 80] {
            out.extend(v.to_be_bytes());
        }
        out
    }

    #[test]
    fn second_import_comes_from_cache_until_the_file_changes() {
        let dir = std::env::temp_dir().join(format!("painter-brush-cache-test-{}", std::process::id()));
        let cache = dir.join("cache");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("round.abr");
        std::fs::write(&file, abr(19)).unwrap();

        let first = import_cached(&file, &cache).unwrap();
        assert!(!first.from_cache);
        let second = import_cached(&file, &cache).unwrap();
        assert!(second.from_cache);
        assert_eq!(second.presets, first.presets);

        // A different file at the same path (size changes) is a miss.
        std::fs::write(&file, [abr(77), vec![0; 3]].concat()).unwrap();
        let third = import_cached(&file, &cache).unwrap();
        assert!(!third.from_cache);
        assert_eq!(third.presets[0].settings.size, 77.0);

        // A corrupt cache entry is ignored and rewritten.
        for entry in std::fs::read_dir(&cache).unwrap() {
            std::fs::write(entry.unwrap().path(), b"garbage").unwrap();
        }
        assert!(!import_cached(&file, &cache).unwrap().from_cache);
        assert!(import_cached(&file, &cache).unwrap().from_cache);
        std::fs::remove_dir_all(dir).ok();
    }
}
