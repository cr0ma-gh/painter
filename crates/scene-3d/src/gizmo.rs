//! Object transforms, picking and the translate / rotate / scale gizmo
//! (written from how Blender's tools behave, not from its code).
//!
//! - **Picking**: a screen ray is tested against each object's world bounds
//!   (slab test), then its triangles (Möller–Trumbore); nearest hit wins.
//! - **Gizmo parts** are hit-tested in screen-space size (the gizmo keeps a
//!   constant size on screen): axis arrows and scale handles by ray-to-segment
//!   distance, rotation rings by where the ray meets the ring's plane.
//! - **Drags** like Blender's G / R / S with axis constraints: along an axis
//!   the object follows the point of the axis line closest to the pointer
//!   ray; on a plane (Shift + axis) or the view plane, the ray-plane hit;
//!   rings turn by the angle swept around the axis; the white ring (view
//!   axis) and trackball (R R) rotate freely. Ctrl snaps (1 unit, 5°, 0.1×).

use crate::camera::{Ray, ViewCamera};
use crate::math::{Mat4, Quat, Vec3};
use crate::mesh::{Aabb, Mesh};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub location: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self { location: Vec3::ZERO, rotation: Quat::IDENTITY, scale: Vec3 { x: 1.0, y: 1.0, z: 1.0 } }
    }
}

impl Transform {
    /// From Blender's object fields (Euler XYZ in radians).
    pub fn from_blender(location: [f32; 3], rotation: [f32; 3], scale: [f32; 3]) -> Self {
        Self { location: Vec3::from_array(location), rotation: Quat::from_euler_xyz(rotation), scale: Vec3::from_array(scale) }
    }

    pub fn matrix(&self) -> Mat4 {
        Mat4::from_trs(self.location, self.rotation, self.scale)
    }

    /// For the properties panel: Euler XYZ in degrees.
    pub fn euler_degrees(&self) -> [f32; 3] {
        self.rotation.to_euler_xyz().map(f32::to_degrees)
    }

    pub fn set_euler_degrees(&mut self, degrees: [f32; 3]) {
        self.rotation = Quat::from_euler_xyz(degrees.map(f32::to_radians));
    }
}

// ---- ray tests ----------------------------------------------------------------------

/// Distance along `ray` to `aabb`, if it hits (slab test).
pub fn ray_aabb(ray: &Ray, aabb: &Aabb) -> Option<f32> {
    let (mut t0, mut t1) = (0.0f32, f32::MAX);
    for axis in 0..3 {
        let (o, d) = (ray.origin.component(axis), ray.dir.component(axis));
        let (lo, hi) = (aabb.min.component(axis), aabb.max.component(axis));
        if d.abs() < 1e-12 {
            if o < lo || o > hi {
                return None;
            }
            continue;
        }
        let (a, b) = ((lo - o) / d, (hi - o) / d);
        t0 = t0.max(a.min(b));
        t1 = t1.min(a.max(b));
        if t0 > t1 {
            return None;
        }
    }
    Some(t0)
}

/// Distance along `ray` to triangle `abc` (Möller–Trumbore, both sides).
pub fn ray_triangle(ray: &Ray, [a, b, c]: [Vec3; 3]) -> Option<f32> {
    let (e1, e2) = (b - a, c - a);
    let p = ray.dir.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = ray.origin - a;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = ray.dir.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t > 0.0).then_some(t)
}

/// Where `ray` meets the plane through `point` with `normal`.
pub fn ray_plane(ray: &Ray, point: Vec3, normal: Vec3) -> Option<Vec3> {
    let d = ray.dir.dot(normal);
    if d.abs() < 1e-9 {
        return None;
    }
    let t = (point - ray.origin).dot(normal) / d;
    (t >= 0.0).then(|| ray.at(t))
}

/// The point of the line `origin + s·axis` closest to `ray` (axis constraints).
pub fn closest_on_line(ray: &Ray, origin: Vec3, axis: Vec3) -> Option<Vec3> {
    let a = axis.normalize();
    let w = origin - ray.origin;
    let (b, d, e) = (a.dot(ray.dir), a.dot(w), ray.dir.dot(w));
    let denom = 1.0 - b * b;
    if denom.abs() < 1e-9 {
        return None; // looking straight down the axis
    }
    let s = (b * e - d) / denom;
    Some(origin + a * s)
}

/// The point of the axis line under the pointer, the way it feels in Blender:
/// the ray meets the plane that contains the axis and faces the viewer most,
/// and that hit is projected onto the axis. Unlike the closest point between
/// the two lines, it tracks the pointer without perspective drift.
pub fn axis_point(ray: &Ray, origin: Vec3, axis: Vec3, view_dir: Vec3) -> Option<Vec3> {
    let a = axis.normalize();
    let normal = a.cross(view_dir.cross(a)).normalize();
    if normal.length() < 1e-6 {
        return closest_on_line(ray, origin, a);
    }
    let hit = ray_plane(ray, origin, normal).or_else(|| ray_plane(ray, origin, -normal))?;
    Some(origin + a * (hit - origin).dot(a))
}

/// Shortest distance between `ray` and the segment `a`–`b`.
pub fn ray_segment_distance(ray: &Ray, a: Vec3, b: Vec3) -> f32 {
    let axis = b - a;
    let len = axis.length();
    let p = closest_on_line(ray, a, axis).unwrap_or(a);
    let s = ((p - a).dot(axis) / (len * len).max(1e-12)).clamp(0.0, 1.0);
    let q = a + axis * s;
    let t = (q - ray.origin).dot(ray.dir).max(0.0);
    (ray.at(t) - q).length()
}

/// An object that can be picked.
pub struct Pickable<'a> {
    pub mesh: &'a Mesh,
    pub transform: &'a Transform,
}

/// The nearest object under `ray`: (index, distance).
pub fn pick(ray: &Ray, objects: &[Pickable<'_>]) -> Option<(usize, f32)> {
    let mut best: Option<(usize, f32)> = None;
    for (i, obj) in objects.iter().enumerate() {
        let m = obj.transform.matrix();
        let Some(inv) = m.inverse() else { continue };
        // Test in object space: the ray goes through the inverse transform.
        let origin = inv.transform_point(ray.origin);
        let local = Ray { origin, dir: (inv.transform_point(ray.at(1.0)) - origin).normalize() };
        if ray_aabb(&local, &obj.mesh.bounds).is_none() {
            continue;
        }
        let hit = obj.mesh.triangles().filter_map(|t| ray_triangle(&local, t)).fold(f32::MAX, f32::min);
        if hit == f32::MAX {
            continue;
        }
        let world = (m.transform_point(local.at(hit)) - ray.origin).length();
        if best.is_none_or(|(_, d)| world < d) {
            best = Some((i, world));
        }
    }
    best
}

// ---- the gizmo ------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GizmoMode {
    Translate,
    Rotate,
    Scale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Constraint {
    /// Along one axis (0 X, 1 Y, 2 Z).
    Axis(usize),
    /// In the plane perpendicular to an axis (Shift + axis key).
    Plane(usize),
    /// Facing the viewer (the gizmo centre / white ring / free move).
    View,
    /// Free rotation following the pointer (R R).
    Trackball,
}

/// Axis colours, Blender's convention (X red, Y green, Z blue).
pub const AXIS_COLORS: [[u8; 3]; 3] = [[230, 60, 70], [120, 200, 40], [60, 120, 230]];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapping {
    pub translate: f32,
    pub rotate_degrees: f32,
    pub scale: f32,
}

impl Default for Snapping {
    fn default() -> Self {
        Self { translate: 1.0, rotate_degrees: 5.0, scale: 0.1 }
    }
}

#[derive(Debug, Clone)]
struct Drag {
    constraint: Constraint,
    start: Transform,
    /// Where the drag started on its constraint (line, plane or ring).
    anchor: Vec3,
    /// Trackball and view scaling work in screen pixels.
    start_px: [f32; 2],
}

#[derive(Debug, Clone)]
pub struct Gizmo {
    pub mode: GizmoMode,
    /// Axes in the object's own orientation instead of the world's.
    pub local: bool,
    /// Size on screen, in pixels (arrow length / ring radius).
    pub size_px: f32,
    pub snapping: Snapping,
    drag: Option<Drag>,
}

impl Default for Gizmo {
    fn default() -> Self {
        Self { mode: GizmoMode::Translate, local: false, size_px: 90.0, snapping: Snapping::default(), drag: None }
    }
}

impl Gizmo {
    /// World units per screen pixel at `point` (keeps the gizmo a constant size).
    pub fn world_per_pixel(camera: &ViewCamera, point: Vec3, height: f32) -> f32 {
        let depth = (point - camera.eye()).dot(camera.forward()).max(camera.near);
        let half = match camera.projection {
            crate::camera::Projection::Perspective { fov_y } => depth * (fov_y * 0.5).tan(),
            crate::camera::Projection::Orthographic => camera.half_height(),
        };
        2.0 * half / height.max(1.0)
    }

    pub fn axes(&self, t: &Transform) -> [Vec3; 3] {
        if self.local { [t.rotation.rotate(Vec3::X), t.rotation.rotate(Vec3::Y), t.rotation.rotate(Vec3::Z)] } else { [Vec3::X, Vec3::Y, Vec3::Z] }
    }

    /// The part under `ray`, `None` if it misses. `scale` is world units per pixel.
    pub fn hit_test(&self, ray: &Ray, t: &Transform, scale: f32) -> Option<Constraint> {
        let (c, len, grab) = (t.location, self.size_px * scale, 8.0 * scale);
        let axes = self.axes(t);
        match self.mode {
            GizmoMode::Translate | GizmoMode::Scale => {
                // The centre handle moves / scales in the view plane.
                if ray_segment_distance(ray, c, c) <= grab * 1.5 {
                    return Some(Constraint::View);
                }
                let mut best: Option<(Constraint, f32)> = None;
                for (i, axis) in axes.iter().enumerate() {
                    let d = ray_segment_distance(ray, c + *axis * (len * 0.2), c + *axis * len);
                    if d <= grab && best.is_none_or(|(_, b)| d < b) {
                        best = Some((Constraint::Axis(i), d));
                    }
                }
                best.map(|(k, _)| k)
            }
            GizmoMode::Rotate => {
                let mut best: Option<(Constraint, f32)> = None;
                for (i, axis) in axes.iter().enumerate() {
                    if let Some(p) = ray_plane(ray, c, *axis) {
                        let off = ((p - c).length() - len).abs();
                        if off <= grab && best.is_none_or(|(_, b)| off < b) {
                            best = Some((Constraint::Axis(i), off));
                        }
                    }
                }
                best.map(|(k, _)| k).or_else(|| {
                    // Inside the rings: trackball.
                    let p = ray_plane(ray, c, -ray.dir)?;
                    ((p - c).length() < len).then_some(Constraint::Trackball)
                })
            }
        }
    }

    /// Where the drag's anchor lies for `constraint` (on the axis, plane or ring).
    fn constrained_point(&self, ray: &Ray, t: &Transform, constraint: Constraint, view_normal: Vec3) -> Option<Vec3> {
        let axes = self.axes(t);
        match (self.mode, constraint) {
            (GizmoMode::Rotate, Constraint::Axis(i)) => ray_plane(ray, t.location, axes[i]),
            (_, Constraint::Axis(i)) => axis_point(ray, t.location, axes[i], view_normal),
            (_, Constraint::Plane(i)) => ray_plane(ray, t.location, axes[i]),
            (_, Constraint::View) | (_, Constraint::Trackball) => ray_plane(ray, t.location, view_normal),
        }
    }

    pub fn begin(&mut self, constraint: Constraint, ray: &Ray, t: &Transform, camera: &ViewCamera, pointer_px: [f32; 2]) -> bool {
        let Some(anchor) = self.constrained_point(ray, t, constraint, camera.forward()) else { return false };
        self.drag = Some(Drag { constraint, start: *t, anchor, start_px: pointer_px });
        true
    }

    pub fn dragging(&self) -> Option<Constraint> {
        self.drag.as_ref().map(|d| d.constraint)
    }

    pub fn end(&mut self) {
        self.drag = None;
    }

    /// The transform for the pointer at `ray` / `pointer_px`; `snap` = Ctrl held.
    pub fn update(&self, ray: &Ray, camera: &ViewCamera, pointer_px: [f32; 2], snap: bool) -> Option<Transform> {
        let drag = self.drag.as_ref()?;
        let start = drag.start;
        let mut out = start;
        let axes = self.axes(&start);
        match (self.mode, drag.constraint) {
            (GizmoMode::Translate, constraint) => {
                let p = self.constrained_point(ray, &start, constraint, camera.forward())?;
                let mut delta = p - drag.anchor;
                if snap {
                    let s = self.snapping.translate;
                    delta = Vec3::from_array(delta.to_array().map(|v| (v / s).round() * s));
                }
                out.location = start.location + delta;
            }
            (GizmoMode::Rotate, Constraint::Axis(i)) => {
                let p = self.constrained_point(ray, &start, drag.constraint, camera.forward())?;
                let (a, b) = ((drag.anchor - start.location).normalize(), (p - start.location).normalize());
                let mut angle = a.cross(b).dot(axes[i]).atan2(a.dot(b));
                if snap {
                    let s = self.snapping.rotate_degrees.to_radians();
                    angle = (angle / s).round() * s;
                }
                out.rotation = (Quat::from_axis_angle(axes[i], angle) * start.rotation).normalize();
            }
            (GizmoMode::Rotate, _) => {
                // Trackball: drag direction sets the axis, distance the angle.
                let (dx, dy) = (pointer_px[0] - drag.start_px[0], pointer_px[1] - drag.start_px[1]);
                let axis = camera.up() * dx + camera.right() * dy;
                if axis.length() > 1e-6 {
                    let angle = (dx * dx + dy * dy).sqrt() * 0.01;
                    out.rotation = (Quat::from_axis_angle(axis, angle) * start.rotation).normalize();
                }
            }
            (GizmoMode::Scale, constraint) => {
                let p = self.constrained_point(ray, &start, constraint, camera.forward())?;
                let (d0, d1) = ((drag.anchor - start.location).length(), (p - start.location).length());
                if d0 < 1e-9 {
                    return Some(start);
                }
                let mut k = d1 / d0;
                // Dragging through the centre flips the sign.
                if (p - start.location).dot(drag.anchor - start.location) < 0.0 {
                    k = -k;
                }
                if snap {
                    let s = self.snapping.scale;
                    k = (k / s).round() * s;
                }
                let s = start.scale;
                out.scale = match constraint {
                    Constraint::Axis(i) => {
                        let mut v = s.to_array();
                        v[i] *= k;
                        Vec3::from_array(v)
                    }
                    Constraint::Plane(i) => {
                        let mut v = s.to_array();
                        for (j, c) in v.iter_mut().enumerate() {
                            if j != i {
                                *c *= k;
                            }
                        }
                        Vec3::from_array(v)
                    }
                    _ => s * k,
                };
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::ViewPreset;
    use crate::math::vec3;

    fn front_camera() -> ViewCamera {
        let mut cam = ViewCamera::default();
        cam.set_view(ViewPreset::Front);
        cam.distance = 10.0;
        cam
    }

    #[test]
    fn ray_primitives() {
        let ray = Ray { origin: vec3(0.0, -5.0, 0.0), dir: Vec3::Y };
        let unit = Aabb { min: vec3(-1.0, -1.0, -1.0), max: vec3(1.0, 1.0, 1.0) };
        assert_eq!(ray_aabb(&ray, &unit), Some(4.0));
        assert!(ray_aabb(&Ray { origin: vec3(3.0, -5.0, 0.0), dir: Vec3::Y }, &unit).is_none());
        let tri = [vec3(-1.0, 0.0, -1.0), vec3(1.0, 0.0, -1.0), vec3(0.0, 0.0, 1.0)];
        assert_eq!(ray_triangle(&ray, tri), Some(5.0));
        assert_eq!(ray_plane(&ray, Vec3::ZERO, Vec3::Y), Some(Vec3::ZERO));
        // The X axis line closest to a ray passing over x = 2.
        let over = Ray { origin: vec3(2.0, -5.0, 3.0), dir: Vec3::Y };
        assert!((closest_on_line(&over, Vec3::ZERO, Vec3::X).unwrap() - vec3(2.0, 0.0, 0.0)).length() < 1e-5);
        assert!((ray_segment_distance(&over, vec3(2.0, 0.0, 0.0), vec3(2.0, 0.0, 1.0)) - 2.0).abs() < 1e-5);
    }

    #[test]
    fn picks_the_nearest_object() {
        let cube = Mesh::cube();
        let near = Transform { location: vec3(0.0, -2.0, 0.0), ..Transform::default() };
        let far = Transform { location: vec3(0.0, 3.0, 0.0), scale: vec3(3.0, 3.0, 3.0), ..Transform::default() };
        let objects = [Pickable { mesh: &cube, transform: &far }, Pickable { mesh: &cube, transform: &near }];
        let cam = front_camera();
        let ray = cam.ray(400.0, 300.0, 800.0, 600.0);
        let (index, dist) = pick(&ray, &objects).unwrap();
        assert_eq!(index, 1);
        // Measured from the ray origin on the near plane (0.01 in front of the eye).
        assert!((dist - (10.0 - 2.5)).abs() < 0.02, "{dist}");
        assert!(pick(&cam.ray(5.0, 5.0, 800.0, 600.0), &objects).is_none());
    }

    #[test]
    fn translate_along_an_axis_and_snap() {
        let cam = front_camera();
        let (w, h) = (800.0, 600.0);
        let mut gizmo = Gizmo::default();
        let t = Transform::default();
        let scale = Gizmo::world_per_pixel(&cam, t.location, h);
        // Grab the X arrow two thirds along it.
        let tip = t.location + Vec3::X * (gizmo.size_px * scale * 0.66);
        let [px, py] = cam.to_screen(tip, w, h).unwrap();
        let ray = cam.ray(px, py, w, h);
        assert_eq!(gizmo.hit_test(&ray, &t, scale), Some(Constraint::Axis(0)));
        assert!(gizmo.begin(Constraint::Axis(0), &ray, &t, &cam, [px, py]));
        // Drag diagonally: only X changes.
        let target = cam.to_screen(tip + vec3(2.3, 0.0, 1.5), w, h).unwrap();
        let moved = gizmo.update(&cam.ray(target[0], target[1], w, h), &cam, target, false).unwrap();
        assert!((moved.location - vec3(2.3, 0.0, 0.0)).length() < 1e-3, "{:?}", moved.location);
        let snapped = gizmo.update(&cam.ray(target[0], target[1], w, h), &cam, target, true).unwrap();
        assert!((snapped.location.x - 2.0).abs() < 1e-5);
    }

    #[test]
    fn rotate_on_a_ring_and_scale_uniformly() {
        let mut cam = front_camera();
        cam.set_view(ViewPreset::Top);
        let (w, h) = (800.0, 600.0);
        let mut gizmo = Gizmo { mode: GizmoMode::Rotate, ..Gizmo::default() };
        let t = Transform::default();
        let r = gizmo.size_px * Gizmo::world_per_pixel(&cam, t.location, h);
        // From the top the Z ring is a circle: grab it at +X, drag to +Y.
        let a = cam.to_screen(vec3(r, 0.0, 0.0), w, h).unwrap();
        let ray = cam.ray(a[0], a[1], w, h);
        assert_eq!(gizmo.hit_test(&ray, &t, r / gizmo.size_px), Some(Constraint::Axis(2)));
        gizmo.begin(Constraint::Axis(2), &ray, &t, &cam, a);
        let b = cam.to_screen(vec3(0.0, r, 0.0), w, h).unwrap();
        let turned = gizmo.update(&cam.ray(b[0], b[1], w, h), &cam, b, false).unwrap();
        assert!((turned.euler_degrees()[2] - 90.0).abs() < 0.1, "{:?}", turned.euler_degrees());

        gizmo.mode = GizmoMode::Scale;
        let start = cam.to_screen(vec3(1.0, 0.0, 0.0), w, h).unwrap();
        gizmo.begin(Constraint::View, &cam.ray(start[0], start[1], w, h), &t, &cam, start);
        let end = cam.to_screen(vec3(2.5, 0.0, 0.0), w, h).unwrap();
        let scaled = gizmo.update(&cam.ray(end[0], end[1], w, h), &cam, end, false).unwrap();
        assert!((scaled.scale - vec3(2.5, 2.5, 2.5)).length() < 1e-3);
    }
}
