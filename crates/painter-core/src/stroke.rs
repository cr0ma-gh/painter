//! Turns pointer samples into evenly spaced dabs, applying the brush's dynamics.
//!
//! Every per-dab value (radius, alpha, rotation) is computed here, once, so
//! the CPU rasteriser and the GPU preview stamp identical dabs. Randomness
//! comes from a generator seeded per stroke, so a vector stroke replays the
//! same way.

use crate::brush::{BrushSettings, Dab, FAST_SPEED};
use crate::smoothing::{Smoother, StrokePoint};

/// Seed used by [`StrokeEngine::new`].
pub const DEFAULT_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

pub struct StrokeEngine {
    settings: BrushSettings,
    smoother: Smoother,
    last: Option<StrokePoint>,
    /// Distance travelled since the last dab.
    carry: f32,
    /// Normalised speed of the current segment, 0..=1.
    velocity: f32,
    /// Direction of the current segment, degrees (counter-clockwise on screen).
    direction: f32,
    rng: u64,
}

impl StrokeEngine {
    pub fn new(settings: &BrushSettings) -> Self {
        Self::with_seed(settings, DEFAULT_SEED)
    }

    /// An engine whose random dynamics are reproducible from `seed`.
    pub fn with_seed(settings: &BrushSettings, seed: u64) -> Self {
        Self {
            settings: settings.clone(),
            smoother: Smoother::new(settings.smoothing, settings.stabilization),
            last: None,
            carry: 0.0,
            velocity: 0.0,
            direction: 0.0,
            // splitmix64 of the seed, never zero (xorshift would stick at 0).
            rng: {
                let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                (z ^ (z >> 31)) | 1
            },
        }
    }

    pub fn add(&mut self, p: StrokePoint) -> Vec<Dab> {
        let mut out = Vec::new();
        if let Some(target) = self.smoother.push(p) {
            self.walk_to(target, &mut out);
        }
        out
    }

    pub fn finish(&mut self) -> Vec<Dab> {
        let mut out = Vec::new();
        for target in self.smoother.finish() {
            self.walk_to(target, &mut out);
        }
        out
    }

    /// Uniform in 0..1 (xorshift64*).
    fn random(&mut self) -> f32 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u64 << 24) as f32
    }

    fn walk_to(&mut self, to: StrokePoint, out: &mut Vec<Dab>) {
        let Some(from) = self.last else {
            let dab = self.dab_at(to);
            out.push(dab);
            self.last = Some(to);
            return;
        };
        let len = from.distance(to);
        if len < 1e-4 {
            self.last = Some(to);
            return;
        }
        let dt = to.time - from.time;
        if dt > 1e-4 {
            self.velocity = (len / dt / FAST_SPEED).clamp(0.0, 1.0);
        }
        // Screen y points down; counter-clockwise on screen is atan2(−dy, dx).
        self.direction = (-(to.y - from.y)).atan2(to.x - from.x).to_degrees();
        let mut pos = 0.0;
        loop {
            let pressure = from.lerp(to, pos / len).pressure;
            let step = self.settings.spacing_px(self.settings.diameter_at(pressure));
            let need = step - self.carry;
            if pos + need > len {
                self.carry += len - pos;
                break;
            }
            pos += need;
            self.carry = 0.0;
            let dab = self.dab_at(from.lerp(to, pos / len));
            out.push(dab);
        }
        self.last = Some(to);
    }

    fn dab_at(&mut self, p: StrokePoint) -> Dab {
        // Always five draws per dab, so the sequence doesn't depend on which dynamics are on.
        let r: [f32; 5] = std::array::from_fn(|_| self.random());
        let s = &self.settings;
        let (pressure, velocity, tilt) = (p.pressure, self.velocity, p.tilt);
        let size = s.size_dynamics.factor(pressure, velocity, tilt, r[0]);
        let density = s.density_dynamics.factor(pressure, velocity, tilt, r[1]);
        let opacity = s.opacity_dynamics.factor(pressure, velocity, tilt, r[2]);
        let thickness = s.thickness_dynamics.factor(pressure, velocity, tilt, r[3]);
        let mut angle = s.rotation.offset(pressure, velocity, tilt, r[4]);
        if s.rotation.follow_direction {
            angle += self.direction;
        }
        // The flips take the low bits of the rotation draw, so the draw count stays five.
        let (mirror, half_turn) = s.rotation.flips((r[4] * (1u32 << 24) as f32) as u32);
        if half_turn {
            angle += 180.0;
        }
        Dab {
            x: p.x,
            y: p.y,
            radius: (s.diameter_at(pressure) * size * 0.5).max(0.05),
            alpha: (s.density_at(pressure) * density * opacity).clamp(0.0, 1.0),
            angle,
            thickness: thickness.clamp(0.01, 1.0),
            mirror,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dabs_follow_spacing() {
        let settings = BrushSettings {
            size: 10.0,
            spacing: 0.1,
            pressure_size: false,
            stabilization: 0,
            ..BrushSettings::g_pen()
        };
        let mut engine = StrokeEngine::new(&settings);
        let mut dabs = engine.add(StrokePoint::new(0.0, 0.0, 1.0));
        dabs.extend(engine.add(StrokePoint::new(50.0, 0.0, 1.0)));
        dabs.extend(engine.add(StrokePoint::new(100.0, 0.0, 1.0)));
        dabs.extend(engine.finish());
        assert_eq!(dabs.len(), 101);
        assert!((dabs.last().unwrap().x - 100.0).abs() < 1e-3);
    }

    #[test]
    fn short_moves_carry_distance() {
        let settings = BrushSettings { size: 20.0, spacing: 0.5, pressure_size: false, stabilization: 0, ..BrushSettings::g_pen() };
        let mut engine = StrokeEngine::new(&settings);
        let mut count = engine.add(StrokePoint::new(0.0, 0.0, 1.0)).len();
        for i in 1..=40 {
            count += engine.add(StrokePoint::new(i as f32 * 0.5, 0.0, 1.0)).len();
        }
        // 20 px travelled with 10 px spacing: start dab + 2.
        assert_eq!(count, 3);
    }

    fn line_dabs(settings: &BrushSettings, seed: u64, points: &[StrokePoint]) -> Vec<Dab> {
        let mut engine = StrokeEngine::with_seed(settings, seed);
        let mut dabs = Vec::new();
        for &p in points {
            dabs.extend(engine.add(p));
        }
        dabs.extend(engine.finish());
        dabs
    }

    fn horizontal(len: f32, seconds: f32) -> Vec<StrokePoint> {
        (0..=20).map(|i| StrokePoint::new(i as f32 * len / 20.0, 50.0, 1.0).at_time(i as f32 * seconds / 20.0)).collect()
    }

    fn plain() -> BrushSettings {
        BrushSettings { size: 10.0, spacing: 0.2, pressure_size: false, stabilization: 0, ..BrushSettings::g_pen() }
    }

    #[test]
    fn random_flips_mirror_some_dabs() {
        let line = |settings: &BrushSettings| {
            let mut engine = StrokeEngine::with_seed(settings, 7);
            let mut dabs = engine.add(StrokePoint::new(0.0, 0.0, 1.0));
            dabs.extend(engine.add(StrokePoint::new(400.0, 0.0, 1.0)));
            dabs
        };
        let plain_dabs = line(&plain());
        assert!(plain_dabs.iter().all(|d| !d.mirror && d.angle == 0.0));

        let flip_x = BrushSettings { rotation: crate::brush::RotationDynamics { random_flip_x: true, ..Default::default() }, ..plain() };
        let dabs = line(&flip_x);
        let mirrored = dabs.iter().filter(|d| d.mirror).count();
        assert!(mirrored > dabs.len() / 4 && mirrored < dabs.len() * 3 / 4, "{mirrored} of {}", dabs.len());
        assert!(dabs.iter().all(|d| d.angle == 0.0), "a left-right flip needs no turn");
        // Same draws as without flips: positions and sizes don't move.
        assert!(dabs.iter().zip(&plain_dabs).all(|(a, b)| a.x == b.x && a.radius == b.radius));

        // A top-bottom flip is a mirror plus a half turn.
        let flip_y = BrushSettings { rotation: crate::brush::RotationDynamics { random_flip_y: true, ..Default::default() }, ..plain() };
        assert!(line(&flip_y).iter().all(|d| d.mirror == (d.angle == 180.0)));
    }

    #[test]
    fn jitter_is_bounded_and_reproducible() {
        let settings = BrushSettings {
            size_dynamics: crate::brush::InputDynamics { jitter: 0.5, ..Default::default() },
            density_dynamics: crate::brush::InputDynamics { jitter: 1.0, ..Default::default() },
            rotation: crate::brush::RotationDynamics { jitter: 1.0, ..Default::default() },
            ..plain()
        };
        let a = line_dabs(&settings, 7, &horizontal(200.0, 1.0));
        assert_eq!(a, line_dabs(&settings, 7, &horizontal(200.0, 1.0)), "same seed, same stroke");
        assert_ne!(a, line_dabs(&settings, 8, &horizontal(200.0, 1.0)), "other seed, other stroke");
        assert!(a.iter().all(|d| (2.5..=5.0).contains(&d.radius)), "size in 50..100%");
        assert!(a.iter().all(|d| (0.0..=1.0).contains(&d.alpha) && d.angle.abs() <= 180.0));
        let spread = a.iter().map(|d| d.radius).fold(f32::MAX, f32::min);
        assert!(spread < 4.0, "jitter actually varies the size");
    }

    #[test]
    fn velocity_and_tilt_curves() {
        // Size shrinks with speed: full when still, 20% at FAST_SPEED.
        let settings = BrushSettings {
            size_dynamics: crate::brush::InputDynamics {
                velocity: Some(crate::brush::PressureCurve::from_points(vec![[0.0, 1.0], [1.0, 0.2]])),
                ..Default::default()
            },
            ..plain()
        };
        let slow = line_dabs(&settings, 1, &horizontal(300.0, 10.0)); // 30 px/s
        let fast = line_dabs(&settings, 1, &horizontal(300.0, 0.05)); // 6000 px/s
        assert!(slow.last().unwrap().radius > 4.9);
        assert!((fast.last().unwrap().radius - 1.0).abs() < 1e-4);

        // Density follows tilt.
        let settings = BrushSettings {
            density_dynamics: crate::brush::InputDynamics { tilt: Some(crate::brush::PressureCurve::linear()), ..Default::default() },
            ..plain()
        };
        let tilted: Vec<StrokePoint> = horizontal(100.0, 1.0).into_iter().map(|p| p.with_tilt(0.25)).collect();
        assert!(line_dabs(&settings, 1, &tilted).iter().all(|d| (d.alpha - 0.25).abs() < 1e-5));
    }

    #[test]
    fn tip_follows_stroke_direction() {
        let settings = BrushSettings { rotation: crate::brush::RotationDynamics { follow_direction: true, ..Default::default() }, ..plain() };
        let right = line_dabs(&settings, 1, &horizontal(100.0, 1.0));
        assert!(right[1..].iter().all(|d| d.angle.abs() < 1e-3));
        // Moving up the screen (y decreasing) is +90° counter-clockwise.
        let up: Vec<StrokePoint> = (0..=10).map(|i| StrokePoint::new(50.0, 100.0 - i as f32 * 10.0, 1.0)).collect();
        assert!(line_dabs(&settings, 1, &up)[1..].iter().all(|d| (d.angle - 90.0).abs() < 1e-3));
    }

    #[test]
    fn pressure_drives_opacity_thickness_and_rotation() {
        use crate::brush::{InputDynamics, PressureCurve, RotationDynamics};
        let settings = BrushSettings {
            opacity_dynamics: InputDynamics { pressure: Some(PressureCurve::linear()), ..Default::default() },
            thickness_dynamics: InputDynamics {
                pressure: Some(PressureCurve::from_points(vec![[0.0, 0.2], [1.0, 1.0]])),
                ..Default::default()
            },
            rotation: RotationDynamics { pressure: Some(PressureCurve::from_points(vec![[0.0, 0.0], [1.0, 0.25]])), ..Default::default() },
            ..plain()
        };
        let points: Vec<StrokePoint> = (0..=10).map(|i| StrokePoint::new(i as f32 * 10.0, 0.0, 0.5)).collect();
        let dabs = line_dabs(&settings, 1, &points);
        assert!(dabs.iter().all(|d| (d.alpha - 0.5).abs() < 1e-5), "opacity × pressure");
        assert!(dabs.iter().all(|d| (d.thickness - 0.6).abs() < 1e-5), "thickness from 0.2 at no pressure to 1");
        assert!(dabs.iter().all(|d| (d.angle - 45.0).abs() < 1e-3), "half pressure = 1/8 turn");
    }
}
