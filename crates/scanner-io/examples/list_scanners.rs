//! Lists the scanners every backend sees, with their capabilities. Read-only:
//! nothing is scanned.
//!
//! ```text
//! cargo run -p painter-scanner-io --example list_scanners
//! ```

fn main() {
    let mut backends = scanner_io::backends();
    let (devices, errors) = scanner_io::list_devices(&mut backends);
    for d in &devices {
        let c = &d.capabilities;
        println!("{} [{}] {} — id {}", d.name, d.backend.name(), d.manufacturer, d.id);
        println!("  resolutions: {:?}", c.resolutions);
        println!("  modes: {:?}", c.color_modes.iter().map(|m| m.name()).collect::<Vec<_>>());
        println!("  bed: {:.0} × {:.0} mm, feeder: {}", c.bed_mm[0], c.bed_mm[1], c.has_feeder);
    }
    if devices.is_empty() {
        println!("no scanners found");
    }
    for e in errors {
        println!("backend error: {e}");
    }
}
