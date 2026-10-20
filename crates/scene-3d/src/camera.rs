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
        match self.projection {
            Projection::Perspective { fov_y } => Mat4::perspective(fov_y, aspect, self.near, self.far),
            // Ortho needs the camera back far enough not to clip what's in front of the pivot.
            Projection::Orthographic => Mat4::orthographic(self.half_height(), aspect, -self.far, self.far),
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

/// The rotation taking camera axes X, Y, Z onto `right`, `up`, `back`.
fn quat_from_axes(right: Vec3, up: Vec3, back: Vec3) -> Quat {
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
