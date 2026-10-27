//! Small 3D math: vectors, quaternions, column-major 4×4 matrices (the layout
//! wgpu uniforms want). Right-handed, Z up like Blender; clip space depth 0..1.

use std::ops::{Add, Mul, Neg, Sub};

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

pub const fn vec3(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3 { x, y, z }
}

impl Vec3 {
    pub const ZERO: Vec3 = vec3(0.0, 0.0, 0.0);
    pub const X: Vec3 = vec3(1.0, 0.0, 0.0);
    pub const Y: Vec3 = vec3(0.0, 1.0, 0.0);
    pub const Z: Vec3 = vec3(0.0, 0.0, 1.0);

    pub fn from_array(a: [f32; 3]) -> Self {
        vec3(a[0], a[1], a[2])
    }

    pub fn to_array(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }

    pub fn dot(self, o: Vec3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    pub fn cross(self, o: Vec3) -> Vec3 {
        vec3(self.y * o.z - self.z * o.y, self.z * o.x - self.x * o.z, self.x * o.y - self.y * o.x)
    }

    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    pub fn normalize(self) -> Vec3 {
        let l = self.length();
        if l > 1e-12 { self * (1.0 / l) } else { Vec3::ZERO }
    }

    pub fn min(self, o: Vec3) -> Vec3 {
        vec3(self.x.min(o.x), self.y.min(o.y), self.z.min(o.z))
    }

    pub fn max(self, o: Vec3) -> Vec3 {
        vec3(self.x.max(o.x), self.y.max(o.y), self.z.max(o.z))
    }

    pub fn component(self, axis: usize) -> f32 {
        [self.x, self.y, self.z][axis]
    }
}

impl Add for Vec3 {
    type Output = Vec3;
    fn add(self, o: Vec3) -> Vec3 {
        vec3(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}

impl Sub for Vec3 {
    type Output = Vec3;
    fn sub(self, o: Vec3) -> Vec3 {
        vec3(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}

impl Mul<f32> for Vec3 {
    type Output = Vec3;
    fn mul(self, k: f32) -> Vec3 {
        vec3(self.x * k, self.y * k, self.z * k)
    }
}

impl Neg for Vec3 {
    type Output = Vec3;
    fn neg(self) -> Vec3 {
        vec3(-self.x, -self.y, -self.z)
    }
}

/// Unit quaternion rotation (w + xi + yj + zk).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quat {
    pub w: f32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Quat {
    pub const IDENTITY: Quat = Quat { w: 1.0, x: 0.0, y: 0.0, z: 0.0 };

    pub fn from_axis_angle(axis: Vec3, angle: f32) -> Quat {
        let a = axis.normalize();
        let (s, c) = (angle * 0.5).sin_cos();
        Quat { w: c, x: a.x * s, y: a.y * s, z: a.z * s }
    }

    /// Blender's default XYZ Euler order: X first, then Y, then Z.
    pub fn from_euler_xyz(e: [f32; 3]) -> Quat {
        Quat::from_axis_angle(Vec3::Z, e[2]) * Quat::from_axis_angle(Vec3::Y, e[1]) * Quat::from_axis_angle(Vec3::X, e[0])
    }

    /// Back to XYZ Euler angles.
    pub fn to_euler_xyz(self) -> [f32; 3] {
        let m = Mat4::from_quat(self).0;
        // Rotation part, rows r, columns c: m[c][r].
        let sy = -m[0][2];
        let y = sy.clamp(-1.0, 1.0).asin();
        if sy.abs() < 0.99999 {
            [m[1][2].atan2(m[2][2]), y, m[0][1].atan2(m[0][0])]
        } else {
            [(-m[2][1]).atan2(m[1][1]), y, 0.0]
        }
    }

    pub fn normalize(self) -> Quat {
        let l = (self.w * self.w + self.x * self.x + self.y * self.y + self.z * self.z).sqrt().max(1e-12);
        Quat { w: self.w / l, x: self.x / l, y: self.y / l, z: self.z / l }
    }

    pub fn conjugate(self) -> Quat {
        Quat { w: self.w, x: -self.x, y: -self.y, z: -self.z }
    }

    pub fn rotate(self, v: Vec3) -> Vec3 {
        let q = vec3(self.x, self.y, self.z);
        let t = q.cross(v) * 2.0;
        v + t * self.w + q.cross(t)
    }

    /// The 4D dot product (the cosine of half the angle between two unit rotations).
    pub fn dot(self, o: Quat) -> f32 {
        self.w * o.w + self.x * o.x + self.y * o.y + self.z * o.z
    }

    /// Spherical interpolation from `self` (`t` = 0) to `to` (`t` = 1), the
    /// short way round, at constant angular speed.
    pub fn slerp(self, to: Quat, t: f32) -> Quat {
        // q and −q are the same rotation: take the nearer one.
        let (to, cos) = match self.dot(to) {
            d if d < 0.0 => (Quat { w: -to.w, x: -to.x, y: -to.y, z: -to.z }, -d),
            d => (to, d),
        };
        let (a, b) = if cos > 0.9995 {
            // Nearly the same rotation: a plain lerp is precise and avoids sin θ ≈ 0.
            (1.0 - t, t)
        } else {
            let theta = cos.min(1.0).acos();
            let s = theta.sin();
            (((1.0 - t) * theta).sin() / s, (t * theta).sin() / s)
        };
        Quat { w: self.w * a + to.w * b, x: self.x * a + to.x * b, y: self.y * a + to.y * b, z: self.z * a + to.z * b }.normalize()
    }
}

impl Mul for Quat {
    type Output = Quat;
    fn mul(self, o: Quat) -> Quat {
        Quat {
            w: self.w * o.w - self.x * o.x - self.y * o.y - self.z * o.z,
            x: self.w * o.x + self.x * o.w + self.y * o.z - self.z * o.y,
            y: self.w * o.y - self.x * o.z + self.y * o.w + self.z * o.x,
            z: self.w * o.z + self.x * o.y - self.y * o.x + self.z * o.w,
        }
    }
}

/// Column-major: `0[c][r]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat4(pub [[f32; 4]; 4]);

impl Mat4 {
    pub const IDENTITY: Mat4 = Mat4([[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]]);

    pub fn from_quat(q: Quat) -> Mat4 {
        let (x, y, z) = (Quat::rotate(q, Vec3::X), Quat::rotate(q, Vec3::Y), Quat::rotate(q, Vec3::Z));
        Mat4([[x.x, x.y, x.z, 0.0], [y.x, y.y, y.z, 0.0], [z.x, z.y, z.z, 0.0], [0.0, 0.0, 0.0, 1.0]])
    }

    /// translation · rotation · scale.
    pub fn from_trs(t: Vec3, r: Quat, s: Vec3) -> Mat4 {
        let mut m = Mat4::from_quat(r);
        for (c, k) in [s.x, s.y, s.z].into_iter().enumerate() {
            for r in 0..3 {
                m.0[c][r] *= k;
            }
        }
        m.0[3] = [t.x, t.y, t.z, 1.0];
        m
    }

    /// World → camera, looking from `eye` towards `target`.
    pub fn look_at(eye: Vec3, target: Vec3, up: Vec3) -> Mat4 {
        let f = (target - eye).normalize();
        let s = f.cross(up).normalize();
        let u = s.cross(f);
        Mat4([
            [s.x, u.x, -f.x, 0.0],
            [s.y, u.y, -f.y, 0.0],
            [s.z, u.z, -f.z, 0.0],
            [-s.dot(eye), -u.dot(eye), f.dot(eye), 1.0],
        ])
    }

    /// Right-handed perspective, depth 0..1 (wgpu).
    pub fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
        let f = 1.0 / (fov_y * 0.5).tan();
        let r = far / (near - far);
        Mat4([[f / aspect, 0.0, 0.0, 0.0], [0.0, f, 0.0, 0.0], [0.0, 0.0, r, -1.0], [0.0, 0.0, r * near, 0.0]])
    }

    /// Right-handed orthographic of half-height `half_h`, depth 0..1.
    pub fn orthographic(half_h: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
        let half_w = half_h * aspect;
        let r = 1.0 / (near - far);
        Mat4([[1.0 / half_w, 0.0, 0.0, 0.0], [0.0, 1.0 / half_h, 0.0, 0.0], [0.0, 0.0, r, 0.0], [0.0, 0.0, near * r, 1.0]])
    }

    pub fn transform_point(&self, p: Vec3) -> Vec3 {
        let m = &self.0;
        let v = [p.x, p.y, p.z, 1.0];
        let out: [f32; 4] = std::array::from_fn(|r| (0..4).map(|c| m[c][r] * v[c]).sum());
        let w = if out[3].abs() > 1e-12 { out[3] } else { 1.0 };
        vec3(out[0] / w, out[1] / w, out[2] / w)
    }

    pub fn transform_vector(&self, v: Vec3) -> Vec3 {
        let m = &self.0;
        vec3(m[0][0] * v.x + m[1][0] * v.y + m[2][0] * v.z, m[0][1] * v.x + m[1][1] * v.y + m[2][1] * v.z, m[0][2] * v.x + m[1][2] * v.y + m[2][2] * v.z)
    }

    pub fn inverse(&self) -> Option<Mat4> {
        // Cofactor expansion on the row-major view.
        let m: [f32; 16] = std::array::from_fn(|i| self.0[i / 4][i % 4]);
        let mut inv = [0f32; 16];
        inv[0] = m[5] * m[10] * m[15] - m[5] * m[11] * m[14] - m[9] * m[6] * m[15] + m[9] * m[7] * m[14] + m[13] * m[6] * m[11] - m[13] * m[7] * m[10];
        inv[4] = -m[4] * m[10] * m[15] + m[4] * m[11] * m[14] + m[8] * m[6] * m[15] - m[8] * m[7] * m[14] - m[12] * m[6] * m[11] + m[12] * m[7] * m[10];
        inv[8] = m[4] * m[9] * m[15] - m[4] * m[11] * m[13] - m[8] * m[5] * m[15] + m[8] * m[7] * m[13] + m[12] * m[5] * m[11] - m[12] * m[7] * m[9];
        inv[12] = -m[4] * m[9] * m[14] + m[4] * m[10] * m[13] + m[8] * m[5] * m[14] - m[8] * m[6] * m[13] - m[12] * m[5] * m[10] + m[12] * m[6] * m[9];
        inv[1] = -m[1] * m[10] * m[15] + m[1] * m[11] * m[14] + m[9] * m[2] * m[15] - m[9] * m[3] * m[14] - m[13] * m[2] * m[11] + m[13] * m[3] * m[10];
        inv[5] = m[0] * m[10] * m[15] - m[0] * m[11] * m[14] - m[8] * m[2] * m[15] + m[8] * m[3] * m[14] + m[12] * m[2] * m[11] - m[12] * m[3] * m[10];
        inv[9] = -m[0] * m[9] * m[15] + m[0] * m[11] * m[13] + m[8] * m[1] * m[15] - m[8] * m[3] * m[13] - m[12] * m[1] * m[11] + m[12] * m[3] * m[9];
        inv[13] = m[0] * m[9] * m[14] - m[0] * m[10] * m[13] - m[8] * m[1] * m[14] + m[8] * m[2] * m[13] + m[12] * m[1] * m[10] - m[12] * m[2] * m[9];
        inv[2] = m[1] * m[6] * m[15] - m[1] * m[7] * m[14] - m[5] * m[2] * m[15] + m[5] * m[3] * m[14] + m[13] * m[2] * m[7] - m[13] * m[3] * m[6];
        inv[6] = -m[0] * m[6] * m[15] + m[0] * m[7] * m[14] + m[4] * m[2] * m[15] - m[4] * m[3] * m[14] - m[12] * m[2] * m[7] + m[12] * m[3] * m[6];
        inv[10] = m[0] * m[5] * m[15] - m[0] * m[7] * m[13] - m[4] * m[1] * m[15] + m[4] * m[3] * m[13] + m[12] * m[1] * m[7] - m[12] * m[3] * m[5];
        inv[14] = -m[0] * m[5] * m[14] + m[0] * m[6] * m[13] + m[4] * m[1] * m[14] - m[4] * m[2] * m[13] - m[12] * m[1] * m[6] + m[12] * m[2] * m[5];
        inv[3] = -m[1] * m[6] * m[11] + m[1] * m[7] * m[10] + m[5] * m[2] * m[11] - m[5] * m[3] * m[10] - m[9] * m[2] * m[7] + m[9] * m[3] * m[6];
        inv[7] = m[0] * m[6] * m[11] - m[0] * m[7] * m[10] - m[4] * m[2] * m[11] + m[4] * m[3] * m[10] + m[8] * m[2] * m[7] - m[8] * m[3] * m[6];
        inv[11] = -m[0] * m[5] * m[11] + m[0] * m[7] * m[9] + m[4] * m[1] * m[11] - m[4] * m[3] * m[9] - m[8] * m[1] * m[7] + m[8] * m[3] * m[5];
        inv[15] = m[0] * m[5] * m[10] - m[0] * m[6] * m[9] - m[4] * m[1] * m[10] + m[4] * m[2] * m[9] + m[8] * m[1] * m[6] - m[8] * m[2] * m[5];
        let det = m[0] * inv[0] + m[1] * inv[4] + m[2] * inv[8] + m[3] * inv[12];
        if det.abs() < 1e-12 {
            return None;
        }
        Some(Mat4(std::array::from_fn(|c| std::array::from_fn(|r| inv[c * 4 + r] / det))))
    }
}

impl Mul for Mat4 {
    type Output = Mat4;
    fn mul(self, o: Mat4) -> Mat4 {
        Mat4(std::array::from_fn(|c| std::array::from_fn(|r| (0..4).map(|k| self.0[k][r] * o.0[c][k]).sum())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-4
    }

    #[test]
    fn rotations_and_matrices_agree() {
        let q = Quat::from_axis_angle(Vec3::Z, std::f32::consts::FRAC_PI_2);
        assert!(close(q.rotate(Vec3::X), Vec3::Y), "counter-clockwise about +Z");
        let e = [0.3, -0.7, 1.2];
        let back = Quat::from_euler_xyz(e).to_euler_xyz();
        assert!((back[0] - e[0]).abs() < 1e-4 && (back[1] - e[1]).abs() < 1e-4 && (back[2] - e[2]).abs() < 1e-4, "{back:?}");
        let m = Mat4::from_trs(vec3(1.0, 2.0, 3.0), q, vec3(2.0, 2.0, 2.0));
        assert!(close(m.transform_point(Vec3::X), vec3(1.0, 4.0, 3.0)));
        let inv = m.inverse().unwrap();
        assert!(close(inv.transform_point(m.transform_point(vec3(0.3, -2.0, 5.0))), vec3(0.3, -2.0, 5.0)));
        assert_eq!((m * Mat4::IDENTITY), m);
    }

    #[test]
    fn projections_map_depth_to_0_1() {
        let p = Mat4::perspective(1.0, 1.5, 0.1, 100.0);
        assert!((p.transform_point(vec3(0.0, 0.0, -0.1)).z - 0.0).abs() < 1e-4);
        assert!((p.transform_point(vec3(0.0, 0.0, -100.0)).z - 1.0).abs() < 1e-4);
        let o = Mat4::orthographic(5.0, 2.0, 0.1, 50.0);
        let c = o.transform_point(vec3(10.0, 5.0, -0.1));
        assert!((c.x - 1.0).abs() < 1e-5 && (c.y - 1.0).abs() < 1e-5 && c.z.abs() < 1e-5);
        let view = Mat4::look_at(vec3(0.0, -10.0, 0.0), Vec3::ZERO, Vec3::Z);
        assert!(close(view.transform_point(Vec3::ZERO), vec3(0.0, 0.0, -10.0)), "target straight ahead");
    }
}
