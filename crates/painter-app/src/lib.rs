//! Painter: the application, as a library so each platform can start it its
//! own way (`main.rs` on the desktop and iPadOS, `android_main` in the
//! `painter-android` crate on Android).

mod actions;
mod adjust_ui;
#[cfg(target_os = "android")]
mod android;
#[cfg(any(target_os = "android", test))]
mod android_touch;
mod advanced_tool;
mod app;
mod brush_cursor;
mod brush_library;
mod brush_ui;
mod clipboard_image;
mod cmyk_ui;
mod clock_widget;
mod collab_ui;
mod chip_picker;
mod color_mode_ui;
mod color_panels;
mod search_layer;
mod settings_reset;
mod setup_import;
mod shape_tool;
mod smart_shape;
mod stamp_tool;
mod canvas;
mod channels_ui;
mod comic_ui;
mod crash_report;
mod curve;
mod dev_shot;
mod dialogs;
mod document_manager;
mod eyedropper;
mod file_types;
mod file_object;
mod files;
mod gamut_selector;
mod hardware;
mod history_ui;
mod icons;
mod i18n;
pub mod po;
mod layer_props;
mod layer_thumbs;
mod layers_ui;
mod lines;
mod magnetic;
mod ios_ui;
mod modifier_keys;
mod panels;
mod nav_gizmo;
mod palm;
mod palm_ui;
mod pose_ui;
mod previews;
mod ruler_bar;
mod recent;
mod rulers_ui;
mod print_dialog;
mod print_ui;
mod scan_source;
mod scene_ui;
mod texture_paint;
#[cfg(target_os = "linux")]
mod pen_x11;
mod tablet;
mod text_tool;
mod theme;
mod toolbar;
mod tool_groups;
mod tools;
mod transform_ui;
mod updater;
mod vector_edit;
mod view;
mod welcome;
mod widgets;
#[cfg(windows)]
mod win_print;
#[cfg(windows)]
mod win_alt_space;
#[cfg(windows)]
mod wintab;
mod workspace_ui;

/// Touch screens (iPadOS, Android): bigger controls, on-screen modifier keys,
/// the pen alone paints, Save As asks for a name in the app.
pub(crate) const MOBILE: bool = cfg!(any(target_os = "ios", target_os = "android"));

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
    // Decided before the window's surface exists: one backend only, since a
    // Vulkan surface takes the window and OpenGL ES can't attach to it after.
    let force_gl = gpu_crash_marker(b"Vulkan crashed while starting; OpenGL ES is used.
");
    let vulkan = if force_gl { Vec::new() } else { vulkan_gpus() };
    let mali = vulkan.iter().any(|info| info.vendor == 0x13B5 || info.name.to_ascii_lowercase().contains("mali"));
    let backends = if force_gl || mali || vulkan.is_empty() { wgpu::Backends::GL } else { wgpu::Backends::VULKAN };
    let mut setup = WgpuSetupCreateNew::default();
    setup.instance_descriptor.backends = backends;
    WgpuConfiguration { wgpu_setup: setup.into(), ..Default::default() }
}

/// Marks this start in the app's data folder (`gpu-starting`, cleared by
/// `gpu_started`). If the last start's marker is still there, it never got
/// past the GPU setup: `gpu-use-opengl` is written with `note`. True when
/// that compatible-mode marker exists; it sticks once made.
fn gpu_crash_marker(note: &[u8]) -> bool {
    let Some(dir) = files::app_data_dir() else { return false };
    let starting = dir.join("gpu-starting");
    let use_gl = dir.join("gpu-use-opengl");
    if starting.exists() {
        let _ = std::fs::write(&use_gl, note);
    }
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(starting, b"");
    use_gl.exists()
}

/// The Vulkan GPUs, listed without touching the window.
#[cfg(target_os = "android")]
fn vulkan_gpus() -> Vec<eframe::wgpu::AdapterInfo> {
    use eframe::wgpu;
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends: wgpu::Backends::VULKAN, ..Default::default() });
    instance.enumerate_adapters(wgpu::Backends::VULKAN).iter().map(|a| a.get_info()).collect()
}

/// Desktop: every backend wgpu has here (Vulkan, DirectX 12, Metal and
/// OpenGL), with device limits that fit the GPU (see [`device_limits`]), so
/// old GPUs (Intel GMA 4500, HD 3000: no DirectX 12, no Vulkan) start on
/// OpenGL instead of failing. Compatibility mode (`--low-gpu` /
/// `--gpu-compat`, `PAINTER_LOW_GPU=1`, or after a start that died while
/// setting up the GPU; `PAINTER_LOW_GPU=0` undoes that) goes straight to
/// OpenGL with downlevel limits.
#[cfg(not(target_os = "android"))]
fn gpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetupCreateNew};
    use eframe::wgpu;
    let crashed = gpu_crash_marker(b"The GPU crashed while starting; compatibility mode (OpenGL) is used.
");
    let args: Vec<String> = std::env::args().collect();
    let env = std::env::var_os("PAINTER_LOW_GPU");
    // `PAINTER_LOW_GPU=0`: back to the usual setup, forgetting a past crash.
    let crashed = if env.as_deref() == Some(std::ffi::OsStr::new("0")) {
        if let Some(d) = files::app_data_dir() {
            let _ = std::fs::remove_file(d.join("gpu-use-opengl"));
        }
        false
    } else {
        crashed
    };
    let compat = crashed || low_gpu_requested(&args, env.as_deref());
    let mut setup = WgpuSetupCreateNew::default();
    // `WGPU_BACKEND` still wins; macOS and iPadOS have no OpenGL in wgpu.
    if compat && wgpu::Backends::from_env().is_none() && !cfg!(target_vendor = "apple") {
        setup.instance_descriptor.backends = wgpu::Backends::GL;
    }
    setup.device_descriptor = std::sync::Arc::new(move |adapter| wgpu::DeviceDescriptor {
        label: Some("egui wgpu device"),
        required_features: wgpu::Features::default(),
        required_limits: device_limits(&adapter.limits(), compat),
        memory_hints: wgpu::MemoryHints::default(),
    });
    WgpuConfiguration { wgpu_setup: setup.into(), ..Default::default() }
}

/// The command-line flags that start in GPU compatibility mode.
pub const LOW_GPU_FLAGS: [&str; 2] = ["--low-gpu", "--gpu-compat"];

/// Compatibility mode asked for: one of [`LOW_GPU_FLAGS`] in `args`, or
/// `PAINTER_LOW_GPU` set to anything but `0`.
#[cfg_attr(target_os = "android", allow(dead_code))]
fn low_gpu_requested(args: &[String], env: Option<&std::ffi::OsStr>) -> bool {
    args.iter().any(|a| LOW_GPU_FLAGS.contains(&a.as_str())) || env.is_some_and(|v| !v.is_empty() && v != "0")
}

/// The limits to ask of a GPU offering `adapter`: wgpu's defaults when it
/// has them (and `compat` is off), else the downlevel ones (OpenGL 3.3 /
/// DirectX 11 class), else WebGL 2's (OpenGL ES 3.0), else just what it has.
/// Textures stay up to 8192 pixels wide where the GPU allows, for 4K screens.
#[cfg_attr(target_os = "android", allow(dead_code))]
fn device_limits(adapter: &eframe::wgpu::Limits, compat: bool) -> eframe::wgpu::Limits {
    use eframe::wgpu::Limits;
    let tiers = [Limits::default(), Limits::downlevel_defaults(), Limits::downlevel_webgl2_defaults()];
    let tiers = if compat { &tiers[1..] } else { &tiers[..] };
    let base = tiers.iter().find(|t| t.check_limits(adapter)).cloned().unwrap_or_else(|| adapter.clone());
    Limits { max_texture_dimension_2d: adapter.max_texture_dimension_2d.min(8192), ..base }
}

/// The app is up, GPU included: this start didn't crash (see `gpu_options`).
fn gpu_started() {
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

/// `painter --register-file-types` / `--unregister-file-types`, `--import-brushes FILE…`,
/// `--copy-csp-materials` (the Windows installer runs them): file types,
/// brushes and materials, no window. `None` when none of them was asked.
pub fn file_types_command(args: &[String]) -> Option<i32> {
    let result = if args.iter().any(|a| a == "--register-file-types") {
        file_types::register()
    } else if args.iter().any(|a| a == "--unregister-file-types") {
        file_types::unregister()
    } else if args.iter().any(|a| a == "--copy-csp-materials") {
        setup_import::copy_csp_materials()
    } else {
        // `--import-brushes FILE…`: every argument after it.
        let i = args.iter().position(|a| a == "--import-brushes")?;
        let files: Vec<std::path::PathBuf> = args[i + 1..].iter().map(std::path::PathBuf::from).collect();
        setup_import::import_brushes(&files)
    };
    Some(match result {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    })
}

/// `reset`: the interface and tool options back to how they come (`--reset-settings`).
pub fn run(options: eframe::NativeOptions, file: Option<std::path::PathBuf>, reset: bool) -> eframe::Result {
    // A panic is written to a crash report, shown at the next start.
    crash_report::install();
    // iPadOS: the Materials folder appears in Files, ready for Clip Studio materials.
    let _ = files::materials_dir();
    eframe::run_native(
        "Painter",
        options,
        Box::new(move |cc| {
            let mut app = app::PainterApp::new(cc);
            gpu_started();
            if let Some(rs) = &cc.wgpu_render_state {
                crash_report::set_renderer(&rs.adapter.get_info());
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::wgpu::Limits;

    #[test]
    fn modern_gpus_get_the_default_limits() {
        let adapter = Limits { max_texture_dimension_2d: 16384, ..Limits::default() };
        let limits = device_limits(&adapter, false);
        assert_eq!(limits, Limits { max_texture_dimension_2d: 8192, ..Limits::default() });
        // Compatibility mode asks for less even of a capable GPU.
        assert_eq!(device_limits(&adapter, true), Limits { max_texture_dimension_2d: 8192, ..Limits::downlevel_defaults() });
    }

    #[test]
    fn downlevel_gpus_get_limits_they_have() {
        // An OpenGL 3.3 class GPU: the default limits would fail the device request.
        let adapter = Limits::downlevel_defaults();
        assert!(!Limits::default().check_limits(&adapter));
        let limits = device_limits(&adapter, false);
        assert_eq!(limits, Limits::downlevel_defaults());
        assert!(limits.check_limits(&adapter));
    }

    #[test]
    fn webgl2_class_and_weaker_gpus_still_start() {
        let adapter = Limits { max_texture_dimension_2d: 4096, ..Limits::downlevel_webgl2_defaults() };
        let limits = device_limits(&adapter, false);
        assert_eq!(limits, Limits { max_texture_dimension_2d: 4096, ..Limits::downlevel_webgl2_defaults() });
        assert!(limits.check_limits(&adapter));
        // Below even WebGL 2: exactly what the GPU offers.
        let adapter = Limits { max_texture_array_layers: 64, ..Limits::downlevel_webgl2_defaults() };
        assert_eq!(device_limits(&adapter, true), adapter);
    }

    #[test]
    fn low_gpu_flags_and_variable() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(low_gpu_requested(&args(&["painter", "--low-gpu"]), None));
        assert!(low_gpu_requested(&args(&["painter", "--gpu-compat", "a.painter"]), None));
        assert!(!low_gpu_requested(&args(&["painter", "a.painter"]), None));
        assert!(low_gpu_requested(&args(&["painter"]), Some("1".as_ref())));
        assert!(!low_gpu_requested(&args(&["painter"]), Some("0".as_ref())));
        assert!(!low_gpu_requested(&args(&["painter"]), Some("".as_ref())));
    }
}
