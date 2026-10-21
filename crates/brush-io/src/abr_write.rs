//! Writes Photoshop brushes (`.abr`, version 6.2): the layout [`crate::abr`]
//! reads, as GIMP and Krita read it too.
//!
//! - `samp`: each sampled tip (and dual-brush tip) as 8-bit raw coverage;
//! - `patt`: each texture as an 8-bit grayscale pattern;
//! - `desc`: one `brushPreset` per brush with its tip (`computedBrush` for
//!   round tips, `sampledBrush` otherwise: diameter, hardness, angle,
//!   roundness, spacing, anti-aliasing), shape dynamics (size from pressure or
//!   tilt with the minimum diameter, jitter; angle and roundness), transfer
//!   (opacity from pressure / tilt, jitter), texture and dual brush.
//!
//! What Photoshop brushes can't hold is left out: Painter's stabilisation,
//! the scatter and colour-jitter settings, the starting taper and pressure
//! curves (Photoshop's controls are on / off with a fixed response).

use crate::descriptor::build::{descriptor, unicode, V};
use crate::types::{BrushIoError, BrushPreset, BrushTip};

/// Photoshop's "control" values for dynamics (`bVTy`).
const CONTROL_OFF: i32 = 0;
const CONTROL_PEN_PRESSURE: i32 = 2;
const CONTROL_PEN_TILT: i32 = 3;
const CONTROL_DIRECTION: i32 = 6;

/// A stable id for a bitmap: a UUID-shaped hex digest of its pixels.
fn bitmap_id(tip: &BrushTip) -> String {
    let mut h: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    for &b in tip.alpha.iter().chain(&tip.width.to_le_bytes()).chain(&tip.height.to_le_bytes()) {
        h = (h ^ b as u128).wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013B);
    }
    let s = format!("{h:032x}");
    format!("{}-{}-{}-{}-{}", &s[..8], &s[8..12], &s[12..16], &s[16..20], &s[20..])
}

fn pad4(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

fn section(out: &mut Vec<u8>, key: &[u8; 4], body: &[u8]) {
    out.extend(b"8BIM");
    out.extend(key);
    out.extend((body.len() as u32).to_be_bytes());
    out.extend(body);
}

/// One `samp` entry: id, the 264-byte v6.2 sample header, bounds, depth 8, raw.
fn sample(id: &str, tip: &BrushTip) -> Vec<u8> {
    let mut s = vec![id.len() as u8];
    s.extend(id.as_bytes());
    s.extend([0u8; 264]);
    for v in [0i32, 0, tip.height as i32, tip.width as i32] {
        s.extend(v.to_be_bytes());
    }
    s.extend(8u16.to_be_bytes());
    s.push(0); // raw
    s.extend(&tip.alpha);
    let mut out = (s.len() as u32).to_be_bytes().to_vec();
    out.extend(s);
    pad4(&mut out);
    out
}

/// One `patt` entry: an 8-bit grayscale pattern in a virtual memory array list.
fn pattern(id: &str, name: &str, image: &BrushTip) -> Vec<u8> {
    let (w, h) = (image.width, image.height);
    let mut p = Vec::new();
    p.extend(1u32.to_be_bytes()); // version
    p.extend(1u32.to_be_bytes()); // grayscale
    p.extend((h as u16).to_be_bytes());
    p.extend((w as u16).to_be_bytes());
    unicode(&mut p, name);
    p.push(id.len() as u8);
    p.extend(id.as_bytes());
    // The channel: written flag, length, depth, rect, depth, compression, data.
    let mut channel = Vec::new();
    channel.extend(8u32.to_be_bytes());
    for v in [0i32, 0, h as i32, w as i32] {
        channel.extend(v.to_be_bytes());
    }
    channel.extend(8u16.to_be_bytes());
    channel.push(0);
    channel.extend(&image.alpha);
    let mut list = Vec::new();
    for v in [0u32, 0, h, w] {
        list.extend(v.to_be_bytes());
    }
    list.extend(24u32.to_be_bytes()); // channel slots
    list.extend(1u32.to_be_bytes());
    list.extend((channel.len() as u32).to_be_bytes());
    list.extend(channel);
    // The other 23 colour slots, the user mask and the sheet mask: not written.
    for _ in 0..25 {
        list.extend(0u32.to_be_bytes());
    }
    p.extend(3u32.to_be_bytes()); // list version
    p.extend((list.len() as u32).to_be_bytes());
    p.extend(list);
    let mut out = (p.len() as u32).to_be_bytes().to_vec();
    out.extend(p);
    pad4(&mut out);
    out
}

/// A dynamics block (`brVr`): the control and the jitter.
fn variation(control: i32, jitter: f32) -> V<'static> {
    V::Obj("brVr", vec![("bVTy", V::Long(control)), ("fStp", V::Long(25)), ("jitter", V::Unit("#Prc", (jitter * 100.0) as f64))])
}

/// The control a dynamic's inputs map to: pressure first, then tilt.
fn control(d: &painter_core::brush::InputDynamics) -> i32 {
    if d.pressure.is_some() {
        CONTROL_PEN_PRESSURE
    } else if d.tilt.is_some() {
        CONTROL_PEN_TILT
    } else {
        CONTROL_OFF
    }
}

/// Writes `presets` as a version 6.2 `.abr`.
pub fn write(presets: &[BrushPreset]) -> Result<Vec<u8>, BrushIoError> {
    if presets.is_empty() {
        return Err(BrushIoError::Unsupported("no brushes to export".into()));
    }
    let mut samp = Vec::new();
    let mut patt = Vec::new();
    let mut seen_tips: Vec<String> = Vec::new();
    let mut seen_patterns: Vec<String> = Vec::new();
    let mut add_tip = |tip: &BrushTip, samp: &mut Vec<u8>| {
        let id = bitmap_id(tip);
        if !seen_tips.contains(&id) {
            samp.extend(sample(&id, tip));
            seen_tips.push(id.clone());
        }
        id
    };
    // Ids and names live as long as the descriptor is built.
    struct Entry {
        name: String,
        tip_id: Option<String>,
        texture: Option<(String, String)>,
        dual_id: Option<String>,
    }
    let mut entries = Vec::new();
    for p in presets {
        let tip_id = p.tip.as_ref().map(|t| add_tip(t, &mut samp));
        let dual_id = p.dual.as_ref().map(|d| add_tip(&d.tip, &mut samp));
        let texture = p.texture.as_ref().map(|t| {
            let id = bitmap_id(&t.image);
            let name = format!("{} texture", p.settings.name);
            if !seen_patterns.contains(&id) {
                patt.extend(pattern(&id, &name, &t.image));
                seen_patterns.push(id.clone());
            }
            (id, name)
        });
        entries.push(Entry { name: p.settings.name.clone(), tip_id, texture, dual_id });
    }

    let mut brushes = Vec::new();
    for (p, e) in presets.iter().zip(&entries) {
        let s = &p.settings;
        let mut tip = vec![
            ("Dmtr", V::Unit("#Pxl", s.size as f64)),
            ("Angl", V::Unit("#Ang", p.angle as f64)),
            ("Rndn", V::Unit("#Prc", (p.thickness.clamp(0.01, 1.0) * 100.0) as f64)),
            ("Spcn", V::Unit("#Prc", (s.spacing * 100.0).clamp(1.0, 1000.0) as f64)),
            ("Intr", V::Bool(s.antialias != painter_core::AntiAlias::None)),
        ];
        let class = match &e.tip_id {
            Some(id) => {
                tip.push(("sampledData", V::Text(id)));
                "sampledBrush"
            }
            None => {
                tip.push(("Hrdn", V::Unit("#Prc", (s.hardness * 100.0) as f64)));
                "computedBrush"
            }
        };
        // From the engine's settings (what the brush paints like now, edits included).
        let mut items: Vec<(&str, V)> = vec![("Nm  ", V::Text(&e.name)), ("Brsh", V::Obj(class, tip))];
        // Shape dynamics.
        let size_control = if s.pressure_size { CONTROL_PEN_PRESSURE } else { control(&s.size_dynamics) };
        let r = &s.rotation;
        let angle_control = if r.follow_direction {
            CONTROL_DIRECTION
        } else if r.pressure.is_some() {
            CONTROL_PEN_PRESSURE
        } else if r.tilt.is_some() {
            CONTROL_PEN_TILT
        } else {
            CONTROL_OFF
        };
        let minimum = if s.pressure_size { s.min_size_ratio } else { 0.0 };
        items.push(("useTipDynamics", V::Bool(true)));
        items.push(("szVr", variation(size_control, s.size_dynamics.jitter)));
        items.push(("minimumDiameter", V::Unit("#Prc", (minimum.clamp(0.0, 1.0) * 100.0) as f64)));
        items.push(("angleDynamics", variation(angle_control, r.jitter)));
        items.push(("roundnessDynamics", variation(control(&s.thickness_dynamics), s.thickness_dynamics.jitter)));
        items.push(("minimumRoundness", V::Unit("#Prc", 25.0)));
        items.push(("flipX", V::Bool(r.random_flip_x)));
        items.push(("flipY", V::Bool(r.random_flip_y)));
        // Transfer: per-dab opacity.
        let flow_control = if s.pressure_density { CONTROL_PEN_PRESSURE } else { control(&s.density_dynamics) };
        items.push(("usePaintDynamics", V::Bool(true)));
        items.push(("opVr", variation(flow_control, s.density_dynamics.jitter)));
        // Texture.
        match (&e.texture, &p.texture) {
            (Some((id, name)), Some(t)) => {
                items.push(("useTexture", V::Bool(true)));
                items.push(("Txtr", V::Obj("Ptrn", vec![("Nm  ", V::Text(name)), ("Idnt", V::Text(id))])));
                items.push(("textureScale", V::Unit("#Prc", t.scale as f64)));
                items.push(("textureDepth", V::Unit("#Prc", (t.density * 100.0) as f64)));
                items.push(("InvT", V::Bool(t.invert)));
            }
            _ => items.push(("useTexture", V::Bool(false))),
        }
        // Dual brush.
        match (&e.dual_id, &p.dual) {
            (Some(id), Some(dual)) => {
                let dual_tip = vec![
                    ("Dmtr", V::Unit("#Pxl", (s.size * dual.scale) as f64)),
                    ("Spcn", V::Unit("#Prc", (s.spacing * 100.0).clamp(1.0, 1000.0) as f64)),
                    ("sampledData", V::Text(id)),
                ];
                items.push(("useDualBrush", V::Bool(true)));
                items.push(("dualBrush", V::Obj("dualBrush", vec![("Brsh", V::Obj("sampledBrush", dual_tip))])));
            }
            _ => items.push(("useDualBrush", V::Bool(false))),
        }
        brushes.push(V::Obj("brushPreset", items));
    }
    let mut desc = 16u32.to_be_bytes().to_vec();
    descriptor(&mut desc, "null", &[("Brsh", V::List(brushes))]);

    let mut out = Vec::new();
    out.extend(6u16.to_be_bytes());
    out.extend(2u16.to_be_bytes());
    section(&mut out, b"samp", &samp);
    if !patt.is_empty() {
        section(&mut out, b"patt", &patt);
    }
    section(&mut out, b"desc", &desc);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BrushTexture, DualBrush};
    use painter_core::{BrushSettings, PressureCurve};

    fn tip(w: u32, h: u32, seed: u8) -> BrushTip {
        BrushTip::new(w, h, (0..w * h).map(|i| (i as u8).wrapping_mul(37).wrapping_add(seed)).collect()).unwrap()
    }

    #[test]
    fn round_trips_through_the_reader() {
        // A round pressure pen and a textured, dual, sampled brush.
        let mut pen = BrushPreset::new(BrushSettings { name: "Pen".into(), size: 14.0, hardness: 0.8, ..BrushSettings::g_pen() });
        pen.settings.min_size_ratio = 0.3;
        let mut chalk = BrushPreset::new(BrushSettings {
            name: "Chalk".into(),
            size: 60.0,
            spacing: 0.25,
            pressure_size: false,
            ..BrushSettings::g_pen()
        });
        chalk.tip = Some(tip(9, 7, 3));
        chalk.angle = 30.0;
        chalk.thickness = 0.5;
        chalk.texture = Some(BrushTexture { image: tip(5, 4, 9), scale: 150.0, density: 0.4, invert: true });
        chalk.dual = Some(DualBrush { tip: tip(6, 6, 1), scale: 0.5 });
        chalk.dynamics.rotation_follows_direction = true;
        chalk.dynamics.flow.pressure = Some(PressureCurve::linear());
        chalk.dynamics.size.random = Some(0.2);
        chalk.apply_dynamics();
        chalk.apply_tip();

        let bytes = write(&[pen.clone(), chalk.clone()]).unwrap();
        let report = crate::abr::read_report(&bytes).unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        let [p, c] = &report.presets[..] else { panic!("two brushes") };
        assert_eq!((p.settings.name.as_str(), p.settings.size), ("Pen", 14.0));
        assert!((p.settings.hardness - 0.8).abs() < 1e-4 && p.tip.is_none());
        assert!(p.settings.pressure_size && (p.settings.min_size_ratio - 0.3).abs() < 1e-4);
        assert_eq!(c.tip, chalk.tip);
        assert!((c.angle - 30.0).abs() < 1e-4 && (c.thickness - 0.5).abs() < 1e-4);
        assert!((c.settings.spacing - 0.25).abs() < 1e-4 && !c.settings.pressure_size);
        let t = c.texture.as_ref().unwrap();
        assert_eq!(t.image, chalk.texture.as_ref().unwrap().image);
        assert!(t.invert && (t.scale - 150.0).abs() < 1e-3 && (t.density - 0.4).abs() < 1e-4);
        let dual = c.dual.as_ref().unwrap();
        assert_eq!(dual.tip, chalk.dual.as_ref().unwrap().tip);
        assert!((dual.scale - 0.5).abs() < 1e-4);
        assert!(c.dynamics.rotation_follows_direction && c.settings.pressure_density);
        assert!((c.dynamics.size.random.unwrap() - 0.2).abs() < 1e-4);
        assert!(write(&[]).is_err());
    }
}
