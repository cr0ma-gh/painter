//! Viewport camera with Blender-style navigation (written from how the
//! controls behave, not from Blender's code): the view orbits a pivot at a
//! distance, with a view rotation, perspective or orthographic projection.
//!
//! | Input (Blender default)        | Method                     |
//! |--------------------------------|----------------------------|
//! | Middle drag                    | [`ViewCamera::orbit`]      |
//! | Shift + middle drag            | [`ViewCamera::pan`]        |
//! | Ctrl + middle drag, wheel      | [`ViewCamera::zoom`]       |
//! | Shift + Ctrl + middle drag     | [`ViewCamera::dolly`]      |
//! | Numpad 4 / 6 with Shift        | [`ViewCamera::roll`]       |
//! | Numpad 1 / 3 / 7 (+Ctrl)       | [`ViewCamera::set_view`]   |
//! | Numpad 5                       | toggle [`Projection`]      |
//! | Numpad . (frame selected)      | [`ViewCamera::frame`]      |
//!
//! Coordinates: world Z up; screen pixels with y down.

use crate::math::{vec3, Mat4, Quat, Vec3};
use crate::mesh::Aabb;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Projection {
    /// Vertical field of view in radians.
    Perspective { fov_y: f32 },
    Orthographic,
}

/// How a middle drag rotates the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrbitStyle {
    /// Around world Z, then the view's horizontal axis: the horizon stays level.
    Turntable,
    /// Free rotation following the pointer.
    Trackball,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewPreset {
    Front,
    Back,
    Right,
    Left,
    Top,
    Bottom,
}

/// A ray in world space (for picking and gizmo drags).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ray {
    pub origin: Vec3,
    /// Unit length.
    pub dir: Vec3,
}

impl Ray {
    pub fn at(&self, t: f32) -> Vec3 {
        self.origin + self.dir * t
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViewCamera {
    /// The point the view orbits.
    pub pivot: Vec3,
    pub distance: f32,
    /// Camera orientation: rotates camera axes (−Z forward, +Y up) into world space.
    pub rotation: Quat,
    pub projection: Projection,
    pub orbit_style: OrbitStyle,
    pub near: f32,
    pub far: f32,
    /// Radians of orbit per pixel dragged.
    pub orbit_speed: f32,
    /// Lens shift: where the view's centre sits, in normalised device
    /// coordinates (x right, y up); the image moves the other way. Zero for
    /// a plain viewport; Blender cameras use it for architectural framing.
    pub shift: [f32; 2],
}

impl Default for ViewCamera {
    /// Blender's startup view: looking at the origin from front-right-above.
    fn default() -> Self {
        let mut cam = Self {
            pivot: Vec3::ZERO,
            distance: 15.0,
            rotation: Quat::IDENTITY,
            projection: Projection::Perspective { fov_y: 50f32.to_radians() },
            orbit_style: OrbitStyle::Turntable,
            near: 0.01,
            far: 1000.0,
            orbit_speed: 0.008,
            shift: [0.0, 0.0],
        };
        cam.set_view(ViewPreset::Front);
        // Front-right and from above, like Blender's startup view.
        cam.orbit(-45f32.to_radians() / cam.orbit_speed, 20f32.to_radians() / cam.orbit_speed);
        cam
    }
}

impl ViewCamera {
    pub fn forward(&self) -> Vec3 {
        self.rotation.rotate(-Vec3::Z)
    }

    pub fn right(&self) -> Vec3 {
        self.rotation.rotate(Vec3::X)
    }

    pub fn up(&self) -> Vec3 {
        self.rotation.rotate(Vec3::Y)
    }

    pub fn eye(&self) -> Vec3 {
        self.pivot - self.forward() * self.distance
    }

    pub fn view_matrix(&self) -> Mat4 {
        Mat4::look_at(self.eye(), self.pivot, self.up())
    }

    /// Half the visible height at the pivot's distance, in world units.
    pub fn half_height(&self) -> f32 {
        match self.projection {
            Projection::Perspective { fov_y } => self.distance * (fov_y * 0.5).tan(),
            Projection::Orthographic => self.distance * (25f32.to_radians()).tan(),
        }
    }

    pub fn projection_matrix(&self, aspect: f32) -> Mat4 {
        let [sx, sy] = self.shift;
        match self.projection {
            Projection::Perspective { fov_y } => {
                let mut m = Mat4::perspective(fov_y, aspect, self.near, self.far);
                // x_ndc gains −m[2][0] (w = −z), so the image moves by −shift.
                m.0[2][0] = sx;
                m.0[2][1] = sy;
                m
            }
            // Ortho needs the camera back far enough not to clip what's in front of the pivot.
            Projection::Orthographic => {
                let mut m = Mat4::orthographic(self.half_height(), aspect, -self.far, self.far);
                m.0[3][0] = -sx;
                m.0[3][1] = -sy;
                m
            }
        }
    }

    pub fn view_projection(&self, aspect: f32) -> Mat4 {
        self.projection_matrix(aspect) * self.view_matrix()
    }

    /// Orbits by a pointer drag of `(dx, dy)` pixels.
    pub fn orbit(&mut self, dx: f32, dy: f32) {
        let (yaw, pitch) = (-dx * self.orbit_speed, -dy * self.orbit_speed);
        self.rotation = match self.orbit_style {
            OrbitStyle::Turntable => {
                let around_z = Quat::from_axis_angle(Vec3::Z, yaw);
                let tilt = Quat::from_axis_angle(self.right(), pitch);
                (around_z * tilt * self.rotation).normalize()
            }
            OrbitStyle::Trackball => {
                // Rotate about the axis perpendicular to the drag, in the view plane.
                let axis = self.up() * dx + self.right() * dy;
                if axis.length() < 1e-9 {
                    return;
                }
                let angle = (dx * dx + dy * dy).sqrt() * self.orbit_speed;
                (Quat::from_axis_angle(axis, -angle) * self.rotation).normalize()
            }
        };
    }

    /// Pans so the point under the pointer follows it: `(dx, dy)` pixels in a
    /// viewport `height` pixels tall.
    pub fn pan(&mut self, dx: f32, dy: f32, height: f32) {
        let per_pixel = 2.0 * self.half_height() / height.max(1.0);
        self.pivot = self.pivot - self.right() * (dx * per_pixel) + self.up() * (dy * per_pixel);
    }

    /// Zooms towards the pivot: `factor` < 1 closer, > 1 further.
    pub fn zoom(&mut self, factor: f32) {
        self.distance = (self.distance * factor).clamp(1e-3, self.far * 0.5);
    }

    /// Moves the pivot (and the view with it) along the view direction.
    pub fn dolly(&mut self, amount: f32) {
        self.pivot = self.pivot + self.forward() * amount;
    }

    /// Rolls the view around its own axis.
    pub fn roll(&mut self, angle: f32) {
        self.rotation = (Quat::from_axis_angle(self.forward(), angle) * self.rotation).normalize();
    }

    pub fn set_view(&mut self, preset: ViewPreset) {
        // Camera axes (right, up, back) for each preset.
        let (right, up) = match preset {
            ViewPreset::Front => (Vec3::X, Vec3::Z),
            ViewPreset::Back => (-Vec3::X, Vec3::Z),
            ViewPreset::Right => (Vec3::Y, Vec3::Z),
            ViewPreset::Left => (-Vec3::Y, Vec3::Z),
            ViewPreset::Top => (Vec3::X, Vec3::Y),
            ViewPreset::Bottom => (Vec3::X, -Vec3::Y),
        };
        self.rotation = quat_from_axes(right, up, right.cross(up));
    }

    pub fn toggle_projection(&mut self) {
        self.projection = match self.projection {
            Projection::Perspective { .. } => Projection::Orthographic,
            Projection::Orthographic => Projection::Perspective { fov_y: 50f32.to_radians() },
        };
    }

    /// Centres the view on `bounds` and backs off until they fit.
    pub fn frame(&mut self, bounds: &Aabb) {
        self.pivot = bounds.center();
        let radius = bounds.radius().max(1e-3);
        let half_fov = match self.projection {
            Projection::Perspective { fov_y } => fov_y * 0.5,
            Projection::Orthographic => 25f32.to_radians(),
        };
        self.distance = radius / half_fov.sin() * 1.1;
    }

    /// The ray through pixel `(x, y)` of a `width` × `height` viewport.
    pub fn ray(&self, x: f32, y: f32, width: f32, height: f32) -> Ray {
        let ndc = vec3(x / width.max(1.0) * 2.0 - 1.0, 1.0 - y / height.max(1.0) * 2.0, 0.0);
        let inv = self.view_projection(width / height.max(1.0)).inverse().unwrap_or(Mat4::IDENTITY);
        let near = inv.transform_point(vec3(ndc.x, ndc.y, 0.0));
        let far = inv.transform_point(vec3(ndc.x, ndc.y, 1.0));
        Ray { origin: near, dir: (far - near).normalize() }
    }

    /// Where a world point lands in a `width` × `height` viewport, if in front.
    pub fn to_screen(&self, p: Vec3, width: f32, height: f32) -> Option<[f32; 2]> {
        let view = self.view_matrix().transform_point(p);
        if matches!(self.projection, Projection::Perspective { .. }) && view.z >= -self.near {
            return None;
        }
        let ndc = self.view_projection(width / height.max(1.0)).transform_point(p);
        Some([(ndc.x + 1.0) * 0.5 * width, (1.0 - ndc.y) * 0.5 * height])
    }
}

impl ViewCamera {
    /// The view through a Blender camera on a `width` × `height` canvas: same
    /// position, orientation, focal length, sensor fit, lens shift and clipping.
    /// When the canvas's shape differs from the render's, the whole camera
    /// frame fits inside it, centred (like Blender's camera view). `focus` is
    /// the point to orbit afterwards (e.g. the scene's centre).
    pub fn from_blend(cam: &crate::blend_loader::BlendCamera, width: f32, height: f32, focus: Option<Vec3>) -> ViewCamera {
        use crate::blend_loader::SensorFit;
        let (hw, hh) = cam.frame_half_size();
        // Lens shift is measured in the fitted side of the frame.
        let horizontal = match cam.sensor_fit {
            SensorFit::Auto => hw >= hh,
            SensorFit::Horizontal => true,
            SensorFit::Vertical => false,
        };
        let unit = 2.0 * if horizontal { hw } else { hh };
        let offset = [cam.shift[0] * unit, cam.shift[1] * unit];
        // The canvas around the frame.
        let aspect = width / height.max(1.0);
        let canvas_hh = hh.max(hw / aspect);
        let canvas_hw = canvas_hh * aspect;
        let mut view = ViewCamera {
            rotation: cam.rotation.normalize(),
            near: cam.clip_start.max(1e-4),
            far: cam.clip_end.max(cam.clip_start * 2.0),
            shift: [offset[0] / canvas_hw, offset[1] / canvas_hh],
            ..ViewCamera::default()
        };
        let forward = view.forward();
        let depth = focus.map_or(0.0, |p| (p - cam.location).dot(forward));
        if cam.orthographic {
            view.projection = Projection::Orthographic;
            // `half_height` is distance · tan 25° for orthographic views.
            view.distance = canvas_hh / 25f32.to_radians().tan();
        } else {
            view.projection = Projection::Perspective { fov_y: 2.0 * canvas_hh.atan() };
            view.distance = if depth > view.near { depth } else { 10.0 };
        }
        // The eye stays on the camera.
        view.pivot = cam.location + forward * view.distance;
        view
    }

    /// Where lines along world direction `dir` meet on a `width` × `height`
    /// canvas: their vanishing point, or `None` when they stay parallel on
    /// screen (orthographic views, or `dir` across the line of sight).
    pub fn vanishing_point(&self, dir: Vec3, width: f32, height: f32) -> Option<[f32; 2]> {
        let Projection::Perspective { fov_y } = self.projection else { return None };
        let dir = dir.normalize();
        let depth = dir.dot(self.forward());
        if depth.abs() < 1e-6 {
            return None;
        }
        let f = 1.0 / (fov_y * 0.5).tan();
        let aspect = width / height.max(1.0);
        let x = f / aspect * dir.dot(self.right()) / depth - self.shift[0];
        let y = f * dir.dot(self.up()) / depth - self.shift[1];
        let p = [(x + 1.0) * 0.5 * width, (1.0 - y) * 0.5 * height];
        // Too far to be told apart from parallel lines (and to stay precise in f32).
        let reach = 50.0 * (width * width + height * height).sqrt();
        let (cx, cy) = (p[0] - width * 0.5, p[1] - height * 0.5);
        (cx * cx + cy * cy).sqrt().le(&reach).then_some(p)
    }

    /// Vanishing points of the world X, Y and Z axes (those that converge on
    /// the canvas), for a perspective ruler matching the view.
    pub fn axis_vanishing_points(&self, width: f32, height: f32) -> Vec<[f32; 2]> {
        [Vec3::X, Vec3::Y, Vec3::Z].into_iter().filter_map(|d| self.vanishing_point(d, width, height)).collect()
    }

    /// The camera a perspective drawing implies: the inverse of
    /// [`Self::axis_vanishing_points`], for 1 to 3 vanishing points on a
    /// `width` × `height` canvas. The eye stands `eye_height` above the ground
    /// (z = 0) over the origin, looking towards the points.
    ///
    /// - 1 point: the depth axis (world Y), a level horizon, a 50° lens.
    /// - 2 points: two perpendicular directions (the horizontal X and Y, or
    ///   one of them and the vertical); the lens follows from them, with the
    ///   image centre at the canvas centre.
    /// - 3 points: X, Y and Z; the image centre is the points' orthocentre
    ///   (a lens shift when it isn't the canvas centre).
    ///
    /// `None` when the points can't be a camera's (e.g. two horizontal points
    /// on the same side of the image centre).
    pub fn from_vanishing_points(points: &[[f32; 2]], width: f32, height: f32, eye_height: f32) -> Option<ViewCamera> {
        let (w, h) = (width.max(1.0), height.max(1.0));
        let mut centre = [w * 0.5, h * 0.5];
        if points.len() >= 3 {
            centre = orthocentre(points[0], points[1], points[2])?;
        }
        let rel = |p: [f32; 2]| [p[0] - centre[0], centre[1] - p[1]];
        // Focal length in pixels.
        let focal = match points.len() {
            0 => return None,
            1 => h * 0.5 / (25f32.to_radians()).tan(),
            _ => {
                let (a, b) = (rel(points[0]), rel(points[1]));
                let f2 = -(a[0] * b[0] + a[1] * b[1]);
                if f2 <= 1.0 {
                    return None;
                }
                f2.sqrt()
            }
        };
        // A point's direction in camera axes (right, up, back: right-handed, so crosses hold).
        let dir = |p: [f32; 2]| {
            let r = rel(p);
            vec3(r[0] / focal, r[1] / focal, -1.0).normalize()
        };
        let dirs: Vec<Vec3> = points.iter().take(3).map(|&p| dir(p)).collect();
        // World axes in camera coordinates.
        let (x, z) = match dirs.len() {
            1 => {
                // Looking along +Y with a level horizon.
                let y = dirs[0];
                let z = (Vec3::Y - y * y.dot(Vec3::Y)).normalize();
                (y.cross(z), z)
            }
            2 => {
                // One of them may be the vertical.
                match (0..2).find(|&i| dirs[i].y.abs() > 0.8 && dirs[1 - i].y.abs() < 0.5) {
                    Some(i) => (dirs[1 - i], if dirs[i].y > 0.0 { dirs[i] } else { -dirs[i] }),
                    None => {
                        let (x, mut y) = (dirs[0], dirs[1]);
                        if x.cross(y).y < 0.0 {
                            y = -y;
                        }
                        (x, x.cross(y))
                    }
                }
            }
            _ => {
                // The vertical is the one closest to the camera's up.
                let k = (0..3).max_by(|&a, &b| dirs[a].y.abs().total_cmp(&dirs[b].y.abs()))?;
                let z = if dirs[k].y > 0.0 { dirs[k] } else { -dirs[k] };
                let x = dirs[(0..3).find(|&i| i != k)?];
                (x, z)
            }
        };
        // Square them up (hand-placed points are never exact).
        let x = (x - z * x.dot(z)).normalize();
        let y = z.cross(x);
        // Camera axes in world coordinates: the rows of [x y z].
        let right = vec3(x.x, y.x, z.x);
        let up = vec3(x.y, y.y, z.y);
        let forward = -vec3(x.z, y.z, z.z);
        let mut cam = ViewCamera {
            rotation: quat_from_axes(right, up, -forward),
            projection: Projection::Perspective { fov_y: 2.0 * (h * 0.5 / focal).atan() },
            shift: [-(centre[0] / w * 2.0 - 1.0), -(1.0 - centre[1] / h * 2.0)],
            ..ViewCamera::default()
        };
        let eye = vec3(0.0, 0.0, eye_height);
        // Orbit the spot on the ground straight ahead, or a point 10 units away.
        cam.distance = if forward.z < -0.02 { (eye_height / -forward.z).clamp(0.5, 200.0) } else { 10.0 };
        cam.pivot = eye + forward * cam.distance;
        Some(cam)
    }
}

/// Where a triangle's three altitudes meet.
fn orthocentre(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> Option<[f32; 2]> {
    // Solve (p − a)·(c − b) = 0 and (p − b)·(c − a) = 0.
    let (d1, d2) = ([c[0] - b[0], c[1] - b[1]], [c[0] - a[0], c[1] - a[1]]);
    let (r1, r2) = (a[0] * d1[0] + a[1] * d1[1], b[0] * d2[0] + b[1] * d2[1]);
    let det = d1[0] * d2[1] - d1[1] * d2[0];
    if det.abs() < 1e-6 {
        return None;
    }
    Some([(r1 * d2[1] - r2 * d1[1]) / det, (d1[0] * r2 - d2[0] * r1) / det])
}

/// The rotation taking camera axes X, Y, Z onto `right`, `up`, `back`.
pub fn quat_from_axes(right: Vec3, up: Vec3, back: Vec3) -> Quat {
    let m = [[right.x, right.y, right.z], [up.x, up.y, up.z], [back.x, back.y, back.z]];
    // Columns are the axes; m[c][r].
    let trace = m[0][0] + m[1][1] + m[2][2];
    let q = if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        Quat { w: 0.25 * s, x: (m[1][2] - m[2][1]) / s, y: (m[2][0] - m[0][2]) / s, z: (m[0][1] - m[1][0]) / s }
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0;
        Quat { w: (m[1][2] - m[2][1]) / s, x: 0.25 * s, y: (m[1][0] + m[0][1]) / s, z: (m[2][0] + m[0][2]) / s }
    } else if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0;
        Quat { w: (m[2][0] - m[0][2]) / s, x: (m[1][0] + m[0][1]) / s, y: 0.25 * s, z: (m[2][1] + m[1][2]) / s }
    } else {
        let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0;
        Quat { w: (m[0][1] - m[1][0]) / s, x: (m[2][0] + m[0][2]) / s, y: (m[2][1] + m[1][2]) / s, z: 0.25 * s }
    };
    q.normalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-3
    }

    #[test]
    fn presets_look_along_the_right_axes() {
        let mut cam = ViewCamera::default();
        cam.set_view(ViewPreset::Front);
        assert!(close(cam.forward(), Vec3::Y), "front looks along +Y");
        assert!(close(cam.up(), Vec3::Z));
        cam.set_view(ViewPreset::Top);
        assert!(close(cam.forward(), -Vec3::Z));
        cam.set_view(ViewPreset::Right);
        assert!(close(cam.forward(), -Vec3::X));
    }

    #[test]
    fn starts_front_right_above() {
        let eye = ViewCamera::default().eye();
        assert!(eye.x > 0.0 && eye.y < 0.0 && eye.z > 0.0, "{eye:?}");
    }

    #[test]
    fn turntable_keeps_the_horizon_level() {
        let mut cam = ViewCamera::default();
        for _ in 0..20 {
            cam.orbit(13.0, -7.0);
        }
        assert!(cam.right().z.abs() < 1e-4, "right vector stays horizontal: {:?}", cam.right());
        assert!(close(cam.pivot, Vec3::ZERO), "orbiting doesn't move the pivot");
        let d = (cam.eye() - cam.pivot).length();
        assert!((d - cam.distance).abs() < 1e-3);
    }

    #[test]
    fn rays_and_projection_agree() {
        let mut cam = ViewCamera::default();
        cam.frame(&Aabb { min: vec3(-1.0, -1.0, -1.0), max: vec3(1.0, 1.0, 1.0) });
        let (w, h) = (800.0, 600.0);
        // The centre ray hits the pivot.
        let ray = cam.ray(w / 2.0, h / 2.0, w, h);
        let t = (cam.pivot - ray.origin).dot(ray.dir);
        assert!(close(ray.at(t), cam.pivot));
        // A point projects to the pixel whose ray passes through it.
        let p = vec3(0.7, -0.3, 0.5);
        let [sx, sy] = cam.to_screen(p, w, h).unwrap();
        let ray = cam.ray(sx, sy, w, h);
        let off = (p - ray.origin) - ray.dir * (p - ray.origin).dot(ray.dir);
        assert!(off.length() < 1e-3, "{off:?}");
        cam.toggle_projection();
        let [sx, sy] = cam.to_screen(p, w, h).unwrap();
        let ray = cam.ray(sx, sy, w, h);
        let off = (p - ray.origin) - ray.dir * (p - ray.origin).dot(ray.dir);
        assert!(off.length() < 1e-3, "ortho: {off:?}");
    }

    #[test]
    fn cameras_from_vanishing_points_round_trip() {
        let (w, h) = (1600.0, 1000.0);
        // Two-point (level), three-point (looking down) and one-point (straight along Y).
        let mut two = ViewCamera::default();
        two.set_view(ViewPreset::Front);
        two.orbit(-35f32.to_radians() / two.orbit_speed, 0.0);
        let three = ViewCamera::default();
        let mut one = ViewCamera::default();
        one.set_view(ViewPreset::Front);
        for (cam, n) in [(two, 2), (three, 3), (one, 1)] {
            let points = cam.axis_vanishing_points(w, h);
            assert_eq!(points.len(), n, "{points:?} {:?}", cam.forward());
            let back = ViewCamera::from_vanishing_points(&points, w, h, 1.6).expect("a camera");
            let again = back.axis_vanishing_points(w, h);
            assert_eq!(again.len(), n, "{points:?} → {again:?}, fwd {:?}", back.forward());
            for p in &points {
                assert!(again.iter().any(|q| (p[0] - q[0]).hypot(p[1] - q[1]) < 0.5), "{n}: {points:?} → {again:?}");
            }
            // Upright, at eye height.
            assert!(back.up().z > 0.0);
            assert!((back.eye().z - 1.6).abs() < 1e-3);
            if n >= 2 {
                let (Projection::Perspective { fov_y: a }, Projection::Perspective { fov_y: b }) = (cam.projection, back.projection) else {
                    panic!("perspective")
                };
                assert!((a - b).abs() < 1e-3, "{a} vs {b}");
            }
        }
        // Two points on the same side of the centre: no camera sees them at a right angle.
        assert!(ViewCamera::from_vanishing_points(&[[900.0, 500.0], [1000.0, 500.0]], w, h, 1.6).is_none());
    }

    #[test]
    fn vanishing_points_are_where_far_points_land() {
        let mut cam = ViewCamera { shift: [0.12, -0.07], ..ViewCamera::default() };
        let (w, h) = (1200.0, 900.0);
        for d in [Vec3::X, Vec3::Y, Vec3::Z, vec3(0.3, -0.8, 0.2)] {
            // Towards whichever end of the line is in front.
            let d = if d.dot(cam.forward()) < 0.0 { -d } else { d };
            let far = cam.to_screen(cam.eye() + d * 1e4, w, h).unwrap();
            let vp = cam.vanishing_point(d, w, h).unwrap();
            assert!((far[0] - vp[0]).abs() < 1.0 && (far[1] - vp[1]).abs() < 1.0, "{d:?}: {far:?} vs {vp:?}");
            assert_eq!(cam.vanishing_point(-d, w, h), Some(vp), "both ends of a line share it");
        }
        // Across the line of sight: parallel on screen.
        assert_eq!(cam.vanishing_point(cam.right(), w, h), None);
        cam.toggle_projection();
        assert!(cam.axis_vanishing_points(w, h).is_empty(), "orthographic views have none");
    }

    #[test]
    fn pan_follows_the_pointer() {
        let mut cam = ViewCamera::default();
        let (w, h) = (800.0, 600.0);
        let before = cam.to_screen(Vec3::ZERO, w, h).unwrap();
        cam.pan(40.0, -25.0, h);
        let after = cam.to_screen(Vec3::ZERO, w, h).unwrap();
        assert!((after[0] - before[0] - 40.0).abs() < 0.5 && (after[1] - before[1] + 25.0).abs() < 0.5, "{before:?} → {after:?}");
        let d = cam.distance;
        cam.zoom(0.5);
        assert_eq!(cam.distance, d * 0.5);
        cam.roll(0.3);
        assert!(close(cam.pivot - cam.forward() * cam.distance, cam.eye()));
    }
}
