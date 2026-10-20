//! Raster-to-vector tracing, after potrace's pipeline:
//!
//! 1. **Threshold** the layer into a [`Bitmap`] (see [`crate::bitmap`]).
//! 2. **Decompose** it into closed pixel-edge contours ([`contours`]); outer
//!    edges and holes come out with opposite orientations, so an even-odd
//!    fill reproduces the shape. Contours smaller than `despeckle` are dropped.
//! 3. **Simplify** each contour (Ramer–Douglas–Peucker, [`simplify_closed`]):
//!    removes the pixel staircase within `simplify` pixels.
//! 4. **Fit curves** ([`fit_curves`]): vertices turning sharper than
//!    `corner_angle` stay corners; the rest become cubic Beziers through the
//!    edge midpoints, which gives smooth tangent-continuous outlines.
//!
//! Potrace additionally optimises the polygon globally and merges curve
//! segments (`opttolerance`); vtracer adds colour clustering for multi-colour
//! images. Both would slot in after steps 3 and 1 respectively.

use std::collections::HashMap;

use crate::bitmap::{Bitmap, ThresholdSource};
use crate::path::{Point, Segment};

#[derive(Debug, Clone, PartialEq)]
pub struct TraceSettings {
    pub source: ThresholdSource,
    /// 0..=255; see [`crate::bitmap::threshold`].
    pub threshold: u8,
    /// Contours enclosing less than this many square pixels are dropped (specks, dust).
    pub despeckle: f32,
    /// Path simplification tolerance in pixels; higher = fewer, looser segments.
    pub simplify: f32,
    /// Corner detection: turns sharper than this many degrees stay corners.
    /// Lower = more corners (crisper, more angular); higher = smoother.
    pub corner_angle: f32,
}

impl Default for TraceSettings {
    fn default() -> Self {
        Self { source: ThresholdSource::Alpha, threshold: 127, despeckle: 4.0, simplify: 0.8, corner_angle: 60.0 }
    }
}

/// Steps 2–4: the bitmap as one even-odd path's segments.
pub fn trace(bitmap: &Bitmap, settings: &TraceSettings) -> Vec<Segment> {
    contours(bitmap)
        .into_iter()
        .filter(|c| signed_area(c).abs() >= settings.despeckle)
        .flat_map(|c| fit_curves(&simplify_closed(&destaircase(&c), settings.simplify), settings.corner_angle))
        .collect()
}

/// Pixel steps sit half a pixel off the true edge. Each edge is replaced by
/// its midpoint, and a vertex survives only between two long edges (a real
/// corner), so diagonals become straight and squares stay square.
pub fn destaircase(polygon: &[Point]) -> Vec<Point> {
    const STEP: f32 = 2.0;
    let n = polygon.len();
    let len = |i: usize| dist2(polygon[i % n], polygon[(i + 1) % n]).sqrt();
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        if len(i + n - 1) > STEP && len(i) > STEP {
            out.push(polygon[i]);
        }
        let (a, b) = (polygon[i], polygon[(i + 1) % n]);
        out.push([(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]);
    }
    out
}

/// Closed contours along pixel edges, with collinear vertices removed. Ink is
/// on the right of the direction of travel (y down), so outer boundaries run
/// clockwise on screen and holes anticlockwise.
pub fn contours(bitmap: &Bitmap) -> Vec<Vec<Point>> {
    // Directed edges between an ink pixel and an empty neighbour.
    let mut edges: Vec<([i32; 2], [i32; 2])> = Vec::new();
    for y in 0..bitmap.height as i32 {
        for x in 0..bitmap.width as i32 {
            if !bitmap.get(x, y) {
                continue;
            }
            if !bitmap.get(x, y - 1) {
                edges.push(([x, y], [x + 1, y]));
            }
            if !bitmap.get(x + 1, y) {
                edges.push(([x + 1, y], [x + 1, y + 1]));
            }
            if !bitmap.get(x, y + 1) {
                edges.push(([x + 1, y + 1], [x, y + 1]));
            }
            if !bitmap.get(x - 1, y) {
                edges.push(([x, y + 1], [x, y]));
            }
        }
    }
    let mut outgoing: HashMap<[i32; 2], Vec<usize>> = HashMap::new();
    for (i, (start, _)) in edges.iter().enumerate() {
        outgoing.entry(*start).or_default().push(i);
    }

    let mut used = vec![false; edges.len()];
    let mut out = Vec::new();
    for first in 0..edges.len() {
        if used[first] {
            continue;
        }
        let origin = edges[first].0;
        let mut polygon = Vec::new();
        let mut current = first;
        loop {
            used[current] = true;
            let (start, end) = edges[current];
            polygon.push(start);
            if end == origin {
                break;
            }
            let dir = [end[0] - start[0], end[1] - start[1]];
            // Where two diagonal pixels touch there are two ways on: turn right,
            // which keeps the pixels as separate shapes.
            let next = outgoing.get(&end).and_then(|candidates| {
                candidates.iter().copied().filter(|&e| !used[e]).max_by_key(|&e| {
                    let (s, t) = edges[e];
                    let c = [t[0] - s[0], t[1] - s[1]];
                    dir[0] * c[1] - dir[1] * c[0]
                })
            });
            match next {
                Some(e) => current = e,
                None => break,
            }
        }
        let polygon = drop_collinear(&polygon);
        if polygon.len() >= 3 {
            out.push(polygon);
        }
    }
    out
}

fn drop_collinear(polygon: &[[i32; 2]]) -> Vec<Point> {
    let n = polygon.len();
    (0..n)
        .filter(|&i| {
            let (a, b, c) = (polygon[(i + n - 1) % n], polygon[i], polygon[(i + 1) % n]);
            (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]) != 0
        })
        .map(|i| [polygon[i][0] as f32, polygon[i][1] as f32])
        .collect()
}

/// Shoelace area; positive for clockwise-on-screen (y down) contours.
pub fn signed_area(polygon: &[Point]) -> f32 {
    let n = polygon.len();
    (0..n).map(|i| {
        let (a, b) = (polygon[i], polygon[(i + 1) % n]);
        a[0] * b[1] - b[0] * a[1]
    }).sum::<f32>()
        * 0.5
}

/// Ramer–Douglas–Peucker on a closed polygon: split at vertex 0 and the vertex
/// farthest from it, simplify both chains.
pub fn simplify_closed(polygon: &[Point], tolerance: f32) -> Vec<Point> {
    let n = polygon.len();
    if n < 4 || tolerance <= 0.0 {
        return polygon.to_vec();
    }
    let far = (1..n)
        .max_by(|&a, &b| dist2(polygon[0], polygon[a]).total_cmp(&dist2(polygon[0], polygon[b])))
        .unwrap_or(n / 2);
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[far] = true;
    let ring: Vec<Point> = polygon.iter().chain(std::iter::once(&polygon[0])).copied().collect();
    rdp(&ring, 0, far, tolerance, &mut keep);
    rdp(&ring, far, n, tolerance, &mut keep);
    let simplified: Vec<Point> = (0..n).filter(|&i| keep[i]).map(|i| polygon[i]).collect();
    if simplified.len() >= 3 {
        simplified
    } else {
        polygon.to_vec()
    }
}

fn rdp(points: &[Point], first: usize, last: usize, tolerance: f32, keep: &mut [bool]) {
    if last <= first + 1 {
        return;
    }
    let (a, b) = (points[first], points[last]);
    let (index, distance) = (first + 1..last)
        .map(|i| (i, segment_distance(points[i], a, b)))
        .max_by(|x, y| x.1.total_cmp(&y.1))
        .unwrap_or((first, 0.0));
    if distance > tolerance {
        keep[index % keep.len()] = true;
        rdp(points, first, index, tolerance, keep);
        rdp(points, index, last, tolerance, keep);
    }
}

fn dist2(a: Point, b: Point) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)
}

fn segment_distance(p: Point, a: Point, b: Point) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    if len2 == 0.0 {
        return dist2(p, a).sqrt();
    }
    let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0);
    dist2(p, [a[0] + t * dx, a[1] + t * dy]).sqrt()
}

/// A closed polygon as a path: corners stay sharp, smooth vertices become
/// cubic curves between the neighbouring edge midpoints.
pub fn fit_curves(polygon: &[Point], corner_angle: f32) -> Vec<Segment> {
    let n = polygon.len();
    if n < 3 {
        return Vec::new();
    }
    let mid = |i: usize| {
        let (a, b) = (polygon[i % n], polygon[(i + 1) % n]);
        [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]
    };
    let lerp = |a: Point, b: Point, t: f32| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
    let mut out = vec![Segment::MoveTo(mid(n - 1))];
    for i in 0..n {
        let (prev, v, next) = (polygon[(i + n - 1) % n], polygon[i], polygon[(i + 1) % n]);
        let (m_in, m_out) = (mid(i + n - 1), mid(i));
        if turn_degrees(prev, v, next) > corner_angle {
            out.push(Segment::LineTo(v));
            out.push(Segment::LineTo(m_out));
        } else {
            // The quadratic through the midpoints with `v` as control, as a cubic.
            out.push(Segment::CubicTo(lerp(m_in, v, 2.0 / 3.0), lerp(m_out, v, 2.0 / 3.0), m_out));
        }
    }
    out.push(Segment::Close);
    out
}

/// How far the direction changes at `v`, 0 (straight on) to 180 (reversal).
fn turn_degrees(prev: Point, v: Point, next: Point) -> f32 {
    let a = [v[0] - prev[0], v[1] - prev[1]];
    let b = [next[0] - v[0], next[1] - v[1]];
    let cross = a[0] * b[1] - a[1] * b[0];
    let dot = a[0] * b[0] + a[1] * b[1];
    cross.atan2(dot).abs().to_degrees()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bitmap(w: u32, h: u32, ink: impl Fn(u32, u32) -> bool) -> Bitmap {
        let mut b = Bitmap::new(w, h);
        for y in 0..h {
            for x in 0..w {
                b.set(x, y, ink(x, y));
            }
        }
        b
    }

    #[test]
    fn a_square_traces_to_four_corners() {
        let b = bitmap(20, 20, |x, y| (5..15).contains(&x) && (5..15).contains(&y));
        let c = contours(&b);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0], vec![[5.0, 5.0], [15.0, 5.0], [15.0, 15.0], [5.0, 15.0]]);
        assert_eq!(signed_area(&c[0]), 100.0, "clockwise on screen");
        let path = trace(&b, &TraceSettings::default());
        assert!(path.iter().all(|s| !matches!(s, Segment::CubicTo(..))), "all corners");
    }

    #[test]
    fn a_ring_has_a_hole_and_a_disc_is_smooth() {
        let ring = bitmap(40, 40, |x, y| {
            let d = ((x as f32 - 19.5).powi(2) + (y as f32 - 19.5).powi(2)).sqrt();
            (8.0..16.0).contains(&d)
        });
        let c = contours(&ring);
        assert_eq!(c.len(), 2);
        assert!(signed_area(&c[0]).signum() != signed_area(&c[1]).signum(), "hole runs the other way");

        let path = trace(&ring, &TraceSettings::default());
        let curves = path.iter().filter(|s| matches!(s, Segment::CubicTo(..))).count();
        let lines = path.iter().filter(|s| matches!(s, Segment::LineTo(..))).count();
        assert!(curves > lines * 3, "{curves} curves, {lines} lines");
        assert_eq!(path.iter().filter(|s| matches!(s, Segment::Close)).count(), 2);
    }

    #[test]
    fn specks_are_dropped_and_diagonal_pixels_stay_apart() {
        let b = bitmap(10, 10, |x, y| (x, y) == (2, 2) || (x, y) == (3, 3));
        assert_eq!(contours(&b).len(), 2, "touching corners are two shapes");
        assert!(trace(&b, &TraceSettings::default()).is_empty(), "1 px specks are below despeckle");
    }

    #[test]
    fn simplification_removes_the_staircase() {
        // A thick diagonal bar: its edges are pixel staircases.
        let b = bitmap(60, 60, |x, y| (x as i32 - y as i32).abs() < 4 && (5..55).contains(&x));
        let raw = &contours(&b)[0];
        let simple = simplify_closed(raw, 0.8);
        assert!(raw.len() > 50 && simple.len() <= 8, "{} → {}", raw.len(), simple.len());
    }
}
