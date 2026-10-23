//! Clip Studio Paint sub tools (`.sut`) and tool databases (`.todb`).
//!
//! Both are SQLite databases. Layout as observed in Clip Studio Paint 1.5 data:
//!
//! - `Node`: the tool tree. `NodeName` is the sub tool name and
//!   `NodeVariantID` points at its parameters.
//! - `Variant`: one row of named parameter columns per sub tool (`BrushSize`,
//!   `BrushFlow`, `BrushHardness`, `BrushInterval`, `AntiAlias`,
//!   `FlickerReduction`, `CompositeMode`, ...). Brushes are the rows with a
//!   `BrushSize`. Columns differ between versions, so they are discovered at runtime.
//! - `*Effector` BLOBs hold dynamics: a 48-byte header of big-endian words
//!   (word 2 is the enabled-source mask, words 8 and 9 are the byte lengths of
//!   two curve blocks that follow), then curve blocks `[12, n, 16]` + n points
//!   of big-endian `f64` (x, y). Bit `0x10` with the first curve is pen
//!   pressure. The second curve and the other bits aren't mapped (their meaning
//!   isn't confirmed); they're kept in `extras`.
//! - `CompositeMode = 27` marks erasers.
//! - Tip and texture images are references to installed materials
//!   (`…:PaintNNN:<uuid>:data:material.layer`). Clip Studio's `material.layer`
//!   payload is an encrypted container and is deliberately not read; instead
//!   the material folder's plain `thumbnail/thumbnail.png` is used as an
//!   approximate tip (dark = ink) or texture (luminance), via
//!   [`MaterialLibrary`]. A `.sut` export may
//!   embed them in a `MaterialFile` table; embedded PNGs are decoded, other
//!   material encodings are kept as references.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use painter_core::{AntiAlias, BrushMode, BrushSettings, PressureCurve, SmoothingMethod};
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags};

use crate::types::{BrushIoError, BrushPreset, BrushTexture, BrushTip, ImportReport, PresetSource};

/// `CompositeMode` value Clip Studio uses for erasers.
const COMPOSITE_ERASE: i64 = 27;
/// Effector enabled-mask bit for pen pressure (first curve block).
const EFFECT_PRESSURE: u32 = 0x10;

/// A decoded `*Effector` BLOB.
#[derive(Clone, Debug, PartialEq)]
pub struct Effector {
    /// Header words (big-endian `i32`).
    pub header: [i32; 12],
    /// Curves in file order; `curves[0]` is the pressure curve when present.
    pub curves: Vec<Option<Vec<[f64; 2]>>>,
}

impl Effector {
    pub fn enabled_mask(&self) -> u32 {
        self.header[2] as u32
    }

    /// The pressure curve, when pressure is enabled.
    pub fn pressure(&self) -> Option<PressureCurve> {
        if self.enabled_mask() & EFFECT_PRESSURE == 0 {
            return None;
        }
        let points = self.curves.first()?.as_ref()?;
        Some(PressureCurve::from_points(points.iter().map(|p| [p[0] as f32, p[1] as f32]).collect()))
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "header": self.header,
            "curves": self.curves,
        })
    }
}

pub fn parse_effector(blob: &[u8]) -> Option<Effector> {
    let word = |i: usize| blob.get(i * 4..i * 4 + 4).map(|b| i32::from_be_bytes(b.try_into().unwrap()));
    if word(0)? != 48 {
        return None;
    }
    let mut header = [0i32; 12];
    for (i, h) in header.iter_mut().enumerate() {
        *h = word(i)?;
    }
    let mut curves = Vec::new();
    let mut offset = 48usize;
    for len in [header[8], header[9]] {
        let len = usize::try_from(len).ok()?;
        if len == 0 {
            curves.push(None);
            continue;
        }
        let block = blob.get(offset..offset + len)?;
        let be = |i: usize| block.get(i..i + 4).map(|b| u32::from_be_bytes(b.try_into().unwrap()) as usize);
        let (head, count, stride) = (be(0)?, be(4)?, be(8)?);
        if stride < 16 || head + count * stride > len {
            return None;
        }
        let f = |i: usize| block.get(i..i + 8).map(|b| f64::from_be_bytes(b.try_into().unwrap()));
        let points = (0..count).map(|i| Some([f(head + i * stride)?, f(head + i * stride + 8)?])).collect::<Option<Vec<_>>>()?;
        curves.push(Some(points));
        offset += len;
    }
    Some(Effector { header, curves })
}

type Row = HashMap<String, Value>;

fn int(row: &Row, key: &str) -> Option<i64> {
    match row.get(key)? {
        Value::Integer(v) => Some(*v),
        Value::Real(v) => Some(*v as i64),
        _ => None,
    }
}

fn real(row: &Row, key: &str) -> Option<f64> {
    match row.get(key)? {
        Value::Integer(v) => Some(*v as f64),
        Value::Real(v) => Some(*v),
        _ => None,
    }
}

fn blob<'a>(row: &'a Row, key: &str) -> Option<&'a [u8]> {
    match row.get(key)? {
        Value::Blob(b) => Some(b),
        _ => None,
    }
}

/// Material references in a pattern/texture array BLOB:
/// `[u32 BE 8][u32 BE count]`, then per entry `[u32 BE entry length]
/// [u32 BE string bytes][UTF-16LE path]...`. Falls back to scanning for
/// ASCII UTF-16LE runs if the structure doesn't fit.
fn material_refs(bytes: &[u8]) -> Vec<String> {
    let be = |i: usize| bytes.get(i..i + 4).map(|b| u32::from_be_bytes(b.try_into().unwrap()) as usize);
    let structured = (|| {
        let count = be(4)?;
        let mut pos = be(0)?;
        let mut out = Vec::new();
        for _ in 0..count {
            let (entry, text) = (be(pos)?, be(pos + 4)?);
            let units: Vec<u16> =
                bytes.get(pos + 8..pos + 8 + text)?.as_chunks::<2>().0.iter().map(|p| u16::from_le_bytes(*p)).collect();
            out.push(String::from_utf16(&units).ok()?);
            pos += entry.max(8 + text);
        }
        Some(out)
    })();
    if let Some(refs) = structured.filter(|r| !r.is_empty()) {
        return refs;
    }
    let mut out = Vec::new();
    let mut current = String::new();
    for pair in bytes.as_chunks::<2>().0 {
        match (pair[1], pair[0]) {
            (0, b) if b.is_ascii_graphic() => current.push(b as char),
            _ => {
                if current.len() >= 8 {
                    out.push(std::mem::take(&mut current));
                }
                current.clear();
            }
        }
    }
    if current.len() >= 8 {
        out.push(current);
    }
    out
}

fn table_columns(db: &Connection, table: &str) -> Result<HashSet<String>, BrushIoError> {
    let mut st = db.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let columns = st.query_map([], |r| r.get::<_, String>(1))?.collect::<Result<_, _>>()?;
    Ok(columns)
}

/// Where Clip Studio keeps installed materials (brush tips and textures).
#[derive(Clone, Debug, Default)]
pub struct MaterialLibrary {
    /// `…\Material\Install` folders holding `PaintNNN\<uuid>\` material folders.
    pub roots: Vec<PathBuf>,
}

impl MaterialLibrary {
    /// This user's Clip Studio material folder (`%APPDATA%\CELSYSUserData\CELSYS\CLIPStudioCommon\Material\Install`).
    pub fn for_current_user() -> Self {
        let roots = std::env::var_os("APPDATA")
            .map(|appdata| PathBuf::from(appdata).join(r"CELSYSUserData\CELSYS\CLIPStudioCommon\Material\Install"))
            .filter(|p| p.is_dir())
            .into_iter()
            .collect();
        Self { roots }
    }

    /// Adds every material folder found under `dir` (a few levels deep): a
    /// copy of Clip Studio's `Material\Install` folder, of a folder above it
    /// (`Material`, `CLIPStudioCommon`, …) or of single `PaintNNN` folders.
    /// For devices without Clip Studio, such as an iPad.
    pub fn add_copied(&mut self, dir: &Path) {
        fn is_category(p: &Path) -> bool {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.len() > 5 && n.starts_with("Paint") && n[5..].chars().all(|c| c.is_ascii_digit())
            })
        }
        fn walk(dir: &Path, depth: u32, roots: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            let subdirs: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect();
            if subdirs.iter().any(|p| is_category(p)) {
                if !roots.iter().any(|r| r == dir) {
                    roots.push(dir.to_path_buf());
                }
                return;
            }
            if depth > 0 {
                for sub in subdirs {
                    walk(&sub, depth - 1, roots);
                }
            }
        }
        walk(dir, 6, &mut self.roots);
    }

    /// The installed folder of a reference like `:Paint004:ad4b…dfba:data:material.layer`.
    pub fn material_dir(&self, reference: &str) -> Option<PathBuf> {
        let parts: Vec<&str> = reference.split(':').filter(|p| !p.is_empty()).collect();
        let at = parts.iter().position(|p| p.starts_with("Paint") && p[5..].chars().all(|c| c.is_ascii_digit()))?;
        let (category, uuid) = (parts[at], *parts.get(at + 1)?);
        if uuid.contains(['/', '\\', '.']) {
            return None;
        }
        self.roots.iter().map(|root| root.join(category).join(uuid)).find(|dir| dir.is_dir())
    }

    fn thumbnail(&self, reference: &str) -> Result<image::RgbaImage, String> {
        let dir = self.material_dir(reference).ok_or_else(|| format!("material {reference} isn't installed"))?;
        let path = dir.join("thumbnail").join("thumbnail.png");
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
            .map(|i| i.to_rgba8())
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The material as a tip: dark, opaque pixels are ink.
    pub fn tip(&self, reference: &str) -> Result<BrushTip, String> {
        let image = self.thumbnail(reference)?;
        let coverage = image
            .pixels()
            .map(|p| {
                let luma = (p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000;
                ((255 - luma) * p[3] as u32 / 255) as u8
            })
            .collect();
        BrushTip::new(image.width(), image.height(), coverage).map_err(|e| e.to_string())
    }

    /// The material as a texture: luminance, transparent counted as white.
    pub fn texture(&self, reference: &str) -> Result<BrushTip, String> {
        let image = self.thumbnail(reference)?;
        let gray = image
            .pixels()
            .map(|p| {
                let luma = (p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000;
                ((luma * p[3] as u32 + 255 * (255 - p[3] as u32)) / 255) as u8
            })
            .collect();
        BrushTip::new(image.width(), image.height(), gray).map_err(|e| e.to_string())
    }
}

pub fn read(path: &Path) -> Result<Vec<BrushPreset>, BrushIoError> {
    read_report(path, &MaterialLibrary::for_current_user()).map(|r| r.presets)
}

/// Reads every brush, resolving tips and textures through `library`.
pub fn read_report(path: &Path, library: &MaterialLibrary) -> Result<ImportReport, BrushIoError> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    read_db_with(&db, library)
}

pub fn read_db(db: &Connection) -> Result<Vec<BrushPreset>, BrushIoError> {
    read_db_with(db, &MaterialLibrary::default()).map(|r| r.presets)
}

/// An effector BLOB for a pressure curve: the template's header words kept
/// (their meaning isn't all known), the pressure bit and first curve replaced.
fn effector_blob(template: Option<&[u8]>, curve: Option<&[[f32; 2]]>) -> Vec<u8> {
    let old = template.and_then(parse_effector);
    let mut header = old.as_ref().map_or([48, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], |e| e.header);
    let mask = header[2] as u32;
    let mut block = Vec::new();
    match curve {
        Some(points) => {
            header[2] = (mask | EFFECT_PRESSURE) as i32;
            for v in [12u32, points.len() as u32, 16] {
                block.extend(v.to_be_bytes());
            }
            for p in points {
                block.extend((p[0] as f64).to_be_bytes());
                block.extend((p[1] as f64).to_be_bytes());
            }
        }
        None => header[2] = (mask & !EFFECT_PRESSURE) as i32,
    }
    // The second curve block, as the template had it.
    let second: Vec<u8> = match (template, old.as_ref()) {
        (Some(blob), Some(e)) if e.header[9] > 0 => {
            let start = 48 + e.header[8].max(0) as usize;
            blob.get(start..start + e.header[9] as usize).map(<[u8]>::to_vec).unwrap_or_default()
        }
        _ => Vec::new(),
    };
    header[0] = 48;
    header[8] = block.len() as i32;
    header[9] = second.len() as i32;
    let mut out: Vec<u8> = header.iter().flat_map(|w| w.to_be_bytes()).collect();
    out.extend(block);
    out.extend(second);
    out
}

/// A pressure curve with its minimum folded in: output `min + (1 − min) · curve`.
fn curve_points(curve: &PressureCurve, minimum: f32) -> Vec<[f32; 2]> {
    let min = minimum.clamp(0.0, 1.0);
    curve.points().iter().map(|p| [p[0], min + (1.0 - min) * p[1]]).collect()
}

/// Writes `preset` as a Clip Studio sub tool, using `template` (a `.sut`
/// Clip Studio exported) for everything Painter doesn't know: its tables,
/// tool type and settings are kept, and the first brush's name and brush
/// parameters are replaced. Returns notes about what couldn't be carried.
///
/// Tip and texture images can't be written: Clip Studio keeps them as
/// materials in an encrypted container, so the sub tool keeps the
/// template's tip and texture.
pub fn write_with_template(template: &Path, preset: &BrushPreset, out: &Path) -> Result<Vec<String>, BrushIoError> {
    if template == out {
        return Err(BrushIoError::Unsupported("the template and the new file must differ".into()));
    }
    std::fs::copy(template, out)?;
    let result = (|| {
        let db = Connection::open(out)?;
        let variant_columns = table_columns(&db, "Variant")?;
        if !table_columns(&db, "Node")?.contains("NodeName") || !variant_columns.contains("BrushSize") {
            return Err(BrushIoError::Corrupt("the template isn't a Clip Studio sub tool (no Node / Variant tables)".into()));
        }
        let (node, variant): (i64, i64) = db
            .query_row(
                "SELECT n.rowid, v.rowid FROM Node n JOIN Variant v ON v.VariantID = n.NodeVariantID \
                 WHERE v.BrushSize IS NOT NULL ORDER BY n.rowid LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| BrushIoError::Corrupt("the template has no brush sub tool".into()))?;
        let brushes: i64 = db.query_row(
            "SELECT COUNT(*) FROM Node n JOIN Variant v ON v.VariantID = n.NodeVariantID WHERE v.BrushSize IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        let mut notes = Vec::new();
        if brushes > 1 {
            notes.push(format!("the template has {brushes} brushes; only the first was replaced"));
        }
        db.execute("UPDATE Node SET NodeName = ?1 WHERE rowid = ?2", rusqlite::params![preset.settings.name, node])?;

        let s = &preset.settings;
        let r = &s.rotation;
        let percent = |v: f32| (v.clamp(0.0, 1.0) * 100.0).round() as i64;
        let antialias = match s.antialias {
            AntiAlias::None => 0,
            AntiAlias::Weak => 1,
            AntiAlias::Medium => 2,
            AntiAlias::Strong => 3,
        };
        let template_mode: Option<i64> = db.query_row("SELECT CompositeMode FROM Variant WHERE rowid = ?1", [variant], |r| r.get(0)).ok().flatten();
        let mode = match (s.mode, s.blend.layer_mode()) {
            (BrushMode::Erase, _) => COMPOSITE_ERASE,
            // A blend mode Clip Studio numbers the same as its layer modes.
            (_, Some(m)) if m != painter_core::BlendMode::Normal => painter_core::clip::blend_number(m),
            _ if template_mode == Some(COMPOSITE_ERASE) => 0,
            _ => template_mode.unwrap_or(0),
        };
        let mut rotation_mask: i64 = db.query_row("SELECT BrushRotationEffector FROM Variant WHERE rowid = ?1", [variant], |r| r.get(0)).ok().flatten().unwrap_or(0);
        rotation_mask &= !0xA0;
        if r.follow_direction {
            rotation_mask |= 0x20;
        }
        if r.jitter > 0.0 {
            rotation_mask |= 0x80;
        }
        let values: Vec<(&str, Value)> = vec![
            ("BrushSize", Value::Real(s.size as f64)),
            ("Opacity", Value::Integer(percent(s.opacity))),
            ("BrushFlow", Value::Integer(percent(s.density))),
            ("BrushHardness", Value::Integer(percent(s.hardness))),
            ("BrushAutoIntervalType", Value::Integer(0)),
            ("BrushInterval", Value::Real((s.spacing * 100.0) as f64)),
            ("AntiAlias", Value::Integer(antialias)),
            ("FlickerReduction", Value::Integer(s.stabilization.min(100) as i64)),
            ("CompositeMode", Value::Integer(mode)),
            ("BrushThickness", Value::Integer(percent(preset.thickness))),
            ("BrushRotation", Value::Real(preset.angle as f64)),
            ("BrushRotationEffector", Value::Integer(rotation_mask)),
            ("BrushRotationRandomScale", Value::Integer(percent(r.jitter))),
            ("BrushPatternReverseHorizontal", Value::Integer(if r.random_flip_x { 2 } else { 0 })),
            ("BrushPatternReverseVertical", Value::Integer(if r.random_flip_y { 2 } else { 0 })),
        ];
        let mut skipped = Vec::new();
        for (column, value) in values {
            if variant_columns.contains(column) {
                db.execute(&format!("UPDATE Variant SET \"{column}\" = ?1 WHERE rowid = ?2"), rusqlite::params![value, variant])?;
            } else {
                skipped.push(column);
            }
        }
        // Pressure curves: size (with its minimum), flow, opacity.
        let size_curve = s.pressure_size.then(|| curve_points(&s.size_curve, s.min_size_ratio));
        let flow_curve = s.pressure_density.then(|| curve_points(&s.density_curve, 0.0));
        let opacity_curve = s.opacity_dynamics.pressure.as_ref().map(|c| curve_points(c, 0.0));
        for (column, curve) in [("BrushSizeEffector", size_curve), ("BrushFlowEffector", flow_curve), ("BrushOpacityEffector", opacity_curve)] {
            if !variant_columns.contains(column) {
                if curve.is_some() {
                    skipped.push(column);
                }
                continue;
            }
            let old: Option<Vec<u8>> = db.query_row(&format!("SELECT \"{column}\" FROM Variant WHERE rowid = ?1"), [variant], |r| r.get(0)).ok().flatten();
            if old.is_none() && curve.is_none() {
                continue;
            }
            let blob = effector_blob(old.as_deref(), curve.as_deref());
            db.execute(&format!("UPDATE Variant SET \"{column}\" = ?1 WHERE rowid = ?2"), rusqlite::params![blob, variant])?;
        }
        if !skipped.is_empty() {
            notes.push(format!("the template's Clip Studio version has no {}", skipped.join(", ")));
        }
        if preset.tip.is_some() || preset.texture.is_some() || preset.dual.is_some() {
            notes.push("tip, texture and second brush images stay the template's (Clip Studio materials can't be written)".into());
        }
        if s.scatter.is_on() || !s.color_jitter.is_default() || s.taper_start > 0.0 {
            notes.push("spray, colour jitter and starting taper aren't carried (their Clip Studio settings aren't mapped)".into());
        }
        Ok(notes)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(out);
    }
    result
}

pub fn read_db_with(db: &Connection, library: &MaterialLibrary) -> Result<ImportReport, BrushIoError> {
    let node_columns = table_columns(db, "Node")?;
    let variant_columns = table_columns(db, "Variant")?;
    if !node_columns.contains("NodeName") || !variant_columns.contains("VariantID") || !variant_columns.contains("BrushSize") {
        return Err(BrushIoError::Corrupt("no Clip Studio sub tool tables (Node / Variant)".into()));
    }
    let materials = embedded_materials(db)?;

    let mut st = db.prepare(
        "SELECT n.NodeName AS NodeName, v.* FROM Node n JOIN Variant v ON v.VariantID = n.NodeVariantID \
         WHERE v.BrushSize IS NOT NULL ORDER BY n.rowid",
    )?;
    let names: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = st.query([])?;
    let mut presets = Vec::new();
    let mut warnings = Vec::new();
    while let Some(r) = rows.next()? {
        let mut row = Row::new();
        for (i, name) in names.iter().enumerate() {
            let value: Value = r.get(i)?;
            if value != Value::Null {
                row.insert(name.clone(), value);
            }
        }
        let mut preset = map_variant(&row, &materials, library, &mut warnings);
        preset.apply_tip();
        presets.push(preset);
    }
    Ok(ImportReport { presets, warnings, from_cache: false })
}

/// PNG images embedded anywhere in a `MaterialFile` table.
fn embedded_materials(db: &Connection) -> Result<Vec<BrushTip>, BrushIoError> {
    let exists: bool = db
        .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'MaterialFile'", [], |r| r.get::<_, i64>(0))
        .map(|n| n > 0)?;
    if !exists {
        return Ok(Vec::new());
    }
    let mut st = db.prepare("SELECT * FROM MaterialFile")?;
    let count = st.column_count();
    let mut rows = st.query([])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        for i in 0..count {
            if let Value::Blob(bytes) = r.get::<_, Value>(i)? {
                out.extend(decode_pngs(&bytes));
            }
        }
    }
    Ok(out)
}

/// Decodes every PNG found inside `bytes`, as tip coverage (alpha when present, else inverted luma).
fn decode_pngs(bytes: &[u8]) -> Vec<BrushTip> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = bytes[from..].windows(SIGNATURE.len()).position(|w| w == SIGNATURE) {
        let start = from + at;
        if let Ok(image) = image::load_from_memory_with_format(&bytes[start..], image::ImageFormat::Png) {
            let (w, h) = (image.width(), image.height());
            let coverage: Vec<u8> = if image.color().has_alpha() {
                image.to_rgba8().pixels().map(|p| p[3]).collect()
            } else {
                image.to_luma8().pixels().map(|p| 255 - p[0]).collect()
            };
            if let Ok(tip) = BrushTip::new(w, h, coverage) {
                out.push(tip);
            }
        }
        from = start + SIGNATURE.len();
    }
    out
}

fn map_variant(row: &Row, materials: &[BrushTip], library: &MaterialLibrary, warnings: &mut Vec<String>) -> BrushPreset {
    let name = match row.get("NodeName") {
        Some(Value::Text(t)) if !t.is_empty() => t.clone(),
        _ => "Clip Studio brush".into(),
    };
    let percent = |key: &str, default: f32| real(row, key).map_or(default, |v| (v as f32 / 100.0).clamp(0.0, 1.0));
    let erase = int(row, "CompositeMode") == Some(COMPOSITE_ERASE);
    let size = real(row, "BrushSize").unwrap_or(10.0) as f32;

    // Interval: explicit when auto-interval is off; otherwise an approximation
    // of Clip Studio's automatic spacing levels.
    let spacing = match int(row, "BrushAutoIntervalType") {
        Some(0) => real(row, "BrushInterval").map_or(0.1, |v| v as f32 / 100.0),
        Some(1) => 0.05,
        Some(3) => 0.25,
        _ => 0.1,
    };
    let antialias = match int(row, "AntiAlias") {
        Some(0) => AntiAlias::None,
        Some(1) => AntiAlias::Weak,
        Some(3) => AntiAlias::Strong,
        _ => AntiAlias::Medium,
    };
    let settings = BrushSettings {
        name,
        mode: if erase { BrushMode::Erase } else { BrushMode::Paint },
        blend: int(row, "CompositeMode")
            .filter(|_| !erase)
            .and_then(painter_core::clip::blend_of)
            .map_or(painter_core::BrushBlend::Normal, painter_core::BrushBlend::from_layer_mode),
        size: size.clamp(0.5, 5000.0),
        min_size_ratio: 0.0,
        pressure_size: false,
        size_curve: PressureCurve::linear(),
        opacity: percent("Opacity", 1.0),
        density: percent("BrushFlow", 1.0),
        pressure_density: false,
        density_curve: PressureCurve::linear(),
        spacing: spacing.max(0.01),
        hardness: percent("BrushHardness", 1.0),
        antialias,
        stabilization: int(row, "FlickerReduction").unwrap_or(0).clamp(0, 100) as u32,
        smoothing: SmoothingMethod::Average,
        // Set from the preset's tip, texture and dual fields by `apply_tip`.
        tip: Default::default(),
        texture: None,
        dual: None,
        // Set from the preset's dynamics by `apply_dynamics`.
        ..BrushSettings::default()
    };
    let mut preset = BrushPreset::new(settings);
    preset.source = PresetSource::ClipStudio { variant_id: int(row, "VariantID").unwrap_or(0) };
    preset.thickness = percent("BrushThickness", 1.0).max(0.01);
    preset.angle = real(row, "BrushRotation").unwrap_or(0.0) as f32;

    // Dynamics: pressure curves of size, flow and opacity are mapped; every
    // effector is kept verbatim. The thickness effector is NOT mapped: on real
    // presets (Mechanical pencil) its first curve falls to 0 at high pressure,
    // which as "pressure → thickness" flattens dabs into slivers. Clip Studio
    // clearly reads it differently (its header carries extra range values whose
    // meaning isn't confirmed), so it stays in `extras` only.
    let effectors = [
        ("BrushSizeEffector", 0usize),
        ("BrushFlowEffector", 1),
        ("BrushOpacityEffector", 2),
        ("BrushThicknessEffector", 3),
    ];
    for (key, target) in effectors {
        let Some(effector) = blob(row, key).and_then(parse_effector) else {
            continue;
        };
        preset.extras.insert(format!("sut.{key}"), effector.to_json());
        let dynamic = match target {
            0 => &mut preset.dynamics.size,
            1 => &mut preset.dynamics.flow,
            2 => &mut preset.dynamics.opacity,
            _ => continue,
        };
        dynamic.pressure = effector.pressure();
    }
    // Tip direction and flips. `BrushRotationEffector` is an integer source
    // mask here; across the stock presets bit 0x80 marks the scatter brushes
    // (Noise, Gauze, Rose…), with `BrushRotationRandomScale` as the amount,
    // and bit 0x20 the flat ones (Flat marker, Flat watercolor brush), which
    // turn with the stroke. `BrushPatternReverse*` = 2 is a random flip.
    if let Some(mask) = int(row, "BrushRotationEffector") {
        if mask & 0x80 != 0 {
            let amount = int(row, "BrushRotationRandomScale").unwrap_or(100);
            preset.dynamics.rotation.random = Some((amount as f32 / 100.0).clamp(0.0, 1.0));
        }
        preset.dynamics.rotation_follows_direction = mask & 0x20 != 0;
    }
    preset.dynamics.random_flip_x = int(row, "BrushPatternReverseHorizontal") == Some(2);
    preset.dynamics.random_flip_y = int(row, "BrushPatternReverseVertical") == Some(2);
    preset.apply_dynamics();

    // Tip: an embedded image, else the installed material's thumbnail, else round.
    let tip_refs = blob(row, "BrushPatternImageArray").map(material_refs).unwrap_or_default();
    if int(row, "BrushUsePatternImage") == Some(1) && !tip_refs.is_empty() {
        preset.extras.insert("sut.tip_materials".into(), serde_json::json!(tip_refs));
        if materials.len() == 1 {
            // A single embedded material in a single-brush file is that brush's tip.
            preset.tip = Some(materials[0].clone());
            preset.extras.insert("sut.tip_source".into(), serde_json::json!("embedded"));
        } else {
            match library.tip(&tip_refs[0]) {
                Ok(tip) => {
                    preset.tip = Some(tip);
                    preset.extras.insert("sut.tip_source".into(), serde_json::json!("material thumbnail (approximate)"));
                }
                Err(err) => warnings.push(format!("\"{}\": tip {err}; using a round tip", preset.settings.name)),
            }
        }
    }
    // Texture: the installed material's thumbnail as luminance.
    if let Some(texture) = blob(row, "TextureImage").map(material_refs).filter(|t| !t.is_empty()) {
        preset.extras.insert("sut.texture_material".into(), serde_json::json!(texture));
        match library.texture(&texture[0]) {
            Ok(image) => {
                preset.texture = Some(BrushTexture {
                    image,
                    // Newer databases leave `TextureScale` NULL and store the
                    // scale in `TextureScale2` (Mechanical pencil: 20%).
                    scale: real(row, "TextureScale2").or_else(|| real(row, "TextureScale")).unwrap_or(100.0) as f32,
                    density: percent("TextureDensity", 0.5),
                    invert: int(row, "TextureReverseDensity") == Some(1),
                });
            }
            Err(err) => warnings.push(format!("\"{}\": texture {err}; painting without texture", preset.settings.name)),
        }
    }

    // Remaining brush parameters, verbatim, so nothing is lost on export.
    let mapped: HashSet<&str> = [
        "NodeName", "VariantID", "BrushSize", "Opacity", "BrushFlow", "BrushHardness", "BrushInterval",
        "BrushAutoIntervalType", "AntiAlias", "FlickerReduction", "CompositeMode", "BrushThickness", "BrushRotation",
        "BrushPatternImageArray", "TextureImage", "BrushUsePatternImage",
    ]
    .into_iter()
    .collect();
    let mut extras = BTreeMap::new();
    for (key, value) in row {
        let relevant = key.starts_with("Brush") || key.starts_with("Texture") || key.starts_with("Stickness") || key.starts_with("Flicker");
        // Blob effectors were stored parsed above; integer ones are kept here.
        let parsed_effector = key.ends_with("Effector") && matches!(value, Value::Blob(_));
        if !relevant || mapped.contains(key.as_str()) || parsed_effector {
            continue;
        }
        let json = match value {
            Value::Integer(v) => serde_json::json!(v),
            Value::Real(v) => serde_json::json!(v),
            Value::Text(t) => serde_json::json!(t),
            Value::Blob(b) => serde_json::json!(format!("<{} bytes>", b.len())),
            Value::Null => continue,
        };
        extras.insert(format!("sut.{key}"), json);
    }
    preset.extras.extend(extras);
    preset
}

#[cfg(test)]
mod tests {
    use super::*;

    /// G-pen's real `BrushSizeEffector` from Clip Studio Paint 1.5 (big-endian words).
    const G_PEN_SIZE_EFFECTOR: [i32; 46] = [
        48, 496, 48, 0, 100, 0, 0, 0, 44, 92, 150, 0, 12, 2, 16, 0, 0, 0, 0, 1072693248, 0, 1072693248, 0, 12, 5, 16, 0, 0,
        1072693248, 0, 1070557965, -156180629, 1072540727, -624722516, 1071187111, -1874167547, 1071225241, -1717986918,
        1071968777, 1327535346, 0, 0, 1072693248, 0, 0, 0,
    ];
    /// Mapping pen's: a four-point pressure curve.
    const MAPPING_PEN_SIZE_EFFECTOR: [i32; 54] = [
        48, 496, 48, 0, 100, 0, 0, 0, 76, 92, 150, 0, 12, 4, 16, 0, 0, 0, 0, 1071644672, 0, 1070557965, -156180629,
        1072540727, -624722516, 1072083167, 1796077233, 1072693248, 0, 1072693248, 0, 12, 5, 16, 0, 0, 1072693248, 0,
        1070958331, 1483715975, 1072693248, 0, 1071854387, 858993459, 0, 0, 1072273817, -1717986918, 0, 0, 1072693248, 0,
        0, 0,
    ];
    /// Calligraphy's: curve present, pressure disabled.
    const NO_PRESSURE_EFFECTOR: [i32; 23] = [48, 496, 0, 0, 0, 0, 0, 0, 44, 0, 100, 0, 12, 2, 16, 0, 0, 0, 0, 1072693248, 0, 1072693248, 0];

    fn bytes(words: &[i32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_be_bytes()).collect()
    }

    #[test]
    fn decodes_real_effectors() {
        let g = parse_effector(&bytes(&G_PEN_SIZE_EFFECTOR)).unwrap();
        assert_eq!(g.curves.len(), 2);
        assert_eq!(g.curves[0].as_ref().unwrap(), &vec![[0.0, 0.0], [1.0, 1.0]]);
        assert_eq!(g.curves[1].as_ref().unwrap().len(), 5);
        assert_eq!(g.pressure().unwrap(), PressureCurve::from_points(vec![[0.0, 0.0], [1.0, 1.0]]));

        let m = parse_effector(&bytes(&MAPPING_PEN_SIZE_EFFECTOR)).unwrap();
        let curve = m.curves[0].as_ref().unwrap();
        assert_eq!(curve.len(), 4);
        assert_eq!(curve[0], [0.0, 0.0]);
        assert_eq!(curve[1][0], 0.5);
        assert!((curve[1][1] - 0.245).abs() < 0.01, "soft start: {:?}", curve[1]);
        assert_eq!(curve[3], [1.0, 1.0]);
        assert!(curve.windows(2).all(|w| w[0][0] < w[1][0]), "x ascending");

        assert!(parse_effector(&bytes(&NO_PRESSURE_EFFECTOR)).unwrap().pressure().is_none());
        assert!(parse_effector(&[0, 0, 0, 1]).is_none());
        assert!(parse_effector(&bytes(&G_PEN_SIZE_EFFECTOR)[..60]).is_none(), "truncated");
    }

    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE Node (_PW_ID INTEGER PRIMARY KEY, NodeName TEXT, NodeVariantID INTEGER);
             CREATE TABLE Variant (_PW_ID INTEGER PRIMARY KEY, VariantID INTEGER, BrushSize REAL, BrushSizeUnit INTEGER,
                Opacity INTEGER, BrushFlow INTEGER, BrushHardness INTEGER, BrushInterval REAL, BrushAutoIntervalType INTEGER,
                AntiAlias INTEGER, FlickerReduction INTEGER, CompositeMode INTEGER, BrushThickness INTEGER,
                BrushRotation REAL, BrushSizeEffector BLOB, BrushUseVectorEraser INTEGER, BrushUsePatternImage INTEGER,
                BrushPatternImageArray BLOB, TextureImage BLOB, TextureScale REAL, TextureDensity INTEGER,
                TextureReverseDensity INTEGER);
             INSERT INTO Node VALUES (1, '', 0), (2, 'G-pen', 10), (3, 'Hard Eraser', 11), (4, 'Calligraphy', 12), (5, 'Move', 13);
             INSERT INTO Variant (VariantID, BrushSize, BrushSizeUnit, Opacity, BrushFlow, BrushHardness, BrushInterval,
                BrushAutoIntervalType, AntiAlias, FlickerReduction, CompositeMode, BrushThickness, BrushRotation, BrushUseVectorEraser)
                VALUES (10, 10, 0, 100, 100, 100, 10, 2, 2, 6, 0, 100, 0, 0),
                       (11, 40, 0, 100, 100, 100, 10, 2, 1, 2, 27, 100, 0, 1),
                       (12, 50, 0, 90, 75, 100, 20, 0, 1, 6, 0, 18, 60, 0);
             INSERT INTO Variant (VariantID) VALUES (13);",
        )
        .unwrap();
        db.execute("UPDATE Variant SET BrushSizeEffector = ?1 WHERE VariantID = 10", [bytes(&G_PEN_SIZE_EFFECTOR)]).unwrap();
        db.execute("UPDATE Variant SET BrushSizeEffector = ?1 WHERE VariantID = 12", [bytes(&NO_PRESSURE_EFFECTOR)]).unwrap();
        // Same layout as Clip Studio's: header, one entry with a path and a trailing name.
        let reference: Vec<u8> = ":Paint004:abcdef12-3456:data:material.layer".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let name: Vec<u8> = "ブラシ".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let mut array = vec![0, 0, 0, 8, 0, 0, 0, 1];
        array.extend(((8 + reference.len() + name.len()) as u32).to_be_bytes());
        array.extend((reference.len() as u32).to_be_bytes());
        array.extend(reference);
        array.extend(name);
        db.execute("UPDATE Variant SET BrushUsePatternImage = 1, BrushPatternImageArray = ?1 WHERE VariantID = 12", [&array]).unwrap();
        // G-pen gets a texture: same material, 200% scale, 40% density, inverted.
        db.execute(
            "UPDATE Variant SET TextureImage = ?1, TextureScale = 200, TextureDensity = 40, TextureReverseDensity = 1 WHERE VariantID = 10",
            [&array],
        )
        .unwrap();
        db
    }

    /// A material library with `:Paint004:abcdef12-3456` installed: a 4×4
    /// thumbnail, white with one black pixel and one transparent pixel.
    fn library() -> (MaterialLibrary, PathBuf) {
        // One folder per call: tests run in parallel and each removes its own.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("painter-sut-test-{}-{n}", std::process::id()));
        let dir = root.join("Paint004").join("abcdef12-3456").join("thumbnail");
        std::fs::create_dir_all(&dir).unwrap();
        let mut image = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 255, 255, 255]));
        image.put_pixel(1, 1, image::Rgba([0, 0, 0, 255]));
        image.put_pixel(2, 2, image::Rgba([0, 0, 0, 0]));
        image.save(dir.join("thumbnail.png")).unwrap();
        (MaterialLibrary { roots: vec![root.clone()] }, root)
    }

    #[test]
    fn finds_copied_material_folders() {
        // A copy of Clip Studio's folder tree, nested under the app's Materials folder.
        let (_, install) = library();
        let copied = install.parent().unwrap().join(format!("{}-copied", install.file_name().unwrap().to_string_lossy()));
        let nested = copied.join("CLIPStudioCommon").join("Material").join("Install");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::rename(&install, &nested).unwrap();
        let mut found = MaterialLibrary::default();
        found.add_copied(&copied);
        assert_eq!(found.roots, vec![nested.clone()]);
        let report = read_db_with(&fixture(), &found).unwrap();
        assert!(report.presets[0].texture.is_some(), "texture from the copied material");
        found.add_copied(&copied);
        assert_eq!(found.roots.len(), 1, "no duplicates");
        std::fs::remove_dir_all(copied).ok();
    }

    #[test]
    fn resolves_material_thumbnails_and_falls_back() {
        let (library, root) = library();
        let report = read_db_with(&fixture(), &library).unwrap();
        let [g, _, cal] = &report.presets[..] else { panic!() };

        let tip = cal.tip.as_ref().expect("tip from the material thumbnail");
        assert_eq!((tip.width, tip.height), (4, 4));
        assert_eq!(tip.alpha[5], 255, "black = ink");
        assert_eq!(tip.alpha[0], 0, "white = no ink");
        assert_eq!(tip.alpha[10], 0, "transparent = no ink");
        assert!(matches!(cal.settings.tip.shape, painter_core::TipShape::Sampled(_)));

        let texture = g.texture.as_ref().expect("texture from the material thumbnail");
        assert_eq!((texture.image.alpha[0], texture.image.alpha[5], texture.image.alpha[10]), (255, 0, 255));
        assert_eq!((texture.scale, texture.density, texture.invert), (200.0, 0.4, true));
        assert!(g.settings.texture.as_ref().is_some_and(|t| (t.scale - 2.0).abs() < 1e-6));
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);

        // Not installed: round tip, no texture, two warnings, still imported.
        let report = read_db_with(&fixture(), &MaterialLibrary::default()).unwrap();
        assert_eq!(report.presets.len(), 3);
        assert!(report.presets[2].tip.is_none() && report.presets[0].texture.is_none());
        assert_eq!(report.warnings.len(), 2, "{:?}", report.warnings);
        assert!(library.material_dir("..:Paint004:../evil:data").is_none(), "no path traversal");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn maps_variants_to_presets() {
        let presets = read_db(&fixture()).unwrap();
        assert_eq!(presets.iter().map(BrushPreset::name).collect::<Vec<_>>(), ["G-pen", "Hard Eraser", "Calligraphy"]);

        let g = &presets[0];
        assert_eq!(g.settings.size, 10.0);
        assert!(g.settings.pressure_size, "pressure bit set");
        assert_eq!(g.settings.stabilization, 6);
        assert_eq!(g.settings.antialias, AntiAlias::Medium);
        assert!(g.extras.contains_key("sut.BrushSizeEffector"));

        let eraser = &presets[1];
        assert_eq!(eraser.settings.mode, BrushMode::Erase);
        assert_eq!(eraser.extras.get("sut.BrushUseVectorEraser"), Some(&serde_json::json!(1)));

        let cal = &presets[2];
        assert!(!cal.settings.pressure_size);
        assert!((cal.settings.opacity - 0.9).abs() < 1e-6 && (cal.settings.density - 0.75).abs() < 1e-6);
        assert!((cal.settings.spacing - 0.2).abs() < 1e-6, "fixed interval");
        assert!((cal.thickness - 0.18).abs() < 1e-6 && cal.angle == 60.0);
        assert_eq!(cal.extras.get("sut.tip_materials"), Some(&serde_json::json!([":Paint004:abcdef12-3456:data:material.layer"])));
        // The flat, rotated tip reaches the engine.
        assert_eq!(cal.settings.tip.kind(), painter_core::TipKind::Ellipse);
        assert_eq!(cal.settings.tip.angle, 60.0);
        assert!(cal.unsupported_features().is_empty());
    }

    /// An effector BLOB: Clip Studio's header (words 2..=11 given, lengths
    /// filled in) followed by curve blocks of big-endian `f64` points.
    fn effector_blob(header: [i32; 10], curves: &[&[[f64; 2]]]) -> Vec<u8> {
        let blocks: Vec<Vec<u8>> = curves
            .iter()
            .map(|points| {
                let mut b = Vec::new();
                for w in [12u32, points.len() as u32, 16] {
                    b.extend(w.to_be_bytes());
                }
                for p in *points {
                    b.extend(p[0].to_be_bytes());
                    b.extend(p[1].to_be_bytes());
                }
                b
            })
            .collect();
        let mut words = [48, 496, header[0], header[1], header[2], header[3], header[4], header[5], 0, 0, header[8], header[9]];
        for (i, block) in blocks.iter().enumerate() {
            words[8 + i] = block.len() as i32;
        }
        let mut out: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
        out.extend(blocks.concat());
        out
    }

    #[test]
    fn thickness_effector_is_kept_but_not_applied() {
        // Mechanical pencil's real thickness effector (Clip Studio Paint 1.5):
        // pressure enabled, curve falling from 1 to 0.
        let blob = effector_blob(
            [48, 45, 100, 63, 0, 0, 0, 0, 200, 0],
            &[&[[0.0, 1.0], [0.2364, 0.8636], [0.6727, 0.0], [1.0, 0.0]], &[[0.0, 0.0], [0.3364, 0.0], [0.5455, 0.0636], [0.8273, 0.8273], [1.0, 1.0]]],
        );
        let curve = parse_effector(&blob).expect("parses").pressure().expect("pressure bit set");
        assert!(curve.eval(1.0) < 0.01, "as pressure → thickness it would flatten every firm dab");
        // Importing keeps it verbatim but doesn't turn it into per-dab flattening.
        let db = fixture();
        db.execute("ALTER TABLE Variant ADD COLUMN BrushThicknessEffector BLOB", []).unwrap();
        db.execute("UPDATE Variant SET BrushThicknessEffector = ?1 WHERE VariantID = 10", [&blob]).unwrap();
        let g = &read_db(&db).unwrap()[0];
        assert!(g.dynamics.thickness.is_empty());
        assert!(g.settings.thickness_dynamics.is_default());
        assert!(g.extras.contains_key("sut.BrushThicknessEffector"));
    }

    #[test]
    fn maps_mechanical_pencil_rotation_flips_and_texture_scale() {
        // Mechanical pencil's values (Clip Studio Paint 1.5): random rotation
        // at 10%, random flips both ways, texture scale in `TextureScale2`.
        let db = fixture();
        db.execute_batch(
            "ALTER TABLE Variant ADD COLUMN BrushRotationEffector INTEGER;
             ALTER TABLE Variant ADD COLUMN BrushRotationRandomScale INTEGER;
             ALTER TABLE Variant ADD COLUMN BrushPatternReverseHorizontal INTEGER;
             ALTER TABLE Variant ADD COLUMN BrushPatternReverseVertical INTEGER;
             ALTER TABLE Variant ADD COLUMN TextureScale2 REAL;
             UPDATE Variant SET BrushRotationEffector = 147, BrushRotationRandomScale = 10,
                BrushPatternReverseHorizontal = 2, BrushPatternReverseVertical = 2,
                TextureScale = NULL, TextureScale2 = 20 WHERE VariantID = 10;
             UPDATE Variant SET BrushRotationEffector = 35, BrushRotationRandomScale = 100,
                BrushPatternReverseHorizontal = 0, BrushPatternReverseVertical = 0 WHERE VariantID = 12;",
        )
        .unwrap();
        let (library, root) = library();
        let report = read_db_with(&db, &library).unwrap();
        let [pencil, _, flat] = &report.presets[..] else { panic!() };

        let rotation = &pencil.settings.rotation;
        assert!((rotation.jitter - 0.1).abs() < 1e-6 && !rotation.follow_direction);
        assert!(rotation.random_flip_x && rotation.random_flip_y);
        assert_eq!(pencil.extras.get("sut.BrushRotationEffector"), Some(&serde_json::json!(147)));
        assert_eq!(pencil.texture.as_ref().map(|t| t.scale), Some(20.0));

        // Bit 0x20 without 0x80: turns with the stroke, no jitter, no flips.
        let rotation = &flat.settings.rotation;
        assert!(rotation.follow_direction && rotation.jitter == 0.0);
        assert!(!rotation.random_flip_x && !rotation.random_flip_y);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn writes_into_a_template_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("painter-sut-write-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let template = dir.join("template.sut");
        let out = dir.join("out.sut");
        let _ = std::fs::remove_file(&template);
        fixture().execute("VACUUM INTO ?1", [template.to_string_lossy()]).unwrap();

        let mut preset = BrushPreset::new(BrushSettings {
            name: "Painter Ink".into(),
            size: 33.0,
            opacity: 0.8,
            density: 0.6,
            hardness: 0.5,
            spacing: 0.2,
            antialias: AntiAlias::Strong,
            stabilization: 12,
            min_size_ratio: 0.25,
            pressure_size: true,
            pressure_density: true,
            ..BrushSettings::g_pen()
        });
        preset.thickness = 0.4;
        preset.angle = 45.0;
        preset.dynamics.rotation_follows_direction = true;
        preset.dynamics.random_flip_x = true;
        let notes = write_with_template(&template, &preset, &out).unwrap();
        assert!(notes.iter().any(|n| n.contains("3 brushes")), "{notes:?}");

        let back = read_report(&out, &MaterialLibrary::default()).unwrap();
        let p = &back.presets[0];
        assert_eq!(p.settings.name, "Painter Ink");
        assert_eq!(p.settings.size, 33.0);
        assert!((p.settings.opacity - 0.8).abs() < 1e-4 && (p.settings.density - 0.6).abs() < 1e-4);
        assert!((p.settings.hardness - 0.5).abs() < 1e-4 && (p.settings.spacing - 0.2).abs() < 1e-4);
        assert_eq!((p.settings.antialias, p.settings.stabilization), (AntiAlias::Strong, 12));
        assert!((p.thickness - 0.4).abs() < 1e-4 && (p.angle - 45.0).abs() < 1e-4);
        // Pressure → size starts at the minimum; pressure → flow is on.
        assert!(p.settings.pressure_size && (p.settings.size_curve.eval(0.0) - 0.25).abs() < 1e-4);
        // The template has no flow effector column: noted, not carried.
        assert!(!p.settings.pressure_density && notes.iter().any(|n| n.contains("BrushFlowEffector")), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("BrushPatternReverseHorizontal")), "flips need columns this template lacks");
        // The other sub tools of the template are untouched; the template too.
        assert_eq!(back.presets[1].settings.name, "Hard Eraser");
        assert_eq!(read_report(&template, &MaterialLibrary::default()).unwrap().presets[0].settings.name, "G-pen");

        // Not a sub tool: refused, and no half-written file is left.
        let other = dir.join("other.sut");
        let db = Connection::open(&other).unwrap();
        db.execute_batch("CREATE TABLE Other (x INTEGER);").unwrap();
        drop(db);
        let bad = dir.join("bad.sut");
        assert!(write_with_template(&other, &preset, &bad).is_err());
        assert!(!bad.exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn rejects_other_databases() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE Other (x INTEGER);").unwrap();
        assert!(read_db(&db).is_err());
    }
}
