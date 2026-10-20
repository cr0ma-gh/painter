//! Windows Image Acquisition (WIA 1.0 interfaces, available on every Windows
//! version since XP and served by the `stisvc` service).
//!
//! 1. **Devices:** `IWiaDevMgr::EnumDeviceInfo(WIA_DEVINFO_ENUM_LOCAL)`; each
//!    entry's property storage gives `WIA_DIP_DEV_ID`, `WIA_DIP_DEV_NAME`,
//!    `WIA_DIP_VEND_DESC`; `WIA_DIP_DEV_TYPE`'s high word keeps scanners.
//! 2. **Capabilities:** `CreateDevice(id)`; the first image child of the root
//!    item is the flatbed. Its `WIA_IPS_XRES` attributes (list or range) give
//!    the resolutions, `WIA_IPA_DATATYPE` the colour modes; the root's
//!    `WIA_DPS_HORIZONTAL/VERTICAL_BED_SIZE` (1/1000 inch) the bed.
//! 3. **Settings:** resolution, data type and depth, then the crop window in
//!    pixels (`WIA_IPS_XPOS`…`YEXTENT`) and brightness / contrast mapped onto
//!    the driver's ranges.
//! 4. **Transfer:** `IWiaDataTransfer::idtGetData` with `TYMED_FILE`; WIA
//!    writes a temporary image file (BMP by default), which is decoded and
//!    released (`ReleaseStgMedium` deletes it).
//!
//! Every call runs on its own short-lived thread in the multithreaded COM
//! apartment, so the caller's thread (the UI) keeps whatever apartment it has.
//! Only device IDs cross threads.

use super::ScannerBackend;
use crate::{Backend, DeviceInfo, ScanError, ScanSettings, ScannedImage};

#[derive(Default)]
pub struct WiaBackend {}

impl ScannerBackend for WiaBackend {
    fn backend(&self) -> Backend {
        Backend::Wia
    }

    fn is_available(&self) -> bool {
        cfg!(windows)
    }

    fn devices(&mut self) -> Result<Vec<DeviceInfo>, ScanError> {
        #[cfg(windows)]
        return com::on_com_thread(com::devices);
        #[cfg(not(windows))]
        Err(unavailable())
    }

    fn scan(&mut self, device: &DeviceInfo, settings: &ScanSettings) -> Result<ScannedImage, ScanError> {
        #[cfg(windows)]
        {
            let (id, settings) = (device.id.clone(), settings.clone());
            com::on_com_thread(move || com::scan(&id, &settings))
        }
        #[cfg(not(windows))]
        {
            let _ = (device, settings);
            Err(unavailable())
        }
    }
}

#[cfg(not(windows))]
fn unavailable() -> ScanError {
    ScanError::BackendUnavailable { backend: Backend::Wia, reason: "WIA exists only on Windows".into() }
}

/// Values from a WIA property's valid-values attribute.
#[derive(Debug, Clone, PartialEq)]
enum Valid {
    List(Vec<i32>),
    Range { min: i32, nominal: i32, max: i32, step: i32 },
}

impl Valid {
    /// WIA attribute vectors: lists are `[count, nominal, values…]`, ranges
    /// `[min, nominal, max, step]`.
    fn from_attribute(flags: u32, values: &[i32]) -> Option<Valid> {
        const WIA_PROP_RANGE: u32 = 0x10;
        const WIA_PROP_LIST: u32 = 0x20;
        if flags & WIA_PROP_RANGE != 0 && values.len() >= 4 {
            Some(Valid::Range { min: values[0], nominal: values[1], max: values[2], step: values[3].max(1) })
        } else if flags & WIA_PROP_LIST != 0 && values.len() >= 2 {
            Some(Valid::List(values[2..].to_vec()))
        } else {
            None
        }
    }

    /// Resolutions to offer: the list, or common DPI values inside the range.
    fn resolutions(&self) -> Vec<u32> {
        let mut out: Vec<u32> = match self {
            Valid::List(values) => values.iter().filter(|&&v| v > 0).map(|&v| v as u32).collect(),
            Valid::Range { min, max, step, .. } => [75, 100, 150, 200, 300, 400, 600, 1200, 2400]
                .into_iter()
                .filter(|&v| v >= *min && v <= *max && (v - min) % step == 0)
                .map(|v| v as u32)
                .collect(),
        };
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Maps -1..=1 onto the range, 0 being the driver's nominal value.
    fn scaled(&self, t: f32) -> Option<i32> {
        let Valid::Range { min, nominal, max, step } = *self else {
            return None;
        };
        let v = if t >= 0.0 { nominal as f32 + t * (max - nominal) as f32 } else { nominal as f32 + t * (nominal - min) as f32 };
        let snapped = min + ((v - min as f32) / step as f32).round() as i32 * step;
        Some(snapped.clamp(min, max))
    }
}

#[cfg(windows)]
mod com {
    use windows::core::{Interface, BSTR, PROPVARIANT, PWSTR};
    use windows::Win32::Devices::ImageAcquisition::*;
    use windows::Win32::System::Com::StructuredStorage::{PROPSPEC, PROPSPEC_0, PRSPEC_PROPID};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, STGMEDIUM, TYMED_FILE};
    use windows::Win32::System::Ole::ReleaseStgMedium;

    use super::Valid;
    use crate::{Backend, Capabilities, ColorMode, DeviceInfo, ScanError, ScanSettings, ScannedImage};

    const STI_DEVICE_TYPE_SCANNER: i32 = 1;
    const FLATBED: i32 = 2; // WIA_DPS_DOCUMENT_HANDLING_SELECT value
    const VT_VECTOR_I4: u16 = 0x1003;
    const VT_VECTOR_UI4: u16 = 0x1013;

    fn err(context: &str, e: windows::core::Error) -> ScanError {
        ScanError::Device(format!("{context}: {} (0x{:08X})", e.message(), e.code().0))
    }

    /// Runs `f` on a fresh thread in the multithreaded apartment.
    pub fn on_com_thread<T: Send + 'static>(f: impl FnOnce() -> Result<T, ScanError> + Send + 'static) -> Result<T, ScanError> {
        std::thread::spawn(move || unsafe {
            let init = CoInitializeEx(None, COINIT_MULTITHREADED);
            let result = f();
            if init.is_ok() {
                CoUninitialize();
            }
            result
        })
        .join()
        .unwrap_or_else(|_| Err(ScanError::Device("the WIA worker panicked".into())))
    }

    fn manager() -> Result<IWiaDevMgr, ScanError> {
        unsafe { CoCreateInstance(&WiaDevMgr, None, CLSCTX_LOCAL_SERVER) }.map_err(|e| {
            ScanError::BackendUnavailable { backend: Backend::Wia, reason: format!("the WIA service isn't reachable: {}", e.message()) }
        })
    }

    fn spec(id: u32) -> PROPSPEC {
        PROPSPEC { ulKind: PRSPEC_PROPID, Anonymous: PROPSPEC_0 { propid: id } }
    }

    fn read(storage: &IWiaPropertyStorage, id: u32) -> Option<PROPVARIANT> {
        let mut value = PROPVARIANT::default();
        unsafe { storage.ReadMultiple(1, &spec(id), &mut value) }.ok()?;
        (!value.is_empty()).then_some(value)
    }

    fn read_i32(storage: &IWiaPropertyStorage, id: u32) -> Option<i32> {
        read(storage, id).and_then(|v| i32::try_from(&v).ok())
    }

    fn read_string(storage: &IWiaPropertyStorage, id: u32) -> Option<String> {
        read(storage, id).and_then(|v| BSTR::try_from(&v).ok()).map(|s| s.to_string())
    }

    fn write_i32(storage: &IWiaPropertyStorage, id: u32, value: i32) -> windows::core::Result<()> {
        let value = PROPVARIANT::from(value);
        unsafe { storage.WriteMultiple(1, &spec(id), &value, WIA_IPA_FIRST) }
    }

    /// The property's valid values (list or range), if the driver states them.
    fn valid(storage: &IWiaPropertyStorage, id: u32) -> Option<Valid> {
        let mut flags = 0u32;
        let mut value = PROPVARIANT::default();
        unsafe { storage.GetPropertyAttributes(1, &spec(id), &mut flags, &mut value) }.ok()?;
        let raw = value.as_raw();
        // SAFETY: the union member read matches the variant type checked first.
        let values: Vec<i32> = unsafe {
            let vt = raw.Anonymous.Anonymous.vt;
            if vt != VT_VECTOR_I4 && vt != VT_VECTOR_UI4 {
                return None;
            }
            let cal = &raw.Anonymous.Anonymous.Anonymous.cal;
            if cal.pElems.is_null() {
                return None;
            }
            std::slice::from_raw_parts(cal.pElems, cal.cElems as usize).to_vec()
        };
        Valid::from_attribute(flags, &values)
    }

    fn storage(item: &IWiaItem) -> Result<IWiaPropertyStorage, ScanError> {
        item.cast().map_err(|e| err("property storage", e))
    }

    pub fn devices() -> Result<Vec<DeviceInfo>, ScanError> {
        let manager = manager()?;
        let list = unsafe { manager.EnumDeviceInfo(WIA_DEVINFO_ENUM_LOCAL as i32) }.map_err(|e| err("listing devices", e))?;
        let mut out = Vec::new();
        loop {
            let mut entry = [None];
            let mut fetched = 0;
            if unsafe { list.Next(1, entry.as_mut_ptr(), &mut fetched) }.is_err() || fetched == 0 {
                break;
            }
            let Some(info) = entry[0].take() else { break };
            let Some(id) = read_string(&info, WIA_DIP_DEV_ID) else { continue };
            if read_i32(&info, WIA_DIP_DEV_TYPE).is_some_and(|t| (t >> 16) != STI_DEVICE_TYPE_SCANNER) {
                continue;
            }
            let name = read_string(&info, WIA_DIP_DEV_NAME).unwrap_or_else(|| id.clone());
            let manufacturer = read_string(&info, WIA_DIP_VEND_DESC).unwrap_or_default();
            // A device that can't be opened (offline network scanner) is still listed, with no capabilities.
            let capabilities = capabilities(&manager, &id).unwrap_or(Capabilities {
                resolutions: Vec::new(),
                color_modes: Vec::new(),
                bed_mm: [0.0; 2],
                has_feeder: false,
            });
            out.push(DeviceInfo { id, name, manufacturer, backend: Backend::Wia, capabilities });
        }
        Ok(out)
    }

    /// The root item and its flatbed (first image) child.
    fn open(manager: &IWiaDevMgr, id: &str) -> Result<(IWiaItem, IWiaItem), ScanError> {
        let root = unsafe { manager.CreateDevice(&BSTR::from(id)) }.map_err(|e| err("opening the scanner", e))?;
        let children = unsafe { root.EnumChildItems() }.map_err(|e| err("reading the scanner", e))?;
        loop {
            let mut entry = [None];
            let mut fetched = 0;
            if unsafe { children.Next(1, entry.as_mut_ptr(), &mut fetched) }.is_err() || fetched == 0 {
                break;
            }
            if let Some(item) = entry[0].take() {
                let kind = unsafe { item.GetItemType() }.unwrap_or(0);
                if kind & WiaItemTypeImage as i32 != 0 {
                    return Ok((root, item));
                }
            }
        }
        Err(ScanError::Device("the scanner exposes no image source".into()))
    }

    fn capabilities(manager: &IWiaDevMgr, id: &str) -> Result<Capabilities, ScanError> {
        let (root, item) = open(manager, id)?;
        let (root_props, props) = (storage(&root)?, storage(&item)?);
        let resolutions = valid(&props, WIA_IPS_XRES).map(|v| v.resolutions()).unwrap_or_default();
        let types = match valid(&props, WIA_IPA_DATATYPE) {
            Some(Valid::List(values)) => values,
            _ => vec![WIA_DATA_COLOR as i32],
        };
        let color_modes = ColorMode::ALL.into_iter().filter(|m| types.contains(&data_type(*m))).collect();
        let bed = |id| read_i32(&root_props, id).map_or(0.0, |thousandths| thousandths as f32 * 0.0254);
        let handling = read_i32(&root_props, WIA_DPS_DOCUMENT_HANDLING_CAPABILITIES).unwrap_or(0);
        Ok(Capabilities {
            resolutions,
            color_modes,
            bed_mm: [bed(WIA_DPS_HORIZONTAL_BED_SIZE), bed(WIA_DPS_VERTICAL_BED_SIZE)],
            has_feeder: handling & 1 != 0,
        })
    }

    fn data_type(mode: ColorMode) -> i32 {
        match mode {
            ColorMode::Color => WIA_DATA_COLOR as i32,
            ColorMode::Grayscale => WIA_DATA_GRAYSCALE as i32,
            ColorMode::LineArt => WIA_DATA_THRESHOLD as i32,
        }
    }

    pub fn scan(id: &str, settings: &ScanSettings) -> Result<ScannedImage, ScanError> {
        let manager = manager()?;
        let (root, item) = open(&manager, id)?;
        let (root_props, props) = (storage(&root)?, storage(&item)?);
        let set = |storage: &IWiaPropertyStorage, prop: u32, value: i32, what: &str| {
            write_i32(storage, prop, value).map_err(|e| ScanError::InvalidSettings(format!("{what} {value}: {}", e.message())))
        };
        // Optional on many drivers; a failure here isn't fatal.
        let _ = write_i32(&root_props, WIA_DPS_DOCUMENT_HANDLING_SELECT, FLATBED);
        set(&props, WIA_IPA_DATATYPE, data_type(settings.color_mode), "colour mode")?;
        let depth = match settings.color_mode {
            ColorMode::Color => 24,
            ColorMode::Grayscale => 8,
            ColorMode::LineArt => 1,
        };
        let _ = write_i32(&props, WIA_IPA_DEPTH, depth);
        let dpi = settings.dpi as i32;
        set(&props, WIA_IPS_XRES, dpi, "resolution")?;
        set(&props, WIA_IPS_YRES, dpi, "resolution")?;

        // Crop window in pixels at this resolution.
        let bed_px = |prop| read_i32(&root_props, prop).map(|thousandths| (thousandths as i64 * dpi as i64 / 1000) as u32);
        if let (Some(w), Some(h)) = (bed_px(WIA_DPS_HORIZONTAL_BED_SIZE), bed_px(WIA_DPS_VERTICAL_BED_SIZE)) {
            if w > 0 && h > 0 {
                let (x, y, cw, ch) = settings.area.to_pixels(w, h);
                // Position first, then extent: drivers validate extents against the position.
                let _ = write_i32(&props, WIA_IPS_XPOS, x as i32);
                let _ = write_i32(&props, WIA_IPS_YPOS, y as i32);
                let _ = write_i32(&props, WIA_IPS_XEXTENT, cw as i32);
                let _ = write_i32(&props, WIA_IPS_YEXTENT, ch as i32);
            }
        }
        for (prop, value) in [(WIA_IPS_BRIGHTNESS, settings.brightness), (WIA_IPS_CONTRAST, settings.contrast)] {
            if let Some(v) = valid(&props, prop).and_then(|range| range.scaled(value)) {
                let _ = write_i32(&props, prop, v);
            }
        }
        let _ = write_i32(&props, WIA_IPA_TYMED, TYMED_FILE.0);

        let transfer: IWiaDataTransfer = item.cast().map_err(|e| err("data transfer", e))?;
        let mut medium = STGMEDIUM { tymed: TYMED_FILE.0 as u32, ..Default::default() };
        unsafe { transfer.idtGetData(&mut medium, None) }.map_err(|e| err("scanning", e))?;
        // SAFETY: a TYMED_FILE medium carries the name of the file WIA wrote.
        let path = unsafe { medium.u.lpszFileName };
        let bytes = if path.is_null() { None } else { unsafe { PWSTR(path.0).to_string() }.ok().and_then(|p| std::fs::read(p).ok()) };
        // Frees the name and deletes the temporary file.
        unsafe { ReleaseStgMedium(&mut medium) };
        let bytes = bytes.ok_or_else(|| ScanError::Device("the scanner returned no image".into()))?;
        crate::decode::decode(&bytes, settings.dpi)
    }
}

#[cfg(test)]
mod tests {
    use super::Valid;

    #[test]
    fn reads_wia_attribute_vectors() {
        let list = Valid::from_attribute(0x20, &[4, 300, 100, 300, 75, 600]).unwrap();
        assert_eq!(list.resolutions(), [75, 100, 300, 600]);
        let range = Valid::from_attribute(0x10, &[50, 300, 1200, 50]).unwrap();
        assert_eq!(range.resolutions(), [100, 150, 200, 300, 400, 600, 1200]);
        assert!(Valid::from_attribute(0x02, &[1, 2]).is_none(), "flag properties aren't value sets");

        let brightness = Valid::Range { min: -1000, nominal: 0, max: 1000, step: 1 };
        assert_eq!(brightness.scaled(0.0), Some(0));
        assert_eq!(brightness.scaled(0.5), Some(500));
        assert_eq!(brightness.scaled(-1.0), Some(-1000));
        assert_eq!(Valid::List(vec![1]).scaled(0.5), None);
    }
}
