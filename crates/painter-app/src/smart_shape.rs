//! Smart shape: draw a line, a circle, a rectangle or a triangle by hand and
//! keep the pen still at the end; the stroke turns into the clean shape, then
//! is drawn on release with the brush's own dynamics (like Procreate's
//! QuickShape). Tool Settings → "Smart shape (hold)" turns it off.
//!
//! The recognition is plain geometry: the stroke is resampled evenly, then
//! scored as a straight line, as a polygon from its corners (Ramer–Douglas–
//! Peucker) and, when it closes, as an ellipse from its second moments.

use std::time::{Duration, Instant};

use eframe::egui::{Color32, Painter, Pos2, Rect, Stroke};
use painter_core::StrokePoint;

use crate::app::PainterApp;

type P = [f32; 2];

/// How long the pen must stay still before the stroke becomes a shape.
pub(crate) const HOLD: Duration = Duration::from_millis(550);
/// Still means moving less than this many screen points.
const STILL: f32 = 4.0;

/// A recognised shape, in canvas pixels.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Shape {
    Line(P, P),
    /// Corners in order; `closed` joins the last to the first.
    Polygon {
        corners: Vec<P>,
        closed: bool,
    },
    Ellipse {
        center: P,
        radii: [f32; 2],
        angle: f32,
    },
}

impl Shape {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Shape::Line(..) => "line",
            Shape::Polygon { corners, closed: true } if corners.len() == 3 => "triangle",
            Shape::Polygon { corners, closed: true } if corners.len() == 4 => "rectangle",
            Shape::Polygon { closed: true, .. } => "polygon",
            Shape::Polygon { .. } => "polyline",
            Shape::Ellipse { radii, .. } if (radii[0] - radii[1]).abs() < 0.06 * radii[0].max(radii[1]) => "circle",
            Shape::Ellipse { .. } => "ellipse",
        }
    }

    /// The outline as points along it, about `spacing` pixels apart.
    pub(crate) fn outline(&self, spacing: f32) -> Vec<P> {
        let edge = |a: P, b: P, out: &mut Vec<P>| {
            let n = (dist(a, b) / spacing.max(0.25)).ceil().max(1.0) as usize;
            out.extend((0..n).map(|i| lerp(a, b, i as f32 / n as f32)));
        };
        let mut out = Vec::new();
        match self {
            Shape::Line(a, b) => {
                edge(*a, *b, &mut out);
                out.push(*b);
            }
            Shape::Polygon { corners, closed } => {
                for w in corners.windows(2) {
                    edge(w[0], w[1], &mut out);
                }
                if *closed {
                    if let (Some(&last), Some(&first)) = (corners.last(), corners.first()) {
                        edge(last, first, &mut out);
                        out.push(first);
                    }
                } else if let Some(&last) = corners.last() {
                    out.push(last);
                }
            }
            Shape::Ellipse { center, radii, angle } => {
                let perimeter = std::f32::consts::PI
                    * (3.0 * (radii[0] + radii[1]) - ((3.0 * radii[0] + radii[1]) * (radii[0] + 3.0 * radii[1])).sqrt());
                let n = (perimeter / spacing.max(0.25)).ceil().max(24.0) as usize;
                let (s, c) = angle.sin_cos();
                for i in 0..=n {
                    let t = i as f32 / n as f32 * std::f32::consts::TAU;
                    let (x, y) = (radii[0] * t.cos(), radii[1] * t.sin());
                    out.push([center[0] + x * c - y * s, center[1] + x * s + y * c]);
                }
            }
        }
        out
    }
}

fn dist(a: P, b: P) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

fn lerp(a: P, b: P, t: f32) -> P {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

/// Distance from `p` to the segment `a`–`b`.
fn to_segment(p: P, a: P, b: P) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    if len2 < 1e-9 {
        return dist(p, a);
    }
    let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0);
    dist(p, [a[0] + dx * t, a[1] + dy * t])
}

fn path_length(points: &[P]) -> f32 {
    points.windows(2).map(|w| dist(w[0], w[1])).sum()
}

/// `n` points evenly spaced along the path.
fn resample(points: &[P], n: usize) -> Vec<P> {
    let total = path_length(points);
    if points.len() < 2 || total <= 0.0 {
        return points.to_vec();
    }
    let step = total / (n - 1) as f32;
    let mut out = vec![points[0]];
    let (mut acc, mut prev) = (0.0, points[0]);
    let mut i = 1;
    while i < points.len() && out.len() < n {
        let d = dist(prev, points[i]);
        if acc + d >= step && d > 0.0 {
            let q = lerp(prev, points[i], (step - acc) / d);
            out.push(q);
            prev = q;
            acc = 0.0;
        } else {
            acc += d;
            prev = points[i];
            i += 1;
        }
    }
    while out.len() < n {
        out.push(*points.last().expect("points"));
    }
    out
}

/// Ramer–Douglas–Peucker: the points that keep the path within `eps`.
fn simplify(points: &[P], eps: f32) -> Vec<P> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let (a, b) = (points[0], points[points.len() - 1]);
    let (k, far) = points[1..points.len() - 1]
        .iter()
        .enumerate()
        .map(|(i, p)| (i + 1, to_segment(*p, a, b)))
        .fold((0, 0.0f32), |best, x| if x.1 > best.1 { x } else { best });
    if far <= eps {
        return vec![a, b];
    }
    let mut left = simplify(&points[..=k], eps);
    left.pop();
    left.extend(simplify(&points[k..], eps));
    left
}

/// Mean distance from the points to the closed or open outline of `corners`.
fn polygon_error(points: &[P], corners: &[P], closed: bool) -> f32 {
    let mut edges: Vec<(P, P)> = corners.windows(2).map(|w| (w[0], w[1])).collect();
    if closed {
        edges.push((corners[corners.len() - 1], corners[0]));
    }
    points.iter().map(|p| edges.iter().map(|(a, b)| to_segment(*p, *a, *b)).fold(f32::MAX, f32::min)).sum::<f32>()
        / points.len() as f32
}

/// The ellipse with the points' second moments, and its mean relative error.
fn fit_ellipse(points: &[P]) -> (Shape, f32) {
    let n = points.len() as f32;
    let cx = points.iter().map(|p| p[0]).sum::<f32>() / n;
    let cy = points.iter().map(|p| p[1]).sum::<f32>() / n;
    let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
    for p in points {
        let (x, y) = (p[0] - cx, p[1] - cy);
        sxx += x * x;
        syy += y * y;
        sxy += x * y;
    }
    let (sxx, syy, sxy) = (sxx / n, syy / n, sxy / n);
    let angle = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    let mid = (sxx + syy) * 0.5;
    let root = (((sxx - syy) * 0.5).powi(2) + sxy * sxy).sqrt();
    // Points spread evenly round an ellipse vary by r²/2 along each axis (exact for circles).
    let radii = [(2.0 * (mid + root)).max(0.0).sqrt(), (2.0 * (mid - root)).max(0.0).sqrt()];
    let (s, c) = angle.sin_cos();
    let error = points
        .iter()
        .map(|p| {
            let (x, y) = (p[0] - cx, p[1] - cy);
            let (u, v) = (x * c + y * s, -x * s + y * c);
            ((u / radii[0].max(1e-3)).powi(2) + (v / radii[1].max(1e-3)).powi(2)).sqrt() - 1.0
        })
        .map(f32::abs)
        .sum::<f32>()
        / n;
    (Shape::Ellipse { center: [cx, cy], radii, angle }, error)
}

/// Four corners squared up into a rectangle when they nearly are one: the
/// rectangle along the first side with the quad's spread.
fn square_up(q: &[P]) -> Vec<P> {
    let angle_at = |i: usize| {
        let (a, b, c) = (q[(i + 3) % 4], q[i], q[(i + 1) % 4]);
        let (u, v) = ([a[0] - b[0], a[1] - b[1]], [c[0] - b[0], c[1] - b[1]]);
        ((u[0] * v[0] + u[1] * v[1]) / (u[0].hypot(u[1]) * v[0].hypot(v[1])).max(1e-6)).acos().to_degrees()
    };
    if (0..4).any(|i| (angle_at(i) - 90.0).abs() > 18.0) {
        return q.to_vec();
    }
    // Axes from the average direction of the sides (mod 90°).
    let mut sx = 0.0;
    let mut sy = 0.0;
    for i in 0..4 {
        let (a, b) = (q[i], q[(i + 1) % 4]);
        let t = (b[1] - a[1]).atan2(b[0] - a[0]) * 4.0;
        sx += t.cos();
        sy += t.sin();
    }
    let theta = sy.atan2(sx) / 4.0;
    let (s, c) = theta.sin_cos();
    let local: Vec<P> = q.iter().map(|p| [p[0] * c + p[1] * s, -p[0] * s + p[1] * c]).collect();
    let cx = local.iter().map(|p| p[0]).sum::<f32>() / 4.0;
    let cy = local.iter().map(|p| p[1]).sum::<f32>() / 4.0;
    let hw = local.iter().map(|p| (p[0] - cx).abs()).sum::<f32>() / 4.0;
    let hh = local.iter().map(|p| (p[1] - cy).abs()).sum::<f32>() / 4.0;
    // Keep the drawn corner order: each corner to its quadrant.
    local
        .iter()
        .map(|p| {
            let (x, y) = (cx + hw * (p[0] - cx).signum(), cy + hh * (p[1] - cy).signum());
            [x * c - y * s, x * s + y * c]
        })
        .collect()
}

/// What shape a hand-drawn path is, if it is clearly one.
pub(crate) fn recognize(raw: &[P]) -> Option<Shape> {
    let length = path_length(raw);
    if raw.len() < 2 || length < 12.0 {
        return None;
    }
    let points = resample(raw, 96);
    let (first, last) = (points[0], points[points.len() - 1]);
    let closed = dist(first, last) < 0.18 * length && length > 40.0;
    if !closed {
        // Straight enough?
        let span = dist(first, last);
        let dev = points.iter().map(|p| to_segment(*p, first, last)).fold(0.0, f32::max);
        if dev <= (0.05 * span).max(2.5) {
            return Some(Shape::Line(first, last));
        }
        // A few straight pieces?
        let corners = simplify(&points, 0.04 * length);
        if corners.len() <= 5 && polygon_error(&points, &corners, false) < 0.015 * length {
            return Some(Shape::Polygon { corners, closed: false });
        }
        return None;
    }
    let (ellipse, ellipse_error) = fit_ellipse(&points);
    let mut corners = simplify(&points, 0.035 * length);
    // The loop starts and ends at about the same place: one corner, not two.
    if corners.len() > 2 && dist(corners[0], corners[corners.len() - 1]) < 0.18 * length {
        let end = corners.pop().expect("corners");
        corners[0] = lerp(corners[0], end, 0.5);
    }
    // A corner where the loop happened to start, on a straight side, isn't one.
    if corners.len() > 3 {
        let n = corners.len();
        let straight = to_segment(corners[0], corners[n - 1], corners[1]) < 0.02 * length;
        if straight {
            corners.remove(0);
        }
    }
    // Both errors relative to the shape's size (its radius, were it round).
    let radius = length / std::f32::consts::TAU;
    let polygon_err = if (3..=4).contains(&corners.len()) { polygon_error(&points, &corners, true) / radius } else { f32::MAX };
    const GOOD: f32 = 0.1;
    if polygon_err < GOOD && polygon_err <= ellipse_error {
        let corners = if corners.len() == 4 { square_up(&corners) } else { corners };
        Some(Shape::Polygon { corners, closed: true })
    } else if ellipse_error < GOOD {
        Some(ellipse)
    } else {
        None
    }
}

/// The smart shape of the stroke being drawn.
pub(crate) struct SmartShape {
    /// The path as drawn (canvas pixels) and the pen along it.
    points: Vec<P>,
    pressure_sum: f32,
    tilt: f32,
    azimuth: f32,
    /// Where the pen came to rest (screen points), and since when.
    rest: (Pos2, Instant),
    /// Once recognised: the shape the release draws.
    pub(crate) shape: Option<Shape>,
}

impl PainterApp {
    /// A brush stroke starts: follow it for a smart shape.
    pub(crate) fn smart_begin(&mut self, p: StrokePoint, screen: Pos2) {
        self.smart = self.tools.smart_shape.then(|| SmartShape {
            points: vec![[p.x, p.y]],
            pressure_sum: p.pressure,
            tilt: p.tilt,
            azimuth: p.azimuth,
            rest: (screen, Instant::now()),
            shape: None,
        });
    }

    /// The pen moved during the stroke.
    pub(crate) fn smart_move(&mut self, p: StrokePoint, screen: Pos2) {
        let Some(s) = &mut self.smart else { return };
        if s.shape.is_some() {
            return;
        }
        s.points.push([p.x, p.y]);
        s.pressure_sum += p.pressure;
        (s.tilt, s.azimuth) = (p.tilt, p.azimuth);
        if s.rest.0.distance(screen) > STILL {
            s.rest = (screen, Instant::now());
        }
    }

    /// Each frame while the pen is down: after a still hold, the stroke becomes the shape.
    pub(crate) fn smart_tick(&mut self, ctx: &eframe::egui::Context) {
        let Some(s) = &mut self.smart else { return };
        if s.shape.is_some() || self.stroke.is_none() {
            return;
        }
        let held = s.rest.1.elapsed();
        if held < HOLD {
            ctx.request_repaint_after(HOLD - held);
            return;
        }
        match recognize(&s.points) {
            Some(shape) => {
                self.status = format!("Smart shape: {} (release to draw it)", shape.name());
                s.shape = Some(shape);
                // The hand-drawn stroke goes; the shape is drawn on release.
                self.discard_stroke();
            }
            // Not a shape: stop looking, the stroke stays as drawn.
            None => self.smart = None,
        }
    }

    /// The pen lifted. True when a smart shape was drawn instead of the stroke.
    pub(crate) fn smart_release(&mut self) -> bool {
        let Some(s) = self.smart.take() else { return false };
        let Some(shape) = s.shape else { return false };
        let pressure = (s.pressure_sum / s.points.len().max(1) as f32).clamp(0.05, 1.0);
        let t0 = self.started.elapsed().as_secs_f32();
        // A steady hand: about 600 px/s, for speed dynamics.
        let mut points = shape.outline(2.0).into_iter().enumerate().map(|(i, [x, y])| {
            StrokePoint::new(x, y, pressure).with_tilt(s.tilt).with_azimuth(s.azimuth).at_time(t0 + i as f32 * 2.0 / 600.0)
        });
        let Some(first) = points.next() else { return true };
        self.begin_stroke(first);
        for p in points {
            self.continue_stroke(p);
        }
        self.end_stroke();
        true
    }

    /// The recognised shape, while the pen is still down.
    pub(crate) fn draw_smart_shape(&self, painter: &Painter, rect: Rect) {
        let Some(shape) = self.smart.as_ref().and_then(|s| s.shape.as_ref()) else { return };
        let doc = self.doc_size();
        let points: Vec<Pos2> =
            shape.outline(4.0).into_iter().map(|[x, y]| self.view.doc_to_screen(rect, doc, Pos2::new(x, y))).collect();
        painter.add(eframe::egui::Shape::line(points.clone(), Stroke::new(3.0_f32, Color32::from_black_alpha(120))));
        painter.add(eframe::egui::Shape::line(points, Stroke::new(1.5_f32, crate::theme::accent())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wobble(points: Vec<P>, amount: f32) -> Vec<P> {
        points
            .into_iter()
            .enumerate()
            .map(|(i, p)| [p[0] + amount * ((i as f32) * 1.7).sin(), p[1] + amount * ((i as f32) * 2.3).cos()])
            .collect()
    }

    fn polyline(corners: &[P], closed: bool) -> Vec<P> {
        let shape = Shape::Polygon { corners: corners.to_vec(), closed };
        shape.outline(3.0)
    }

    #[test]
    fn hand_drawn_shapes_are_recognised() {
        // A shaky line.
        let line = wobble((0..60).map(|i| [10.0 + i as f32 * 5.0, 20.0 + i as f32 * 2.0]).collect(), 1.5);
        assert_eq!(recognize(&line).unwrap().name(), "line");

        // A circle, slightly off.
        let circle: Vec<P> = (0..=100)
            .map(|i| {
                let t = i as f32 / 100.0 * std::f32::consts::TAU;
                [200.0 + 80.0 * t.cos(), 150.0 + 80.0 * t.sin()]
            })
            .collect();
        let circle = wobble(circle, 2.0);
        let Some(Shape::Ellipse { center, radii, .. }) = recognize(&circle) else { panic!("{:?}", recognize(&circle)) };
        assert!(dist(center, [200.0, 150.0]) < 4.0 && (radii[0] - 80.0).abs() < 6.0 && (radii[1] - 80.0).abs() < 6.0, "{radii:?}");

        // A wobbly rectangle comes out square.
        let rect = wobble(polyline(&[[10.0, 10.0], [210.0, 14.0], [208.0, 110.0], [12.0, 106.0]], true), 1.5);
        let Some(Shape::Polygon { corners, closed: true }) = recognize(&rect) else { panic!("{:?}", recognize(&rect)) };
        assert_eq!(corners.len(), 4);
        for i in 0..4 {
            let (a, b, c) = (corners[(i + 3) % 4], corners[i], corners[(i + 1) % 4]);
            let dot = (a[0] - b[0]) * (c[0] - b[0]) + (a[1] - b[1]) * (c[1] - b[1]);
            assert!(dot.abs() < 1.0, "right angles: {corners:?}");
        }

        // A triangle.
        let tri = wobble(polyline(&[[100.0, 10.0], [180.0, 150.0], [20.0, 150.0]], true), 1.0);
        assert_eq!(recognize(&tri).unwrap().name(), "triangle");

        // Scribbles are left alone.
        let scribble: Vec<P> = (0..200).map(|i| [(i as f32 * 0.37).sin() * 90.0 + i as f32, (i as f32 * 0.91).cos() * 70.0]).collect();
        assert!(recognize(&scribble).is_none(), "{:?}", recognize(&scribble));
    }

    #[test]
    fn outlines_close_and_follow_the_shape() {
        let e = Shape::Ellipse { center: [0.0, 0.0], radii: [50.0, 20.0], angle: 0.0 };
        let o = e.outline(2.0);
        assert!(dist(o[0], *o.last().unwrap()) < 1e-3);
        assert!(o.iter().all(|p| ((p[0] / 50.0).powi(2) + (p[1] / 20.0).powi(2) - 1.0).abs() < 1e-3));
        assert_eq!(Shape::Line([0.0, 0.0], [10.0, 0.0]).outline(2.0).last(), Some(&[10.0, 0.0]));
    }
}
