//! Krita brush presets (`.kpp`) and resource bundles (`.bundle`).
//!
//! A `.kpp` is a PNG (the preset's preview) whose `preset` text chunk holds
//! the settings as XML: `<Preset paintopid="paintbrush" name="…">` with one
//! `<param name="…">` per setting. The tip is the `brush_definition` param:
//! an `auto_brush` (a generated round or square mask: diameter, ratio, fade,
//! spikes) or a predefined brush (`gbr_brush`, `png_brush`…) naming a tip
//! file, which travels next to the preset in a bundle. Pen dynamics are curve
//! options (`PressureSize`, `SizeSensor`, `SizeUseSameCurve`,
//! `SizecommonCurve`…), curves written as `x,y;x,y;`.
//!
//! A `.bundle` is a ZIP: stored `mimetype`, `META-INF/manifest.xml` listing
//! each resource with its type and MD5, `meta.xml`, `preview.png`, and the
//! resources in `paintoppresets/`, `brushes/`, `patterns/`.
//!
//! Krita has a dozen brush engines; Painter maps the pixel engine
//! ("paintbrush") and the shared parts of the others (tip, size, opacity,
//! flow, spacing, dynamics, scatter, texture), and says which were
//! approximated. The preset file and its tip are kept, so an unedited preset
//! exports back unchanged and an edited one keeps the parameters Painter
//! doesn't have.

use std::collections::BTreeMap;
use std::io::Read;

use base64::Engine;
use painter_core::zipfile::{ZipReader, ZipWriter};
use painter_core::{BrushMode, BrushSettings, PressureCurve};

use crate::types::{BrushIoError, BrushPreset, BrushTexture, BrushTip, Dynamic, ImportReport, PresetSource};

pub const PRESET_EXTENSION: &str = "kpp";
pub const BUNDLE_EXTENSION: &str = "bundle";
const BUNDLE_MIMETYPE: &str = "application/x-krita-resourcebundle";

fn corrupt(msg: impl Into<String>) -> BrushIoError {
    BrushIoError::Corrupt(msg.into())
}

// ---- MD5 (RFC 1321), for bundle manifests and resource references ----------------------

pub fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16,
        23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32).collect();
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend(((data.len() as u64).wrapping_mul(8)).to_le_bytes());
    let mut h = [0x6745_2301u32, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    for chunk in msg.as_chunks::<64>().0 {
        let m: Vec<u32> = chunk.as_chunks::<4>().0.iter().map(|w| u32::from_le_bytes(*w)).collect();
        let [mut a, mut b, mut c, mut d] = h;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 16];
    for (i, w) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    out
}

fn md5_hex(data: &[u8]) -> String {
    md5(data).iter().map(|b| format!("{b:02x}")).collect()
}

// ---- PNG text chunks --------------------------------------------------------------------

/// The text chunks (tEXt, zTXt, iTXt) of a PNG, by keyword.
fn png_texts(png: &[u8]) -> Result<BTreeMap<String, String>, BrushIoError> {
    if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(corrupt("not a PNG"));
    }
    let inflate = |data: &[u8]| -> Option<Vec<u8>> {
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(data).read_to_end(&mut out).ok()?;
        Some(out)
    };
    let mut out = BTreeMap::new();
    let mut at = 8;
    while at + 8 <= png.len() {
        let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
        let kind = &png[at + 4..at + 8];
        let Some(body) = png.get(at + 8..at + 8 + len) else { break };
        at += 12 + len;
        let Some(nul) = body.iter().position(|b| *b == 0) else { continue };
        let key = String::from_utf8_lossy(&body[..nul]).into_owned();
        let rest = &body[nul + 1..];
        let text = match kind {
            b"tEXt" => Some(rest.iter().map(|&b| b as char).collect()),
            b"zTXt" => rest.get(1..).and_then(inflate).map(|t| t.iter().map(|&b| b as char).collect()),
            b"iTXt" => {
                let (compressed, rest) = (rest.first().copied().unwrap_or(0), rest.get(2..).unwrap_or_default());
                // Language tag and translated keyword, each NUL-terminated.
                let mut parts = rest.splitn(3, |b| *b == 0);
                let (_, _, text) = (parts.next(), parts.next(), parts.next().unwrap_or_default());
                let text = if compressed == 1 { inflate(text) } else { Some(text.to_vec()) };
                text.map(|t| String::from_utf8_lossy(&t).into_owned())
            }
            _ => None,
        };
        if let Some(text) = text {
            out.insert(key, text);
        }
    }
    Ok(out)
}

/// `png` with Latin-1 tEXt chunks added before IEND.
fn with_texts(png: &[u8], texts: &[(&str, &str)]) -> Vec<u8> {
    let iend = png.len() - 12;
    let mut out = png[..iend].to_vec();
    for (key, text) in texts {
        let mut body = key.as_bytes().to_vec();
        body.push(0);
        body.extend(text.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }));
        out.extend((body.len() as u32).to_be_bytes());
        let mut chunk = b"tEXt".to_vec();
        chunk.extend(&body);
        out.extend(&chunk);
        out.extend(crc32fast::hash(&chunk).to_be_bytes());
    }
    out.extend(&png[iend..]);
    out
}

// ---- tips and resources -----------------------------------------------------------------

/// A tip file from a bundle or a preset's embedded resources, as coverage.
fn decode_tip(name: &str, data: &[u8]) -> Option<BrushTip> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".gbr") {
        return crate::gimp::read_gbr(data).ok()?.presets.pop()?.tip;
    }
    if lower.ends_with(".gih") {
        return crate::gimp::read_gih(data).ok()?.presets.pop()?.tip;
    }
    // PNG and other images: Krita inks the dark parts of grey images (within
    // their alpha) and the alpha of colour ones.
    let img = image::load_from_memory(data).ok()?;
    let grey = !img.color().has_color();
    let rgba = img.to_rgba8();
    let alpha = rgba
        .pixels()
        .map(|p| {
            let [r, g, b, a] = p.0;
            if grey {
                let l = (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000;
                ((255 - l) * a as u32 / 255) as u8
            } else {
                a
            }
        })
        .collect();
    BrushTip::new(rgba.width(), rgba.height(), alpha).ok()
}

/// A pattern image as luminance (light lets ink through).
fn decode_pattern(data: &[u8]) -> Option<BrushTip> {
    let img = image::load_from_memory(data).ok()?.to_luma_alpha8();
    let lum = img.pixels().map(|p| ((p.0[0] as u32 * p.0[1] as u32 + 255 * (255 - p.0[1] as u32)) / 255) as u8).collect();
    BrushTip::new(img.width(), img.height(), lum).ok()
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

// ---- preset XML -------------------------------------------------------------------------

/// A Krita curve, `x,y;x,y;`.
fn parse_curve(s: &str) -> Option<PressureCurve> {
    let pts: Vec<[f32; 2]> = s
        .split(';')
        .filter_map(|p| {
            let (x, y) = p.trim().split_once(',')?;
            Some([x.trim().parse().ok()?, y.trim().parse().ok()?])
        })
        .collect();
    (pts.len() >= 2).then(|| PressureCurve::from_points(pts))
}

fn curve_string(c: &PressureCurve) -> String {
    c.points().iter().map(|[x, y]| format!("{x},{y};")).collect()
}

fn xml(s: &str) -> Option<roxmltree::Document<'_>> {
    roxmltree::Document::parse_with_options(s, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() }).ok()
}

/// One curve option ("Size", "Opacity", "Flow", "Rotation"…) as a dynamic.
fn curve_option(p: &BTreeMap<String, String>, id: &str) -> Option<Dynamic> {
    if p.get(&format!("Pressure{id}")).map(String::as_str) != Some("true") {
        return None;
    }
    let same = p.get(&format!("{id}UseSameCurve")).is_none_or(|v| v == "true");
    let use_curve = p.get(&format!("{id}UseCurve")).is_none_or(|v| v == "true");
    let common = p.get(&format!("{id}commonCurve")).and_then(|c| parse_curve(c));
    let mut d = Dynamic::default();
    let sensors = p.get(&format!("{id}Sensor")).map(String::as_str).unwrap_or("");
    let doc = xml(sensors);
    let mut list: Vec<(String, Option<PressureCurve>)> = Vec::new();
    if let Some(doc) = &doc {
        let root = doc.root_element();
        let nodes: Vec<roxmltree::Node> = if root.attribute("id") == Some("sensorslist") {
            root.children().filter(|c| c.has_tag_name("ChildSensor")).collect()
        } else {
            vec![root]
        };
        for n in nodes {
            let curve = n.children().find(|c| c.has_tag_name("curve")).and_then(|c| c.text()).and_then(parse_curve);
            list.push((n.attribute("id").unwrap_or("").to_string(), curve));
        }
    }
    if list.is_empty() {
        list.push(("pressure".into(), None));
    }
    for (sensor, curve) in list {
        let curve = if !use_curve {
            PressureCurve::linear()
        } else if same {
            common.clone().or(curve).unwrap_or_else(PressureCurve::linear)
        } else {
            curve.or(common.clone()).unwrap_or_else(PressureCurve::linear)
        };
        match sensor.as_str() {
            "pressure" | "pressurein" => d.pressure = Some(curve),
            "speed" => d.velocity = Some(curve),
            "tiltelevation" | "xtilt" | "ytilt" => d.tilt = Some(curve),
            "fuzzy" | "fuzzystroke" => d.random = Some(1.0 - curve.points().iter().map(|p| p[1]).fold(1.0, f32::min)),
            _ => {}
        }
    }
    // The option's own minimum (Krita 5's strength range).
    if let Some(min) = p.get(&format!("{id}MinValue")).and_then(|v| v.parse::<f32>().ok()) {
        d.minimum = Some(min.clamp(0.0, 1.0));
    }
    Some(d)
}

fn sensor_list(p: &BTreeMap<String, String>, id: &str) -> Vec<String> {
    let s = p.get(&format!("{id}Sensor")).map(String::as_str).unwrap_or("");
    xml(s)
        .map(|doc| {
            let root = doc.root_element();
            if root.attribute("id") == Some("sensorslist") {
                root.children().filter_map(|c| c.attribute("id").map(str::to_string)).collect()
            } else {
                root.attribute("id").map(str::to_string).into_iter().collect()
            }
        })
        .unwrap_or_default()
}

fn num(p: &BTreeMap<String, String>, key: &str) -> Option<f32> {
    p.get(key)?.trim().parse().ok()
}

fn flag(p: &BTreeMap<String, String>, key: &str) -> bool {
    p.get(key).is_some_and(|v| v == "true" || v == "1")
}

/// The params of a preset's XML, in order.
/// A preset's engine, name and params.
type ParsedPreset = (String, String, Vec<(String, String)>);

fn parse_preset(xml_text: &str) -> Result<ParsedPreset, BrushIoError> {
    let doc = xml(xml_text).ok_or_else(|| corrupt("unreadable preset XML"))?;
    let root = doc.root_element();
    if !root.has_tag_name("Preset") {
        return Err(corrupt("no <Preset>"));
    }
    let paintop = root.attribute("paintopid").unwrap_or("paintbrush").to_string();
    let name = root.attribute("name").unwrap_or("Krita brush").to_string();
    let params = root
        .children()
        .filter(|c| c.has_tag_name("param"))
        .map(|c| (c.attribute("name").unwrap_or("").to_string(), c.text().unwrap_or("").to_string()))
        .collect();
    Ok((paintop, name, params))
}

/// Reads one `.kpp`. `resource` finds a tip or pattern file by name (in the bundle).
pub fn read_kpp(bytes: &[u8], resource: &dyn Fn(&str) -> Option<Vec<u8>>) -> Result<ImportReport, BrushIoError> {
    let texts = png_texts(bytes)?;
    let preset_xml = texts.get("preset").ok_or_else(|| corrupt("no Krita preset in this PNG"))?;
    let (paintop, name, params) = parse_preset(preset_xml)?;
    let p: BTreeMap<String, String> = params.iter().cloned().collect();
    let mut warnings = Vec::new();

    // Resources embedded in the preset (Krita 5): "resource_N" chunks, base64 in XML.
    let mut embedded: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for (key, text) in &texts {
        if !key.starts_with("resource_") {
            continue;
        }
        if let Some(doc) = xml(text) {
            for n in doc.descendants().filter(|n| n.is_element()) {
                let (Some(file), Some(data)) = (n.attribute("filename").or(n.attribute("name")), n.text()) else { continue };
                let clean: String = data.chars().filter(|c| !c.is_whitespace()).collect();
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(clean) {
                    embedded.insert(file_name(file).to_string(), bytes);
                }
            }
        }
    }
    let find = |file: &str| embedded.get(file_name(file)).cloned().or_else(|| resource(file_name(file)));

    let mut settings = BrushSettings::g_pen();
    settings.name = name.clone();
    settings.opacity = num(&p, "OpacityValue").unwrap_or(1.0).clamp(0.0, 1.0);
    settings.density = num(&p, "FlowValue").unwrap_or(1.0).clamp(0.01, 1.0);
    settings.pressure_size = false;
    settings.pressure_density = false;
    settings.stabilization = 0;
    if flag(&p, "EraserMode") || p.get("CompositeOp").is_some_and(|c| c == "erase") {
        settings.mode = BrushMode::Erase;
    }
    let mut preset = BrushPreset::new(settings);
    preset.source = PresetSource::Krita { paintop: paintop.clone() };

    // The tip.
    let mut tip_file = None;
    if let Some(def) = p.get("brush_definition").and_then(|d| xml(d).map(|_| d.clone())) {
        let doc = xml(&def).expect("parsed above");
        let brush = doc.descendants().find(|n| n.has_tag_name("Brush")).ok_or_else(|| corrupt("no <Brush>"))?;
        let attr = |n: roxmltree::Node, k: &str| n.attribute(k).and_then(|v| v.parse::<f32>().ok());
        preset.settings.spacing = attr(brush, "spacing").unwrap_or(0.1).clamp(0.01, 10.0);
        if brush.attribute("useAutoSpacing") == Some("1") {
            // Krita's auto spacing grows with the square root of the size.
            let coeff = attr(brush, "autoSpacingCoeff").unwrap_or(1.0);
            let size = attr(brush, "diameter").unwrap_or(40.0).max(1.0);
            preset.settings.spacing = (coeff * size.sqrt() / size).clamp(0.01, 10.0);
        }
        preset.angle = attr(brush, "angle").unwrap_or(0.0).to_degrees();
        match brush.attribute("type").unwrap_or("auto_brush") {
            "auto_brush" => {
                let mask = brush.children().find(|c| c.has_tag_name("MaskGenerator"));
                if let Some(m) = mask {
                    let diameter = attr(m, "diameter").or(attr(m, "radius").map(|r| r * 2.0)).unwrap_or(40.0);
                    preset.settings.size = diameter.max(0.5);
                    preset.thickness = attr(m, "ratio").unwrap_or(1.0).clamp(0.01, 1.0);
                    let fade = attr(m, "hfade").unwrap_or(1.0).clamp(0.0, 1.0);
                    preset.settings.hardness = match m.attribute("id") {
                        Some("soft") | Some("gaussian") => fade * 0.5,
                        _ => fade,
                    };
                    if m.attribute("type") == Some("rect") {
                        warnings.push(format!("“{name}”: a square tip is drawn round"));
                    }
                    if attr(m, "spikes").unwrap_or(2.0) > 2.0 {
                        warnings.push(format!("“{name}”: spiked tips are drawn round"));
                    }
                }
                if attr(brush, "randomness").unwrap_or(0.0) > 0.0 || attr(brush, "density").unwrap_or(1.0) < 1.0 {
                    warnings.push(format!("“{name}”: the tip's randomness and density aren't supported"));
                }
            }
            kind => {
                let file = brush.attribute("filename").unwrap_or("");
                match find(file).and_then(|data| decode_tip(file, &data).map(|t| (t, data))) {
                    Some((tip, data)) => {
                        let scale = attr(brush, "scale").unwrap_or(1.0);
                        preset.settings.size = (tip.width.max(tip.height) as f32 * scale).max(0.5);
                        preset.tip = Some(tip);
                        tip_file = Some((file_name(file).to_string(), data));
                    }
                    None => {
                        warnings.push(format!("“{name}”: tip {file} ({kind}) not found; using a round one"));
                        preset.settings.size = 40.0 * attr(brush, "scale").unwrap_or(1.0);
                    }
                }
            }
        }
    } else {
        warnings.push(format!("“{name}”: no tip definition; using a round one"));
    }

    // Dynamics.
    if let Some(d) = curve_option(&p, "Size") {
        preset.dynamics.size = d;
    }
    if let Some(d) = curve_option(&p, "Opacity") {
        preset.dynamics.opacity = d;
    }
    if let Some(d) = curve_option(&p, "Flow") {
        preset.dynamics.flow = d;
    }
    if let Some(d) = curve_option(&p, "Ratio") {
        preset.dynamics.thickness = d;
    }
    if let Some(mut d) = curve_option(&p, "Rotation") {
        if sensor_list(&p, "Rotation").iter().any(|s| s == "drawingangle") {
            preset.dynamics.rotation_follows_direction = true;
        }
        // Krita's rotation from pressure etc. spans a full turn, like ours.
        d.minimum = None;
        preset.dynamics.rotation = d;
    }
    if flag(&p, "PressureScatter") {
        let amount = num(&p, "ScatterValue").unwrap_or(1.0);
        preset.settings.scatter.amount = amount.max(0.0);
        preset.settings.scatter.count = 1;
        preset.settings.scatter.both_axes = flag(&p, "Scattering/AxisX") && flag(&p, "Scattering/AxisY");
    }
    if flag(&p, "Texture/Pattern/Enabled") {
        let image = p
            .get("Texture/Pattern/Pattern")
            .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok())
            .or_else(|| p.get("Texture/Pattern/PatternFileName").and_then(|f| find(f)))
            .and_then(|data| decode_pattern(&data));
        match image {
            Some(image) => {
                preset.texture = Some(BrushTexture {
                    image,
                    scale: num(&p, "Texture/Pattern/Scale").unwrap_or(1.0) * 100.0,
                    density: num(&p, "Texture/StrengthValue").or(num(&p, "Texture/Strength/Value")).unwrap_or(1.0).clamp(0.0, 1.0),
                    invert: flag(&p, "Texture/Pattern/Invert"),
                })
            }
            None => warnings.push(format!("“{name}”: its texture couldn't be read")),
        }
    }
    match paintop.as_str() {
        "paintbrush" => {}
        "colorsmudge" => warnings.push(format!("“{name}”: colour smudging isn't supported; the brush paints its colour")),
        other => warnings.push(format!("“{name}”: Krita's {other} engine is approximated with Painter's brush")),
    }
    preset.apply_dynamics();
    preset.apply_tip();
    preset.extras.insert("krita.paintop".into(), paintop.into());
    if let Some((file, data)) = tip_file {
        preset.extras.insert("krita.tip_name".into(), file.into());
        preset.extras.insert("krita_tip.original".into(), base64::engine::general_purpose::STANDARD.encode(data).into());
    }
    preset.keep_original("krita", bytes);
    Ok(ImportReport { presets: vec![preset], warnings, from_cache: false })
}

/// Reads every preset of a bundle, with their tips and patterns.
pub fn read_bundle(bytes: Vec<u8>) -> Result<ImportReport, BrushIoError> {
    let zip = ZipReader::new(bytes).map_err(corrupt)?;
    let names: Vec<String> = zip.names().map(str::to_string).collect();
    let resource = |file: &str| -> Option<Vec<u8>> {
        let wanted = file.to_ascii_lowercase();
        let path = names
            .iter()
            .find(|n| (n.starts_with("brushes/") || n.starts_with("patterns/")) && file_name(n).to_ascii_lowercase() == wanted)?;
        zip.read(path)
    };
    let mut report = ImportReport::default();
    for name in names.iter().filter(|n| n.starts_with("paintoppresets/") && n.to_ascii_lowercase().ends_with(".kpp")) {
        let Some(data) = zip.read(name) else { continue };
        match read_kpp(&data, &resource) {
            Ok(r) => {
                report.presets.extend(r.presets);
                report.warnings.extend(r.warnings);
            }
            Err(e) => report.warnings.push(format!("{}: {e}", file_name(name))),
        }
    }
    if report.presets.is_empty() {
        return Err(BrushIoError::Unsupported("a bundle without brush presets".into()));
    }
    Ok(report)
}

// ---- writing ----------------------------------------------------------------------------

/// A file name safe in a bundle and in Latin-1 text chunks.
fn safe_name(name: &str) -> String {
    let s: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.') { c } else { '_' }).collect();
    let s = s.trim().to_string();
    if s.is_empty() {
        "brush".into()
    } else {
        s
    }
}

fn xml_attr(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '"' => "&quot;".into(),
            c if c.is_ascii() => c.to_string(),
            c => format!("&#{};", c as u32),
        })
        .collect()
}

fn sensor_xml(sensors: &[(&str, &PressureCurve)]) -> String {
    if let [(id, curve)] = sensors {
        return format!("<!DOCTYPE params>\n<params id=\"{id}\">\n <curve>{}</curve>\n</params>\n", curve_string(curve));
    }
    let mut s = "<!DOCTYPE params>\n<params id=\"sensorslist\">\n".to_string();
    for (id, curve) in sensors {
        s += &format!(" <ChildSensor id=\"{id}\">\n  <curve>{}</curve>\n </ChildSensor>\n", curve_string(curve));
    }
    s + "</params>\n"
}

/// Writes one curve option: on with the given sensors, or off.
fn set_option(p: &mut Vec<(String, String)>, id: &str, sensors: &[(&str, &PressureCurve)]) {
    let mut put = |k: String, v: String| match p.iter_mut().find(|(n, _)| *n == k) {
        Some(e) => e.1 = v,
        None => p.push((k, v)),
    };
    put(format!("Pressure{id}"), (!sensors.is_empty()).to_string());
    if let Some((_, first)) = sensors.first() {
        put(format!("{id}Sensor"), sensor_xml(sensors));
        put(format!("{id}UseCurve"), "true".into());
        put(format!("{id}UseSameCurve"), (sensors.len() == 1).to_string());
        put(format!("{id}commonCurve"), curve_string(first));
        put(format!("{id}curveMode"), "0".into());
    }
}

fn put(p: &mut Vec<(String, String)>, k: &str, v: impl Into<String>) {
    let v = v.into();
    match p.iter_mut().find(|(n, _)| n == k) {
        Some(e) => e.1 = v,
        None => p.push((k.to_string(), v)),
    }
}

/// A square preview of the tip, black on white, for the `.kpp` image.
fn preview_png(preset: &BrushPreset) -> Vec<u8> {
    const N: u32 = 200;
    let mut img = image::RgbaImage::from_pixel(N, N, image::Rgba([255, 255, 255, 255]));
    let cover = |x: f32, y: f32| -> f32 {
        match &preset.tip {
            Some(t) => {
                let s = t.width.max(t.height) as f32 / 160.0;
                let (tx, ty) = ((x - 100.0) * s + t.width as f32 / 2.0, (y - 100.0) * s + t.height as f32 / 2.0);
                if tx < 0.0 || ty < 0.0 || tx >= t.width as f32 || ty >= t.height as f32 {
                    0.0
                } else {
                    t.alpha[ty as usize * t.width as usize + tx as usize] as f32 / 255.0
                }
            }
            None => {
                let d = ((x - 100.0).powi(2) + ((y - 100.0) / preset.thickness.max(0.1)).powi(2)).sqrt() / 80.0;
                let h = preset.settings.hardness.clamp(0.0, 0.999);
                ((1.0 - d) / (1.0 - h)).clamp(0.0, 1.0)
            }
        }
    };
    for (x, y, p) in img.enumerate_pixels_mut() {
        let v = (255.0 * (1.0 - cover(x as f32, y as f32))) as u8;
        *p = image::Rgba([v, v, v, 255]);
    }
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).expect("encoding to memory");
    out
}

/// The preset as a `.kpp`, and the tip file it refers to (name, bytes), if any.
/// An unedited Krita preset comes back unchanged; an edited one keeps the
/// parameters Painter doesn't have.
/// A tip file: its name and bytes.
pub type TipFile = (String, Vec<u8>);

pub fn write_kpp(preset: &BrushPreset) -> Result<(Vec<u8>, Option<TipFile>), BrushIoError> {
    let original_tip = || {
        let name = preset.extras.get("krita.tip_name")?.as_str()?.to_string();
        Some((name, preset.original("krita_tip")?))
    };
    if let Some(bytes) = preset.unchanged_original("krita") {
        return Ok((bytes, original_tip()));
    }
    let (paintop, mut params) = match preset.original("krita").and_then(|b| png_texts(&b).ok()).and_then(|t| t.get("preset").cloned())
    {
        Some(x) => {
            let (op, _, params) = parse_preset(&x)?;
            (op, params)
        }
        None => ("paintbrush".to_string(), Vec::new()),
    };
    let s = &preset.settings;
    let name = preset.name();

    // Tip: the edited preset's own bitmap as a .gbr (or the original tip file, unchanged).
    let mut tip_file = None;
    let tip_def = match &preset.tip {
        Some(tip) => {
            let (file, data) = match original_tip().filter(|(n, d)| decode_tip(n, d).as_ref() == Some(tip)) {
                Some(t) => t,
                None => (format!("{}.gbr", safe_name(name)), crate::gimp::write_gbr(preset)?),
            };
            let def = format!(
                "<Brush type=\"{}\" filename=\"{}\" md5sum=\"{}\" spacing=\"{}\" angle=\"{}\" scale=\"{}\" BrushVersion=\"2\" ColorAsMask=\"1\" useAutoSpacing=\"0\" autoSpacingCoeff=\"1\" brushApplication=\"0\"/>",
                if file.to_ascii_lowercase().ends_with(".gbr") { "gbr_brush" } else { "png_brush" },
                xml_attr(&file),
                md5_hex(&data),
                s.spacing,
                preset.angle.to_radians(),
                s.size / tip.width.max(tip.height) as f32,
            );
            tip_file = Some((file, data));
            def
        }
        None => format!(
            "<Brush type=\"auto_brush\" spacing=\"{}\" angle=\"{}\" randomness=\"0\" density=\"1\" BrushVersion=\"2\" useAutoSpacing=\"0\" autoSpacingCoeff=\"0.8\"><MaskGenerator diameter=\"{}\" ratio=\"{}\" hfade=\"{h}\" vfade=\"{h}\" spikes=\"2\" type=\"circle\" id=\"default\" antialiasEdges=\"1\"/></Brush>",
            s.spacing,
            preset.angle.to_radians(),
            s.size,
            preset.thickness,
            h = s.hardness,
        ),
    };
    put(&mut params, "brush_definition", tip_def);
    put(&mut params, "paintop", paintop.as_str());
    put(&mut params, "OpacityValue", s.opacity.to_string());
    put(&mut params, "FlowValue", s.density.to_string());
    put(&mut params, "EraserMode", (s.mode == BrushMode::Erase).to_string());

    let min_size = s.min_size_ratio;
    let size_curve =
        PressureCurve::from_points(s.size_curve.points().iter().map(|[x, y]| [*x, min_size + (1.0 - min_size) * y]).collect());
    let mut size: Vec<(&str, &PressureCurve)> = Vec::new();
    if s.pressure_size {
        size.push(("pressure", &size_curve));
    }
    let velocity = s.size_dynamics.velocity.clone();
    if let Some(v) = &velocity {
        size.push(("speed", v));
    }
    set_option(&mut params, "Size", &size);
    let flow: Vec<(&str, &PressureCurve)> = if s.pressure_density { vec![("pressure", &s.density_curve)] } else { vec![] };
    set_option(&mut params, "Flow", &flow);
    let opacity: Vec<(&str, &PressureCurve)> = s.opacity_dynamics.pressure.iter().map(|c| ("pressure", c)).collect();
    set_option(&mut params, "Opacity", &opacity);
    let linear = PressureCurve::linear();
    let rotation: Vec<(&str, &PressureCurve)> = if s.rotation.follow_direction { vec![("drawingangle", &linear)] } else { vec![] };
    set_option(&mut params, "Rotation", &rotation);
    if s.scatter.amount > 0.0 {
        set_option(&mut params, "Scatter", &[("pressure", &PressureCurve::from_points(vec![[0.0, 1.0], [1.0, 1.0]]))]);
        put(&mut params, "ScatterValue", s.scatter.amount.to_string());
        put(&mut params, "Scattering/AxisX", "true");
        put(&mut params, "Scattering/AxisY", s.scatter.both_axes.to_string());
    } else {
        set_option(&mut params, "Scatter", &[]);
    }
    if let Some(t) = &preset.texture {
        let mut png = Vec::new();
        if let Some(img) = image::GrayImage::from_raw(t.image.width, t.image.height, t.image.alpha.clone()) {
            img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|e| corrupt(e.to_string()))?;
        }
        put(&mut params, "Texture/Pattern/Enabled", "true");
        put(&mut params, "Texture/Pattern/Pattern", base64::engine::general_purpose::STANDARD.encode(&png));
        put(&mut params, "Texture/Pattern/Name", format!("{} texture", safe_name(name)));
        put(&mut params, "Texture/Pattern/Scale", (t.scale / 100.0).to_string());
        put(&mut params, "Texture/Pattern/Invert", t.invert.to_string());
        put(&mut params, "Texture/StrengthValue", t.density.to_string());
    } else {
        put(&mut params, "Texture/Pattern/Enabled", "false");
    }

    let mut xml_out = format!("<Preset paintopid=\"{}\" name=\"{}\">\n", xml_attr(&paintop), xml_attr(name));
    for (k, v) in &params {
        xml_out += &format!(" <param name=\"{}\" type=\"string\"><![CDATA[{}]]></param>\n", xml_attr(k), v.replace("]]>", "]] >"));
    }
    xml_out += "</Preset>\n";
    let png = with_texts(&preview_png(preset), &[("version", "2.2"), ("preset", &xml_out)]);
    Ok((png, tip_file))
}

/// Presets as a Krita bundle, with their tips.
pub fn write_bundle(presets: &[BrushPreset], title: &str) -> Result<Vec<u8>, BrushIoError> {
    let mut zip = ZipWriter::new();
    zip.add("mimetype", BUNDLE_MIMETYPE.as_bytes(), false);
    let mut entries = Vec::new();
    let mut used: Vec<String> = Vec::new();
    let mut unique = |base: String, ext: &str| {
        let mut name = format!("{base}.{ext}");
        let mut i = 2;
        while used.contains(&name.to_ascii_lowercase()) {
            name = format!("{base} {i}.{ext}");
            i += 1;
        }
        used.push(name.to_ascii_lowercase());
        name
    };
    let mut tips: Vec<String> = Vec::new();
    for preset in presets {
        let (kpp, tip) = write_kpp(preset)?;
        let path = format!("paintoppresets/{}", unique(safe_name(preset.name()), PRESET_EXTENSION));
        entries.push(("paintoppresets", path.clone(), md5_hex(&kpp)));
        zip.add(&path, &kpp, false);
        if let Some((file, data)) = tip {
            if !tips.contains(&file) {
                let path = format!("brushes/{file}");
                entries.push(("brushes", path.clone(), md5_hex(&data)));
                zip.add(&path, &data, true);
                tips.push(file);
            }
        }
    }
    let mut manifest = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<manifest:manifest xmlns:manifest=\"urn:oasis:names:tc:opendocument:xmlns:manifest:1.0\" manifest:version=\"1.2\">\n <manifest:file-entry manifest:media-type=\"application/x-krita-resourcebundle\" manifest:full-path=\"/\"/>\n",
    );
    for (kind, path, md5) in &entries {
        manifest += &format!(
            " <manifest:file-entry manifest:media-type=\"{kind}\" manifest:full-path=\"{}\" manifest:md5sum=\"{md5}\"/>\n",
            xml_attr(path)
        );
    }
    manifest += "</manifest:manifest>\n";
    zip.add("META-INF/manifest.xml", manifest.as_bytes(), true);
    let meta = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<meta:meta xmlns:meta=\"urn:oasis:names:tc:opendocument:xmlns:meta:1.0\" xmlns:dc=\"urn:oasis:names:tc:opendocument:xmlns:dc:1.0\">\n <meta:generator>Painter</meta:generator>\n <dc:title>{}</dc:title>\n <dc:description>Brushes exported from Painter</dc:description>\n <meta:bundle-version>1</meta:bundle-version>\n</meta:meta>\n",
        xml_attr(title)
    );
    zip.add("meta.xml", meta.as_bytes(), true);
    if let Some(first) = presets.first() {
        zip.add("preview.png", &preview_png(first), false);
    }
    Ok(zip.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_the_rfc() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"The quick brown fox jumps over the lazy dog"), "9e107d9d372bb6826bd81d3542a419d6");
        assert_eq!(md5_hex(&[b'a'; 1000]), "cabe45dcc9ae5b66ba86600cca6b8ba8");
    }

    /// A preset as Krita writes it: auto brush, pressure size with a curve, flow, eraser off.
    fn krita_kpp() -> Vec<u8> {
        let def = r#"<Brush type="auto_brush" spacing="0.05" angle="0.785398" randomness="0" density="1" BrushVersion="2" useAutoSpacing="0" autoSpacingCoeff="0.8"><MaskGenerator diameter="24" ratio="0.5" hfade="0.6" vfade="0.6" spikes="2" type="circle" id="default" antialiasEdges="1"/></Brush>"#;
        let sensor = "<!DOCTYPE params>\n<params id=\"pressure\">\n <curve>0,0;0.5,0.2;1,1;</curve>\n</params>\n";
        let xml = format!(
            "<Preset paintopid=\"paintbrush\" name=\"Pencil &#233;\">\n <param name=\"brush_definition\" type=\"string\"><![CDATA[{def}]]></param>\n <param name=\"OpacityValue\" type=\"string\"><![CDATA[0.8]]></param>\n <param name=\"FlowValue\" type=\"string\"><![CDATA[0.5]]></param>\n <param name=\"PressureSize\" type=\"string\"><![CDATA[true]]></param>\n <param name=\"SizeSensor\" type=\"string\"><![CDATA[{sensor}]]></param>\n <param name=\"SizeUseCurve\" type=\"string\"><![CDATA[true]]></param>\n <param name=\"SizeUseSameCurve\" type=\"string\"><![CDATA[false]]></param>\n <param name=\"KritaOnlyThing\" type=\"string\"><![CDATA[42]]></param>\n</Preset>\n"
        );
        with_texts(&preview_png(&BrushPreset::new(BrushSettings::g_pen())), &[("version", "2.2"), ("preset", &xml)])
    }

    #[test]
    fn reads_a_krita_preset() {
        let r = read_kpp(&krita_kpp(), &|_| None).unwrap();
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        let p = &r.presets[0];
        assert_eq!(p.name(), "Pencil é");
        let s = &p.settings;
        assert_eq!((s.size, s.opacity, s.density), (24.0, 0.8, 0.5));
        assert!((s.spacing - 0.05).abs() < 1e-6 && (s.hardness - 0.6).abs() < 1e-6);
        assert!((p.thickness - 0.5).abs() < 1e-6 && (p.angle - 45.0).abs() < 1e-3);
        assert!(s.pressure_size);
        assert!((s.size_curve.eval(0.5) - 0.2).abs() < 1e-5);
        assert_eq!(p.source, PresetSource::Krita { paintop: "paintbrush".into() });
    }

    #[test]
    fn round_trips_unchanged_and_edited_presets() {
        let original = krita_kpp();
        let p = read_kpp(&original, &|_| None).unwrap().presets.remove(0);
        assert_eq!(write_kpp(&p).unwrap().0, original, "unedited: byte for byte");
        let mut edited = p.clone();
        edited.settings.size = 50.0;
        edited.settings.pressure_density = true;
        let (kpp, tip) = write_kpp(&edited).unwrap();
        assert!(tip.is_none());
        let texts = png_texts(&kpp).unwrap();
        assert!(texts["preset"].contains("KritaOnlyThing"), "Krita's own params kept");
        let back = &read_kpp(&kpp, &|_| None).unwrap().presets[0];
        assert_eq!(back.settings.size, 50.0);
        assert!(back.settings.pressure_density && back.settings.pressure_size);
        assert!((back.settings.size_curve.eval(0.5) - 0.2).abs() < 1e-5);
        assert_eq!(back.name(), "Pencil é");
    }

    #[test]
    fn bundles_carry_bitmap_tips() {
        let mut s = BrushSettings::g_pen();
        s.name = "Chalk/1".into();
        s.size = 64.0;
        let mut p = BrushPreset::new(s);
        p.tip = Some(BrushTip::new(4, 2, vec![0, 50, 100, 150, 200, 250, 255, 0]).unwrap());
        p.texture = Some(BrushTexture {
            image: BrushTip::new(2, 2, vec![0, 255, 128, 64]).unwrap(),
            scale: 50.0,
            density: 0.7,
            invert: false,
        });
        p.apply_tip();
        let round = BrushPreset::new(BrushSettings::g_pen());
        let bundle = write_bundle(&[p.clone(), round], "Test").unwrap();
        let zip = ZipReader::new(bundle.clone()).unwrap();
        let names: Vec<&str> = zip.names().collect();
        assert_eq!(names[0], "mimetype");
        assert!(names.contains(&"brushes/Chalk_1.gbr") && names.contains(&"paintoppresets/G-Pen.kpp"), "{names:?}");
        let manifest = String::from_utf8(zip.read("META-INF/manifest.xml").unwrap()).unwrap();
        assert!(manifest.contains(&md5_hex(&zip.read("brushes/Chalk_1.gbr").unwrap())));
        let r = read_bundle(bundle).unwrap();
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(r.presets.len(), 2);
        let back = &r.presets[0];
        assert_eq!(back.tip, p.tip);
        assert_eq!(back.settings.size, 64.0);
        let t = back.texture.as_ref().unwrap();
        assert_eq!((t.image.alpha.as_slice(), t.scale, t.density), (&[0u8, 255, 128, 64][..], 50.0, 0.7));
        // Re-exporting the imported preset gives the same preset and tip files.
        let (kpp, tip) = write_kpp(back).unwrap();
        assert_eq!(kpp, zip.read("paintoppresets/Chalk_1.kpp").unwrap());
        assert_eq!(tip.unwrap().1, zip.read("brushes/Chalk_1.gbr").unwrap());
        // Still unchanged after the brush list's own format (and the import cache).
        let kept = crate::native::read(&crate::native::to_bytes(&r.presets).unwrap()).unwrap();
        assert_eq!(write_kpp(&kept[0]).unwrap().0, kpp);
    }
}
