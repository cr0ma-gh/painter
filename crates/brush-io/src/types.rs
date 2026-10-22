//! The unified brush representation shared by every format.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use painter_core::brush::{InputDynamics, RotationDynamics};
use painter_core::{BrushSettings, DabTexture, DabTip, DualTip, PressureCurve, TipBitmap, TipShape};
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub enum BrushIoError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
    /// Structurally invalid input.
    Corrupt(String),
    /// Valid input using a feature or version we don't read.
    Unsupported(String),
}

impl fmt::Display for BrushIoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BrushIoError::Io(e) => write!(f, "{e}"),
            BrushIoError::Sqlite(e) => write!(f, "not a readable Clip Studio database: {e}"),
            BrushIoError::Json(e) => write!(f, "invalid .rustbrush: {e}"),
            BrushIoError::Corrupt(msg) => write!(f, "corrupt brush file: {msg}"),
            BrushIoError::Unsupported(msg) => write!(f, "unsupported: {msg}"),
        }
    }
}

impl std::error::Error for BrushIoError {}

impl From<std::io::Error> for BrushIoError {
    fn from(e: std::io::Error) -> Self {
        BrushIoError::Io(e)
    }
}

impl From<rusqlite::Error> for BrushIoError {
    fn from(e: rusqlite::Error) -> Self {
        BrushIoError::Sqlite(e)
    }
}

impl From<serde_json::Error> for BrushIoError {
    fn from(e: serde_json::Error) -> Self {
        BrushIoError::Json(e)
    }
}

/// An 8-bit coverage bitmap for a brush tip (255 = full ink), row-major.
/// Serialised as a grayscale PNG in base64 (see `native`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "crate::native::ImageFile", try_from = "crate::native::ImageFile")]
pub struct BrushTip {
    pub width: u32,
    pub height: u32,
    pub alpha: Vec<u8>,
}

impl BrushTip {
    pub fn new(width: u32, height: u32, alpha: Vec<u8>) -> Result<Self, BrushIoError> {
        if width == 0 || height == 0 || alpha.len() != (width as usize) * (height as usize) {
            return Err(BrushIoError::Corrupt(format!("tip {width}×{height} with {} bytes", alpha.len())));
        }
        Ok(Self { width, height, alpha })
    }

    /// Coverage-weighted fraction of the bounding box that is inked, 0..=1.
    pub fn fill_ratio(&self) -> f32 {
        self.alpha.iter().map(|&a| a as f32).sum::<f32>() / (255.0 * self.alpha.len() as f32)
    }
}

/// A paper/grain texture applied over strokes. `image` is luminance: light
/// lets ink through, dark holds it back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrushTexture {
    pub image: BrushTip,
    /// Percent (100 = one texel per canvas pixel).
    pub scale: f32,
    /// 0..=1.
    pub density: f32,
    #[serde(default)]
    pub invert: bool,
}

/// A second tip masking every dab ("dual brush").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DualBrush {
    pub tip: BrushTip,
    /// Dual tip diameter over the main diameter.
    pub scale: f32,
}

/// What an import produced, including what it had to fall back on.
#[derive(Clone, Debug, Default)]
pub struct ImportReport {
    pub presets: Vec<BrushPreset>,
    /// Problems handled with a fallback (e.g. an unreadable tip replaced by a round one).
    pub warnings: Vec<String>,
    /// Served from the brush cache instead of parsing the source file.
    pub from_cache: bool,
}

/// How one brush parameter reacts to the pen.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Dynamic {
    pub pressure: Option<PressureCurve>,
    pub velocity: Option<PressureCurve>,
    pub tilt: Option<PressureCurve>,
    /// Random jitter amount, 0..=1.
    pub random: Option<f32>,
    /// Value at zero input as a fraction of the base value, when the source states it.
    pub minimum: Option<f32>,
}

impl Dynamic {
    pub fn is_empty(&self) -> bool {
        *self == Dynamic::default()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Dynamics {
    #[serde(default, skip_serializing_if = "Dynamic::is_empty")]
    pub size: Dynamic,
    #[serde(default, skip_serializing_if = "Dynamic::is_empty")]
    pub opacity: Dynamic,
    /// Per-dab flow / density.
    #[serde(default, skip_serializing_if = "Dynamic::is_empty")]
    pub flow: Dynamic,
    #[serde(default, skip_serializing_if = "Dynamic::is_empty")]
    pub thickness: Dynamic,
    #[serde(default, skip_serializing_if = "Dynamic::is_empty")]
    pub rotation: Dynamic,
    /// The tip turns with the stroke direction.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rotation_follows_direction: bool,
    /// The tip is mirrored left-right / top-bottom on random dabs.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub random_flip_x: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub random_flip_y: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PresetSource {
    #[default]
    Native,
    ClipStudio {
        variant_id: i64,
    },
    Photoshop {
        version: u16,
        sample_id: Option<String>,
    },
    /// A Krita paint op preset; `paintop` is its engine ("paintbrush", "colorsmudge"…).
    Krita {
        paintop: String,
    },
    MyPaint,
    Gimp,
}

/// One brush preset in Painter's unified representation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrushPreset {
    /// What the brush engine uses. `settings.name` is the preset name;
    /// `settings.tip` is derived from the fields below (see [`Self::apply_tip`]).
    pub settings: BrushSettings,
    /// Sampled tip shape, when the source had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip: Option<BrushTip>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture: Option<BrushTexture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dual: Option<DualBrush>,
    /// Source dynamics, including ones the engine doesn't apply yet.
    #[serde(default)]
    pub dynamics: Dynamics,
    /// Tip shape beyond round: thickness (aspect, 0..=1) and angle in degrees.
    #[serde(default = "one")]
    pub thickness: f32,
    #[serde(default)]
    pub angle: f32,
    #[serde(default)]
    pub source: PresetSource,
    /// Source parameters with no field above, kept verbatim for round trips.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extras: BTreeMap<String, serde_json::Value>,
}

fn one() -> f32 {
    1.0
}

impl BrushPreset {
    pub fn new(settings: BrushSettings) -> Self {
        Self {
            settings,
            tip: None,
            texture: None,
            dual: None,
            dynamics: Dynamics::default(),
            thickness: 1.0,
            angle: 0.0,
            source: PresetSource::Native,
            extras: BTreeMap::new(),
        }
    }

    pub fn name(&self) -> &str {
        &self.settings.name
    }

    /// Copies the dynamics into `settings`:
    /// - size and flow (density): pressure, velocity, tilt and random;
    /// - opacity and thickness: pressure, velocity, tilt and random, per dab;
    /// - rotation: pressure, velocity and tilt (curve output × 360°), random,
    ///   following the stroke direction, and random flips.
    pub fn apply_dynamics(&mut self) {
        let d = &self.dynamics;
        if let Some(curve) = &d.size.pressure {
            self.settings.pressure_size = true;
            self.settings.size_curve = curve.clone();
            self.settings.min_size_ratio = d.size.minimum.unwrap_or(0.0).clamp(0.0, 1.0);
        }
        if let Some(curve) = &d.flow.pressure {
            self.settings.pressure_density = true;
            self.settings.density_curve = curve.clone();
        }
        // Size and flow pressure already went to their own curves above.
        let input = |dynamic: &Dynamic, with_pressure: bool| InputDynamics {
            pressure: if with_pressure { dynamic.pressure.clone() } else { None },
            velocity: dynamic.velocity.clone(),
            tilt: dynamic.tilt.clone(),
            jitter: dynamic.random.unwrap_or(0.0).clamp(0.0, 1.0),
        };
        self.settings.size_dynamics = input(&d.size, false);
        self.settings.density_dynamics = input(&d.flow, false);
        self.settings.opacity_dynamics = input(&d.opacity, true);
        self.settings.thickness_dynamics = input(&d.thickness, true);
        self.settings.rotation = RotationDynamics {
            follow_direction: d.rotation_follows_direction,
            // No file format here carries it: kept from the brush.
            follow_tilt: self.settings.rotation.follow_tilt,
            jitter: d.rotation.random.unwrap_or(0.0).clamp(0.0, 1.0),
            pressure: d.rotation.pressure.clone(),
            velocity: d.rotation.velocity.clone(),
            tilt: d.rotation.tilt.clone(),
            random_flip_x: d.random_flip_x,
            random_flip_y: d.random_flip_y,
        };
    }

    /// Sets the engine's tip, paper texture and dual tip (`settings.tip`,
    /// `settings.texture`, `settings.dual`) from this preset's bitmaps.
    /// Every reader calls it; call it again after changing those fields.
    pub fn apply_tip(&mut self) {
        let bitmap = |t: &BrushTip| TipBitmap::new(t.width, t.height, t.alpha.clone()).map(Arc::new);
        let shape = self.tip.as_ref().and_then(bitmap).map_or(TipShape::Round, TipShape::Sampled);
        self.settings.tip = DabTip { shape, thickness: self.thickness, angle: self.angle };
        self.settings.texture = self.texture.as_ref().and_then(|t| {
            Some(DabTexture { bitmap: bitmap(&t.image)?, scale: t.scale / 100.0, density: t.density, invert: t.invert })
        });
        self.settings.dual = self.dual.as_ref().and_then(|d| Some(DualTip { bitmap: bitmap(&d.tip)?, scale: d.scale }));
    }

    /// A hash of everything the engine uses, to tell whether the preset was
    /// edited since it was imported.
    fn fingerprint(&self) -> String {
        let json = serde_json::to_vec(&(&self.settings, &self.tip, &self.texture, &self.dual, &self.dynamics, self.thickness, self.angle))
            .unwrap_or_default();
        let hash = json.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x0100_0000_01b3));
        format!("{hash:016x}")
    }

    /// Keeps the file this preset was read from (`format`: "krita", "mypaint"…)
    /// so exporting it back to that format, unedited, gives the same bytes.
    /// Call it once the preset is complete.
    pub fn keep_original(&mut self, format: &str, bytes: &[u8]) {
        use base64::Engine;
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        self.extras.insert(format!("{format}.original"), data.into());
        self.extras.insert(format!("{format}.fingerprint"), self.fingerprint().into());
    }

    /// The original `format` file, if this preset came from one and hasn't been edited since.
    pub fn unchanged_original(&self, format: &str) -> Option<Vec<u8>> {
        use base64::Engine;
        let print = self.extras.get(&format!("{format}.fingerprint"))?.as_str()?;
        if print != self.fingerprint() {
            return None;
        }
        base64::engine::general_purpose::STANDARD.decode(self.extras.get(&format!("{format}.original"))?.as_str()?).ok()
    }

    /// The original `format` file even if the preset was edited (for its
    /// parameters our engine doesn't have).
    pub fn original(&self, format: &str) -> Option<Vec<u8>> {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.decode(self.extras.get(&format!("{format}.original"))?.as_str()?).ok()
    }

    /// Features of this preset the brush engine doesn't apply.
    pub fn unsupported_features(&self) -> Vec<&'static str> {
        let d = &self.dynamics;
        let mut out = Vec::new();
        if [&d.opacity, &d.thickness, &d.rotation].iter().any(|x| x.minimum.is_some()) {
            out.push("minimum of opacity/thickness/rotation");
        }
        out
    }
}
