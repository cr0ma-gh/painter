//! Procedural layer content: gradients and screentone.
//!
//! Evaluated per pixel centre `(x + 0.5, y + 0.5)`. `painter-render`'s
//! `composite.wgsl` evaluates the same formulas on the GPU; keep them in sync.

use std::f32::consts::PI;

use serde::{Deserialize, Serialize};

use crate::blend::Rgba;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GradientShape {
    #[default]
    Linear,
    Radial,
}

impl GradientShape {
    pub const ALL: [GradientShape; 2] = [GradientShape::Linear, GradientShape::Radial];

    pub fn name(self) -> &'static str {
        match self {
            GradientShape::Linear => "Linear",
            GradientShape::Radial => "Radial",
        }
    }

    pub fn shader_id(self) -> u32 {
        match self {
            GradientShape::Linear => 0,
            GradientShape::Radial => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Gradient {
    pub start: [f32; 2],
    pub end: [f32; 2],
    /// Straight (non-premultiplied) RGBA.
    pub from: [u8; 4],
    pub to: [u8; 4],
    pub shape: GradientShape,
}

impl Gradient {
    /// Gradient position in 0..=1 at a pixel centre.
    pub fn t(&self, x: f32, y: f32) -> f32 {
        let (dx, dy) = (self.end[0] - self.start[0], self.end[1] - self.start[1]);
        let (px, py) = (x - self.start[0], y - self.start[1]);
        let len2 = (dx * dx + dy * dy).max(1e-6);
        let t = match self.shape {
            GradientShape::Linear => (px * dx + py * dy) / len2,
            GradientShape::Radial => ((px * px + py * py) / len2).sqrt(),
        };
        t.clamp(0.0, 1.0)
    }

    /// Premultiplied colour at a pixel centre.
    pub fn sample(&self, x: f32, y: f32) -> Rgba {
        let t = self.t(x, y);
        let mix = |i: usize| (self.from[i] as f32 + (self.to[i] as f32 - self.from[i] as f32) * t) / 255.0;
        let a = mix(3);
        [mix(0) * a, mix(1) * a, mix(2) * a, a]
    }
}

/// A dot screentone covering the canvas.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tone {
    pub color: [u8; 3],
    /// Dot pitch in canvas pixels.
    pub cell: f32,
    /// Screen angle in degrees.
    pub angle: f32,
    /// Ink coverage, 0..=1.
    pub density: f32,
}

impl Default for Tone {
    fn default() -> Self {
        Self { color: [0, 0, 0], cell: 8.0, angle: 45.0, density: 0.3 }
    }
}

impl Tone {
    /// Dot coverage at a pixel centre (1 px anti-aliased edge).
    pub fn coverage(&self, x: f32, y: f32) -> f32 {
        let cell = self.cell.max(2.0);
        let (s, c) = self.angle.to_radians().sin_cos();
        let (qx, qy) = (x * c + y * s, -x * s + y * c);
        let fx = (qx / cell).rem_euclid(1.0) - 0.5;
        let fy = (qy / cell).rem_euclid(1.0) - 0.5;
        let d = fx.hypot(fy) * cell;
        let r = cell * (self.density.clamp(0.0, 1.0) / PI).sqrt();
        (r - d + 0.5).clamp(0.0, 1.0)
    }

    pub fn sample(&self, x: f32, y: f32) -> Rgba {
        let a = self.coverage(x, y);
        [self.color[0] as f32 / 255.0 * a, self.color[1] as f32 / 255.0 * a, self.color[2] as f32 / 255.0 * a, a]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_gradient_endpoints() {
        let g = Gradient {
            start: [0.0, 0.0],
            end: [10.0, 0.0],
            from: [255, 0, 0, 255],
            to: [0, 0, 255, 0],
            shape: GradientShape::Linear,
        };
        assert_eq!(g.sample(-5.0, 3.0), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(g.sample(20.0, 3.0)[3], 0.0);
        assert!((g.sample(5.0, 0.0)[3] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn tone_density_matches_average_coverage() {
        let tone = Tone { cell: 10.0, angle: 30.0, density: 0.3, ..Tone::default() };
        let n = 200;
        let sum: f32 = (0..n * n).map(|i| tone.coverage((i % n) as f32 + 0.5, (i / n) as f32 + 0.5)).sum();
        assert!((sum / (n * n) as f32 - 0.3).abs() < 0.03);
    }
}
