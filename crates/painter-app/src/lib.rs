//! Painter: the application, as a library so each platform can start it its
//! own way (`main.rs` on the desktop and iPadOS, `android_main` in the
//! `painter-android` crate on Android).

mod adjust_ui;
#[cfg(target_os = "android")]
mod android;
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
mod texture_paint;
#[cfg(target_os = "linux")]
mod pen_x11;
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

/// Touch screens (iPadOS, Android): bigger controls, on-screen modifier keys,
/// the pen alone paints, Save As asks for a name in the app.
pub(crate) const MOBILE: bool = cfg!(any(target_os = "ios", target_os = "android"));

/// A panic is written to painter-crash.txt in the app's documents folder
/// (iPadOS, Android), the only way to see why the app closed there.
fn install_crash_log() {
    let Some(dir) = files::documents_dir() else { return };
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let _ = std::fs::write(dir.join("painter-crash.txt"), format!("{info}\n\n{backtrace}"));
        previous(info);
    }));
}

/// The window and renderer every platform starts with.
pub fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Painter")
            .with_app_id("painter")
            .with_inner_size([1400.0, 860.0])
            .with_min_inner_size([900.0, 600.0])
            .with_maximized(true),
        event_loop_builder: linux_x11(),
        ..Default::default()
    }
}

/// Linux: start on X11, through XWayland under Wayland, where pen tablets
/// report pressure and tilt (see `pen_x11`). `PAINTER_WAYLAND=1` keeps
/// native Wayland (no pen pressure there yet).
#[cfg(target_os = "linux")]
fn linux_x11() -> Option<eframe::EventLoopBuilderHook> {
    let has_x = std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty());
    let want_wayland = std::env::var_os("PAINTER_WAYLAND").is_some_and(|v| v != "0");
    if !has_x || want_wayland {
        return None;
    }
    Some(Box::new(|builder| {
        use winit::platform::x11::EventLoopBuilderExtX11;
        builder.with_x11();
    }))
}

#[cfg(not(target_os = "linux"))]
fn linux_x11() -> Option<eframe::EventLoopBuilderHook> {
    None
}

/// Runs the app until its window closes, opening `file` first if given.
pub fn run(options: eframe::NativeOptions, file: Option<std::path::PathBuf>) -> eframe::Result {
    install_crash_log();
    // iPadOS: the Materials folder appears in Files, ready for Clip Studio materials.
    let _ = files::materials_dir();
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

/// Android: the activity (its folders, keyboard and system services), before [`run`].
#[cfg(target_os = "android")]
pub fn set_android_app(app: winit::platform::android::activity::AndroidApp) {
    files::set_android_folders(app.internal_data_path(), app.external_data_path());
    android::set_app(app);
}
