//! MyPaint brushes (`.myb`, version 3 JSON, also used by Krita's MyPaint
//! engine and Drawpile): `settings` maps each libmypaint setting to a
//! `base_value` plus `inputs`, piecewise-linear offsets driven by the pen
//! (`"pressure": [[x, y], …]`, speed, tilt, direction, random…).
//!
//! libmypaint is a different engine (dabs per radius, logarithmic radius,
//! opacity linearised over overlapping dabs, smudging), so reading maps the
//! settings Painter has an equivalent for: radius and its pressure curve,
//! opacity and its pressure curve (through libmypaint's linearisation),
//! hardness, dab spacing, random offset and radius, elliptical dabs and their
//! angle (following the stroke direction), eraser and slow tracking. The
//! whole file is kept, so an unedited brush exports back unchanged and an
//! edited one keeps the settings Painter doesn't have.

use painter_core::{BrushMode, BrushSettings, PressureCurve};
use serde_json::{json, Map, Value};

use crate::types::{BrushIoError, BrushPreset, ImportReport, PresetSource};

pub const EXTENSION: &str = "myb";

fn setting<'a>(settings: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    settings.get(key)
}

fn base(settings: &Map<String, Value>, key: &str, default: f32) -> f32 {
    setting(settings, key).and_then(|s| s.get("base_value")).and_then(Value::as_f64).map_or(default, |v| v as f32)
}

/// An input's mapping points, if the setting uses it.
fn input(settings: &Map<String, Value>, key: &str, name: &str) -> Option<Vec<[f32; 2]>> {
    let points = setting(settings, key)?.get("inputs")?.get(name)?.as_array()?;
    let pts: Vec<[f32; 2]> = points
        .iter()
        .filter_map(|p| {
            let p = p.as_array()?;
            Some([p.first()?.as_f64()? as f32, p.get(1)?.as_f64()? as f32])
        })
        .collect();
    (pts.len() >= 2).then_some(pts)
}

/// Piecewise-linear lookup, flat beyond the ends (as libmypaint does).
fn lookup(points: &[[f32; 2]], x: f32) -> f32 {
    if x <= points[0][0] {
        return points[0][1];
    }
    for w in points.windows(2) {
        let ([x0, y0], [x1, y1]) = (w[0], w[1]);
        if x <= x1 {
            return if x1 > x0 { y0 + (y1 - y0) * (x - x0) / (x1 - x0) } else { y1 };
        }
    }
    points[points.len() - 1][1]
}

const SAMPLES: usize = 16;

/// A curve through `f` sampled over pressure 0..=1, normalised by its
/// maximum; also returns that maximum.
fn sampled(f: impl Fn(f32) -> f32) -> (PressureCurve, f32) {
    let ys: Vec<f32> = (0..=SAMPLES).map(|i| f(i as f32 / SAMPLES as f32).max(0.0)).collect();
    let max = ys.iter().copied().fold(0.0f32, f32::max);
    let pts = ys.iter().enumerate().map(|(i, y)| [i as f32 / SAMPLES as f32, if max > 0.0 { y / max } else { 0.0 }]).collect();
    (PressureCurve::from_points(pts), max)
}

/// libmypaint's dabs per pixel along a stroke, after `opaque_linearize`.
fn dabs_per_pixel(s: &Map<String, Value>) -> f32 {
    let dpp = (base(s, "dabs_per_actual_radius", 2.0) + base(s, "dabs_per_basic_radius", 0.0)) * 2.0;
    1.0 + base(s, "opaque_linearize", 0.9) * (dpp.max(1.0) - 1.0)
}

/// Reads a `.myb`; `name` is the brush's name (MyPaint names brushes by file).
pub fn read(bytes: &[u8], name: &str) -> Result<ImportReport, BrushIoError> {
    let root: Value = serde_json::from_slice(bytes).map_err(|_| {
        BrushIoError::Unsupported("MyPaint brush in the old text format (open and save it in MyPaint 1.x or later first)".into())
    })?;
    let version = root.get("version").and_then(Value::as_u64).unwrap_or(0);
    if version != 3 {
        return Err(BrushIoError::Unsupported(format!("MyPaint brush version {version}")));
    }
    let s = root.get("settings").and_then(Value::as_object).ok_or_else(|| BrushIoError::Corrupt("no settings".into()))?;
    let mut warnings = Vec::new();
    let mut settings = BrushSettings::g_pen();
    settings.name = name.to_string();

    // Radius: base and pressure offsets are in natural-log space.
    let log_r = base(s, "radius_logarithmic", 2.0);
    let size_pressure = input(s, "radius_logarithmic", "pressure");
    let (size_curve, r_max) = sampled(|p| (log_r + size_pressure.as_ref().map_or(0.0, |pts| lookup(pts, p))).exp());
    settings.size = (2.0 * r_max).max(0.5);
    settings.pressure_size = size_pressure.is_some();
    settings.min_size_ratio = 0.0;
    settings.size_curve = if settings.pressure_size { size_curve } else { PressureCurve::linear() };

    // Opacity: opaque × opaque_multiply, spread over the overlapping dabs.
    let (o, o_in) = (base(s, "opaque", 1.0), input(s, "opaque", "pressure"));
    let has_multiply = setting(s, "opaque_multiply").is_some();
    let (m, m_in) = (base(s, "opaque_multiply", 0.0), input(s, "opaque_multiply", "pressure"));
    let dpp = dabs_per_pixel(s);
    let alpha = |p: f32| {
        let o = o + o_in.as_ref().map_or(0.0, |pts| lookup(pts, p));
        let m = if has_multiply { m + m_in.as_ref().map_or(0.0, |pts| lookup(pts, p)) } else { 1.0 };
        let a = (o * m).clamp(0.0, 1.0);
        1.0 - (1.0 - a).powf(1.0 / dpp)
    };
    let (density_curve, dab_alpha) = sampled(alpha);
    settings.opacity = 1.0;
    settings.density = dab_alpha.clamp(0.01, 1.0);
    settings.pressure_density = o_in.is_some() || m_in.is_some();
    settings.density_curve = if settings.pressure_density { density_curve } else { PressureCurve::linear() };

    settings.hardness = base(s, "hardness", 0.8).clamp(0.0, 1.0);
    let per_radius = base(s, "dabs_per_actual_radius", 2.0) + base(s, "dabs_per_basic_radius", 0.0);
    settings.spacing = (1.0 / (2.0 * per_radius.max(0.05))).clamp(0.01, 10.0);
    settings.stabilization = (base(s, "slow_tracking", 0.0) * 10.0).round().clamp(0.0, 100.0) as u32;
    if base(s, "eraser", 0.0) > 0.5 {
        settings.mode = BrushMode::Erase;
    }
    let offset = base(s, "offset_by_random", 0.0);
    if offset > 0.0 {
        settings.scatter.amount = offset / 2.0;
        settings.scatter.count = 1;
        settings.scatter.both_axes = true;
    }
    let mut preset = BrushPreset::new(settings);
    let radius_random = base(s, "radius_by_random", 0.0);
    if radius_random > 0.0 {
        preset.dynamics.size.random = Some((1.0 - (-2.0 * radius_random).exp()).clamp(0.0, 1.0));
    }
    if preset.settings.pressure_size {
        preset.dynamics.size.pressure = Some(preset.settings.size_curve.clone());
        preset.dynamics.size.minimum = Some(0.0);
    }
    if preset.settings.pressure_density {
        preset.dynamics.flow.pressure = Some(preset.settings.density_curve.clone());
    }
    preset.thickness = (1.0 / base(s, "elliptical_dab_ratio", 1.0).max(1.0)).clamp(0.01, 1.0);
    preset.angle = base(s, "elliptical_dab_angle", 90.0);
    preset.dynamics.rotation_follows_direction = input(s, "elliptical_dab_angle", "direction").is_some();
    preset.source = PresetSource::MyPaint;
    preset.apply_dynamics();
    preset.apply_tip();

    if base(s, "smudge", 0.0) > 0.0
        || setting(s, "smudge").and_then(|v| v.get("inputs")).and_then(Value::as_object).is_some_and(|i| !i.is_empty())
    {
        warnings.push(format!("“{name}”: smudging isn't supported; the brush paints its colour"));
    }
    if ["change_color_h", "change_color_l", "change_color_hsv_s", "change_color_v", "change_color_hsl_s"]
        .iter()
        .any(|k| base(s, k, 0.0) != 0.0)
    {
        warnings.push(format!("“{name}”: colour changes along the stroke aren't supported"));
    }
    preset.keep_original("mypaint", bytes);
    Ok(ImportReport { presets: vec![preset], warnings, from_cache: false })
}

fn set(s: &mut Map<String, Value>, key: &str, base: f32, inputs: Value) {
    s.insert(key.into(), json!({ "base_value": base, "inputs": inputs }));
}

/// Points of `curve` mapped through `f`, at most eight (libmypaint's limit).
fn points(curve: &PressureCurve, f: impl Fn(f32) -> f32) -> Value {
    let pts: Vec<Value> = (0..=6).map(|i| i as f32 / 6.0).map(|x| json!([x, f(curve.eval(x))])).collect();
    Value::Array(pts)
}

/// The preset as a `.myb`. An unedited MyPaint brush comes back as it was
/// read; an edited one keeps the settings Painter has no equivalent for.
pub fn write(preset: &BrushPreset) -> Result<Vec<u8>, BrushIoError> {
    if let Some(bytes) = preset.unchanged_original("mypaint") {
        return Ok(bytes);
    }
    let mut root: Value = preset.original("mypaint").and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(
        || json!({ "comment": "MyPaint brush file", "group": "", "parent_brush_name": "", "settings": {}, "version": 3 }),
    );
    let st = &preset.settings;
    let s = root.get_mut("settings").and_then(Value::as_object_mut).ok_or_else(|| BrushIoError::Corrupt("no settings".into()))?;
    let log_r = (st.size / 2.0).max(0.2).ln();
    let size_inputs = if st.pressure_size {
        let min = st.min_size_ratio;
        json!({ "pressure": points(&st.size_curve, |c| (min + (1.0 - min) * c).max(0.01).ln()) })
    } else {
        json!({})
    };
    set(s, "radius_logarithmic", log_r, size_inputs);
    // Dab alpha → libmypaint's stroke opacity (undoing its linearisation).
    let dpp = {
        let per_radius = 1.0 / (2.0 * st.spacing.max(0.01));
        1.0 + 0.9 * (per_radius * 2.0 - 1.0).max(0.0)
    };
    let stroke = |dab: f32| 1.0 - (1.0 - dab.clamp(0.0, 0.999)).powf(dpp);
    let opaque = stroke(st.density) * st.opacity;
    set(s, "opaque", opaque, json!({}));
    if st.pressure_density {
        let full = stroke(st.density).max(1e-4);
        set(s, "opaque_multiply", 0.0, json!({ "pressure": points(&st.density_curve, |c| stroke(st.density * c) / full) }));
    } else {
        set(s, "opaque_multiply", 1.0, json!({}));
    }
    set(s, "opaque_linearize", 0.9, json!({}));
    set(s, "hardness", st.hardness, json!({}));
    set(s, "dabs_per_actual_radius", 1.0 / (2.0 * st.spacing.max(0.01)), json!({}));
    set(s, "dabs_per_basic_radius", 0.0, json!({}));
    set(s, "dabs_per_second", 0.0, json!({}));
    set(s, "offset_by_random", st.scatter.amount * 2.0, json!({}));
    let random = preset.dynamics.size.random.unwrap_or(st.size_dynamics.jitter);
    set(s, "radius_by_random", if random > 0.0 { -(1.0 - random.min(0.99)).ln() / 2.0 } else { 0.0 }, json!({}));
    set(s, "elliptical_dab_ratio", 1.0 / preset.thickness.clamp(0.05, 1.0), json!({}));
    let direction = if st.rotation.follow_direction { json!({ "direction": [[0.0, 0.0], [180.0, 180.0]] }) } else { json!({}) };
    set(s, "elliptical_dab_angle", if st.rotation.follow_direction { 0.0 } else { preset.angle }, direction);
    set(s, "eraser", if st.mode == BrushMode::Erase { 1.0 } else { 0.0 }, json!({}));
    set(s, "slow_tracking", st.stabilization as f32 / 10.0, json!({}));
    if !s.contains_key("anti_aliasing") {
        set(s, "anti_aliasing", 1.0, json!({}));
    }
    Ok(serde_json::to_vec_pretty(&root)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEN: &str = r#"{
      "comment": "MyPaint brush file", "group": "", "parent_brush_name": "classic/pen", "version": 3,
      "settings": {
        "radius_logarithmic": {"base_value": 1.0, "inputs": {"pressure": [[0, -1.0], [1, 0.0]]}},
        "opaque": {"base_value": 1.0, "inputs": {}},
        "opaque_multiply": {"base_value": 0.0, "inputs": {"pressure": [[0, 0], [1, 1]]}},
        "hardness": {"base_value": 0.9, "inputs": {}},
        "dabs_per_actual_radius": {"base_value": 2.5, "inputs": {}},
        "elliptical_dab_ratio": {"base_value": 2.0, "inputs": {}},
        "elliptical_dab_angle": {"base_value": 45.0, "inputs": {}},
        "slow_tracking": {"base_value": 2.0, "inputs": {}},
        "smudge": {"base_value": 0.5, "inputs": {}},
        "custom_input": {"base_value": 0.3, "inputs": {}}
      }
    }"#;

    #[test]
    fn reads_the_engine_settings() {
        let r = read(PEN.as_bytes(), "Pen").unwrap();
        let p = &r.presets[0];
        let s = &p.settings;
        assert_eq!(s.name, "Pen");
        assert!((s.size - 2.0 * 1f32.exp()).abs() < 1e-3, "{}", s.size);
        assert!(s.pressure_size);
        assert!((s.size_curve.eval(0.0) - (-1f32).exp()).abs() < 1e-3, "radius e^0 at no pressure");
        assert!(s.pressure_density && s.density_curve.eval(0.0) < 0.01);
        assert!((s.spacing - 0.2).abs() < 1e-5);
        assert!((s.hardness - 0.9).abs() < 1e-6);
        assert!((p.thickness - 0.5).abs() < 1e-6 && p.angle == 45.0);
        assert_eq!(s.stabilization, 20);
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert_eq!(p.source, PresetSource::MyPaint);
    }

    #[test]
    fn unedited_brushes_round_trip_exactly_and_edits_keep_the_rest() {
        let p = read(PEN.as_bytes(), "Pen").unwrap().presets.remove(0);
        assert_eq!(write(&p).unwrap(), PEN.as_bytes());
        let mut edited = p.clone();
        edited.settings.hardness = 0.3;
        edited.settings.size = 40.0;
        let out = write(&edited).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["settings"]["custom_input"]["base_value"], 0.3, "unknown settings kept");
        assert_eq!(v["parent_brush_name"], "classic/pen");
        let back = &read(&out, "Pen").unwrap().presets[0];
        assert!((back.settings.hardness - 0.3).abs() < 1e-6);
        assert!((back.settings.size - 40.0).abs() < 0.05, "{}", back.settings.size);
        assert!((back.settings.spacing - p.settings.spacing).abs() < 1e-4);
        assert!((back.settings.density - p.settings.density).abs() < 0.02, "{} vs {}", back.settings.density, p.settings.density);
    }

    #[test]
    fn writes_new_brushes() {
        let mut s = BrushSettings::g_pen();
        s.name = "Ink".into();
        s.mode = BrushMode::Erase;
        let out = write(&BrushPreset::new(s.clone())).unwrap();
        let back = &read(&out, "Ink").unwrap().presets[0];
        assert_eq!(back.settings.mode, BrushMode::Erase);
        assert!((back.settings.size - s.size).abs() < 0.05);
        assert!(back.settings.pressure_size);
        // G-Pen: 10 % of the size at no pressure.
        assert!((back.settings.size_curve.eval(0.0) - 0.1).abs() < 0.01);
        assert!(read(b"version 2\nradius_logarithmic 2.0", "old").is_err());
    }
}
