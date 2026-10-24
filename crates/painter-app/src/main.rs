#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> eframe::Result {
    // `painter --thumbnail IN OUT SIZE`: a file manager asking for a thumbnail.
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = painter_app::thumbnail_command(&args) {
        std::process::exit(code);
    }
    // `painter --register-file-types` / `--unregister-file-types`: the installer setting up.
    if let Some(code) = painter_app::file_types_command(&args) {
        std::process::exit(code);
    }
    // `painter --reset-settings [FILE]`: the interface and tool options as they come.
    let reset = std::env::args_os().any(|a| a == "--reset-settings");
    let file = std::env::args_os().skip(1).find(|a| a != "--reset-settings").map(std::path::PathBuf::from);
    painter_app::run(painter_app::native_options(), file, reset)
}
