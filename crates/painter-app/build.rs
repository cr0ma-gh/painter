//! Builds the iPadOS glue (native/*.m: document picker, window setup); nothing elsewhere.

fn main() {
    println!("cargo:rerun-if-changed=native/ios_files.m");
    println!("cargo:rerun-if-changed=native/ios_ui.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("ios") {
        return;
    }
    cc::Build::new().file("native/ios_files.m").file("native/ios_ui.m").flag("-fobjc-arc").compile("painter_ios");
    for framework in ["UIKit", "Foundation", "UniformTypeIdentifiers"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
