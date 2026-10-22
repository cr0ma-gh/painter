//! Builds the iPadOS glue (native/*.m: document picker, window setup, document camera); nothing elsewhere.

fn main() {
    println!("cargo:rerun-if-changed=native/ios_files.m");
    println!("cargo:rerun-if-changed=native/ios_ui.m");
    println!("cargo:rerun-if-changed=native/ios_scan.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("ios") {
        return;
    }
    cc::Build::new().file("native/ios_files.m").file("native/ios_ui.m").file("native/ios_scan.m").flag("-fobjc-arc").compile("painter_ios");
    for framework in ["UIKit", "Foundation", "UniformTypeIdentifiers", "VisionKit"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
