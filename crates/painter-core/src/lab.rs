//! CIE L*a*b* (D50, as Photoshop's Lab Color): conversions from and to
//! sRGB, and edits limited to some of the L, a, b channels.
//!
//! A Lab document keeps its pixels in sRGB; the mode decides its channels
//! (Lightness, a, b), what the colour panel shows and how PSDs are written.

use crate::blend::{premultiply, unpremultiply};
use crate::tile::{TileCoord, TileStore};

/// D50 white.
const WHITE: [f32; 3] = [0.9642, 1.0, 0.8251];

fn to_linear(v: u8) -> f32 {
    let c = v as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn from_linear(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let v = if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

/// sRGB → L (0–100), a, b (about −128–127).
pub fn from_rgb(rgb: [u8; 3]) -> [f32; 3] {
    let [r, g, b] = rgb.map(to_linear);
    // sRGB to XYZ, adapted to D50 (Bradford).
    let x = 0.436_074_7 * r + 0.385_064_9 * g + 0.143_080_4 * b;
    let y = 0.222_504_5 * r + 0.716_878_6 * g + 0.060_616_9 * b;
    let z = 0.013_932_2 * r + 0.097_104_5 * g + 0.714_173_3 * b;
    let f = |t: f32| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(x / WHITE[0]), f(y / WHITE[1]), f(z / WHITE[2]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// L, a, b → sRGB (colours outside sRGB are clipped).
pub fn to_rgb([l, a, b]: [f32; 3]) -> [u8; 3] {
    let fy = (l + 16.0) / 116.0;
    let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
    let inv = |t: f32| if t.powi(3) > 216.0 / 24389.0 { t.powi(3) } else { (116.0 * t - 16.0) * 27.0 / 24389.0 };
    let (x, y, z) = (inv(fx) * WHITE[0], inv(fy) * WHITE[1], inv(fz) * WHITE[2]);
    let r = 3.133_856 * x - 1.616_866_7 * y - 0.490_614_6 * z;
    let g = -0.978_768_4 * x + 1.916_141_5 * y + 0.033_454 * z;
    let bl = 0.071_945_3 * x - 0.228_991_4 * y + 1.405_242_7 * z;
    [from_linear(r), from_linear(g), from_linear(bl)]
}

/// A channel as Photoshop stores it, 0–255: L × 2.55, a and b + 128.
pub fn channel_byte(lab: [f32; 3], c: usize) -> u8 {
    let v = if c == 0 { lab[0] * 2.55 } else { lab[c.min(2)] + 128.0 };
    v.round().clamp(0.0, 255.0) as u8
}

/// After an edit: in the pixels that changed, only the channels in `edit`
/// (L, a, b) take the new values; the others keep the ones from `before`.
pub fn keep_channels(tiles: &mut TileStore, before: &TileStore, coords: &[TileCoord], edit: [bool; 3]) {
    if edit == [true; 3] {
        return;
    }
    for &coord in coords {
        let Some(mut tile) = tiles.get(coord).map(|t| (**t).clone()) else { continue };
        let old = before.get(coord);
        for (i, px) in tile.bytes_mut().as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let was = old.map_or([0; 4], |o| o.bytes().as_chunks::<4>().0[i]);
            // New pixels on empty ground have nothing to keep.
            if px[3] == 0 || was[3] == 0 || *px == was {
                continue;
            }
            let (n, o) = (unpremultiply(*px), unpremultiply(was));
            let (new, old) = (from_rgb([n[0], n[1], n[2]]), from_rgb([o[0], o[1], o[2]]));
            let mixed = [0, 1, 2].map(|k| if edit[k] { new[k] } else { old[k] });
            let [r, g, b] = to_rgb(mixed);
            *px = premultiply([r, g, b, px[3]]);
        }
        tiles.insert(coord, tile);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_colours_and_round_trips() {
        let white = from_rgb([255, 255, 255]);
        assert!((white[0] - 100.0).abs() < 0.1 && white[1].abs() < 0.2 && white[2].abs() < 0.2, "{white:?}");
        assert!(from_rgb([0, 0, 0])[0].abs() < 0.01);
        // Photoshop shows sRGB red as about L 54, a 81, b 70.
        let red = from_rgb([255, 0, 0]);
        assert!((red[0] - 54.3).abs() < 1.0 && (red[1] - 80.8).abs() < 1.5 && (red[2] - 69.9).abs() < 1.5, "{red:?}");
        for rgb in [[12, 200, 77], [255, 128, 0], [3, 4, 250], [128, 128, 128]] {
            assert_eq!(to_rgb(from_rgb(rgb)), rgb);
        }
        assert_eq!(channel_byte([50.0, -10.0, 20.0], 0), 128);
        assert_eq!(channel_byte([50.0, -10.0, 20.0], 1), 118);
    }

    #[test]
    fn edits_can_change_only_lightness() {
        // Muted colours, so the mix stays inside sRGB.
        let before = TileStore::from_rgba(64, 64, &[180, 120, 110, 255].repeat(64 * 64));
        let mut after = TileStore::from_rgba(64, 64, &[90, 100, 140, 255].repeat(64 * 64));
        let coords: Vec<TileCoord> = after.iter().map(|(c, _)| c).collect();
        keep_channels(&mut after, &before, &coords, [true, false, false]);
        let p = after.pixel(3, 3);
        let (l_new, l_blue, old) = (from_rgb([p[0], p[1], p[2]]), from_rgb([90, 100, 140]), from_rgb([180, 120, 110]));
        assert!((l_new[0] - l_blue[0]).abs() < 1.5, "lightness from the edit");
        assert!(p[0] > p[2], "still reddish: {p:?}");
        assert!((l_new[1] - old[1]).abs() < 2.0 && (l_new[2] - old[2]).abs() < 2.0, "a and b kept: {l_new:?} vs {old:?}");
    }
}
