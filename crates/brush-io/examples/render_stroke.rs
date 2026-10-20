//! Renders one test stroke with an imported preset to a PNG, for checking how
//! a brush looks without the app.
//!
//! ```text
//! cargo run -p painter-brush-io --example render_stroke -- tools.todb "Mechanical pencil" out.png
//! ```
//! The stroke is an S curve whose pressure rises from 0.1 to 1 and back.

use painter_core::raster::paint_stroke;
use painter_core::{blend, StrokeEngine, StrokePoint, TileStore};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [file, name, out] = &args[..] else {
        eprintln!("usage: render_stroke <brush file> <preset name> <out.png>");
        std::process::exit(2);
    };
    let presets = brush_io::import(std::path::Path::new(file)).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(1);
    });
    let Some(preset) = presets.iter().find(|p| p.name() == name) else {
        eprintln!("no preset named {name:?}");
        std::process::exit(1);
    };
    let (w, h) = (900u32, 260u32);
    let points: Vec<StrokePoint> = (0..=400)
        .map(|i| {
            let t = i as f32 / 400.0;
            let pressure = 0.1 + 0.9 * (std::f32::consts::PI * t).sin();
            let x = 60.0 + t * 780.0;
            let y = 130.0 + 70.0 * (t * std::f32::consts::TAU).sin();
            StrokePoint::new(x, y, pressure).at_time(t * 1.5)
        })
        .collect();
    let mut engine = StrokeEngine::with_seed(&preset.settings, 1);
    let mut dabs = Vec::new();
    for p in points {
        dabs.extend(engine.add(p));
    }
    dabs.extend(engine.finish());
    let mut tiles = TileStore::new();
    paint_stroke(&mut tiles, &dabs, [0.1, 0.1, 0.12], &preset.settings, (w, h), false, None);

    // Composite over white paper.
    let mut image = image::RgbaImage::new(w, h);
    for (x, y, px) in image.enumerate_pixels_mut() {
        let s = blend::to_f32(tiles.pixel(x as i32, y as i32));
        let c = |i: usize| ((s[i] + (1.0 - s[3])) * 255.0).round().clamp(0.0, 255.0) as u8;
        *px = image::Rgba([c(0), c(1), c(2), 255]);
    }
    image.save(out).expect("write PNG");
    let thin = dabs.iter().filter(|d| d.thickness < 0.5).count();
    let mirrored = dabs.iter().filter(|d| d.mirror).count();
    println!("{} dabs, {thin} flattened below 50%, {mirrored} mirrored → {out}", dabs.len());
}
