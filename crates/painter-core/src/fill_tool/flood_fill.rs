//! Scanline flood fill over byte masks, square dilation in linear time, and
//! the fill itself: flood, gap bridging, anti-halo expansion.

use super::gap_detection::close_gaps;
use super::{par_rows, Bitmap, FillOptions};

/// What a click fills.
#[derive(Clone, Debug)]
pub struct Fill {
    pub region: Bitmap,
    /// Whether gap closing was used (the click wasn't inside a gap or a
    /// spot narrower than the gap size, where it's skipped).
    pub gaps_closed: bool,
}

/// The fill for a click at `seed` over the `open` pixels (see
/// [`super::reference_mask`]). `None` when the click is outside the canvas
/// or on a pixel the fill may not cover.
pub fn fill(open: &Bitmap, seed: (i32, i32), options: &FillOptions) -> Option<Fill> {
    let (w, h) = (open.width as i32, open.height as i32);
    if seed.0 < 0 || seed.1 < 0 || seed.0 >= w || seed.1 >= h || !open.get(seed.0 as usize, seed.1 as usize) {
        return None;
    }
    let seed = (seed.0 as usize, seed.1 as usize);
    let mut gaps_closed = false;
    let mut region = if !options.contiguous {
        open.clone()
    } else if options.gap > 0 {
        // Flood the area the closed boundary encloses, then let the fill
        // into the bridges and corners the closing added: those touch this
        // area, while the far side of each bridge stays out.
        let closed = close_gaps(&open.inverted(), options.gap);
        if closed.get(seed.0, seed.1) {
            flood(open, seed)
        } else {
            gaps_closed = true;
            let mut region = flood(&closed.inverted(), seed);
            let mut bridges = open.clone();
            bridges.and(&closed);
            grow_within(&mut region, &bridges);
            region
        }
    } else {
        flood(open, seed)
    };
    match options.expand {
        e if e > 0 => {
            // Only under what stopped the fill (line art, its anti-aliasing),
            // never into open areas the fill didn't reach.
            let mut grown = dilate(&region, e as usize);
            grown.and_not(open);
            for (r, g) in region.data.iter_mut().zip(&grown.data) {
                *r |= *g;
            }
        }
        e if e < 0 => region = erode(&region, (-e) as usize),
        _ => {}
    }
    Some(Fill { region, gaps_closed })
}

/// The 4-connected area of `open` around `seed`.
pub fn flood(open: &Bitmap, seed: (usize, usize)) -> Bitmap {
    let mut region = Bitmap::new(open.width, open.height);
    if open.get(seed.0, seed.1) {
        fill_spans(open, &mut region, vec![seed]);
    }
    region
}

/// Grows `region` through `allowed` pixels (any distance), 4-connected.
pub fn grow_within(region: &mut Bitmap, allowed: &Bitmap) {
    let (w, h) = (region.width, region.height);
    let mut seeds = Vec::new();
    for y in 0..h {
        let row = y * w;
        for x in 0..w {
            let i = row + x;
            if allowed.data[i] == 0 || region.data[i] != 0 {
                continue;
            }
            let touches = (x > 0 && region.data[i - 1] != 0)
                || (x + 1 < w && region.data[i + 1] != 0)
                || (y > 0 && region.data[i - w] != 0)
                || (y + 1 < h && region.data[i + w] != 0);
            if touches {
                seeds.push((x, y));
            }
        }
    }
    fill_spans(allowed, region, seeds);
}

/// Span (scanline) flood: fills runs of `open` pixels not yet in `region`
/// from each seed, queueing one seed per run in the rows above and below.
fn fill_spans(open: &Bitmap, region: &mut Bitmap, mut stack: Vec<(usize, usize)>) {
    let (w, h) = (open.width, open.height);
    let free = |region: &Bitmap, i: usize| open.data[i] != 0 && region.data[i] == 0;
    while let Some((x, y)) = stack.pop() {
        let row = y * w;
        if !free(region, row + x) {
            continue;
        }
        let mut left = x;
        while left > 0 && free(region, row + left - 1) {
            left -= 1;
        }
        let mut right = x;
        while right + 1 < w && free(region, row + right + 1) {
            right += 1;
        }
        region.data[row + left..=row + right].fill(1);
        for ny in [y.wrapping_sub(1), y + 1] {
            if ny >= h {
                continue;
            }
            let nrow = ny * w;
            let mut px = left;
            while px <= right {
                if free(region, nrow + px) {
                    stack.push((px, ny));
                    // One seed per run: skip to its end.
                    while px <= right && free(region, nrow + px) {
                        px += 1;
                    }
                }
                px += 1;
            }
        }
    }
}

/// Square dilation of radius `r` (a `2r + 1` square), linear in the pixel
/// count whatever `r`: a running window sum along rows, then down columns.
pub fn dilate(src: &Bitmap, r: usize) -> Bitmap {
    let (w, h) = (src.width, src.height);
    if r == 0 || w == 0 || h == 0 {
        return src.clone();
    }
    // Rows: prefix sums per row.
    let mut horizontal = vec![0u8; w * h];
    par_rows(&mut horizontal, w, |y0, rows| {
        let mut prefix = vec![0u32; w + 1];
        for (k, out) in rows.chunks_mut(w).enumerate() {
            let line = &src.data[(y0 + k) * w..(y0 + k + 1) * w];
            for x in 0..w {
                prefix[x + 1] = prefix[x] + line[x] as u32;
            }
            for (x, o) in out.iter_mut().enumerate() {
                *o = (prefix[(x + r + 1).min(w)] > prefix[x.saturating_sub(r)]) as u8;
            }
        }
    });
    // Columns: a window count per column, slid down the rows.
    let mut counts = vec![0u32; w];
    for row in horizontal.chunks(w).take(r.min(h - 1) + 1) {
        for (c, &v) in counts.iter_mut().zip(row) {
            *c += v as u32;
        }
    }
    let mut out = Bitmap::new(w, h);
    for y in 0..h {
        for (o, &c) in out.data[y * w..(y + 1) * w].iter_mut().zip(&counts) {
            *o = (c > 0) as u8;
        }
        if y + r + 1 < h {
            for (c, &v) in counts.iter_mut().zip(&horizontal[(y + r + 1) * w..(y + r + 2) * w]) {
                *c += v as u32;
            }
        }
        if y >= r {
            for (c, &v) in counts.iter_mut().zip(&horizontal[(y - r) * w..(y - r + 1) * w]) {
                *c -= v as u32;
            }
        }
    }
    out
}

/// Square erosion of radius `r`; outside the canvas counts as set, so
/// shapes touching the edge aren't eaten from it.
pub fn erode(src: &Bitmap, r: usize) -> Bitmap {
    dilate(&src.inverted(), r).inverted()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `w` × `h` open canvas with closed (line) pixels where `line(x, y)`.
    pub(crate) fn canvas(w: usize, h: usize, line: impl Fn(usize, usize) -> bool) -> Bitmap {
        let mut b = Bitmap::filled(w, h);
        for y in 0..h {
            for x in 0..w {
                if line(x, y) {
                    b.set(x, y, false);
                }
            }
        }
        b
    }

    #[test]
    fn flood_stops_at_lines_and_counts_match() {
        // A ring: open inside and outside, a 1 px line between.
        let open = canvas(60, 60, |x, y| {
            let d = ((x as f32 - 30.0).powi(2) + (y as f32 - 30.0).powi(2)).sqrt();
            (d - 20.0).abs() < 0.7
        });
        let inside = flood(&open, (30, 30));
        assert!(inside.get(30, 45));
        assert!(!inside.get(2, 2));
        let outside = flood(&open, (2, 2));
        // Inside + outside + line = everything.
        assert_eq!(inside.count() + outside.count() + open.inverted().count(), 60 * 60);
    }

    #[test]
    fn dilation_matches_a_naive_square_filter() {
        let src = canvas(37, 23, |x, y| (x * 7 + y * 13) % 11 == 0);
        let src = src.inverted(); // sparse set pixels
        for r in [1, 2, 5] {
            let fast = dilate(&src, r);
            for y in 0..23usize {
                for x in 0..37usize {
                    let naive = (y.saturating_sub(r)..=(y + r).min(22))
                        .any(|yy| (x.saturating_sub(r)..=(x + r).min(36)).any(|xx| src.get(xx, yy)));
                    assert_eq!(fast.get(x, y), naive, "r {r} at {x},{y}");
                }
            }
        }
    }

    #[test]
    fn expansion_goes_under_lines_only() {
        // Vertical line at x = 20..=21 on a 40 × 40 canvas.
        let open = canvas(40, 40, |x, _| x == 20 || x == 21);
        let f = fill(&open, (5, 5), &FillOptions { expand: 1, ..FillOptions::default() }).unwrap();
        assert!(f.region.get(20, 10), "grows under the line");
        assert!(!f.region.get(21, 10), "one pixel only");
        let f = fill(&open, (5, 5), &FillOptions { expand: 5, ..FillOptions::default() }).unwrap();
        assert!(f.region.get(21, 10));
        assert!(!f.region.get(22, 10), "never past the line into open paper");
        let f = fill(&open, (5, 5), &FillOptions { expand: -2, ..FillOptions::default() }).unwrap();
        assert!(f.region.get(17, 10) && !f.region.get(18, 10), "inset");
        assert!(fill(&open, (20, 5), &FillOptions::default()).is_none(), "click on the line");
        let all = fill(&open, (5, 5), &FillOptions { contiguous: false, expand: 0, ..FillOptions::default() }).unwrap();
        assert!(all.region.get(30, 30));
    }
}
