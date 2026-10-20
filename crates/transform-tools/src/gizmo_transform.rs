//! The on-canvas transform gizmo, independent of any UI toolkit: handle
//! layout and hit-testing, drags that edit the transform for each mode, and
//! the HUD's numeric fields. The app draws [`TransformGizmo::handles`] and
//! forwards pointer events; [`TransformGizmo::matrix`] (or the warp mesh)
//! drives the GPU preview, and the same values drive the commit.

use crate::crop::BoxHandle;
use crate::matrix::{rect_corners, snap_angle, AffineParams, Mat3, Point};
use crate::resample::Interpolation;
use crate::warp::MeshWarp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformMode {
    /// Scale (8 handles), rotate (handle above the box), move, pivot.
    ScaleRotate,
    /// Edge handles shear instead of scaling.
    Skew,
    /// Corners move in mirrored pairs, like converging lines in a photo.
    Perspective,
    /// Every corner moves on its own.
    FreeDistort,
    /// An N×N grid of points bends the image.
    MeshWarp,
}

impl TransformMode {
    pub const ALL: [TransformMode; 5] =
        [TransformMode::ScaleRotate, TransformMode::Skew, TransformMode::Perspective, TransformMode::FreeDistort, TransformMode::MeshWarp];

    pub fn name(self) -> &'static str {
        match self {
            TransformMode::ScaleRotate => "Scale / Rotate",
            TransformMode::Skew => "Skew",
            TransformMode::Perspective => "Perspective",
            TransformMode::FreeDistort => "Free Distort",
            TransformMode::MeshWarp => "Mesh Warp",
        }
    }

    fn uses_corners(self) -> bool {
        matches!(self, TransformMode::Perspective | TransformMode::FreeDistort)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GizmoHandle {
    Scale(BoxHandle),
    /// Corner index: top-left, top-right, bottom-right, bottom-left.
    Corner(usize),
    Rotate,
    Pivot,
    Move,
    MeshPoint(usize, usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    /// Keep proportions while scaling; snap rotation to 15°.
    pub shift: bool,
    /// Scale about the pivot instead of the opposite handle.
    pub alt: bool,
}

/// Enter / Esc / the HUD buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformAction {
    Apply,
    Cancel,
    Reset,
}

/// Numbers shown in (and edited from) the HUD.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hud {
    /// Where the source's centre lands.
    pub position: Point,
    /// Percent.
    pub scale: [f64; 2],
    /// Degrees.
    pub rotation: f64,
    pub interpolation: Interpolation,
}

#[derive(Debug, Clone, PartialEq)]
struct Drag {
    handle: GizmoHandle,
    start: Point,
    affine: AffineParams,
    corners: [Point; 4],
    warp: MeshWarp,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransformGizmo {
    /// Untransformed content bounds `(x, y, w, h)`.
    pub source: [f64; 4],
    pub mode: TransformMode,
    pub affine: AffineParams,
    /// Destination corners in the corner modes.
    pub corners: [Point; 4],
    pub warp: MeshWarp,
    pub interpolation: Interpolation,
    drag: Option<Drag>,
}

/// Rotation handle distance above the box, in screen pixels.
const ROTATE_OFFSET: f64 = 28.0;
const MESH_CELLS: usize = 4;

impl TransformGizmo {
    pub fn new(source: [f64; 4]) -> Self {
        let centre = [source[0] + source[2] * 0.5, source[1] + source[3] * 0.5];
        Self {
            source,
            mode: TransformMode::ScaleRotate,
            affine: AffineParams::identity(centre),
            corners: rect_corners(source),
            warp: MeshWarp::new(source, MESH_CELLS, MESH_CELLS),
            interpolation: Interpolation::Bilinear,
            drag: None,
        }
    }

    /// Source → canvas. In mesh warp mode this is the transform the grid was
    /// built from; the warp itself is [`Self::warp`].
    pub fn matrix(&self) -> Mat3 {
        if self.mode.uses_corners() {
            Mat3::rect_to_quad(self.source, self.corners).unwrap_or(Mat3::IDENTITY)
        } else {
            self.affine.matrix()
        }
    }

    /// Switches mode, carrying the current shape over (affine → corners →
    /// grid). Going back from corners or a warp to an affine mode keeps the
    /// affine parameters it had.
    pub fn set_mode(&mut self, mode: TransformMode) {
        if mode == self.mode {
            return;
        }
        let m = self.matrix();
        if mode.uses_corners() && !self.mode.uses_corners() {
            self.corners = rect_corners(self.source).map(|p| m.apply(p));
        }
        if mode == TransformMode::MeshWarp {
            let mut warp = MeshWarp::new(self.source, MESH_CELLS, MESH_CELLS);
            for p in &mut warp.points {
                *p = m.apply(*p);
            }
            self.warp = warp;
        }
        self.mode = mode;
        self.drag = None;
    }

    /// Back to the untransformed state, keeping the mode.
    pub fn reset(&mut self) {
        let mode = self.mode;
        *self = TransformGizmo { interpolation: self.interpolation, ..TransformGizmo::new(self.source) };
        self.mode = if mode == TransformMode::MeshWarp { TransformMode::ScaleRotate } else { mode };
        self.set_mode(mode);
    }

    /// The outline of the transformed source (the warp's border in mesh mode).
    pub fn outline(&self) -> Vec<Point> {
        match self.mode {
            TransformMode::MeshWarp => {
                let n = 16;
                let edge = |f: &dyn Fn(f64) -> (f64, f64)| (0..n).map(|k| {
                    let (u, v) = f(k as f64 / n as f64);
                    self.warp.evaluate(u, v)
                }).collect::<Vec<_>>();
                [edge(&|t| (t, 0.0)), edge(&|t| (1.0, t)), edge(&|t| (1.0 - t, 1.0)), edge(&|t| (0.0, 1.0 - t))].concat()
            }
            _ => {
                let m = self.matrix();
                rect_corners(self.source).map(|p| m.apply(p)).to_vec()
            }
        }
    }

    /// Every handle with its canvas position. `zoom` (screen px per canvas px)
    /// keeps the rotation handle a fixed screen distance away.
    pub fn handles(&self, zoom: f64) -> Vec<(GizmoHandle, Point)> {
        let m = self.matrix();
        match self.mode {
            TransformMode::ScaleRotate | TransformMode::Skew => {
                let mut out: Vec<_> = BoxHandle::ALL.iter().map(|&h| (GizmoHandle::Scale(h), m.apply(h.position(self.source)))).collect();
                let top = m.apply(BoxHandle::Top.position(self.source));
                let centre = m.apply([self.source[0] + self.source[2] * 0.5, self.source[1] + self.source[3] * 0.5]);
                let (dx, dy) = (top[0] - centre[0], top[1] - centre[1]);
                let len = (dx * dx + dy * dy).sqrt().max(1e-9);
                let off = ROTATE_OFFSET / zoom.max(1e-6);
                out.push((GizmoHandle::Rotate, [top[0] + dx / len * off, top[1] + dy / len * off]));
                out.push((GizmoHandle::Pivot, m.apply(self.affine.pivot)));
                out
            }
            TransformMode::Perspective | TransformMode::FreeDistort => {
                self.corners.iter().enumerate().map(|(i, &p)| (GizmoHandle::Corner(i), p)).collect()
            }
            TransformMode::MeshWarp => (0..=self.warp.rows)
                .flat_map(|j| (0..=self.warp.cols).map(move |i| (i, j)))
                .map(|(i, j)| (GizmoHandle::MeshPoint(i, j), self.warp.point(i, j)))
                .collect(),
        }
    }

    /// The handle under `p` (canvas px) within `radius` canvas px; inside the
    /// outline is a move.
    pub fn hit_test(&self, p: Point, radius: f64, zoom: f64) -> Option<GizmoHandle> {
        let nearest = self
            .handles(zoom)
            .into_iter()
            .map(|(h, q)| (h, (q[0] - p[0]).hypot(q[1] - p[1])))
            .filter(|(_, d)| *d <= radius)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((h, _)) = nearest {
            return Some(h);
        }
        inside(&self.outline(), p).then_some(GizmoHandle::Move)
    }

    pub fn begin_drag(&mut self, handle: GizmoHandle, at: Point) {
        self.drag = Some(Drag { handle, start: at, affine: self.affine, corners: self.corners, warp: self.warp.clone() });
    }

    pub fn end_drag(&mut self) {
        self.drag = None;
    }

    pub fn dragging(&self) -> Option<GizmoHandle> {
        self.drag.as_ref().map(|d| d.handle)
    }

    /// Updates the transform for the pointer at `p`.
    pub fn drag_to(&mut self, p: Point, mods: Modifiers) {
        let Some(drag) = self.drag.clone() else { return };
        let delta = [p[0] - drag.start[0], p[1] - drag.start[1]];
        match drag.handle {
            GizmoHandle::Move => match self.mode {
                TransformMode::ScaleRotate | TransformMode::Skew => {
                    self.affine.translate = [drag.affine.translate[0] + delta[0], drag.affine.translate[1] + delta[1]];
                }
                TransformMode::Perspective | TransformMode::FreeDistort => {
                    self.corners = drag.corners.map(|c| [c[0] + delta[0], c[1] + delta[1]]);
                }
                TransformMode::MeshWarp => {
                    for (q, s) in self.warp.points.iter_mut().zip(&drag.warp.points) {
                        *q = [s[0] + delta[0], s[1] + delta[1]];
                    }
                }
            },
            GizmoHandle::Rotate => {
                let c = drag.affine.matrix().apply(drag.affine.pivot);
                let a0 = (drag.start[1] - c[1]).atan2(drag.start[0] - c[0]);
                let a1 = (p[1] - c[1]).atan2(p[0] - c[0]);
                let mut degrees = drag.affine.rotation + (a1 - a0).to_degrees();
                if mods.shift {
                    degrees = snap_angle(degrees, 15.0);
                }
                self.affine.rotation = degrees;
            }
            GizmoHandle::Pivot => {
                let Some(inv) = drag.affine.matrix().inverse() else { return };
                self.affine = with_pivot(&drag.affine, inv.apply(p));
            }
            GizmoHandle::Scale(handle) if self.mode == TransformMode::Skew && matches!(handle, BoxHandle::Top | BoxHandle::Bottom | BoxHandle::Left | BoxHandle::Right) => {
                self.affine = skew_drag(&drag.affine, self.source, handle, delta);
            }
            GizmoHandle::Scale(handle) => {
                self.affine = scale_drag(&drag.affine, self.source, handle, p, mods);
            }
            GizmoHandle::Corner(i) => {
                let mut corners = drag.corners;
                corners[i] = [corners[i][0] + delta[0], corners[i][1] + delta[1]];
                if self.mode == TransformMode::Perspective {
                    // Horizontal motion mirrors on the same edge, vertical on the same side.
                    let (h, v) = (i ^ 1, 3 - i);
                    corners[h][0] -= delta[0];
                    corners[v][1] -= delta[1];
                }
                // Refuse folded quads: the homography would flip inside out.
                if Mat3::rect_to_quad(self.source, corners).is_some_and(|m| m.determinant() > 0.0) && convex(&corners) {
                    self.corners = corners;
                }
            }
            GizmoHandle::MeshPoint(i, j) => {
                let s = drag.warp.point(i, j);
                *self.warp.point_mut(i, j) = [s[0] + delta[0], s[1] + delta[1]];
            }
        }
    }

    pub fn hud(&self) -> Hud {
        let centre = [self.source[0] + self.source[2] * 0.5, self.source[1] + self.source[3] * 0.5];
        Hud {
            position: self.matrix().apply(centre),
            scale: self.affine.scale.map(|s| s * 100.0),
            rotation: self.affine.rotation,
            interpolation: self.interpolation,
        }
    }

    /// Applies HUD edits (affine modes; the corner and warp modes only take
    /// the interpolation and position).
    pub fn set_hud(&mut self, hud: Hud) {
        self.interpolation = hud.interpolation;
        let current = self.hud();
        let shift = [hud.position[0] - current.position[0], hud.position[1] - current.position[1]];
        match self.mode {
            TransformMode::ScaleRotate | TransformMode::Skew => {
                self.affine.scale = hud.scale.map(|s| (s / 100.0).max(1e-4));
                self.affine.rotation = hud.rotation;
                // Re-read the position after scale and rotation changed.
                let now = self.hud().position;
                self.affine.translate[0] += hud.position[0] - now[0];
                self.affine.translate[1] += hud.position[1] - now[1];
            }
            TransformMode::Perspective | TransformMode::FreeDistort => {
                self.corners = self.corners.map(|c| [c[0] + shift[0], c[1] + shift[1]]);
            }
            TransformMode::MeshWarp => {
                for q in &mut self.warp.points {
                    *q = [q[0] + shift[0], q[1] + shift[1]];
                }
            }
        }
    }
}

/// Same transform, different pivot: the translation absorbs the move.
fn with_pivot(params: &AffineParams, pivot: Point) -> AffineParams {
    let before = params.matrix();
    let mut out = AffineParams { pivot, ..*params };
    let after = out.matrix();
    let (a, b) = (before.apply(pivot), after.apply(pivot));
    out.translate[0] += a[0] - b[0];
    out.translate[1] += a[1] - b[1];
    out
}

/// Rotation × skew (the linear part without scale).
fn linear(params: &AffineParams) -> Mat3 {
    Mat3::rotate(params.rotation.to_radians()) * Mat3::skew(params.skew[0], params.skew[1])
}

/// Scale so the dragged handle follows the pointer, the opposite handle (or
/// the pivot with Alt) staying put; Shift keeps the proportions.
fn scale_drag(params: &AffineParams, source: [f64; 4], handle: BoxHandle, p: Point, mods: Modifiers) -> AffineParams {
    let m = params.matrix();
    let (sx, sy) = handle.sides();
    let anchor_src = if mods.alt { params.pivot } else { opposite(handle).position(source) };
    let handle_src = handle.position(source);
    let anchor = m.apply(anchor_src);
    let Some(l_inv) = linear(params).inverse() else { return *params };
    // The pointer's offset from the anchor in the scaled-only frame.
    let v = l_inv.apply([p[0] - anchor[0], p[1] - anchor[1]]);
    let d = [handle_src[0] - anchor_src[0], handle_src[1] - anchor_src[1]];
    let mut scale = params.scale;
    if sx != 0 && d[0].abs() > 1e-9 {
        scale[0] = v[0] / d[0];
    }
    if sy != 0 && d[1].abs() > 1e-9 {
        scale[1] = v[1] / d[1];
    }
    if mods.shift {
        let ratio = params.scale[1] / params.scale[0];
        let k = if sx == 0 { scale[1] / params.scale[1] } else if sy == 0 { scale[0] / params.scale[0] } else { (scale[0] / params.scale[0]).max(scale[1] / params.scale[1]) };
        scale = [params.scale[0] * k, params.scale[0] * k * ratio];
    }
    let scale = scale.map(|s| if s.abs() < 1e-4 { 1e-4_f64.copysign(s) } else { s });
    let mut out = AffineParams { scale, ..*params };
    // Keep the anchor where it was.
    let moved = out.matrix().apply(anchor_src);
    out.translate[0] += anchor[0] - moved[0];
    out.translate[1] += anchor[1] - moved[1];
    out
}

/// Edge handles in Skew mode: dragging along the edge shears the box.
fn skew_drag(params: &AffineParams, source: [f64; 4], handle: BoxHandle, delta: [f64; 2]) -> AffineParams {
    let Some(r_inv) = Mat3::rotate(params.rotation.to_radians()).inverse() else { return *params };
    let local = r_inv.apply(delta);
    let mut out = *params;
    let (_, sy) = handle.sides();
    let edge = handle.position(source);
    if sy != 0 {
        // Top/bottom: the edge slides along x by kx × its (scaled) distance from the pivot.
        let distance = (edge[1] - params.pivot[1]) * params.scale[1];
        if distance.abs() > 1e-9 {
            out.skew[0] = params.skew[0] + local[0] / distance;
        }
    } else {
        let distance = (edge[0] - params.pivot[0]) * params.scale[0];
        if distance.abs() > 1e-9 {
            out.skew[1] = params.skew[1] + local[1] / distance;
        }
    }
    out
}

fn opposite(handle: BoxHandle) -> BoxHandle {
    BoxHandle::ALL[(BoxHandle::ALL.iter().position(|h| *h == handle).unwrap_or(0) + 4) % 8]
}

/// Even-odd point in polygon.
fn inside(polygon: &[Point], p: Point) -> bool {
    let mut odd = false;
    let n = polygon.len();
    for i in 0..n {
        let (a, b) = (polygon[i], polygon[(i + n - 1) % n]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
            odd = !odd;
        }
    }
    odd
}

/// Whether the quad's corners turn the same way at every vertex.
fn convex(q: &[Point; 4]) -> bool {
    let turn = |i: usize| {
        let (a, b, c) = (q[i], q[(i + 1) % 4], q[(i + 2) % 4]);
        (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0])
    };
    let t: Vec<f64> = (0..4).map(turn).collect();
    t.iter().all(|&v| v > 0.0) || t.iter().all(|&v| v < 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Point, b: Point) -> bool {
        (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6
    }

    fn drag(g: &mut TransformGizmo, h: GizmoHandle, from: Point, to: Point, mods: Modifiers) {
        g.begin_drag(h, from);
        g.drag_to(to, mods);
        g.end_drag();
    }

    #[test]
    fn scale_keeps_the_opposite_corner() {
        let mut g = TransformGizmo::new([0.0, 0.0, 100.0, 50.0]);
        drag(&mut g, GizmoHandle::Scale(BoxHandle::BottomRight), [100.0, 50.0], [200.0, 75.0], Modifiers::default());
        assert!(close(g.matrix().apply([0.0, 0.0]), [0.0, 0.0]), "anchor fixed");
        assert!(close(g.matrix().apply([100.0, 50.0]), [200.0, 75.0]));
        let hud = g.hud();
        assert!((hud.scale[0] - 200.0).abs() < 1e-6 && (hud.scale[1] - 150.0).abs() < 1e-6);
        // Shift keeps the 2:1.5 proportions.
        let mut g = TransformGizmo::new([0.0, 0.0, 100.0, 100.0]);
        drag(&mut g, GizmoHandle::Scale(BoxHandle::Right), [100.0, 50.0], [300.0, 50.0], Modifiers { shift: true, alt: false });
        assert!((g.affine.scale[0] - g.affine.scale[1]).abs() < 1e-9 && (g.affine.scale[0] - 3.0).abs() < 1e-9);
    }

    #[test]
    fn rotate_snaps_and_hit_tests() {
        let mut g = TransformGizmo::new([0.0, 0.0, 100.0, 100.0]);
        let rotate = g.handles(1.0).into_iter().find(|(h, _)| *h == GizmoHandle::Rotate).unwrap().1;
        assert!(close(rotate, [50.0, -28.0]));
        assert_eq!(g.hit_test([51.0, -27.0], 6.0, 1.0), Some(GizmoHandle::Rotate));
        assert_eq!(g.hit_test([30.0, 30.0], 6.0, 1.0), Some(GizmoHandle::Move));
        assert_eq!(g.hit_test([300.0, 300.0], 6.0, 1.0), None);
        // A quarter turn clockwise from the top handle to the right, snapped.
        drag(&mut g, GizmoHandle::Rotate, [50.0, -28.0], [130.0, 47.0], Modifiers { shift: true, alt: false });
        assert_eq!(g.affine.rotation, 90.0);
        assert!(close(g.matrix().apply([50.0, 50.0]), [50.0, 50.0]), "about the centre");
    }

    #[test]
    fn moving_the_pivot_keeps_the_image() {
        let mut g = TransformGizmo::new([0.0, 0.0, 100.0, 100.0]);
        g.affine.rotation = 30.0;
        let before = g.matrix();
        drag(&mut g, GizmoHandle::Pivot, before.apply([50.0, 50.0]), [10.0, 10.0], Modifiers::default());
        let after = g.matrix();
        for p in [[0.0, 0.0], [100.0, 40.0]] {
            assert!(close(before.apply(p), after.apply(p)));
        }
        assert!(close(after.apply(g.affine.pivot), [10.0, 10.0]));
    }

    #[test]
    fn perspective_mirrors_and_free_distort_does_not() {
        let mut g = TransformGizmo::new([0.0, 0.0, 100.0, 100.0]);
        g.set_mode(TransformMode::Perspective);
        drag(&mut g, GizmoHandle::Corner(0), [0.0, 0.0], [20.0, 0.0], Modifiers::default());
        assert_eq!(g.corners[0], [20.0, 0.0]);
        assert_eq!(g.corners[1], [80.0, 0.0], "top edge narrows symmetrically");
        assert_eq!(g.corners[2], [100.0, 100.0]);
        let m = g.matrix();
        assert!(close(m.apply([0.0, 0.0]), [20.0, 0.0]) && !m.is_affine());

        g.set_mode(TransformMode::FreeDistort);
        drag(&mut g, GizmoHandle::Corner(2), [100.0, 100.0], [120.0, 90.0], Modifiers::default());
        assert_eq!(g.corners[2], [120.0, 90.0]);
        assert_eq!(g.corners[3], [0.0, 100.0]);
        // A corner dragged across the quad would fold it: refused.
        drag(&mut g, GizmoHandle::Corner(0), [20.0, 0.0], [200.0, 200.0], Modifiers::default());
        assert_eq!(g.corners[0], [20.0, 0.0]);
    }

    #[test]
    fn modes_carry_the_shape_and_hud_edits_apply() {
        let mut g = TransformGizmo::new([0.0, 0.0, 100.0, 100.0]);
        g.affine.scale = [2.0, 2.0];
        g.set_mode(TransformMode::MeshWarp);
        assert!(close(g.warp.evaluate(1.0, 1.0), g.affine.matrix().apply([100.0, 100.0])));
        let start = g.warp.point(2, 2);
        drag(&mut g, GizmoHandle::MeshPoint(2, 2), start, [0.0, 0.0], Modifiers::default());
        assert_eq!(g.warp.point(2, 2), [0.0, 0.0]);

        let mut g = TransformGizmo::new([0.0, 0.0, 100.0, 100.0]);
        let mut hud = g.hud();
        hud.scale = [50.0, 50.0];
        hud.rotation = 45.0;
        hud.position = [10.0, 20.0];
        hud.interpolation = Interpolation::Lanczos3;
        g.set_hud(hud);
        let after = g.hud();
        assert!(close(after.position, [10.0, 20.0]));
        assert!((after.scale[0] - 50.0).abs() < 1e-9 && after.rotation == 45.0);
        assert_eq!(after.interpolation, Interpolation::Lanczos3);
        g.reset();
        assert_eq!(g.matrix(), Mat3::IDENTITY);
        assert_eq!(g.interpolation, Interpolation::Lanczos3);
    }

    #[test]
    fn skew_edges_shear() {
        let mut g = TransformGizmo::new([0.0, 0.0, 100.0, 100.0]);
        g.set_mode(TransformMode::Skew);
        drag(&mut g, GizmoHandle::Scale(BoxHandle::Bottom), [50.0, 100.0], [70.0, 100.0], Modifiers::default());
        let m = g.matrix();
        // Bottom edge moved right by 20, top edge left by 20 (shear about the centre).
        assert!(close(m.apply([50.0, 100.0]), [70.0, 100.0]), "{:?}", m.apply([50.0, 100.0]));
        assert!(close(m.apply([50.0, 0.0]), [30.0, 0.0]));
    }
}
