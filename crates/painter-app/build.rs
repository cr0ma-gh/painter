//! Builds the iPadOS glue (native/*.m: document picker, window setup, document
//! camera), sets `cfg(desktop)` where the system file dialogs exist, and on
//! Windows (MSVC) gives `painter.exe` its icon.

fn main() {
    println!("cargo:rerun-if-changed=native/ios_files.m");
    println!("cargo:rerun-if-changed=native/ios_ui.m");
    println!("cargo:rerun-if-changed=native/ios_scan.m");
    // `desktop`: the system file dialogs (rfd) are there; not on iPadOS or Android.
    println!("cargo::rustc-check-cfg=cfg(desktop)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os != "ios" && os != "android" {
        println!("cargo:rustc-cfg=desktop");
    }
    if os == "windows" && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        windows_icon();
    }
    if os != "ios" {
        return;
    }
    cc::Build::new().file("native/ios_files.m").file("native/ios_ui.m").file("native/ios_scan.m").flag("-fobjc-arc").compile("painter_ios");
    for framework in ["UIKit", "Foundation", "UniformTypeIdentifiers", "VisionKit"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}

/// The app icon as a compiled resource file (`.res`: an icon group and its
/// PNG images, the layout Windows' resource compiler writes), handed to the
/// MSVC linker for the `painter` binary.
fn windows_icon() {
    let dir = std::path::Path::new("../../packaging/icons");
    let sizes = [16u32, 24, 32, 48, 64, 128, 256];
    let mut images = Vec::new();
    for s in sizes {
        let path = dir.join(format!("painter-{s}.png"));
        println!("cargo:rerun-if-changed={}", path.display());
        match std::fs::read(&path) {
            Ok(png) => images.push((s, png)),
            Err(_) => return,
        }
    }
    // One resource: header (type and name as ordinals), then the data padded to 4 bytes.
    fn resource(out: &mut Vec<u8>, kind: u16, name: u16, data: &[u8]) {
        let mut h = Vec::new();
        h.extend((data.len() as u32).to_le_bytes());
        h.extend(32u32.to_le_bytes()); // header size
        h.extend([0xFF, 0xFF]);
        h.extend(kind.to_le_bytes());
        h.extend([0xFF, 0xFF]);
        h.extend(name.to_le_bytes());
        h.extend(0u32.to_le_bytes()); // data version
        h.extend(0x1030u16.to_le_bytes()); // moveable, pure, discardable
        h.extend(0x0409u16.to_le_bytes()); // language
        h.extend(0u32.to_le_bytes()); // version
        h.extend(0u32.to_le_bytes()); // characteristics
        out.extend(h);
        out.extend(data);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    }
    let mut res = Vec::new();
    // The empty first entry every .res file starts with.
    res.extend([0u8, 0, 0, 0, 32, 0, 0, 0, 0xFF, 0xFF, 0, 0, 0xFF, 0xFF, 0, 0]);
    res.extend([0u8; 16]);
    const RT_ICON: u16 = 3;
    const RT_GROUP_ICON: u16 = 14;
    let mut group = Vec::new();
    group.extend(0u16.to_le_bytes());
    group.extend(1u16.to_le_bytes()); // icons
    group.extend((images.len() as u16).to_le_bytes());
    for (i, (s, png)) in images.iter().enumerate() {
        let id = i as u16 + 1;
        resource(&mut res, RT_ICON, id, png);
        let side = if *s >= 256 { 0 } else { *s as u8 };
        group.extend([side, side, 0, 0]);
        group.extend(1u16.to_le_bytes()); // planes
        group.extend(32u16.to_le_bytes()); // bits per pixel
        group.extend((png.len() as u32).to_le_bytes());
        group.extend(id.to_le_bytes());
    }
    resource(&mut res, RT_GROUP_ICON, 1, &group);
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("painter-icon.res");
    if std::fs::write(&out, res).is_ok() {
        println!("cargo:rustc-link-arg-bins={}", out.display());
    }
}
