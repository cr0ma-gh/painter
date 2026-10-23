//! Image colour modes, as Photoshop's Image → Mode: RGB, Grayscale and
//! Bitmap (pure black and white), all 8 bits per channel.
//!
//! Grayscale turns every layer's colours to their luminance and keeps later
//! edits grey; Bitmap flattens the image into one black-and-white layer (50 %
//! threshold, error diffusion or a halftone screen) and keeps later edits
//! black or white. Back to RGB, the pixels stay as they are and colour can
//! be used again.

use serde::{Deserialize, Serialize};

use crate::blend::unpremultiply;
use crate::layer::{Layer, LayerKind};
use crate::tile::{TileCoord, TileStore};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorMode {
    #[default]
    Rgb,
    Grayscale,
    Bitmap,
}

impl ColorMode {
    pub const ALL: [ColorMode; 3] = [ColorMode::Rgb, ColorMode::Grayscale, ColorMode::Bitmap];

    pub fn name(self) -> &'static str {
        match self {
            ColorMode::Rgb => "RGB Color",
            ColorMode::Grayscale => "Grayscale",
            ColorMode::Bitmap => "Bitmap",
        }
    }

    /// Short form for the status bar.
    pub fn short(self) -> &'static str {
        match self {
            ColorMode::Rgb => "RGB/8",
            ColorMode::Grayscale => "Gray/8",
            ColorMode::Bitmap => "Bitmap",
        }
    }
}

/// How Bitmap turns greys into black and white.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BitmapMethod {
    /// Darker than mid-grey: black.
    Threshold,
    /// Floyd–Steinberg error diffusion: greys become a scatter of dots.
    Diffusion,
    /// Round dots on a screen of `cell` pixels at `angle` degrees (manga tone).
    Halftone { cell: f32, angle: f32 },
}

/// Rec. 601 luminance of a colour, 0..=255.
pub fn luma([r, g, b]: [u8; 3]) -> u8 {
    (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32 + 0.5).min(255.0) as u8
}

fn gray_px(px: [u8; 4]) -> [u8; 4] {
    // Premultiplied colour is linear in alpha, so its luminance is the premultiplied grey.
    let v = luma([px[0], px[1], px[2]]).min(px[3]);
    [v, v, v, px[3]]
}

/// Grey versions of the `coords` tiles of `tiles` (every tile with `None`).
pub fn gray_tiles(tiles: &mut TileStore, coords: Option<&[TileCoord]>) {
    let all: Vec<TileCoord>;
    let coords = match coords {
        Some(c) => c,
        None => {
            all = tiles.iter().map(|(c, _)| c).collect();
            &all
        }
    };
    for &coord in coords {
        let Some(mut tile) = tiles.get(coord).map(|t| (**t).clone()) else { continue };
        for px in tile.bytes_mut().as_chunks_mut::<4>().0 {
            *px = gray_px(*px);
        }
        tiles.insert(coord, tile);
    }
}

/// The `coords` tiles of `tiles` snapped to black, white or transparent (Bitmap edits).
pub fn threshold_tiles(tiles: &mut TileStore, coords: &[TileCoord]) {
    for &coord in coords {
        let Some(mut tile) = tiles.get(coord).map(|t| (**t).clone()) else { continue };
        for px in tile.bytes_mut().as_chunks_mut::<4>().0 {
            *px = if px[3] < 128 {
                [0; 4]
            } else {
                let s = unpremultiply(*px);
                let v = if luma([s[0], s[1], s[2]]) >= 128 { 255 } else { 0 };
                [v, v, v, 255]
            };
        }
        tiles.insert(coord, tile);
    }
}

/// Every colour in a layer tree to grey: pixels, vector lines (re-drawn),
/// fills, gradients and tones. Adjustment layers are left as they are.
pub fn gray_layers(layers: &mut [Layer], canvas: (u32, u32)) {
    let g3 = |c: [u8; 3]| {
        let v = luma(c);
        [v, v, v]
    };
    for layer in layers {
        match &mut layer.kind {
            LayerKind::Raster(tiles) => gray_tiles(tiles, None),
            LayerKind::Vector(v) => {
                for s in &mut v.strokes {
                    let c = s.color.map(|x| (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
                    let g = luma(c) as f32 / 255.0;
                    s.color = [g; 3];
                }
                crate::vector::rasterize(v, canvas);
            }
            LayerKind::Folder(f) => gray_layers(&mut f.children, canvas),
            LayerKind::Fill { color } => *color = g3(*color),
            LayerKind::Gradient(g) => {
                for c in [&mut g.from, &mut g.to] {
                    let [r, gg, b] = g3([c[0], c[1], c[2]]);
                    *c = [r, gg, b, c[3]];
                }
            }
            LayerKind::Tone(t) => t.color = g3(t.color),
            LayerKind::Adjustment(_) => {}
        }
    }
}

/// A flattened image (premultiplied RGBA8, `w` × `h`) in black and white,
/// opaque, over white.
pub fn to_bitmap(rgba: &[u8], w: usize, h: usize, method: BitmapMethod) -> Vec<u8> {
    // Grey over white, 0..=255 as f32 for error diffusion.
    let mut grey: Vec<f32> =
        rgba.as_chunks::<4>().0.iter().map(|p| luma([p[0], p[1], p[2]]) as f32 + (255 - p[3]) as f32).map(|v| v.min(255.0)).collect();
    let mut out = vec![0u8; w * h * 4];
    let mut put = |i: usize, white: bool| {
        let v = if white { 255 } else { 0 };
        out[i * 4..i * 4 + 4].copy_from_slice(&[v, v, v, 255]);
    };
    match method {
        BitmapMethod::Threshold => {
            for (i, &v) in grey.iter().enumerate() {
                put(i, v >= 128.0);
            }
        }
        BitmapMethod::Diffusion => {
            for y in 0..h {
                for x in 0..w {
                    let i = y * w + x;
                    let old = grey[i];
                    let white = old >= 128.0;
                    put(i, white);
                    let err = old - if white { 255.0 } else { 0.0 };
                    let mut spread = |dx: isize, dy: usize, k: f32| {
                        let nx = x as isize + dx;
                        if nx >= 0 && (nx as usize) < w && y + dy < h {
                            grey[(y + dy) * w + nx as usize] += err * k;
                        }
                    };
                    spread(1, 0, 7.0 / 16.0);
                    spread(-1, 1, 3.0 / 16.0);
                    spread(0, 1, 5.0 / 16.0);
                    spread(1, 1, 1.0 / 16.0);
                }
            }
        }
        BitmapMethod::Halftone { cell, angle } => {
            let cell = cell.max(2.0);
            let (s, c) = angle.to_radians().sin_cos();
            for y in 0..h {
                for x in 0..w {
                    let i = y * w + x;
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    // Position inside the rotated cell, -0.5..0.5.
                    let (u, v) = ((px * c + py * s) / cell, (-px * s + py * c) / cell);
                    let (fu, fv) = (u - u.round(), v - v.round());
                    let ink = 1.0 - grey[i] / 255.0;
                    // A dot of area `ink`: radius from π r² = ink (in cell units).
                    let r = (ink / std::f32::consts::PI).sqrt();
                    put(i, fu.hypot(fv) > r);
                }
            }
        }
    }
    out
}

/// Straight colour → the mode's colour (what the brush paints with).
pub fn mode_color(mode: ColorMode, rgb: [f32; 3]) -> [f32; 3] {
    let c = rgb.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
    match mode {
        ColorMode::Rgb => rgb,
        ColorMode::Grayscale => [luma(c) as f32 / 255.0; 3],
        ColorMode::Bitmap => [if luma(c) >= 128 { 1.0 } else { 0.0 }; 3],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grey_keeps_alpha_and_luminance() {
        let mut t = TileStore::from_rgba(64, 64, &[255, 0, 0, 255].repeat(64 * 64));
        gray_tiles(&mut t, None);
        assert_eq!(t.pixel(1, 1), [76, 76, 76, 255]);
        let mut half = TileStore::from_rgba(64, 64, &[0, 128, 0, 128].repeat(64 * 64));
        gray_tiles(&mut half, None);
        assert_eq!(half.pixel(1, 1), [75, 75, 75, 128]);
    }

    #[test]
    fn bitmap_keeps_the_average_grey() {
        // Mid-grey: diffusion and halftone give about half black.
        let (w, h) = (64, 64);
        let grey = [128u8, 128, 128, 255].repeat(w * h);
        for method in [BitmapMethod::Diffusion, BitmapMethod::Halftone { cell: 8.0, angle: 45.0 }] {
            let out = to_bitmap(&grey, w, h, method);
            let black = out.as_chunks::<4>().0.iter().filter(|p| p[0] == 0).count() as f32 / (w * h) as f32;
            assert!((black - 0.5).abs() < 0.1, "{method:?}: {black}");
            assert!(out.as_chunks::<4>().0.iter().all(|p| (p[0] == 0 || p[0] == 255) && p[3] == 255));
        }
        let t = to_bitmap(&grey, w, h, BitmapMethod::Threshold);
        assert!(t.as_chunks::<4>().0.iter().all(|p| p[0] == 255));
        assert_eq!(mode_color(ColorMode::Bitmap, [0.2, 0.3, 0.1]), [0.0; 3]);
    }
}
