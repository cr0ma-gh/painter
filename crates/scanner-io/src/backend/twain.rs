//! TWAIN through the data source manager (stub).
//!
//! Implementation plan (load `TWAINDSM.dll` with `libloading`, call `DSM_Entry`):
//!
//! 1. `DG_CONTROL / DAT_PARENT / MSG_OPENDSM` with the main window handle.
//! 2. **Devices:** `DAT_IDENTITY / MSG_GETFIRST`, then `MSG_GETNEXT` until
//!    `TWRC_ENDOFLIST`; `ProductName` and `Manufacturer` give the names.
//! 3. **Capabilities:** `MSG_OPENDS`, then `DAT_CAPABILITY / MSG_GET` for
//!    `ICAP_XRESOLUTION` (enumeration or range), `ICAP_PIXELTYPE`
//!    (`TWPT_RGB`, `TWPT_GRAY`, `TWPT_BW`), `ICAP_PHYSICALWIDTH` / `HEIGHT`
//!    (inches) and `CAP_FEEDERENABLED`.
//! 4. **Settings:** `MSG_SET` the same capabilities, plus `DAT_IMAGELAYOUT`
//!    for the crop frame (inches) and `ICAP_BRIGHTNESS` / `ICAP_CONTRAST`
//!    (-1000..=1000).
//! 5. **Transfer:** `DAT_USERINTERFACE / MSG_ENABLEDS` with `ShowUI = false`,
//!    pump window messages until `MSG_XFERREADY`, then a native transfer
//!    (`DAT_IMAGENATIVEXFER`, a DIB handle) or memory transfer
//!    (`DAT_IMAGEMEMXFER`, strips), and `DAT_PENDINGXFERS / MSG_ENDXFER`.
//! 6. Close with `MSG_DISABLEDS`, `MSG_CLOSEDS`, `MSG_CLOSEDSM`.

use super::ScannerBackend;
use crate::{Backend, DeviceInfo, ScanError, ScanSettings, ScannedImage};

#[derive(Default)]
pub struct TwainBackend {}

impl TwainBackend {
    fn unavailable(&self) -> ScanError {
        ScanError::BackendUnavailable { backend: Backend::Twain, reason: "the TWAIN data source manager isn't wired up yet".into() }
    }
}

impl ScannerBackend for TwainBackend {
    fn backend(&self) -> Backend {
        Backend::Twain
    }

    fn is_available(&self) -> bool {
        cfg!(windows)
    }

    fn devices(&mut self) -> Result<Vec<DeviceInfo>, ScanError> {
        Err(self.unavailable())
    }

    fn scan(&mut self, _device: &DeviceInfo, _settings: &ScanSettings) -> Result<ScannedImage, ScanError> {
        Err(self.unavailable())
    }
}
