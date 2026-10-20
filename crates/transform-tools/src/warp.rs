//! Mesh warp: an N×M grid of control points over the source rectangle,
//! interpolated with a Catmull-Rom surface so the image bends smoothly.
//!
//! Rendering doesn't invert the warp. [`MeshWarp::tessellate`] samples the
//! surface into a fine triangle mesh whose vertices carry source coordinates;
//! the GPU draws it like any textured mesh and the CPU commit rasterises the
//! same triangles, so preview and result agree.

use crate::matrix::Point;

#[derive(Debug, Clone, PartialEq)]
pub struct MeshWarp {
    /// Source rectangle `(x, y, w, h)` the grid spans.
    pub rect: [f64; 4],
    /// Cells across and down (the grid has `cols + 1` × `rows + 1` points).
    pub cols: usize,
    pub rows: usize,
    /// Row-major control points in canvas space.
    pub points: Vec<Point>,
}

/// A tessellated warp: positions in canvas space, `uv` in source pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct WarpMesh {
    pub positions: Vec<Point>,
    pub uvs: Vec<Point>,
    pub indices: Vec<u32>,
}

impl MeshWarp {
    /// An undistorted grid over `rect`.
    pub fn new(rect: [f64; 4], cols: usize, rows: usize) -> Self {
        let (cols, rows) = (cols.max(1), rows.max(1));
        let points = (0..=rows)
            .flat_map(|j| {
                (0..=cols).map(move |i| [rect[0] + rect[2] * i as f64 / cols as f64, rect[1] + rect[3] * j as f64 / rows as f64])
            })
            .collect();
        Self { rect, cols, rows, points }
    }

    pub fn point(&self, i: usize, j: usize) -> Point {
        self.points[j * (self.cols + 1) + i]
    }

    pub fn point_mut(&mut self, i: usize, j: usize) -> &mut Point {
        &mut self.points[j * (self.cols + 1) + i]
    }

    /// Grid point, clamped so the surface extends past the border linearly.
    fn grid(&self, i: isize, j: isize) -> Point {
        let (ci, cj) = (i.clamp(0, self.cols as isize), j.clamp(0, self.rows as isize));
        let p = self.point(ci as usize, cj as usize);
        // Reflect beyond the edge so the border tangent continues the last cell.
        let (oi, oj) = (i - ci, j - cj);
        if oi == 0 && oj == 0 {
            return p;
        }
        let q = self.point((ci - oi.signum()).clamp(0, self.cols as isize) as usize, (cj - oj.signum()).clamp(0, self.rows as isize) as usize);
        [2.0 * p[0] - q[0], 2.0 * p[1] - q[1]]
    }

    /// Where source parameter `(u, v)` (0..=1 across the rect) lands.
    pub fn evaluate(&self, u: f64, v: f64) -> Point {
        let (gx, gy) = (u.clamp(0.0, 1.0) * self.cols as f64, v.clamp(0.0, 1.0) * self.rows as f64);
        let (i, j) = ((gx.floor() as isize).min(self.cols as isize - 1), (gy.floor() as isize).min(self.rows as isize - 1));
        let (tx, ty) = (gx - i as f64, gy - j as f64);
        let (wx, wy) = (catmull_rom(tx), catmull_rom(ty));
        let mut out = [0.0; 2];
        for (b, wyb) in wy.iter().enumerate() {
            for (a, wxa) in wx.iter().enumerate() {
                let p = self.grid(i + a as isize - 1, j + b as isize - 1);
                out[0] += p[0] * wxa * wyb;
                out[1] += p[1] * wxa * wyb;
            }
        }
        out
    }

    /// The surface sampled `subdivisions` times per cell side.
    pub fn tessellate(&self, subdivisions: usize) -> WarpMesh {
        let (nx, ny) = (self.cols * subdivisions.max(1), self.rows * subdivisions.max(1));
        let [x, y, w, h] = self.rect;
        let mut mesh = WarpMesh { positions: Vec::new(), uvs: Vec::new(), indices: Vec::new() };
        for j in 0..=ny {
            for i in 0..=nx {
                let (u, v) = (i as f64 / nx as f64, j as f64 / ny as f64);
                mesh.positions.push(self.evaluate(u, v));
                mesh.uvs.push([x + u * w, y + v * h]);
            }
        }
        let row = (nx + 1) as u32;
        for j in 0..ny as u32 {
            for i in 0..nx as u32 {
                let k = j * row + i;
                mesh.indices.extend([k, k + 1, k + row, k + 1, k + row + 1, k + row]);
            }
        }
        mesh
    }
}

/// Catmull-Rom weights for the 4 points around `t` in 0..=1 (interpolating:
/// the curve passes through the control points).
fn catmull_rom(t: f64) -> [f64; 4] {
    let (t2, t3) = (t * t, t * t * t);
    [
        0.5 * (-t3 + 2.0 * t2 - t),
        0.5 * (3.0 * t3 - 5.0 * t2 + 2.0),
        0.5 * (-3.0 * t3 + 4.0 * t2 + t),
        0.5 * (t3 - t2),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Point, b: Point) -> bool {
        (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9
    }

    #[test]
    fn an_untouched_grid_is_the_identity() {
        let warp = MeshWarp::new([10.0, 20.0, 100.0, 50.0], 3, 2);
        assert_eq!(warp.points.len(), 4 * 3);
        for (u, v) in [(0.0, 0.0), (0.3, 0.7), (1.0, 1.0), (0.5, 0.5)] {
            assert!(close(warp.evaluate(u, v), [10.0 + 100.0 * u, 20.0 + 50.0 * v]), "{u} {v}");
        }
    }

    #[test]
    fn the_surface_passes_through_moved_points_and_stays_smooth() {
        let mut warp = MeshWarp::new([0.0, 0.0, 90.0, 90.0], 3, 3);
        *warp.point_mut(1, 1) = [40.0, 20.0];
        assert!(close(warp.evaluate(1.0 / 3.0, 1.0 / 3.0), [40.0, 20.0]), "interpolating");
        assert!(close(warp.evaluate(1.0, 1.0), [90.0, 90.0]), "far corner unmoved");
        // Neighbouring samples move continuously.
        let a = warp.evaluate(0.30, 0.33);
        let b = warp.evaluate(0.31, 0.33);
        assert!(((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt() < 2.0);
        let mesh = warp.tessellate(4);
        assert_eq!(mesh.positions.len(), 13 * 13);
        assert_eq!(mesh.indices.len(), 12 * 12 * 6);
        assert_eq!(mesh.uvs[0], [0.0, 0.0]);
    }
}
