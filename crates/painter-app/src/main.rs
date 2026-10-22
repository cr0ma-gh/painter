#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> eframe::Result {
    // `painter --thumbnail IN OUT SIZE`: a file manager asking for a thumbnail.
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = painter_app::thumbnail_command(&args) {
        std::process::exit(code);
    }
    let file = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    painter_app::run(painter_app::native_options(), file)
}
