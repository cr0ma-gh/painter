//! Committing a transform: the floating pixels resampled onto the canvas and
//! laid over what's left of the layer (premultiplied source-over), exactly
//! what `painter-render` shows while the transform is open.

use painter_core::{TileCoord, TileStore, TILE_SIZE};

use crate::matrix::{Mat3, Point};
use crate::resample::{resample, sample, Image, Interpolation};

/// Where the floating image (its own pixel coordinates) goes on the canvas.
#[derive(Debug, Clone, PartialEq)]
pub enum Placement {
    /// Image pixels → canvas pixels.
    Matrix(Mat3),
    /// Triangles with canvas positions and image-pixel sources.
    Mesh { positions: Vec<Point>, sources: Vec<Point>, indices: Vec<u32> },
}

impl Placement {
    /// Canvas bounds `(x0, y0, x1, y1)` of the placed image, clipped to the canvas.
    fn bounds(&self, image: &Image, canvas: (u32, u32)) -> Option<(i32, i32, i32, i32)> {
        let points: Vec<Point> = match self {
            Placement::Matrix(m) => {
                let (w, h) = (image.width as f64, image.height as f64);
                // Filters reach past the image edge (Lanczos3: 3 source pixels), so
                // the bounds include that fringe.
                let r = 3.0;
                // A perspective whose vanishing line crosses the image can't be bounded: use the canvas.
                let corners = [[-r, -r], [w + r, -r], [w + r, h + r], [-r, h + r]];
                let behind = corners.iter().any(|&[x, y]| m.0[2][0] * x + m.0[2][1] * y + m.0[2][2] <= 0.0);
                if behind {
                    vec![[0.0, 0.0], [canvas.0 as f64, canvas.1 as f64]]
                } else {
                    corners.iter().map(|&p| m.apply(p)).collect()
                }
            }
            Placement::Mesh { positions, .. } => positions.clone(),
        };
        let x0 = points.iter().map(|p| p[0]).fold(f64::MAX, f64::min).floor().max(0.0) as i32;
        let y0 = points.iter().map(|p| p[1]).fold(f64::MAX, f64::min).floor().max(0.0) as i32;
        let x1 = points.iter().map(|p| p[0]).fold(f64::MIN, f64::max).ceil().min(canvas.0 as f64) as i32;
        let y1 = points.iter().map(|p| p[1]).fold(f64::MIN, f64::max).ceil().min(canvas.1 as f64) as i32;
        (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
    }
}

/// The placed image over its canvas bounds, and where those bounds start.
pub fn render_floating(image: &Image, placement: &Placement, filter: Interpolation, canvas: (u32, u32)) -> Option<(Image, [i32; 2])> {
    let (x0, y0, x1, y1) = placement.bounds(image, canvas)?;
    let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
    let out = match placement {
        Placement::Matrix(m) => resample(image, &(Mat3::translate(-x0 as f64, -y0 as f64) * *m), w, h, filter)?,
        Placement::Mesh { positions, sources, indices } => {
            let shifted: Vec<Point> = positions.iter().map(|p| [p[0] - x0 as f64, p[1] - y0 as f64]).collect();
            rasterize_mesh(image, &shifted, sources, indices, w, h, filter)
        }
    };
    Some((out, [x0, y0]))
}

/// Draws textured triangles like the GPU does: pixel centres inside each
/// triangle (top-left fill rule), sources interpolated linearly, overlaps
/// stacked with premultiplied source-over.
pub fn rasterize_mesh(image: &Image, positions: &[Point], sources: &[Point], indices: &[u32], width: u32, height: u32, filter: Interpolation) -> Image {
    let mut out = Image::new(width, height);
    let mut acc = vec![[0.0f32; 4]; (width * height) as usize];
    for tri in indices.as_chunks::<3>().0 {
        let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| positions[i as usize]);
        let [sa, sb, sc] = [tri[0], tri[1], tri[2]].map(|i| sources[i as usize]);
        let area = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
        if area.abs() < 1e-12 {
            continue;
        }
        // Make the winding consistent so the edge tests share a sign.
        let (b, c, sb, sc, area) = if area < 0.0 { (c, b, sc, sb, -area) } else { (b, c, sb, sc, area) };
        let x0 = a[0].min(b[0]).min(c[0]).floor().max(0.0) as u32;
        let y0 = a[1].min(b[1]).min(c[1]).floor().max(0.0) as u32;
        let x1 = (a[0].max(b[0]).max(c[0]).ceil().max(0.0) as u32).min(width);
        let y1 = (a[1].max(b[1]).max(c[1]).ceil().max(0.0) as u32).min(height);
        let edge = |p: Point, q: Point, r: Point| (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0]);
        // Top-left rule (y down, clockwise-positive area): include an edge's
        // pixels only if it's a top edge (horizontal, going right) or a left edge (going up).
        let owns = |p: Point, q: Point| (p[1] == q[1] && q[0] > p[0]) || q[1] < p[1];
        for y in y0..y1 {
            for x in x0..x1 {
                let p = [x as f64 + 0.5, y as f64 + 0.5];
                let (w0, w1, w2) = (edge(b, c, p), edge(c, a, p), edge(a, b, p));
                let inside = |w: f64, from: Point, to: Point| w > 0.0 || (w == 0.0 && owns(from, to));
                if !(inside(w0, b, c) && inside(w1, c, a) && inside(w2, a, b)) {
                    continue;
                }
                let (u, v, t) = (w0 / area, w1 / area, w2 / area);
                let s = [u * sa[0] + v * sb[0] + t * sc[0], u * sa[1] + v * sb[1] + t * sc[1]];
                let src = sample(image, s[0] as f32, s[1] as f32, filter);
                let a_src = src[3].clamp(0.0, 1.0);
                let src = [src[0].clamp(0.0, a_src), src[1].clamp(0.0, a_src), src[2].clamp(0.0, a_src), a_src];
                let dst = &mut acc[(y * width + x) as usize];
                *dst = std::array::from_fn(|k| src[k] + dst[k] * (1.0 - a_src));
            }
        }
    }
    for (dst, px) in out.rgba.as_chunks_mut::<4>().0.iter_mut().zip(&acc) {
        *dst = px.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
    }
    out
}

/// `image` laid over `tiles` at `origin` (premultiplied source-over).
pub fn composite_over(tiles: &TileStore, image: &Image, origin: [i32; 2]) -> TileStore {
    let mut out = tiles.clone();
    for y in 0..image.height {
        for x in 0..image.width {
            let i = ((y * image.width + x) * 4) as usize;
            let s = &image.rgba[i..i + 4];
            if s[3] == 0 && s[..3] == [0, 0, 0] {
                continue;
            }
            let (cx, cy) = (origin[0] + x as i32, origin[1] + y as i32);
            let coord = TileCoord::containing(cx, cy);
            let (ox, oy) = coord.origin();
            let j = (((cy - oy) as u32 * TILE_SIZE + (cx - ox) as u32) * 4) as usize;
            let tile = out.get_or_create_mut(coord);
            let d = &mut tile.bytes_mut()[j..j + 4];
            let keep = 255 - s[3] as u32;
            for k in 0..4 {
                d[k] = (s[k] as u32 + (d[k] as u32 * keep + 127) / 255).min(255) as u8;
            }
        }
    }
    out
}

/// The whole commit: `floating` placed over `rest`.
pub fn commit(rest: &TileStore, floating: &Image, placement: &Placement, filter: Interpolation, canvas: (u32, u32)) -> TileStore {
    match render_floating(floating, placement, filter, canvas) {
        Some((image, origin)) => composite_over(rest, &image, origin),
        None => rest.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::warp::MeshWarp;

    fn solid(w: u32, h: u32, px: [u8; 4]) -> Image {
        Image { width: w, height: h, rgba: (0..w * h).flat_map(|_| px).collect() }
    }

    #[test]
    fn matrix_commit_lands_over_the_rest() {
        let mut rest = TileStore::new();
        rest = composite_over(&rest, &solid(4, 4, [0, 0, 255, 255]), [0, 0]);
        let floating = solid(2, 2, [255, 0, 0, 255]);
        let out = commit(&rest, &floating, &Placement::Matrix(Mat3::translate(1.0, 1.0)), Interpolation::Nearest, (8, 8));
        assert_eq!(out.pixel(0, 0), [0, 0, 255, 255]);
        assert_eq!(out.pixel(1, 1), [255, 0, 0, 255]);
        assert_eq!(out.pixel(2, 2), [255, 0, 0, 255]);
        assert_eq!(out.pixel(3, 3), [0, 0, 255, 255]);
        // Outside the canvas: clipped, not an error.
        let gone = commit(&rest, &floating, &Placement::Matrix(Mat3::translate(50.0, 0.0)), Interpolation::Nearest, (8, 8));
        assert_eq!(gone.pixel(0, 0), rest.pixel(0, 0));
    }

    #[test]
    fn identity_mesh_matches_the_image() {
        let image = Image { width: 6, height: 4, rgba: (0..24u32).flat_map(|i| [(i * 10) as u8, 0, 0, 255]).collect() };
        let warp = MeshWarp::new([0.0, 0.0, 6.0, 4.0], 2, 2).tessellate(2);
        let placement = Placement::Mesh { positions: warp.positions, sources: warp.uvs, indices: warp.indices };
        let (out, origin) = render_floating(&image, &placement, Interpolation::Nearest, (16, 16)).unwrap();
        assert_eq!(origin, [0, 0]);
        assert_eq!(out, image, "every pixel covered exactly once");
    }
}
