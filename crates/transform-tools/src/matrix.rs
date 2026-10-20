//! 3×3 projective matrices (homographies) and the parameter sets the
//! transform modes edit.
//!
//! Points are canvas pixels, y down, as row vectors `[x, y, 1]` multiplied on
//! the right: `p' = M · p` with the perspective divide by the third component.
//! Matrices are `f64`; the GPU gets `f32` copies of the inverse.

use std::ops::Mul;

pub type Point = [f64; 2];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat3(pub [[f64; 3]; 3]);

impl Mat3 {
    pub const IDENTITY: Mat3 = Mat3([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);

    pub fn translate(dx: f64, dy: f64) -> Mat3 {
        Mat3([[1.0, 0.0, dx], [0.0, 1.0, dy], [0.0, 0.0, 1.0]])
    }

    pub fn scale(sx: f64, sy: f64) -> Mat3 {
        Mat3([[sx, 0.0, 0.0], [0.0, sy, 0.0], [0.0, 0.0, 1.0]])
    }

    /// Clockwise on screen (y down) by `radians`.
    pub fn rotate(radians: f64) -> Mat3 {
        let (s, c) = radians.sin_cos();
        Mat3([[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]])
    }

    /// Shear: x += kx·y, y += ky·x.
    pub fn skew(kx: f64, ky: f64) -> Mat3 {
        Mat3([[1.0, kx, 0.0], [ky, 1.0, 0.0], [0.0, 0.0, 1.0]])
    }

    /// `m` applied about `pivot` instead of the origin.
    pub fn about(pivot: Point, m: Mat3) -> Mat3 {
        Mat3::translate(pivot[0], pivot[1]) * m * Mat3::translate(-pivot[0], -pivot[1])
    }

    pub fn apply(&self, [x, y]: Point) -> Point {
        let m = &self.0;
        let w = m[2][0] * x + m[2][1] * y + m[2][2];
        let w = if w.abs() < 1e-12 { 1e-12_f64.copysign(w) } else { w };
        [(m[0][0] * x + m[0][1] * y + m[0][2]) / w, (m[1][0] * x + m[1][1] * y + m[1][2]) / w]
    }

    pub fn determinant(&self) -> f64 {
        let m = &self.0;
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    }

    pub fn inverse(&self) -> Option<Mat3> {
        let det = self.determinant();
        if det.abs() < 1e-12 {
            return None;
        }
        let m = &self.0;
        let c = |r0: usize, c0: usize, r1: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
        let adj = [
            [c(1, 1, 2, 2), -c(0, 1, 2, 2), c(0, 1, 1, 2)],
            [-c(1, 0, 2, 2), c(0, 0, 2, 2), -c(0, 0, 1, 2)],
            [c(1, 0, 2, 1), -c(0, 0, 2, 1), c(0, 0, 1, 1)],
        ];
        Some(Mat3(adj.map(|row| row.map(|v| v / det))))
    }

    /// Whether the bottom row is (0, 0, 1): no perspective.
    pub fn is_affine(&self) -> bool {
        let m = &self.0;
        m[2][0].abs() < 1e-12 && m[2][1].abs() < 1e-12 && (m[2][2] - 1.0).abs() < 1e-12
    }

    /// Row-major `f32` for uniforms, normalised so `m[2][2]` is 1 when possible.
    pub fn to_f32(&self) -> [[f32; 3]; 3] {
        let k = if self.0[2][2].abs() > 1e-12 { self.0[2][2] } else { 1.0 };
        self.0.map(|row| row.map(|v| (v / k) as f32))
    }

    /// The homography taking the unit square's corners (0,0), (1,0), (1,1),
    /// (0,1) to `quad` (Heckbert's closed form). `None` when three corners are
    /// collinear.
    pub fn square_to_quad(quad: [Point; 4]) -> Option<Mat3> {
        let [[x0, y0], [x1, y1], [x2, y2], [x3, y3]] = quad;
        let (sx, sy) = (x0 - x1 + x2 - x3, y0 - y1 + y2 - y3);
        if sx.abs() < 1e-12 && sy.abs() < 1e-12 {
            // A parallelogram: affine.
            return Some(Mat3([[x1 - x0, x3 - x0, x0], [y1 - y0, y3 - y0, y0], [0.0, 0.0, 1.0]]));
        }
        let (dx1, dy1, dx2, dy2) = (x1 - x2, y1 - y2, x3 - x2, y3 - y2);
        let den = dx1 * dy2 - dx2 * dy1;
        if den.abs() < 1e-12 {
            return None;
        }
        let g = (sx * dy2 - dx2 * sy) / den;
        let h = (dx1 * sy - sx * dy1) / den;
        let m = Mat3([
            [x1 - x0 + g * x1, x3 - x0 + h * x3, x0],
            [y1 - y0 + g * y1, y3 - y0 + h * y3, y0],
            [g, h, 1.0],
        ]);
        // Three collinear corners give a singular map.
        (m.determinant().abs() > 1e-9).then_some(m)
    }

    /// The homography taking `from` corners onto `to` corners (both in the
    /// order top-left, top-right, bottom-right, bottom-left).
    pub fn quad_to_quad(from: [Point; 4], to: [Point; 4]) -> Option<Mat3> {
        Some(Mat3::square_to_quad(to)? * Mat3::square_to_quad(from)?.inverse()?)
    }

    /// Maps the rectangle `(x, y, w, h)` onto `quad`.
    pub fn rect_to_quad(rect: [f64; 4], quad: [Point; 4]) -> Option<Mat3> {
        Mat3::quad_to_quad(rect_corners(rect), quad)
    }
}

impl Mul for Mat3 {
    type Output = Mat3;

    fn mul(self, rhs: Mat3) -> Mat3 {
        let (a, b) = (&self.0, &rhs.0);
        Mat3(std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|k| a[r][k] * b[k][c]).sum())))
    }
}

/// Corners of `(x, y, w, h)`: top-left, top-right, bottom-right, bottom-left.
pub fn rect_corners([x, y, w, h]: [f64; 4]) -> [Point; 4] {
    [[x, y], [x + w, y], [x + w, y + h], [x, y + h]]
}

/// What the Scale & Rotate and Skew modes edit; composed as
/// translate · rotate · skew · scale about the pivot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AffineParams {
    /// Pivot in source (untransformed) coordinates.
    pub pivot: Point,
    /// Where the pivot ends up, as an offset from where it started.
    pub translate: [f64; 2],
    pub scale: [f64; 2],
    /// Degrees, clockwise on screen.
    pub rotation: f64,
    /// Shear factors (x by y, y by x).
    pub skew: [f64; 2],
}

impl AffineParams {
    pub fn identity(pivot: Point) -> Self {
        Self { pivot, translate: [0.0; 2], scale: [1.0; 2], rotation: 0.0, skew: [0.0; 2] }
    }

    pub fn matrix(&self) -> Mat3 {
        let local = Mat3::rotate(self.rotation.to_radians()) * Mat3::skew(self.skew[0], self.skew[1]) * Mat3::scale(self.scale[0], self.scale[1]);
        Mat3::translate(self.translate[0], self.translate[1]) * Mat3::about(self.pivot, local)
    }
}

/// Rounds `degrees` to the nearest multiple of `step` (Shift while rotating).
pub fn snap_angle(degrees: f64, step: f64) -> f64 {
    if step <= 0.0 {
        return degrees;
    }
    (degrees / step).round() * step
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Point, b: Point) -> bool {
        (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6
    }

    #[test]
    fn affine_pieces_compose() {
        let m = Mat3::about([10.0, 10.0], Mat3::rotate(std::f64::consts::FRAC_PI_2));
        assert!(close(m.apply([20.0, 10.0]), [10.0, 20.0]), "clockwise on screen");
        assert!(close(m.apply([10.0, 10.0]), [10.0, 10.0]), "pivot stays");
        let inv = m.inverse().unwrap();
        assert!(close(inv.apply(m.apply([3.0, 7.0])), [3.0, 7.0]));
        assert!(Mat3::scale(0.0, 1.0).inverse().is_none());
        assert!(m.is_affine());
    }

    #[test]
    fn quads_map_exactly() {
        let quad = [[10.0, 5.0], [90.0, 20.0], [70.0, 80.0], [15.0, 60.0]];
        let h = Mat3::square_to_quad(quad).unwrap();
        for (corner, target) in rect_corners([0.0, 0.0, 1.0, 1.0]).into_iter().zip(quad) {
            assert!(close(h.apply(corner), target), "{corner:?}");
        }
        assert!(!h.is_affine());
        let rect = [20.0, 30.0, 40.0, 20.0];
        let m = Mat3::rect_to_quad(rect, quad).unwrap();
        for (corner, target) in rect_corners(rect).into_iter().zip(quad) {
            assert!(close(m.apply(corner), target));
        }
        // Straight lines stay straight: the rect's centre goes to the diagonals' crossing.
        let c = m.apply([40.0, 40.0]);
        let cross = |p: Point, a: Point, b: Point| (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
        assert!(cross(c, quad[0], quad[2]).abs() < 1e-6 && cross(c, quad[1], quad[3]).abs() < 1e-6);
        assert!(Mat3::square_to_quad([[0.0, 0.0], [1.0, 0.0], [2.0, 0.0], [0.0, 1.0]]).is_none(), "collinear");
    }

    #[test]
    fn params_and_snapping() {
        let p = AffineParams { scale: [2.0, 0.5], rotation: 90.0, ..AffineParams::identity([0.0, 0.0]) };
        assert!(close(p.matrix().apply([1.0, 2.0]), [-1.0, 2.0]));
        let moved = AffineParams { translate: [5.0, -3.0], ..AffineParams::identity([4.0, 4.0]) };
        assert!(close(moved.matrix().apply([4.0, 4.0]), [9.0, 1.0]));
        assert_eq!(snap_angle(52.0, 15.0), 45.0);
        assert_eq!(snap_angle(-8.0, 15.0), -15.0);
    }
}
