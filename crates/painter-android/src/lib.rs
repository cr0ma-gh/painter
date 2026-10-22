//! Painter on Android. Android starts the app through `android_main` in this
//! shared library (a NativeActivity); everything else is `painter-app`.
//! Documents live in the app's own storage (Android/data/<app>/files), where
//! the in-app file list finds them.

#[cfg(target_os = "android")]
#[no_mangle]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    painter_app::set_android_app(app.clone());
    let mut options = painter_app::native_options();
    options.android_app = Some(app);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| painter_app::run(options, None)));
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(err)) => eprintln!("Painter: {err}"),
        Err(_) => eprintln!("Painter: stopped after a panic"),
    }
    // Android keeps the process for the next launch, but winit's event loop
    // can't be made twice in one process: end it, so the next start is fresh.
    std::process::exit(0);
}
