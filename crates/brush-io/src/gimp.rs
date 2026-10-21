//! GIMP brushes (also read by Krita and MyPaint's tools):
//!
//! - `.gbr`: one bitmap tip. Big-endian header: header size, version (1–3),
//!   width, height, bytes per pixel (1 = grey coverage, 4 = RGBA colour
//!   brush), then from version 2 the magic `GIMP` and the spacing (percent of
//!   the tip size), then the NUL-terminated UTF-8 name; the pixels follow.
//!   Grey values are ink coverage (255 = full).
//! - `.gih` ("image hose"): a name line, a `<cells> <parameters>` line, then
//!   that many `.gbr` brushes, picked per dab (randomly, by angle, pressure…).
//!   Our engine has one tip per brush, so the first cell is used.
//! - `.vbr`: a parametric round / square / diamond tip as text lines.
//!
//! Written: `.gbr` for bitmap tips (grey), `.vbr` for round tips.

use crate::types::{BrushIoError, BrushPreset, BrushTip, ImportReport, PresetSource};
use painter_core::BrushSettings;

fn corrupt(msg: impl Into<String>) -> BrushIoError {
    BrushIoError::Corrupt(msg.into())
}

fn be32(d: &[u8], i: usize) -> Result<u32, BrushIoError> {
    Ok(u32::from_be_bytes(d.get(i..i + 4).ok_or_else(|| corrupt("truncated GIMP brush"))?.try_into().unwrap()))
}

/// A preset around a GIMP tip: size from the bitmap, spacing from the file.
fn preset(name: &str, tip: Option<BrushTip>, size: f32, spacing_percent: f32) -> BrushPreset {
    let mut settings = BrushSettings::g_pen();
    settings.name = if name.trim().is_empty() { "GIMP brush".into() } else { name.trim().to_string() };
    settings.size = size.max(1.0);
    settings.spacing = (spacing_percent / 100.0).clamp(0.01, 10.0);
    // GIMP's default dynamics ("Pressure Opacity") vary opacity, not size.
    settings.pressure_size = false;
    settings.pressure_density = true;
    let mut preset = BrushPreset::new(settings);
    preset.tip = tip;
    preset.source = PresetSource::Gimp;
    preset.extras.insert("gimp.spacing".into(), spacing_percent.into());
    preset.apply_tip();
    preset
}

/// One `.gbr` brush at the start of `data`; also returns the bytes it took.
fn gbr_at(data: &[u8], warnings: &mut Vec<String>) -> Result<(BrushPreset, usize), BrushIoError> {
    let header = be32(data, 0)? as usize;
    let version = be32(data, 4)?;
    let (w, h, bpp) = (be32(data, 8)?, be32(data, 12)?, be32(data, 16)? as usize);
    let (spacing, name_at) = match version {
        1 => (25, 20),
        2 | 3 => {
            if data.get(20..24) != Some(b"GIMP") {
                return Err(corrupt("no GIMP magic"));
            }
            (be32(data, 24)?, 28)
        }
        v => return Err(BrushIoError::Unsupported(format!("GIMP brush version {v}"))),
    };
    if w == 0 || h == 0 || w > 10_000 || h > 10_000 || header < name_at {
        return Err(corrupt(format!("brush size {w}×{h}")));
    }
    let name_bytes = data.get(name_at..header).ok_or_else(|| corrupt("truncated name"))?;
    let name = String::from_utf8_lossy(name_bytes.split(|b| *b == 0).next().unwrap_or_default()).into_owned();
    let n = (w * h) as usize;
    let pixels = data.get(header..header + n * bpp).ok_or_else(|| corrupt("truncated pixels"))?;
    let alpha = match bpp {
        1 => pixels.to_vec(),
        4 => {
            warnings.push(format!("“{name}”: colour brush; its shape is kept, its colours aren't"));
            pixels.as_chunks::<4>().0.iter().map(|p| p[3]).collect()
        }
        2 => pixels.as_chunks::<2>().0.iter().map(|p| p[0]).collect(),
        b => return Err(BrushIoError::Unsupported(format!("{b} bytes per pixel"))),
    };
    let tip = BrushTip::new(w, h, alpha)?;
    Ok((preset(&name, Some(tip), w.max(h) as f32, spacing as f32), header + n * bpp))
}

pub fn read_gbr(data: &[u8]) -> Result<ImportReport, BrushIoError> {
    let mut warnings = Vec::new();
    let (preset, _) = gbr_at(data, &mut warnings)?;
    Ok(ImportReport { presets: vec![preset], warnings, from_cache: false })
}

pub fn read_gih(data: &[u8]) -> Result<ImportReport, BrushIoError> {
    let mut lines = data.splitn(3, |b| *b == b'\n');
    let name = String::from_utf8_lossy(lines.next().unwrap_or_default()).trim().to_string();
    let params = String::from_utf8_lossy(lines.next().ok_or_else(|| corrupt("truncated .gih"))?).into_owned();
    let rest = lines.next().ok_or_else(|| corrupt("truncated .gih"))?;
    let cells: usize = params.split_whitespace().next().and_then(|n| n.parse().ok()).ok_or_else(|| corrupt("no cell count"))?;
    let mut warnings = Vec::new();
    let (mut preset, _) = gbr_at(rest, &mut warnings)?;
    preset.settings.name = if name.is_empty() { preset.settings.name } else { name };
    preset.extras.insert("gimp.gih_parameters".into(), params.trim().into());
    if cells > 1 {
        warnings.push(format!("“{}”: an image hose of {cells} tips; the first one is used", preset.settings.name));
    }
    Ok(ImportReport { presets: vec![preset], warnings, from_cache: false })
}

pub fn read_vbr(data: &[u8]) -> Result<ImportReport, BrushIoError> {
    let text = String::from_utf8_lossy(data);
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    if lines.first() != Some(&"GIMP-VBR") {
        return Err(corrupt("not a GIMP-VBR file"));
    }
    let f = |i: usize| -> Result<f32, BrushIoError> {
        lines.get(i).and_then(|l| l.parse().ok()).ok_or_else(|| corrupt(format!("bad .vbr line {}", i + 1)))
    };
    let name = lines.get(2).copied().unwrap_or("");
    let mut warnings = Vec::new();
    // 1.0: spacing radius hardness aspect angle. 1.5 adds the shape and spikes.
    let (spacing, radius, hardness, aspect, angle) = match lines.get(1).copied() {
        Some("1.0") => (f(3)?, f(4)?, f(5)?, f(6)?, f(7)?),
        Some("1.5") => {
            let shape = lines.get(3).copied().unwrap_or("circle");
            if shape != "circle" {
                warnings.push(format!("“{name}”: {shape} tips are drawn round"));
            }
            (f(4)?, f(5)?, f(7)?, f(8)?, f(9)?)
        }
        other => return Err(BrushIoError::Unsupported(format!("GIMP-VBR version {}", other.unwrap_or("?")))),
    };
    let mut p = preset(name, None, radius * 2.0, spacing);
    p.settings.hardness = hardness.clamp(0.0, 1.0);
    p.thickness = (1.0 / aspect.max(1.0)).clamp(0.01, 1.0);
    p.angle = angle;
    p.apply_tip();
    Ok(ImportReport { presets: vec![p], warnings, from_cache: false })
}

/// A bitmap tip as `.gbr` (version 2, grey).
pub fn write_gbr(preset: &BrushPreset) -> Result<Vec<u8>, BrushIoError> {
    let tip = preset.tip.as_ref().ok_or_else(|| BrushIoError::Unsupported("a round tip (use .vbr)".into()))?;
    let name = preset.name().as_bytes();
    let header = 28 + name.len() + 1;
    let spacing = (preset.settings.spacing * 100.0).round().clamp(1.0, 1000.0) as u32;
    let mut out = Vec::with_capacity(header + tip.alpha.len());
    for v in [header as u32, 2, tip.width, tip.height, 1] {
        out.extend(v.to_be_bytes());
    }
    out.extend(b"GIMP");
    out.extend(spacing.to_be_bytes());
    out.extend(name);
    out.push(0);
    out.extend(&tip.alpha);
    Ok(out)
}

/// A round tip as `.vbr` (version 1.0).
pub fn write_vbr(preset: &BrushPreset) -> Vec<u8> {
    let s = &preset.settings;
    format!(
        "GIMP-VBR\n1.0\n{}\n{:.6}\n{:.6}\n{:.6}\n{:.6}\n{:.6}\n",
        preset.name().replace('\n', " "),
        (s.spacing * 100.0).clamp(1.0, 1000.0),
        (s.size / 2.0).max(0.5),
        s.hardness.clamp(0.0, 1.0),
        (1.0 / preset.thickness.clamp(0.05, 1.0)).min(20.0),
        preset.angle
    )
    .into_bytes()
}

/// The GIMP format a preset is written in: `.gbr` with a bitmap tip, else `.vbr`.
pub fn write(preset: &BrushPreset) -> Result<(&'static str, Vec<u8>), BrushIoError> {
    if preset.tip.is_some() {
        Ok(("gbr", write_gbr(preset)?))
    } else {
        Ok(("vbr", write_vbr(preset)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tip() -> BrushTip {
        BrushTip::new(3, 2, vec![0, 128, 255, 10, 20, 30]).unwrap()
    }

    #[test]
    fn gbr_round_trip() {
        let mut p = preset("Grain", Some(tip()), 3.0, 40.0);
        p.settings.spacing = 0.4;
        let bytes = write_gbr(&p).unwrap();
        assert_eq!(&bytes[20..24], b"GIMP");
        let back = read_gbr(&bytes).unwrap();
        let q = &back.presets[0];
        assert_eq!(q.name(), "Grain");
        assert_eq!(q.tip, Some(tip()));
        assert!((q.settings.spacing - 0.4).abs() < 1e-6);
        assert_eq!(q.settings.size, 3.0);
        assert!(back.warnings.is_empty());
    }

    #[test]
    fn reads_version_1_colour_and_hoses() {
        // Version 1: no magic or spacing; colour pixels give their alpha.
        let mut v1 = Vec::new();
        for v in [20 + 3, 1, 2, 1, 4] {
            v1.extend((v as u32).to_be_bytes());
        }
        v1.extend(b"ab\0");
        v1.extend([9, 9, 9, 200, 1, 1, 1, 50]);
        let r = read_gbr(&v1).unwrap();
        assert_eq!(r.presets[0].tip.as_ref().unwrap().alpha, [200, 50]);
        assert_eq!(r.warnings.len(), 1);

        let cell = write_gbr(&preset("cell", Some(tip()), 3.0, 20.0)).unwrap();
        let mut gih = b"Hose\n2 ncells:2 cellwidth:3 cellheight:2 dim:1 rank0:2 sel0:random\n".to_vec();
        gih.extend(&cell);
        gih.extend(&cell);
        let r = read_gih(&gih).unwrap();
        assert_eq!(r.presets[0].name(), "Hose");
        assert_eq!(r.presets[0].tip, Some(tip()));
        assert!(r.warnings[0].contains("2 tips"));
        assert!(read_gbr(b"junk").is_err());
    }

    #[test]
    fn vbr_round_trip() {
        let mut p = preset("Soft", None, 30.0, 10.0);
        p.settings.hardness = 0.25;
        p.thickness = 0.5;
        p.angle = 30.0;
        let back = &read_vbr(&write_vbr(&p)).unwrap().presets[0];
        assert_eq!(back.name(), "Soft");
        assert!((back.settings.size - 30.0).abs() < 1e-3 && (back.settings.hardness - 0.25).abs() < 1e-5);
        assert!((back.thickness - 0.5).abs() < 1e-5 && (back.angle - 30.0).abs() < 1e-4);
        let v15 = b"GIMP-VBR\n1.5\nStar\nsquare\n20\n5\n4\n1\n1\n0\n";
        let r = read_vbr(v15).unwrap();
        assert_eq!(r.presets[0].settings.size, 10.0);
        assert_eq!(r.warnings.len(), 1);
    }
}
