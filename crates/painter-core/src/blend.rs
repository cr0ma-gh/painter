//! Layer blend modes using the W3C compositing formulas on premultiplied RGBA.
//!
//! `painter-render/src/shaders/composite.wgsl` implements the same math on the
//! GPU; keep the two in sync. Blending happens on sRGB-encoded values, which is
//! what digital painters expect from these modes.

use serde::{Deserialize, Serialize};

pub type Rgba = [f32; 4];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[repr(u32)]
pub enum BlendMode {
    #[default]
    Normal = 0,
    Darken = 1,
    Multiply = 2,
    ColorBurn = 3,
    LinearBurn = 4,
    Lighten = 5,
    Screen = 6,
    ColorDodge = 7,
    Add = 8,
    Overlay = 9,
    SoftLight = 10,
    HardLight = 11,
    Difference = 12,
}

impl BlendMode {
    pub const ALL: [BlendMode; 13] = [
        BlendMode::Normal,
        BlendMode::Darken,
        BlendMode::Multiply,
        BlendMode::ColorBurn,
        BlendMode::LinearBurn,
        BlendMode::Lighten,
        BlendMode::Screen,
        BlendMode::ColorDodge,
        BlendMode::Add,
        BlendMode::Overlay,
        BlendMode::SoftLight,
        BlendMode::HardLight,
        BlendMode::Difference,
    ];

    pub fn name(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Darken => "Darken",
            BlendMode::Multiply => "Multiply",
            BlendMode::ColorBurn => "Color Burn",
            BlendMode::LinearBurn => "Linear Burn",
            BlendMode::Lighten => "Lighten",
            BlendMode::Screen => "Screen",
            BlendMode::ColorDodge => "Color Dodge",
            BlendMode::Add => "Add (Linear Dodge)",
            BlendMode::Overlay => "Overlay",
            BlendMode::SoftLight => "Soft Light",
            BlendMode::HardLight => "Hard Light",
            BlendMode::Difference => "Difference",
        }
    }

    /// Mode id understood by the WGSL compositor.
    pub fn shader_id(self) -> u32 {
        self as u32
    }

    /// Blend one straight-alpha channel: `cb` backdrop, `cs` source.
    pub fn channel(self, cb: f32, cs: f32) -> f32 {
        match self {
            BlendMode::Normal => cs,
            BlendMode::Darken => cb.min(cs),
            BlendMode::Multiply => cb * cs,
            BlendMode::ColorBurn => {
                if cb >= 1.0 {
                    1.0
                } else if cs <= 0.0 {
                    0.0
                } else {
                    1.0 - ((1.0 - cb) / cs).min(1.0)
                }
            }
            BlendMode::LinearBurn => (cb + cs - 1.0).max(0.0),
            BlendMode::Lighten => cb.max(cs),
            BlendMode::Screen => screen(cb, cs),
            BlendMode::ColorDodge => {
                if cb <= 0.0 {
                    0.0
                } else if cs >= 1.0 {
                    1.0
                } else {
                    (cb / (1.0 - cs)).min(1.0)
                }
            }
            BlendMode::Add => (cb + cs).min(1.0),
            BlendMode::Overlay => hard_light(cs, cb),
            BlendMode::SoftLight => soft_light(cb, cs),
            BlendMode::HardLight => hard_light(cb, cs),
            BlendMode::Difference => (cb - cs).abs(),
        }
    }
}

fn screen(cb: f32, cs: f32) -> f32 {
    cb + cs - cb * cs
}

fn hard_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        cb * 2.0 * cs
    } else {
        screen(cb, 2.0 * cs - 1.0)
    }
}

fn soft_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
    } else {
        let d = if cb <= 0.25 { ((16.0 * cb - 12.0) * cb + 4.0) * cb } else { cb.sqrt() };
        cb + (2.0 * cs - 1.0) * (d - cb)
    }
}

/// Composites premultiplied `s` onto premultiplied `b`.
pub fn composite(b: Rgba, s: Rgba, mode: BlendMode) -> Rgba {
    let (sa, ba) = (s[3], b[3]);
    if sa <= 0.0 {
        return b;
    }
    let mut out = [0.0; 4];
    for i in 0..3 {
        let cs = (s[i] / sa).min(1.0);
        let cb = if ba > 0.0 { (b[i] / ba).min(1.0) } else { 0.0 };
        out[i] = s[i] * (1.0 - ba) + b[i] * (1.0 - sa) + sa * ba * mode.channel(cb, cs);
    }
    out[3] = sa + ba * (1.0 - sa);
    out
}

pub fn to_f32(px: [u8; 4]) -> Rgba {
    px.map(|c| c as f32 / 255.0)
}

/// Quantises premultiplied colour, keeping the `rgb <= a` invariant.
pub fn to_u8(c: Rgba) -> [u8; 4] {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let a = q(c[3]);
    [q(c[0]).min(a), q(c[1]).min(a), q(c[2]).min(a), a]
}

pub fn premultiply(px: [u8; 4]) -> [u8; 4] {
    let a = px[3] as u32;
    let m = |c: u8| ((c as u32 * a + 127) / 255) as u8;
    [m(px[0]), m(px[1]), m(px[2]), px[3]]
}

pub fn unpremultiply(px: [u8; 4]) -> [u8; 4] {
    let a = px[3] as u32;
    if a == 0 {
        return [0; 4];
    }
    let u = |c: u8| ((c as u32 * 255 + a / 2) / a).min(255) as u8;
    [u(px[0]), u(px[1]), u(px[2]), px[3]]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: Rgba, b: Rgba) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn normal_opaque_source_replaces() {
        let out = composite([0.2, 0.3, 0.4, 1.0], [1.0, 0.0, 0.0, 1.0], BlendMode::Normal);
        assert!(approx(out, [1.0, 0.0, 0.0, 1.0]));
    }

    #[test]
    fn identity_elements() {
        let src = [0.25, 0.5, 0.75, 1.0];
        assert!(approx(composite([1.0; 4], src, BlendMode::Multiply), src));
        assert!(approx(composite([0.0, 0.0, 0.0, 1.0], src, BlendMode::Screen), src));
        assert!(approx(composite([0.0, 0.0, 0.0, 1.0], src, BlendMode::Add), src));
    }

    #[test]
    fn empty_backdrop_shows_source_for_any_mode() {
        let src = [0.1, 0.2, 0.3, 0.5];
        for mode in BlendMode::ALL {
            assert!(approx(composite([0.0; 4], src, mode), src), "{mode:?}");
        }
    }

    #[test]
    fn premultiply_round_trip() {
        let px = [200, 100, 50, 128];
        let back = unpremultiply(premultiply(px));
        for i in 0..3 {
            assert!((back[i] as i32 - px[i] as i32).abs() <= 2);
        }
    }
}
