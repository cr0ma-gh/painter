//! Acquisition backends. Each wraps one platform API behind [`ScannerBackend`].

pub mod escl;
mod http;
mod mdns;
pub mod sane;
pub mod twain;
pub mod wia;

use crate::{DeviceInfo, ScanError, ScanSettings, ScannedImage};

/// One acquisition API. Calls block until the device answers, so the app runs
/// `preview` and `scan` on a worker thread.
pub trait ScannerBackend: Send {
    fn backend(&self) -> crate::Backend;

    /// Whether this backend can work here at all (platform, installed runtime).
    fn is_available(&self) -> bool;

    /// Connected devices with their capabilities.
    fn devices(&mut self) -> Result<Vec<DeviceInfo>, ScanError>;

    /// A low-resolution scan of the whole bed for choosing the crop region.
    fn preview(&mut self, device: &DeviceInfo, settings: &ScanSettings) -> Result<ScannedImage, ScanError> {
        self.scan(device, &settings.preview())
    }

    /// The final scan with `settings` (already validated against the device).
    fn scan(&mut self, device: &DeviceInfo, settings: &ScanSettings) -> Result<ScannedImage, ScanError>;
}

/// Every backend for this platform, preferred first: WIA, then TWAIN on
/// Windows; SANE elsewhere; eSCL network scanners everywhere (iPad too).
pub fn backends() -> Vec<Box<dyn ScannerBackend>> {
    let all: Vec<Box<dyn ScannerBackend>> =
        vec![
            Box::new(wia::WiaBackend::default()),
            Box::new(twain::TwainBackend::default()),
            Box::new(sane::SaneBackend::default()),
            Box::new(escl::EsclBackend::default()),
        ];
    all.into_iter().filter(|b| b.is_available()).collect()
}

/// Devices from every available backend, plus the errors of backends that failed.
pub fn list_devices(backends: &mut [Box<dyn ScannerBackend>]) -> (Vec<DeviceInfo>, Vec<ScanError>) {
    let mut devices = Vec::new();
    let mut errors = Vec::new();
    for backend in backends.iter_mut() {
        match backend.devices() {
            Ok(found) => devices.extend(found),
            Err(err) => errors.push(err),
        }
    }
    (devices, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Backend, Capabilities, ColorMode};

    /// A pretend flatbed: an A4 page with a dark diagonal line.
    struct FakeScanner;

    impl ScannerBackend for FakeScanner {
        fn backend(&self) -> Backend {
            Backend::Wia
        }
        fn is_available(&self) -> bool {
            true
        }
        fn devices(&mut self) -> Result<Vec<DeviceInfo>, ScanError> {
            Ok(vec![DeviceInfo {
                id: "fake".into(),
                name: "Fake flatbed".into(),
                manufacturer: "Painter".into(),
                backend: Backend::Wia,
                capabilities: Capabilities {
                    resolutions: vec![75, 150, 300],
                    color_modes: vec![ColorMode::Grayscale],
                    bed_mm: [210.0, 297.0],
                    has_feeder: false,
                },
            }])
        }
        fn scan(&mut self, _device: &DeviceInfo, settings: &ScanSettings) -> Result<ScannedImage, ScanError> {
            let (w, h) = ((210.0 / 25.4 * settings.dpi as f32) as u32, (297.0 / 25.4 * settings.dpi as f32) as u32);
            let (x, y, cw, ch) = settings.area.to_pixels(w, h);
            let gray: Vec<u8> = (0..cw * ch).map(|i| if (i % cw + x) == (i / cw + y) { 20 } else { 235 }).collect();
            ScannedImage::from_gray(cw, ch, settings.dpi, &gray)
        }
    }

    #[test]
    fn a_backend_drives_the_whole_pipeline() {
        let mut backends: Vec<Box<dyn ScannerBackend>> = vec![Box::new(FakeScanner)];
        let (devices, errors) = list_devices(&mut backends);
        assert!(errors.is_empty());
        let device = &devices[0];
        let settings = ScanSettings { dpi: 280, ..ScanSettings::default() }.validated(&device.capabilities).unwrap();
        assert_eq!(settings.dpi, 300);

        let preview = backends[0].preview(device, &settings).unwrap();
        assert_eq!(preview.dpi, 75);
        let image = backends[0].scan(device, &ScanSettings { area: crate::ScanArea { x: 0.0, y: 0.0, width: 0.1, height: 0.1 }, ..settings }).unwrap();
        let prepared = crate::prepare_for_import(&image, &crate::ImportOptions::default());
        // Paper became transparent, the line stayed.
        assert_eq!(prepared.rgba[3 + 4], 0);
        assert!(prepared.rgba[3] > 200);
    }

    #[test]
    fn stub_backends_say_why_they_are_unavailable() {
        let mut twain = twain::TwainBackend::default();
        match twain.devices() {
            Err(ScanError::BackendUnavailable { backend: Backend::Twain, .. }) => {}
            other => panic!("{other:?}"),
        }
    }
}
