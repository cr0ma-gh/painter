//! Painter: the application, as a library so each platform can start it its
//! own way (`main.rs` on the desktop and iPadOS, `android_main` in the
//! `painter-android` crate on Android).

mod actions;
mod adjust_ui;
#[cfg(target_os = "android")]
mod android;
mod advanced_tool;
mod app;
mod brush_cursor;
mod brush_library;
mod clipboard_image;
mod chip_picker;
mod color_panels;
mod search_layer;
mod settings_reset;
mod shape_tool;
mod smart_shape;
mod stamp_tool;
mod canvas;
mod channels_ui;
mod comic_ui;
mod curve;
mod dev_shot;
mod dialogs;
mod document_manager;
mod eyedropper;
mod file_types;
mod file_object;
mod files;
mod gamut_selector;
mod history_ui;
mod icons;
mod layers_ui;
mod lines;
mod magnetic;
mod ios_ui;
mod modifier_keys;
mod panels;
mod nav_gizmo;
mod pose_ui;
mod previews;
mod ruler_bar;
mod recent;
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
mod updater;
mod vector_edit;
mod view;
mod welcome;
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

/// The ensō, for the window and the taskbar.
fn app_icon() -> eframe::egui::IconData {
    let png = include_bytes!("../../../packaging/icons/painter-256.png");
    match image::load_from_memory(png) {
        Ok(img) => {
            let img = img.to_rgba8();
            eframe::egui::IconData { width: img.width(), height: img.height(), rgba: img.into_raw() }
        }
        Err(_) => eframe::egui::IconData::default(),
    }
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
            .with_maximized(true)
            .with_icon(app_icon()),
        event_loop_builder: linux_x11(),
        wgpu_options: gpu_options(),
        ..Default::default()
    }
}

/// Android: which graphics API draws the app. Vulkan, except on ARM Mali
/// GPUs, whose Vulkan shader compiler crashes on the canvas shaders (seen on
/// a Pixel 9), and after a start that died while setting up the GPU: those
/// use OpenGL ES, which has its own compiler. The choice sticks once made.
/// Only the chosen API is started: the window can belong to one of them.
#[cfg(target_os = "android")]
fn gpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetupCreateNew};
    use eframe::wgpu;
    let dir = files::app_data_dir();
    let starting = dir.as_ref().map(|d| d.join("gpu-starting"));
    let use_gl = dir.as_ref().map(|d| d.join("gpu-use-opengl"));
    // The marker of the last start is still there: it never got past the GPU setup.
    if let (Some(starting), Some(use_gl)) = (&starting, &use_gl) {
        if starting.exists() {
            let _ = std::fs::write(use_gl, b"Vulkan crashed while starting; OpenGL ES is used.\n");
        }
        if let Some(d) = &dir {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(starting, b"");
    }
    // Decided before the window's surface exists: one backend only, since a
    // Vulkan surface takes the window and OpenGL ES can't attach to it after.
    let force_gl = use_gl.is_some_and(|p| p.exists());
    let vulkan = if force_gl { Vec::new() } else { vulkan_gpus() };
    let mali = vulkan.iter().any(|info| info.vendor == 0x13B5 || info.name.to_ascii_lowercase().contains("mali"));
    let backends = if force_gl || mali || vulkan.is_empty() { wgpu::Backends::GL } else { wgpu::Backends::VULKAN };
    let mut setup = WgpuSetupCreateNew::default();
    setup.instance_descriptor.backends = backends;
    WgpuConfiguration { wgpu_setup: setup.into(), ..Default::default() }
}

/// The Vulkan GPUs, listed without touching the window.
#[cfg(target_os = "android")]
fn vulkan_gpus() -> Vec<eframe::wgpu::AdapterInfo> {
    use eframe::wgpu;
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends: wgpu::Backends::VULKAN, ..Default::default() });
    instance.enumerate_adapters(wgpu::Backends::VULKAN).iter().map(|a| a.get_info()).collect()
}

#[cfg(not(target_os = "android"))]
fn gpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    Default::default()
}

/// The app is up, GPU included: this start didn't crash (see `gpu_options`).
fn gpu_started() {
    #[cfg(target_os = "android")]
    if let Some(d) = files::app_data_dir() {
        let _ = std::fs::remove_file(d.join("gpu-starting"));
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
/// `painter --thumbnail IN OUT SIZE` (see `file_types`): the exit code when those arguments were given.
pub fn thumbnail_command(args: &[String]) -> Option<i32> {
    file_types::thumbnail_command(args)
}

/// `reset`: the interface and tool options back to how they come (`--reset-settings`).
pub fn run(options: eframe::NativeOptions, file: Option<std::path::PathBuf>, reset: bool) -> eframe::Result {
    install_crash_log();
    // iPadOS: the Materials folder appears in Files, ready for Clip Studio materials.
    let _ = files::materials_dir();
    eframe::run_native(
        "Painter",
        options,
        Box::new(move |cc| {
            let mut app = app::PainterApp::new(cc);
            gpu_started();
            if reset {
                app.reset_settings(&cc.egui_ctx, settings_reset::ResetChoice::default());
            }
            if let Some(path) = file {
                app.open_any(path);
                // Opened with a file: straight to it.
                app.welcome_open = false;
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
