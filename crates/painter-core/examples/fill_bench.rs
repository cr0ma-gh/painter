//! Times the fill pipeline on an A4 page at 600 dpi (4961 × 7016) of line
//! art: a grid of open rings, each with a small gap.
//!
//! cargo run --release -p painter-core --example fill_bench

use std::time::Instant;

use painter_core::fill_tool::{close_gaps, fill, open_mask_from_tiles, FillOptions};
use painter_core::tile::{TileCoord, TileStore};

fn main() {
    let (w, h) = (4961u32, 7016u32);
    let mut inks = TileStore::new();
    let t = Instant::now();
    // Rings of radius 220 px, 6 px thick, with a 6 px gap on the right.
    for cy in (300..h as i32 - 300).step_by(600) {
        for cx in (300..w as i32 - 300).step_by(600) {
            for y in cy - 230..=cy + 230 {
                for x in cx - 230..=cx + 230 {
                    let (dx, dy) = ((x - cx) as f32, (y - cy) as f32);
                    let d = (dx * dx + dy * dy).sqrt();
                    if (d - 220.0).abs() <= 3.0 && !(dx > 0.0 && dy.abs() < 3.0) {
                        let c = TileCoord::containing(x, y);
                        let (ox, oy) = c.origin();
                        inks.get_or_create_mut(c).set_pixel((x - ox) as u32, (y - oy) as u32, [0, 0, 0, 255]);
                    }
                }
            }
        }
    }
    println!("line art: {} tiles in {:?}", inks.len(), t.elapsed());
    let canvas = (w, h);
    let seed = (300, 300); // inside the first ring

    let t = Instant::now();
    let open = open_mask_from_tiles(&inks, canvas, seed, 16);
    println!("open mask        {:>8.1?}", t.elapsed());

    let t = Instant::now();
    let closed = close_gaps(&open.inverted(), 10);
    println!("gap closing      {:>8.1?}  ({} boundary px)", t.elapsed(), closed.count());

    for (label, options) in [
        ("fill, no gaps   ", FillOptions { gap: 0, ..FillOptions::default() }),
        ("fill, gap 10 px ", FillOptions { gap: 10, ..FillOptions::default() }),
        ("background, gap ", FillOptions { gap: 10, ..FillOptions::default() }),
    ] {
        let seed = if label.starts_with("background") { (5, 5) } else { seed };
        let t = Instant::now();
        let f = fill(&open, seed, &options).unwrap();
        let fill_time = t.elapsed();
        let t = Instant::now();
        let mask = f.region.to_mask();
        println!("{label} {:>8.1?}  + to tiles {:>7.1?}  ({} px, {} tiles)", fill_time, t.elapsed(), f.region.count(), mask.len());
    }
}
