//! Scanner acquisition for importing paper sketches.
//!
//! The pipeline:
//!
//! 1. **Discover** devices through every available [`ScannerBackend`]
//!    ([`backends`], [`list_devices`]): WIA on Windows, TWAIN and SANE as
//!    further backends.
//! 2. **Configure** a [`ScanSettings`]: resolution, [`ColorMode`], and the
//!    region to scan, checked against the device's [`Capabilities`].
//! 3. **Preview** at low resolution ([`ScannerBackend::preview`]) so the user
//!    can choose the crop region ([`ScanArea`]).
//! 4. **Acquire** the final image ([`ScannerBackend::scan`]) as a
//!    [`ScannedImage`].
//! 5. **Clean up** for painting ([`prepare_for_import`]): crop, orientation
//!    (rotation and mirroring, [`orient`]), auto-contrast,
//!    and optionally extract the line art (paper → transparency).
//! 6. **Import**: the app turns the result into a new raster layer
//!    ([`ScannedImage::to_premultiplied`] feeds `TileStore::from_rgba`).
//!
//! Steps 1–4 are stubs on every backend for now (they report
//! [`ScanError::BackendUnavailable`]); steps 5 and 6 are implemented and tested.

mod backend;
mod decode;
mod process;

pub use backend::{
    backends,
    escl::{add_address, add_targets, EsclBackend, Target as NetworkScanner},
    list_devices,
    sane::SaneBackend,
    twain::TwainBackend,
    wia::WiaBackend,
    ScannerBackend,
};
pub use process::{auto_contrast, crop, extract_line_art, orient, prepare_for_import, ImportOptions, LineArtOptions, Orientation, Rotation};

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum ScanError {
    /// The backend isn't built for this platform or isn't implemented yet.
    BackendUnavailable { backend: Backend, reason: String },
    /// The device disappeared, is busy or reported an error.
    Device(String),
    /// Settings the device can't honour (resolution, mode, area).
    InvalidSettings(String),
    /// The user cancelled in a driver-provided dialog.
    Cancelled,
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::BackendUnavailable { backend, reason } => write!(f, "{} scanning unavailable: {reason}", backend.name()),
            ScanError::Device(msg) => write!(f, "scanner error: {msg}"),
            ScanError::InvalidSettings(msg) => write!(f, "invalid scan settings: {msg}"),
            ScanError::Cancelled => write!(f, "scan cancelled"),
        }
    }
}

impl std::error::Error for ScanError {}

/// Which acquisition API a device is reached through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    /// Windows Image Acquisition 2.0 (`IWiaDevMgr2`), built into Windows.
    Wia,
    /// TWAIN data sources through the TWAIN DSM (`TWAINDSM.dll`).
    Twain,
    /// SANE (`libsane`) on Linux and BSD.
    Sane,
    /// eSCL ("AirScan") over the network: every platform, iPad included.
    Escl,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Backend::Wia => "WIA",
            Backend::Twain => "TWAIN",
            Backend::Sane => "SANE",
            Backend::Escl => "AirScan",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorMode {
    Color,
    Grayscale,
    /// 1-bit black and white, thresholded by the device.
    LineArt,
}

impl ColorMode {
    pub const ALL: [ColorMode; 3] = [ColorMode::Color, ColorMode::Grayscale, ColorMode::LineArt];

    pub fn name(self) -> &'static str {
        match self {
            ColorMode::Color => "Color",
            ColorMode::Grayscale => "Grayscale",
            ColorMode::LineArt => "Line Art",
        }
    }
}

/// What a device supports, as reported by its backend.
#[derive(Debug, Clone, PartialEq)]
pub struct Capabilities {
    /// Supported resolutions in DPI, ascending.
    pub resolutions: Vec<u32>,
    pub color_modes: Vec<ColorMode>,
    /// Scan bed size in millimetres (width, height).
    pub bed_mm: [f32; 2],
    pub has_feeder: bool,
}

impl Capabilities {
    /// The supported resolution closest to `dpi`.
    pub fn nearest_resolution(&self, dpi: u32) -> Option<u32> {
        self.resolutions.iter().copied().min_by_key(|r| r.abs_diff(dpi))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceInfo {
    /// Backend-specific identifier (WIA device ID, TWAIN product name, SANE device name).
    pub id: String,
    pub name: String,
    pub manufacturer: String,
    pub backend: Backend,
    pub capabilities: Capabilities,
}

/// Region of the bed to scan, as fractions of its size (0..=1, top-left origin).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScanArea {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl ScanArea {
    pub const FULL: ScanArea = ScanArea { x: 0.0, y: 0.0, width: 1.0, height: 1.0 };

    /// Clamped to the bed, never empty.
    pub fn clamped(self) -> ScanArea {
        let x = self.x.clamp(0.0, 0.99);
        let y = self.y.clamp(0.0, 0.99);
        ScanArea { x, y, width: self.width.clamp(0.01, 1.0 - x), height: self.height.clamp(0.01, 1.0 - y) }
    }

    /// The area in pixels of an image `width` × `height`: (x, y, w, h).
    pub fn to_pixels(self, width: u32, height: u32) -> (u32, u32, u32, u32) {
        let a = self.clamped();
        let x = (a.x * width as f32).floor() as u32;
        let y = (a.y * height as f32).floor() as u32;
        let w = ((a.width * width as f32).round() as u32).clamp(1, width - x.min(width - 1));
        let h = ((a.height * height as f32).round() as u32).clamp(1, height - y.min(height - 1));
        (x, y, w, h)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScanSettings {
    pub dpi: u32,
    pub color_mode: ColorMode,
    pub area: ScanArea,
    /// Device-side adjustments, -1..=1 (0 = driver default).
    pub brightness: f32,
    pub contrast: f32,
}

impl Default for ScanSettings {
    fn default() -> Self {
        Self { dpi: 300, color_mode: ColorMode::Grayscale, area: ScanArea::FULL, brightness: 0.0, contrast: 0.0 }
    }
}

impl ScanSettings {
    /// Checks the settings against a device, snapping the resolution to a supported one.
    pub fn validated(&self, caps: &Capabilities) -> Result<ScanSettings, ScanError> {
        let dpi = caps
            .nearest_resolution(self.dpi)
            .ok_or_else(|| ScanError::InvalidSettings("the device reports no resolutions".into()))?;
        if !caps.color_modes.contains(&self.color_mode) {
            return Err(ScanError::InvalidSettings(format!("{} mode isn't supported", self.color_mode.name())));
        }
        Ok(ScanSettings { dpi, area: self.area.clamped(), ..self.clone() })
    }

    /// Settings for a quick preview of the whole bed.
    pub fn preview(&self) -> ScanSettings {
        ScanSettings { dpi: 75, area: ScanArea::FULL, ..self.clone() }
    }
}

/// An acquired image: straight (not premultiplied) RGBA8, row-major.
#[derive(Debug, Clone, PartialEq)]
pub struct ScannedImage {
    pub width: u32,
    pub height: u32,
    pub dpi: u32,
    pub rgba: Vec<u8>,
}

impl ScannedImage {
    pub fn new(width: u32, height: u32, dpi: u32, rgba: Vec<u8>) -> Result<Self, ScanError> {
        if width == 0 || height == 0 || rgba.len() != (width * height * 4) as usize {
            return Err(ScanError::Device(format!("{width}×{height} image with {} bytes", rgba.len())));
        }
        Ok(Self { width, height, dpi, rgba })
    }

    /// From 8-bit grey, as WIA and SANE deliver grayscale scans.
    pub fn from_gray(width: u32, height: u32, dpi: u32, gray: &[u8]) -> Result<Self, ScanError> {
        let rgba = gray.iter().flat_map(|&g| [g, g, g, 255]).collect();
        Self::new(width, height, dpi, rgba)
    }

    /// Premultiplied RGBA8, the layout `painter_core::TileStore::from_rgba` takes.
    pub fn to_premultiplied(&self) -> Vec<u8> {
        self.rgba
            .as_chunks::<4>().0.iter()
            .flat_map(|p| {
                let a = p[3] as u32;
                let m = |c: u8| ((c as u32 * a + 127) / 255) as u8;
                [m(p[0]), m(p[1]), m(p[2]), p[3]]
            })
            .collect()
    }

    /// Physical size in millimetres.
    pub fn size_mm(&self) -> [f32; 2] {
        let mm = |px: u32| px as f32 / self.dpi.max(1) as f32 * 25.4;
        [mm(self.width), mm(self.height)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> Capabilities {
        Capabilities {
            resolutions: vec![75, 150, 300, 600, 1200],
            color_modes: vec![ColorMode::Color, ColorMode::Grayscale],
            bed_mm: [216.0, 297.0],
            has_feeder: false,
        }
    }

    #[test]
    fn settings_snap_to_the_device() {
        let s = ScanSettings { dpi: 400, ..ScanSettings::default() }.validated(&caps()).unwrap();
        assert_eq!(s.dpi, 300);
        let line_art = ScanSettings { color_mode: ColorMode::LineArt, ..ScanSettings::default() };
        assert!(matches!(line_art.validated(&caps()), Err(ScanError::InvalidSettings(_))));
        assert_eq!(ScanSettings::default().preview().dpi, 75);
    }

    #[test]
    fn areas_clamp_to_the_bed() {
        let a = ScanArea { x: 0.5, y: -0.2, width: 0.9, height: 2.0 }.clamped();
        assert_eq!((a.x, a.y), (0.5, 0.0));
        assert!((a.width - 0.5).abs() < 1e-6 && (a.height - 1.0).abs() < 1e-6);
        assert_eq!(ScanArea::FULL.to_pixels(100, 50), (0, 0, 100, 50));
        assert_eq!(ScanArea { x: 0.25, y: 0.5, width: 0.5, height: 0.5 }.to_pixels(100, 50), (25, 25, 50, 25));
    }

    #[test]
    fn premultiplies_and_measures() {
        let image = ScannedImage::new(2, 1, 254, vec![200, 100, 0, 255, 200, 100, 0, 128]).unwrap();
        assert_eq!(image.to_premultiplied(), [200, 100, 0, 255, 100, 50, 0, 128]);
        assert!((image.size_mm()[0] - 0.2).abs() < 1e-4);
        assert!(ScannedImage::new(2, 2, 300, vec![0; 4]).is_err());
    }
}
