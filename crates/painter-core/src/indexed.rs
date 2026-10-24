//! Indexed Color (Photoshop's Image → Mode → Indexed Color): the image in
//! at most 256 colours, its colour table. The conversion picks the table
//! (adaptive from the picture, exact, web, uniform…), can force black and
//! white or the primaries in, keep transparency, and dither; later edits snap
//! to the table, and Color Table changes colours where they're used.

use crate::blend::{premultiply, unpremultiply};
use crate::layer::{Layer, LayerKind};
use crate::tile::{TileCoord, TileStore};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteKind {
    /// The picture's own colours (median cut), as Photoshop's Local (Adaptive).
    Adaptive,
    /// Every colour of the picture, when it has 256 or fewer.
    Exact,
    /// The 216 colours of the web palette.
    Web,
    /// Evenly spaced levels of red, green and blue.
    Uniform,
    /// Greys.
    Grayscale,
}

impl PaletteKind {
    pub const ALL: [PaletteKind; 5] =
        [PaletteKind::Adaptive, PaletteKind::Exact, PaletteKind::Web, PaletteKind::Uniform, PaletteKind::Grayscale];

    pub fn name(self) -> &'static str {
        match self {
            PaletteKind::Adaptive => "Local (Adaptive)",
            PaletteKind::Exact => "Exact",
            PaletteKind::Web => "Web",
            PaletteKind::Uniform => "Uniform",
            PaletteKind::Grayscale => "Grayscale",
        }
    }

    /// Whether the number of colours applies.
    pub fn takes_count(self) -> bool {
        matches!(self, PaletteKind::Adaptive | PaletteKind::Uniform | PaletteKind::Grayscale)
    }
}

/// Colours always in the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forced {
    None,
    BlackAndWhite,
    /// Black, white, red, green, blue, cyan, magenta, yellow.
    Primaries,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dither {
    None,
    /// Floyd–Steinberg error diffusion.
    Diffusion,
    /// An ordered 8 × 8 pattern.
    Pattern,
    Noise,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IndexedOptions {
    pub palette: PaletteKind,
    /// 2–256.
    pub colors: usize,
    pub forced: Forced,
    /// Keep see-through pixels (else they become white).
    pub transparency: bool,
    pub dither: Dither,
    /// 0–1.
    pub amount: f32,
}

impl Default for IndexedOptions {
    fn default() -> Self {
        Self {
            palette: PaletteKind::Adaptive,
            colors: 256,
            forced: Forced::None,
            transparency: true,
            dither: Dither::Diffusion,
            amount: 0.75,
        }
    }
}

fn forced_colors(f: Forced) -> Vec<[u8; 3]> {
    match f {
        Forced::None => vec![],
        Forced::BlackAndWhite => vec![[0, 0, 0], [255, 255, 255]],
        Forced::Primaries => {
            vec![[0, 0, 0], [255, 255, 255], [255, 0, 0], [0, 255, 0], [0, 0, 255], [0, 255, 255], [255, 0, 255], [255, 255, 0]]
        }
    }
}

/// The web palette.
pub fn web() -> Vec<[u8; 3]> {
    let l = [0u8, 51, 102, 153, 204, 255];
    let mut out = Vec::with_capacity(216);
    for r in l {
        for g in l {
            for b in l {
                out.push([r, g, b]);
            }
        }
    }
    out
}

/// `n` greys, black to white.
pub fn grays(n: usize) -> Vec<[u8; 3]> {
    let n = n.clamp(2, 256);
    (0..n).map(|i| [(i * 255 / (n - 1)) as u8; 3]).collect()
}

/// Up to `n` colours on an even grid of red, green and blue.
pub fn uniform(n: usize) -> Vec<[u8; 3]> {
    let k = ((n.max(8) as f64).cbrt().floor() as usize).max(2);
    let level = |i: usize| (i * 255 / (k - 1)) as u8;
    (0..k).flat_map(|r| (0..k).flat_map(move |g| (0..k).map(move |b| [level(r), level(g), level(b)]))).collect()
}

/// Tables for the Color Table dialog: a rainbow and a black body (fire) ramp.
pub fn spectrum() -> Vec<[u8; 3]> {
    (0..256)
        .map(|i| {
            let h = i as f32 / 256.0 * 6.0;
            let x = (1.0 - (h % 2.0 - 1.0).abs()) * 255.0;
            let x = x as u8;
            match h as u32 {
                0 => [255, x, 0],
                1 => [x, 255, 0],
                2 => [0, 255, x],
                3 => [0, x, 255],
                4 => [x, 0, 255],
                _ => [255, 0, x],
            }
        })
        .collect()
}

pub fn black_body() -> Vec<[u8; 3]> {
    (0..256)
        .map(|i| {
            let t = i as f32 / 255.0;
            let c = |start: f32| ((t - start) / (1.0 - start) * 3.0).clamp(0.0, 1.0);
            [(c(0.0) * 255.0) as u8, (c(0.33) * 255.0) as u8, (c(0.66) * 255.0) as u8]
        })
        .collect()
}

/// Median cut over a histogram of 5-bit-per-channel colours, weighted by count.
fn median_cut(pixels: &[[u8; 3]], n: usize) -> Vec<[u8; 3]> {
    let mut hist = vec![0u32; 32 * 32 * 32];
    for p in pixels {
        hist[((p[0] as usize >> 3) << 10) | ((p[1] as usize >> 3) << 5) | (p[2] as usize >> 3)] += 1;
    }
    // Each bin's colour is the average of the pixels in it.
    let mut sums = vec![[0u64; 3]; hist.len()];
    for p in pixels {
        let i = ((p[0] as usize >> 3) << 10) | ((p[1] as usize >> 3) << 5) | (p[2] as usize >> 3);
        for k in 0..3 {
            sums[i][k] += p[k] as u64;
        }
    }
    let bins: Vec<([f32; 3], u32)> =
        hist.iter().enumerate().filter(|(_, &c)| c > 0).map(|(i, &c)| (sums[i].map(|s| s as f32 / c as f32), c)).collect();
    let mut boxes: Vec<Vec<([f32; 3], u32)>> = vec![bins];
    while boxes.len() < n {
        // Split the box with the widest spread (times its pixel count).
        let score = |b: &Vec<([f32; 3], u32)>| {
            if b.len() < 2 {
                return (0.0, 0);
            }
            let mut best = (0.0f32, 0usize);
            for k in 0..3 {
                let (lo, hi) = b.iter().fold((f32::MAX, f32::MIN), |(l, h), (c, _)| (l.min(c[k]), h.max(c[k])));
                if hi - lo > best.0 {
                    best = (hi - lo, k);
                }
            }
            let count: u32 = b.iter().map(|(_, c)| *c).sum();
            (best.0 * (count as f32).sqrt(), best.1)
        };
        let best = boxes.iter().map(score).enumerate().max_by(|a, b| a.1 .0.total_cmp(&b.1 .0));
        let Some((i, (spread, axis))) = best else { break };
        if spread <= 0.0 {
            // Nothing left to split: fewer colours than asked.
            break;
        }
        let mut b = boxes.swap_remove(i);
        b.sort_by(|x, y| x.0[axis].total_cmp(&y.0[axis]));
        let total: u32 = b.iter().map(|(_, c)| *c).sum();
        let mut acc = 0;
        let cut = b.iter().position(|(_, c)| {
            acc += c;
            acc * 2 >= total
        });
        let cut = cut.unwrap_or(0).clamp(0, b.len() - 2) + 1;
        let rest = b.split_off(cut);
        boxes.push(b);
        boxes.push(rest);
    }
    boxes
        .iter()
        .filter(|b| !b.is_empty())
        .map(|b| {
            let total: f32 = b.iter().map(|(_, c)| *c as f32).sum();
            let mut m = [0.0f32; 3];
            for (c, n) in b {
                for k in 0..3 {
                    m[k] += c[k] * *n as f32 / total;
                }
            }
            m.map(|v| (v + 0.5).clamp(0.0, 255.0) as u8)
        })
        .collect()
}

/// The colours of a flattened picture (premultiplied RGBA8) as they'd be indexed:
/// over white unless transparency is kept, see-through pixels left out.
fn colors_of(rgba: &[u8], transparency: bool) -> Vec<[u8; 3]> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .filter(|p| !transparency || p[3] >= 128)
        .map(|p| {
            if transparency {
                let s = unpremultiply(*p);
                [s[0], s[1], s[2]]
            } else {
                let w = 255 - p[3];
                [p[0].saturating_add(w), p[1].saturating_add(w), p[2].saturating_add(w)]
            }
        })
        .collect()
}

/// The colour table for a picture. `Err` when Exact meets more than 256 colours.
pub fn build_palette(rgba: &[u8], opts: &IndexedOptions) -> Result<Vec<[u8; 3]>, String> {
    let forced = forced_colors(opts.forced);
    let room = opts.colors.clamp(2, 256).saturating_sub(forced.len()).max(1);
    let mut table = match opts.palette {
        PaletteKind::Web => web(),
        PaletteKind::Uniform => uniform(room),
        PaletteKind::Grayscale => grays(room),
        PaletteKind::Exact => {
            let mut seen: Vec<[u8; 3]> = colors_of(rgba, opts.transparency);
            seen.sort_unstable();
            seen.dedup();
            if seen.len() + forced.len() > 256 {
                return Err(format!("The picture has {} colours: Exact takes 256 at most", seen.len()));
            }
            seen
        }
        PaletteKind::Adaptive => {
            let px = colors_of(rgba, opts.transparency);
            if px.is_empty() {
                vec![[255; 3]]
            } else {
                median_cut(&px, room)
            }
        }
    };
    for f in forced.into_iter().rev() {
        if !table.contains(&f) {
            table.insert(0, f);
        }
    }
    table.truncate(256);
    Ok(table)
}

/// Nearest-colour lookup through a 64-level cube.
pub struct Nearest {
    cube: Vec<u8>,
}

impl Nearest {
    pub fn new(palette: &[[u8; 3]]) -> Self {
        let mut cube = vec![0u8; 64 * 64 * 64];
        for (i, slot) in cube.iter_mut().enumerate() {
            let c = [(i >> 12) as i32 * 4 + 2, ((i >> 6) & 63) as i32 * 4 + 2, (i & 63) as i32 * 4 + 2];
            *slot = nearest_slow(palette, c) as u8;
        }
        Self { cube }
    }

    pub fn index(&self, c: [u8; 3]) -> usize {
        self.cube[((c[0] as usize >> 2) << 12) | ((c[1] as usize >> 2) << 6) | (c[2] as usize >> 2)] as usize
    }
}

fn nearest_slow(palette: &[[u8; 3]], c: [i32; 3]) -> usize {
    let mut best = (i32::MAX, 0);
    for (i, p) in palette.iter().enumerate() {
        // Weighted for the eye: green counts most.
        let d = [p[0] as i32 - c[0], p[1] as i32 - c[1], p[2] as i32 - c[2]];
        let e = 2 * d[0] * d[0] + 4 * d[1] * d[1] + 3 * d[2] * d[2];
        if e < best.0 {
            best = (e, i);
        }
    }
    best.1
}

/// Index of the table colour closest to `c`, exactly (for one colour).
pub fn nearest(palette: &[[u8; 3]], c: [u8; 3]) -> usize {
    nearest_slow(palette, c.map(|v| v as i32))
}

const BAYER: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

/// The picture (premultiplied RGBA8, `w` × `h`) in the table's colours:
/// premultiplied, each pixel opaque or (with transparency) fully clear.
pub fn index_image(rgba: &[u8], w: usize, h: usize, palette: &[[u8; 3]], opts: &IndexedOptions) -> Vec<u8> {
    let near = Nearest::new(palette);
    let amount = opts.amount.clamp(0.0, 1.0);
    let clear = |p: &[u8; 4]| opts.transparency && p[3] < 128;
    let mut colour: Vec<[f32; 3]> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            let c = if opts.transparency {
                let s = unpremultiply(*p);
                [s[0], s[1], s[2]]
            } else {
                let wh = 255 - p[3];
                [p[0].saturating_add(wh), p[1].saturating_add(wh), p[2].saturating_add(wh)]
            };
            c.map(|v| v as f32)
        })
        .collect();
    let mut out = vec![0u8; w * h * 4];
    let spread = 255.0 / (palette.len() as f32).cbrt().max(2.0);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let src = &rgba[i * 4..i * 4 + 4];
            if clear(&[src[0], src[1], src[2], src[3]]) {
                continue;
            }
            let mut c = colour[i];
            match opts.dither {
                Dither::Pattern => {
                    let t = (BAYER[y % 8][x % 8] as f32 + 0.5) / 64.0 - 0.5;
                    c = c.map(|v| v + t * spread * amount);
                }
                Dither::Noise => {
                    let hsh = ((x as u32).wrapping_mul(73_856_093) ^ (y as u32).wrapping_mul(19_349_663)).wrapping_mul(2_654_435_761);
                    let t = (hsh >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
                    c = c.map(|v| v + t * spread * amount);
                }
                Dither::None | Dither::Diffusion => {}
            }
            let q = c.map(|v| v.round().clamp(0.0, 255.0) as u8);
            let pick = palette[near.index(q)];
            out[i * 4..i * 4 + 4].copy_from_slice(&[pick[0], pick[1], pick[2], 255]);
            if opts.dither == Dither::Diffusion {
                let err = [0, 1, 2].map(|k| (c[k] - pick[k] as f32) * amount);
                let mut give = |dx: isize, dy: usize, f: f32| {
                    let nx = x as isize + dx;
                    if nx >= 0 && (nx as usize) < w && y + dy < h {
                        let j = (y + dy) * w + nx as usize;
                        for k in 0..3 {
                            colour[j][k] += err[k] * f;
                        }
                    }
                };
                give(1, 0, 7.0 / 16.0);
                give(-1, 1, 3.0 / 16.0);
                give(0, 1, 5.0 / 16.0);
                give(1, 1, 1.0 / 16.0);
            }
        }
    }
    out
}

/// After an edit of `coords`: every pixel in a table colour (no dithering),
/// opaque or clear.
pub fn snap_tiles(tiles: &mut TileStore, coords: &[TileCoord], palette: &[[u8; 3]]) {
    if palette.is_empty() {
        return;
    }
    let near = Nearest::new(palette);
    for &coord in coords {
        let Some(mut tile) = tiles.get(coord).map(|t| (**t).clone()) else { continue };
        for px in tile.bytes_mut().as_chunks_mut::<4>().0 {
            *px = if px[3] < 128 {
                [0; 4]
            } else {
                let s = unpremultiply(*px);
                let c = palette[near.index([s[0], s[1], s[2]])];
                [c[0], c[1], c[2], 255]
            };
        }
        tiles.insert(coord, tile);
    }
}

/// Colour Table edits: wherever a layer has `from[i]`, it gets `to[i]`.
pub fn recolor_layers(layers: &mut [Layer], from: &[[u8; 3]], to: &[[u8; 3]]) {
    let map: std::collections::HashMap<[u8; 3], [u8; 3]> =
        from.iter().zip(to).filter(|(a, b)| a != b).map(|(a, b)| (*a, *b)).collect();
    if map.is_empty() {
        return;
    }
    for layer in layers {
        match &mut layer.kind {
            LayerKind::Raster(tiles) => {
                let coords: Vec<TileCoord> = tiles.iter().map(|(c, _)| c).collect();
                for coord in coords {
                    let Some(mut tile) = tiles.get(coord).map(|t| (**t).clone()) else { continue };
                    let mut changed = false;
                    for px in tile.bytes_mut().as_chunks_mut::<4>().0 {
                        if px[3] == 0 {
                            continue;
                        }
                        let s = unpremultiply(*px);
                        if let Some(c) = map.get(&[s[0], s[1], s[2]]) {
                            *px = premultiply([c[0], c[1], c[2], px[3]]);
                            changed = true;
                        }
                    }
                    if changed {
                        tiles.insert(coord, tile);
                    }
                }
            }
            LayerKind::Folder(f) => recolor_layers(&mut f.children, from, to),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture() -> (Vec<u8>, usize, usize) {
        // A red-to-blue ramp with a see-through corner.
        let (w, h) = (64, 32);
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                if x < 4 && y < 4 {
                    px.extend([0, 0, 0, 0]);
                } else {
                    px.extend([(255 - x * 4) as u8, 40, (x * 4) as u8, 255]);
                }
            }
        }
        (px, w, h)
    }

    #[test]
    fn tables_of_the_asked_size() {
        let (px, _, _) = picture();
        let o = IndexedOptions { colors: 16, ..Default::default() };
        let t = build_palette(&px, &o).unwrap();
        assert!(t.len() <= 16 && t.len() >= 8, "{}", t.len());
        assert_eq!(web().len(), 216);
        assert_eq!(uniform(256).len(), 216);
        assert_eq!(grays(4), vec![[0; 3], [85; 3], [170; 3], [255; 3]]);
        let forced = build_palette(&px, &IndexedOptions { colors: 8, forced: Forced::BlackAndWhite, ..Default::default() }).unwrap();
        assert!(forced.contains(&[0, 0, 0]) && forced.contains(&[255, 255, 255]) && forced.len() <= 8);
        // 64 colours fit Exact; a smooth photo wouldn't.
        assert_eq!(build_palette(&px, &IndexedOptions { palette: PaletteKind::Exact, ..Default::default() }).unwrap().len(), 64);
        let many: Vec<u8> = (0..300u32).flat_map(|i| [i as u8, (i / 256) as u8 * 100, 7, 255]).collect();
        assert!(build_palette(&many, &IndexedOptions { palette: PaletteKind::Exact, ..Default::default() }).is_err());
    }

    #[test]
    fn indexed_pixels_are_table_colours() {
        let (px, w, h) = picture();
        for dither in [Dither::None, Dither::Diffusion, Dither::Pattern, Dither::Noise] {
            let o = IndexedOptions { colors: 6, dither, ..Default::default() };
            let t = build_palette(&px, &o).unwrap();
            let out = index_image(&px, w, h, &t, &o);
            for (i, p) in out.as_chunks::<4>().0.iter().enumerate() {
                if i % w < 4 && i / w < 4 {
                    assert_eq!(*p, [0; 4], "{dither:?}: transparency kept");
                } else {
                    assert!(p[3] == 255 && t.contains(&[p[0], p[1], p[2]]), "{dither:?}: {p:?}");
                }
            }
        }
        // Without transparency, the clear corner is white paper.
        let o = IndexedOptions {
            transparency: false,
            forced: Forced::BlackAndWhite,
            colors: 8,
            dither: Dither::None,
            ..Default::default()
        };
        let t = build_palette(&px, &o).unwrap();
        assert_eq!(&index_image(&px, w, h, &t, &o)[..4], &[255, 255, 255, 255]);
    }

    #[test]
    fn edits_snap_and_colours_change_by_table() {
        let table = vec![[0, 0, 0], [250, 10, 10], [255, 255, 255]];
        let mut tiles = TileStore::from_rgba(64, 64, &[230, 40, 30, 255].repeat(64 * 64));
        let coords: Vec<TileCoord> = tiles.iter().map(|(c, _)| c).collect();
        snap_tiles(&mut tiles, &coords, &table);
        assert_eq!(tiles.pixel(5, 5), [250, 10, 10, 255]);
        let mut layers = vec![Layer::raster_with(crate::layer::LayerId(1), "Index", tiles)];
        recolor_layers(&mut layers, &table, &[[0, 0, 0], [0, 0, 200], [255, 255, 255]]);
        assert_eq!(layers[0].tiles().unwrap().pixel(5, 5), [0, 0, 200, 255]);
    }
}
