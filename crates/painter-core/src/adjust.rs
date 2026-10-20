//! Colour corrections: what adjustment (correction) layers do to everything
//! below them, non-destructively.
//!
//! Every adjustment compiles to one [`AdjustOp`]: a per-channel lookup table,
//! a lookup by luminance (gradient map, threshold), or a hue / saturation /
//! lightness shift. The CPU compositor (`io::flatten`) and the GPU one
//! (painter-render's `composite.wgsl`) evaluate the same op on the
//! unpremultiplied backdrop, then mix it in by opacity × mask × clipping.

use serde::{Deserialize, Serialize};

use crate::brush::PressureCurve;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Adjustment {
    /// Both −1..1.
    BrightnessContrast { brightness: f32, contrast: f32 },
    /// Input black / white points and gamma, output black / white (0..1).
    Levels { input_black: f32, input_white: f32, gamma: f32, output_black: f32, output_white: f32 },
    /// One curve for R, G and B together.
    ToneCurve { curve: PressureCurve },
    /// Hue in degrees (−180..180); saturation and lightness −1..1.
    HueSaturation { hue: f32, saturation: f32, lightness: f32 },
    /// Cyan–red, magenta–green, yellow–blue shifts (−1..1) for shadows,
    /// midtones and highlights.
    ColorBalance { shadows: [f32; 3], midtones: [f32; 3], highlights: [f32; 3] },
    /// Tones per channel, 2..=255.
    Posterize { levels: u32 },
    /// Pixels brighter than `level` (0..1) turn white, the rest black.
    Threshold { level: f32 },
    Invert,
    /// Luminance mapped onto a gradient between two colours.
    GradientMap { from: [u8; 3], to: [u8; 3] },
}

/// What the compositors run.
#[derive(Clone, Debug, PartialEq)]
pub enum AdjustOp {
    /// out.r = lut[r].r, out.g = lut[g].g, out.b = lut[b].b (indices 0..=255).
    Channels(Box<[[u8; 4]; 256]>),
    /// out = lut[luminance].rgb.
    Luma(Box<[[u8; 4]; 256]>),
    /// Hue (radians), saturation and lightness shifts.
    Hsl { hue: f32, saturation: f32, lightness: f32 },
}

impl AdjustOp {
    /// Shader id (`composite.wgsl`).
    pub fn shader_id(&self) -> u32 {
        match self {
            AdjustOp::Channels(_) => 0,
            AdjustOp::Luma(_) => 1,
            AdjustOp::Hsl { .. } => 2,
        }
    }

    /// The lookup table as a 256 × 1 RGBA8 texture, for the table ops.
    pub fn lut_bytes(&self) -> Option<Vec<u8>> {
        match self {
            AdjustOp::Channels(lut) | AdjustOp::Luma(lut) => Some(lut.iter().flatten().copied().collect()),
            AdjustOp::Hsl { .. } => None,
        }
    }

    /// Applies the op to a premultiplied RGBA8 pixel; returns the straight
    /// result. Table indices come from integer maths, so the GPU (which does
    /// the same) lands on exactly the same entry, even for step-like tables.
    pub fn apply_pixel(&self, px: [u8; 4]) -> [f32; 3] {
        let a = px[3] as u32;
        let straight = |c: u8| ((c as u32 * 255 + a / 2) / a.max(1)).min(255) as usize;
        let s = [straight(px[0]), straight(px[1]), straight(px[2])];
        match self {
            AdjustOp::Channels(lut) => [lut[s[0]][0] as f32 / 255.0, lut[s[1]][1] as f32 / 255.0, lut[s[2]][2] as f32 / 255.0],
            AdjustOp::Luma(lut) => {
                let l = (299 * s[0] + 587 * s[1] + 114 * s[2] + 500) / 1000;
                let e = lut[l.min(255)];
                [e[0] as f32 / 255.0, e[1] as f32 / 255.0, e[2] as f32 / 255.0]
            }
            AdjustOp::Hsl { .. } => {
                let f = px[3] as f32 / 255.0;
                let c = |v: u8| (v as f32 / 255.0 / f).min(1.0);
                self.apply([c(px[0]), c(px[1]), c(px[2])])
            }
        }
    }

    /// Applies the op to a straight (unpremultiplied) colour, 0..1.
    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        let index = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as usize;
        match self {
            AdjustOp::Channels(lut) => {
                [lut[index(c[0])][0] as f32 / 255.0, lut[index(c[1])][1] as f32 / 255.0, lut[index(c[2])][2] as f32 / 255.0]
            }
            AdjustOp::Luma(lut) => {
                let e = lut[index(luma(c))];
                [e[0] as f32 / 255.0, e[1] as f32 / 255.0, e[2] as f32 / 255.0]
            }
            AdjustOp::Hsl { hue, saturation, lightness } => hsl_shift(c, *hue, *saturation, *lightness),
        }
    }
}

/// Rec. 601 luminance (also in `composite.wgsl`).
pub fn luma(c: [f32; 3]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

fn table(f: impl Fn(usize, f32) -> f32) -> Box<[[u8; 4]; 256]> {
    let mut lut = Box::new([[0u8; 4]; 256]);
    for (i, e) in lut.iter_mut().enumerate() {
        let v = i as f32 / 255.0;
        for (ch, out) in e.iter_mut().take(3).enumerate() {
            *out = (f(ch, v).clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        e[3] = 255;
    }
    lut
}

impl Adjustment {
    pub fn name(&self) -> &'static str {
        match self {
            Adjustment::BrightnessContrast { .. } => "Brightness / Contrast",
            Adjustment::Levels { .. } => "Levels",
            Adjustment::ToneCurve { .. } => "Tone Curve",
            Adjustment::HueSaturation { .. } => "Hue / Saturation / Lightness",
            Adjustment::ColorBalance { .. } => "Color Balance",
            Adjustment::Posterize { .. } => "Posterize",
            Adjustment::Threshold { .. } => "Threshold",
            Adjustment::Invert => "Invert",
            Adjustment::GradientMap { .. } => "Gradient Map",
        }
    }

    /// Each kind with neutral or typical settings, for "New Correction Layer".
    pub fn defaults() -> Vec<Adjustment> {
        vec![
            Adjustment::BrightnessContrast { brightness: 0.0, contrast: 0.0 },
            Adjustment::Levels { input_black: 0.0, input_white: 1.0, gamma: 1.0, output_black: 0.0, output_white: 1.0 },
            Adjustment::ToneCurve { curve: PressureCurve::linear() },
            Adjustment::HueSaturation { hue: 0.0, saturation: 0.0, lightness: 0.0 },
            Adjustment::ColorBalance { shadows: [0.0; 3], midtones: [0.0; 3], highlights: [0.0; 3] },
            Adjustment::Posterize { levels: 4 },
            Adjustment::Threshold { level: 0.5 },
            Adjustment::Invert,
            Adjustment::GradientMap { from: [20, 16, 40], to: [255, 236, 200] },
        ]
    }

    pub fn op(&self) -> AdjustOp {
        match self {
            Adjustment::BrightnessContrast { brightness, contrast } => {
                // Contrast pivots on mid grey; ±1 is a steep S or a flat grey.
                let c = contrast.clamp(-1.0, 0.999);
                let factor = if c >= 0.0 { 1.0 / (1.0 - c) } else { 1.0 + c };
                AdjustOp::Channels(table(|_, v| (v + brightness * 0.5 - 0.5) * factor + 0.5))
            }
            Adjustment::Levels { input_black, input_white, gamma, output_black, output_white } => {
                let span = (input_white - input_black).max(1e-3);
                let g = gamma.clamp(0.1, 10.0);
                AdjustOp::Channels(table(|_, v| {
                    let t = ((v - input_black) / span).clamp(0.0, 1.0).powf(1.0 / g);
                    output_black + t * (output_white - output_black)
                }))
            }
            Adjustment::ToneCurve { curve } => AdjustOp::Channels(table(|_, v| curve.eval(v))),
            Adjustment::HueSaturation { hue, saturation, lightness } => {
                AdjustOp::Hsl { hue: hue.to_radians(), saturation: *saturation, lightness: *lightness }
            }
            Adjustment::ColorBalance { shadows, midtones, highlights } => {
                // Each tone range weighted by how much of the value falls in it.
                AdjustOp::Channels(table(|ch, v| {
                    let w_shadow = (1.0 - v * 2.0).clamp(0.0, 1.0);
                    let w_high = (v * 2.0 - 1.0).clamp(0.0, 1.0);
                    let w_mid = 1.0 - w_shadow - w_high;
                    v + 0.25 * (shadows[ch] * w_shadow + midtones[ch] * w_mid + highlights[ch] * w_high)
                }))
            }
            Adjustment::Posterize { levels } => {
                let n = (*levels).clamp(2, 255) as f32 - 1.0;
                AdjustOp::Channels(table(|_, v| (v * n).round() / n))
            }
            Adjustment::Threshold { level } => {
                AdjustOp::Luma(table(|_, v| if v >= *level { 1.0 } else { 0.0 }))
            }
            Adjustment::Invert => AdjustOp::Channels(table(|_, v| 1.0 - v)),
            Adjustment::GradientMap { from, to } => {
                AdjustOp::Luma(table(|ch, v| (from[ch] as f32 + (to[ch] as f32 - from[ch] as f32) * v) / 255.0))
            }
        }
    }
}

/// Hue / saturation / lightness shift (mirrored by `hsl_shift` in `composite.wgsl`).
pub fn hsl_shift(c: [f32; 3], hue: f32, saturation: f32, lightness: f32) -> [f32; 3] {
    let [h, s, l] = rgb_to_hsl(c);
    let h = (h + hue / std::f32::consts::TAU).rem_euclid(1.0);
    // Scaled, so greys stay grey.
    let s = s * (1.0 + saturation);
    let l = if lightness >= 0.0 { l + (1.0 - l) * lightness } else { l * (1.0 + lightness) };
    hsl_to_rgb([h, s.clamp(0.0, 1.0), l.clamp(0.0, 1.0)])
}

pub fn rgb_to_hsl([r, g, b]: [f32; 3]) -> [f32; 3] {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;
    let d = max - min;
    if d <= 1e-6 {
        return [0.0, 0.0, l];
    }
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    [h / 6.0, s, l]
}

pub fn hsl_to_rgb([h, s, l]: [f32; 3]) -> [f32; 3] {
    if s <= 1e-6 {
        return [l, l, l];
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let channel = |t: f32| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() <= tol)
    }

    #[test]
    fn neutral_settings_change_nothing() {
        let grey = [0.3, 0.55, 0.8];
        for a in Adjustment::defaults() {
            let neutral = !matches!(a, Adjustment::Posterize { .. } | Adjustment::Threshold { .. } | Adjustment::Invert | Adjustment::GradientMap { .. });
            if neutral {
                assert!(close(a.op().apply(grey), grey, 1.0 / 255.0), "{}", a.name());
            }
        }
    }

    #[test]
    fn each_kind_does_its_job() {
        let op = |a: Adjustment| a.op();
        assert!(close(op(Adjustment::Invert).apply([0.2, 0.5, 1.0]), [0.8, 0.5, 0.0], 0.005));
        let t = op(Adjustment::Threshold { level: 0.5 });
        assert_eq!(t.apply([0.9, 0.9, 0.9]), [1.0, 1.0, 1.0]);
        assert_eq!(t.apply([0.1, 0.2, 0.3]), [0.0, 0.0, 0.0]);
        let p = op(Adjustment::Posterize { levels: 2 });
        assert_eq!(p.apply([0.2, 0.7, 0.49]), [0.0, 1.0, 0.0]);
        let b = op(Adjustment::BrightnessContrast { brightness: 0.4, contrast: 0.0 });
        assert!(b.apply([0.5; 3])[0] > 0.65);
        let c = op(Adjustment::BrightnessContrast { brightness: 0.0, contrast: 0.5 });
        assert!(c.apply([0.25; 3])[0] < 0.1 && c.apply([0.75; 3])[0] > 0.9, "contrast spreads around grey");
        let l = op(Adjustment::Levels { input_black: 0.2, input_white: 0.8, gamma: 1.0, output_black: 0.0, output_white: 1.0 });
        assert!(close(l.apply([0.2, 0.5, 0.8]), [0.0, 0.5, 1.0], 0.005));
        // Hue +120°: red becomes green.
        let h = op(Adjustment::HueSaturation { hue: 120.0, saturation: 0.0, lightness: 0.0 });
        assert!(close(h.apply([1.0, 0.0, 0.0]), [0.0, 1.0, 0.0], 1e-4));
        let gray = op(Adjustment::HueSaturation { hue: 0.0, saturation: -1.0, lightness: 0.0 }).apply([0.9, 0.1, 0.1]);
        assert!((gray[0] - gray[1]).abs() < 1e-4, "desaturated");
        let g = op(Adjustment::GradientMap { from: [0, 0, 0], to: [255, 0, 0] });
        assert!(close(g.apply([1.0, 1.0, 1.0]), [1.0, 0.0, 0.0], 0.005));
        let cb = op(Adjustment::ColorBalance { shadows: [0.0; 3], midtones: [1.0, 0.0, -1.0], highlights: [0.0; 3] });
        let out = cb.apply([0.5; 3]);
        assert!(out[0] > 0.7 && out[2] < 0.3, "midtones warmed");
    }

    #[test]
    fn hsl_round_trips() {
        for c in [[0.1, 0.5, 0.9], [1.0, 0.2, 0.2], [0.3, 0.3, 0.3], [0.0, 1.0, 0.5]] {
            assert!(close(hsl_to_rgb(rgb_to_hsl(c)), c, 1e-5), "{c:?}");
        }
    }

    #[test]
    fn luts_upload_as_256_texels() {
        let op = Adjustment::Invert.op();
        let bytes = op.lut_bytes().unwrap();
        assert_eq!(bytes.len(), 256 * 4);
        assert_eq!(&bytes[..4], &[255, 255, 255, 255]);
        assert!(Adjustment::HueSaturation { hue: 0.0, saturation: 0.0, lightness: 0.0 }.op().lut_bytes().is_none());
    }
}
