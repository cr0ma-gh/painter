//! Photoshop brushes (`.abr`).
//!
//! Layouts follow the community documentation of the format, as implemented
//! by GIMP's and Krita's ABR loaders and Adobe's published pattern layout:
//!
//! - **v1 / v2**: `u16 version, u16 count`, then per brush `u16 type, u32 size`.
//!   Type 1 (computed) carries spacing/diameter/hardness; type 2 (sampled)
//!   carries spacing, an optional UCS-2 name (v2), bounds, depth, compression
//!   and the tip bitmap (raw or PackBits RLE).
//! - **v6 / v7 / v10** (`u16 version, u16 subversion`): `8BIM` sections:
//!   - `samp`: tip bitmaps keyed by UUID;
//!   - `patt`: texture patterns (image mode, size, name, UUID, then a
//!     "virtual memory array list" of RLE/raw channels);
//!   - `desc`: an ActionDescriptor with per-brush parameters: tip (`Brsh`:
//!     diameter, spacing, hardness, angle, roundness, `sampledData`),
//!     dynamics (`szVr`, `opVr`), texture (`useTexture`, `Txtr` → pattern,
//!     `textureScale`, `textureDepth`, `InvT`) and dual brush (`useDualBrush`,
//!     `dualBrush` → its own `Brsh`).
//!
//! Tips are 8-bit coverage (16-bit data keeps the high byte); patterns become
//! 8-bit luminance. A tip, pattern or dual brush that can't be decoded falls
//! back to a round tip / no texture, with a warning, instead of failing the file.

use painter_core::{AntiAlias, BrushSettings, PressureCurve};

use crate::descriptor::{read_descriptor, Cursor, Descriptor, Value};
use crate::types::{BrushIoError, BrushPreset, BrushTexture, BrushTip, DualBrush, ImportReport, PresetSource};

/// Photoshop dynamics "control" values (`bVTy`).
const CONTROL_PEN_PRESSURE: f64 = 2.0;
const CONTROL_PEN_TILT: f64 = 3.0;
const CONTROL_INITIAL_DIRECTION: f64 = 6.0;
const CONTROL_DIRECTION: f64 = 7.0;

struct Sample {
    id: Option<String>,
    name: Option<String>,
    spacing: Option<f32>,
    tip: BrushTip,
}

/// A texture pattern from the `patt` section.
struct Pattern {
    id: String,
    name: String,
    image: BrushTip,
}

pub fn read(bytes: &[u8]) -> Result<Vec<BrushPreset>, BrushIoError> {
    read_report(bytes).map(|r| r.presets)
}

/// Cleans Photoshop / Adobe internal localization resource strings.
///
/// Adobe `.abr` files frequently store brush names formatted as localization resource keys,
/// such as `$$$/Presets/Brushes/HeavyMarkerSmallerTip=Heavy Marker Smaller Tip` or
/// `$$$/Presets/Brushes/DryMedia=Dry Media`.
///
/// This function strips the resource key prefix and returns the clean human-readable name.
pub fn clean_brush_name(raw: &str) -> String {
    let s = raw.trim().trim_matches('\0').trim();
    if s.is_empty() {
        return String::new();
    }

    // Case 1: Prefixed with Adobe resource key marker "$$$" or "$$"
    if let Some(rest) = s.strip_prefix("$$$/").or_else(|| s.strip_prefix("$$$")).or_else(|| s.strip_prefix("$$")) {
        // If there's an '=' sign, the human-readable display name is after '='
        if let Some((_key, val)) = rest.split_once('=') {
            let val = val.trim().trim_matches('\0').trim();
            if !val.is_empty() {
                return val.to_string();
            }
        }
        // If no '=' or val was empty, take the last path component
        if let Some(last) = rest.rsplit('/').next() {
            let last = last.trim().trim_matches('\0').trim();
            if !last.is_empty() {
                return last.to_string();
            }
        }
    }

    // Case 2: Contains '=' where key looks like a resource path (e.g. "Presets/Brushes/Key=Display Name")
    if let Some((key, val)) = s.split_once('=') {
        if key.contains('/') || key.contains("Presets") || key.contains("Brushes") {
            let val = val.trim().trim_matches('\0').trim();
            if !val.is_empty() {
                return val.to_string();
            }
        }
    }

    s.to_string()
}

/// Reads every preset, collecting fallbacks taken for unreadable parts.
pub fn read_report(bytes: &[u8]) -> Result<ImportReport, BrushIoError> {
    let mut c = Cursor::new(bytes);
    let version = c.u16()?;
    let mut warnings = Vec::new();
    let mut presets = match version {
        1 | 2 => read_v1_v2(&mut c, version, &mut warnings),
        6..=10 => read_v6(&mut c, version, &mut warnings),
        other => Err(BrushIoError::Unsupported(format!("ABR version {other}"))),
    }?;
    presets.iter_mut().for_each(BrushPreset::apply_tip);
    Ok(ImportReport { presets, warnings, from_cache: false })
}

fn decode_bitmap(c: &mut Cursor<'_>, width: usize, height: usize, depth: u16, compression: u8) -> Result<Vec<u8>, BrushIoError> {
    let bytes_per_px = match depth {
        8 => 1,
        16 => 2,
        other => return Err(BrushIoError::Unsupported(format!("{other}-bit image data"))),
    };
    let row = width * bytes_per_px;
    let raw = match compression {
        0 => c.take(row * height)?.to_vec(),
        1 => {
            let counts: Vec<usize> = (0..height).map(|_| c.u16().map(usize::from)).collect::<Result<_, _>>()?;
            let mut out = Vec::with_capacity(row * height);
            for n in counts {
                out.extend(painter_core::psd::packbits_decode(c.take(n)?, row).ok_or_else(|| BrushIoError::Corrupt("bad RLE row".into()))?);
            }
            out
        }
        other => return Err(BrushIoError::Unsupported(format!("compression {other}"))),
    };
    Ok(if bytes_per_px == 2 { raw.as_chunks::<2>().0.iter().map(|p| p[0]).collect() } else { raw })
}

/// `top, left, bottom, right` → (width, height), with sanity limits.
fn dimensions(bounds: [i32; 4]) -> Result<(usize, usize), BrushIoError> {
    let (w, h) = (bounds[3] - bounds[1], bounds[2] - bounds[0]);
    if w <= 0 || h <= 0 || w > 8192 || h > 8192 {
        return Err(BrushIoError::Corrupt(format!("image bounds {bounds:?}")));
    }
    Ok((w as usize, h as usize))
}

fn round_preset(name: String, version: u16) -> BrushPreset {
    let mut preset =
        BrushPreset::new(BrushSettings { name, pressure_size: false, stabilization: 0, ..BrushSettings::g_pen() });
    preset.source = PresetSource::Photoshop { version, sample_id: None };
    preset
}

fn read_v1_v2(c: &mut Cursor<'_>, version: u16, warnings: &mut Vec<String>) -> Result<Vec<BrushPreset>, BrushIoError> {
    let count = c.u16()?;
    let mut presets = Vec::new();
    for index in 0..count {
        let kind = c.u16()?;
        let size = c.u32()? as usize;
        let end = c.pos + size;
        if end > c.data.len() {
            warnings.push(format!("brush {} is truncated; stopped there", index + 1));
            break;
        }
        match kind {
            1 => {
                // Computed (round) brush.
                let _misc = c.u32()?;
                let spacing = c.u16()?;
                let diameter = c.u16()?;
                let roundness = c.u16()?;
                let angle = c.u16()?;
                let hardness = c.u16()?;
                let mut preset = round_preset(format!("ABR round {}", index + 1), version);
                preset.settings.size = diameter.max(1) as f32;
                preset.settings.spacing = (spacing as f32 / 100.0).max(0.01);
                preset.settings.hardness = (hardness as f32 / 100.0).clamp(0.0, 1.0);
                preset.thickness = (roundness as f32 / 100.0).clamp(0.01, 1.0);
                preset.angle = angle as f32;
                presets.push(preset);
            }
            2 => {
                let _misc = c.u32()?;
                let spacing = c.u16()?;
                let name = if version == 2 { Some(c.unicode()?) } else { None };
                let name = name
                    .map(|n| clean_brush_name(&n))
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| format!("ABR brush {}", index + 1));
                let antialias = c.u8()? != 0;
                let tip = (|| {
                    for _ in 0..4 {
                        c.i16()?; // short bounds, superseded by the long ones
                    }
                    let bounds = [c.i32()?, c.i32()?, c.i32()?, c.i32()?];
                    let depth = c.u16()?;
                    let compression = c.u8()?;
                    let (w, h) = dimensions(bounds)?;
                    BrushTip::new(w as u32, h as u32, decode_bitmap(c, w, h, depth, compression)?)
                })();
                let mut preset = match tip {
                    Ok(tip) => {
                        let sample = Sample { id: None, name: Some(name), spacing: Some(spacing as f32 / 100.0), tip };
                        sampled_preset(sample, version, index as usize)
                    }
                    Err(err) => {
                        warnings.push(format!("\"{name}\": tip unreadable ({err}); using a round tip"));
                        round_preset(name, version)
                    }
                };
                if !antialias {
                    preset.settings.antialias = AntiAlias::None;
                }
                presets.push(preset);
            }
            // Unknown record types are skipped; the size field lets us continue.
            _ => {}
        }
        c.pos = end;
    }
    Ok(presets)
}

fn sampled_preset(sample: Sample, version: u16, index: usize) -> BrushPreset {
    let size = sample.tip.width.max(sample.tip.height) as f32;
    let name = sample
        .name
        .as_deref()
        .map(clean_brush_name)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("ABR brush {}", index + 1));
    let mut preset = BrushPreset::new(BrushSettings {
        name,
        size,
        spacing: sample.spacing.unwrap_or(0.25).max(0.01),
        // The bitmap carries the softness.
        hardness: 1.0,
        pressure_size: false,
        stabilization: 0,
        ..BrushSettings::g_pen()
    });
    preset.tip = Some(sample.tip);
    preset.source = PresetSource::Photoshop { version, sample_id: sample.id };
    preset
}

/// A section key and its body.
type Section<'a> = ([u8; 4], &'a [u8]);

/// Finds `8BIM` sections, tolerating up to 3 padding bytes between them.
fn sections<'a>(c: &mut Cursor<'a>) -> Result<Vec<Section<'a>>, BrushIoError> {
    let mut out = Vec::new();
    while c.remaining() >= 12 {
        let mut skipped = 0;
        while c.remaining() >= 12 && &c.data[c.pos..c.pos + 4] != b"8BIM" {
            if skipped == 3 {
                return Err(BrushIoError::Corrupt(format!("expected an 8BIM section at byte {}", c.pos)));
            }
            c.pos += 1;
            skipped += 1;
        }
        if c.remaining() < 12 {
            break;
        }
        c.take(4)?;
        let key: [u8; 4] = c.take(4)?.try_into().unwrap();
        let len = c.u32()? as usize;
        out.push((key, c.take(len)?));
    }
    Ok(out)
}

fn read_samples(body: &[u8], header_skip: usize, warnings: &mut Vec<String>) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut s = Cursor::new(body);
    while s.remaining() >= 4 {
        let Ok(size) = s.u32() else { break };
        let start = s.pos;
        let next = (start + (size as usize).div_ceil(4) * 4).min(body.len());
        let sample = (|| {
            let id_len = s.u8()? as usize;
            let id = String::from_utf8_lossy(s.take(id_len)?).into_owned();
            s.take(header_skip)?; // fixed-size sample header fields we don't use
            let bounds = [s.i32()?, s.i32()?, s.i32()?, s.i32()?];
            let depth = s.u16()?;
            let compression = s.u8()?;
            let (w, h) = dimensions(bounds)?;
            let tip = BrushTip::new(w as u32, h as u32, decode_bitmap(&mut s, w, h, depth, compression)?)?;
            Ok::<_, BrushIoError>(Sample { id: Some(id), name: None, spacing: None, tip })
        })();
        match sample {
            Ok(sample) => samples.push(sample),
            Err(err) => warnings.push(format!("tip sample {} unreadable ({err}); skipped", samples.len() + 1)),
        }
        s.pos = next;
    }
    samples
}

/// Parses the `patt` section: one pattern after another, each length-prefixed.
fn read_patterns(body: &[u8], warnings: &mut Vec<String>) -> Vec<Pattern> {
    let mut patterns = Vec::new();
    let mut p = Cursor::new(body);
    while p.remaining() >= 4 {
        let Ok(len) = p.u32() else { break };
        let start = p.pos;
        let next = (start + (len as usize).div_ceil(4) * 4).min(body.len());
        match read_pattern(&mut Cursor::new(&body[start..(start + len as usize).min(body.len())])) {
            Ok(pattern) => patterns.push(pattern),
            Err(err) => warnings.push(format!("texture pattern {} unreadable ({err}); skipped", patterns.len() + 1)),
        }
        p.pos = next;
    }
    patterns
}

fn read_pattern(c: &mut Cursor<'_>) -> Result<Pattern, BrushIoError> {
    let _version = c.u32()?;
    let mode = c.u32()?;
    let height = c.u16()? as usize;
    let width = c.u16()? as usize;
    let name = c.unicode()?;
    let id_len = c.u8()? as usize;
    let id = String::from_utf8_lossy(c.take(id_len)?).into_owned();
    let palette = if mode == 2 { Some(c.take(768)?.to_vec()) } else { None };
    // Virtual memory array list.
    let _vmal_version = c.u32()?;
    let _vmal_len = c.u32()?;
    let _rect = [c.u32()?, c.u32()?, c.u32()?, c.u32()?];
    let channels = c.u32()? as usize;
    let mut planes = Vec::new();
    // Colour channels, then a user mask and a sheet mask.
    for _ in 0..channels + 2 {
        if c.remaining() < 4 || c.u32()? == 0 {
            continue; // not written
        }
        let len = c.u32()? as usize;
        if len == 0 {
            continue;
        }
        let end = c.pos + len;
        let _depth = c.u32()?;
        let rect = [c.i32()?, c.i32()?, c.i32()?, c.i32()?];
        let depth = c.u16()?;
        let compression = c.u8()?;
        let (w, h) = dimensions(rect)?;
        planes.push(decode_bitmap(c, w, h, depth, compression)?);
        c.pos = end;
    }
    let n = width * height;
    if planes.is_empty() || planes.iter().any(|p| p.len() != n) {
        return Err(BrushIoError::Corrupt(format!("pattern \"{name}\" channel data doesn't match {width}×{height}")));
    }
    let gray: Vec<u8> = match (mode, &palette) {
        // Indexed: luminance of the palette entry.
        (2, Some(table)) => planes[0].iter().map(|&i| luma(table[i as usize], table[256 + i as usize], table[512 + i as usize])).collect(),
        // RGB (and others with ≥3 channels): luminance.
        (_, _) if planes.len() >= 3 && mode != 1 => (0..n).map(|i| luma(planes[0][i], planes[1][i], planes[2][i])).collect(),
        // Grayscale / bitmap / single channel.
        _ => planes[0].clone(),
    };
    Ok(Pattern { id, name, image: BrushTip::new(width as u32, height as u32, gray)? })
}

fn luma(r: u8, g: u8, b: u8) -> u8 {
    ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114 + 500) / 1000) as u8
}

fn read_v6(c: &mut Cursor<'_>, version: u16, warnings: &mut Vec<String>) -> Result<Vec<BrushPreset>, BrushIoError> {
    let subversion = c.u16()?;
    let header_skip = match subversion {
        1 => 10,
        2 => 264,
        other => return Err(BrushIoError::Unsupported(format!("ABR {version} subversion {other}"))),
    };
    let mut samples = Vec::new();
    let mut patterns = Vec::new();
    let mut desc = None;
    for (key, body) in sections(c)? {
        match &key {
            b"samp" => samples = read_samples(body, header_skip, warnings),
            b"patt" => patterns = read_patterns(body, warnings),
            b"desc" => {
                let mut d = Cursor::new(body);
                d.u32()?; // descriptor version (16)
                match read_descriptor(&mut d) {
                    Ok(parsed) => desc = Some(parsed),
                    Err(err) => warnings.push(format!("brush parameters unreadable ({err}); using tips only")),
                }
            }
            _ => {} // phry (hierarchy), ...
        }
    }

    let Some(desc) = desc else {
        // No parameters: one preset per sampled tip.
        return Ok(samples.into_iter().enumerate().map(|(i, s)| sampled_preset(s, version, i)).collect());
    };
    let mut presets = Vec::new();
    for (index, entry) in desc.list("Brsh").unwrap_or(&[]).iter().enumerate() {
        let Value::Descriptor(entry) = entry else {
            continue;
        };
        presets.push(preset_from_desc(entry, &samples, &patterns, version, index, warnings));
    }
    Ok(presets)
}

/// A tip descriptor's sampled bitmap, if it names one we have.
fn sample_for<'a>(tip: Option<&Descriptor>, samples: &'a [Sample]) -> (Option<String>, Option<&'a Sample>) {
    let id = tip.and_then(|t| t.text("sampledData")).map(str::to_string);
    let sample = id.as_deref().and_then(|id| samples.iter().find(|s| s.id.as_deref() == Some(id)));
    (id, sample)
}

/// A soft disc bitmap, for a computed (round) dual brush.
fn disc_tip(hardness: f32) -> BrushTip {
    const N: u32 = 64;
    let alpha = (0..N * N)
        .map(|i| {
            let (x, y) = ((i % N) as f32 + 0.5 - N as f32 / 2.0, (i / N) as f32 + 0.5 - N as f32 / 2.0);
            let d = x.hypot(y) / (N as f32 / 2.0);
            let inner = hardness.clamp(0.0, 0.99);
            let t = ((d - inner) / (1.0 - inner)).clamp(0.0, 1.0);
            ((1.0 - t * t * (3.0 - 2.0 * t)) * 255.0).round() as u8
        })
        .collect();
    BrushTip::new(N, N, alpha).expect("fixed size")
}

/// Builds a preset from one `desc` brush entry.
fn preset_from_desc(
    entry: &Descriptor,
    samples: &[Sample],
    patterns: &[Pattern],
    version: u16,
    index: usize,
    warnings: &mut Vec<String>,
) -> BrushPreset {
    let name = entry
        .text("Nm  ")
        .map(clean_brush_name)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("ABR brush {}", index + 1));
    let tip_desc = entry.descriptor("Brsh");
    let (sample_id, sample) = sample_for(tip_desc, samples);

    let mut preset = match sample {
        Some(s) => sampled_preset(Sample { id: sample_id.clone(), name: None, spacing: None, tip: s.tip.clone() }, version, index),
        None => {
            if let Some(id) = &sample_id {
                warnings.push(format!("\"{name}\": tip {id} missing; using a round tip"));
            }
            round_preset(String::new(), version)
        }
    };
    preset.settings.name = name.clone();
    preset.source = PresetSource::Photoshop { version, sample_id };
    if let Some(tip) = tip_desc {
        if let Some(d) = tip.number("Dmtr") {
            preset.settings.size = (d as f32).clamp(1.0, 5000.0);
        }
        if let Some(s) = tip.number("Spcn") {
            preset.settings.spacing = (s as f32 / 100.0).max(0.01);
        }
        if let Some(h) = tip.number("Hrdn") {
            preset.settings.hardness = (h as f32 / 100.0).clamp(0.0, 1.0);
        }
        if let Some(a) = tip.number("Angl") {
            preset.angle = a as f32;
        }
        if let Some(r) = tip.number("Rndn") {
            preset.thickness = (r as f32 / 100.0).clamp(0.01, 1.0);
        }
        if tip.flag("Intr") == Some(false) {
            preset.settings.antialias = AntiAlias::None;
        }
    }

    // Shape dynamics: size variation controlled by pen pressure.
    let tip_dynamics = entry.flag("useTipDynamics").unwrap_or(true);
    if let Some(size) = entry.descriptor("szVr").filter(|_| tip_dynamics) {
        let minimum = entry.number("minimumDiameter").map(|m| (m as f32 / 100.0).clamp(0.0, 1.0));
        match size.number("bVTy") {
            Some(CONTROL_PEN_PRESSURE) => {
                preset.dynamics.size.pressure = Some(PressureCurve::linear());
                preset.dynamics.size.minimum = minimum;
            }
            // Tilt: full size upright, down to the minimum when flat.
            Some(CONTROL_PEN_TILT) => {
                let low = minimum.unwrap_or(0.0);
                preset.dynamics.size.tilt = Some(PressureCurve::from_points(vec![[0.0, 1.0], [1.0, low]]));
            }
            _ => {}
        }
        if let Some(j) = size.number("jitter").filter(|j| *j > 0.0) {
            preset.dynamics.size.random = Some((j as f32 / 100.0).clamp(0.0, 1.0));
        }
    }
    // Transfer: per-dab opacity ("opacity jitter") → our flow/density.
    let paint_dynamics = entry.flag("usePaintDynamics").unwrap_or(true);
    if let Some(opacity) = entry.descriptor("opVr").filter(|_| paint_dynamics) {
        match opacity.number("bVTy") {
            Some(CONTROL_PEN_PRESSURE) => preset.dynamics.flow.pressure = Some(PressureCurve::linear()),
            Some(CONTROL_PEN_TILT) => {
                preset.dynamics.flow.tilt = Some(PressureCurve::from_points(vec![[0.0, 1.0], [1.0, 0.0]]));
            }
            _ => {}
        }
        if let Some(j) = opacity.number("jitter").filter(|j| *j > 0.0) {
            preset.dynamics.flow.random = Some((j as f32 / 100.0).clamp(0.0, 1.0));
        }
    }
    // Angle dynamics: jitter (100% = ±180°) and direction control.
    if let Some(angle) = entry.descriptor("angleDynamics").filter(|_| tip_dynamics) {
        if let Some(j) = angle.number("jitter").filter(|j| *j > 0.0) {
            preset.dynamics.rotation.random = Some((j as f32 / 100.0).clamp(0.0, 1.0));
        }
        match angle.number("bVTy") {
            Some(CONTROL_DIRECTION | CONTROL_INITIAL_DIRECTION) => preset.dynamics.rotation_follows_direction = true,
            // Pressure / tilt sweep the angle through a full turn.
            Some(CONTROL_PEN_PRESSURE) => preset.dynamics.rotation.pressure = Some(PressureCurve::linear()),
            Some(CONTROL_PEN_TILT) => preset.dynamics.rotation.tilt = Some(PressureCurve::linear()),
            _ => {}
        }
    }
    // Roundness dynamics: tip thickness from pressure / tilt, down to `minimumRoundness`.
    if let Some(round) = entry.descriptor("roundnessDynamics").filter(|_| tip_dynamics) {
        let low = entry.number("minimumRoundness").map_or(0.25, |m| (m as f32 / 100.0).clamp(0.01, 1.0));
        match round.number("bVTy") {
            Some(CONTROL_PEN_PRESSURE) => {
                preset.dynamics.thickness.pressure = Some(PressureCurve::from_points(vec![[0.0, low], [1.0, 1.0]]));
            }
            Some(CONTROL_PEN_TILT) => {
                preset.dynamics.thickness.tilt = Some(PressureCurve::from_points(vec![[0.0, 1.0], [1.0, low]]));
            }
            _ => {}
        }
        if let Some(j) = round.number("jitter").filter(|j| *j > 0.0) {
            preset.dynamics.thickness.random = Some((j as f32 / 100.0).clamp(0.0, 1.0));
        }
    }
    preset.apply_dynamics();

    // Texture: a pattern from the `patt` section, by UUID (or name).
    if entry.flag("useTexture") == Some(true) {
        if let Some(reference) = entry.descriptor("Txtr") {
            let id = reference.text("Idnt");
            let pattern_name = reference.text("Nm  ");
            let found = patterns
                .iter()
                .find(|p| id.is_some_and(|id| p.id == id))
                .or_else(|| patterns.iter().find(|p| pattern_name.is_some_and(|n| p.name == n)));
            match found {
                Some(pattern) => {
                    preset.texture = Some(BrushTexture {
                        image: pattern.image.clone(),
                        scale: entry.number("textureScale").unwrap_or(100.0) as f32,
                        density: (entry.number("textureDepth").unwrap_or(100.0) as f32 / 100.0).clamp(0.0, 1.0),
                        invert: entry.flag("InvT").unwrap_or(false),
                    });
                }
                None => warnings.push(format!(
                    "\"{name}\": texture pattern {} not in the file; painting without texture",
                    pattern_name.or(id).unwrap_or("?")
                )),
            }
        }
    }

    // Dual brush: its own tip, scaled relative to the main diameter.
    if entry.flag("useDualBrush") == Some(true) {
        if let Some(dual) = entry.descriptor("dualBrush") {
            let dual_tip_desc = dual.descriptor("Brsh");
            let (dual_id, dual_sample) = sample_for(dual_tip_desc, samples);
            let main = preset.settings.size.max(1.0);
            let diameter = dual_tip_desc.and_then(|t| t.number("Dmtr")).map_or(main, |d| d as f32);
            let tip = match (dual_sample, &dual_id) {
                (Some(s), _) => Some(s.tip.clone()),
                (None, None) => Some(disc_tip(dual_tip_desc.and_then(|t| t.number("Hrdn")).map_or(1.0, |h| h as f32 / 100.0))),
                (None, Some(id)) => {
                    warnings.push(format!("\"{name}\": dual brush tip {id} missing; dual brush dropped"));
                    None
                }
            };
            if let Some(tip) = tip {
                preset.dual = Some(DualBrush { tip, scale: (diameter / main).clamp(0.05, 8.0) });
            }
        }
    }

    for key in ["Wtdg", "Nose", "Rpt ", "useScatter", "useBrushPose", "textureBlendMode", "dualBrush"] {
        if let Some(value) = entry.get(key) {
            preset.extras.insert(format!("abr.{}", key.trim()), describe(value));
        }
    }
    preset
}

/// A JSON rendering of a descriptor value, for `extras`.
fn describe(value: &Value) -> serde_json::Value {
    match value {
        Value::Descriptor(d) => {
            serde_json::Value::Object(d.items.iter().map(|(k, v)| (k.trim().to_string(), describe(v))).collect())
        }
        Value::List(l) => serde_json::Value::Array(l.iter().map(describe).collect()),
        Value::Double(v) | Value::Unit(_, v) => serde_json::json!(v),
        Value::Text(t) => serde_json::json!(t),
        Value::Enum(t, e) => serde_json::json!(format!("{}.{}", t.trim(), e.trim())),
        Value::Integer(v) => serde_json::json!(v),
        Value::Bool(b) => serde_json::json!(b),
        Value::Class(c) => serde_json::json!(c),
        Value::Raw(r) => serde_json::json!(format!("<{} bytes>", r.len())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::build::{descriptor, V};

    fn tip_pixels(w: usize, h: usize) -> Vec<u8> {
        (0..w * h).map(|i| ((i * 37) % 256) as u8).collect()
    }

    fn rle(data: &[u8], w: usize) -> Vec<u8> {
        let mut counts = Vec::new();
        let mut body = Vec::new();
        for row in data.chunks(w) {
            let before = body.len();
            painter_core::psd::packbits_encode(row, &mut body);
            counts.extend(((body.len() - before) as u16).to_be_bytes());
        }
        [counts, body].concat()
    }

    fn sampled_v2(name: &str, w: usize, h: usize, rle_data: bool) -> Vec<u8> {
        let mut rec = Vec::new();
        rec.extend(0u32.to_be_bytes()); // misc
        rec.extend(25u16.to_be_bytes()); // spacing %
        crate::descriptor::build::unicode(&mut rec, name);
        rec.push(1); // antialias
        rec.extend([0u8; 8]); // short bounds
        for v in [0i32, 0, h as i32, w as i32] {
            rec.extend(v.to_be_bytes());
        }
        rec.extend(8u16.to_be_bytes());
        let pixels = tip_pixels(w, h);
        if rle_data {
            rec.push(1);
            rec.extend(rle(&pixels, w));
        } else {
            rec.push(0);
            rec.extend(&pixels);
        }
        let mut out = Vec::new();
        out.extend(2u16.to_be_bytes());
        out.extend((rec.len() as u32).to_be_bytes());
        out.extend(rec);
        out
    }

    #[test]
    fn reads_v2_sampled_and_computed() {
        let mut file = Vec::new();
        file.extend(2u16.to_be_bytes()); // version
        file.extend(3u16.to_be_bytes()); // count
        file.extend(sampled_v2("Spatter", 5, 4, false));
        file.extend(sampled_v2("Chalk", 7, 3, true));
        // Computed brush.
        file.extend(1u16.to_be_bytes());
        file.extend(14u32.to_be_bytes());
        file.extend(0u32.to_be_bytes());
        for v in [10u16, 19, 60, 30, 80] {
            file.extend(v.to_be_bytes());
        }

        let presets = read(&file).unwrap();
        assert_eq!(presets.len(), 3);
        assert_eq!(presets[0].name(), "Spatter");
        assert_eq!(presets[0].tip.as_ref().unwrap().alpha, tip_pixels(5, 4));
        assert!((presets[0].settings.spacing - 0.25).abs() < 1e-6);
        assert_eq!(presets[1].tip.as_ref().unwrap().alpha, tip_pixels(7, 3));
        let round = &presets[2];
        assert!(round.tip.is_none());
        assert_eq!(round.settings.size, 19.0);
        assert!((round.settings.hardness - 0.8).abs() < 1e-6);
        assert!((round.thickness - 0.6).abs() < 1e-6);
    }

    fn v6_file(subversion: u16, with_desc: bool) -> Vec<u8> {
        let (w, h) = (6usize, 5usize);
        let mut sample = Vec::new();
        let id = "12345678-90ab-cdef-1234-567890abcdef";
        sample.push(id.len() as u8);
        sample.extend(id.as_bytes());
        sample.extend(vec![0u8; if subversion == 1 { 10 } else { 264 }]);
        for v in [0i32, 0, h as i32, w as i32] {
            sample.extend(v.to_be_bytes());
        }
        sample.extend(16u16.to_be_bytes()); // 16-bit samples
        sample.push(0);
        for p in tip_pixels(w, h) {
            sample.extend([p, 0x7f]);
        }
        let mut samp = Vec::new();
        samp.extend((sample.len() as u32).to_be_bytes());
        samp.extend(&sample);
        while !samp.len().is_multiple_of(4) {
            samp.push(0);
        }

        let mut file = Vec::new();
        file.extend(10u16.to_be_bytes());
        file.extend(subversion.to_be_bytes());
        file.extend(b"8BIMsamp");
        file.extend((samp.len() as u32).to_be_bytes());
        file.extend(&samp);
        if with_desc {
            let mut d = Vec::new();
            d.extend(16u32.to_be_bytes());
            descriptor(
                &mut d,
                "null",
                &[(
                    "Brsh",
                    V::List(vec![
                        V::Obj(
                            "brushPreset",
                            vec![
                                ("Nm  ", V::Text("Dry Ink")),
                                (
                                    "Brsh",
                                    V::Obj(
                                        "sampledBrush",
                                        vec![
                                            ("Dmtr", V::Unit("#Pxl", 42.0)),
                                            ("Spcn", V::Unit("#Prc", 12.0)),
                                            ("Angl", V::Unit("#Ang", 30.0)),
                                            ("Rndn", V::Unit("#Prc", 50.0)),
                                            ("sampledData", V::Text(id)),
                                        ],
                                    ),
                                ),
                                ("useTipDynamics", V::Bool(true)),
                                ("szVr", V::Obj("brVr", vec![("bVTy", V::Long(2)), ("jitter", V::Unit("#Prc", 10.0))])),
                                ("minimumDiameter", V::Unit("#Prc", 20.0)),
                                ("usePaintDynamics", V::Bool(true)),
                                ("opVr", V::Obj("brVr", vec![("bVTy", V::Long(2))])),
                            ],
                        ),
                        V::Obj(
                            "brushPreset",
                            vec![
                                ("Nm  ", V::Text("Hard Round")),
                                ("Brsh", V::Obj("computedBrush", vec![("Dmtr", V::Unit("#Pxl", 9.0)), ("Hrdn", V::Unit("#Prc", 100.0))])),
                            ],
                        ),
                    ]),
                )],
            );
            file.extend(b"8BIMdesc");
            file.extend((d.len() as u32).to_be_bytes());
            file.extend(&d);
        }
        file
    }

    #[test]
    fn reads_v6_samples_and_descriptor() {
        let presets = read(&v6_file(2, true)).unwrap();
        assert_eq!(presets.len(), 2);
        let ink = &presets[0];
        assert_eq!(ink.name(), "Dry Ink");
        assert_eq!(ink.tip.as_ref().unwrap().alpha, tip_pixels(6, 5), "16-bit tips keep the high byte");
        // The engine paints the sampled tip, flattened and rotated.
        let engine_tip = &ink.settings.tip;
        assert_eq!(engine_tip.bitmap().unwrap().alpha(), &tip_pixels(6, 5)[..]);
        assert!((engine_tip.thickness - 0.5).abs() < 1e-6 && engine_tip.angle == 30.0);
        assert_eq!(ink.settings.size, 42.0);
        assert!((ink.settings.spacing - 0.12).abs() < 1e-6);
        assert!(ink.settings.pressure_size && ink.settings.pressure_density);
        assert!((ink.settings.min_size_ratio - 0.2).abs() < 1e-6);
        assert_eq!(ink.dynamics.size.random, Some(0.1));
        assert!((ink.thickness - 0.5).abs() < 1e-6 && ink.angle == 30.0);
        let round = &presets[1];
        assert!(round.tip.is_none());
        assert_eq!((round.settings.size, round.settings.hardness), (9.0, 1.0));
    }

    #[test]
    fn v6_without_descriptor_and_subversion_1() {
        let presets = read(&v6_file(1, false)).unwrap();
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].tip.as_ref().unwrap().width, 6);
    }

    #[test]
    fn rejects_garbage() {
        assert!(read(&[0, 3, 0, 0]).is_err());
        assert!(read(&[0, 2]).is_err());
    }

    fn v6_sample(id: &str, w: usize, h: usize, pixels: &[u8]) -> Vec<u8> {
        let mut sample = vec![id.len() as u8];
        sample.extend(id.as_bytes());
        sample.extend(vec![0u8; 264]);
        for v in [0i32, 0, h as i32, w as i32] {
            sample.extend(v.to_be_bytes());
        }
        sample.extend(8u16.to_be_bytes());
        sample.push(1);
        sample.extend(rle(pixels, w));
        let mut out = (sample.len() as u32).to_be_bytes().to_vec();
        out.extend(sample);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        out
    }

    fn pattern(id: &str, name: &str, w: usize, h: usize, gray: &[u8], rgb: bool) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend(1u32.to_be_bytes());
        p.extend((if rgb { 3u32 } else { 1u32 }).to_be_bytes());
        p.extend((h as u16).to_be_bytes());
        p.extend((w as u16).to_be_bytes());
        crate::descriptor::build::unicode(&mut p, name);
        p.push(id.len() as u8);
        p.extend(id.as_bytes());
        let channels = if rgb { 3 } else { 1 };
        let mut vmal = Vec::new();
        for v in [0u32, 0, h as u32, w as u32] {
            vmal.extend(v.to_be_bytes());
        }
        vmal.extend((channels as u32).to_be_bytes());
        for _ in 0..channels {
            let mut ch = Vec::new();
            ch.extend(8u32.to_be_bytes());
            for v in [0i32, 0, h as i32, w as i32] {
                ch.extend(v.to_be_bytes());
            }
            ch.extend(8u16.to_be_bytes());
            ch.push(1);
            ch.extend(rle(gray, w));
            vmal.extend(1u32.to_be_bytes());
            vmal.extend((ch.len() as u32).to_be_bytes());
            vmal.extend(ch);
        }
        vmal.extend([0u8; 8]); // user mask and sheet mask: not written
        p.extend(3u32.to_be_bytes());
        p.extend((vmal.len() as u32).to_be_bytes());
        p.extend(vmal);
        let mut out = (p.len() as u32).to_be_bytes().to_vec();
        out.extend(p);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        out
    }

    fn section(key: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = b"8BIM".to_vec();
        out.extend(key);
        out.extend((body.len() as u32).to_be_bytes());
        out.extend(body);
        out
    }

    #[test]
    fn reads_patterns_textures_dual_brushes_and_falls_back() {
        let main_id = "aaaaaaaa-0000-0000-0000-000000000001";
        let dual_id = "aaaaaaaa-0000-0000-0000-000000000002";
        let bad_id = "aaaaaaaa-0000-0000-0000-000000000003";
        let mut samp = v6_sample(main_id, 6, 5, &tip_pixels(6, 5));
        samp.extend(v6_sample(dual_id, 4, 4, &tip_pixels(4, 4)));
        // A sample with impossible bounds.
        let mut bad = vec![bad_id.len() as u8];
        bad.extend(bad_id.as_bytes());
        bad.extend(vec![0u8; 264]);
        for v in [0i32, 0, -5, 3] {
            bad.extend(v.to_be_bytes());
        }
        bad.extend([0, 8, 0]);
        samp.extend((bad.len() as u32).to_be_bytes());
        samp.extend(&bad);
        while !samp.len().is_multiple_of(4) {
            samp.push(0);
        }

        let gray: Vec<u8> = (0..12).map(|i| (i * 20) as u8).collect();
        let mut patt = pattern("pat-gray", "Canvas", 4, 3, &gray, false);
        patt.extend(pattern("pat-rgb", "Linen", 4, 3, &gray, true));

        let mut d = 16u32.to_be_bytes().to_vec();
        descriptor(
            &mut d,
            "null",
            &[(
                "Brsh",
                V::List(vec![
                    V::Obj(
                        "brushPreset",
                        vec![
                            ("Nm  ", V::Text("Textured Dual")),
                            ("Brsh", V::Obj("sampledBrush", vec![("Dmtr", V::Unit("#Pxl", 42.0)), ("sampledData", V::Text(main_id))])),
                            ("useTexture", V::Bool(true)),
                            ("Txtr", V::Obj("Ptrn", vec![("Nm  ", V::Text("Canvas")), ("Idnt", V::Text("pat-gray"))])),
                            ("textureScale", V::Unit("#Prc", 50.0)),
                            ("textureDepth", V::Unit("#Prc", 70.0)),
                            ("InvT", V::Bool(true)),
                            ("useDualBrush", V::Bool(true)),
                            (
                                "dualBrush",
                                V::Obj("dualBrush", vec![("Brsh", V::Obj("sampledBrush", vec![("Dmtr", V::Unit("#Pxl", 21.0)), ("sampledData", V::Text(dual_id))]))]),
                            ),
                        ],
                    ),
                    V::Obj(
                        "brushPreset",
                        vec![
                            ("Nm  ", V::Text("RGB pattern by name")),
                            ("useTexture", V::Bool(true)),
                            ("Txtr", V::Obj("Ptrn", vec![("Nm  ", V::Text("Linen"))])),
                        ],
                    ),
                    V::Obj(
                        "brushPreset",
                        vec![
                            ("Nm  ", V::Text("Broken")),
                            ("Brsh", V::Obj("sampledBrush", vec![("sampledData", V::Text(bad_id))])),
                            ("useTexture", V::Bool(true)),
                            ("Txtr", V::Obj("Ptrn", vec![("Idnt", V::Text("nowhere"))])),
                            ("useDualBrush", V::Bool(true)),
                            ("dualBrush", V::Obj("dualBrush", vec![("Brsh", V::Obj("computedBrush", vec![("Hrdn", V::Unit("#Prc", 50.0))]))])),
                        ],
                    ),
                ]),
            )],
        );

        let mut file = 10u16.to_be_bytes().to_vec();
        file.extend(2u16.to_be_bytes());
        file.extend(section(b"samp", &samp));
        file.extend(section(b"patt", &patt));
        file.extend(section(b"desc", &d));

        let report = read_report(&file).unwrap();
        let [textured, by_name, broken] = &report.presets[..] else { panic!("{} presets", report.presets.len()) };

        let texture = textured.texture.as_ref().unwrap();
        assert_eq!(texture.image.alpha, gray);
        assert_eq!((texture.scale, texture.density, texture.invert), (50.0, 0.7, true));
        let dual = textured.dual.as_ref().unwrap();
        assert_eq!(dual.tip.alpha, tip_pixels(4, 4));
        assert!((dual.scale - 0.5).abs() < 1e-6);
        // Engine settings follow (scale in canvas px per texel).
        let engine_texture = textured.settings.texture.as_ref().unwrap();
        assert!((engine_texture.scale - 0.5).abs() < 1e-6 && engine_texture.invert);
        assert!(textured.settings.dual.is_some());

        assert_eq!(by_name.texture.as_ref().unwrap().image.alpha, gray, "RGB pattern → luminance");

        // Corrupt tip → round tip; missing pattern → no texture; computed dual → disc.
        assert!(broken.tip.is_none() && broken.texture.is_none());
        assert_eq!(broken.dual.as_ref().unwrap().tip.width, 64);
        assert_eq!(report.warnings.len(), 3, "{:?}", report.warnings);
        assert!(report.warnings.iter().any(|w| w.contains("tip sample 3 unreadable")));
        assert!(report.warnings.iter().any(|w| w.contains("Broken") && w.contains("round tip")));
        assert!(report.warnings.iter().any(|w| w.contains("nowhere")));
    }

    #[test]
    fn maps_tilt_jitter_and_direction_dynamics() {
        let mut d = 16u32.to_be_bytes().to_vec();
        descriptor(
            &mut d,
            "null",
            &[(
                "Brsh",
                V::List(vec![V::Obj(
                    "brushPreset",
                    vec![
                        ("Nm  ", V::Text("Calligraphic")),
                        ("Brsh", V::Obj("computedBrush", vec![("Dmtr", V::Unit("#Pxl", 20.0)), ("Rndn", V::Unit("#Prc", 30.0))])),
                        ("szVr", V::Obj("brVr", vec![("bVTy", V::Long(3)), ("jitter", V::Unit("#Prc", 25.0))])),
                        ("minimumDiameter", V::Unit("#Prc", 40.0)),
                        ("opVr", V::Obj("brVr", vec![("bVTy", V::Long(3))])),
                        ("angleDynamics", V::Obj("brVr", vec![("bVTy", V::Long(7)), ("jitter", V::Unit("#Prc", 50.0))])),
                        ("roundnessDynamics", V::Obj("brVr", vec![("bVTy", V::Long(2)), ("jitter", V::Unit("#Prc", 20.0))])),
                        ("minimumRoundness", V::Unit("#Prc", 30.0)),
                    ],
                )]),
            )],
        );
        let mut file = 10u16.to_be_bytes().to_vec();
        file.extend(2u16.to_be_bytes());
        file.extend(section(b"desc", &d));
        let preset = &read(&file).unwrap()[0];
        let s = &preset.settings;
        let size_tilt = s.size_dynamics.tilt.as_ref().expect("size from tilt");
        assert!(size_tilt.eval(0.0) == 1.0 && (size_tilt.eval(1.0) - 0.4).abs() < 1e-6);
        assert!((s.size_dynamics.jitter - 0.25).abs() < 1e-6);
        assert_eq!(s.density_dynamics.tilt.as_ref().unwrap().eval(1.0), 0.0);
        assert!(s.rotation.follow_direction && (s.rotation.jitter - 0.5).abs() < 1e-6);
        let thickness = s.thickness_dynamics.pressure.as_ref().expect("roundness from pressure");
        assert!((thickness.eval(0.0) - 0.3).abs() < 1e-6 && thickness.eval(1.0) == 1.0);
        assert!((s.thickness_dynamics.jitter - 0.2).abs() < 1e-6);
        assert!(!s.pressure_size, "tilt control, not pressure");
        assert!(preset.unsupported_features().is_empty());
    }

    #[test]
    fn test_clean_brush_name_adobe_localization_keys() {
        // Screenshot example cases:
        assert_eq!(
            clean_brush_name("$$$/Presets/Brushes/HeavyMarkerSmallerTip=Heavy Marker Smaller Tip"),
            "Heavy Marker Smaller Tip"
        );
        assert_eq!(
            clean_brush_name("$$$/Presets/Brushes/KyleBok=Kyle's Bok"),
            "Kyle's Bok"
        );
        assert_eq!(
            clean_brush_name("$$$/VectorPresets/Brushes/Charcoal=Charcoal 01"),
            "Charcoal 01"
        );
        // Without equals sign
        assert_eq!(
            clean_brush_name("$$$/Presets/Brushes/RoughPencil"),
            "RoughPencil"
        );
        // Path with equals sign but no $$$
        assert_eq!(
            clean_brush_name("Presets/Brushes/DryMedia=Rough Chalk"),
            "Rough Chalk"
        );
        // Null-terminated or whitespace padding
        assert_eq!(
            clean_brush_name("  $$$/Presets/Brushes/SoftRound30=Soft Round 30\0  "),
            "Soft Round 30"
        );
        // Normal names preserved untouched
        assert_eq!(clean_brush_name("G-Pen"), "G-Pen");
        assert_eq!(clean_brush_name("Airbrush"), "Airbrush");
        assert_eq!(clean_brush_name("Hard Round 19"), "Hard Round 19");
        assert_eq!(clean_brush_name("   "), "");
    }
}
