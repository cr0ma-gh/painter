//! iPadOS window behaviour (`native/ios_ui.m`): no status bar over the menu
//! bar, edge swipes go to Painter first, and the UI stays inside the safe
//! area. Nothing on other platforms.

use eframe::egui;

#[derive(Default)]
pub(crate) struct IosWindow {
    #[cfg_attr(not(target_os = "ios"), allow(dead_code))]
    prepared: bool,
}

#[cfg(target_os = "ios")]
mod native {
    use std::ffi::c_int;

    extern "C" {
        pub(super) fn painter_ios_prepare_window() -> c_int;
        pub(super) fn painter_ios_safe_area(top: *mut f64, left: *mut f64, bottom: *mut f64, right: *mut f64);
    }
}

impl IosWindow {
    /// Sets the window up once it exists, and returns the safe area insets
    /// (top, left, bottom, right) in egui points.
    #[cfg(target_os = "ios")]
    fn insets(&mut self, ctx: &egui::Context) -> [f32; 4] {
        // SAFETY: plain calls into UIKit on the main thread (where eframe runs);
        // the pointers are to locals.
        unsafe {
            if !self.prepared {
                self.prepared = native::painter_ios_prepare_window() != 0;
            }
            let (mut t, mut l, mut b, mut r) = (0.0, 0.0, 0.0, 0.0);
            native::painter_ios_safe_area(&mut t, &mut l, &mut b, &mut r);
            // UIKit points → physical pixels → egui points (which include the zoom factor).
            let scale = ctx.native_pixels_per_point().unwrap_or(1.0) / ctx.pixels_per_point();
            [t, l, b, r].map(|v| v as f32 * scale)
        }
    }

    #[cfg(not(target_os = "ios"))]
    fn insets(&mut self, _ctx: &egui::Context) -> [f32; 4] {
        [0.0; 4]
    }

    /// Empty bands along the edges the system draws over; call before the
    /// app's own panels so they sit inside.
    pub(crate) fn safe_area(&mut self, ctx: &egui::Context, fill: egui::Color32) {
        let [top, left, bottom, right] = self.insets(ctx);
        let frame = egui::Frame::new().fill(fill);
        if top > 0.5 {
            egui::TopBottomPanel::top("safe_area_top").exact_height(top).frame(frame).show_separator_line(false).show(ctx, |_| {});
        }
        if bottom > 0.5 {
            egui::TopBottomPanel::bottom("safe_area_bottom")
                .exact_height(bottom)
                .frame(frame)
                .show_separator_line(false)
                .show(ctx, |_| {});
        }
        if left > 0.5 {
            egui::SidePanel::left("safe_area_left")
                .exact_width(left)
                .resizable(false)
                .frame(frame)
                .show_separator_line(false)
                .show(ctx, |_| {});
        }
        if right > 0.5 {
            egui::SidePanel::right("safe_area_right")
                .exact_width(right)
                .resizable(false)
                .frame(frame)
                .show_separator_line(false)
                .show(ctx, |_| {});
        }
    }
}

#[cfg(target_os = "ios")]
mod net {
    use std::ffi::{c_char, CString};

    extern "C" {
        fn painter_https_get(url: *const c_char, len: *mut usize) -> *mut u8;
        fn painter_free(p: *mut u8);
        fn painter_open_url(url: *const c_char);
    }

    /// The body of an HTTPS GET (call off the main thread: it waits).
    pub(crate) fn https_get(url: &str) -> Option<Vec<u8>> {
        let url = CString::new(url).ok()?;
        let mut len = 0usize;
        // SAFETY: a NUL-terminated URL in; the buffer is copied out and freed with painter_free.
        unsafe {
            let p = painter_https_get(url.as_ptr(), &mut len);
            if p.is_null() {
                return None;
            }
            let body = std::slice::from_raw_parts(p, len).to_vec();
            painter_free(p);
            Some(body)
        }
    }

    /// Opens a link in Safari.
    pub(crate) fn open_url(url: &str) {
        if let Ok(url) = CString::new(url) {
            // SAFETY: a NUL-terminated string; UIKit copies it.
            unsafe { painter_open_url(url.as_ptr()) }
        }
    }
}

#[cfg(target_os = "ios")]
pub(crate) use net::{https_get, open_url};
