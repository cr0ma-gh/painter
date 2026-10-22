//! SANE (Scanner Access Now Easy) on Linux and BSD: USB and network
//! scanners through the system's `libsane`, loaded at run time so the app
//! starts without it.
//!
//! Following the SANE standard's C interface: `sane_init`, the devices from
//! `sane_get_devices`, then per device its options (`resolution`, `mode`,
//! `source`, and the scan area `tl-x` / `tl-y` / `br-x` / `br-y` whose
//! ranges give the bed size in millimetres). A scan sets those options,
//! `sane_start`s, reads the frames (`sane_get_parameters` + `sane_read` to
//! end of file) and unpacks 1-, 8- or 16-bit grey or RGB rows.

// The decoding helpers are only called by the native part.
#![cfg_attr(not(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android")))), allow(dead_code))]

use super::ScannerBackend;
use crate::{Backend, DeviceInfo, ScanError, ScanSettings, ScannedImage};

/// Where SANE can live at all.
const PLATFORM: bool = cfg!(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android"))));

#[derive(Default)]
pub struct SaneBackend {}

impl ScannerBackend for SaneBackend {
    fn backend(&self) -> Backend {
        Backend::Sane
    }

    fn is_available(&self) -> bool {
        PLATFORM
    }

    fn devices(&mut self) -> Result<Vec<DeviceInfo>, ScanError> {
        #[cfg(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android"))))]
        return native::devices();
        #[allow(unreachable_code)]
        Err(unavailable("not on this platform"))
    }

    fn scan(&mut self, device: &DeviceInfo, settings: &ScanSettings) -> Result<ScannedImage, ScanError> {
        #[cfg(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android"))))]
        return native::scan(device, settings);
        #[allow(unreachable_code)]
        {
            let _ = (device, settings);
            Err(unavailable("not on this platform"))
        }
    }
}

fn unavailable(reason: &str) -> ScanError {
    ScanError::BackendUnavailable { backend: Backend::Sane, reason: reason.into() }
}

/// SANE's frame formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Frame {
    Gray,
    Rgb,
    Red,
    Green,
    Blue,
}

impl Frame {
    fn from_raw(v: i32) -> Option<Frame> {
        Some(match v {
            0 => Frame::Gray,
            1 => Frame::Rgb,
            2 => Frame::Red,
            3 => Frame::Green,
            4 => Frame::Blue,
            _ => return None,
        })
    }
}

/// One frame's rows unpacked into `rgb` (3 bytes a pixel, `width` × `lines`):
/// grey and RGB fill every channel, red / green / blue frames their own.
/// `depth` 1 is line art (a set bit is black), 16-bit samples are native-endian.
pub(crate) fn unpack_frame(frame: Frame, depth: u32, bytes_per_line: usize, width: usize, data: &[u8], rgb: &mut Vec<u8>) {
    if bytes_per_line == 0 || width == 0 {
        return;
    }
    let lines = data.len() / bytes_per_line;
    if rgb.len() < width * lines * 3 {
        rgb.resize(width * lines * 3, 0);
    }
    let channels = if frame == Frame::Rgb { 3 } else { 1 };
    for y in 0..lines {
        let row = &data[y * bytes_per_line..(y + 1) * bytes_per_line];
        for x in 0..width {
            for c in 0..channels {
                let sample = x * channels + c;
                let v = match depth {
                    1 => {
                        let bit = row.get(sample / 8).map_or(0, |b| (b >> (7 - sample % 8)) & 1);
                        if bit == 1 { 0 } else { 255 }
                    }
                    16 => row.get(sample * 2..sample * 2 + 2).map_or(0, |b| (u16::from_ne_bytes([b[0], b[1]]) >> 8) as u8),
                    _ => row.get(sample).copied().unwrap_or(0),
                };
                let o = (y * width + x) * 3;
                match frame {
                    Frame::Gray => rgb[o..o + 3].fill(v),
                    Frame::Rgb => rgb[o + c] = v,
                    Frame::Red => rgb[o] = v,
                    Frame::Green => rgb[o + 1] = v,
                    Frame::Blue => rgb[o + 2] = v,
                }
            }
        }
    }
}

/// The usual resolutions a range-constrained scanner offers.
pub(crate) fn resolutions_in(min: u32, max: u32) -> Vec<u32> {
    let mut out: Vec<u32> = [75, 100, 150, 200, 300, 400, 600, 1200, 2400, 4800].into_iter().filter(|r| (min..=max).contains(r)).collect();
    for r in [min, max] {
        if r > 0 && !out.contains(&r) {
            out.push(r);
        }
    }
    out.sort_unstable();
    out
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android"))))]
mod native {
    use std::ffi::{c_char, c_int, c_void, CStr, CString};

    use super::{resolutions_in, unavailable, unpack_frame, Frame};
    use crate::{Backend, Capabilities, ColorMode, DeviceInfo, ScanError, ScanSettings, ScannedImage};

    type Status = c_int;
    type Handle = *mut c_void;
    const GOOD: Status = 0;
    const CANCELLED: Status = 2;
    const EOF: Status = 5;

    const TYPE_BOOL: c_int = 0;
    const TYPE_INT: c_int = 1;
    const TYPE_FIXED: c_int = 2;
    const TYPE_STRING: c_int = 3;
    const CONSTRAINT_RANGE: c_int = 1;
    const CONSTRAINT_WORD_LIST: c_int = 2;
    const CONSTRAINT_STRING_LIST: c_int = 3;
    const CAP_INACTIVE: c_int = 32;
    const ACTION_GET: c_int = 0;
    const ACTION_SET: c_int = 1;

    #[repr(C)]
    struct Device {
        name: *const c_char,
        vendor: *const c_char,
        model: *const c_char,
        kind: *const c_char,
    }

    #[repr(C)]
    struct Range {
        min: c_int,
        max: c_int,
        quant: c_int,
    }

    #[repr(C)]
    struct OptionDescriptor {
        name: *const c_char,
        title: *const c_char,
        desc: *const c_char,
        kind: c_int,
        unit: c_int,
        size: c_int,
        cap: c_int,
        constraint_type: c_int,
        constraint: *const c_void,
    }

    #[repr(C)]
    #[derive(Default)]
    struct Parameters {
        format: c_int,
        last_frame: c_int,
        bytes_per_line: c_int,
        pixels_per_line: c_int,
        lines: c_int,
        depth: c_int,
    }

    /// `libsane`, loaded and initialised; closed when dropped.
    struct Sane {
        _lib: libloading::Library,
        exit: unsafe extern "C" fn(),
        get_devices: unsafe extern "C" fn(*mut *const *const Device, c_int) -> Status,
        open: unsafe extern "C" fn(*const c_char, *mut Handle) -> Status,
        close: unsafe extern "C" fn(Handle),
        descriptor: unsafe extern "C" fn(Handle, c_int) -> *const OptionDescriptor,
        control: unsafe extern "C" fn(Handle, c_int, c_int, *mut c_void, *mut c_int) -> Status,
        parameters: unsafe extern "C" fn(Handle, *mut Parameters) -> Status,
        start: unsafe extern "C" fn(Handle) -> Status,
        read: unsafe extern "C" fn(Handle, *mut u8, c_int, *mut c_int) -> Status,
        cancel: unsafe extern "C" fn(Handle),
        strstatus: unsafe extern "C" fn(Status) -> *const c_char,
    }

    impl Sane {
        fn load() -> Result<Sane, ScanError> {
            // SAFETY: loading the system's SANE library; its initialisers are plain C.
            let lib = unsafe { libloading::Library::new("libsane.so.1").or_else(|_| libloading::Library::new("libsane.so")) }
                .map_err(|_| unavailable("libsane isn't installed (package sane / libsane1)"))?;
            macro_rules! sym {
                ($name:literal) => {
                    // SAFETY: the symbol has the SANE standard's signature, matching the field's type.
                    *unsafe { lib.get($name) }.map_err(|e| unavailable(&format!("libsane: {e}")))?
                };
            }
            let init: unsafe extern "C" fn(*mut c_int, *const c_void) -> Status = sym!(b"sane_init\0");
            let sane = Sane {
                exit: sym!(b"sane_exit\0"),
                get_devices: sym!(b"sane_get_devices\0"),
                open: sym!(b"sane_open\0"),
                close: sym!(b"sane_close\0"),
                descriptor: sym!(b"sane_get_option_descriptor\0"),
                control: sym!(b"sane_control_option\0"),
                parameters: sym!(b"sane_get_parameters\0"),
                start: sym!(b"sane_start\0"),
                read: sym!(b"sane_read\0"),
                cancel: sym!(b"sane_cancel\0"),
                strstatus: sym!(b"sane_strstatus\0"),
                _lib: lib,
            };
            let mut version = 0;
            // SAFETY: no authorisation callback (null), as allowed.
            let status = unsafe { init(&mut version, std::ptr::null()) };
            if status != GOOD {
                return Err(ScanError::Device(format!("sane_init: {}", sane.message(status))));
            }
            Ok(sane)
        }

        fn message(&self, status: Status) -> String {
            // SAFETY: sane_strstatus returns a static string.
            let p = unsafe { (self.strstatus)(status) };
            if p.is_null() {
                format!("status {status}")
            } else {
                unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
            }
        }

        fn check(&self, what: &str, status: Status) -> Result<(), ScanError> {
            match status {
                GOOD => Ok(()),
                CANCELLED => Err(ScanError::Cancelled),
                s => Err(ScanError::Device(format!("{what}: {}", self.message(s)))),
            }
        }
    }

    impl Drop for Sane {
        fn drop(&mut self) {
            // SAFETY: matches the successful sane_init in `load`.
            unsafe { (self.exit)() }
        }
    }

    fn text(p: *const c_char) -> String {
        if p.is_null() {
            String::new()
        } else {
            // SAFETY: SANE strings are NUL-terminated and live while the library does.
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
    }

    /// An open device; closed when dropped.
    struct Open<'a> {
        sane: &'a Sane,
        handle: Handle,
    }

    /// One option: its index and descriptor.
    struct Opt<'a> {
        index: c_int,
        d: &'a OptionDescriptor,
    }

    impl Opt<'_> {
        fn active(&self) -> bool {
            self.d.cap & CAP_INACTIVE == 0
        }

        /// Range constraint (min, max) as plain numbers (fixed point converted).
        fn range(&self) -> Option<(f64, f64)> {
            if self.d.constraint_type != CONSTRAINT_RANGE || self.d.constraint.is_null() {
                return None;
            }
            // SAFETY: a range constraint points at a SANE_Range.
            let r = unsafe { &*(self.d.constraint as *const Range) };
            Some((self.number(r.min), self.number(r.max)))
        }

        fn words(&self) -> Vec<f64> {
            if self.d.constraint_type != CONSTRAINT_WORD_LIST || self.d.constraint.is_null() {
                return Vec::new();
            }
            let list = self.d.constraint as *const c_int;
            // SAFETY: a word list starts with its length, then that many words.
            let n = unsafe { *list }.max(0) as usize;
            (1..=n).map(|i| self.number(unsafe { *list.add(i) })).collect()
        }

        fn strings(&self) -> Vec<String> {
            if self.d.constraint_type != CONSTRAINT_STRING_LIST || self.d.constraint.is_null() {
                return Vec::new();
            }
            let list = self.d.constraint as *const *const c_char;
            let mut out = Vec::new();
            for i in 0..256 {
                // SAFETY: a NULL-terminated array of strings.
                let p = unsafe { *list.add(i) };
                if p.is_null() {
                    break;
                }
                out.push(text(p));
            }
            out
        }

        fn number(&self, word: c_int) -> f64 {
            if self.d.kind == TYPE_FIXED {
                word as f64 / 65536.0
            } else {
                word as f64
            }
        }

        fn word(&self, value: f64) -> c_int {
            if self.d.kind == TYPE_FIXED {
                (value * 65536.0).round() as c_int
            } else {
                value.round() as c_int
            }
        }
    }

    impl<'a> Open<'a> {
        fn new(sane: &'a Sane, name: &str) -> Result<Self, ScanError> {
            let name = CString::new(name).map_err(|_| ScanError::Device("bad device name".into()))?;
            let mut handle = std::ptr::null_mut();
            // SAFETY: valid name; handle written on success.
            sane.check("open", unsafe { (sane.open)(name.as_ptr(), &mut handle) })?;
            Ok(Open { sane, handle })
        }

        fn options(&self) -> Vec<(String, Opt<'_>)> {
            let mut count: c_int = 0;
            // SAFETY: option 0 is the option count, an SANE_Int.
            let status = unsafe { (self.sane.control)(self.handle, 0, ACTION_GET, (&mut count as *mut c_int).cast(), std::ptr::null_mut()) };
            if status != GOOD {
                return Vec::new();
            }
            (1..count.clamp(0, 1000))
                .filter_map(|i| {
                    // SAFETY: valid option index; the descriptor lives while the device is open.
                    let d = unsafe { (self.sane.descriptor)(self.handle, i) };
                    (!d.is_null()).then(|| {
                        let d = unsafe { &*d };
                        (text(d.name), Opt { index: i, d })
                    })
                })
                .collect()
        }

        fn find(&self, name: &str) -> Option<Opt<'_>> {
            self.options().into_iter().find(|(n, o)| n == name && o.active()).map(|(_, o)| o)
        }

        fn set_number(&self, name: &str, value: f64) {
            let Some(o) = self.find(name) else { return };
            if !matches!(o.d.kind, TYPE_INT | TYPE_FIXED | TYPE_BOOL) {
                return;
            }
            let mut w = o.word(value);
            // SAFETY: a single word option takes a SANE_Word.
            unsafe { (self.sane.control)(self.handle, o.index, ACTION_SET, (&mut w as *mut c_int).cast(), std::ptr::null_mut()) };
        }

        fn set_string(&self, name: &str, value: &str) {
            let Some(o) = self.find(name) else { return };
            if o.d.kind != TYPE_STRING {
                return;
            }
            let mut buf = vec![0u8; (o.d.size.max(1) as usize).max(value.len() + 1)];
            buf[..value.len()].copy_from_slice(value.as_bytes());
            // SAFETY: a string option takes a buffer of at least its size.
            unsafe { (self.sane.control)(self.handle, o.index, ACTION_SET, buf.as_mut_ptr().cast(), std::ptr::null_mut()) };
        }

        fn capabilities(&self) -> (Capabilities, Vec<(ColorMode, String)>) {
            let mut caps = Capabilities { resolutions: Vec::new(), color_modes: Vec::new(), bed_mm: [216.0, 297.0], has_feeder: false };
            let mut modes = Vec::new();
            for (name, o) in self.options() {
                match name.as_str() {
                    "resolution" => {
                        let mut r: Vec<u32> = o.words().into_iter().map(|v| v.round() as u32).filter(|v| *v > 0).collect();
                        if r.is_empty() {
                            if let Some((lo, hi)) = o.range() {
                                r = resolutions_in(lo.round() as u32, hi.round() as u32);
                            }
                        }
                        r.sort_unstable();
                        r.dedup();
                        caps.resolutions = r;
                    }
                    "mode" => {
                        for s in o.strings() {
                            let l = s.to_ascii_lowercase();
                            let mode = if l.contains("color") || l.contains("colour") {
                                Some(ColorMode::Color)
                            } else if l.contains("gray") || l.contains("grey") {
                                Some(ColorMode::Grayscale)
                            } else if l.contains("lineart") || l.contains("binary") || l.contains("black") {
                                Some(ColorMode::LineArt)
                            } else {
                                None
                            };
                            if let Some(m) = mode.filter(|m| !modes.iter().any(|(x, _)| x == m)) {
                                modes.push((m, s));
                            }
                        }
                    }
                    "source" => caps.has_feeder = o.strings().iter().any(|s| s.to_ascii_lowercase().contains("adf") || s.contains("Feeder")),
                    "br-x" => {
                        if let Some((_, hi)) = o.range() {
                            caps.bed_mm[0] = hi as f32;
                        }
                    }
                    "br-y" => {
                        if let Some((_, hi)) = o.range() {
                            caps.bed_mm[1] = hi as f32;
                        }
                    }
                    _ => {}
                }
            }
            if caps.resolutions.is_empty() {
                caps.resolutions = vec![75, 150, 300, 600];
            }
            caps.color_modes = modes.iter().map(|(m, _)| *m).collect();
            if caps.color_modes.is_empty() {
                caps.color_modes = vec![ColorMode::Color, ColorMode::Grayscale];
            }
            (caps, modes)
        }
    }

    impl Drop for Open<'_> {
        fn drop(&mut self) {
            // SAFETY: the handle came from sane_open.
            unsafe { (self.sane.close)(self.handle) }
        }
    }

    pub(super) fn devices() -> Result<Vec<DeviceInfo>, ScanError> {
        let sane = Sane::load()?;
        let mut list: *const *const Device = std::ptr::null();
        // Local devices only: network SANE backends answer slowly, and AirScan covers those.
        sane.check("devices", unsafe { (sane.get_devices)(&mut list, 1) })?;
        let mut out = Vec::new();
        for i in 0..64 {
            // SAFETY: a NULL-terminated array of device pointers.
            let d = unsafe { *list.add(i) };
            if d.is_null() {
                break;
            }
            let d = unsafe { &*d };
            let (id, vendor, model) = (text(d.name), text(d.vendor), text(d.model));
            // Network scanners SANE reaches through its eSCL backend are listed by AirScan already.
            if id.starts_with("escl:") || id.starts_with("airscan:") {
                continue;
            }
            let capabilities = match Open::new(&sane, &id) {
                Ok(open) => open.capabilities().0,
                Err(_) => continue, // busy or no permission
            };
            out.push(DeviceInfo {
                name: if model.is_empty() { id.clone() } else { format!("{vendor} {model}").trim().to_string() },
                manufacturer: vendor,
                id,
                backend: Backend::Sane,
                capabilities,
            });
        }
        Ok(out)
    }

    pub(super) fn scan(device: &DeviceInfo, settings: &ScanSettings) -> Result<ScannedImage, ScanError> {
        let sane = Sane::load()?;
        let open = Open::new(&sane, &device.id)?;
        let (caps, modes) = open.capabilities();
        if let Some((_, name)) = modes.iter().find(|(m, _)| *m == settings.color_mode) {
            open.set_string("mode", name);
        }
        open.set_number("resolution", settings.dpi as f64);
        let a = settings.area.clamped();
        let [bw, bh] = caps.bed_mm;
        open.set_number("tl-x", (a.x * bw) as f64);
        open.set_number("tl-y", (a.y * bh) as f64);
        open.set_number("br-x", ((a.x + a.width) * bw) as f64);
        open.set_number("br-y", ((a.y + a.height) * bh) as f64);
        for (name, value) in [("brightness", settings.brightness), ("contrast", settings.contrast)] {
            if value != 0.0 {
                if let Some((lo, hi)) = open.find(name).and_then(|o| o.range()) {
                    let mid = (lo + hi) / 2.0;
                    open.set_number(name, mid + value as f64 * (hi - lo) / 2.0);
                }
            }
        }

        let mut rgb = Vec::new();
        let mut width = 0usize;
        loop {
            // SAFETY: an open handle.
            sane.check("start", unsafe { (sane.start)(open.handle) })?;
            let mut p = Parameters::default();
            sane.check("parameters", unsafe { (sane.parameters)(open.handle, &mut p) })?;
            let frame = Frame::from_raw(p.format).ok_or_else(|| ScanError::Device(format!("unknown frame format {}", p.format)))?;
            let mut data = Vec::with_capacity((p.bytes_per_line.max(0) as usize) * (p.lines.max(0) as usize));
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let mut len: c_int = 0;
                // SAFETY: buf has buf.len() bytes.
                let status = unsafe { (sane.read)(open.handle, buf.as_mut_ptr(), buf.len() as c_int, &mut len) };
                if status == EOF {
                    break;
                }
                if status != GOOD {
                    unsafe { (sane.cancel)(open.handle) };
                    sane.check("read", status)?;
                }
                data.extend_from_slice(&buf[..len.max(0) as usize]);
            }
            width = p.pixels_per_line.max(0) as usize;
            unpack_frame(frame, p.depth.max(1) as u32, p.bytes_per_line.max(0) as usize, width, &data, &mut rgb);
            if p.last_frame != 0 {
                break;
            }
        }
        unsafe { (sane.cancel)(open.handle) };
        if width == 0 || rgb.is_empty() {
            return Err(ScanError::Device("the scanner sent no picture".into()));
        }
        let height = rgb.len() / 3 / width;
        let rgba: Vec<u8> = rgb.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect();
        ScannedImage::new(width as u32, height as u32, settings.dpi, rgba)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_unpack_from_every_depth() {
        // Line art: bits, set = black.
        let mut rgb = Vec::new();
        unpack_frame(Frame::Gray, 1, 1, 3, &[0b0100_0000], &mut rgb);
        assert_eq!(rgb, [255, 255, 255, 0, 0, 0, 255, 255, 255]);
        // 8-bit RGB, two rows of one pixel with padding at the end of each line.
        let mut rgb = Vec::new();
        unpack_frame(Frame::Rgb, 8, 4, 1, &[1, 2, 3, 0, 4, 5, 6, 0], &mut rgb);
        assert_eq!(rgb, [1, 2, 3, 4, 5, 6]);
        // 16-bit grey keeps the high byte.
        let mut rgb = Vec::new();
        unpack_frame(Frame::Gray, 16, 2, 1, &0xABCDu16.to_ne_bytes(), &mut rgb);
        assert_eq!(rgb, [0xAB; 3]);
        // Three-pass colour fills one channel per frame.
        let mut rgb = Vec::new();
        unpack_frame(Frame::Red, 8, 1, 1, &[9], &mut rgb);
        unpack_frame(Frame::Blue, 8, 1, 1, &[7], &mut rgb);
        assert_eq!(rgb, [9, 0, 7]);
    }

    #[test]
    fn ranges_give_usual_resolutions() {
        assert_eq!(resolutions_in(50, 600), vec![50, 75, 100, 150, 200, 300, 400, 600]);
        assert_eq!(resolutions_in(300, 300), vec![300]);
    }
}
