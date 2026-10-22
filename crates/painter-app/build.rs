//! Builds the iPadOS glue (native/*.m: document picker, window setup, document
//! camera), and sets `cfg(desktop)` where the system file dialogs exist.

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
    if os != "ios" {
        return;
    }
    cc::Build::new().file("native/ios_files.m").file("native/ios_ui.m").file("native/ios_scan.m").flag("-fobjc-arc").compile("painter_ios");
    for framework in ["UIKit", "Foundation", "UniformTypeIdentifiers", "VisionKit"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
