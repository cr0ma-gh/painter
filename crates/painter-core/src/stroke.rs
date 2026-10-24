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
    /// Distance along the stroke at the dab being made (for the starting taper).
    travelled: f32,
    /// Length of the stroke's path up to the last point walked to.
    path: f32,
    /// A second generator for scattering, so brushes without it draw the same
    /// random numbers as before it existed.
    scatter_rng: u64,
    /// The last few input points, for the curve through them (see `feed`).
    curve: Vec<StrokePoint>,
}

impl StrokeEngine {
    pub fn new(settings: &BrushSettings) -> Self {
        Self::with_seed(settings, DEFAULT_SEED)
    }

    /// The dab a pen at `point` would stamp right now (pressure, tilt and lean
    /// included; no speed, randomness as for a stroke's first dab): what a
    /// brush cursor or tip preview shows.
    pub fn preview_dab(settings: &BrushSettings, point: StrokePoint) -> Dab {
        Self::with_seed(settings, DEFAULT_SEED).dab_at(point)
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
            travelled: 0.0,
            path: 0.0,
            scatter_rng: (seed.rotate_left(17) ^ 0xD1B5_4A32_D192_ED03) | 1,
            curve: Vec::new(),
        }
    }

    pub fn add(&mut self, p: StrokePoint) -> Vec<Dab> {
        let mut out = Vec::new();
        if let Some(target) = self.smoother.push(p) {
            self.feed(target, &mut out);
        }
        out
    }

    pub fn finish(&mut self) -> Vec<Dab> {
        let mut out = Vec::new();
        for target in self.smoother.finish() {
            self.feed(target, &mut out);
        }
        // The last segment waited for a point after it; draw it now.
        let n = self.curve.len();
        if n >= 2 {
            let p0 = if n >= 3 { self.curve[n - 3] } else { reflect(self.curve[n - 1], self.curve[n - 2]) };
            let (p1, p2) = (self.curve[n - 2], self.curve[n - 1]);
            let p3 = if n >= 3 { extrapolate(p2, p1, p0) } else { reflect(p1, p2) };
            self.spline_segment(p0, p1, p2, p3, &mut out);
        }
        self.curve.clear();
        out
    }

    /// Input points become a curve, not a polyline: a centripetal
    /// Catmull-Rom spline through them, so sparse samples (a fast stroke
    /// at a low event rate) still draw round. Each segment is drawn once the
    /// point after it is known, one sample behind the pen.
    fn feed(&mut self, p: StrokePoint, out: &mut Vec<Dab>) {
        let Some(&last) = self.curve.last() else {
            self.curve.push(p);
            self.walk_to(p, out);
            return;
        };
        if last.distance(p) < 1e-3 {
            // Same place: keep the newest pressure / time for the next segment.
            if let Some(l) = self.curve.last_mut() {
                *l = StrokePoint { x: l.x, y: l.y, ..p };
            }
            return;
        }
        self.curve.push(p);
        if self.curve.len() > 4 {
            self.curve.remove(0);
        }
        let n = self.curve.len();
        if n >= 3 {
            let (p1, p2, p3) = (self.curve[n - 3], self.curve[n - 2], self.curve[n - 1]);
            let p0 = if n >= 4 { self.curve[n - 4] } else { extrapolate(p1, p2, p3) };
            self.spline_segment(p0, p1, p2, p3, out);
        }
    }

    /// Walks the curve from `p1` to `p2` (with neighbours `p0`, `p3`) in
    /// short straight steps; pressure, tilt and time change evenly.
    fn spline_segment(&mut self, p0: StrokePoint, p1: StrokePoint, p2: StrokePoint, p3: StrokePoint, out: &mut Vec<Dab>) {
        let chord = p1.distance(p2);
        if chord < 1e-4 {
            return;
        }
        // About one step per pixel of the chord.
        let steps = chord.ceil().clamp(1.0, 4096.0) as usize;
        let knot = |a: StrokePoint, b: StrokePoint| a.distance(b).max(1e-4).sqrt();
        let (t1, t2, t3) = {
            let t1 = knot(p0, p1);
            let t2 = t1 + knot(p1, p2);
            (t1, t2, t2 + knot(p2, p3))
        };
        let at = |t: f32| -> [f32; 2] {
            // Barry-Goldman pyramid of the centripetal Catmull-Rom.
            let mix = |a: [f32; 2], b: [f32; 2], ta: f32, tb: f32| {
                let w = (t - ta) / (tb - ta);
                [a[0] + (b[0] - a[0]) * w, a[1] + (b[1] - a[1]) * w]
            };
            let [q0, q1, q2, q3] = [p0, p1, p2, p3].map(|p| [p.x, p.y]);
            let a1 = mix(q0, q1, 0.0, t1);
            let a2 = mix(q1, q2, t1, t2);
            let a3 = mix(q2, q3, t2, t3);
            let b1 = mix(a1, a2, 0.0, t2);
            let b2 = mix(a2, a3, t1, t3);
            mix(b1, b2, t1, t2)
        };
        for i in 1..=steps {
            let u = i as f32 / steps as f32;
            let [x, y] = if i == steps { [p2.x, p2.y] } else { at(t1 + (t2 - t1) * u) };
            let base = p1.lerp(p2, u);
            self.walk_to(StrokePoint { x, y, ..base }, out);
        }
    }

    /// Uniform in 0..1 (xorshift64*).
    fn random(&mut self) -> f32 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Uniform in 0..1 from the scattering generator.
    fn scatter_random(&mut self) -> f32 {
        self.scatter_rng ^= self.scatter_rng >> 12;
        self.scatter_rng ^= self.scatter_rng << 25;
        self.scatter_rng ^= self.scatter_rng >> 27;
        (self.scatter_rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Adds `dab` with the starting taper and scattering applied.
    fn emit(&mut self, mut dab: Dab, out: &mut Vec<Dab>) {
        let taper = self.settings.taper_start;
        if taper > 0.0 {
            let t = (self.travelled / taper).clamp(0.0, 1.0).max(0.05);
            dab.radius = (dab.radius * t).max(0.05);
            dab.alpha *= t;
        }
        let scatter = self.settings.scatter;
        if !scatter.is_on() {
            out.push(dab);
            return;
        }
        let count = scatter.count.clamp(1, 16);
        let fewer = (count as f32 * scatter.count_jitter.clamp(0.0, 1.0) * self.scatter_random()).floor() as u32;
        let reach = scatter.amount.max(0.0) * dab.radius * 2.0;
        // Across the stroke (and along it with `both_axes`); y points down on screen.
        let (sin, cos) = self.direction.to_radians().sin_cos();
        let (along, across) = ([cos, -sin], [sin, cos]);
        for _ in 0..(count - fewer).max(1) {
            let a = (self.scatter_random() * 2.0 - 1.0) * reach;
            let b = if scatter.both_axes { (self.scatter_random() * 2.0 - 1.0) * reach } else { 0.0 };
            out.push(Dab { x: dab.x + across[0] * a + along[0] * b, y: dab.y + across[1] * a + along[1] * b, ..dab });
        }
    }

    fn walk_to(&mut self, to: StrokePoint, out: &mut Vec<Dab>) {
        let Some(from) = self.last else {
            let dab = self.dab_at(to);
            self.emit(dab, out);
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
            self.travelled = self.path + pos;
            let dab = self.dab_at(from.lerp(to, pos / len));
            self.emit(dab, out);
        }
        self.path += len;
        self.last = Some(to);
    }

    fn dab_at(&mut self, p: StrokePoint) -> Dab {
        // Always five draws per dab, so the sequence doesn't depend on which dynamics are on.
        let r: [f32; 5] = std::array::from_fn(|_| self.random());
        let s = &self.settings;
        let (pressure, velocity, tilt) = (p.pressure, self.velocity, p.tilt);
        let mut size = s.size_dynamics.factor(pressure, velocity, tilt, r[0]);
        let mut density = s.density_dynamics.factor(pressure, velocity, tilt, r[1]);
        let opacity = s.opacity_dynamics.factor(pressure, velocity, tilt, r[2]);
        let mut thickness = s.thickness_dynamics.factor(pressure, velocity, tilt, r[3]);
        let mut angle = s.rotation.offset(pressure, velocity, tilt, r[4]);
        if s.rotation.follow_direction {
            angle += self.direction;
        }
        // Where the pen leans, as a tip angle (counter-clockwise from the right):
        // the azimuth is clockwise from the top of the screen.
        let lean_angle = 90.0 - p.azimuth;
        let leaning = tilt > 0.02;
        if s.rotation.follow_tilt && leaning {
            angle += lean_angle;
        }
        let side = (s.tilt_side.clamp(0.0, 1.0) * pencil_side(tilt)).clamp(0.0, 1.0);
        if side > 0.0 {
            // The tip lies down along the lean, as Clip Studio's pencils do: a
            // broad band across the lean (up to 4× at full tilt), flatter
            // along it, barely lighter.
            size *= 1.0 + 3.0 * side;
            thickness *= 1.0 - 0.6 * side;
            density *= 1.0 - 0.1 * side;
            if !s.rotation.follow_tilt {
                angle += lean_angle;
            }
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

/// The point mirrored from `p` through `centre` (for a curve's missing end neighbour).
fn reflect(p: StrokePoint, centre: StrokePoint) -> StrokePoint {
    StrokePoint { x: 2.0 * centre.x - p.x, y: 2.0 * centre.y - p.y, ..centre }
}

/// The point before `a` on the parabola through `a`, `b`, `c` (evenly
/// spaced): a curve's missing end neighbour that keeps its bend.
fn extrapolate(a: StrokePoint, b: StrokePoint, c: StrokePoint) -> StrokePoint {
    StrokePoint { x: 3.0 * a.x - 3.0 * b.x + c.x, y: 3.0 * a.y - 3.0 * b.y + c.y, ..a }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scatter_and_starting_taper() {
        let base = BrushSettings { size: 10.0, spacing: 0.5, pressure_size: false, stabilization: 0, ..BrushSettings::g_pen() };
        let run = |settings: &BrushSettings| {
            let mut e = StrokeEngine::with_seed(settings, 3);
            let mut dabs = e.add(StrokePoint::new(0.0, 100.0, 1.0));
            dabs.extend(e.add(StrokePoint::new(200.0, 100.0, 1.0)));
            dabs.extend(e.finish());
            dabs
        };
        let plain = run(&base);
        // Scattered across a horizontal stroke: y spreads, x stays on the dab, count multiplies.
        let sprayed = run(&BrushSettings { scatter: crate::Scatter { amount: 2.0, count: 3, ..Default::default() }, ..base.clone() });
        assert_eq!(sprayed.len(), plain.len() * 3);
        assert!(sprayed.iter().all(|d| (d.y - 100.0).abs() <= 20.0 + 1e-3));
        assert!(sprayed.iter().any(|d| (d.y - 100.0).abs() > 2.0), "some land off the line");
        assert!(sprayed.chunks(3).zip(&plain).all(|(c, p)| c.iter().all(|d| (d.x - p.x).abs() < 1e-3)));
        // The ordinary dabs don't change when scattering is off (own random numbers).
        assert_eq!(run(&base), plain);
        // Starting taper: small and light at first, full size after the distance.
        let tapered = run(&BrushSettings { taper_start: 50.0, ..base.clone() });
        assert!(tapered[0].radius < plain[0].radius * 0.2 && tapered[0].alpha < plain[0].alpha * 0.2);
        let late = tapered.iter().zip(&plain).find(|(d, _)| d.x > 60.0).unwrap();
        assert_eq!(late.0.radius, late.1.radius);
        // Colour jitter keeps colours valid and does nothing at zero.
        let j = crate::ColorJitter { hue: 0.5, saturation: 0.3, value: 0.3 };
        let c = j.apply([0.8, 0.2, 0.1], [0.9, 0.1, 0.5]);
        assert!(c.iter().all(|v| (0.0..=1.0).contains(v)) && c != [0.8, 0.2, 0.1]);
        assert_eq!(crate::ColorJitter::default().apply([0.8, 0.2, 0.1], [0.9, 0.1, 0.5]), [0.8, 0.2, 0.1]);
        let back = crate::brush::hsv_to_rgb(crate::brush::rgb_to_hsv([0.3, 0.6, 0.9]));
        assert!((back[0] - 0.3).abs() < 1e-5 && (back[2] - 0.9).abs() < 1e-5);
    }

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
    fn sparse_samples_draw_a_curve_through_them() {
        // Five samples on a circle of radius 100, as a fast stroke at a low
        // event rate gives: the dabs stay near the circle, not on its chords.
        let settings = BrushSettings { size: 4.0, spacing: 0.25, pressure_size: false, stabilization: 0, ..BrushSettings::g_pen() };
        let mut engine = StrokeEngine::new(&settings);
        let mut dabs = Vec::new();
        for k in 0..=4 {
            let a = (k as f32 * 30.0).to_radians();
            dabs.extend(engine.add(StrokePoint::new(100.0 * a.cos(), 100.0 * a.sin(), 1.0)));
        }
        dabs.extend(engine.finish());
        let worst = dabs.iter().map(|d| (d.x.hypot(d.y) - 100.0).abs()).fold(0.0f32, f32::max);
        // A 30° chord sags 3.4 px below the arc; the curve stays well within that.
        assert!(worst < 1.0, "off the circle by {worst}");
        // It still passes through every sample and ends at the last one.
        let last = dabs.last().unwrap();
        let end = 120f32.to_radians();
        assert!((last.x - 100.0 * end.cos()).abs() < 2.0 && (last.y - 100.0 * end.sin()).abs() < 2.0);
        // A straight line stays straight.
        let mut engine = StrokeEngine::new(&settings);
        let mut line = engine.add(StrokePoint::new(0.0, 0.0, 1.0));
        for x in [30.0, 75.0, 90.0, 200.0] {
            line.extend(engine.add(StrokePoint::new(x, 0.0, 1.0)));
        }
        line.extend(engine.finish());
        assert!(line.iter().all(|d| d.y.abs() < 1e-3) && line.windows(2).all(|w| w[1].x > w[0].x));
    }

    #[test]
    fn short_moves_carry_distance() {
        let settings = BrushSettings { size: 20.0, spacing: 0.5, pressure_size: false, stabilization: 0, ..BrushSettings::g_pen() };
        let mut engine = StrokeEngine::new(&settings);
        let mut count = engine.add(StrokePoint::new(0.0, 0.0, 1.0)).len();
        for i in 1..=40 {
            count += engine.add(StrokePoint::new(i as f32 * 0.5, 0.0, 1.0)).len();
        }
        count += engine.finish().len();
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
            dabs.extend(engine.finish());
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
    fn leaning_pens_turn_and_lay_the_tip_down() {
        // A pen leaning to the right (azimuth 90°), half way down.
        let leaning: Vec<StrokePoint> = horizontal(100.0, 1.0).into_iter().map(|p| p.with_tilt(0.5).with_azimuth(90.0)).collect();
        let upright = line_dabs(&plain(), 1, &horizontal(100.0, 1.0));
        // Follow tilt: the tip turns to the lean (0° = pointing right), nothing else changes.
        let follow = BrushSettings { rotation: crate::brush::RotationDynamics { follow_tilt: true, ..Default::default() }, ..plain() };
        let dabs = line_dabs(&follow, 1, &leaning);
        assert!(dabs.iter().all(|d| d.angle.abs() < 1e-3 && d.thickness == 1.0));
        // Upright pens don't turn the tip.
        let still: Vec<StrokePoint> = horizontal(100.0, 1.0).into_iter().map(|p| p.with_azimuth(90.0)).collect();
        assert!(line_dabs(&follow, 1, &still).iter().all(|d| d.angle == 0.0));
        // Pencil side: a normal grip draws a plain line; laid down, the tip
        // goes wider, flatter across, lighter, along the lean; more with more tilt.
        let side = BrushSettings { tilt_side: 1.0, ..plain() };
        let grip = line_dabs(&side, 1, &leaning)[3];
        assert_eq!((grip.radius, grip.thickness), (upright[3].radius, upright[3].thickness), "half tilt: no side yet");
        let laid: Vec<StrokePoint> = horizontal(100.0, 1.0).into_iter().map(|p| p.with_tilt(0.9).with_azimuth(90.0)).collect();
        let (d, u) = (line_dabs(&side, 1, &laid)[3], upright[3]);
        assert!(d.radius > u.radius * 2.0 && d.thickness < 0.75 && d.alpha < u.alpha && d.angle.abs() < 1e-3);
        let flatter: Vec<StrokePoint> = horizontal(100.0, 1.0).into_iter().map(|p| p.with_tilt(1.0).with_azimuth(90.0)).collect();
        assert!(line_dabs(&side, 1, &flatter)[3].thickness < d.thickness);
        assert_eq!(pencil_side(0.6), 0.0);
        assert_eq!(pencil_side(1.0), 1.0);
        assert!(pencil_side(0.8) < 0.5, "comes in slowly");
        // The smoother keeps the lean direction.
        let smoothed = BrushSettings { stabilization: 30, ..follow };
        assert!(line_dabs(&smoothed, 1, &leaning).iter().all(|d| d.angle.abs() < 1e-3));
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

/// How far the tip lies down for a tilt (0 upright ..= 1 at 60° of lean).
/// A normal grip (up to about 36° of lean) draws a plain line, as in Clip
/// Studio; past it the side comes in, slowly at first, fully with the pen
/// laid down.
pub fn pencil_side(tilt: f32) -> f32 {
    const GRIP: f32 = 0.6;
    let t = ((tilt - GRIP) / (1.0 - GRIP)).clamp(0.0, 1.0);
    t * t
}
