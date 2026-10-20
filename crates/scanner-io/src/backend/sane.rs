//! SANE on Linux and BSD (stub).
//!
//! Implementation plan (load `libsane.so.1` with `libloading`):
//!
//! 1. `sane_init`, then **devices** from `sane_get_devices(local_only = true)`
//!    (`name`, `vendor`, `model`).
//! 2. **Capabilities:** `sane_open`, walk `sane_get_option_descriptor` for the
//!    well-known options: `resolution` (word list or range), `mode` (string
//!    list: "Color", "Gray", "Lineart"), and the geometry options
//!    `tl-x`/`tl-y`/`br-x`/`br-y` whose ranges give the bed size (mm, fixed point).
//! 3. **Settings:** `sane_control_option(SET_VALUE)` for each, the crop area
//!    in millimetres, `brightness` / `contrast` when present.
//! 4. **Transfer:** `sane_start`, `sane_get_parameters` (format, bytes per
//!    line, depth), `sane_read` until `SANE_STATUS_EOF`, unpack 1/8/16-bit
//!    rows into [`crate::ScannedImage`].
//! 5. `sane_close`, `sane_exit`; `SANE_STATUS_CANCELLED` → [`ScanError::Cancelled`].

use super::ScannerBackend;
use crate::{Backend, DeviceInfo, ScanError, ScanSettings, ScannedImage};

#[derive(Default)]
pub struct SaneBackend {}

impl SaneBackend {
    fn unavailable(&self) -> ScanError {
        ScanError::BackendUnavailable { backend: Backend::Sane, reason: "libsane isn't wired up yet".into() }
    }
}

impl ScannerBackend for SaneBackend {
    fn backend(&self) -> Backend {
        Backend::Sane
    }

    fn is_available(&self) -> bool {
        cfg!(all(unix, not(target_os = "macos")))
    }

    fn devices(&mut self) -> Result<Vec<DeviceInfo>, ScanError> {
        Err(self.unavailable())
    }

    fn scan(&mut self, _device: &DeviceInfo, _settings: &ScanSettings) -> Result<ScannedImage, ScanError> {
        Err(self.unavailable())
    }
}
