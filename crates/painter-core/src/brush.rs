//! Brush parameters and the dabs a stroke produces.

use serde::{Deserialize, Serialize};

use crate::tip::{DabTexture, DabTip, DualTip};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AntiAlias {
    None,
    Weak,
    #[default]
    Medium,
    Strong,
}

impl AntiAlias {
    pub const ALL: [AntiAlias; 4] = [AntiAlias::None, AntiAlias::Weak, AntiAlias::Medium, AntiAlias::Strong];

    /// Width in pixels of the edge ramp.
    pub fn width(self) -> f32 {
        match self {
            AntiAlias::None => 0.0,
            AntiAlias::Weak => 0.5,
            AntiAlias::Medium => 1.0,
            AntiAlias::Strong => 2.0,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            AntiAlias::None => "None",
            AntiAlias::Weak => "Weak",
            AntiAlias::Medium => "Medium",
            AntiAlias::Strong => "Strong",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BrushMode {
    #[default]
    Paint,
    Erase,
}

/// How a stroke's colour mixes with the layer's pixels under it (Clip Studio's
/// brush blending modes): like a layer blend mode, but applied as the stroke
/// lands on the pixels of the same layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BrushBlend {
    #[default]
    Normal,
    /// Paints only where the layer is transparent (under what's there).
    Behind,
    Darken,
    Multiply,
    ColorBurn,
    LinearBurn,
    Lighten,
    Screen,
    ColorDodge,
    Add,
    Overlay,
    SoftLight,
    HardLight,
    Difference,
}

impl BrushBlend {
    pub const ALL: [BrushBlend; 14] = [
        BrushBlend::Normal,
        BrushBlend::Behind,
        BrushBlend::Darken,
        BrushBlend::Multiply,
        BrushBlend::ColorBurn,
        BrushBlend::LinearBurn,
        BrushBlend::Lighten,
        BrushBlend::Screen,
        BrushBlend::ColorDodge,
        BrushBlend::Add,
        BrushBlend::Overlay,
        BrushBlend::SoftLight,
        BrushBlend::HardLight,
        BrushBlend::Difference,
    ];

    /// The layer blend mode it mixes with (`None` for Behind).
    pub fn layer_mode(self) -> Option<crate::blend::BlendMode> {
        use crate::blend::BlendMode as M;
        Some(match self {
            BrushBlend::Normal => M::Normal,
            BrushBlend::Behind => return None,
            BrushBlend::Darken => M::Darken,
            BrushBlend::Multiply => M::Multiply,
            BrushBlend::ColorBurn => M::ColorBurn,
            BrushBlend::LinearBurn => M::LinearBurn,
            BrushBlend::Lighten => M::Lighten,
            BrushBlend::Screen => M::Screen,
            BrushBlend::ColorDodge => M::ColorDodge,
            BrushBlend::Add => M::Add,
            BrushBlend::Overlay => M::Overlay,
            BrushBlend::SoftLight => M::SoftLight,
            BrushBlend::HardLight => M::HardLight,
            BrushBlend::Difference => M::Difference,
        })
    }

    pub fn name(self) -> &'static str {
        match self.layer_mode() {
            Some(m) => m.name(),
            None => "Behind",
        }
    }

    /// The brush mode mixing like layer mode `m`.
    pub fn from_layer_mode(m: crate::blend::BlendMode) -> BrushBlend {
        BrushBlend::ALL.into_iter().find(|b| b.layer_mode() == Some(m)).unwrap_or_default()
    }

    /// The id the GPU stroke preview understands: the layer mode's, or 100 for Behind.
    pub fn shader_id(self) -> u32 {
        self.layer_mode().map_or(100, |m| m.shader_id())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SmoothingMethod {
    /// Moving average over the last N input samples.
    #[default]
    Average,
    /// "Lazy" brush: the dab position is dragged behind the pointer on a string.
    Pulled,
}

impl SmoothingMethod {
    pub const ALL: [SmoothingMethod; 2] = [SmoothingMethod::Average, SmoothingMethod::Pulled];

    pub fn name(self) -> &'static str {
        match self {
            SmoothingMethod::Average => "Weighted average",
            SmoothingMethod::Pulled => "Pulled string",
        }
    }
}

/// Piecewise-linear mapping from pen pressure to a 0..1 factor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PressureCurve {
    points: Vec<[f32; 2]>,
}

impl Default for PressureCurve {
    fn default() -> Self {
        Self::linear()
    }
}

impl PressureCurve {
    pub fn linear() -> Self {
        Self { points: vec![[0.0, 0.0], [1.0, 1.0]] }
    }

    /// `y = x^gamma`: below 1 feels soft (light touch goes far), above 1 feels firm.
    pub fn gamma(gamma: f32) -> Self {
        let points = (0..=16)
            .map(|i| {
                let x = i as f32 / 16.0;
                [x, x.powf(gamma)]
            })
            .collect();
        Self { points }
    }

    /// Control points are clamped to the unit square, sorted, and pinned at x = 0 and x = 1.
    pub fn from_points(points: Vec<[f32; 2]>) -> Self {
        let mut points: Vec<[f32; 2]> =
            points.into_iter().map(|[x, y]| [x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)]).collect();
        points.sort_by(|a, b| a[0].total_cmp(&b[0]));
        if points.first().is_none_or(|p| p[0] > 0.0) {
            points.insert(0, [0.0, 0.0]);
        }
        if points.last().is_none_or(|p| p[0] < 1.0) {
            points.push([1.0, 1.0]);
        }
        Self { points }
    }

    pub fn points(&self) -> &[[f32; 2]] {
        &self.points
    }

    pub fn eval(&self, pressure: f32) -> f32 {
        let p = pressure.clamp(0.0, 1.0);
        let pts = &self.points;
        let i = pts.partition_point(|q| q[0] < p);
        if i == 0 {
            return pts[0][1];
        }
        if i >= pts.len() {
            return pts[pts.len() - 1][1];
        }
        let (a, b) = (pts[i - 1], pts[i]);
        let span = b[0] - a[0];
        if span <= f32::EPSILON {
            return b[1];
        }
        a[1] + (b[1] - a[1]) * (p - a[0]) / span
    }
}

/// Stroke speed (canvas px per second) treated as "fast" (normalised velocity 1).
pub const FAST_SPEED: f32 = 3000.0;

/// How a brush parameter reacts to pen pressure, stroke speed, pen tilt and
/// randomness. Each source multiplies the parameter. (Size and density also
/// keep their original pressure fields, `size_curve` and `density_curve`.)
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputDynamics {
    /// Factor from pen pressure.
    pub pressure: Option<PressureCurve>,
    /// Factor from normalised speed: 0 = still, 1 = [`FAST_SPEED`] or faster.
    pub velocity: Option<PressureCurve>,
    /// Factor from tilt: 0 = upright pen, 1 = flat.
    pub tilt: Option<PressureCurve>,
    /// Random reduction per dab, 0..=1: the factor is uniform in `1 − jitter ..= 1`.
    pub jitter: f32,
}

impl InputDynamics {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// `random` is uniform in 0..1.
    pub fn factor(&self, pressure: f32, velocity: f32, tilt: f32, random: f32) -> f32 {
        let mut f = 1.0;
        if let Some(curve) = &self.pressure {
            f *= curve.eval(pressure);
        }
        if let Some(curve) = &self.velocity {
            f *= curve.eval(velocity);
        }
        if let Some(curve) = &self.tilt {
            f *= curve.eval(tilt);
        }
        f * (1.0 - self.jitter.clamp(0.0, 1.0) * random)
    }
}

/// Per-dab tip rotation, added to the tip's own angle.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RotationDynamics {
    /// Turn the tip with the stroke direction.
    pub follow_direction: bool,
    /// Turn the tip towards where the pen leans (its azimuth), while it's tilted.
    pub follow_tilt: bool,
    /// Random rotation, 0..=1 of a half turn either way (1 = ±180°).
    pub jitter: f32,
    /// Turns from pen pressure: the curve's output × 360°.
    pub pressure: Option<PressureCurve>,
    /// Turns from normalised speed.
    pub velocity: Option<PressureCurve>,
    /// Turns from pen tilt.
    pub tilt: Option<PressureCurve>,
    /// Mirror the tip left-right on a random half of the dabs.
    pub random_flip_x: bool,
    /// Mirror the tip top-bottom on a random half of the dabs.
    pub random_flip_y: bool,
}

impl RotationDynamics {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Degrees added to the tip angle, excluding the stroke direction.
    pub fn offset(&self, pressure: f32, velocity: f32, tilt: f32, random: f32) -> f32 {
        let turns = |curve: &Option<PressureCurve>, input: f32| curve.as_ref().map_or(0.0, |c| c.eval(input) * 360.0);
        turns(&self.pressure, pressure)
            + turns(&self.velocity, velocity)
            + turns(&self.tilt, tilt)
            + self.jitter.clamp(0.0, 1.0) * (random * 2.0 - 1.0) * 180.0
    }

    /// This dab's random flips as (mirror left-right, extra half turn): a
    /// top-bottom flip is a left-right one turned 180°. `bits` are two random
    /// bits independent of the rotation jitter's draw.
    pub fn flips(&self, bits: u32) -> (bool, bool) {
        let x = self.random_flip_x && bits & 1 != 0;
        let y = self.random_flip_y && bits & 2 != 0;
        (x != y, y)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrushSettings {
    pub name: String,
    pub mode: BrushMode,
    /// How the stroke mixes with the layer under it.
    #[serde(default)]
    pub blend: BrushBlend,
    /// Diameter in canvas pixels at full pressure.
    pub size: f32,
    /// Fraction of `size` used at zero pressure when `pressure_size` is on.
    pub min_size_ratio: f32,
    pub pressure_size: bool,
    pub size_curve: PressureCurve,
    /// Per-stroke ceiling: overlapping dabs within one stroke never exceed it.
    pub opacity: f32,
    /// Per-dab alpha; builds up inside a stroke.
    pub density: f32,
    pub pressure_density: bool,
    pub density_curve: PressureCurve,
    /// Dab interval as a fraction of the current diameter.
    pub spacing: f32,
    /// 1 = solid disc, 0 = falloff all the way from the centre.
    pub hardness: f32,
    pub antialias: AntiAlias,
    /// 0..=100.
    pub stabilization: u32,
    pub smoothing: SmoothingMethod,
    /// Tip shape; round unless a preset brought a bitmap or a flat tip.
    #[serde(default, skip_serializing_if = "DabTip::is_default")]
    pub tip: DabTip,
    /// Paper texture the ink follows, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture: Option<DabTexture>,
    /// Second tip masking every dab, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dual: Option<DualTip>,
    /// Size from speed, tilt and jitter (multiplies the pressure-based size).
    #[serde(default, skip_serializing_if = "InputDynamics::is_default")]
    pub size_dynamics: InputDynamics,
    /// Density from speed, tilt and jitter (multiplies the pressure-based density).
    #[serde(default, skip_serializing_if = "InputDynamics::is_default")]
    pub density_dynamics: InputDynamics,
    /// Per-dab opacity from pressure, speed, tilt and jitter; multiplies density.
    #[serde(default, skip_serializing_if = "InputDynamics::is_default")]
    pub opacity_dynamics: InputDynamics,
    /// Tip thickness (flattening) per dab; multiplies the tip's own thickness.
    #[serde(default, skip_serializing_if = "InputDynamics::is_default")]
    pub thickness_dynamics: InputDynamics,
    #[serde(default, skip_serializing_if = "RotationDynamics::is_default")]
    pub rotation: RotationDynamics,
    /// Dabs sprayed around the stroke (Clip Studio's spraying effect,
    /// Photoshop's scattering).
    #[serde(default, skip_serializing_if = "Scatter::is_default")]
    pub scatter: Scatter,
    /// Colour varied per stroke (Clip Studio's color jitter, Photoshop's
    /// color dynamics "apply per tip" off).
    #[serde(default, skip_serializing_if = "ColorJitter::is_default")]
    pub color_jitter: ColorJitter,
    /// Starting: size and density grow over this many canvas pixels at the
    /// start of a stroke (0 = off).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub taper_start: f32,
    /// Pencil side, 0..=1: as the pen leans, the tip lies down along the lean like
    /// a pencil's side: wider, flatter across, lighter. 0 = off.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tilt_side: f32,
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

/// Dabs scattered around each point of the stroke.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Scatter {
    /// How far dabs land from the stroke, in brush diameters (0 = off).
    pub amount: f32,
    /// Dabs per stroke point, 1..=16.
    pub count: u32,
    /// Random reduction of the count, 0..=1.
    pub count_jitter: f32,
    /// Scatter along the stroke as well as across it.
    pub both_axes: bool,
}

impl Scatter {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn is_on(&self) -> bool {
        self.amount > 0.0 || self.count > 1
    }
}

/// Random change of the main colour, per stroke.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorJitter {
    /// Hue shift either way, 0..=1 of a half turn.
    pub hue: f32,
    /// Saturation and value change either way, 0..=1.
    pub saturation: f32,
    pub value: f32,
}

impl ColorJitter {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// `rgb` (0..1) varied with three uniform random numbers in 0..1.
    pub fn apply(&self, rgb: [f32; 3], r: [f32; 3]) -> [f32; 3] {
        if self.is_default() {
            return rgb;
        }
        let [h, s, v] = rgb_to_hsv(rgb);
        let h = (h + (r[0] * 2.0 - 1.0) * self.hue.clamp(0.0, 1.0) * 0.5).rem_euclid(1.0);
        let s = (s + (r[1] * 2.0 - 1.0) * self.saturation.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        let v = (v + (r[2] * 2.0 - 1.0) * self.value.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        hsv_to_rgb([h, s, v])
    }
}

/// HSV (0..1 each) from RGB (0..1 each).
pub fn rgb_to_hsv([r, g, b]: [f32; 3]) -> [f32; 3] {
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    let h = if d <= 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    [h, if max > 0.0 { d / max } else { 0.0 }, max]
}

pub fn hsv_to_rgb([h, s, v]: [f32; 3]) -> [f32; 3] {
    let h6 = h.rem_euclid(1.0) * 6.0;
    let c = v * s;
    let x = c * (1.0 - (h6 % 2.0 - 1.0).abs());
    let (r, g, b) = match h6 as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r + m, g + m, b + m]
}

impl Default for BrushSettings {
    fn default() -> Self {
        Self::g_pen()
    }
}

impl BrushSettings {
    pub fn g_pen() -> Self {
        Self {
            name: "G-Pen".into(),
            mode: BrushMode::Paint,
            blend: BrushBlend::Normal,
            size: 12.0,
            min_size_ratio: 0.1,
            pressure_size: true,
            size_curve: PressureCurve::linear(),
            opacity: 1.0,
            density: 1.0,
            pressure_density: false,
            density_curve: PressureCurve::linear(),
            spacing: 0.08,
            hardness: 1.0,
            antialias: AntiAlias::Medium,
            stabilization: 8,
            smoothing: SmoothingMethod::Average,
            tip: DabTip::default(),
            texture: None,
            dual: None,
            size_dynamics: InputDynamics::default(),
            density_dynamics: InputDynamics::default(),
            opacity_dynamics: InputDynamics::default(),
            thickness_dynamics: InputDynamics::default(),
            rotation: RotationDynamics::default(),
            scatter: Scatter::default(),
            color_jitter: ColorJitter::default(),
            taper_start: 0.0,
            tilt_side: 0.0,
        }
    }

    pub fn pencil() -> Self {
        Self {
            name: "Pencil".into(),
            // Shading with the side of the lead when the pen leans.
            tilt_side: 0.7,
            size: 6.0,
            min_size_ratio: 0.6,
            opacity: 0.9,
            density: 0.6,
            pressure_density: true,
            spacing: 0.15,
            hardness: 0.6,
            antialias: AntiAlias::Weak,
            stabilization: 3,
            ..Self::g_pen()
        }
    }

    pub fn airbrush() -> Self {
        Self {
            name: "Airbrush".into(),
            size: 120.0,
            min_size_ratio: 1.0,
            pressure_size: false,
            density: 0.08,
            pressure_density: true,
            spacing: 0.1,
            hardness: 0.0,
            antialias: AntiAlias::Strong,
            stabilization: 0,
            ..Self::g_pen()
        }
    }

    pub fn hard_eraser() -> Self {
        Self {
            name: "Hard Eraser".into(),
            mode: BrushMode::Erase,
            size: 30.0,
            pressure_size: false,
            stabilization: 0,
            ..Self::g_pen()
        }
    }

    pub fn soft_eraser() -> Self {
        Self {
            name: "Soft Eraser".into(),
            mode: BrushMode::Erase,
            ..Self::airbrush()
        }
    }

    pub fn presets() -> Vec<BrushSettings> {
        vec![Self::g_pen(), Self::pencil(), Self::airbrush(), Self::hard_eraser(), Self::soft_eraser()]
    }

    pub fn diameter_at(&self, pressure: f32) -> f32 {
        let scale = if self.pressure_size {
            self.min_size_ratio + (1.0 - self.min_size_ratio) * self.size_curve.eval(pressure)
        } else {
            1.0
        };
        (self.size * scale).max(0.1)
    }

    pub fn density_at(&self, pressure: f32) -> f32 {
        let scale = if self.pressure_density { self.density_curve.eval(pressure) } else { 1.0 };
        (self.density * scale).clamp(0.0, 1.0)
    }

    /// Distance between dabs for a given diameter, never below half a pixel.
    pub fn spacing_px(&self, diameter: f32) -> f32 {
        (self.spacing * diameter).max(0.5)
    }
}

/// One stamp of the brush tip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dab {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    /// Per-dab alpha (density × pressure).
    pub alpha: f32,
    /// Extra tip rotation for this dab, degrees (rotation dynamics).
    pub angle: f32,
    /// Multiplies the tip's thickness for this dab (thickness dynamics); 1 = unchanged.
    pub thickness: f32,
    /// Mirror the tip left-right (in tip space, before rotation) for this dab.
    pub mirror: bool,
}

impl Dab {
    /// Sub-pixel tips are drawn at radius 0.5 with alpha scaled by area, so
    /// thin strokes fade instead of vanishing. CPU and GPU rasterisers both
    /// stamp the normalised dab.
    pub fn normalized(self) -> Dab {
        if self.radius >= 0.5 {
            return self;
        }
        Dab { radius: 0.5, alpha: self.alpha * (self.radius / 0.5).powi(2), ..self }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_interpolates() {
        let curve = PressureCurve::from_points(vec![[0.5, 0.2]]);
        assert_eq!(curve.points().len(), 3);
        assert!((curve.eval(0.25) - 0.1).abs() < 1e-6);
        assert!((curve.eval(0.75) - 0.6).abs() < 1e-6);
        assert_eq!(curve.eval(2.0), 1.0);
    }

    #[test]
    fn pressure_scales_size() {
        let brush = BrushSettings::g_pen();
        assert!((brush.diameter_at(1.0) - 12.0).abs() < 1e-5);
        assert!((brush.diameter_at(0.0) - 1.2).abs() < 1e-5);
    }
}
