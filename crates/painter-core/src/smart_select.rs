//! Selections that find edges by themselves (no machine learning):
//! - the magnetic lasso's "live wire": the cheapest path between two points
//!   where cost is low along strong edges, so the path hugs outlines;
//! - object selection: inside a rectangle, everything that isn't the
//!   background reaching in from the rectangle's border.
//!
//! Both work on a premultiplied RGBA8 crop of the image (`w` × `h`, row-major)
//! in crop coordinates.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

fn luma(px: &[u8]) -> f32 {
    // Over white, so transparent and white read alike (lines on a blank layer).
    let a = px[3] as f32 / 255.0;
    let c = |v: u8| v as f32 / 255.0 + (1.0 - a);
    0.299 * c(px[0]) + 0.587 * c(px[1]) + 0.114 * c(px[2])
}

/// Edge strength in 0..=1 per pixel (Sobel on luminance over white, normalised).
pub fn edge_strength(rgba: &[u8], w: usize, h: usize) -> Vec<f32> {
    let l: Vec<f32> = rgba.as_chunks::<4>().0.iter().map(|c| luma(&c[..])).collect();
    let at = |x: isize, y: isize| l[(y.clamp(0, h as isize - 1) as usize) * w + x.clamp(0, w as isize - 1) as usize];
    let mut g = vec![0.0f32; w * h];
    let mut max = 1e-6f32;
    for y in 0..h as isize {
        for x in 0..w as isize {
            let gx =
                at(x + 1, y - 1) + 2.0 * at(x + 1, y) + at(x + 1, y + 1) - at(x - 1, y - 1) - 2.0 * at(x - 1, y) - at(x - 1, y + 1);
            let gy =
                at(x - 1, y + 1) + 2.0 * at(x, y + 1) + at(x + 1, y + 1) - at(x - 1, y - 1) - 2.0 * at(x, y - 1) - at(x + 1, y - 1);
            let v = gx.hypot(gy);
            g[y as usize * w + x as usize] = v;
            max = max.max(v);
        }
    }
    g.iter_mut().for_each(|v| *v /= max);
    g
}

/// The cheapest 8-connected path from `from` to `to` (inclusive) over
/// `edges` (from [`edge_strength`]): steps along strong edges cost little.
pub fn live_wire(edges: &[f32], w: usize, h: usize, from: (usize, usize), to: (usize, usize)) -> Vec<(usize, usize)> {
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let clamp = |(x, y): (usize, usize)| (x.min(w - 1), y.min(h - 1));
    let (from, to) = (clamp(from), clamp(to));
    let idx = |(x, y): (usize, usize)| y * w + x;
    let cost = |i: usize| 0.05 + (1.0 - edges[i]).powi(2);
    let mut dist = vec![f32::INFINITY; w * h];
    let mut prev = vec![usize::MAX; w * h];
    let mut heap = BinaryHeap::new();
    dist[idx(from)] = 0.0;
    // Distances as integers (×1024) so the heap has a total order.
    heap.push(Reverse((0u32, idx(from))));
    let goal = idx(to);
    while let Some(Reverse((d, i))) = heap.pop() {
        if i == goal {
            break;
        }
        let d = d as f32 / 1024.0;
        if d > dist[i] + 1e-3 {
            continue;
        }
        let (x, y) = (i % w, i / w);
        for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)] {
            let (nx, ny) = (x as isize + dx, y as isize + dy);
            if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                continue;
            }
            let j = ny as usize * w + nx as usize;
            let step = if dx != 0 && dy != 0 { std::f32::consts::SQRT_2 } else { 1.0 };
            let nd = d + step * cost(j);
            if nd < dist[j] {
                dist[j] = nd;
                prev[j] = i;
                heap.push(Reverse(((nd * 1024.0) as u32, j)));
            }
        }
    }
    let mut path = vec![to];
    let mut i = goal;
    while i != idx(from) {
        i = prev[i];
        if i == usize::MAX {
            return vec![from, to];
        }
        path.push((i % w, i / w));
    }
    path.reverse();
    path
}

/// Inside the crop, what isn't background: pixels the border's colours don't
/// reach by flooding through similar neighbours (`tolerance` 0..=255), with
/// holes filled and specks dropped. `None` when nothing stands out.
pub fn object_in_rect(rgba: &[u8], w: usize, h: usize, tolerance: u8) -> Option<Vec<bool>> {
    if w < 3 || h < 3 {
        return None;
    }
    let px = |i: usize| -> [i32; 4] { [0, 1, 2, 3].map(|k| rgba[i * 4 + k] as i32) };
    let close = |a: [i32; 4], b: [i32; 4]| (0..4).map(|k| (a[k] - b[k]).abs()).max().unwrap_or(0) <= tolerance as i32;
    // Background: flood from every border pixel through pixels like that border
    // pixel (not like the neighbour before, which would creep along soft edges).
    let mut background = vec![false; w * h];
    let mut stack: Vec<(usize, [i32; 4])> = Vec::new();
    for x in 0..w {
        stack.extend([x, (h - 1) * w + x].map(|i| (i, px(i))));
    }
    for y in 0..h {
        stack.extend([y * w, y * w + w - 1].map(|i| (i, px(i))));
    }
    for &(i, _) in &stack {
        background[i] = true;
    }
    while let Some((i, seed)) = stack.pop() {
        let (x, y) = (i % w, i / w);
        let mut visit = |j: usize| {
            if !background[j] && close(seed, px(j)) {
                background[j] = true;
                stack.push((j, seed));
            }
        };
        if x > 0 {
            visit(i - 1);
        }
        if x + 1 < w {
            visit(i + 1);
        }
        if y > 0 {
            visit(i - w);
        }
        if y + 1 < h {
            visit(i + w);
        }
    }
    let object: Vec<bool> = background.iter().map(|b| !b).collect();
    // Keep the object's parts (components larger than a speck), then fill their holes.
    let min_area = (w * h / 400).max(4);
    let mut keep = vec![false; w * h];
    let mut seen = vec![false; w * h];
    let mut any = false;
    for start in 0..w * h {
        if !object[start] || seen[start] {
            continue;
        }
        let mut part = vec![start];
        seen[start] = true;
        let mut k = 0;
        while k < part.len() {
            let i = part[k];
            k += 1;
            let (x, y) = (i % w, i / w);
            for (nx, ny) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)] {
                if nx < w && ny < h {
                    let j = ny * w + nx;
                    if object[j] && !seen[j] {
                        seen[j] = true;
                        part.push(j);
                    }
                }
            }
        }
        if part.len() >= min_area {
            any = true;
            for i in part {
                keep[i] = true;
            }
        }
    }
    if !any {
        return None;
    }
    // Holes: what the outside (flooded from the border through unkept pixels) doesn't reach.
    let mut outside = vec![false; w * h];
    let mut stack: Vec<usize> =
        (0..w).flat_map(|x| [x, (h - 1) * w + x]).chain((0..h).flat_map(|y| [y * w, y * w + w - 1])).filter(|&i| !keep[i]).collect();
    for &i in &stack {
        outside[i] = true;
    }
    while let Some(i) = stack.pop() {
        let (x, y) = (i % w, i / w);
        for (nx, ny) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)] {
            if nx < w && ny < h {
                let j = ny * w + nx;
                if !keep[j] && !outside[j] {
                    outside[j] = true;
                    stack.push(j);
                }
            }
        }
    }
    Some(outside.iter().map(|o| !o).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A white crop with a black ring of radius 20 around (32, 32).
    fn ring() -> (Vec<u8>, usize, usize) {
        let (w, h) = (64, 64);
        let mut img = vec![255u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let d = ((x as f32 - 32.0).powi(2) + (y as f32 - 32.0).powi(2)).sqrt();
                if (d - 20.0).abs() < 1.5 {
                    img[(y * w + x) * 4..][..3].copy_from_slice(&[0, 0, 0]);
                }
            }
        }
        (img, w, h)
    }

    #[test]
    fn the_wire_follows_the_ring() {
        let (img, w, h) = ring();
        let edges = edge_strength(&img, w, h);
        // From the ring's top to its right: the straight line cuts inside, the wire goes round.
        let path = live_wire(&edges, w, h, (32, 12), (52, 32));
        assert_eq!((path[0], *path.last().unwrap()), ((32, 12), (52, 32)));
        let far = path
            .iter()
            .map(|&(x, y)| (((x as f32 - 32.0).powi(2) + (y as f32 - 32.0).powi(2)).sqrt() - 20.0).abs())
            .fold(0.0, f32::max);
        assert!(far < 3.0, "the path strays {far} px from the ring");
    }

    #[test]
    fn objects_stand_out_from_the_background() {
        let (img, w, h) = ring();
        let mask = object_in_rect(&img, w, h, 24).unwrap();
        // The ring and its inside (a hole, filled), not the corners.
        assert!(mask[32 * w + 32]);
        assert!(mask[12 * w + 32]);
        assert!(!mask[2 * w + 2]);
        assert!(object_in_rect(&vec![255u8; w * h * 4], w, h, 24).is_none());
    }

    #[test]
    fn soft_edges_dont_let_the_background_in() {
        // A grey ellipse with a 3 px soft edge: neighbours along it differ by little.
        let (w, h) = (200usize, 150usize);
        let mut img = vec![255u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = ((x as f32 - 100.0) / 70.0, (y as f32 - 75.0) / 45.0);
                let edge = ((1.0 - (dx * dx + dy * dy).sqrt()) * 45.0 / 3.0).clamp(0.0, 1.0);
                let v = (255.0 - edge * 175.0) as u8;
                img[(y * w + x) * 4..][..3].copy_from_slice(&[v, v, v]);
            }
        }
        let mask = object_in_rect(&img, w, h, 32).unwrap();
        assert!(mask[75 * w + 100]);
        assert!(!mask[5 * w + 5]);
    }
}
