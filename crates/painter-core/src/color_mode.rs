//! Image colour modes, as Photoshop's Image → Mode, all 8 bits per channel:
//! Bitmap, Grayscale, Duotone, Indexed Color, RGB, CMYK, Lab and Multichannel.
//!
//! Pixels are always RGB; the mode decides the colours they may take, the
//! channels, how the canvas shows them and how files are written.
//! - Grayscale turns every layer's colours to their luminance and keeps later
//!   edits grey; Duotone does the same and prints the grey with its inks
//!   ([`crate::duotone`]).
//! - Bitmap flattens the image into one black-and-white layer (50 %
//!   threshold, error diffusion or a halftone screen) and keeps later edits
//!   black or white; Indexed Color flattens it into its colour table
//!   ([`crate::indexed`]); Multichannel flattens it into cyan, magenta and
//!   yellow spot channels.
//! - CMYK keeps colours printable ([`crate::cmyk`]); Lab has the L, a and b
//!   channels ([`crate::lab`]).
//!
//! Back to RGB, the pixels stay as they are (Duotone's inked colours are
//! laid in) and colour can be used again.

use serde::{Deserialize, Serialize};

use crate::blend::{premultiply, unpremultiply};
use crate::layer::{Layer, LayerKind};
use crate::tile::{TileCoord, TileStore};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorMode {
    #[default]
    Rgb,
    Grayscale,
    Bitmap,
    /// Colours kept printable with a CMYK press profile (`Document::cmyk`).
    Cmyk,
    /// Grey printed with the inks of `Document::duotone`.
    Duotone,
    /// The colours of `Document::palette` only.
    Indexed,
    Lab,
    /// Cyan, magenta and yellow spot channels.
    Multichannel,
}

impl ColorMode {
    /// In Photoshop's menu order.
    pub const ALL: [ColorMode; 8] = [
        ColorMode::Bitmap,
        ColorMode::Grayscale,
        ColorMode::Duotone,
        ColorMode::Indexed,
        ColorMode::Rgb,
        ColorMode::Cmyk,
        ColorMode::Lab,
        ColorMode::Multichannel,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ColorMode::Rgb => "RGB Color",
            ColorMode::Cmyk => "CMYK Color",
            ColorMode::Grayscale => "Grayscale",
            ColorMode::Bitmap => "Bitmap",
            ColorMode::Duotone => "Duotone",
            ColorMode::Indexed => "Indexed Color",
            ColorMode::Lab => "Lab Color",
            ColorMode::Multichannel => "Multichannel",
        }
    }

    /// Short form for the status bar.
    pub fn short(self) -> &'static str {
        match self {
            ColorMode::Rgb => "RGB/8",
            ColorMode::Cmyk => "CMYK/8",
            ColorMode::Grayscale => "Gray/8",
            ColorMode::Bitmap => "Bitmap",
            ColorMode::Duotone => "Duotone/8",
            ColorMode::Indexed => "Index",
            ColorMode::Lab => "Lab/8",
            ColorMode::Multichannel => "Multichannel/8",
        }
    }

    /// Whether the document's colours are greys (Grayscale, Duotone, Bitmap).
    pub fn is_gray(self) -> bool {
        matches!(self, ColorMode::Grayscale | ColorMode::Duotone | ColorMode::Bitmap)
    }

    /// Whether converting to it merges the layers into one (as in Photoshop).
    pub fn flattens(self) -> bool {
        matches!(self, ColorMode::Bitmap | ColorMode::Indexed | ColorMode::Multichannel)
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
        // CMYK: made printable by the app's Little CMS proofer; Indexed: the
        // nearest table colour, picked by the app.
        ColorMode::Rgb | ColorMode::Cmyk | ColorMode::Lab | ColorMode::Multichannel | ColorMode::Indexed => rgb,
        ColorMode::Grayscale | ColorMode::Duotone => [luma(c) as f32 / 255.0; 3],
        ColorMode::Bitmap => [if luma(c) >= 128 { 1.0 } else { 0.0 }; 3],
    }
}

/// Every colour in a layer tree through `f` (straight RGB): pixels, vector
/// lines (re-drawn), fills, gradients and tones. Adjustment layers stay.
pub fn map_layer_colors(layers: &mut [Layer], canvas: (u32, u32), f: &dyn Fn([u8; 3]) -> [u8; 3]) {
    for layer in layers {
        match &mut layer.kind {
            LayerKind::Raster(tiles) => {
                let coords: Vec<TileCoord> = tiles.iter().map(|(c, _)| c).collect();
                for coord in coords {
                    let Some(mut tile) = tiles.get(coord).map(|t| (**t).clone()) else { continue };
                    for px in tile.bytes_mut().as_chunks_mut::<4>().0 {
                        if px[3] > 0 {
                            let s = unpremultiply(*px);
                            let [r, g, b] = f([s[0], s[1], s[2]]);
                            *px = premultiply([r, g, b, px[3]]);
                        }
                    }
                    tiles.insert(coord, tile);
                }
            }
            LayerKind::Vector(v) => {
                for s in &mut v.strokes {
                    let c = f(s.color.map(|x| (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u8));
                    s.color = c.map(|x| x as f32 / 255.0);
                }
                for t in &mut v.texts {
                    t.color = f(t.color);
                    t.outline_color = f(t.outline_color);
                    if let Some(b) = &mut t.balloon {
                        b.line = f(b.line);
                        let [r, g, bl] = f([b.fill[0], b.fill[1], b.fill[2]]);
                        b.fill = [r, g, bl, b.fill[3]];
                    }
                }
                crate::vector::rasterize(v, canvas);
            }
            LayerKind::Folder(fo) => map_layer_colors(&mut fo.children, canvas, f),
            LayerKind::Fill { color } => *color = f(*color),
            LayerKind::Gradient(g) => {
                for c in [&mut g.from, &mut g.to] {
                    let [r, gg, b] = f([c[0], c[1], c[2]]);
                    *c = [r, gg, b, c[3]];
                }
            }
            LayerKind::Tone(t) => t.color = f(t.color),
            LayerKind::Adjustment(_) => {}
        }
    }
}

/// Duotone's inked colours laid into the layers: the document becomes RGB
/// looking the same (exports, and leaving Duotone for a colour mode).
pub fn ink_duotone(doc: &mut crate::Document) {
    if doc.mode != ColorMode::Duotone {
        return;
    }
    if let Some(d) = doc.duotone.clone() {
        let table = d.table();
        let canvas = doc.canvas();
        map_layer_colors(&mut doc.layers, canvas, &|c| table[luma(c) as usize]);
    }
    doc.mode = ColorMode::Rgb;
    doc.profile = crate::color_profile::ColorProfile::Srgb;
}

/// How the canvas shows a document whose look isn't its pixels, as a
/// [`crate::cmyk::LUT_SIZE`]³ table from RGB to the RGB on screen (red
/// fastest, then green, then blue; RGBA bytes):
/// - Duotone: the grey through its inks;
/// - Lab with only some of L, a, b shown: one alone as grey, two with the
///   third neutral;
/// - Multichannel with only some inks shown: one alone as grey, several
///   printed over each other.
///
/// `None` when the pixels show as they are.
pub fn display_lut(doc: &crate::Document, shown: [bool; 4]) -> Option<Vec<u8>> {
    let n = crate::cmyk::LUT_SIZE;
    let three = [shown[0], shown[1], shown[2]];
    let all = three == [true; 3];
    let duotone = doc.duotone.as_ref().map(|d| d.table());
    let f: Box<dyn Fn([u8; 3]) -> [u8; 3]> = match doc.mode {
        ColorMode::Duotone => {
            let table = duotone?;
            Box::new(move |c| table[luma(c) as usize])
        }
        ColorMode::Lab if !all => Box::new(move |c| {
            let lab = crate::lab::from_rgb(c);
            let alone: Vec<usize> = (0..3).filter(|&k| three[k]).collect();
            if let [k] = alone[..] {
                [crate::lab::channel_byte(lab, k); 3]
            } else {
                let neutral = [50.0, 0.0, 0.0];
                crate::lab::to_rgb([0, 1, 2].map(|k| if three[k] { lab[k] } else { neutral[k] }))
            }
        }),
        ColorMode::Multichannel if !all => Box::new(move |c| {
            let alone: Vec<usize> = (0..3).filter(|&k| three[k]).collect();
            if let [k] = alone[..] {
                [c[k]; 3]
            } else {
                [0, 1, 2].map(|k| if three[k] { c[k] } else { 255 })
            }
        }),
        _ => return None,
    };
    let step = |i: usize| ((i * 255) as f32 / (n - 1) as f32).round() as u8;
    Some(
        (0..n * n * n)
            .flat_map(|i| {
                let [r, g, b] = f([step(i % n), step(i / n % n), step(i / (n * n))]);
                [r, g, b, 255]
            })
            .collect(),
    )
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
    }

    #[test]
    fn duotone_and_channel_views_and_inking() {
        let mut doc = crate::Document::new(64, 64);
        let n = crate::cmyk::LUT_SIZE;
        let at = |lut: &[u8], r: usize, g: usize, b: usize| {
            let i = (b * n * n + g * n + r) * 4;
            [lut[i], lut[i + 1], lut[i + 2]]
        };
        assert!(display_lut(&doc, [true; 4]).is_none(), "RGB shows its pixels");
        doc.mode = ColorMode::Duotone;
        doc.duotone = Some(crate::duotone::Duotone::default());
        let lut = display_lut(&doc, [true; 4]).unwrap();
        assert_eq!(lut.len(), n * n * n * 4);
        assert_eq!(at(&lut, n - 1, n - 1, n - 1), [255; 3], "white paper stays white");
        let mid = at(&lut, 16, 16, 16);
        assert!(mid[0] > mid[2], "grey prints brownish: {mid:?}");
        // Lab, lightness alone: grey from L.
        doc.mode = ColorMode::Lab;
        let lut = display_lut(&doc, [true, false, false, true]).unwrap();
        assert_eq!(at(&lut, n - 1, 0, 0), [crate::lab::channel_byte(crate::lab::from_rgb([255, 0, 0]), 0); 3]);
        // Multichannel, cyan alone: the red values as grey.
        doc.mode = ColorMode::Multichannel;
        let lut = display_lut(&doc, [true, false, false, true]).unwrap();
        assert_eq!(at(&lut, n - 1, 0, 0), [255; 3]);
        // Duotone into RGB: a grey pixel takes its inked colour.
        doc.mode = ColorMode::Duotone;
        let id = doc.alloc_id();
        let mut layer = Layer::raster(id, "grey");
        layer.kind = LayerKind::Raster(TileStore::from_rgba(64, 64, &[150, 150, 150, 255].repeat(64 * 64)));
        doc.layers = vec![layer];
        ink_duotone(&mut doc);
        assert_eq!(doc.mode, ColorMode::Rgb);
        let p = doc.layers[0].tiles().unwrap().pixel(3, 3);
        let want = crate::duotone::Duotone::default().render(150);
        assert_eq!([p[0], p[1], p[2]], want);
    }

    #[test]
    fn modes_in_photoshops_order() {
        assert_eq!(ColorMode::ALL.map(|m| m.name())[..4], ["Bitmap", "Grayscale", "Duotone", "Indexed Color"]);
        assert!(ColorMode::Indexed.flattens() && !ColorMode::Lab.flattens() && ColorMode::Duotone.is_gray());
        let t = to_bitmap(&[128u8, 128, 128, 255].repeat(4), 2, 2, BitmapMethod::Threshold);
        assert!(t.as_chunks::<4>().0.iter().all(|p| p[0] == 255));
        assert_eq!(mode_color(ColorMode::Bitmap, [0.2, 0.3, 0.1]), [0.0; 3]);
    }
}
