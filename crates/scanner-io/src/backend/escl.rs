//! eSCL ("AirScan", "Mopria scan"): scanning over the network with plain
//! HTTP and XML, what iPads and Macs use with most network scanners and
//! multifunction printers made since about 2014 (Brother, Canon, Epson, HP,
//! Kyocera, Ricoh, Xerox…). It works the same on every platform, iPadOS
//! included, where USB and WIA scanners can't be reached.
//!
//! 1. **Find** scanners: DNS-SD `_uscan._tcp` ([`super::mdns`] on computers,
//!    Bonjour on iPad through [`add_targets`]) or an address typed in
//!    ([`add_address`]).
//! 2. **Capabilities:** `GET {root}/ScannerCapabilities`: make and model,
//!    platen size (1/300 inch), colour modes, resolutions, file formats,
//!    whether there's a document feeder.
//! 3. **Scan:** `POST {root}/ScanJobs` with the settings (region, mode,
//!    resolution, format) → `201 Created` and the job's `Location`; then
//!    `GET {job}/NextDocument` until the page comes (`503` while the scanner
//!    is still busy), decoded as JPEG or PNG; then the job is deleted.

use std::sync::Mutex;
use std::time::Duration;

use super::http::{request, Url};
use super::ScannerBackend;
use crate::{Backend, Capabilities, ColorMode, DeviceInfo, ScanError, ScanSettings, ScannedImage};

/// A scanner's eSCL root, e.g. `http://192.168.1.30:80/eSCL`, and its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub url: String,
}

/// Scanners found by Bonjour (iPad) or typed in, besides those found by mDNS.
static KNOWN: Mutex<Vec<Target>> = Mutex::new(Vec::new());

/// Adds scanners found outside this crate (Bonjour on iPad).
pub fn add_targets(targets: impl IntoIterator<Item = Target>) {
    if let Ok(mut known) = KNOWN.lock() {
        for t in targets {
            if !known.iter().any(|k| k.url == t.url) {
                known.push(t);
            }
        }
    }
}

/// Adds a scanner by address: `192.168.1.30`, `scanner.local:8080`, or a
/// full `http://…/eSCL` root. Returns its root URL.
pub fn add_address(address: &str) -> Result<String, String> {
    let a = address.trim().trim_end_matches('/');
    if a.is_empty() {
        return Err("Type the scanner's address (e.g. 192.168.1.30)".into());
    }
    let url = if a.starts_with("http://") {
        if Url::parse(a).is_some_and(|u| u.path.len() > 1) {
            a.to_string()
        } else {
            format!("{a}/eSCL")
        }
    } else if a.starts_with("https://") {
        return Err("Secure (https) scanning isn't supported; use the scanner's plain address".into());
    } else {
        format!("http://{a}/eSCL")
    };
    Url::parse(&url).ok_or_else(|| format!("{address} isn't an address"))?;
    add_targets([Target { name: a.to_string(), url: url.clone() }]);
    Ok(url)
}

fn known() -> Vec<Target> {
    KNOWN.lock().map(|k| k.clone()).unwrap_or_default()
}

/// What `ScannerCapabilities` says, the parts used here.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EsclCaps {
    pub make_and_model: String,
    pub version: String,
    /// Platen size in 1/300 inch.
    pub max_width: u32,
    pub max_height: u32,
    /// eSCL names: `RGB24`, `Grayscale8`, `BlackAndWhite1`…
    pub color_modes: Vec<String>,
    pub resolutions: Vec<u32>,
    pub formats: Vec<String>,
    pub has_feeder: bool,
}

/// Elements anywhere under `node` with this local name.
fn all<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &'a str) -> impl Iterator<Item = roxmltree::Node<'a, 'i>> + 'a {
    node.descendants().filter(move |n| n.is_element() && n.tag_name().name() == name)
}

fn text_of(node: roxmltree::Node, name: &str) -> Option<String> {
    all(node, name).next().and_then(|n| n.text()).map(|t| t.trim().to_string())
}

fn num_of(node: roxmltree::Node, name: &str) -> Option<u32> {
    text_of(node, name)?.parse().ok()
}

/// Parses `ScannerCapabilities` (the platen's; the feeder is only noted).
pub fn parse_capabilities(xml: &str) -> Result<EsclCaps, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("unreadable capabilities: {e}"))?;
    let root = doc.root_element();
    let platen = all(root, "PlatenInputCaps").next().ok_or("the scanner has no flatbed")?;
    let mut caps = EsclCaps {
        make_and_model: text_of(root, "MakeAndModel").unwrap_or_else(|| "eSCL scanner".into()),
        version: text_of(root, "Version").unwrap_or_else(|| "2.0".into()),
        max_width: num_of(platen, "MaxWidth").unwrap_or(2550),
        max_height: num_of(platen, "MaxHeight").unwrap_or(3508),
        has_feeder: all(root, "Adf").next().is_some(),
        ..EsclCaps::default()
    };
    for m in all(platen, "ColorMode").filter_map(|n| n.text()) {
        let m = m.trim().to_string();
        if !caps.color_modes.contains(&m) {
            caps.color_modes.push(m);
        }
    }
    for r in all(platen, "DiscreteResolution") {
        if let Some(x) = num_of(r, "XResolution") {
            caps.resolutions.push(x);
        }
    }
    for range in all(platen, "XResolutionRange") {
        // A range: the usual steps inside it.
        let (min, max) = (num_of(range, "Min").unwrap_or(75), num_of(range, "Max").unwrap_or(600));
        caps.resolutions.extend([75, 100, 150, 200, 300, 400, 600, 1200].into_iter().filter(|r| (min..=max).contains(r)));
    }
    caps.resolutions.sort_unstable();
    caps.resolutions.dedup();
    for f in all(platen, "DocumentFormat").chain(all(platen, "DocumentFormatExt")).filter_map(|n| n.text()) {
        let f = f.trim().to_string();
        if !caps.formats.contains(&f) {
            caps.formats.push(f);
        }
    }
    Ok(caps)
}

impl EsclCaps {
    pub fn capabilities(&self) -> Capabilities {
        let mut modes = Vec::new();
        for (escl, mode) in [("RGB24", ColorMode::Color), ("Grayscale8", ColorMode::Grayscale), ("BlackAndWhite1", ColorMode::LineArt)]
        {
            if self.color_modes.iter().any(|m| m == escl) {
                modes.push(mode);
            }
        }
        Capabilities {
            resolutions: self.resolutions.clone(),
            color_modes: modes,
            bed_mm: [self.max_width as f32 / 300.0 * 25.4, self.max_height as f32 / 300.0 * 25.4],
            has_feeder: self.has_feeder,
        }
    }

    /// The format to ask for: a picture the app can read.
    fn format(&self) -> Option<&'static str> {
        ["image/jpeg", "image/png"]
            .into_iter()
            .find(|f| self.formats.iter().any(|g| g == f))
            .or(self.formats.is_empty().then_some("image/jpeg"))
    }
}

/// The `ScanSettings` document for a job.
pub fn scan_settings_xml(caps: &EsclCaps, settings: &ScanSettings, format: &str) -> String {
    let a = settings.area.clamped();
    let (w, h) = (caps.max_width as f32, caps.max_height as f32);
    let (x, y) = ((a.x * w).round() as u32, (a.y * h).round() as u32);
    let (cw, ch) = (((a.width * w).round() as u32).max(1), ((a.height * h).round() as u32).max(1));
    let mode = match settings.color_mode {
        ColorMode::Color => "RGB24",
        ColorMode::Grayscale => "Grayscale8",
        ColorMode::LineArt => "BlackAndWhite1",
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<scan:ScanSettings xmlns:scan="http://schemas.hp.com/imaging/escl/2011/05/03" xmlns:pwg="http://www.pwg.org/schemas/2010/12/sm">
  <pwg:Version>{version}</pwg:Version>
  <pwg:ScanRegions>
    <pwg:ScanRegion>
      <pwg:ContentRegionUnits>escl:ThreeHundredthsOfInches</pwg:ContentRegionUnits>
      <pwg:XOffset>{x}</pwg:XOffset>
      <pwg:YOffset>{y}</pwg:YOffset>
      <pwg:Width>{cw}</pwg:Width>
      <pwg:Height>{ch}</pwg:Height>
    </pwg:ScanRegion>
  </pwg:ScanRegions>
  <pwg:InputSource>Platen</pwg:InputSource>
  <scan:ColorMode>{mode}</scan:ColorMode>
  <scan:XResolution>{dpi}</scan:XResolution>
  <scan:YResolution>{dpi}</scan:YResolution>
  <pwg:DocumentFormat>{format}</pwg:DocumentFormat>
  <scan:DocumentFormatExt>{format}</scan:DocumentFormatExt>
</scan:ScanSettings>
"#,
        version = caps.version,
        dpi = settings.dpi,
    )
}

/// The width asked for, in 1/300 inch.
fn region_width(caps: &EsclCaps, settings: &ScanSettings) -> u32 {
    ((settings.area.clamped().width * caps.max_width as f32).round() as u32).max(1)
}

/// The resolution a scan really has: some devices send another one than asked
/// (a Brother sends 200 dpi for 100), so it's worked out from the width
/// received; the one asked for when they agree within 3 %.
pub fn actual_dpi(pixels: u32, width_300ths: u32, asked: u32) -> u32 {
    let measured = pixels as f32 / (width_300ths as f32 / 300.0);
    if (measured - asked as f32).abs() <= asked as f32 * 0.03 {
        asked
    } else {
        (measured.round() as u32).max(1)
    }
}

fn device_error(msg: impl Into<String>) -> ScanError {
    ScanError::Device(msg.into())
}

fn capabilities_of(root: &Url) -> Result<EsclCaps, ScanError> {
    let url =
        root.join(&format!("{}/ScannerCapabilities", root.path.trim_end_matches('/'))).ok_or_else(|| device_error("bad address"))?;
    let r = request("GET", &url, None, Duration::from_secs(6)).map_err(device_error)?;
    if r.status != 200 {
        return Err(device_error(format!("the scanner answered {} to ScannerCapabilities", r.status)));
    }
    parse_capabilities(&String::from_utf8_lossy(&r.body)).map_err(device_error)
}

#[derive(Default)]
pub struct EsclBackend {}

impl ScannerBackend for EsclBackend {
    fn backend(&self) -> Backend {
        Backend::Escl
    }

    fn is_available(&self) -> bool {
        true
    }

    fn devices(&mut self) -> Result<Vec<DeviceInfo>, ScanError> {
        let mut targets = known();
        // iPadOS doesn't let apps multicast; the app's Bonjour search fills `known` there.
        if !cfg!(target_os = "ios") {
            for t in super::mdns::discover(Duration::from_millis(1500)) {
                if !targets.iter().any(|k| k.url == t.url) {
                    targets.push(t);
                }
            }
        }
        let mut devices = Vec::new();
        let mut last_error = None;
        for t in targets {
            let Some(root) = Url::parse(&t.url) else { continue };
            match capabilities_of(&root) {
                Ok(caps) => devices.push(DeviceInfo {
                    id: t.url.clone(),
                    name: if caps.make_and_model.is_empty() { t.name.clone() } else { caps.make_and_model.clone() },
                    manufacturer: caps.make_and_model.split_whitespace().next().unwrap_or("").to_string(),
                    backend: Backend::Escl,
                    capabilities: caps.capabilities(),
                }),
                Err(e) => last_error = Some(device_error(format!("{} ({}): {e}", t.name, root.host))),
            }
        }
        match (devices.is_empty(), last_error) {
            (true, Some(e)) => Err(e),
            _ => Ok(devices),
        }
    }

    fn scan(&mut self, device: &DeviceInfo, settings: &ScanSettings) -> Result<ScannedImage, ScanError> {
        let root = Url::parse(&device.id).ok_or_else(|| device_error("bad scanner address"))?;
        let caps = capabilities_of(&root)?;
        let format =
            caps.format().ok_or_else(|| device_error("the scanner only sends formats Painter can't read (no JPEG or PNG)"))?;
        let body = scan_settings_xml(&caps, settings, format);
        let jobs = root.join(&format!("{}/ScanJobs", root.path.trim_end_matches('/'))).ok_or_else(|| device_error("bad address"))?;
        // A scanner finishing its last job answers 503 for a few seconds.
        let mut created =
            request("POST", &jobs, Some(("text/xml", body.as_bytes())), Duration::from_secs(30)).map_err(device_error)?;
        for _ in 0..5 {
            if created.status != 503 {
                break;
            }
            std::thread::sleep(Duration::from_secs(2));
            created = request("POST", &jobs, Some(("text/xml", body.as_bytes())), Duration::from_secs(30)).map_err(device_error)?;
        }
        match created.status {
            201 => {}
            503 => return Err(device_error("the scanner is busy; try again in a moment")),
            409 => return Err(device_error("the scanner is in use or its lid/feeder isn't ready")),
            s => return Err(device_error(format!("the scanner refused the job ({s})"))),
        }
        let job =
            created.header("Location").and_then(|l| jobs.join(l)).ok_or_else(|| device_error("the scanner gave no job address"))?;
        let next =
            job.join(&format!("{}/NextDocument", job.path.trim_end_matches('/'))).ok_or_else(|| device_error("bad job address"))?;
        // The page comes once the scan head has passed; 503 means not yet.
        let mut result = Err(device_error("the scan didn't arrive"));
        for _ in 0..90 {
            match request("GET", &next, None, Duration::from_secs(120)) {
                Ok(r) if r.status == 200 && !r.body.is_empty() => {
                    result = crate::decode::decode(&r.body, settings.dpi).map(|mut img| {
                        img.dpi = actual_dpi(img.width, region_width(&caps, settings), settings.dpi);
                        img
                    });
                    break;
                }
                Ok(r) if r.status == 503 => std::thread::sleep(Duration::from_secs(1)),
                Ok(r) => {
                    result = Err(device_error(format!("the scanner answered {} instead of the page", r.status)));
                    break;
                }
                Err(e) => {
                    result = Err(device_error(e));
                    break;
                }
            }
        }
        // A flatbed job ends when NextDocument answers 404 (no more pages);
        // until then the scanner stays busy. Then the job is removed.
        if result.is_ok() {
            let _ = request("GET", &next, None, Duration::from_secs(20));
        }
        let _ = request("DELETE", &job, None, Duration::from_secs(5));
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ScanArea;

    const CAPS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<scan:ScannerCapabilities xmlns:pwg="http://www.pwg.org/schemas/2010/12/sm" xmlns:scan="http://schemas.hp.com/imaging/escl/2011/05/03">
  <pwg:Version>2.63</pwg:Version>
  <pwg:MakeAndModel>Brother MFC-J5340DW</pwg:MakeAndModel>
  <scan:Platen><scan:PlatenInputCaps>
    <scan:MinWidth>16</scan:MinWidth><scan:MaxWidth>2550</scan:MaxWidth>
    <scan:MinHeight>16</scan:MinHeight><scan:MaxHeight>3508</scan:MaxHeight>
    <scan:SettingProfiles><scan:SettingProfile>
      <scan:ColorModes><scan:ColorMode>BlackAndWhite1</scan:ColorMode><scan:ColorMode>Grayscale8</scan:ColorMode><scan:ColorMode>RGB24</scan:ColorMode></scan:ColorModes>
      <scan:DocumentFormats><pwg:DocumentFormat>application/pdf</pwg:DocumentFormat><pwg:DocumentFormat>image/jpeg</pwg:DocumentFormat></scan:DocumentFormats>
      <scan:SupportedResolutions><scan:DiscreteResolutions>
        <scan:DiscreteResolution><scan:XResolution>100</scan:XResolution><scan:YResolution>100</scan:YResolution></scan:DiscreteResolution>
        <scan:DiscreteResolution><scan:XResolution>300</scan:XResolution><scan:YResolution>300</scan:YResolution></scan:DiscreteResolution>
        <scan:DiscreteResolution><scan:XResolution>600</scan:XResolution><scan:YResolution>600</scan:YResolution></scan:DiscreteResolution>
      </scan:DiscreteResolutions></scan:SupportedResolutions>
    </scan:SettingProfile></scan:SettingProfiles>
  </scan:PlatenInputCaps></scan:Platen>
  <scan:Adf><scan:AdfSimplexInputCaps/></scan:Adf>
</scan:ScannerCapabilities>"#;

    #[test]
    fn reads_capabilities() {
        let caps = parse_capabilities(CAPS).unwrap();
        assert_eq!(caps.make_and_model, "Brother MFC-J5340DW");
        assert_eq!((caps.max_width, caps.max_height, caps.has_feeder), (2550, 3508, true));
        assert_eq!(caps.resolutions, [100, 300, 600]);
        assert_eq!(caps.format(), Some("image/jpeg"));
        let c = caps.capabilities();
        assert_eq!(c.color_modes, [ColorMode::Color, ColorMode::Grayscale, ColorMode::LineArt]);
        assert!((c.bed_mm[0] - 215.9).abs() < 0.1 && (c.bed_mm[1] - 297.0).abs() < 0.1);
        assert!(parse_capabilities("<nope/>").is_err());
        let ranged = CAPS.replace(
            "<scan:DiscreteResolutions>",
            "<scan:ResolutionRange><scan:XResolutionRange><scan:Min>100</scan:Min><scan:Max>400</scan:Max></scan:XResolutionRange></scan:ResolutionRange><scan:DiscreteResolutions>",
        );
        assert_eq!(parse_capabilities(&ranged).unwrap().resolutions, [100, 150, 200, 300, 400, 600]);
    }

    #[test]
    fn writes_scan_settings() {
        let caps = parse_capabilities(CAPS).unwrap();
        let settings = ScanSettings {
            dpi: 300,
            color_mode: ColorMode::Grayscale,
            area: ScanArea { x: 0.5, y: 0.0, width: 0.5, height: 0.25 },
            ..ScanSettings::default()
        };
        let xml = scan_settings_xml(&caps, &settings, "image/jpeg");
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let get = |n: &str| text_of(doc.root_element(), n).unwrap();
        assert_eq!((get("Version"), get("ColorMode"), get("XResolution")), ("2.63".into(), "Grayscale8".into(), "300".into()));
        assert_eq!((get("XOffset"), get("Width"), get("Height")), ("1275".into(), "1275".into(), "877".into()));
        assert_eq!(get("DocumentFormat"), "image/jpeg");
    }

    #[test]
    fn addresses_typed_in() {
        assert_eq!(add_address("192.168.1.30").unwrap(), "http://192.168.1.30/eSCL");
        assert_eq!(add_address(" scanner.local:8080/ ").unwrap(), "http://scanner.local:8080/eSCL");
        assert_eq!(add_address("http://10.0.0.5/escl").unwrap(), "http://10.0.0.5/escl");
        assert!(add_address("https://x").is_err() && add_address("").is_err());
        assert!(known().iter().any(|t| t.url == "http://192.168.1.30/eSCL"));
    }

    #[test]
    fn resolution_from_what_arrives() {
        // A Letter-wide region (2550/300 in) at 100 dpi asked, 1680 px sent: ~198 dpi.
        assert_eq!(actual_dpi(1680, 2550, 100), 198);
        assert_eq!(actual_dpi(850, 2550, 100), 100);
        assert_eq!(actual_dpi(2540, 2550, 300), 300, "close enough");
    }
}
