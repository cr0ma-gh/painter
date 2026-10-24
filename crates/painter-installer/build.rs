//! Puts what the installer carries into it:
//! - the app: `PAINTER_EXE` names a built `painter.exe` (release), stored
//!   compressed; without it the installer builds with no app inside (it then
//!   says so instead of installing) — for checks and screenshots;
//! - on Windows (MSVC), the icon and a manifest that runs it as the user
//!   (Windows would otherwise ask for administrator rights for any program
//!   named "Setup").

use std::io::Write;
use std::path::{Path, PathBuf};

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    payload(&out_dir);
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    if windows && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        resources(&out_dir);
    }
}

fn payload(out_dir: &Path) {
    println!("cargo::rustc-check-cfg=cfg(has_payload)");
    println!("cargo:rerun-if-env-changed=PAINTER_EXE");
    let out = out_dir.join("painter.exe.deflate");
    let source = std::env::var_os("PAINTER_EXE").map(PathBuf::from).filter(|p| p.is_file());
    match source.and_then(|p| std::fs::read(&p).ok().map(|b| (p, b))) {
        Some((path, bytes)) => {
            println!("cargo:rerun-if-changed={}", path.display());
            let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
            z.write_all(&bytes).expect("compress");
            std::fs::write(&out, z.finish().expect("compress")).expect("write payload");
            println!("cargo:rustc-cfg=has_payload");
        }
        None => {
            std::fs::write(&out, []).expect("write payload");
            if std::env::var_os("PAINTER_EXE").is_some() {
                println!("cargo:warning=PAINTER_EXE doesn't name a file: the installer carries no app");
            }
        }
    }
}

/// The icon group and the manifest as a compiled resource file (`.res`), handed
/// to the MSVC linker (the same layout as the app's, see painter-app's build.rs).
fn resources(out_dir: &Path) {
    let dir = Path::new("../../packaging/icons");
    let mut images = Vec::new();
    for s in [16u32, 24, 32, 48, 64, 128, 256] {
        let path = dir.join(format!("painter-{s}.png"));
        println!("cargo:rerun-if-changed={}", path.display());
        match std::fs::read(&path) {
            Ok(png) => images.push((s, png)),
            Err(_) => return,
        }
    }
    fn resource(out: &mut Vec<u8>, kind: u16, name: u16, data: &[u8]) {
        let mut h = Vec::new();
        h.extend((data.len() as u32).to_le_bytes());
        h.extend(32u32.to_le_bytes());
        h.extend([0xFF, 0xFF]);
        h.extend(kind.to_le_bytes());
        h.extend([0xFF, 0xFF]);
        h.extend(name.to_le_bytes());
        h.extend(0u32.to_le_bytes());
        h.extend(0x1030u16.to_le_bytes());
        h.extend(0x0409u16.to_le_bytes());
        h.extend(0u32.to_le_bytes());
        h.extend(0u32.to_le_bytes());
        out.extend(h);
        out.extend(data);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    }
    let mut res = Vec::new();
    res.extend([0u8, 0, 0, 0, 32, 0, 0, 0, 0xFF, 0xFF, 0, 0, 0xFF, 0xFF, 0, 0]);
    res.extend([0u8; 16]);
    const RT_ICON: u16 = 3;
    const RT_GROUP_ICON: u16 = 14;
    const RT_MANIFEST: u16 = 24;
    let mut group = Vec::new();
    group.extend(0u16.to_le_bytes());
    group.extend(1u16.to_le_bytes());
    group.extend((images.len() as u16).to_le_bytes());
    for (i, (s, png)) in images.iter().enumerate() {
        let id = i as u16 + 1;
        resource(&mut res, RT_ICON, id, png);
        let side = if *s >= 256 { 0 } else { *s as u8 };
        group.extend([side, side, 0, 0]);
        group.extend(1u16.to_le_bytes());
        group.extend(32u16.to_le_bytes());
        group.extend((png.len() as u32).to_le_bytes());
        group.extend(id.to_le_bytes());
    }
    resource(&mut res, RT_GROUP_ICON, 1, &group);
    let manifest = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="Painter.Setup" version="1.0.0.0"/>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security><requestedPrivileges><requestedExecutionLevel level="asInvoker" uiAccess="false"/></requestedPrivileges></security>
  </trustInfo>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </windowsSettings>
  </application>
</assembly>
"#;
    resource(&mut res, RT_MANIFEST, 1, manifest.as_bytes());
    let out = out_dir.join("installer.res");
    if std::fs::write(&out, res).is_ok() {
        println!("cargo:rustc-link-arg-bins={}", out.display());
        // Our manifest, not one the linker makes.
        println!("cargo:rustc-link-arg-bins=/MANIFEST:NO");
    }
}
