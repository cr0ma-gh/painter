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
    // The spec says Latin-1, but Krita writes UTF-8: prefer it, fall back to Latin-1.
    let decode = |bytes: Vec<u8>| -> String {
        String::from_utf8(bytes)
            .unwrap_or_else(|e| e.into_bytes().into_iter().map(char::from).collect())
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
            b"tEXt" => Some(decode(rest.to_vec())),
            b"zTXt" => rest.get(1..).and_then(inflate).map(decode),
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

/// A file a preset links to: a tip (`brushes`) or a texture (`patterns`).
#[derive(Clone, Debug, PartialEq)]
pub struct Resource {
    /// Krita's resource type: "brushes" or "patterns".
    pub kind: String,
    pub file: String,
    pub data: Vec<u8>,
}

impl Resource {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({ "kind": self.kind, "file": self.file, "data": base64::engine::general_purpose::STANDARD.encode(&self.data) })
    }

    fn from_json(v: &serde_json::Value) -> Option<Self> {
        Some(Self {
            kind: v.get("kind")?.as_str()?.to_string(),
            file: v.get("file")?.as_str()?.to_string(),
            data: base64::engine::general_purpose::STANDARD.decode(v.get("data")?.as_str()?).ok()?,
        })
    }
}

/// A tip file as coverage, as Krita reads it: `.gbr` / `.gih` as GIMP does;
/// images used as a mask (the default) ink their dark parts within their
/// alpha (grey images without alpha are laid on white first, which is the
/// same); image stamps keep their colours, of which Painter keeps the alpha.
fn decode_tip(name: &str, data: &[u8], stamp: bool) -> Option<BrushTip> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".gbr") {
        return crate::gimp::read_gbr(data).ok()?.presets.pop()?.tip;
    }
    if lower.ends_with(".gih") {
        return crate::gimp::read_gih(data).ok()?.presets.pop()?.tip;
    }
    let rgba = image::load_from_memory(data).ok()?.to_rgba8();
    let alpha = rgba
        .pixels()
        .map(|p| {
            let [r, g, b, a] = p.0;
            if stamp {
                a
            } else {
                let l = (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000;
                ((255 - l) * a as u32 / 255) as u8
            }
        })
        .collect();
    BrushTip::new(rgba.width(), rgba.height(), alpha).ok()
}

/// A pattern (PNG or GIMP `.pat`) as luminance: light lets ink through,
/// transparent parts count as white.
fn decode_pattern(name: &str, data: &[u8]) -> Option<BrushTip> {
    let (w, h, rgba) = if name.to_ascii_lowercase().ends_with(".pat") {
        crate::gimp::read_pat(data)?
    } else {
        let img = image::load_from_memory(data).ok()?.to_rgba8();
        (img.width(), img.height(), img.into_raw())
    };
    let lum = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|[r, g, b, a]| {
            let l = (*r as u32 * 299 + *g as u32 * 587 + *b as u32 * 114) / 1000;
            ((l * *a as u32 + 255 * (255 - *a as u32)) / 255) as u8
        })
        .collect();
    BrushTip::new(w, h, lum).ok()
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

/// A curve that always gives full value: the option has no effect.
fn is_flat_full(c: &PressureCurve) -> bool {
    c.points().iter().all(|p| p[1] >= 0.999)
}

fn xml(s: &str) -> Option<roxmltree::Document<'_>> {
    roxmltree::Document::parse_with_options(s, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() }).ok()
}

fn num(p: &BTreeMap<String, String>, key: &str) -> Option<f32> {
    p.get(key)?.trim().parse().ok()
}

/// A boolean param ("true" / "false", or 1 / 0).
fn boolean(p: &BTreeMap<String, String>, key: &str, default: bool) -> bool {
    p.get(key).map_or(default, |v| matches!(v.trim(), "true" | "1"))
}

fn flag(p: &BTreeMap<String, String>, key: &str) -> bool {
    boolean(p, key, false)
}

/// The sensors of a curve option's `<id>Sensor` XML: one `<params id="…">`
/// or a `sensorslist` of `<ChildSensor id="…">`, each with its own curve.
fn sensors(p: &BTreeMap<String, String>, id: &str) -> Vec<(String, Option<PressureCurve>)> {
    let text = p.get(&format!("{id}Sensor")).map(String::as_str).unwrap_or("");
    let Some(doc) = xml(text) else { return Vec::new() };
    let root = doc.root_element();
    let nodes: Vec<roxmltree::Node> = if root.attribute("id") == Some("sensorslist") {
        root.children().filter(|c| c.has_tag_name("ChildSensor")).collect()
    } else {
        vec![root]
    };
    nodes
        .into_iter()
        .map(|n| {
            let curve = n.children().find(|c| c.has_tag_name("curve")).and_then(|c| c.text()).and_then(parse_curve);
            (n.attribute("id").unwrap_or("").to_string(), curve)
        })
        .collect()
}

/// One curve option ("Size", "Opacity", "Flow", "Ratio", "Rotation") as a
/// dynamic, read the way Krita 5 does. A checkable option is on only with
/// `Pressure<id>`; Opacity and Flow can't be switched off. With no sensor
/// listed, pressure is the sensor. With `<id>UseSameCurve` (the default)
/// every sensor uses `<id>commonCurve`; presets older than per-sensor curves
/// use `Custom<id>` / `Curve<id>`. A curve that is always full does nothing.
fn curve_option(p: &BTreeMap<String, String>, id: &str, checkable: bool) -> Option<Dynamic> {
    if checkable && !flag(p, &format!("Pressure{id}")) {
        return None;
    }
    let mut list = sensors(p, id);
    if list.is_empty() {
        list.push(("pressure".into(), None));
    }
    let defined = p.get(&format!("{id}Sensor")).is_some_and(|s| s.contains("curve"));
    let mut common = list.iter().rev().find_map(|(_, c)| c.clone());
    if !defined {
        common = if flag(p, &format!("Custom{id}")) { p.get(&format!("Curve{id}")).and_then(|c| parse_curve(c)) } else { None };
        list.iter_mut().for_each(|s| s.1 = common.clone());
    }
    let same = boolean(p, &format!("{id}UseSameCurve"), true);
    if same {
        if let Some(c) = p.get(&format!("{id}commonCurve")).and_then(|c| parse_curve(c)) {
            common = Some(c);
        }
    }
    let use_curve = boolean(p, &format!("{id}UseCurve"), true);
    let mut d = Dynamic::default();
    for (sensor, own) in list {
        let curve = if !use_curve {
            PressureCurve::linear()
        } else if same {
            common.clone().unwrap_or_else(PressureCurve::linear)
        } else {
            own.or_else(|| common.clone()).unwrap_or_else(PressureCurve::linear)
        };
        if is_flat_full(&curve) {
            continue;
        }
        match sensor.as_str() {
            "pressure" | "pressurein" => d.pressure = Some(curve),
            "speed" => d.velocity = Some(curve),
            "tiltelevation" | "xtilt" | "ytilt" => d.tilt = Some(curve),
            "fuzzy" | "fuzzystroke" => d.random = Some(1.0 - curve.points().iter().map(|p| p[1]).fold(1.0, f32::min)),
            _ => {}
        }
    }
    Some(d)
}

/// A preset's engine, name, params (in order) and embedded resources.
struct ParsedPreset {
    paintop: String,
    name: String,
    params: Vec<(String, String)>,
    resources: Vec<Resource>,
}

fn parse_preset(xml_text: &str) -> Result<ParsedPreset, BrushIoError> {
    // Krita fixes presets saved with a CDATA section nested in curves.
    let xml_text = xml_text.replace("<curve><![CDATA[", "<curve>").replace("]]></curve>", "</curve>");
    let doc = xml(&xml_text).ok_or_else(|| corrupt("unreadable preset XML"))?;
    let root = doc.root_element();
    if !root.has_tag_name("Preset") {
        return Err(corrupt("no <Preset>"));
    }
    let params = root
        .children()
        .filter(|c| c.has_tag_name("param"))
        .map(|c| (c.attribute("name").unwrap_or("").to_string(), c.text().unwrap_or("").to_string()))
        .collect();
    // Krita 5 ("version" 5.0) embeds the tips and patterns it links to.
    let resources = root
        .children()
        .filter(|c| c.has_tag_name("resources"))
        .flat_map(|r| r.children().filter(|c| c.has_tag_name("resource")))
        .filter_map(|r| {
            let clean: String = r.text().unwrap_or("").chars().filter(|c| !c.is_whitespace()).collect();
            Some(Resource {
                kind: r.attribute("type").unwrap_or("").to_string(),
                file: file_name(r.attribute("filename").or(r.attribute("name"))?).to_string(),
                data: base64::engine::general_purpose::STANDARD.decode(clean).ok()?,
            })
        })
        .collect();
    Ok(ParsedPreset {
        paintop: root.attribute("paintopid").unwrap_or("paintbrush").to_string(),
        name: root.attribute("name").unwrap_or("Krita brush").to_string(),
        params,
        resources,
    })
}

/// Reads one `.kpp`. `resource` finds a tip or pattern file by name (in a
/// bundle, or next to the preset) when the preset doesn't embed it.
pub fn read_kpp(bytes: &[u8], resource: &dyn Fn(&str) -> Option<Vec<u8>>) -> Result<ImportReport, BrushIoError> {
    let texts = png_texts(bytes)?;
    let preset_xml = texts.get("preset").ok_or_else(|| corrupt("no Krita preset in this PNG"))?;
    let ParsedPreset { paintop, name, params, resources } = parse_preset(preset_xml)?;
    let p: BTreeMap<String, String> = params.iter().cloned().collect();
    let mut warnings = Vec::new();
    // Files the preset linked to, kept so an unedited export gives them back.
    let mut linked: Vec<Resource> = Vec::new();
    let mut find = |kind: &str, file: &str| -> Option<Vec<u8>> {
        let file = file_name(file);
        if file.is_empty() {
            return None;
        }
        if let Some(r) = resources.iter().find(|r| r.file.eq_ignore_ascii_case(file)) {
            return Some(r.data.clone());
        }
        let data = resource(file)?;
        linked.push(Resource { kind: kind.into(), file: file.into(), data: data.clone() });
        Some(data)
    };

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
    if let Some(def) = p.get("brush_definition").filter(|d| xml(d).is_some()) {
        let doc = xml(def).expect("parsed above");
        let brush = doc.descendants().find(|n| n.has_tag_name("Brush")).ok_or_else(|| corrupt("no <Brush>"))?;
        let attr = |n: roxmltree::Node, k: &str| n.attribute(k).and_then(|v| v.trim().parse::<f32>().ok());
        let kind = brush.attribute("type").unwrap_or("auto_brush");
        // Krita's defaults differ: 1.0 for generated tips, 0.25 for the others.
        preset.settings.spacing = attr(brush, "spacing").unwrap_or(if kind == "auto_brush" { 1.0 } else { 0.25 }).clamp(0.01, 10.0);
        preset.angle = attr(brush, "angle").unwrap_or(0.0).to_degrees();
        match kind {
            "auto_brush" => {
                if let Some(m) = brush.children().find(|c| c.has_tag_name("MaskGenerator")) {
                    // Krita 2.2 wrote the diameter as "radius".
                    preset.settings.size = attr(m, "radius").or(attr(m, "diameter")).unwrap_or(1.0).max(0.5);
                    preset.thickness = attr(m, "ratio").unwrap_or(1.0).clamp(0.01, 1.0);
                    // Fade = the part of the radius that is solid: our hardness.
                    let fade = attr(m, "hfade").unwrap_or(1.0).clamp(0.0, 1.0);
                    preset.settings.hardness = match m.attribute("id") {
                        Some("soft") | Some("gaussian") => fade * 0.5,
                        _ => fade,
                    };
                    if m.attribute("type").is_some_and(|t| t != "circle") {
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
                // brushApplication: 0 alpha mask, 1 image stamp, 2 / 3 lightness / gradient map.
                let stamp = brush.attribute("brushApplication").map_or(brush.attribute("ColorAsMask") == Some("0"), |a| a == "1");
                let tip = find("brushes", file).and_then(|data| {
                    if kind == "abr_brush" {
                        // A tip inside a Photoshop brush file, by name.
                        let wanted = brush.attribute("name").unwrap_or("");
                        let mut tips = crate::abr::read(&data).ok()?;
                        let i = tips.iter().position(|t| t.name() == wanted).unwrap_or(0);
                        (i < tips.len()).then(|| tips.swap_remove(i).tip).flatten()
                    } else {
                        decode_tip(file, &data, stamp)
                    }
                });
                // Before Krita 2.4 (BrushVersion 1) the scale was halved.
                let mut scale = attr(brush, "scale").unwrap_or(1.0);
                if brush.attribute("BrushVersion").unwrap_or("1") == "1" {
                    scale *= 2.0;
                }
                match tip {
                    Some(tip) => {
                        preset.settings.size = (tip.width.max(tip.height) as f32 * scale).max(0.5);
                        preset.tip = Some(tip);
                    }
                    None => {
                        warnings.push(format!("“{name}”: tip {file} ({kind}) not found; using a round one"));
                        preset.settings.size = (40.0 * scale).max(0.5);
                    }
                }
                if stamp {
                    warnings.push(format!("“{name}”: a colour image tip paints with the brush colour"));
                }
            }
        }
        if brush.attribute("useAutoSpacing") == Some("1") {
            // Krita's auto spacing: coefficient × √size pixels.
            let coeff = attr(brush, "autoSpacingCoeff").unwrap_or(1.0);
            let size = preset.settings.size.max(1.0);
            preset.settings.spacing = (coeff * size.sqrt() / size).clamp(0.01, 10.0);
        }
    } else {
        warnings.push(format!("“{name}”: no tip definition; using a round one"));
    }

    // Dynamics.
    if let Some(d) = curve_option(&p, "Size", true) {
        preset.dynamics.size = d;
    }
    if let Some(d) = curve_option(&p, "Opacity", false) {
        preset.dynamics.opacity = d;
    }
    if let Some(d) = curve_option(&p, "Flow", false) {
        preset.dynamics.flow = d;
    }
    if let Some(d) = curve_option(&p, "Ratio", true) {
        preset.dynamics.thickness = d;
    }
    if let Some(mut d) = curve_option(&p, "Rotation", true) {
        if sensors(&p, "Rotation").iter().any(|(s, _)| s == "drawingangle") {
            preset.dynamics.rotation_follows_direction = true;
        }
        d.minimum = None;
        preset.dynamics.rotation = d;
    }
    if flag(&p, "PressureScatter") {
        // Amount in brush diameters, 0..=5 (5 when absent); both axes by default.
        let amount = num(&p, "ScatterValue").or(num(&p, "Scattering/Amount")).unwrap_or(5.0);
        preset.settings.scatter.amount = amount.max(0.0);
        preset.settings.scatter.count = 1;
        preset.settings.scatter.both_axes = boolean(&p, "Scattering/AxisX", true) && boolean(&p, "Scattering/AxisY", true);
    }
    if flag(&p, "Texture/Pattern/Enabled") {
        // Krita 5 links the pattern by file name / MD5; old presets embed it as base64.
        let file = p.get("Texture/Pattern/PatternFileName").map(|f| file_name(f).to_string()).unwrap_or_default();
        let image = find("patterns", &file).and_then(|data| decode_pattern(&file, &data)).or_else(|| {
            let data = base64::engine::general_purpose::STANDARD.decode(p.get("Texture/Pattern/Pattern")?.trim()).ok()?;
            decode_pattern("pattern.png", &data)
        });
        match image {
            Some(image) => {
                let strength = if flag(&p, "PressureTexture/Strength/") { num(&p, "Texture/Strength/Value") } else { None };
                preset.texture = Some(BrushTexture::new(
                    image,
                    num(&p, "Texture/Pattern/Scale").unwrap_or(1.0) * 100.0,
                    strength.unwrap_or(1.0).clamp(0.0, 1.0),
                    flag(&p, "Texture/Pattern/Invert"),
                ))
            }
            None => warnings.push(format!("“{name}”: its texture {file} couldn't be read")),
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
    if !linked.is_empty() {
        preset.extras.insert("krita.resources".into(), linked.iter().map(Resource::json).collect::<Vec<_>>().into());
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

fn put(p: &mut Vec<(String, String)>, k: &str, v: impl Into<String>) {
    let v = v.into();
    match p.iter_mut().find(|(n, _)| n == k) {
        Some(e) => e.1 = v,
        None => p.push((k.to_string(), v)),
    }
}

/// Writes one curve option: on with the given sensors, or off. Options that
/// can't be switched off (Opacity, Flow) get a curve that is always full
/// instead, or Krita would apply pressure.
fn set_option(p: &mut Vec<(String, String)>, id: &str, sensors: &[(&str, &PressureCurve)], checkable: bool) {
    let full = PressureCurve::from_points(vec![[0.0, 1.0], [1.0, 1.0]]);
    let off: [(&str, &PressureCurve); 1] = [("pressure", &full)];
    let sensors = if sensors.is_empty() && !checkable { &off[..] } else { sensors };
    put(p, &format!("Pressure{id}"), (!sensors.is_empty()).to_string());
    if let Some((_, first)) = sensors.first() {
        put(p, &format!("{id}Sensor"), sensor_xml(sensors));
        put(p, &format!("{id}UseCurve"), "true");
        put(p, &format!("{id}UseSameCurve"), (sensors.len() == 1).to_string());
        put(p, &format!("{id}commonCurve"), curve_string(first));
        put(p, &format!("{id}curveMode"), "0");
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

/// The preset as a `.kpp` (Krita 5 format, its tip and texture embedded so
/// the file works on its own), and those files for a bundle. An unedited
/// Krita preset comes back unchanged with the files it linked to; an edited
/// one keeps the parameters Painter doesn't have.
pub fn write_kpp(preset: &BrushPreset) -> Result<(Vec<u8>, Vec<Resource>), BrushIoError> {
    let linked: Vec<Resource> = preset
        .extras
        .get("krita.resources")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(Resource::from_json).collect())
        .unwrap_or_default();
    if let Some(bytes) = preset.unchanged_original("krita") {
        // The files it linked to, and the ones it embeds (for a bundle).
        let mut all = linked;
        let embedded = png_texts(&bytes).ok().and_then(|t| t.get("preset").cloned()).and_then(|x| parse_preset(&x).ok());
        for r in embedded.map(|p| p.resources).unwrap_or_default() {
            if !all.iter().any(|a| a.kind == r.kind && a.file == r.file) {
                all.push(r);
            }
        }
        return Ok((bytes, all));
    }
    let original = preset.original("krita").and_then(|b| png_texts(&b).ok()).and_then(|t| t.get("preset").cloned());
    let (paintop, mut params, embedded) = match original {
        Some(x) => {
            let parsed = parse_preset(&x)?;
            (parsed.paintop, parsed.params, parsed.resources)
        }
        None => ("paintbrush".to_string(), Vec::new(), Vec::new()),
    };
    let s = &preset.settings;
    let name = preset.name();
    let mut resources: Vec<Resource> = Vec::new();

    // Tip: the original file when it still gives this bitmap, else a .gbr of it.
    let tip_def = match &preset.tip {
        Some(tip) => {
            let original = linked.iter().chain(&embedded).find(|r| r.kind == "brushes" && decode_tip(&r.file, &r.data, false).as_ref() == Some(tip));
            let res = match original {
                Some(r) => r.clone(),
                None => Resource { kind: "brushes".into(), file: format!("{}.gbr", safe_name(name)), data: crate::gimp::write_gbr(preset)? },
            };
            let def = format!(
                "<Brush type=\"{}\" filename=\"{}\" md5sum=\"{}\" spacing=\"{}\" useAutoSpacing=\"0\" autoSpacingCoeff=\"1\" angle=\"{}\" scale=\"{}\" brushApplication=\"0\" ColorAsMask=\"1\" BrushVersion=\"2\"/>",
                if res.file.to_ascii_lowercase().ends_with(".gbr") { "gbr_brush" } else { "png_brush" },
                xml_attr(&res.file),
                md5_hex(&res.data),
                s.spacing,
                preset.angle.to_radians(),
                s.size / tip.width.max(tip.height) as f32,
            );
            resources.push(res);
            def
        }
        None => format!(
            "<Brush type=\"auto_brush\" BrushVersion=\"2\" spacing=\"{}\" useAutoSpacing=\"0\" autoSpacingCoeff=\"0.8\" angle=\"{}\" randomness=\"0\" density=\"1\"><MaskGenerator diameter=\"{}\" ratio=\"{}\" hfade=\"{h}\" vfade=\"{h}\" spikes=\"2\" type=\"circle\" antialiasEdges=\"1\" id=\"default\"/></Brush>",
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
    if let Some(v) = &s.size_dynamics.velocity {
        size.push(("speed", v));
    }
    set_option(&mut params, "Size", &size, true);
    let flow: Vec<(&str, &PressureCurve)> = if s.pressure_density { vec![("pressure", &s.density_curve)] } else { vec![] };
    set_option(&mut params, "Flow", &flow, false);
    let opacity: Vec<(&str, &PressureCurve)> = s.opacity_dynamics.pressure.iter().map(|c| ("pressure", c)).collect();
    set_option(&mut params, "Opacity", &opacity, false);
    let linear = PressureCurve::linear();
    let rotation: Vec<(&str, &PressureCurve)> = if s.rotation.follow_direction { vec![("drawingangle", &linear)] } else { vec![] };
    set_option(&mut params, "Rotation", &rotation, true);
    let full = PressureCurve::from_points(vec![[0.0, 1.0], [1.0, 1.0]]);
    if s.scatter.amount > 0.0 {
        set_option(&mut params, "Scatter", &[("pressure", &full)], true);
        put(&mut params, "ScatterValue", s.scatter.amount.to_string());
        put(&mut params, "Scattering/AxisX", "true");
        put(&mut params, "Scattering/AxisY", s.scatter.both_axes.to_string());
    } else {
        set_option(&mut params, "Scatter", &[], true);
    }
    // Texture: linked by file name and MD5 (Krita 5), embedded below, and as
    // base64 in the preset for older Krita.
    params.retain(|(k, _)| !k.starts_with("Texture"));
    if let Some(t) = &preset.texture {
        let mut png = Vec::new();
        if let Some(img) = image::GrayImage::from_raw(t.image.width, t.image.height, t.image.alpha.clone()) {
            img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|e| corrupt(e.to_string()))?;
        }
        let file = format!("{} texture.png", safe_name(name));
        let md5 = md5(&png);
        put(&mut params, "Texture/Pattern/Enabled", "true");
        put(&mut params, "Texture/Pattern/PatternFileName", file.as_str());
        put(&mut params, "Texture/Pattern/Name", format!("{} texture", safe_name(name)));
        put(&mut params, "Texture/Pattern/PatternMD5Sum", md5.iter().map(|b| format!("{b:02x}")).collect::<String>());
        put(&mut params, "Texture/Pattern/PatternMD5", base64::engine::general_purpose::STANDARD.encode(md5));
        put(&mut params, "Texture/Pattern/Pattern", base64::engine::general_purpose::STANDARD.encode(&png));
        put(&mut params, "Texture/Pattern/Scale", (t.scale / 100.0).to_string());
        put(&mut params, "Texture/Pattern/Invert", t.invert.to_string());
        set_option(&mut params, "Texture/Strength/", &[("pressure", &full)], true);
        put(&mut params, "Texture/Strength/Value", t.density.to_string());
        resources.push(Resource { kind: "patterns".into(), file, data: png });
    } else {
        put(&mut params, "Texture/Pattern/Enabled", "false");
    }

    let mut xml_out = format!(
        "<Preset paintopid=\"{}\" name=\"{}\" embedded_resources=\"{}\">\n",
        xml_attr(&paintop),
        xml_attr(name),
        resources.len()
    );
    if !resources.is_empty() {
        xml_out += " <resources>\n";
        for r in &resources {
            let stem = r.file.rsplit_once('.').map_or(r.file.as_str(), |(s, _)| s);
            xml_out += &format!(
                "  <resource type=\"{}\" name=\"{}\" filename=\"{}\" md5sum=\"{}\">{}</resource>\n",
                r.kind,
                xml_attr(stem),
                xml_attr(&r.file),
                md5_hex(&r.data),
                base64::engine::general_purpose::STANDARD.encode(&r.data)
            );
        }
        xml_out += " </resources>\n";
    }
    for (k, v) in &params {
        xml_out += &format!(" <param name=\"{}\" type=\"string\"><![CDATA[{}]]></param>\n", xml_attr(k), v.replace("]]>", "]] >"));
    }
    xml_out += "</Preset>\n";
    let png = with_texts(&preview_png(preset), &[("version", "5.0"), ("preset", &xml_out)]);
    Ok((png, resources))
}

/// Presets as a Krita bundle, with their tips and textures.
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
    let mut files: Vec<String> = Vec::new();
    for preset in presets {
        let (kpp, linked) = write_kpp(preset)?;
        let path = format!("paintoppresets/{}", unique(safe_name(preset.name()), PRESET_EXTENSION));
        entries.push(("paintoppresets".to_string(), path.clone(), md5_hex(&kpp)));
        zip.add(&path, &kpp, false);
        for r in linked {
            let path = format!("{}/{}", r.kind, r.file);
            if !files.contains(&path) {
                entries.push((r.kind.clone(), path.clone(), md5_hex(&r.data)));
                zip.add(&path, &r.data, true);
                files.push(path);
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
        let (kpp, files) = write_kpp(&edited).unwrap();
        assert!(files.is_empty());
        let texts = png_texts(&kpp).unwrap();
        assert!(texts["preset"].contains("KritaOnlyThing"), "Krita's own params kept");
        let back = &read_kpp(&kpp, &|_| None).unwrap().presets[0];
        assert_eq!(back.settings.size, 50.0);
        assert!(back.settings.pressure_density && back.settings.pressure_size);
        assert!((back.settings.size_curve.eval(0.5) - 0.2).abs() < 1e-5);
        assert_eq!(back.name(), "Pencil é");
        // Opacity and flow follow pressure in Krita unless their curve is flat.
        assert!(p.dynamics.opacity.pressure.is_some() && back.settings.opacity_dynamics.pressure.is_some());
    }

    #[test]
    fn bundles_carry_bitmap_tips() {
        let mut s = BrushSettings::g_pen();
        s.name = "Chalk/1".into();
        s.size = 64.0;
        let mut p = BrushPreset::new(s);
        p.tip = Some(BrushTip::new(4, 2, vec![0, 50, 100, 150, 200, 250, 255, 0]).unwrap());
        p.texture = Some(BrushTexture::new(BrushTip::new(2, 2, vec![0, 255, 128, 64]).unwrap(), 50.0, 0.7, false));
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
        let (kpp, files) = write_kpp(back).unwrap();
        assert_eq!(kpp, zip.read("paintoppresets/Chalk_1.kpp").unwrap());
        let gbr = files.iter().find(|f| f.kind == "brushes").unwrap();
        assert_eq!(gbr.data, zip.read("brushes/Chalk_1.gbr").unwrap());
        assert!(files.iter().any(|f| f.kind == "patterns"));
        assert!(names.contains(&"patterns/Chalk_1 texture.png"), "{names:?}");
        // No opacity or flow pressure: written as flat curves, read back as none.
        assert!(r.presets[1].settings.opacity_dynamics.pressure.is_none() && !r.presets[1].settings.pressure_density);
        // Still unchanged after the brush list's own format (and the import cache).
        let kept = crate::native::read(&crate::native::to_bytes(&r.presets).unwrap()).unwrap();
        assert_eq!(write_kpp(&kept[0]).unwrap().0, kpp);
    }

    /// A preset with only its XML: no resources, Krita defaults left out.
    fn kpp_with(params: &str, def: &str) -> Vec<u8> {
        let xml = format!(
            "<Preset paintopid=\"paintbrush\" name=\"T\">\n <param name=\"brush_definition\" type=\"string\"><![CDATA[{def}]]></param>\n{params}</Preset>\n"
        );
        with_texts(&preview_png(&BrushPreset::new(BrushSettings::g_pen())), &[("version", "2.2"), ("preset", &xml)])
    }

    #[test]
    fn follows_krita_defaults_and_legacy_keys() {
        // Krita 2.2 called the diameter "radius"; no spacing means 1.0 for generated tips.
        let old = r#"<Brush type="auto_brush"><MaskGenerator radius="30" hfade="1" type="rct"/></Brush>"#;
        let flat = "<!DOCTYPE params>\n<params id=\"pressure\">\n <curve>0,1;1,1;</curve>\n</params>\n";
        let params = format!(
            " <param name=\"OpacitySensor\" type=\"string\"><![CDATA[{flat}]]></param>\n <param name=\"FlowSensor\" type=\"string\"><![CDATA[{flat}]]></param>\n <param name=\"PressureSize\" type=\"internal\">true</param>\n <param name=\"CustomSize\" type=\"internal\">true</param>\n <param name=\"CurveSize\" type=\"string\"><![CDATA[0,0.5;1,1;]]></param>\n <param name=\"PressureScatter\" type=\"internal\">1</param>\n"
        );
        let r = read_kpp(&kpp_with(&params, old), &|_| None).unwrap();
        let p = &r.presets[0];
        assert_eq!(p.settings.size, 30.0);
        assert_eq!(p.settings.spacing, 1.0);
        assert!(r.warnings.iter().any(|w| w.contains("square")), "{:?}", r.warnings);
        assert!(p.settings.opacity_dynamics.pressure.is_none() && !p.settings.pressure_density, "flat curves do nothing");
        assert!((p.settings.size_curve.eval(0.0) - 0.5).abs() < 1e-5, "legacy CurveSize");
        assert_eq!((p.settings.scatter.amount, p.settings.scatter.both_axes), (5.0, true), "scatter defaults");

        // Predefined tips: BrushVersion 1 doubled the scale; spacing defaults to 0.25.
        let tip = crate::gimp::write_gbr(&{
            let mut b = BrushPreset::new(BrushSettings::g_pen());
            b.tip = Some(BrushTip::new(10, 4, vec![255; 40]).unwrap());
            b
        })
        .unwrap();
        let def = r#"<Brush type="gbr_brush" filename="dir/tip.gbr" scale="1.5"/>"#;
        let r = read_kpp(&kpp_with("", def), &|f| (f == "tip.gbr").then(|| tip.clone())).unwrap();
        let p = &r.presets[0];
        assert_eq!((p.settings.size, p.settings.spacing), (30.0, 0.25));
        // With no opacity sensor, pressure drives it (Krita's opacity can't be switched off).
        assert!(p.settings.opacity_dynamics.pressure.is_some());
    }

    #[test]
    fn standalone_presets_embed_their_tip() {
        let mut p = BrushPreset::new(BrushSettings::g_pen());
        p.settings.name = "Stamp".into();
        p.tip = Some(BrushTip::new(3, 3, vec![0, 255, 0, 255, 255, 255, 0, 255, 0]).unwrap());
        p.apply_tip();
        let (kpp, files) = write_kpp(&p).unwrap();
        assert_eq!(files.len(), 1);
        let texts = png_texts(&kpp).unwrap();
        assert_eq!(texts["version"], "5.0");
        assert!(texts["preset"].contains("embedded_resources=\"1\""));
        // Read with no bundle around it: the tip comes from the preset itself.
        let back = &read_kpp(&kpp, &|_| None).unwrap().presets[0];
        assert_eq!(back.tip, p.tip);
    }

    #[test]
    fn png_tips_ink_their_dark_parts() {
        let mut png = Vec::new();
        image::GrayImage::from_raw(2, 1, vec![0, 255])
            .unwrap()
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(decode_tip("a.png", &png, false).unwrap().alpha, [255, 0]);
        let mut rgba = Vec::new();
        image::RgbaImage::from_raw(2, 1, vec![255, 0, 0, 200, 0, 0, 0, 100])
            .unwrap()
            .write_to(&mut std::io::Cursor::new(&mut rgba), image::ImageFormat::Png)
            .unwrap();
        // As a mask: darkness × alpha. As a stamp: the alpha.
        let mask = decode_tip("b.png", &rgba, false).unwrap().alpha;
        assert_eq!(mask[1], 100);
        assert!(mask[0] > 100 && mask[0] < 200);
        assert_eq!(decode_tip("b.png", &rgba, true).unwrap().alpha, [200, 100]);
    }

    /// `png` with a raw tEXt or zTXt chunk added before IEND.
    fn with_raw_text(png: &[u8], kind: &[u8; 4], key: &str, text: &[u8]) -> Vec<u8> {
        let mut body = key.as_bytes().to_vec();
        body.push(0);
        if kind == b"zTXt" {
            body.push(0);
            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            std::io::Write::write_all(&mut z, text).unwrap();
            body.extend(z.finish().unwrap());
        } else {
            body.extend(text);
        }
        let iend = png.len() - 12;
        let mut out = png[..iend].to_vec();
        out.extend((body.len() as u32).to_be_bytes());
        let mut chunk = kind.to_vec();
        chunk.extend(&body);
        out.extend(&chunk);
        out.extend(crc32fast::hash(&chunk).to_be_bytes());
        out.extend(&png[iend..]);
        out
    }

    #[test]
    fn text_chunks_decode_utf8_and_fall_back_to_latin1() {
        let base = preview_png(&BrushPreset::new(BrushSettings::g_pen()));
        let utf8 = "Crayon à l'eau é — 鉛筆 ひらがな Карандаш";
        let png = with_raw_text(&base, b"tEXt", "plain", utf8.as_bytes());
        let png = with_raw_text(&png, b"zTXt", "packed", utf8.as_bytes());
        let png = with_raw_text(&png, b"tEXt", "latin1", b"Caf\xe9");
        let png = with_raw_text(&png, b"zTXt", "zlatin1", b"na\xefve");
        let texts = png_texts(&png).unwrap();
        assert_eq!(texts["plain"], utf8);
        assert_eq!(texts["packed"], utf8);
        assert_eq!(texts["latin1"], "Café");
        assert_eq!(texts["zlatin1"], "naïve");
    }

    #[test]
    fn reads_a_preset_with_raw_utf8_in_its_xml() {
        let xml = "<Preset paintopid=\"paintbrush\" name=\"鉛筆 è\">\n <param name=\"OpacityValue\" type=\"string\"><![CDATA[0.8]]></param>\n</Preset>\n";
        let base = preview_png(&BrushPreset::new(BrushSettings::g_pen()));
        let png = with_texts(&base, &[("version", "2.2")]);
        let png = with_raw_text(&png, b"zTXt", "preset", xml.as_bytes());
        let r = read_kpp(&png, &|_| None).unwrap();
        assert_eq!(r.presets[0].name(), "鉛筆 è");
    }
}
