//! Exports a demo document to SVG and renders that SVG back to PNG with
//! resvg, to check the export by eye.
//!
//! ```text
//! cargo run -p painter-vector-export --example svg_preview -- out.svg out.png
//! ```

use painter_core::{BrushSettings, Document, Gradient, GradientShape, Layer, LayerKind, Mask, StrokePoint, TileStore, Tone, VectorStroke};
use resvg::{tiny_skia, usvg};
use usvg::TreeParsing;
use vector_export::{export_svg, ExportOptions};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [svg_path, png_path] = &args[..] else {
        eprintln!("usage: svg_preview <out.svg> <out.png>");
        std::process::exit(2);
    };
    let (w, h) = (400u32, 240u32);
    let mut doc = Document::new(w, h);

    // "Scanned" line art: a ring and a slanted bar, antialiased, on the first layer.
    let rgba: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            let (x, y) = ((i % w) as f32 + 0.5, (i / w) as f32 + 0.5);
            let ring = (((x - 110.0).powi(2) + (y - 120.0).powi(2)).sqrt() - 70.0).abs() - 9.0;
            let bar = ((x - 0.4 * y - 170.0).abs() - 6.0).max((y - 120.0).abs() - 90.0);
            let a = (0.5 - ring.min(bar)).clamp(0.0, 1.0);
            let a = (a * 255.0) as u8;
            [(30 * a as u32 / 255) as u8, (40 * a as u32 / 255) as u8, (90 * a as u32 / 255) as u8, a]
        })
        .collect();
    let base = doc.active;
    {
        let layer = doc.find_mut(base).unwrap();
        layer.name = "Line art".into();
        layer.kind = LayerKind::Raster(TileStore::from_rgba(w, h, &rgba));
        // Mask hides a band through the ring.
        let hidden: Vec<u8> = (0..w * h).flat_map(|i| if (100..115).contains(&(i / w)) { [0, 0, 0, 255] } else { [0; 4] }).collect();
        layer.mask = Some(Mask { tiles: TileStore::from_rgba(w, h, &hidden), enabled: true });
    }
    // A gradient clipped to the line art.
    let gid = doc.alloc_id();
    let mut shade = Layer::gradient(gid, "Shade", Gradient { start: [40.0, 0.0], end: [240.0, 0.0], from: [255, 60, 0, 255], to: [0, 120, 255, 255], shape: GradientShape::Linear });
    shade.clip = true;
    doc.layers.push(shade);
    // Pressure-tapered vector strokes.
    let vid = doc.alloc_id();
    let mut lines = Layer::vector(vid, "Ink");
    if let LayerKind::Vector(v) = &mut lines.kind {
        for k in 0..3 {
            let points = (0..=40)
                .map(|i| {
                    let t = i as f32 / 40.0;
                    StrokePoint::new(230.0 + t * 150.0, 50.0 + k as f32 * 60.0 + (t * std::f32::consts::TAU).sin() * 20.0, (t * std::f32::consts::PI).sin().max(0.05))
                })
                .collect();
            v.strokes.push(VectorStroke { points, settings: BrushSettings { size: 14.0, stabilization: 0, ..BrushSettings::g_pen() }, color: [0.1, 0.1, 0.1], seed: k, engine: painter_core::VECTOR_ENGINE });
        }
    }
    doc.layers.push(lines);
    let tid = doc.alloc_id();
    let mut tone = Layer::tone(tid, "Tone", Tone { cell: 8.0, angle: 45.0, density: 0.2, color: [200, 0, 120] });
    tone.opacity = 0.35;
    doc.layers.push(tone);

    let report = export_svg(&doc, &ExportOptions::default()).expect("export");
    let svg = report.to_svg();
    std::fs::write(svg_path, &svg).expect("write svg");
    println!("{} paths, {} bytes, warnings: {:?}", report.paths, svg.len(), report.warnings);

    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).expect("parse svg");
    let rtree = resvg::Tree::from_usvg(&tree);
    let mut pixmap = tiny_skia::Pixmap::new(w, h).unwrap();
    rtree.render(tiny_skia::Transform::default(), &mut pixmap.as_mut());
    pixmap.save_png(png_path).expect("write png");
}
