//! Comic panels drawn on a scanned page: finds the rectangles drawn as frames
//! and gives each panel's area, in reading order.
//!
//! The paper that isn't ink is split into connected regions. The one touching
//! the page's edge is the page around the panels; a closed region is a panel's
//! inside when it's big enough and ink surrounds it almost all the way round,
//! just outside its bounds (a frame), which leaves out closed shapes inside
//! the drawings. Each panel is then widened over its frame line. Works on a
//! smaller copy of the page, so a 600 dpi scan takes a moment.

use crate::ScannedImage;

/// A panel on the page, in pixels (frame included).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// How much of the page ink covers at a pixel: dark and opaque (line art with
/// transparent paper counts by its alpha).
fn ink(p: &[u8]) -> bool {
    let lum = (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32) / 255.0;
    (1.0 - lum) * (p[3] as f32 / 255.0) > 0.45
}

/// The panels drawn on `page`, in reading order: row by row from the top,
/// left to right (or right to left for manga, `right_to_left`).
pub fn find_panels(page: &ScannedImage, right_to_left: bool) -> Vec<PanelRect> {
    let (fw, fh) = (page.width as usize, page.height as usize);
    if fw < 16 || fh < 16 {
        return Vec::new();
    }
    // Work at most about 1000 px across: each cell is ink if any pixel in it is.
    let step = fw.max(fh).div_ceil(1000).max(1);
    let (w, h) = (fw.div_ceil(step), fh.div_ceil(step));
    let mut dark = vec![false; w * h];
    for y in 0..fh {
        for x in 0..fw {
            let i = (y * fw + x) * 4;
            if ink(&page.rgba[i..i + 4]) {
                dark[(y / step) * w + x / step] = true;
            }
        }
    }
    // Connected regions of paper (4-neighbours).
    let mut label = vec![u32::MAX; w * h];
    let mut regions: Vec<(usize, [usize; 4], bool)> = Vec::new(); // (area, [x0, y0, x1, y1], touches the edge)
    let mut stack = Vec::new();
    for start in 0..w * h {
        if dark[start] || label[start] != u32::MAX {
            continue;
        }
        let id = regions.len() as u32;
        let mut area = 0;
        let mut b = [usize::MAX, usize::MAX, 0, 0];
        let mut edge = false;
        label[start] = id;
        stack.push(start);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            area += 1;
            b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
            edge |= x == 0 || y == 0 || x == w - 1 || y == h - 1;
            let mut visit = |j: usize| {
                if !dark[j] && label[j] == u32::MAX {
                    label[j] = id;
                    stack.push(j);
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
        regions.push((area, b, edge));
    }
    let is_dark = |x: isize, y: isize| x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h && dark[y as usize * w + x as usize];
    // Ink found within `reach` cells outward from (x, y) in direction (dx, dy).
    let ink_out = |x: isize, y: isize, dx: isize, dy: isize, reach: isize| (1..=reach).any(|k| is_dark(x + dx * k, y + dy * k));
    let mut panels: Vec<[usize; 4]> = Vec::new();
    let min_side = (w.min(h) as f32 * 0.06) as usize;
    for (area, b, edge) in &regions {
        if *edge {
            continue;
        }
        let (bw, bh) = (b[2] - b[0] + 1, b[3] - b[1] + 1);
        if bw < min_side.max(4) || bh < min_side.max(4) || (*area as f32) < 0.3 * (bw * bh) as f32 {
            continue;
        }
        // A frame: ink just outside the bounds along (almost) every side.
        let reach = 3;
        let (x0, y0, x1, y1) = (b[0] as isize, b[1] as isize, b[2] as isize, b[3] as isize);
        let side = |hits: usize, n: usize| hits as f32 >= n as f32 * 0.85;
        let top = (x0..=x1).filter(|&x| ink_out(x, y0, 0, -1, reach)).count();
        let bottom = (x0..=x1).filter(|&x| ink_out(x, y1, 0, 1, reach)).count();
        let left = (y0..=y1).filter(|&y| ink_out(x0, y, -1, 0, reach)).count();
        let right = (y0..=y1).filter(|&y| ink_out(x1, y, 1, 0, reach)).count();
        if side(top, bw) && side(bottom, bw) && side(left, bh) && side(right, bh) {
            panels.push(*b);
        }
    }
    // A panel inside another (a frame drawn in a panel) belongs to it.
    let inside = |a: &[usize; 4], b: &[usize; 4]| a != b && a[0] >= b[0] && a[1] >= b[1] && a[2] <= b[2] && a[3] <= b[3];
    let outer: Vec<[usize; 4]> = panels.iter().filter(|a| !panels.iter().any(|b| inside(a, b))).copied().collect();
    // Widened over the frame line: outward while the line continues.
    let mut rects: Vec<PanelRect> = outer
        .iter()
        .map(|b| {
            let grow = |mut v: isize, d: isize, horizontal: bool| {
                for _ in 0..(w.max(h) / 30).max(2) {
                    let row_has_ink = if horizontal {
                        (b[1] as isize..=b[3] as isize).filter(|&y| is_dark(v + d, y)).count() * 2 > (b[3] - b[1] + 1)
                    } else {
                        (b[0] as isize..=b[2] as isize).filter(|&x| is_dark(x, v + d)).count() * 2 > (b[2] - b[0] + 1)
                    };
                    if !row_has_ink {
                        break;
                    }
                    v += d;
                }
                v
            };
            let x0 = grow(b[0] as isize, -1, true).max(0) as usize;
            let x1 = grow(b[2] as isize, 1, true).min(w as isize - 1) as usize;
            let y0 = grow(b[1] as isize, -1, false).max(0) as usize;
            let y1 = grow(b[3] as isize, 1, false).min(h as isize - 1) as usize;
            // Back to page pixels.
            let (px0, py0) = ((x0 * step) as u32, (y0 * step) as u32);
            let (px1, py1) = (((x1 + 1) * step).min(fw) as u32, ((y1 + 1) * step).min(fh) as u32);
            PanelRect { x: px0, y: py0, width: px1 - px0, height: py1 - py0 }
        })
        .collect();
    // Reading order: rows (panels whose middles overlap vertically), then across.
    rects.sort_by_key(|r| r.y);
    let mut rows: Vec<Vec<PanelRect>> = Vec::new();
    for r in rects {
        let mid = r.y + r.height / 2;
        match rows.last_mut() {
            Some(row) if row.iter().any(|o| mid >= o.y && mid <= o.y + o.height) => row.push(r),
            _ => rows.push(vec![r]),
        }
    }
    rows.into_iter()
        .flat_map(|mut row| {
            row.sort_by_key(|r| r.x);
            if right_to_left {
                row.reverse();
            }
            row
        })
        .collect()
}

/// The page with only `panel` kept: everything outside it transparent.
pub fn isolate(page: &ScannedImage, panel: &PanelRect) -> ScannedImage {
    let mut out = page.clone();
    for y in 0..page.height {
        for x in 0..page.width {
            if x < panel.x || y < panel.y || x >= panel.x + panel.width || y >= panel.y + panel.height {
                let i = ((y * page.width + x) * 4) as usize;
                out.rgba[i..i + 4].copy_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A white page with black rectangles drawn as frames (3 px lines).
    fn page(frames: &[(u32, u32, u32, u32)], w: u32, h: u32) -> ScannedImage {
        let mut rgba = vec![255u8; (w * h * 4) as usize];
        let mut dot = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            rgba[i..i + 3].copy_from_slice(&[0, 0, 0]);
        };
        for &(x0, y0, x1, y1) in frames {
            for t in 0..3 {
                for x in x0..=x1 {
                    dot(x, y0 + t);
                    dot(x, y1 - t);
                }
                for y in y0..=y1 {
                    dot(x0 + t, y);
                    dot(x1 - t, y);
                }
            }
        }
        ScannedImage::new(w, h, 300, rgba).unwrap()
    }

    #[test]
    fn finds_framed_panels_in_reading_order() {
        // Two panels on top, one wide below; a small closed "eye" inside the first.
        let mut p = page(&[(20, 20, 180, 140), (200, 20, 380, 140), (20, 160, 380, 280), (60, 60, 70, 70)], 400, 300);
        // A stroke inside the wide panel, crossing part of it.
        for x in 40..300 {
            let i = ((220 * 400 + x) * 4) as usize;
            p.rgba[i..i + 3].copy_from_slice(&[0, 0, 0]);
        }
        let panels = find_panels(&p, false);
        assert_eq!(panels.len(), 3, "{panels:?}");
        let near = |r: &PanelRect, x0: u32, y0: u32, x1: u32, y1: u32| {
            r.x.abs_diff(x0) <= 2 && r.y.abs_diff(y0) <= 2 && (r.x + r.width).abs_diff(x1 + 1) <= 2 && (r.y + r.height).abs_diff(y1 + 1) <= 2
        };
        assert!(near(&panels[0], 20, 20, 180, 140), "{:?}", panels[0]);
        assert!(near(&panels[1], 200, 20, 380, 140), "{:?}", panels[1]);
        assert!(near(&panels[2], 20, 160, 380, 280), "{:?}", panels[2]);
        // Manga order: right to left within a row.
        let manga = find_panels(&p, true);
        assert_eq!(manga[0], panels[1]);
        assert_eq!(manga[1], panels[0]);
        // Isolating keeps one panel.
        let one = isolate(&p, &panels[1]);
        assert_eq!(one.rgba[((30 * 400 + 30) * 4 + 3) as usize], 0, "outside is transparent");
        assert_eq!(one.rgba[((30 * 400 + 250) * 4 + 3) as usize], 255);
        // A page with no frames has no panels.
        assert!(find_panels(&page(&[], 200, 200), false).is_empty());
    }
}
