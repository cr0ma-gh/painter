//! Renders the built-in Pencil with an upright and a leaning pen to a PNG,
//! for checking how tilt shapes the stroke (as Clip Studio's pencils).
//!
//! ```text
//! cargo run -p painter-brush-io --example tilt_strokes -- out.png
//! ```
//! Top: upright hatching. Middle: the same strokes with the pen leaning
//! towards the bottom right (full tilt). Bottom: a zigzag that starts upright
//! and leans more and more.

use painter_core::raster::paint_stroke;
use painter_core::{blend, BrushSettings, StrokeEngine, StrokePoint, TileStore};

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "tilt_strokes.png".into());
    let (w, h) = (700u32, 760u32);
    let settings = BrushSettings::pencil();
    let mut tiles = TileStore::new();
    let mut stroke = |from: (f32, f32), to: (f32, f32), tilt: f32| {
        let mut engine = StrokeEngine::with_seed(&settings, 7);
        let mut dabs = Vec::new();
        for i in 0..=120 {
            let t = i as f32 / 120.0;
            // Pressure rises and falls, as a quick hatch.
            let pressure = 0.15 + 0.85 * (std::f32::consts::PI * t).sin();
            let p = StrokePoint::new(from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t, pressure)
                .with_tilt(tilt)
                .with_azimuth(135.0)
                .at_time(t * 0.4);
            dabs.extend(engine.add(p));
        }
        dabs.extend(engine.finish());
        paint_stroke(&mut tiles, &dabs, [0.15, 0.15, 0.15], &settings, (w, h), false, None);
    };
    for i in 0..6 {
        let o = i as f32 * 12.0;
        stroke((30.0 + o, 280.0 - o * 0.3), (250.0 + o, 60.0 + o * 0.5), 0.0);
        stroke((90.0 + o, 380.0 - o * 0.3), (320.0 + o, 230.0 + o * 0.4), 1.0);
    }
    // The zigzag: upright at its start, fully leaning at its end.
    for i in 0..8 {
        let t = i as f32 / 7.0;
        let x = 200.0 + t * 400.0;
        stroke((x, 520.0 + t * 60.0), (x + 120.0, 320.0 + t * 120.0), t);
    }
    let mut image = image::RgbaImage::new(w, h);
    for (x, y, px) in image.enumerate_pixels_mut() {
        let s = blend::to_f32(tiles.pixel(x as i32, y as i32));
        let c = |i: usize| ((s[i] + (1.0 - s[3])) * 255.0).round().clamp(0.0, 255.0) as u8;
        *px = image::Rgba([c(0), c(1), c(2), 255]);
    }
    image.save(&out).expect("write PNG");
    println!("→ {out}");
}
