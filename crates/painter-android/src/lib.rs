//! Painter on Android. Android starts the app through `android_main` in this
//! shared library (a NativeActivity); everything else is `painter-app`.
//! Documents live in the app's own storage (Android/data/<app>/files), where
//! the in-app file list finds them.

#[cfg(target_os = "android")]
#[no_mangle]
fn android_main(app: eframe::egui_winit::winit::platform::android::activity::AndroidApp) {
    painter_app::set_android_folders(app.internal_data_path(), app.external_data_path());
    let mut options = painter_app::native_options();
    options.android_app = Some(app);
    if let Err(err) = painter_app::run(options, None) {
        eprintln!("Painter: {err}");
    }
}
