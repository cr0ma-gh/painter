//! The COM entry points are exported as PRIVATE (as Windows expects of them).
fn main() {
    let target = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() && target == "msvc" {
        let def = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("painter_shell.def");
        println!("cargo:rustc-cdylib-link-arg=/DEF:{}", def.display());
        println!("cargo:rerun-if-changed=painter_shell.def");
    }
}
