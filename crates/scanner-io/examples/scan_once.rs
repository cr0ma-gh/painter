//! Scans once with the first device and saves the raw scan and the cleaned-up
//! line art (what File → Import → From Scanner puts on a layer) as PNGs.
//!
//! ```text
//! cargo run -p painter-scanner-io --example scan_once -- 150 gray raw.png lineart.png
//! ```
//! Modes: `color`, `gray`, `lineart`.

use scanner_io::{backends, list_devices, prepare_for_import, ColorMode, ImportOptions, ScanSettings};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dpi, mode, raw_path, prepared_path] = &args[..] else {
        eprintln!("usage: scan_once <dpi> <color|gray|lineart> <raw.png> <lineart.png>");
        std::process::exit(2);
    };
    let color_mode = match mode.as_str() {
        "color" => ColorMode::Color,
        "lineart" => ColorMode::LineArt,
        _ => ColorMode::Grayscale,
    };
    let mut backends = backends();
    let (devices, errors) = list_devices(&mut backends);
    let Some(device) = devices.first() else {
        eprintln!("no scanner found: {errors:?}");
        std::process::exit(1);
    };
    let settings = ScanSettings { dpi: dpi.parse().unwrap_or(150), color_mode, ..ScanSettings::default() };
    let settings = settings.validated(&device.capabilities).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
    println!("scanning with {} at {} dpi, {}…", device.name, settings.dpi, settings.color_mode.name());
    let backend = backends.iter_mut().find(|b| b.backend() == device.backend).unwrap();
    let started = std::time::Instant::now();
    let image = backend.scan(device, &settings).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
    let [w_mm, h_mm] = image.size_mm();
    println!("{}×{} px ({w_mm:.0}×{h_mm:.0} mm) in {:.1} s", image.width, image.height, started.elapsed().as_secs_f32());
    image::save_buffer(raw_path, &image.rgba, image.width, image.height, image::ColorType::Rgba8).expect("write raw");
    let prepared = prepare_for_import(&image, &ImportOptions::default());
    image::save_buffer(prepared_path, &prepared.rgba, prepared.width, prepared.height, image::ColorType::Rgba8).expect("write line art");
    let inked = prepared.rgba.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count();
    println!("line art: {:.1}% of pixels keep ink", inked as f32 * 100.0 / (prepared.width * prepared.height) as f32);
}
