#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod adjust_ui;
mod advanced_tool;
mod app;
mod brush_library;
mod clipboard_image;
mod color_panels;
mod search_layer;
mod canvas;
mod comic_ui;
mod curve;
mod dialogs;
mod document_manager;
mod eyedropper;
mod files;
mod icons;
mod layers_ui;
mod lines;
mod ios_ui;
mod modifier_keys;
mod panels;
mod previews;
mod rulers_ui;
mod print_ui;
mod scan_source;
mod scene_ui;
mod tablet;
mod theme;
mod toolbar;
mod tools;
mod transform_ui;
mod view;
mod widgets;
#[cfg(windows)]
mod wintab;
mod workspace_ui;

/// iPadOS: a panic is written to Documents/painter-crash.txt (visible in the
/// Files app), the only way to see why the app closed without a Mac.
fn install_crash_log() {
    let Some(dir) = files::documents_dir() else { return };
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let _ = std::fs::write(dir.join("painter-crash.txt"), format!("{info}\n\n{backtrace}"));
        previous(info);
    }));
}

fn main() -> eframe::Result {
    install_crash_log();
    // iPadOS: the Materials folder appears in Files, ready for Clip Studio materials.
    let _ = files::materials_dir();
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Painter")
            .with_inner_size([1400.0, 860.0])
            .with_min_inner_size([900.0, 600.0])
            .with_maximized(true),
        ..Default::default()
    };
    let file = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    eframe::run_native(
        "Painter",
        options,
        Box::new(move |cc| {
            let mut app = app::PainterApp::new(cc);
            if let Some(path) = file {
                app.open_any(path);
            }
            Ok(Box::new(app))
        }),
    )
}
