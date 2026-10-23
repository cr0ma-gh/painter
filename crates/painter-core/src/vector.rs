//! Vector layers: strokes are kept as input points and re-rasterised on change.

use crate::brush::Dab;
use crate::layer::{VectorLayer, VectorStroke};
use crate::raster::paint_stroke;
use crate::stroke::StrokeEngine;
use crate::tile::TileStore;

/// The dabs a recorded stroke produces (deterministic replay).
pub fn stroke_dabs(stroke: &VectorStroke) -> Vec<Dab> {
    let mut engine = StrokeEngine::with_seed(&stroke.settings, stroke.seed);
    let mut dabs = Vec::new();
    for &point in &stroke.points {
        dabs.extend(engine.add(point));
    }
    dabs.extend(engine.finish());
    dabs
}

/// Rebuilds the layer cache from its strokes.
pub fn rasterize(layer: &mut VectorLayer, canvas: (u32, u32)) {
    let mut cache = TileStore::new();
    for stroke in &layer.strokes {
        paint_stroke(&mut cache, &stroke_dabs(stroke), stroke.color, &stroke.settings, canvas, false, None);
    }
    for text in &layer.texts {
        crate::text::render(text, &mut cache, canvas);
    }
    layer.cache = cache;
}

/// Vector eraser: removes every stroke touched by the eraser dabs and
/// re-rasterises. Returns whether anything was removed.
pub fn erase_touching(layer: &mut VectorLayer, eraser: &[Dab], canvas: (u32, u32)) -> bool {
    if eraser.is_empty() {
        return false;
    }
    let bounds = |dabs: &[Dab]| {
        dabs.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, d| {
            [b[0].min(d.x - d.radius), b[1].min(d.y - d.radius), b[2].max(d.x + d.radius), b[3].max(d.y + d.radius)]
        })
    };
    let eb = bounds(eraser);
    let before = layer.strokes.len();
    layer.strokes.retain(|stroke| {
        let dabs = stroke_dabs(stroke);
        let sb = bounds(&dabs);
        if sb[0] > eb[2] || sb[2] < eb[0] || sb[1] > eb[3] || sb[3] < eb[1] {
            return true;
        }
        !dabs.iter().any(|d| eraser.iter().any(|e| (d.x - e.x).hypot(d.y - e.y) < d.radius + e.radius))
    });
    let changed = layer.strokes.len() != before;
    if changed {
        rasterize(layer, canvas);
    }
    changed
}

/// Where a crossing or a touch is along a stroke: segment `i`, fraction `u` of it.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
struct At {
    i: usize,
    u: f32,
}

impl At {
    fn key(self) -> f32 {
        self.i as f32 + self.u
    }
}

/// Where segments `a0`–`a1` and `b0`–`b1` cross: the fractions along each.
fn crossing(a0: [f32; 2], a1: [f32; 2], b0: [f32; 2], b1: [f32; 2]) -> Option<(f32, f32)> {
    let (r, s) = ([a1[0] - a0[0], a1[1] - a0[1]], [b1[0] - b0[0], b1[1] - b0[1]]);
    let den = r[0] * s[1] - r[1] * s[0];
    if den.abs() < 1e-9 {
        return None;
    }
    let q = [b0[0] - a0[0], b0[1] - a0[1]];
    let t = (q[0] * s[1] - q[1] * s[0]) / den;
    let u = (q[0] * r[1] - q[1] * r[0]) / den;
    ((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)).then_some((t, u))
}

/// A stroke's points as a polyline.
fn polyline(stroke: &VectorStroke) -> Vec<[f32; 2]> {
    stroke.points.iter().map(|p| [p.x, p.y]).collect()
}

fn bbox(line: &[[f32; 2]], pad: f32) -> [f32; 4] {
    line.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, p| [b[0].min(p[0] - pad), b[1].min(p[1] - pad), b[2].max(p[0] + pad), b[3].max(p[1] + pad)])
}

fn overlap(a: [f32; 4], b: [f32; 4]) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

/// The point at `at` along `points`, pressure and pen interpolated.
fn point_at(points: &[crate::smoothing::StrokePoint], at: At) -> crate::smoothing::StrokePoint {
    let a = points[at.i];
    let Some(&b) = points.get(at.i + 1) else { return a };
    let l = |x: f32, y: f32| x + (y - x) * at.u;
    crate::smoothing::StrokePoint { x: l(a.x, b.x), y: l(a.y, b.y), pressure: l(a.pressure, b.pressure), tilt: l(a.tilt, b.tilt), azimuth: a.azimuth, time: l(a.time, b.time) }
}

/// The stroke between two places along it (`None` when too short to keep).
fn piece(stroke: &VectorStroke, from: At, to: At) -> Option<VectorStroke> {
    let mut points = vec![point_at(&stroke.points, from)];
    points.extend(stroke.points[(from.i + 1).min(stroke.points.len())..=to.i.min(stroke.points.len() - 1)].iter().copied());
    points.push(point_at(&stroke.points, to));
    points.dedup_by(|a, b| (a.x - b.x).abs() < 1e-4 && (a.y - b.y).abs() < 1e-4);
    let length: f32 = points.windows(2).map(|w| (w[1].x - w[0].x).hypot(w[1].y - w[0].y)).sum();
    (points.len() >= 2 && length >= 0.5).then(|| VectorStroke { points, ..stroke.clone() })
}

/// Clip Studio's "erase up to intersection": each stroke the eraser touches
/// is cut where other strokes (or itself) cross it, and only the pieces the
/// eraser touched go; the rest stay as strokes of their own. Returns whether
/// anything changed.
pub fn erase_to_intersections(layer: &mut VectorLayer, eraser: &[Dab], canvas: (u32, u32)) -> bool {
    if eraser.is_empty() {
        return false;
    }
    let lines: Vec<Vec<[f32; 2]>> = layer.strokes.iter().map(polyline).collect();
    let boxes: Vec<[f32; 4]> = lines.iter().zip(&layer.strokes).map(|(l, s)| bbox(l, s.settings.size * 0.5)).collect();
    let eraser_box = eraser.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, d| {
        [b[0].min(d.x - d.radius), b[1].min(d.y - d.radius), b[2].max(d.x + d.radius), b[3].max(d.y + d.radius)]
    });
    let mut out = Vec::with_capacity(layer.strokes.len());
    let mut changed = false;
    for (k, stroke) in layer.strokes.iter().enumerate() {
        let line = &lines[k];
        if line.len() < 2 || !overlap(boxes[k], eraser_box) {
            out.push(stroke.clone());
            continue;
        }
        // Where the eraser touches the stroke's centre line (within its half width).
        let half = stroke.settings.size * 0.5;
        let mut touched: Vec<At> = Vec::new();
        for (i, w) in line.windows(2).enumerate() {
            let (a, b) = (w[0], w[1]);
            let d = [b[0] - a[0], b[1] - a[1]];
            let len2 = (d[0] * d[0] + d[1] * d[1]).max(1e-12);
            for e in eraser {
                let u = (((e.x - a[0]) * d[0] + (e.y - a[1]) * d[1]) / len2).clamp(0.0, 1.0);
                let (px, py) = (a[0] + d[0] * u, a[1] + d[1] * u);
                if (e.x - px).hypot(e.y - py) <= e.radius + half {
                    touched.push(At { i, u });
                }
            }
        }
        if touched.is_empty() {
            out.push(stroke.clone());
            continue;
        }
        changed = true;
        // Where other strokes, and the stroke itself, cross it.
        let mut cuts: Vec<At> = Vec::new();
        for (j, other) in lines.iter().enumerate() {
            if !overlap(boxes[k], boxes[j]) {
                continue;
            }
            for (i, w) in line.windows(2).enumerate() {
                for (m, v) in other.windows(2).enumerate() {
                    // Itself: not with neighbouring segments (they share a point).
                    if j == k && i.abs_diff(m) <= 1 {
                        continue;
                    }
                    if let Some((t, _)) = crossing(w[0], w[1], v[0], v[1]) {
                        cuts.push(At { i, u: t });
                    }
                }
            }
        }
        cuts.sort_by(|a, b| a.key().total_cmp(&b.key()));
        // Pieces between consecutive cuts (and the ends); keep the untouched ones.
        let start = At { i: 0, u: 0.0 };
        let end = At { i: line.len() - 2, u: 1.0 };
        let mut bounds = vec![start];
        bounds.extend(cuts.iter().copied().filter(|c| c.key() > start.key() + 1e-4 && c.key() < end.key() - 1e-4));
        bounds.push(end);
        for w in bounds.windows(2) {
            let (from, to) = (w[0], w[1]);
            let hit = touched.iter().any(|t| t.key() >= from.key() - 1e-5 && t.key() <= to.key() + 1e-5);
            if !hit {
                out.extend(piece(stroke, from, to));
            }
        }
    }
    if changed {
        layer.strokes = out;
        rasterize(layer, canvas);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::BrushSettings;
    use crate::smoothing::StrokePoint;

    fn stroke(y: f32) -> VectorStroke {
        VectorStroke {
            points: (0..20).map(|i| StrokePoint::new(5.0 + i as f32 * 4.0, y, 1.0)).collect(),
            settings: BrushSettings { stabilization: 0, ..BrushSettings::g_pen() },
            color: [0.0, 0.0, 0.0],
            seed: 0,
        }
    }

    #[test]
    fn eraser_removes_only_touched_strokes() {
        let mut layer = VectorLayer { strokes: vec![stroke(10.0), stroke(60.0)], cache: TileStore::new(), ..Default::default() };
        rasterize(&mut layer, (100, 100));
        assert!(layer.cache.pixel(40, 10)[3] > 200);
        let eraser = [Dab { x: 40.0, y: 12.0, radius: 3.0, alpha: 1.0, angle: 0.0, thickness: 1.0, mirror: false }];
        assert!(erase_touching(&mut layer, &eraser, (100, 100)));
        assert_eq!(layer.strokes.len(), 1);
        assert_eq!(layer.cache.pixel(40, 10)[3], 0);
        assert!(layer.cache.pixel(40, 60)[3] > 200);
        assert!(!erase_touching(&mut layer, &eraser, (100, 100)));
    }
}

#[cfg(test)]
mod intersection_tests {
    use super::*;
    use crate::brush::BrushSettings;
    use crate::smoothing::StrokePoint;

    fn line(from: [f32; 2], to: [f32; 2]) -> VectorStroke {
        VectorStroke {
            points: (0..=20).map(|i| i as f32 / 20.0).map(|t| StrokePoint::new(from[0] + (to[0] - from[0]) * t, from[1] + (to[1] - from[1]) * t, 1.0)).collect(),
            settings: BrushSettings { stabilization: 0, size: 4.0, ..BrushSettings::g_pen() },
            color: [0.0; 3],
            seed: 0,
        }
    }

    fn dab(x: f32, y: f32) -> Dab {
        Dab { x, y, radius: 2.0, alpha: 1.0, angle: 0.0, thickness: 1.0, mirror: false }
    }

    #[test]
    fn only_the_overhang_past_a_crossing_goes() {
        // A horizontal line crossed by a vertical one at x = 30; erase the bit left of it.
        let mut layer = VectorLayer { strokes: vec![line([10.0, 50.0], [90.0, 50.0]), line([30.0, 10.0], [30.0, 90.0])], cache: TileStore::new(), ..Default::default() };
        assert!(erase_to_intersections(&mut layer, &[dab(15.0, 50.0)], (100, 100)));
        assert_eq!(layer.strokes.len(), 2, "the vertical line and what's right of the crossing");
        let rest = &layer.strokes[0];
        assert!((rest.points[0].x - 30.0).abs() < 0.01, "starts at the crossing: {}", rest.points[0].x);
        assert!((rest.points.last().unwrap().x - 90.0).abs() < 0.01);
        assert_eq!(layer.strokes[1].points.len(), 21, "the vertical line untouched");
        // Pixels: the overhang is gone, the rest is drawn.
        assert_eq!(layer.cache.pixel(15, 50)[3], 0);
        assert!(layer.cache.pixel(60, 50)[3] > 200);
    }

    #[test]
    fn a_middle_piece_between_two_crossings() {
        let mut layer = VectorLayer {
            strokes: vec![line([10.0, 50.0], [90.0, 50.0]), line([30.0, 10.0], [30.0, 90.0]), line([70.0, 10.0], [70.0, 90.0])],
            cache: TileStore::new(),
            texts: Vec::new(),
        };
        assert!(erase_to_intersections(&mut layer, &[dab(50.0, 50.0)], (100, 100)));
        // Left end and right end stay as two strokes, plus the two verticals.
        assert_eq!(layer.strokes.len(), 4);
        let ends: Vec<(f32, f32)> = layer.strokes.iter().filter(|s| (s.points[0].y - 50.0).abs() < 0.01).map(|s| (s.points[0].x, s.points.last().unwrap().x)).collect();
        assert_eq!(ends.len(), 2);
        assert!(ends.iter().any(|&(a, b)| (a - 10.0).abs() < 0.01 && (b - 30.0).abs() < 0.01));
        assert!(ends.iter().any(|&(a, b)| (a - 70.0).abs() < 0.01 && (b - 90.0).abs() < 0.01));
        // Nothing crossing: the whole stroke goes.
        let mut lone = VectorLayer { strokes: vec![line([10.0, 50.0], [90.0, 50.0])], cache: TileStore::new(), ..Default::default() };
        assert!(erase_to_intersections(&mut lone, &[dab(50.0, 50.0)], (100, 100)));
        assert!(lone.strokes.is_empty());
        assert!(!erase_to_intersections(&mut lone, &[dab(50.0, 50.0)], (100, 100)));
    }
}

// ---- editing points ---------------------------------------------------------------

/// The points worth handles on a stroke recorded as many input points: the
/// ones a Ramer–Douglas–Peucker simplification keeps within `eps` pixels,
/// always the ends. Indices into `stroke.points`, increasing.
pub fn control_points(stroke: &VectorStroke, eps: f32) -> Vec<usize> {
    fn rdp(p: &[crate::smoothing::StrokePoint], lo: usize, hi: usize, eps: f32, out: &mut Vec<usize>) {
        if hi <= lo + 1 {
            return;
        }
        let (a, b) = (p[lo], p[hi]);
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let len = dx.hypot(dy).max(1e-6);
        let (mut far, mut at) = (0.0f32, lo);
        for (i, q) in p.iter().enumerate().take(hi).skip(lo + 1) {
            let d = ((q.x - a.x) * dy - (q.y - a.y) * dx).abs() / len;
            if d > far {
                (far, at) = (d, i);
            }
        }
        if far > eps {
            rdp(p, lo, at, eps, out);
            out.push(at);
            rdp(p, at, hi, eps, out);
        }
    }
    let n = stroke.points.len();
    if n == 0 {
        return Vec::new();
    }
    let mut out = vec![0];
    rdp(&stroke.points, 0, n - 1, eps.max(0.1), &mut out);
    if n > 1 {
        out.push(n - 1);
    }
    out
}

/// Moves control point `c` (an index into `controls`) by `delta`, dragging the
/// points around it along: fully at the control, easing to nothing at the
/// neighbouring controls, so the line bends smoothly.
pub fn move_control(points: &mut [crate::smoothing::StrokePoint], controls: &[usize], c: usize, delta: [f32; 2]) {
    let Some(&at) = controls.get(c) else { return };
    let lo = if c > 0 { controls[c - 1] } else { at };
    let hi = controls.get(c + 1).copied().unwrap_or(at);
    for (i, p) in points.iter_mut().enumerate().take(hi + 1).skip(lo) {
        let w = if i == at {
            1.0
        } else if i < at {
            (i - lo) as f32 / (at - lo) as f32
        } else {
            (hi - i) as f32 / (hi - at) as f32
        };
        // Smooth at both ends of the bend.
        let w = w * w * (3.0 - 2.0 * w);
        p.x += delta[0] * w;
        p.y += delta[1] * w;
    }
}

/// Takes control point `c` away: the points between its neighbours follow
/// the straight line between them (the ends can't be removed).
pub fn remove_control(points: &mut [crate::smoothing::StrokePoint], controls: &[usize], c: usize) -> bool {
    if c == 0 || c + 1 >= controls.len() {
        return false;
    }
    let (lo, hi) = (controls[c - 1], controls[c + 1]);
    let (a, b) = (points[lo], points[hi]);
    for (i, p) in points.iter_mut().enumerate().take(hi).skip(lo + 1) {
        let t = (i - lo) as f32 / (hi - lo) as f32;
        p.x = a.x + (b.x - a.x) * t;
        p.y = a.y + (b.y - a.y) * t;
    }
    true
}

/// The stroke whose line passes nearest `p` (within `reach` plus its half
/// width), topmost first among equals: its index and distance.
pub fn stroke_at(layer: &VectorLayer, p: [f32; 2], reach: f32) -> Option<(usize, f32)> {
    let mut best: Option<(usize, f32)> = None;
    for (k, s) in layer.strokes.iter().enumerate() {
        let half = s.settings.size * 0.5;
        let line = polyline(s);
        let d = match line.as_slice() {
            [] => continue,
            [only] => (only[0] - p[0]).hypot(only[1] - p[1]),
            _ => line
                .windows(2)
                .map(|w| {
                    let (a, b) = (w[0], w[1]);
                    let d = [b[0] - a[0], b[1] - a[1]];
                    let u = (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / (d[0] * d[0] + d[1] * d[1]).max(1e-12)).clamp(0.0, 1.0);
                    (a[0] + d[0] * u - p[0]).hypot(a[1] + d[1] * u - p[1])
                })
                .fold(f32::MAX, f32::min),
        };
        let d = (d - half).max(0.0);
        if d <= reach && best.is_none_or(|(_, b)| d <= b) {
            best = Some((k, d));
        }
    }
    best
}

#[cfg(test)]
mod editing_tests {
    use super::*;
    use crate::brush::BrushSettings;
    use crate::smoothing::StrokePoint;

    fn stroke(points: Vec<[f32; 2]>) -> VectorStroke {
        VectorStroke {
            points: points.into_iter().map(|[x, y]| StrokePoint::new(x, y, 1.0)).collect(),
            settings: BrushSettings { size: 4.0, ..BrushSettings::g_pen() },
            color: [0.0; 3],
            seed: 0,
        }
    }

    #[test]
    fn controls_bend_and_straighten() {
        // An L drawn as 21 points: its controls are the two ends and the corner.
        let mut pts: Vec<[f32; 2]> = (0..=10).map(|i| [i as f32 * 10.0, 0.0]).collect();
        pts.extend((1..=10).map(|i| [100.0, i as f32 * 10.0]));
        let mut s = stroke(pts);
        let controls = control_points(&s, 1.0);
        assert_eq!(controls, vec![0, 10, 20]);
        // Pull the corner out: it moves fully, the ends stay, points between ease.
        move_control(&mut s.points, &controls, 1, [10.0, -10.0]);
        assert_eq!((s.points[10].x, s.points[10].y), (110.0, -10.0));
        assert_eq!((s.points[0].x, s.points[0].y), (0.0, 0.0));
        assert_eq!((s.points[20].x, s.points[20].y), (100.0, 100.0));
        let mid = s.points[5];
        assert!(mid.x > 50.0 && mid.x < 60.0 && mid.y < 0.0 && mid.y > -10.0, "{mid:?}");
        // Removing the corner makes the line straight from end to end.
        assert!(remove_control(&mut s.points, &controls, 1));
        assert!((s.points[10].x - 50.0).abs() < 1e-4 && (s.points[10].y - 50.0).abs() < 1e-4);
        assert!(!remove_control(&mut s.points, &controls, 0), "ends stay");
        // Picking: the stroke within reach of its edge.
        let layer = VectorLayer { strokes: vec![stroke(vec![[0.0, 0.0], [100.0, 0.0]]), stroke(vec![[0.0, 50.0], [100.0, 50.0]])], cache: TileStore::new(), ..Default::default() };
        assert_eq!(stroke_at(&layer, [50.0, 46.0], 3.0).map(|x| x.0), Some(1));
        assert!(stroke_at(&layer, [50.0, 25.0], 3.0).is_none());
    }
}
