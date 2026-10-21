//! Rulers and guides: perspective (1, 2 or 3 vanishing points), symmetry
//! (mirror and rotational), parallel lines, concentric circles / ellipses,
//! radial lines, straight rulers and guides, and the canvas grid.
//!
//! Two jobs:
//! - **Snapping**: once a stroke has moved a few pixels, [`StrokeSnap`]
//!   picks the guide that best matches its direction (the line towards a
//!   vanishing point, the circle through the start, …) and projects every
//!   later point onto it.
//! - **Symmetry**: [`symmetry_copies`] gives the transforms that repeat a
//!   stroke; the app stamps every dab once per transform.
//!
//! Coordinates are canvas pixels, y down. Angles are degrees in canvas
//! terms (clockwise on screen), except brush dab angles, which are
//! counter-clockwise on screen like everywhere else in the brush engine.

use serde::{Deserialize, Serialize};

use crate::brush::Dab;

pub type P = [f32; 2];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RulerKind {
    /// 1 to 3 vanishing points. With one, strokes also snap horizontal and
    /// vertical; with two, vertical too.
    Perspective {
        vanishing: Vec<P>,
    },
    /// `lines` copies around `origin`; with `mirror`, each is also reflected
    /// (one line + mirror = left / right symmetry).
    Symmetry {
        origin: P,
        angle: f32,
        lines: u32,
        mirror: bool,
    },
    ParallelLines {
        angle: f32,
    },
    /// Circles (or ellipses: `ratio` < 1 squashes them along `angle`) around `center`.
    Concentric {
        center: P,
        ratio: f32,
        angle: f32,
    },
    RadialLines {
        center: P,
    },
    /// A straight ruler: strokes that start near it run along it.
    Line {
        a: P,
        b: P,
    },
    /// A horizontal (`vertical` false) or vertical guide at `position`.
    Guide {
        vertical: bool,
        position: f32,
    },
    /// Curvilinear perspective: vanishing points left / right of `center`
    /// at `radius_x` along `angle` (the horizon) and above / below at
    /// `radius_y`; lines heading for a pair are arcs through both. With
    /// `fifth`, lines also run straight out of the centre (fisheye, 5 points).
    Curvilinear {
        center: P,
        radius_x: f32,
        radius_y: f32,
        angle: f32,
        fifth: bool,
    },
}

/// Where a layer's ruler applies (Clip Studio's "show in" setting).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RulerScope {
    /// Only while its own layer is being edited.
    #[default]
    EditingLayer,
    /// While any layer in the same folder is edited.
    SameFolder,
    /// Whatever layer is edited.
    AllLayers,
}

impl RulerScope {
    pub const ALL: [RulerScope; 3] = [RulerScope::EditingLayer, RulerScope::SameFolder, RulerScope::AllLayers];

    pub fn label(self) -> &'static str {
        match self {
            RulerScope::EditingLayer => "Only its layer",
            RulerScope::SameFolder => "Layers in the same folder",
            RulerScope::AllLayers => "All layers",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ruler {
    pub kind: RulerKind,
    pub visible: bool,
    /// Strokes snap to it (symmetry rulers: strokes are repeated).
    pub snap: bool,
    /// Id of the layer it belongs to; `None` (files from before rulers had
    /// layers) applies everywhere.
    #[serde(default)]
    pub layer: Option<u64>,
    #[serde(default)]
    pub scope: RulerScope,
}

impl Ruler {
    /// A ruler for the whole document.
    pub fn new(kind: RulerKind) -> Self {
        Self { kind, visible: true, snap: true, layer: None, scope: RulerScope::EditingLayer }
    }

    /// A ruler on layer `layer`, active while that layer is edited.
    pub fn on_layer(kind: RulerKind, layer: u64) -> Self {
        Self { layer: Some(layer), ..Self::new(kind) }
    }

    /// The same ruler with every position scaled by `f` about the canvas
    /// origin (for copies into a document at another resolution).
    pub fn scaled(&self, f: f32) -> Ruler {
        let s = |p: P| [p[0] * f, p[1] * f];
        let kind = match &self.kind {
            RulerKind::Perspective { vanishing } => RulerKind::Perspective { vanishing: vanishing.iter().map(|v| s(*v)).collect() },
            RulerKind::Symmetry { origin, angle, lines, mirror } => {
                RulerKind::Symmetry { origin: s(*origin), angle: *angle, lines: *lines, mirror: *mirror }
            }
            RulerKind::ParallelLines { angle } => RulerKind::ParallelLines { angle: *angle },
            RulerKind::Concentric { center, ratio, angle } => RulerKind::Concentric { center: s(*center), ratio: *ratio, angle: *angle },
            RulerKind::RadialLines { center } => RulerKind::RadialLines { center: s(*center) },
            RulerKind::Line { a, b } => RulerKind::Line { a: s(*a), b: s(*b) },
            RulerKind::Guide { vertical, position } => RulerKind::Guide { vertical: *vertical, position: position * f },
            RulerKind::Curvilinear { center, radius_x, radius_y, angle, fifth } => RulerKind::Curvilinear {
                center: s(*center),
                radius_x: radius_x * f,
                radius_y: radius_y * f,
                angle: *angle,
                fifth: *fifth,
            },
        };
        Ruler { kind, ..self.clone() }
    }

    pub fn label(&self) -> &'static str {
        match &self.kind {
            RulerKind::Perspective { vanishing } => match vanishing.len() {
                1 => "1-point perspective",
                2 => "2-point perspective",
                _ => "3-point perspective",
            },
            RulerKind::Symmetry { mirror: true, lines: 1, .. } => "Mirror symmetry",
            RulerKind::Symmetry { mirror: true, .. } => "Kaleidoscope",
            RulerKind::Symmetry { .. } => "Rotational symmetry",
            RulerKind::ParallelLines { .. } => "Parallel lines",
            RulerKind::Concentric { .. } => "Concentric circles",
            RulerKind::RadialLines { .. } => "Radial lines",
            RulerKind::Line { .. } => "Straight ruler",
            RulerKind::Guide { vertical: true, .. } => "Vertical guide",
            RulerKind::Guide { vertical: false, .. } => "Horizontal guide",
            RulerKind::Curvilinear { fifth: false, .. } => "4-point curvilinear perspective",
            RulerKind::Curvilinear { fifth: true, .. } => "5-point curvilinear perspective",
        }
    }

    /// Draggable points (canvas pixels) and what each one is.
    pub fn handles(&self) -> Vec<P> {
        match &self.kind {
            RulerKind::Perspective { vanishing } => vanishing.clone(),
            RulerKind::Symmetry { origin, angle, .. } => vec![*origin, along(*origin, *angle, HANDLE_ARM)],
            RulerKind::ParallelLines { .. } => Vec::new(),
            RulerKind::Concentric { center, .. } | RulerKind::RadialLines { center } => vec![*center],
            RulerKind::Line { a, b } => vec![*a, *b],
            RulerKind::Guide { .. } => Vec::new(),
            RulerKind::Curvilinear { center, radius_x, radius_y, angle, .. } => {
                let [_, right, _, bottom] = curvilinear_points(*center, *radius_x, *radius_y, *angle);
                vec![*center, right, bottom]
            }
        }
    }

    /// Moves handle `index` to `p`.
    pub fn drag_handle(&mut self, index: usize, p: P) {
        match &mut self.kind {
            RulerKind::Perspective { vanishing } => {
                if let Some(v) = vanishing.get_mut(index) {
                    *v = p;
                }
            }
            RulerKind::Symmetry { origin, angle, .. } => match index {
                0 => *origin = p,
                _ => *angle = (p[1] - origin[1]).atan2(p[0] - origin[0]).to_degrees(),
            },
            RulerKind::Concentric { center, .. } | RulerKind::RadialLines { center } => *center = p,
            RulerKind::Line { a, b } => {
                if index == 0 {
                    *a = p;
                } else {
                    *b = p;
                }
            }
            RulerKind::Curvilinear { center, radius_x, radius_y, angle, .. } => match index {
                0 => *center = p,
                // The right-hand point turns the horizon and sets its reach.
                1 => {
                    let d = sub(p, *center);
                    *radius_x = len(d).max(1.0);
                    *angle = d[1].atan2(d[0]).to_degrees();
                }
                // The lower point sets the vertical reach.
                _ => {
                    let (s, c) = angle.to_radians().sin_cos();
                    *radius_y = dot(sub(p, *center), [-s, c]).abs().max(1.0);
                }
            },
            RulerKind::ParallelLines { .. } | RulerKind::Guide { .. } => {}
        }
    }
}

/// A curvilinear ruler's four vanishing points: left, right, top, bottom
/// (canvas y runs down, so "bottom" is `center` + `radius_y` across the horizon).
pub fn curvilinear_points(center: P, radius_x: f32, radius_y: f32, angle: f32) -> [P; 4] {
    let (s, c) = angle.to_radians().sin_cos();
    let (u, v) = ([c, s], [-s, c]);
    let at = |a: f32, b: f32| [center[0] + u[0] * a + v[0] * b, center[1] + u[1] * a + v[1] * b];
    [at(-radius_x, 0.0), at(radius_x, 0.0), at(0.0, -radius_y), at(0.0, radius_y)]
}

/// The circle through `p` and the two points `center ± reach · axis` (unit
/// `axis`), as a guide; a straight line when `p` is on their line, or so
/// close to it that the circle is too large to tell apart.
pub fn arc_through(center: P, axis: P, reach: f32, p: P) -> Guide {
    let across = [-axis[1], axis[0]];
    let d = sub(p, center);
    let (pa, pc) = (dot(d, axis), dot(d, across));
    // The circle's centre is on the perpendicular bisector: center + t · across.
    let t = (pa * pa + pc * pc - reach * reach) / (2.0 * pc);
    let radius = (t * t + reach * reach).sqrt();
    if pc.abs() < 1e-3 || !radius.is_finite() || radius > 1e6 {
        return Guide::Line { origin: p, dir: axis };
    }
    Guide::Ellipse { center: [center[0] + across[0] * t, center[1] + across[1] * t], radius, ratio: 1.0, angle: 0.0 }
}

/// Distance of the symmetry ruler's angle handle from its origin.
pub const HANDLE_ARM: f32 = 120.0;

fn along(p: P, angle_deg: f32, d: f32) -> P {
    let (s, c) = angle_deg.to_radians().sin_cos();
    [p[0] + c * d, p[1] + s * d]
}

fn sub(a: P, b: P) -> P {
    [a[0] - b[0], a[1] - b[1]]
}

fn dot(a: P, b: P) -> f32 {
    a[0] * b[0] + a[1] * b[1]
}

fn len(a: P) -> f32 {
    dot(a, a).sqrt()
}

fn norm(a: P) -> Option<P> {
    let l = len(a);
    (l > 1e-4).then(|| [a[0] / l, a[1] / l])
}

/// The canvas grid.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Grid {
    pub visible: bool,
    /// Cell size in canvas pixels.
    pub spacing: f32,
    /// Lighter lines between the main ones.
    pub subdivisions: u32,
    pub origin: P,
}

impl Default for Grid {
    fn default() -> Self {
        Self { visible: false, spacing: 100.0, subdivisions: 4, origin: [0.0, 0.0] }
    }
}

/// A document's rulers, guides and grid.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RulerSet {
    pub rulers: Vec<Ruler>,
    /// Master switch for snapping and symmetry (Ctrl+1).
    #[serde(default = "yes")]
    pub snapping: bool,
    #[serde(default)]
    pub grid: Grid,
}

fn yes() -> bool {
    true
}

impl Default for RulerSet {
    fn default() -> Self {
        Self { rulers: Vec::new(), snapping: true, grid: Grid::default() }
    }
}

impl RulerSet {
    pub fn is_empty(&self) -> bool {
        self.rulers.is_empty()
    }

    /// Rulers that act on strokes now.
    pub fn active(&self) -> impl Iterator<Item = &Ruler> {
        self.rulers.iter().filter(move |r| self.snapping && r.snap)
    }

    /// Default rulers placed for a `w` × `h` canvas.
    pub fn perspective(points: usize, w: f32, h: f32) -> Ruler {
        let horizon = h * 0.45;
        let vanishing = match points {
            1 => vec![[w * 0.5, horizon]],
            2 => vec![[-w * 0.35, horizon], [w * 1.35, horizon]],
            _ => vec![[-w * 0.35, horizon], [w * 1.35, horizon], [w * 0.5, h * 2.6]],
        };
        Ruler::new(RulerKind::Perspective { vanishing })
    }

    /// A 4- or 5-point curvilinear ruler centred on a `w` × `h` canvas: the
    /// side points just outside the canvas for four; all four on one circle
    /// inside it for the fisheye five.
    pub fn curvilinear(points: usize, w: f32, h: f32) -> Ruler {
        let center = [w * 0.5, h * 0.5];
        let (radius_x, radius_y) = if points >= 5 { (w.min(h) * 0.48, w.min(h) * 0.48) } else { (w * 0.6, h * 0.6) };
        Ruler::new(RulerKind::Curvilinear { center, radius_x, radius_y, angle: 0.0, fifth: points >= 5 })
    }
}

// ---- snapping ---------------------------------------------------------------------------

/// What a snapped stroke runs along.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Guide {
    /// The line through `origin` along unit `dir`.
    Line { origin: P, dir: P },
    /// The ellipse through the start point: centre, radius along `angle`,
    /// ratio of the other axis.
    Ellipse { center: P, radius: f32, ratio: f32, angle: f32 },
}

impl Guide {
    pub fn project(&self, p: P) -> P {
        match *self {
            Guide::Line { origin, dir } => {
                let t = dot(sub(p, origin), dir);
                [origin[0] + dir[0] * t, origin[1] + dir[1] * t]
            }
            Guide::Ellipse { center, radius, ratio, angle } => {
                // Into the ellipse's frame, stretched to a circle.
                let (s, c) = angle.to_radians().sin_cos();
                let d = sub(p, center);
                let (u, v) = (d[0] * c + d[1] * s, (-d[0] * s + d[1] * c) / ratio.max(1e-3));
                let l = (u * u + v * v).sqrt().max(1e-4);
                let (u, v) = (u / l * radius, v / l * radius * ratio);
                [center[0] + u * c - v * s, center[1] + u * s + v * c]
            }
        }
    }

    /// Unit direction of the guide at `p` (for picking the best guide).
    fn direction_at(&self, p: P) -> Option<P> {
        match *self {
            Guide::Line { dir, .. } => Some(dir),
            Guide::Ellipse { center, ratio, angle, .. } => {
                let (s, c) = angle.to_radians().sin_cos();
                let d = sub(p, center);
                let (u, v) = (d[0] * c + d[1] * s, (-d[0] * s + d[1] * c) / ratio.max(1e-3));
                // Tangent of the circle, squashed back.
                let (tu, tv) = (-v, u * ratio);
                norm([tu * c - tv * s, tu * s + tv * c])
            }
        }
    }
}

/// The guides a stroke starting at `start` could follow. `near` is how far
/// (canvas pixels) a straight ruler or guide catches a stroke.
pub fn candidate_guides(set: &RulerSet, start: P, near: f32) -> Vec<Guide> {
    let mut out = Vec::new();
    let line = |dir: P| norm(dir).map(|dir| Guide::Line { origin: start, dir });
    for r in set.active() {
        match &r.kind {
            RulerKind::Perspective { vanishing } => {
                out.extend(vanishing.iter().filter_map(|v| line(sub(*v, start))));
                if vanishing.len() <= 2 {
                    out.push(Guide::Line { origin: start, dir: [0.0, 1.0] });
                }
                if vanishing.len() == 1 {
                    out.push(Guide::Line { origin: start, dir: [1.0, 0.0] });
                }
            }
            RulerKind::ParallelLines { angle } => {
                let (s, c) = angle.to_radians().sin_cos();
                out.push(Guide::Line { origin: start, dir: [c, s] });
            }
            RulerKind::RadialLines { center } => out.extend(line(sub(start, *center))),
            RulerKind::Concentric { center, ratio, angle } => {
                let probe = Guide::Ellipse { center: *center, radius: 1.0, ratio: *ratio, angle: *angle };
                // The radius that puts `start` on the ellipse.
                let on_unit = probe.project(start);
                let r = len(sub(start, *center)) / len(sub(on_unit, *center)).max(1e-4);
                if r > 0.5 {
                    out.push(Guide::Ellipse { center: *center, radius: r, ratio: *ratio, angle: *angle });
                }
            }
            RulerKind::Line { a, b } => {
                if let Some(dir) = norm(sub(*b, *a)) {
                    let g = Guide::Line { origin: *a, dir };
                    if len(sub(g.project(start), start)) <= near {
                        out.push(g);
                    }
                }
            }
            RulerKind::Guide { vertical, position } => {
                let g = if *vertical {
                    Guide::Line { origin: [*position, 0.0], dir: [0.0, 1.0] }
                } else {
                    Guide::Line { origin: [0.0, *position], dir: [1.0, 0.0] }
                };
                if len(sub(g.project(start), start)) <= near {
                    out.push(g);
                }
            }
            RulerKind::Curvilinear { center, radius_x, radius_y, angle, fifth } => {
                let (s, c) = angle.to_radians().sin_cos();
                // Towards left / right: arcs through those two points; up / down likewise.
                out.push(arc_through(*center, [c, s], *radius_x, start));
                out.push(arc_through(*center, [-s, c], *radius_y, start));
                if *fifth {
                    out.extend(line(sub(start, *center)));
                }
            }
            RulerKind::Symmetry { .. } => {}
        }
    }
    out
}

/// Snapping state for one stroke.
#[derive(Clone, Debug)]
pub struct StrokeSnap {
    start: P,
    candidates: Vec<Guide>,
    guide: Option<Guide>,
}

impl StrokeSnap {
    /// `None` when no ruler offers a guide here (the stroke stays free).
    pub fn new(set: &RulerSet, start: P, near: f32) -> Option<Self> {
        let candidates = candidate_guides(set, start, near);
        (!candidates.is_empty()).then_some(Self { start, candidates, guide: None })
    }

    pub fn guide(&self) -> Option<Guide> {
        self.guide
    }

    /// Where point `p` goes. Until the stroke has moved `decide` pixels the
    /// guide isn't known: `None` (hold the point back).
    pub fn apply(&mut self, p: P, decide: f32) -> Option<P> {
        if self.guide.is_none() {
            let moved = sub(p, self.start);
            if len(moved) < decide {
                return None;
            }
            let dir = norm(moved)?;
            // The guide whose direction at the start matches the movement best.
            self.guide = self.candidates.iter().copied().max_by(|a, b| {
                let score = |g: &Guide| g.direction_at(self.start).map_or(0.0, |d| dot(d, dir).abs());
                score(a).total_cmp(&score(b))
            });
        }
        self.guide.map(|g| g.project(p))
    }
}

// ---- symmetry ---------------------------------------------------------------------------

/// A 2D affine map: `[a, b, c, d, tx, ty]` with x' = a·x + b·y + tx,
/// y' = c·x + d·y + ty; `mirror` when it flips orientation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Copy2D {
    pub m: [f32; 6],
    /// Canvas angle (degrees) of the rotation part; for mirrors, of the axis.
    pub angle: f32,
    pub mirror: bool,
}

impl Copy2D {
    pub fn apply(&self, p: P) -> P {
        let [a, b, c, d, tx, ty] = self.m;
        [a * p[0] + b * p[1] + tx, c * p[0] + d * p[1] + ty]
    }

    /// A dab repeated by this map. `tip_angle` is the brush tip's own angle
    /// (degrees, counter-clockwise on screen) so mirrored tips stay right.
    pub fn dab(&self, dab: &Dab, tip_angle: f32) -> Dab {
        let [x, y] = self.apply([dab.x, dab.y]);
        let angle = if self.mirror {
            // A reflection across an axis at screen angle β (= −canvas angle)
            // turns a tip at ψ into one at 2β − ψ + 180°, flipped left-right.
            let beta = -self.angle;
            let total = tip_angle + dab.angle;
            2.0 * beta - total + 180.0 - tip_angle
        } else {
            dab.angle - self.angle
        };
        Dab { x, y, angle, mirror: dab.mirror != self.mirror, ..*dab }
    }
}

fn rotation_about(o: P, deg: f32) -> Copy2D {
    let (s, c) = deg.to_radians().sin_cos();
    let m = [c, -s, s, c, o[0] - c * o[0] + s * o[1], o[1] - s * o[0] - c * o[1]];
    Copy2D { m, angle: deg, mirror: false }
}

fn reflection_about(o: P, axis_deg: f32) -> Copy2D {
    let (s, c) = (2.0 * axis_deg).to_radians().sin_cos();
    // Reflection across the line through o at axis_deg.
    let m = [c, s, s, -c, o[0] - c * o[0] - s * o[1], o[1] - s * o[0] + c * o[1]];
    Copy2D { m, angle: axis_deg, mirror: true }
}

/// Every extra copy the symmetry rulers make of a stroke (not the stroke
/// itself). Several symmetry rulers combine.
pub fn symmetry_copies(set: &RulerSet) -> Vec<Copy2D> {
    let mut maps = vec![Copy2D { m: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], angle: 0.0, mirror: false }];
    for r in set.active() {
        let RulerKind::Symmetry { origin, angle, lines, mirror } = &r.kind else { continue };
        let lines = (*lines).clamp(1, 64);
        // Rotations by k·360/lines (the first is the stroke itself), and with
        // mirror the reflections across the axes at angle + k·180/lines.
        let mut own: Vec<Copy2D> = (0..lines).map(|k| rotation_about(*origin, 360.0 * k as f32 / lines as f32)).collect();
        if *mirror {
            own.extend((0..lines).map(|k| reflection_about(*origin, angle + 180.0 * k as f32 / lines as f32)));
        }
        // Compose with what earlier symmetry rulers made.
        let mut next = Vec::new();
        for a in &maps {
            for b in &own {
                next.push(compose(b, a));
            }
        }
        maps = next;
    }
    maps.into_iter().skip(1).collect()
}

/// `b` after `a`.
fn compose(b: &Copy2D, a: &Copy2D) -> Copy2D {
    let [a1, b1, c1, d1, x1, y1] = a.m;
    let [a2, b2, c2, d2, x2, y2] = b.m;
    let m =
        [a2 * a1 + b2 * c1, a2 * b1 + b2 * d1, c2 * a1 + d2 * c1, c2 * b1 + d2 * d1, a2 * x1 + b2 * y1 + x2, c2 * x1 + d2 * y1 + y2];
    let mirror = a.mirror != b.mirror;
    // Recover the angle: for rotations the rotation angle, for mirrors the axis.
    let angle = if mirror { (m[2]).atan2(m[0]).to_degrees() * 0.5 } else { m[2].atan2(m[0]).to_degrees() };
    Copy2D { m, angle, mirror }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: P, b: P) -> bool {
        (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3
    }

    #[test]
    fn perspective_strokes_head_for_the_vanishing_point_they_follow() {
        let mut set = RulerSet::default();
        set.rulers.push(Ruler::new(RulerKind::Perspective { vanishing: vec![[0.0, 100.0], [1000.0, 100.0]] }));
        // Start at (500, 300); move roughly towards the right point.
        let mut s = StrokeSnap::new(&set, [500.0, 300.0], 10.0).unwrap();
        assert_eq!(s.apply([503.0, 300.0], 8.0), None, "too early to tell");
        let p = s.apply([560.0, 270.0], 8.0).unwrap();
        // On the line from the start to (1000, 100).
        let d = sub([1000.0, 100.0], [500.0, 300.0]);
        let q = sub(p, [500.0, 300.0]);
        assert!((d[0] * q[1] - d[1] * q[0]).abs() < 1e-2);
        // A mostly vertical movement takes the vertical guide instead.
        let mut v = StrokeSnap::new(&set, [500.0, 300.0], 10.0).unwrap();
        let p = v.apply([505.0, 400.0], 8.0).unwrap();
        assert!(close(p, [500.0, 400.0]));
    }

    #[test]
    fn curvilinear_strokes_bend_through_the_vanishing_points() {
        let (center, rx, ry) = ([500.0, 400.0], 600.0, 450.0);
        let [left, right, top, bottom] = curvilinear_points(center, rx, ry, 0.0);
        assert!(close(left, [-100.0, 400.0]) && close(right, [1100.0, 400.0]));
        assert!(close(top, [500.0, -50.0]) && close(bottom, [500.0, 850.0]));
        let on_circle = |g: Guide, p: P| match g {
            Guide::Ellipse { center, radius, .. } => (len(sub(p, center)) - radius).abs() < 0.05,
            Guide::Line { .. } => false,
        };
        // The arc through a start point passes through both side points (and the start).
        let start = [700.0, 250.0];
        let g = arc_through(center, [1.0, 0.0], rx, start);
        assert!(on_circle(g, left) && on_circle(g, right) && on_circle(g, start), "{g:?}");
        // On the horizon itself it's the straight horizon.
        assert!(matches!(arc_through(center, [1.0, 0.0], rx, [800.0, 400.0]), Guide::Line { dir: [1.0, 0.0], .. }));

        let mut set = RulerSet::default();
        let kind = RulerKind::Curvilinear { center, radius_x: rx, radius_y: ry, angle: 0.0, fifth: false };
        set.rulers.push(Ruler::new(kind));
        // A mostly horizontal stroke bends along the left / right arc.
        let mut s = StrokeSnap::new(&set, start, 10.0).unwrap();
        let p = s.apply([760.0, 252.0], 8.0).unwrap();
        assert!(on_circle(arc_through(center, [1.0, 0.0], rx, start), p), "{p:?}");
        // A mostly vertical one along the top / bottom arc.
        let mut s = StrokeSnap::new(&set, start, 10.0).unwrap();
        let p = s.apply([702.0, 320.0], 8.0).unwrap();
        let vertical = arc_through(center, [0.0, 1.0], ry, start);
        assert!(on_circle(vertical, p) && on_circle(vertical, top) && on_circle(vertical, bottom), "{p:?}");
        // Five points: straight out of the centre too.
        let RulerKind::Curvilinear { fifth, .. } = &mut set.rulers[0].kind else { unreachable!() };
        *fifth = true;
        let mut s = StrokeSnap::new(&set, start, 10.0).unwrap();
        let p = s.apply([800.0, 175.0], 8.0).unwrap();
        let (d, q) = (sub(start, center), sub(p, center));
        assert!((d[0] * q[1] - d[1] * q[0]).abs() < 1e-1, "along the line from the centre: {p:?}");
        // Handles: centre, right point (turns the horizon), lower point.
        let mut r = set.rulers[0].clone();
        assert_eq!(r.handles().len(), 3);
        r.drag_handle(1, [500.0, 1000.0]);
        let RulerKind::Curvilinear { radius_x, angle, .. } = r.kind else { unreachable!() };
        assert!((radius_x - 600.0).abs() < 1e-3 && (angle - 90.0).abs() < 1e-3);
        let scaled = set.rulers[0].scaled(0.5);
        assert!(matches!(scaled.kind, RulerKind::Curvilinear { radius_x: 300.0, center: [250.0, 200.0], .. }));
    }

    #[test]
    fn concentric_and_radial_and_guides() {
        let mut set = RulerSet::default();
        set.rulers.push(Ruler::new(RulerKind::Concentric { center: [0.0, 0.0], ratio: 1.0, angle: 0.0 }));
        let mut s = StrokeSnap::new(&set, [100.0, 0.0], 10.0).unwrap();
        let p = s.apply([100.0, 30.0], 8.0).unwrap();
        assert!((len(p) - 100.0).abs() < 1e-3, "stays on the circle");

        set.rulers[0] = Ruler::new(RulerKind::RadialLines { center: [0.0, 0.0] });
        let mut s = StrokeSnap::new(&set, [100.0, 100.0], 10.0).unwrap();
        let p = s.apply([150.0, 140.0], 8.0).unwrap();
        assert!((p[0] - p[1]).abs() < 1e-3, "along the spoke");

        set.rulers[0] = Ruler::new(RulerKind::Guide { vertical: true, position: 50.0 });
        assert!(StrokeSnap::new(&set, [200.0, 10.0], 10.0).is_none(), "far from the guide");
        let mut s = StrokeSnap::new(&set, [55.0, 10.0], 10.0).unwrap();
        assert!(close(s.apply([70.0, 200.0], 8.0).unwrap(), [50.0, 200.0]));

        set.snapping = false;
        assert!(StrokeSnap::new(&set, [55.0, 10.0], 10.0).is_none(), "master switch");
    }

    #[test]
    fn ellipses_keep_their_shape() {
        let g = Guide::Ellipse { center: [0.0, 0.0], radius: 100.0, ratio: 0.5, angle: 0.0 };
        assert!(close(g.project([300.0, 0.0]), [100.0, 0.0]));
        assert!(close(g.project([0.0, 300.0]), [0.0, 50.0]));
    }

    #[test]
    fn symmetry_copies_and_dabs() {
        let mut set = RulerSet::default();
        set.rulers.push(Ruler::new(RulerKind::Symmetry { origin: [100.0, 0.0], angle: 90.0, lines: 1, mirror: true }));
        let maps = symmetry_copies(&set);
        assert_eq!(maps.len(), 1, "one mirror image");
        assert!(close(maps[0].apply([70.0, 20.0]), [130.0, 20.0]), "mirrored across x = 100");
        assert!(maps[0].mirror);

        set.rulers[0].kind = RulerKind::Symmetry { origin: [0.0, 0.0], angle: 0.0, lines: 4, mirror: false };
        let maps = symmetry_copies(&set);
        assert_eq!(maps.len(), 3);
        let pts: Vec<P> = maps.iter().map(|m| m.apply([10.0, 0.0])).collect();
        for want in [[-10.0, 0.0], [0.0, -10.0], [0.0, 10.0]] {
            assert!(pts.iter().any(|p| close(*p, want)), "{want:?} in {pts:?}");
        }

        // Kaleidoscope: 3 lines with mirrors = 6 images, 5 copies.
        set.rulers[0].kind = RulerKind::Symmetry { origin: [0.0, 0.0], angle: 0.0, lines: 3, mirror: true };
        assert_eq!(symmetry_copies(&set).len(), 5);

        // A rotated dab turns with the copy.
        let dab = Dab { x: 10.0, y: 0.0, radius: 3.0, alpha: 1.0, angle: 0.0, thickness: 1.0, mirror: false };
        let quarter = rotation_about([0.0, 0.0], 90.0);
        let d = quarter.dab(&dab, 0.0);
        assert!(close([d.x, d.y], [0.0, 10.0]));
        assert_eq!(d.angle, -90.0);
        set.snapping = false;
        assert!(symmetry_copies(&set).is_empty());
    }
}
