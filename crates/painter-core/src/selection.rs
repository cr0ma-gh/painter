//! Selections: a sparse coverage mask over the canvas (tile alpha = selected).
//!
//! `None` on the document means "no selection" (everything is editable). A
//! selection that ends up empty still exists and protects every pixel.

use crate::tile::{Tile, TileCoord, TileStore, TILE_SIZE};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SelectionOp {
    #[default]
    Replace,
    Add,
    Subtract,
    Intersect,
}

impl SelectionOp {
    pub const ALL: [SelectionOp; 4] = [SelectionOp::Replace, SelectionOp::Add, SelectionOp::Subtract, SelectionOp::Intersect];

    pub fn name(self) -> &'static str {
        match self {
            SelectionOp::Replace => "New",
            SelectionOp::Add => "Add",
            SelectionOp::Subtract => "Subtract",
            SelectionOp::Intersect => "Intersect",
        }
    }
}

/// Mask pixels are premultiplied white, so every channel equals the coverage.
fn mask_px(a: u8) -> [u8; 4] {
    [a, a, a, a]
}

fn tiles_across(canvas: (u32, u32)) -> (i32, i32) {
    (canvas.0.div_ceil(TILE_SIZE) as i32, canvas.1.div_ceil(TILE_SIZE) as i32)
}

/// Builds a mask from a per-pixel coverage function over a pixel bounding box.
pub fn mask_from_fn(canvas: (u32, u32), bbox: [i32; 4], coverage: impl Fn(i32, i32) -> f32) -> TileStore {
    let mut out = TileStore::new();
    let x0 = bbox[0].max(0);
    let y0 = bbox[1].max(0);
    let x1 = bbox[2].min(canvas.0 as i32 - 1);
    let y1 = bbox[3].min(canvas.1 as i32 - 1);
    if x0 > x1 || y0 > y1 {
        return out;
    }
    let (t0, t1) = (TileCoord::containing(x0, y0), TileCoord::containing(x1, y1));
    for ty in t0.y..=t1.y {
        for tx in t0.x..=t1.x {
            let coord = TileCoord::new(tx, ty);
            let (ox, oy) = coord.origin();
            let mut tile = Tile::empty();
            for py in y0.max(oy)..=y1.min(oy + TILE_SIZE as i32 - 1) {
                for px in x0.max(ox)..=x1.min(ox + TILE_SIZE as i32 - 1) {
                    let a = (coverage(px, py).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    if a > 0 {
                        tile.set_pixel((px - ox) as u32, (py - oy) as u32, mask_px(a));
                    }
                }
            }
            out.insert(coord, tile);
        }
    }
    out
}

/// Axis-aligned rectangle between two corners (any order), snapped to pixels.
pub fn rect_mask(a: [f32; 2], b: [f32; 2], canvas: (u32, u32)) -> TileStore {
    let (x0, x1) = (a[0].min(b[0]).round() as i32, a[0].max(b[0]).round() as i32 - 1);
    let (y0, y1) = (a[1].min(b[1]).round() as i32, a[1].max(b[1]).round() as i32 - 1);
    mask_from_fn(canvas, [x0, y0, x1, y1], |_, _| 1.0)
}

/// Ellipse inscribed in the rectangle between two corners, anti-aliased.
pub fn ellipse_mask(a: [f32; 2], b: [f32; 2], canvas: (u32, u32)) -> TileStore {
    let (cx, cy) = ((a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5);
    let (rx, ry) = (((a[0] - b[0]).abs() * 0.5).max(0.5), ((a[1] - b[1]).abs() * 0.5).max(0.5));
    let bbox = [(cx - rx).floor() as i32, (cy - ry).floor() as i32, (cx + rx).ceil() as i32, (cy + ry).ceil() as i32];
    mask_from_fn(canvas, bbox, |px, py| {
        let (dx, dy) = ((px as f32 + 0.5 - cx) / rx, (py as f32 + 0.5 - cy) / ry);
        let d = dx.hypot(dy);
        // Distance to the edge in pixels, approximately, for a 1 px ramp.
        (1.0 - d) * rx.min(ry) + 0.5
    })
}

/// Polygon (lasso) with even-odd filling and 4× vertical, exact horizontal anti-aliasing.
pub fn polygon_mask(points: &[[f32; 2]], canvas: (u32, u32)) -> TileStore {
    if points.len() < 3 {
        return TileStore::new();
    }
    let min_y = points.iter().map(|p| p[1]).fold(f32::MAX, f32::min).floor().max(0.0) as i32;
    let max_y = points.iter().map(|p| p[1]).fold(f32::MIN, f32::max).ceil().min(canvas.1 as f32) as i32;
    let min_x = points.iter().map(|p| p[0]).fold(f32::MAX, f32::min).floor().max(0.0) as i32;
    let max_x = points.iter().map(|p| p[0]).fold(f32::MIN, f32::max).ceil().min(canvas.0 as f32) as i32;
    if min_x >= max_x || min_y >= max_y {
        return TileStore::new();
    }
    let width = (max_x - min_x) as usize;
    let mut out = TileStore::new();
    let mut row = vec![0f32; width + 1];
    const SUB: usize = 4;
    for py in min_y..max_y {
        row.iter_mut().for_each(|v| *v = 0.0);
        for s in 0..SUB {
            let y = py as f32 + (s as f32 + 0.5) / SUB as f32;
            let mut xs: Vec<f32> = Vec::new();
            for i in 0..points.len() {
                let (p, q) = (points[i], points[(i + 1) % points.len()]);
                if (p[1] <= y) != (q[1] <= y) {
                    xs.push(p[0] + (y - p[1]) / (q[1] - p[1]) * (q[0] - p[0]));
                }
            }
            xs.sort_by(f32::total_cmp);
            for span in xs.as_chunks::<2>().0 {
                let a = (span[0] - min_x as f32).clamp(0.0, width as f32);
                let b = (span[1] - min_x as f32).clamp(0.0, width as f32);
                let (ia, ib) = (a.floor() as usize, b.floor() as usize);
                let w = 1.0 / SUB as f32;
                if ia == ib {
                    row[ia] += (b - a) * w;
                } else {
                    row[ia] += (ia as f32 + 1.0 - a) * w;
                    for v in &mut row[ia + 1..ib] {
                        *v += w;
                    }
                    row[ib] += (b - ib as f32) * w;
                }
            }
        }
        for (i, &v) in row[..width].iter().enumerate() {
            let a = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            if a > 0 {
                let (x, y) = (min_x + i as i32, py);
                let coord = TileCoord::containing(x, y);
                let (ox, oy) = coord.origin();
                out.get_or_create_mut(coord).set_pixel((x - ox) as u32, (y - oy) as u32, mask_px(a));
            }
        }
    }
    out
}

/// Every pixel selected.
pub fn select_all(canvas: (u32, u32)) -> TileStore {
    mask_from_fn(canvas, [0, 0, canvas.0 as i32 - 1, canvas.1 as i32 - 1], |_, _| 1.0)
}

/// Combines a new shape with the current selection.
pub fn combine(current: Option<&TileStore>, shape: TileStore, op: SelectionOp, canvas: (u32, u32)) -> TileStore {
    let Some(current) = current else {
        return match op {
            SelectionOp::Replace | SelectionOp::Add | SelectionOp::Intersect => shape,
            // Subtracting from "no selection" (= everything) leaves the inverse.
            SelectionOp::Subtract => combine(Some(&select_all(canvas)), shape, op, canvas),
        };
    };
    let f: fn(u8, u8) -> u8 = match op {
        SelectionOp::Replace => return shape,
        SelectionOp::Add => |a, b| a.max(b),
        SelectionOp::Subtract => |a, b| ((a as u32 * (255 - b as u32) + 127) / 255) as u8,
        SelectionOp::Intersect => |a, b| a.min(b),
    };
    let mut coords: Vec<TileCoord> = current.iter().map(|(c, _)| c).chain(shape.iter().map(|(c, _)| c)).collect();
    coords.sort();
    coords.dedup();
    let mut out = TileStore::new();
    let empty = Tile::empty();
    for coord in coords {
        let a = current.get(coord).map_or(&empty, |t| t);
        let b = shape.get(coord).map_or(&empty, |t| t);
        let mut tile = Tile::empty();
        for (o, (pa, pb)) in tile.bytes_mut().as_chunks_mut::<4>().0.iter_mut().zip(a.bytes().as_chunks::<4>().0.iter().zip(b.bytes().as_chunks::<4>().0)) {
            *o = mask_px(f(pa[3], pb[3]));
        }
        out.insert(coord, tile);
    }
    out
}

/// Selects everything that was not selected.
pub fn invert(current: Option<&TileStore>, canvas: (u32, u32)) -> TileStore {
    let Some(current) = current else {
        return TileStore::new();
    };
    let (tx, ty) = tiles_across(canvas);
    let mut out = TileStore::new();
    // Unselected tiles inside the canvas all invert to the same full tile:
    // share one, so inverting a small selection doesn't cost a tile per canvas tile.
    let mut full = Tile::empty();
    for ly in 0..TILE_SIZE {
        for lx in 0..TILE_SIZE {
            full.set_pixel(lx, ly, mask_px(255));
        }
    }
    let full = std::sync::Arc::new(full);
    for y in 0..ty {
        for x in 0..tx {
            let coord = TileCoord::new(x, y);
            let (ox, oy) = coord.origin();
            let src = current.get(coord);
            let interior = ox + TILE_SIZE as i32 <= canvas.0 as i32 && oy + TILE_SIZE as i32 <= canvas.1 as i32;
            if src.is_none() && interior {
                out.insert_shared(coord, full.clone());
                continue;
            }
            let mut tile = Tile::empty();
            for ly in 0..TILE_SIZE {
                for lx in 0..TILE_SIZE {
                    if ox + lx as i32 >= canvas.0 as i32 || oy + ly as i32 >= canvas.1 as i32 {
                        continue;
                    }
                    let a = src.map_or(0, |t| t.pixel(lx, ly)[3]);
                    tile.set_pixel(lx, ly, mask_px(255 - a));
                }
            }
            out.insert(coord, tile);
        }
    }
    out
}

/// A layer's opacity as a selection, within the canvas: Photoshop's
/// Ctrl+click on a layer thumbnail (Clip Studio's Selection from Layer).
pub fn from_alpha(tiles: &TileStore, canvas: (u32, u32)) -> TileStore {
    let mut out = TileStore::new();
    for (coord, tile) in tiles.iter() {
        let mut sel = Tile::empty();
        for (o, p) in sel.bytes_mut().as_chunks_mut::<4>().0.iter_mut().zip(tile.bytes().as_chunks::<4>().0) {
            *o = mask_px(p[3]);
        }
        out.insert(coord, sel);
    }
    out.clip_to_canvas(canvas.0, canvas.1);
    out
}

/// Selected pixel-space bounds `[x0, y0, x1, y1]` (inclusive), if anything is selected.
pub fn bounds(selection: &TileStore) -> Option<[i32; 4]> {
    let mut b: Option<[i32; 4]> = None;
    for (coord, tile) in selection.iter() {
        let (ox, oy) = coord.origin();
        for (i, px) in tile.bytes().as_chunks::<4>().0.iter().enumerate() {
            if px[3] > 0 {
                let (x, y) = (ox + (i as u32 % TILE_SIZE) as i32, oy + (i as u32 / TILE_SIZE) as i32);
                b = Some(b.map_or([x, y, x, y], |b| [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)]));
            }
        }
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_and_combine() {
        let canvas = (100, 100);
        let a = rect_mask([10.0, 10.0], [50.0, 50.0], canvas);
        assert_eq!(a.pixel(10, 10)[3], 255);
        assert_eq!(a.pixel(50, 50)[3], 0);
        let b = rect_mask([30.0, 30.0], [70.0, 70.0], canvas);
        let union = combine(Some(&a), b.clone(), SelectionOp::Add, canvas);
        assert_eq!(union.pixel(60, 60)[3], 255);
        let cut = combine(Some(&a), b.clone(), SelectionOp::Subtract, canvas);
        assert_eq!(cut.pixel(40, 40)[3], 0);
        assert_eq!(cut.pixel(15, 15)[3], 255);
        let both = combine(Some(&a), b, SelectionOp::Intersect, canvas);
        assert_eq!(bounds(&both), Some([30, 30, 49, 49]));
    }

    #[test]
    fn polygon_area_is_close_to_exact() {
        let canvas = (64, 64);
        let tri = polygon_mask(&[[0.0, 0.0], [40.0, 0.0], [0.0, 40.0]], canvas);
        let area: f32 = tri.iter().flat_map(|(_, t)| t.bytes().as_chunks::<4>().0.iter().map(|p| p[3] as f32 / 255.0).collect::<Vec<_>>()).sum();
        assert!((area - 800.0).abs() < 8.0, "area {area}");
    }

    #[test]
    fn invert_and_ellipse() {
        let canvas = (40, 30);
        let e = ellipse_mask([0.0, 0.0], [40.0, 30.0], canvas);
        assert_eq!(e.pixel(20, 15)[3], 255);
        assert_eq!(e.pixel(0, 0)[3], 0);
        let inv = invert(Some(&e), canvas);
        assert_eq!(inv.pixel(0, 0)[3], 255);
        assert_eq!(inv.pixel(20, 15)[3], 0);
        assert!(invert(None, canvas).is_empty());
    }

    #[test]
    fn layer_alpha_becomes_a_selection_inside_the_canvas() {
        let mut tiles = TileStore::new();
        tiles.get_or_create_mut(TileCoord::new(0, 0)).set_pixel(3, 4, [10, 20, 30, 90]);
        tiles.get_or_create_mut(TileCoord::new(5, 5)).set_pixel(0, 0, [255; 4]);
        let sel = from_alpha(&tiles, (40, 30));
        assert_eq!(sel.pixel(3, 4), [90; 4]);
        assert_eq!(sel.pixel(0, 0)[3], 0);
        assert!(sel.get(TileCoord::new(5, 5)).is_none(), "off the canvas");
    }
}
