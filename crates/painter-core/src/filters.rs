//! Destructive filters on a layer's pixels: Gaussian, motion and radial
//! blur, unsharp mask, noise.
//!
//! Each works on a dense copy of the area that can change (the layer's
//! content grown by the filter's reach, cut to the canvas and to the
//! selection), on premultiplied RGBA8, rows split across threads. With a
//! selection, the result is mixed in by its alpha, so feathered selections
//! fade the filter out.

use crate::fill_tool::par_rows;
use crate::tile::{Tile, TileCoord, TileStore, TILE_SIZE};

#[derive(Clone, Debug, PartialEq)]
pub enum Filter {
    /// Strength in pixels (about two standard deviations).
    GaussianBlur { radius: f32 },
    /// Streak length in pixels along `angle` (degrees, clockwise on screen).
    MotionBlur { angle: f32, distance: f32 },
    /// Zoom blur towards `center`; `amount` 0..1 of the distance to it.
    RadialBlur { center: [f32; 2], amount: f32 },
    /// Sharpen: adds `amount` × (pixel − blurred) at `radius`.
    UnsharpMask { radius: f32, amount: f32 },
    /// Random brightness changes up to `amount` (0..1).
    Noise { amount: f32, monochrome: bool, seed: u64 },
}

impl Filter {
    pub fn name(&self) -> &'static str {
        match self {
            Filter::GaussianBlur { .. } => "Gaussian Blur",
            Filter::MotionBlur { .. } => "Motion Blur",
            Filter::RadialBlur { .. } => "Radial Blur",
            Filter::UnsharpMask { .. } => "Unsharp Mask",
            Filter::Noise { .. } => "Add Noise",
        }
    }

    /// How far (pixels) a pixel's colour can travel.
    fn reach(&self, canvas: (u32, u32)) -> Option<u32> {
        Some(match self {
            Filter::GaussianBlur { radius } | Filter::UnsharpMask { radius, .. } => (radius * 1.6).ceil() as u32 + 2,
            Filter::MotionBlur { distance, .. } => (distance * 0.5).ceil() as u32 + 1,
            // Can move content anywhere towards the centre: the whole canvas.
            Filter::RadialBlur { .. } => return None,
            Filter::Noise { .. } => 0,
        })
        .map(|r| r.min(canvas.0.max(canvas.1)))
    }
}

/// A rectangle in canvas pixels: x, y, width, height.
type Area = [u32; 4];

fn bounds_of(tiles: &TileStore, canvas: (u32, u32)) -> Option<Area> {
    let ts = TILE_SIZE as i32;
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for (c, tile) in tiles.iter() {
        if tile.is_transparent() {
            continue;
        }
        let (ox, oy) = c.origin();
        x0 = x0.min(ox);
        y0 = y0.min(oy);
        x1 = x1.max(ox + ts);
        y1 = y1.max(oy + ts);
    }
    if x0 > x1 {
        return None;
    }
    clip([x0, y0, x1, y1], canvas)
}

fn clip([x0, y0, x1, y1]: [i32; 4], canvas: (u32, u32)) -> Option<Area> {
    let (x0, y0) = (x0.max(0), y0.max(0));
    let (x1, y1) = (x1.min(canvas.0 as i32), y1.min(canvas.1 as i32));
    (x1 > x0 && y1 > y0).then_some([x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32])
}

fn grow([x, y, w, h]: Area, by: u32, canvas: (u32, u32)) -> Option<Area> {
    let by = by as i32;
    clip([x as i32 - by, y as i32 - by, (x + w) as i32 + by, (y + h) as i32 + by], canvas)
}

fn intersect(a: Area, b: Area) -> Option<Area> {
    let (x0, y0) = (a[0].max(b[0]), a[1].max(b[1]));
    let (x1, y1) = ((a[0] + a[2]).min(b[0] + b[2]), (a[1] + a[3]).min(b[1] + b[3]));
    (x1 > x0 && y1 > y0).then_some([x0, y0, x1 - x0, y1 - y0])
}

/// The pixels of `area` as a dense premultiplied RGBA8 buffer.
fn extract(tiles: &TileStore, [x0, y0, w, h]: Area) -> Vec<u8> {
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let px = tiles.pixel((x0 + x) as i32, (y0 + y) as i32);
            out[((y * w + x) * 4) as usize..((y * w + x) * 4 + 4) as usize].copy_from_slice(&px);
        }
    }
    out
}

/// `tiles` with `filtered` written over `area`, mixed by the selection's alpha.
fn write_back(tiles: &TileStore, area: Area, original: &[u8], filtered: &[u8], selection: Option<&TileStore>) -> TileStore {
    let [x0, y0, w, h] = area;
    let mut out = tiles.clone();
    let ts = TILE_SIZE;
    for ty in y0 / ts..=(y0 + h - 1) / ts {
        for tx in x0 / ts..=(x0 + w - 1) / ts {
            let coord = TileCoord::new(tx as i32, ty as i32);
            let mut tile: Tile = out.get(coord).map(|t| (**t).clone()).unwrap_or_else(Tile::empty);
            let sel_tile = selection.and_then(|s| s.get(coord));
            for ly in 0..ts {
                let y = ty * ts + ly;
                if y < y0 || y >= y0 + h {
                    continue;
                }
                for lx in 0..ts {
                    let x = tx * ts + lx;
                    if x < x0 || x >= x0 + w {
                        continue;
                    }
                    let i = (((y - y0) * w + (x - x0)) * 4) as usize;
                    let k = match selection {
                        None => 255u32,
                        Some(_) => sel_tile.map_or(0, |t| t.pixel(lx, ly)[3] as u32),
                    };
                    if k == 0 {
                        continue;
                    }
                    let px: [u8; 4] = std::array::from_fn(|c| {
                        let (a, b) = (original[i + c] as u32, filtered[i + c] as u32);
                        ((a * (255 - k) + b * k + 127) / 255) as u8
                    });
                    tile.set_pixel(lx, ly, px);
                }
            }
            if tile.is_transparent() {
                out.remove(coord);
            } else {
                out.insert(coord, tile);
            }
        }
    }
    out
}

/// Runs `filter` on `tiles` (a `canvas`-sized layer), limited to `selection`.
pub fn apply(tiles: &TileStore, canvas: (u32, u32), selection: Option<&TileStore>, filter: &Filter) -> TileStore {
    let full = [0, 0, canvas.0, canvas.1];
    // What can change: the content grown by the reach, inside the selection.
    let mut area = match filter.reach(canvas) {
        Some(reach) => match bounds_of(tiles, canvas).and_then(|b| grow(b, reach, canvas)) {
            Some(a) => a,
            None => return tiles.clone(),
        },
        None => full,
    };
    if let Some(sel) = selection {
        match bounds_of(sel, canvas).and_then(|b| intersect(b, area)) {
            Some(a) => area = a,
            None => return tiles.clone(),
        }
    }
    // Read a little more than is written, so blurs see their neighbours.
    let read = match filter.reach(canvas) {
        Some(reach) => grow(area, reach, canvas).unwrap_or(area),
        None => full,
    };
    let src = extract(tiles, read);
    let dst = run(&src, read[2] as usize, read[3] as usize, filter, [read[0] as f32, read[1] as f32]);
    // Cut the written area back out of the read one.
    let (dx, dy) = ((area[0] - read[0]) as usize, (area[1] - read[1]) as usize);
    let (rw, w, h) = (read[2] as usize, area[2] as usize, area[3] as usize);
    let cut = |buf: &[u8]| -> Vec<u8> {
        let mut out = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            let start = ((dy + y) * rw + dx) * 4;
            out.extend_from_slice(&buf[start..start + w * 4]);
        }
        out
    };
    write_back(tiles, area, &cut(&src), &cut(&dst), selection)
}

/// The filter on a dense `w` × `h` buffer whose top-left is at canvas `origin`.
fn run(src: &[u8], w: usize, h: usize, filter: &Filter, origin: [f32; 2]) -> Vec<u8> {
    match *filter {
        Filter::GaussianBlur { radius } => gaussian(src, w, h, radius),
        Filter::UnsharpMask { radius, amount } => {
            let blurred = gaussian(src, w, h, radius);
            let mut out = src.to_vec();
            for (o, (s, b)) in out.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0.iter().zip(blurred.as_chunks::<4>().0)) {
                let a = s[3] as f32;
                for c in 0..3 {
                    let v = s[c] as f32 + amount * (s[c] as f32 - b[c] as f32);
                    o[c] = v.round().clamp(0.0, a) as u8;
                }
            }
            out
        }
        Filter::MotionBlur { angle, distance } => {
            let (sy, sx) = angle.to_radians().sin_cos();
            let n = (distance.max(1.0)).round() as i32;
            let offsets: Vec<(i32, i32)> = (0..=n)
                .map(|k| {
                    let t = k as f32 - n as f32 * 0.5;
                    ((t * sx).round() as i32, (t * sy).round() as i32)
                })
                .collect();
            sample_average(src, w, h, |_, _| offsets.clone())
        }
        Filter::RadialBlur { center, amount } => {
            let samples = 24;
            let amount = amount.clamp(0.0, 1.0);
            sample_average(src, w, h, move |x, y| {
                let (cx, cy) = (center[0] - origin[0], center[1] - origin[1]);
                let (vx, vy) = (cx - x as f32, cy - y as f32);
                (0..samples)
                    .map(|k| {
                        let t = amount * k as f32 / samples as f32;
                        ((vx * t).round() as i32, (vy * t).round() as i32)
                    })
                    .collect()
            })
        }
        Filter::Noise { amount, monochrome, seed } => {
            let mut out = src.to_vec();
            let width = w;
            par_rows(&mut out, width * 4, |y0, rows| {
                for (k, px) in rows.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    if px[3] == 0 {
                        continue;
                    }
                    let (x, y) = (k % width, y0 + k / width);
                    let a = px[3] as f32;
                    let base = hash(x as u64, y as u64, seed);
                    for (c, v) in px.iter_mut().take(3).enumerate() {
                        let r = if monochrome { base } else { hash(x as u64, y as u64, seed.wrapping_add(c as u64 + 1)) };
                        let n = (r * 2.0 - 1.0) * amount * a;
                        *v = (*v as f32 + n).round().clamp(0.0, a) as u8;
                    }
                }
            });
            out
        }
    }
}

/// 0..1 from a pixel position and seed.
fn hash(x: u64, y: u64, seed: u64) -> f32 {
    let mut h = x.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ y.wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ seed.wrapping_mul(0x1656_67B1_9E37_79F9);
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h ^= h >> 33;
    (h >> 40) as f32 / (1u64 << 24) as f32
}

/// Each output pixel is the average of the source at the offsets `taps(x, y)`
/// gives (clamped to the buffer).
fn sample_average(src: &[u8], w: usize, h: usize, taps: impl Fn(usize, usize) -> Vec<(i32, i32)> + Sync) -> Vec<u8> {
    let mut out = vec![0u8; src.len()];
    par_rows(&mut out, w * 4, |y0, rows| {
        for (k, px) in rows.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = (k % w, y0 + k / w);
            let offsets = taps(x, y);
            let mut sum = [0u32; 4];
            for (dx, dy) in &offsets {
                let sx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                let sy = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                let i = (sy * w + sx) * 4;
                for (s, v) in sum.iter_mut().zip(&src[i..i + 4]) {
                    *s += *v as u32;
                }
            }
            let n = offsets.len().max(1) as u32;
            *px = sum.map(|s| ((s + n / 2) / n) as u8);
        }
    });
    out
}

/// Gaussian blur as three box blurs (horizontal then vertical each), whose
/// widths match a Gaussian of σ = radius / 2.
fn gaussian(src: &[u8], w: usize, h: usize, radius: f32) -> Vec<u8> {
    let sigma = (radius * 0.5).max(0.01);
    let mut buf = src.to_vec();
    for r in box_radii(sigma, 3) {
        if r == 0 {
            continue;
        }
        buf = box_rows(&buf, w, r);
        buf = transpose(&box_rows(&transpose(&buf, w, h), h, r), h, w);
    }
    buf
}

/// Box radii for `n` passes approximating a Gaussian of `sigma`.
fn box_radii(sigma: f32, n: usize) -> Vec<usize> {
    let ideal = (12.0 * sigma * sigma / n as f32 + 1.0).sqrt();
    let mut wl = ideal.floor() as i32;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wu = wl + 2;
    let m = ((12.0 * sigma * sigma - (n as i32 * wl * wl + 4 * n as i32 * wl + 3 * n as i32) as f32) / (-4 * wl - 4) as f32).round() as i32;
    (0..n as i32).map(|i| (if i < m { wl } else { wu }).max(1) as usize / 2).collect()
}

/// Box blur of radius `r` along each row (clamped edges), running sums.
fn box_rows(src: &[u8], w: usize, r: usize) -> Vec<u8> {
    let mut out = vec![0u8; src.len()];
    let n = (2 * r + 1) as u32;
    par_rows(&mut out, w * 4, |y0, rows| {
        for (k, row_out) in rows.chunks_mut(w * 4).enumerate() {
            let row = &src[(y0 + k) * w * 4..(y0 + k + 1) * w * 4];
            let at = |x: i64, c: usize| row[(x.clamp(0, w as i64 - 1) as usize) * 4 + c] as u32;
            let mut sum: [u32; 4] = std::array::from_fn(|c| (-(r as i64)..=(r as i64)).map(|x| at(x, c)).sum());
            for x in 0..w {
                for c in 0..4 {
                    row_out[x * 4 + c] = ((sum[c] + n / 2) / n) as u8;
                    sum[c] = sum[c] + at(x as i64 + r as i64 + 1, c) - at(x as i64 - r as i64, c);
                }
            }
        }
    });
    out
}

fn transpose(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; src.len()];
    for y in 0..h {
        for x in 0..w {
            let (i, j) = ((y * w + x) * 4, (x * h + y) * 4);
            out[j..j + 4].copy_from_slice(&src[i..i + 4]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1-pixel white vertical line at x = 50 on a 100 × 60 canvas.
    fn line() -> TileStore {
        let mut t = TileStore::new();
        for y in 0..60 {
            let c = TileCoord::containing(50, y);
            let (ox, oy) = c.origin();
            t.get_or_create_mut(c).set_pixel((50 - ox) as u32, (y - oy) as u32, [255; 4]);
        }
        t
    }

    #[test]
    fn gaussian_spreads_and_keeps_the_total() {
        let canvas = (100, 60);
        let out = apply(&line(), canvas, None, &Filter::GaussianBlur { radius: 8.0 });
        let row: Vec<u32> = (0..100).map(|x| out.pixel(x, 30)[3] as u32).collect();
        assert!(row[50] < 255 && row[50] > 0, "the line is softened");
        assert!(row[46] > 0 && row[54] > 0, "and spread");
        assert_eq!(row[20], 0);
        let total: u32 = row.iter().sum();
        assert!((total as i32 - 255).abs() < 20, "alpha is conserved: {total}");
        assert!((row[47] as i32 - row[53] as i32).abs() <= 1, "symmetric");
    }

    #[test]
    fn motion_blur_streaks_along_its_angle() {
        let canvas = (100, 60);
        let horizontal = apply(&line(), canvas, None, &Filter::MotionBlur { angle: 0.0, distance: 10.0 });
        assert!(horizontal.pixel(54, 30)[3] > 0 && horizontal.pixel(50, 30)[3] < 255);
        let vertical = apply(&line(), canvas, None, &Filter::MotionBlur { angle: 90.0, distance: 10.0 });
        assert_eq!(vertical.pixel(50, 30), [255; 4], "along the line: unchanged");
        assert_eq!(vertical.pixel(53, 30)[3], 0);
    }

    #[test]
    fn selection_limits_the_filter() {
        let canvas = (100, 60);
        let mut sel = TileStore::new();
        for y in 0..30 {
            for x in 0..100 {
                let c = TileCoord::containing(x, y);
                let (ox, oy) = c.origin();
                sel.get_or_create_mut(c).set_pixel((x - ox) as u32, (y - oy) as u32, [255; 4]);
            }
        }
        let out = apply(&line(), canvas, Some(&sel), &Filter::GaussianBlur { radius: 8.0 });
        assert!(out.pixel(50, 10)[3] < 255, "blurred inside");
        assert_eq!(out.pixel(50, 50), [255; 4], "untouched outside");
        assert_eq!(out.pixel(53, 50)[3], 0);
    }

    #[test]
    fn sharpen_radial_and_noise() {
        let canvas = (100, 60);
        // A soft grey stripe gets harder edges.
        let soft = apply(&line(), canvas, None, &Filter::GaussianBlur { radius: 6.0 });
        let sharp = apply(&soft, canvas, None, &Filter::UnsharpMask { radius: 6.0, amount: 1.5 });
        assert!(sharp.pixel(50, 30)[0] >= soft.pixel(50, 30)[0]);
        let p = sharp.pixel(50, 30);
        assert!(p[0] <= p[3], "still premultiplied");

        let radial = apply(&line(), canvas, None, &Filter::RadialBlur { center: [50.0, 0.0], amount: 0.5 });
        assert!(radial.pixel(50, 40)[3] > 0, "pixels on the ray to the centre stay");

        let mut grey = TileStore::new();
        for y in 0..10 {
            for x in 0..10 {
                grey.get_or_create_mut(TileCoord::new(0, 0)).set_pixel(x, y, [128, 128, 128, 255]);
            }
        }
        let noisy = apply(&grey, canvas, None, &Filter::Noise { amount: 0.3, monochrome: true, seed: 1 });
        let values: std::collections::HashSet<u8> = (0..10).map(|x| noisy.pixel(x, 5)[0]).collect();
        assert!(values.len() > 3, "varied");
        let p = noisy.pixel(3, 3);
        assert!(p[0] == p[1] && p[1] == p[2], "monochrome");
        assert_eq!(noisy.pixel(20, 20)[3], 0, "transparent stays transparent");
        assert_eq!(apply(&TileStore::new(), canvas, None, &Filter::GaussianBlur { radius: 3.0 }).len(), 0);
    }
}
