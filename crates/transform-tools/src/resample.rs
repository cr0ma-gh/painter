//! Resampling through a transform: the CPU reference for commits and for the
//! GPU preview (`painter-render`'s `transform.wgsl` mirrors these formulas).
//!
//! Every output pixel centre is mapped back into the source with the inverse
//! matrix and filtered there. Images are premultiplied RGBA8 and filtered
//! premultiplied, so transparent edges don't fringe. Taps outside the source
//! read as transparent.

use crate::matrix::Mat3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Interpolation {
    /// Pixel art and aliased selections.
    Nearest,
    #[default]
    Bilinear,
    /// Catmull-Rom (Keys, a = −0.5): sharper than bilinear.
    Bicubic,
    /// 6×6 windowed sinc: sharpest for line art; may ring slightly.
    Lanczos3,
}

impl Interpolation {
    pub const ALL: [Interpolation; 4] = [Interpolation::Nearest, Interpolation::Bilinear, Interpolation::Bicubic, Interpolation::Lanczos3];

    pub fn name(self) -> &'static str {
        match self {
            Interpolation::Nearest => "Nearest neighbour",
            Interpolation::Bilinear => "Bilinear",
            Interpolation::Bicubic => "Bicubic",
            Interpolation::Lanczos3 => "Lanczos3",
        }
    }

    /// The id `transform.wgsl` switches on.
    pub fn shader_id(self) -> u32 {
        self as u32
    }
}

/// A premultiplied RGBA8 image.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Image {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, rgba: vec![0; (width * height * 4) as usize] }
    }

    fn texel(&self, x: i64, y: i64) -> [f32; 4] {
        if x < 0 || y < 0 || x >= self.width as i64 || y >= self.height as i64 {
            return [0.0; 4];
        }
        let i = ((y as usize) * self.width as usize + x as usize) * 4;
        std::array::from_fn(|c| self.rgba[i + c] as f32 / 255.0)
    }
}

/// `src` drawn through `forward` (source pixels → output pixels) into a
/// `width` × `height` image. `None` when the transform is degenerate.
pub fn resample(src: &Image, forward: &Mat3, width: u32, height: u32, filter: Interpolation) -> Option<Image> {
    let inverse = forward.inverse()?;
    // Same f32 matrix the GPU gets, so both see the same source positions.
    let m = inverse.to_f32();
    let mut out = Image::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let w = m[2][0] * px + m[2][1] * py + m[2][2];
            if w <= 0.0 {
                // Behind the vanishing line: nothing to show.
                continue;
            }
            let sx = (m[0][0] * px + m[0][1] * py + m[0][2]) / w;
            let sy = (m[1][0] * px + m[1][1] * py + m[1][2]) / w;
            let c = sample(src, sx, sy, filter);
            let i = ((y * width + x) * 4) as usize;
            // Premultiplied: colour can't exceed alpha after ringing filters.
            let a = c[3].clamp(0.0, 1.0);
            for (dst, v) in out.rgba[i..i + 3].iter_mut().zip(c) {
                *dst = (v.clamp(0.0, a) * 255.0).round() as u8;
            }
            out.rgba[i + 3] = (a * 255.0).round() as u8;
        }
    }
    Some(out)
}

/// Filtered sample at source position `(sx, sy)` (pixel centres at +0.5).
pub fn sample(src: &Image, sx: f32, sy: f32, filter: Interpolation) -> [f32; 4] {
    let (fx, fy) = (sx - 0.5, sy - 0.5);
    match filter {
        Interpolation::Nearest => src.texel(sx.floor() as i64, sy.floor() as i64),
        Interpolation::Bilinear => {
            let (x0, y0) = (fx.floor(), fy.floor());
            let (tx, ty) = (fx - x0, fy - y0);
            let (x0, y0) = (x0 as i64, y0 as i64);
            let lerp = |a: [f32; 4], b: [f32; 4], t: f32| std::array::from_fn::<f32, 4, _>(|c| a[c] + (b[c] - a[c]) * t);
            let top = lerp(src.texel(x0, y0), src.texel(x0 + 1, y0), tx);
            let bottom = lerp(src.texel(x0, y0 + 1), src.texel(x0 + 1, y0 + 1), tx);
            lerp(top, bottom, ty)
        }
        Interpolation::Bicubic => separable(src, fx, fy, 2, cubic),
        Interpolation::Lanczos3 => separable(src, fx, fy, 3, lanczos3),
    }
}

/// A separable filter with `radius` taps either side, weights normalised.
fn separable(src: &Image, fx: f32, fy: f32, radius: i64, kernel: fn(f32) -> f32) -> [f32; 4] {
    let (x0, y0) = (fx.floor() as i64, fy.floor() as i64);
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let taps = (1 - radius)..=radius;
    let wx: Vec<f32> = taps.clone().map(|k| kernel(k as f32 - tx)).collect();
    let wy: Vec<f32> = taps.clone().map(|k| kernel(k as f32 - ty)).collect();
    let mut sum = [0.0f32; 4];
    let mut total = 0.0f32;
    for (j, ky) in taps.clone().enumerate() {
        for (i, kx) in taps.clone().enumerate() {
            let w = wx[i] * wy[j];
            let t = src.texel(x0 + kx, y0 + ky);
            for c in 0..4 {
                sum[c] += t[c] * w;
            }
            total += w;
        }
    }
    if total.abs() > 1e-6 {
        sum.map(|v| v / total)
    } else {
        sum
    }
}

/// Keys cubic, a = −0.5 (Catmull-Rom).
fn cubic(x: f32) -> f32 {
    let x = x.abs();
    if x < 1.0 {
        (1.5 * x - 2.5) * x * x + 1.0
    } else if x < 2.0 {
        ((-0.5 * x + 2.5) * x - 4.0) * x + 2.0
    } else {
        0.0
    }
}

fn lanczos3(x: f32) -> f32 {
    let x = x.abs();
    if x < 1e-5 {
        return 1.0;
    }
    if x >= 3.0 {
        return 0.0;
    }
    let px = std::f32::consts::PI * x;
    3.0 * px.sin() * (px / 3.0).sin() / (px * px)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker(w: u32, h: u32) -> Image {
        let rgba = (0..w * h).flat_map(|i| if (i % w + i / w).is_multiple_of(2) { [255, 255, 255, 255] } else { [0, 0, 0, 255] }).collect();
        Image { width: w, height: h, rgba }
    }

    #[test]
    fn identity_is_exact_for_every_filter() {
        let src = checker(9, 7);
        for filter in Interpolation::ALL {
            let out = resample(&src, &Mat3::IDENTITY, 9, 7, filter).unwrap();
            assert_eq!(out, src, "{filter:?}");
        }
    }

    #[test]
    fn integer_translation_and_upscale() {
        let src = checker(4, 4);
        let out = resample(&src, &Mat3::translate(2.0, 1.0), 8, 8, Interpolation::Nearest).unwrap();
        assert_eq!(&out.rgba[(8 + 2) * 4..][..4], &src.rgba[..4]);
        assert_eq!(out.rgba[3], 0, "uncovered stays transparent");
        // 2× nearest keeps hard pixels; bilinear blends at the seams.
        let up = resample(&src, &Mat3::scale(2.0, 2.0), 8, 8, Interpolation::Nearest).unwrap();
        assert!(up.rgba.chunks(4).all(|p| p[0] == 0 || p[0] == 255));
        let smooth = resample(&src, &Mat3::scale(2.0, 2.0), 8, 8, Interpolation::Bilinear).unwrap();
        assert!(smooth.rgba.chunks(4).any(|p| p[0] > 0 && p[0] < 255));
        assert!(resample(&src, &Mat3::scale(0.0, 1.0), 4, 4, Interpolation::Bilinear).is_none());
    }

    #[test]
    fn kernels_are_interpolating() {
        assert_eq!(cubic(0.0), 1.0);
        assert_eq!(cubic(1.0), 0.0);
        assert_eq!(lanczos3(0.0), 1.0);
        assert!(lanczos3(1.0).abs() < 1e-6 && lanczos3(2.0).abs() < 1e-6);
    }
}
