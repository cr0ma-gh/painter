//! Builds the iPadOS document picker (native/ios_files.m); nothing elsewhere.

fn main() {
    println!("cargo:rerun-if-changed=native/ios_files.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("ios") {
        return;
    }
    cc::Build::new().file("native/ios_files.m").flag("-fobjc-arc").compile("painter_ios_files");
    for framework in ["UIKit", "Foundation", "UniformTypeIdentifiers"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
