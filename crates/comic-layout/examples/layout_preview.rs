//! Renders a template on a page format to PNG (panel borders over white),
//! with the trim and inner frame drawn as thin guides.
//!
//! ```text
//! cargo run -p painter-comic-layout --example layout_preview -- "Manga B5" "Manga dynamic 6" 150 out.png
//! ```

use comic_layout::frame_tool::BorderStyle;
use comic_layout::{ComicPage, PagePreset, Panel, PanelId, Template};
use painter_core::{io, Document, Layer};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [preset, template, dpi, out] = &args[..] else {
        eprintln!("usage: layout_preview <preset> <template> <dpi> <out.png>");
        std::process::exit(2);
    };
    let mut preset = PagePreset::builtin()
        .into_iter()
        .find(|p| &p.name == preset)
        .expect("no such preset");
    if preset.webtoon.is_none() {
        preset.dpi = dpi.parse().unwrap_or(150);
    }
    let template = Template::builtin()
        .into_iter()
        .find(|t| t.name == template)
        .expect("no such template");
    let geometry = preset.geometry();
    let gutter = preset.gutter();
    let mut doc = Document::new(geometry.canvas.0, geometry.canvas.1);
    let mut page = ComicPage::new(geometry.clone(), gutter);
    // Guides: trim (grey) and inner frame (blue), as thin borderless-style outlines.
    let guide = |rect, color| {
        let mut p = Panel::rect(PanelId(0), rect);
        p.border = BorderStyle {
            width: 1.5,
            color,
            dash: Some([6.0, 4.0]),
            ..BorderStyle::default()
        };
        comic_layout::panel_mask::border_tiles(&p, geometry.canvas)
    };
    let id = doc.alloc_id();
    doc.layers.push(Layer::raster_with(
        id,
        "trim",
        guide(geometry.trim, [150, 150, 150]),
    ));
    let id = doc.alloc_id();
    doc.layers.push(Layer::raster_with(
        id,
        "inner",
        guide(geometry.inner, [60, 120, 220]),
    ));
    let width = (geometry.canvas.0 as f64 / 300.0).max(2.0);
    for mut panel in template.for_page(&geometry, &gutter, 1) {
        panel.border.width = width;
        page.add_panel(&mut doc, panel);
    }
    io::export_png(&doc, std::path::Path::new(out)).expect("write png");
    let order: Vec<u32> = page.reading_order().iter().map(|p| p.0).collect();
    println!(
        "{} × {} px, {} panels, reading order {:?}",
        geometry.canvas.0,
        geometry.canvas.1,
        page.panels.len(),
        order
    );
}
