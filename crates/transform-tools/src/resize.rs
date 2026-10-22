//! Image → Image Size: the whole document resampled to a new pixel size.
//!
//! Every raster layer, mask and the selection is resampled; vector strokes
//! and gradient handles are scaled and re-rasterised; fills, tones and
//! correction layers just cover the new canvas.
//!
//! [`Interpolation::Nearest`] keeps pixels: each output pixel is a copy of
//! one source pixel, no new colours, so pixel art scaled by a whole factor
//! stays exactly blocky. The other filters average neighbouring pixels;
//! shrinking with them first averages each output pixel's area (box), so
//! reductions don't alias.

use painter_core::{Document, Layer, LayerKind, TileStore, TILE_SIZE};

use crate::matrix::Mat3;
use crate::resample::{resample, Image, Interpolation};

/// A tile store as one premultiplied image of `width` × `height`.
fn to_image(tiles: &TileStore, width: u32, height: u32) -> Image {
    let mut img = Image::new(width, height);
    for (coord, tile) in tiles.iter() {
        let (ox, oy) = coord.origin();
        for y in 0..TILE_SIZE {
            let py = oy + y as i32;
            if py < 0 || py >= height as i32 {
                continue;
            }
            for x in 0..TILE_SIZE {
                let px = ox + x as i32;
                if px < 0 || px >= width as i32 {
                    continue;
                }
                let i = ((py as u32 * width + px as u32) * 4) as usize;
                img.rgba[i..i + 4].copy_from_slice(&tile.pixel(x, y));
            }
        }
    }
    img
}

/// Averages each `kx` × `ky` block (whole factors; edges average what's there).
fn box_shrink(src: &Image, kx: u32, ky: u32) -> Image {
    let (w, h) = (src.width.div_ceil(kx), src.height.div_ceil(ky));
    let mut out = Image::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0u32; 4];
            let mut n = 0;
            for sy in y * ky..((y + 1) * ky).min(src.height) {
                for sx in x * kx..((x + 1) * kx).min(src.width) {
                    let i = ((sy * src.width + sx) * 4) as usize;
                    for (s, v) in sum.iter_mut().zip(&src.rgba[i..i + 4]) {
                        *s += *v as u32;
                    }
                    n += 1;
                }
            }
            let i = ((y * w + x) * 4) as usize;
            for (dst, s) in out.rgba[i..i + 4].iter_mut().zip(sum) {
                *dst = ((s + n / 2) / n.max(1)) as u8;
            }
        }
    }
    out
}

/// `img` resampled to `width` × `height` with `filter`.
pub fn resize_image(img: &Image, width: u32, height: u32, filter: Interpolation) -> Image {
    if (img.width, img.height) == (width, height) {
        return img.clone();
    }
    let mut src = img.clone();
    if filter != Interpolation::Nearest {
        // Shrinking a lot: average whole blocks first, then the filter does the rest.
        let (kx, ky) = ((src.width / width.max(1)).max(1), (src.height / height.max(1)).max(1));
        if kx > 1 || ky > 1 {
            src = box_shrink(&src, kx, ky);
        }
    }
    let m = Mat3::scale(width as f64 / src.width as f64, height as f64 / src.height as f64);
    resample(&src, &m, width, height, filter).unwrap_or_else(|| Image::new(width, height))
}

/// Tiles resampled from a `from` canvas to a `to` canvas.
fn resize_tiles(tiles: &TileStore, from: (u32, u32), to: (u32, u32), filter: Interpolation) -> TileStore {
    if tiles.is_empty() {
        return TileStore::new();
    }
    let out = resize_image(&to_image(tiles, from.0, from.1), to.0, to.1, filter);
    TileStore::from_rgba(to.0, to.1, &out.rgba)
}

/// Resizes the document to `width` × `height` pixels with `filter`. With
/// `keep_print_size` the resolution changes so the page stays the same
/// size on paper; otherwise it stays and the page grows or shrinks.
pub fn resize_document(doc: &mut Document, width: u32, height: u32, filter: Interpolation, keep_print_size: bool) {
    let to = (width.clamp(1, painter_core::MAX_CANVAS_SIZE), height.clamp(1, painter_core::MAX_CANVAS_SIZE));
    let from = doc.canvas();
    if to == from {
        return;
    }
    let (sx, sy) = (to.0 as f32 / from.0 as f32, to.1 as f32 / from.1 as f32);
    fn walk(layers: &mut [Layer], from: (u32, u32), to: (u32, u32), s: (f32, f32), filter: Interpolation) {
        for layer in layers {
            if let Some(mask) = &mut layer.mask {
                // Masks are coverage: nearest keeps hard edges, otherwise smooth.
                mask.tiles = resize_tiles(&mask.tiles, from, to, filter);
            }
            match &mut layer.kind {
                LayerKind::Raster(tiles) => *tiles = resize_tiles(tiles, from, to, filter),
                LayerKind::Vector(v) => {
                    let width_scale = (s.0 * s.1).sqrt();
                    for stroke in &mut v.strokes {
                        for p in &mut stroke.points {
                            p.x *= s.0;
                            p.y *= s.1;
                        }
                        stroke.settings.size *= width_scale;
                    }
                    painter_core::vector::rasterize(v, to);
                }
                LayerKind::Folder(folder) => walk(&mut folder.children, from, to, s, filter),
                LayerKind::Gradient(g) => {
                    for p in [&mut g.start, &mut g.end] {
                        p[0] *= s.0;
                        p[1] *= s.1;
                    }
                }
                LayerKind::Fill { .. } | LayerKind::Tone(_) | LayerKind::Adjustment(_) => {}
            }
        }
    }
    walk(&mut doc.layers, from, to, (sx, sy), filter);
    if let Some(selection) = &doc.selection {
        doc.selection = Some(resize_tiles(selection, from, to, filter));
    }
    doc.width = to.0;
    doc.height = to.1;
    if keep_print_size {
        doc.dpi = ((doc.dpi as f32 * sx.max(sy)).round() as u32).max(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use painter_core::TileCoord;

    fn checker(w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let v = if (x + y) % 2 == 0 { 255 } else { 0 };
                img.rgba[i..i + 4].copy_from_slice(&[v, 0, 255 - v, 255]);
            }
        }
        img
    }

    #[test]
    fn nearest_keeps_pixels_exactly() {
        let src = checker(3, 2);
        let big = resize_image(&src, 9, 6, Interpolation::Nearest);
        for y in 0..6 {
            for x in 0..9 {
                let (i, j) = (((y * 9 + x) * 4) as usize, (((y / 3) * 3 + x / 3) * 4) as usize);
                assert_eq!(big.rgba[i..i + 4], src.rgba[j..j + 4], "({x}, {y}) is a copy of ({}, {})", x / 3, y / 3);
            }
        }
        // And back down: the same picture.
        assert_eq!(resize_image(&big, 3, 2, Interpolation::Nearest), src);
        // No colour that wasn't there.
        let odd = resize_image(&src, 7, 5, Interpolation::Nearest);
        assert!(odd.rgba.as_chunks::<4>().0.iter().all(|p| *p == [255, 0, 0, 255] || *p == [0, 0, 255, 255]));
    }

    #[test]
    fn smooth_filters_average_when_shrinking() {
        let src = checker(64, 64);
        let small = resize_image(&src, 8, 8, Interpolation::Bilinear);
        // Half red, half blue everywhere: purple, not aliased stripes.
        assert!(
            small.rgba.as_chunks::<4>().0.iter().all(|p| p[0].abs_diff(128) <= 3 && p[2].abs_diff(127) <= 3),
            "{:?}",
            &small.rgba[..8]
        );
    }

    #[test]
    fn resizes_every_layer_and_the_page() {
        let mut doc = Document::new(64, 32);
        let id = doc.active;
        let red = [200, 0, 0, 255];
        doc.find_mut(id).unwrap().tiles_mut().unwrap().get_or_create_mut(TileCoord::new(0, 0)).set_pixel(10, 5, red);
        doc.dpi = 100;
        resize_document(&mut doc, 192, 96, Interpolation::Nearest, true);
        assert_eq!((doc.width, doc.height, doc.dpi), (192, 96, 300));
        let t = doc.find(id).unwrap().tiles().unwrap();
        for (x, y) in [(30, 15), (32, 17), (31, 16)] {
            assert_eq!(t.pixel(x, y), red, "pixel (10, 5) became the block 30..33 × 15..18");
        }
        assert_eq!(t.pixel(33, 15)[3], 0);
        assert_eq!(t.pixel(29, 15)[3], 0);
        resize_document(&mut doc, 64, 32, Interpolation::Nearest, false);
        assert_eq!(doc.dpi, 300, "kept without keep_print_size");
        assert_eq!(doc.find(id).unwrap().tiles().unwrap().pixel(10, 5), red);
    }
}
