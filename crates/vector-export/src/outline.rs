//! Vector-layer strokes as SVG shapes.
//!
//! A stroke's width varies with pen pressure, which an SVG `stroke-width`
//! can't express. [`StrokeMode::FilledOutline`] keeps it: the stroke is
//! replayed into its dabs (same engine and seed as on the canvas), the dab
//! centres and radii give left and right edges, and round caps close the
//! shape, which is then curve-fitted like a traced contour.
//! [`StrokeMode::Centerline`] gives an editable line of the average width.

use painter_core::vector::stroke_dabs;
use painter_core::{Dab, VectorStroke};

use crate::path::{Point, Segment, StrokeStyle, VectorPath};
use crate::trace::{fit_curves, simplify_closed};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrokeMode {
    /// A filled shape following the pressure profile.
    FilledOutline,
    /// A single stroked centre line with round caps; loses pressure.
    Centerline,
}

/// Outline points per cap (half circle).
const CAP_STEPS: usize = 8;

pub fn stroke_path(stroke: &VectorStroke, mode: StrokeMode, simplify: f32) -> Option<VectorPath> {
    let dabs = stroke_dabs(stroke);
    if dabs.is_empty() {
        return None;
    }
    let opacity = stroke.settings.opacity;
    match mode {
        StrokeMode::FilledOutline => {
            let polygon = outline_polygon(&dabs);
            let polygon = simplify_closed(&polygon, simplify.min(0.5));
            // Everything along a stroke outline is smooth except the cap joins.
            let segments = fit_curves(&polygon, 100.0);
            Some(VectorPath::filled(segments, stroke.color, opacity))
        }
        StrokeMode::Centerline => {
            let mut segments = vec![Segment::MoveTo([dabs[0].x, dabs[0].y])];
            segments.extend(dabs.iter().skip(1).map(|d| Segment::LineTo([d.x, d.y])));
            let width = 2.0 * dabs.iter().map(|d| d.radius).sum::<f32>() / dabs.len() as f32;
            Some(VectorPath::stroked(segments, StrokeStyle { color: stroke.color, width, opacity }))
        }
    }
}

/// Left edge forward, a round cap, right edge back, a round cap.
pub fn outline_polygon(dabs: &[Dab]) -> Vec<Point> {
    let circle = |d: &Dab, from: f32, to: f32| -> Vec<Point> {
        (0..=CAP_STEPS)
            .map(|k| {
                let a = from + (to - from) * k as f32 / CAP_STEPS as f32;
                [d.x + d.radius * a.cos(), d.y + d.radius * a.sin()]
            })
            .collect()
    };
    if dabs.len() == 1 {
        let mut c = circle(&dabs[0], 0.0, std::f32::consts::TAU);
        c.pop();
        return c;
    }
    let n = dabs.len();
    let tangent = |i: usize| {
        let (a, b) = (&dabs[i.saturating_sub(1)], &dabs[(i + 1).min(n - 1)]);
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let len = (dx * dx + dy * dy).sqrt().max(1e-6);
        [dx / len, dy / len]
    };
    let mut left = Vec::with_capacity(n);
    let mut right = Vec::with_capacity(n);
    for (i, d) in dabs.iter().enumerate() {
        let t = tangent(i);
        // y down: (−t.y, t.x) points to the right of travel.
        let normal = [-t[1], t[0]];
        right.push([d.x + normal[0] * d.radius, d.y + normal[1] * d.radius]);
        left.push([d.x - normal[0] * d.radius, d.y - normal[1] * d.radius]);
    }
    let angle = |t: Point| t[1].atan2(t[0]);
    let (start, end) = (tangent(0), tangent(n - 1));
    let pi = std::f32::consts::PI;
    let half = std::f32::consts::FRAC_PI_2;
    let mut polygon = left;
    // End cap: from the left side round the front to the right side.
    polygon.extend(circle(&dabs[n - 1], angle(end) - half, angle(end) + half).into_iter().skip(1));
    polygon.extend(right.into_iter().rev().skip(1));
    // Start cap: from the right side round the back to the left side.
    let mut cap = circle(&dabs[0], angle(start) + half, angle(start) + half + pi);
    cap.pop();
    polygon.extend(cap.into_iter().skip(1));
    polygon
}

#[cfg(test)]
mod tests {
    use super::*;
    use painter_core::{BrushSettings, StrokePoint};

    fn stroke(pressures: &[f32]) -> VectorStroke {
        let points = pressures
            .iter()
            .enumerate()
            .map(|(i, &p)| StrokePoint::new(10.0 + i as f32 * 10.0, 50.0, p))
            .collect();
        VectorStroke {
            points,
            settings: BrushSettings { size: 20.0, stabilization: 0, ..BrushSettings::g_pen() },
            color: [0.1, 0.2, 0.3],
            seed: 1,
            engine: painter_core::VECTOR_ENGINE,
        }
    }

    #[test]
    fn outline_follows_pressure() {
        let s = stroke(&[0.1, 0.5, 1.0, 1.0, 1.0, 0.5, 0.1]);
        let dabs = stroke_dabs(&s);
        let polygon = outline_polygon(&dabs);
        // Thicker in the middle than near the ends.
        let width_at = |x: f32| {
            let ys: Vec<f32> = polygon.iter().filter(|p| (p[0] - x).abs() < 2.0).map(|p| p[1]).collect();
            ys.iter().cloned().fold(f32::MIN, f32::max) - ys.iter().cloned().fold(f32::MAX, f32::min)
        };
        assert!(width_at(40.0) > width_at(15.0) * 1.5, "{} vs {}", width_at(40.0), width_at(15.0));

        let path = stroke_path(&s, StrokeMode::FilledOutline, 0.8).unwrap();
        assert_eq!(path.contours(), 1);
        assert_eq!(path.fill, Some([0.1, 0.2, 0.3]));
    }

    #[test]
    fn centerline_keeps_an_average_width() {
        let path = stroke_path(&stroke(&[1.0, 1.0, 1.0]), StrokeMode::Centerline, 0.8).unwrap();
        let style = path.stroke.unwrap();
        assert!((style.width - 20.0).abs() < 2.0, "{}", style.width);
        assert!(path.fill.is_none());
    }
}
