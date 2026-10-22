//! Pictures on the system clipboard, copied in other apps (a screenshot, an
//! image from a browser or another painting program): arboard on Windows,
//! macOS and Linux, UIPasteboard on iPad (`native/ios_files.m`), the
//! ClipboardManager on Android (pictures read through their content URI).
//!
//! egui only reports text pastes, so these are read directly.

/// A picture from the clipboard: straight (not premultiplied) RGBA8.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ClipboardImage {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Vec<u8>,
}

impl ClipboardImage {
    /// Premultiplied pixels, as layers store them.
    pub(crate) fn premultiplied(&self) -> Vec<u8> {
        let mut px = self.rgba.clone();
        for p in px.as_chunks_mut::<4>().0 {
            *p = painter_core::blend::premultiply(*p);
        }
        px
    }

    /// The picture as a PNG file's bytes.
    pub(crate) fn to_png(&self) -> Result<Vec<u8>, String> {
        let mut png = Vec::new();
        image::RgbaImage::from_raw(self.width, self.height, self.rgba.clone())
            .ok_or("clipboard picture of the wrong size")?
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        Ok(png)
    }
}

/// The picture on the clipboard, if there is one.
pub(crate) fn read() -> Option<ClipboardImage> {
    native::read().filter(|i| i.width > 0 && i.height > 0 && i.rgba.len() == (i.width * i.height * 4) as usize)
}

/// A path of an image file copied as text (e.g. "Copy as path" in Explorer).
pub(crate) fn copied_image_path() -> Option<std::path::PathBuf> {
    let text = native::text()?;
    let path = std::path::PathBuf::from(text.trim().trim_matches('"'));
    let image = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["png", "jpg", "jpeg", "bmp"].iter().any(|x| x.eq_ignore_ascii_case(e)));
    (image && path.is_file()).then_some(path)
}

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
mod native {
    use super::ClipboardImage;

    pub(super) fn read() -> Option<ClipboardImage> {
        let img = arboard::Clipboard::new().ok()?.get_image().ok()?;
        Some(ClipboardImage { width: img.width as u32, height: img.height as u32, rgba: img.bytes.into_owned() })
    }

    pub(super) fn text() -> Option<String> {
        arboard::Clipboard::new().ok()?.get_text().ok()
    }
}

#[cfg(target_os = "ios")]
mod native {
    use super::ClipboardImage;
    use std::ffi::c_char;

    extern "C" {
        fn painter_clipboard_png(len: *mut usize) -> *mut c_char;
        fn painter_free_string(s: *mut c_char);
    }

    pub(super) fn read() -> Option<ClipboardImage> {
        let mut len = 0usize;
        // SAFETY: returns NULL or `len` malloc'd bytes we own and free below.
        let ptr = unsafe { painter_clipboard_png(&mut len) };
        if ptr.is_null() {
            return None;
        }
        let png = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) }.to_vec();
        unsafe { painter_free_string(ptr) };
        let img = image::load_from_memory(&png).ok()?.to_rgba8();
        Some(ClipboardImage { width: img.width(), height: img.height(), rgba: img.into_raw() })
    }

    pub(super) fn text() -> Option<String> {
        None
    }
}

#[cfg(target_os = "android")]
mod native {
    use super::ClipboardImage;

    /// A picture copied in another app: the file behind the clipboard's URI.
    pub(super) fn read() -> Option<ClipboardImage> {
        let bytes = crate::android::clipboard_file()?;
        let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
        Some(ClipboardImage { width: img.width(), height: img.height(), rgba: img.into_raw() })
    }

    pub(super) fn text() -> Option<String> {
        crate::android::clipboard_text()
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux", target_os = "ios", target_os = "android")))]
mod native {
    use super::ClipboardImage;

    pub(super) fn read() -> Option<ClipboardImage> {
        None
    }

    pub(super) fn text() -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_for_layers_and_files() {
        let img = ClipboardImage { width: 2, height: 1, rgba: vec![200, 100, 50, 128, 1, 2, 3, 0] };
        let pre = img.premultiplied();
        assert_eq!(&pre[4..], &[0, 0, 0, 0]);
        assert!(pre[0] < 200 && pre[3] == 128);
        let png = img.to_png().unwrap();
        let back = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(back.into_raw(), img.rgba);
    }
}
