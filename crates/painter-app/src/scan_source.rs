//! Pages for the scanner dialog that don't come from a scanner: the iPad's
//! document camera (VisionKit, `native/ios_scan.m`: it finds the sheet,
//! straightens its perspective and can take several pages), or a photo or
//! scan as an image file (on every platform). They go through the same
//! cleanup as a scan: crop, orientation, auto contrast, line art.
//!
//! A photographed page has no resolution of its own; it is worked out from
//! the paper size the user picks (the page's longer side over the paper's).

use std::path::{Path, PathBuf};

use scanner_io::ScannedImage;

/// Paper sizes for a photographed page, in millimetres (portrait).
pub(crate) const PAGE_SIZES: [(&str, [f32; 2]); 7] = [
    ("A4", [210.0, 297.0]),
    ("A5", [148.0, 210.0]),
    ("A3", [297.0, 420.0]),
    ("B5", [182.0, 257.0]),
    ("B4 (manga)", [257.0, 364.0]),
    ("Letter", [215.9, 279.4]),
    ("Postcard", [100.0, 148.0]),
];

/// The resolution of a `w` × `h` pixel picture of a page of `paper` mm,
/// from the longer sides (the page may lie either way).
pub(crate) fn page_dpi(w: u32, h: u32, paper: [f32; 2]) -> u32 {
    let long_mm = paper[0].max(paper[1]);
    let long_px = w.max(h) as f32;
    ((long_px / (long_mm / 25.4)).round() as u32).clamp(10, 4800)
}

/// An image file as a page, its resolution from `paper`.
pub(crate) fn load_page(path: &Path, paper: [f32; 2]) -> Result<ScannedImage, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let img = image::load_from_memory(&bytes).map_err(|e| format!("{}: {e}", path.display()))?.to_rgba8();
    let (w, h) = img.dimensions();
    ScannedImage::new(w, h, page_dpi(w, h, paper), img.into_raw()).map_err(|e| e.to_string())
}

/// Whether this device has the document camera.
pub(crate) fn camera_supported() -> bool {
    native::supported()
}

/// Opens the document camera; the pages arrive through [`take_pages`].
pub(crate) fn start_camera() {
    native::start();
}

/// The camera is open.
pub(crate) fn camera_active() -> bool {
    native::active()
}

/// Pages the camera finished with (temporary PNG files), oldest first.
pub(crate) fn take_pages() -> Vec<PathBuf> {
    std::iter::from_fn(native::take_page).collect()
}

/// Why the camera failed, once.
pub(crate) fn take_error() -> Option<String> {
    native::take_error()
}

/// Starts looking for network (eSCL / AirScan) scanners with Bonjour (iPad;
/// elsewhere the scanner backend asks the network itself).
pub(crate) fn find_network_scanners() {
    native::find_network();
}

/// Network scanners Bonjour found since last asked.
pub(crate) fn take_network_scanners() -> Vec<scanner_io::NetworkScanner> {
    std::iter::from_fn(native::take_network)
        .filter_map(|line| {
            let (name, url) = line.split_once('\t')?;
            Some(scanner_io::NetworkScanner { name: name.to_string(), url: url.to_string() })
        })
        .collect()
}

#[cfg(target_os = "ios")]
mod native {
    use std::ffi::{c_char, c_int, CStr};
    use std::path::PathBuf;

    extern "C" {
        fn painter_scan_supported() -> c_int;
        fn painter_scan_document();
        fn painter_scan_active() -> c_int;
        fn painter_take_scanned() -> *mut c_char;
        fn painter_take_scan_error() -> *mut c_char;
        fn painter_find_network_scanners();
        fn painter_take_network_scanner() -> *mut c_char;
        fn painter_free_string(s: *mut c_char);
    }

    /// Takes a malloc'd C string from the glue.
    fn take(s: *mut c_char) -> Option<String> {
        if s.is_null() {
            return None;
        }
        // SAFETY: a NUL-terminated string from strdup, freed right after.
        let out = unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned();
        unsafe { painter_free_string(s) };
        Some(out)
    }

    pub(super) fn supported() -> bool {
        // SAFETY: reads a class property.
        unsafe { painter_scan_supported() != 0 }
    }

    pub(super) fn start() {
        // SAFETY: schedules the camera on the main queue.
        unsafe { painter_scan_document() }
    }

    pub(super) fn active() -> bool {
        // SAFETY: reads a flag.
        unsafe { painter_scan_active() != 0 }
    }

    pub(super) fn take_page() -> Option<PathBuf> {
        // SAFETY: returns NULL or a string we own.
        take(unsafe { painter_take_scanned() }).map(PathBuf::from)
    }

    pub(super) fn take_error() -> Option<String> {
        // SAFETY: returns NULL or a string we own.
        take(unsafe { painter_take_scan_error() })
    }

    pub(super) fn find_network() {
        // SAFETY: schedules a Bonjour search on the main queue.
        unsafe { painter_find_network_scanners() }
    }

    pub(super) fn take_network() -> Option<String> {
        // SAFETY: returns NULL or a string we own.
        take(unsafe { painter_take_network_scanner() })
    }
}

#[cfg(not(target_os = "ios"))]
mod native {
    use std::path::PathBuf;

    pub(super) fn supported() -> bool {
        false
    }
    pub(super) fn start() {}
    pub(super) fn active() -> bool {
        false
    }
    pub(super) fn take_page() -> Option<PathBuf> {
        None
    }
    pub(super) fn take_error() -> Option<String> {
        None
    }
    pub(super) fn find_network() {}
    pub(super) fn take_network() -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_resolution_from_the_paper() {
        // An A4 page photographed at 2480 × 3508: 300 dpi, either way round.
        assert_eq!(page_dpi(2480, 3508, PAGE_SIZES[0].1), 300);
        assert_eq!(page_dpi(3508, 2480, PAGE_SIZES[0].1), 300);
        assert_eq!(page_dpi(1, 1, PAGE_SIZES[0].1), 10, "clamped");
        let dir = std::env::temp_dir().join(format!("painter-scan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("page.png");
        image::RgbaImage::from_pixel(148, 210, image::Rgba([250, 250, 250, 255])).save(&path).unwrap();
        let page = load_page(&path, PAGE_SIZES[1].1).unwrap();
        assert_eq!((page.width, page.height, page.dpi), (148, 210, 25));
        assert!(load_page(&dir.join("missing.png"), PAGE_SIZES[0].1).is_err());
        std::fs::remove_dir_all(&dir).ok();
        assert!(!camera_supported() && take_pages().is_empty(), "no camera off iPad");
    }
}
