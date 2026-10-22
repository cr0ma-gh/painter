//! File → Set Up File Types: Painter's files in the system's file manager.
//!
//! - Windows: `.painter` opens in Painter with its icon, and Explorer shows
//!   each project's picture as its thumbnail (the `painter-shell` DLL carried
//!   inside the app); Photoshop, Krita, OpenRaster and Clip Studio files get
//!   Painter under "Open with" without changing their default app. All of it
//!   for the current user only (HKCU), and "Remove" takes it away again.
//! - Linux: the `application/x-painter` type, a desktop entry for the menus
//!   and "Open with", the icon, and a thumbnailer (`painter --thumbnail`) for
//!   file managers that use them (Files, Nemo, Caja, Thunar, Dolphin with
//!   its plugin), in `~/.local/share`.
//! - macOS, iPadOS and Android declare their file types in the app bundle /
//!   manifest: there's nothing to set up.
//!
//! Also here: `painter --thumbnail IN OUT SIZE`, which writes the thumbnail
//! a Painter, Krita or OpenRaster file carries as a PNG (the Linux thumbnailer).

use crate::app::PainterApp;

/// `painter_shell::CLSID` (checked by a test): the thumbnail provider's class.
const SHELL_CLSID: u128 = 0x5a1c_9a3e_2f4b_4c8e_9d3a_7e61_2b0c_7f41;

/// The other formats Painter opens ("Open with" only: their default app stays).
const OTHER_EXTENSIONS: [&str; 4] = ["psd", "kra", "ora", "clip"];

/// `painter --thumbnail IN OUT SIZE`: `Some(exit code)` when those arguments were given.
pub(crate) fn thumbnail_command(args: &[String]) -> Option<i32> {
    let i = args.iter().position(|a| a == "--thumbnail")?;
    let (Some(input), Some(output)) = (args.get(i + 1), args.get(i + 2)) else {
        eprintln!("usage: painter --thumbnail INPUT OUTPUT [SIZE]");
        return Some(2);
    };
    let size = args.get(i + 3).and_then(|s| s.parse::<u32>().ok()).unwrap_or(256).clamp(16, 1024);
    let made = std::fs::read(input)
        .ok()
        .and_then(|bytes| painter_core::project::embedded_thumbnail(&bytes))
        .and_then(|png| image::load_from_memory_with_format(&png, image::ImageFormat::Png).ok())
        .map(|img| if img.width().max(img.height()) > size { img.thumbnail(size, size) } else { img })
        .is_some_and(|img| img.save_with_format(output, image::ImageFormat::Png).is_ok());
    Some(if made { 0 } else { 1 })
}

/// One registry value under `HKEY_CURRENT_USER` (an empty name is the key's default value).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RegValue {
    pub key: String,
    pub name: String,
    pub value: String,
}

/// What Windows needs: `exe` is painter.exe, `dll` the installed thumbnail provider.
pub(crate) fn windows_entries(exe: &str, dll: Option<&str>) -> Vec<RegValue> {
    let mut out = Vec::new();
    let mut set = |key: &str, name: &str, value: &str| {
        out.push(RegValue { key: format!(r"Software\Classes\{key}"), name: name.into(), value: value.into() });
    };
    let open = format!("\"{exe}\" \"%1\"");
    set(".painter", "", "Painter.Project");
    set(".painter", "Content Type", "application/x-painter");
    set(".painter", "PerceivedType", "image");
    set(r".painter\OpenWithProgids", "Painter.Project", "");
    set("Painter.Project", "", "Painter project");
    set(r"Painter.Project\DefaultIcon", "", &format!("\"{exe}\",0"));
    set(r"Painter.Project\shell\open\command", "", &open);
    set("Painter.Document", "", "Picture (opens in Painter)");
    set(r"Painter.Document\DefaultIcon", "", &format!("\"{exe}\",0"));
    set(r"Painter.Document\shell\open\command", "", &open);
    for ext in OTHER_EXTENSIONS {
        set(&format!(r".{ext}\OpenWithProgids"), "Painter.Document", "");
    }
    set(r"Applications\painter.exe", "FriendlyAppName", "Painter");
    set(r"Applications\painter.exe\shell\open\command", "", &open);
    for ext in ["painter"].iter().chain(OTHER_EXTENSIONS.iter()) {
        set(r"Applications\painter.exe\SupportedTypes", &format!(".{ext}"), "");
    }
    if let Some(dll) = dll {
        let clsid = clsid_string();
        set(&format!(r"CLSID\{clsid}"), "", "Painter thumbnails");
        set(&format!(r"CLSID\{clsid}\InprocServer32"), "", dll);
        set(&format!(r"CLSID\{clsid}\InprocServer32"), "ThreadingModel", "Apartment");
        // IThumbnailProvider's shell extension slot.
        set(r".painter\ShellEx\{e357fccd-a995-4576-b01f-234630154e96}", "", &clsid);
    }
    out
}

/// Keys "Remove" deletes (with everything under them).
pub(crate) fn windows_owned_keys() -> Vec<String> {
    let mut keys: Vec<String> = ["Painter.Project", "Painter.Document", r"Applications\painter.exe", r".painter\ShellEx"]
        .iter()
        .map(|k| format!(r"Software\Classes\{k}"))
        .collect();
    keys.push(format!(r"Software\Classes\CLSID\{}", clsid_string()));
    keys
}

/// `{5A1C9A3E-2F4B-4C8E-9D3A-7E612B0C7F41}`.
fn clsid_string() -> String {
    let v = SHELL_CLSID;
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:04X}-{:012X}}}",
        (v >> 96) as u32,
        (v >> 80) as u16,
        (v >> 64) as u16,
        (v >> 48) as u16,
        v & 0xFFFF_FFFF_FFFF
    )
}

/// Linux: (path under `~/.local/share`, contents) for each file to write.
pub(crate) fn linux_files(exe: &str) -> Vec<(String, Vec<u8>)> {
    let quoted = if exe.contains(' ') { format!("\"{exe}\"") } else { exe.to_string() };
    let types = "application/x-painter;image/vnd.adobe.photoshop;application/x-krita;image/openraster;application/x-clip-studio-paint;image/png;image/jpeg;";
    let mime = r#"<?xml version="1.0" encoding="UTF-8"?>
<mime-info xmlns="http://www.freedesktop.org/standards/shared-mime-info">
  <mime-type type="application/x-painter">
    <comment>Painter project</comment>
    <sub-class-of type="application/zip"/>
    <glob pattern="*.painter"/>
    <generic-icon name="application-x-painter"/>
  </mime-type>
  <mime-type type="application/x-clip-studio-paint">
    <comment>Clip Studio Paint illustration</comment>
    <glob pattern="*.clip"/>
  </mime-type>
</mime-info>
"#;
    let desktop = format!(
        "[Desktop Entry]\nType=Application\nName=Painter\nComment=Paint, ink and lay out comics\nExec={quoted} %F\nIcon=painter\nTerminal=false\nCategories=Graphics;2DGraphics;RasterGraphics;\nMimeType={types}\n"
    );
    let thumbnailer = format!(
        "[Thumbnailer Entry]\nTryExec={quoted}\nExec={quoted} --thumbnail %i %o %s\nMimeType=application/x-painter;image/openraster;application/x-krita;\n"
    );
    let icon = include_bytes!("../../../packaging/icons/painter-256.png").to_vec();
    vec![
        ("mime/packages/painter.xml".into(), mime.as_bytes().to_vec()),
        ("applications/painter.desktop".into(), desktop.into_bytes()),
        ("thumbnailers/painter.thumbnailer".into(), thumbnailer.into_bytes()),
        ("icons/hicolor/256x256/apps/painter.png".into(), icon.clone()),
        ("icons/hicolor/256x256/mimetypes/application-x-painter.png".into(), icon),
    ]
}

/// The program to register: the AppImage when running from one, else this executable.
fn program_path() -> Option<String> {
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        return Some(appimage.to_string_lossy().into_owned());
    }
    std::env::current_exe().ok().map(|p| p.to_string_lossy().into_owned())
}

#[cfg(windows)]
mod windows_registry {
    use super::RegValue;
    use windows::core::HSTRING;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE,
        REG_SZ,
    };
    use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};

    pub(super) fn write(values: &[RegValue]) -> Result<(), String> {
        for v in values {
            unsafe {
                let mut key = HKEY::default();
                let r = RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    &HSTRING::from(&v.key),
                    0,
                    None,
                    REG_OPTION_NON_VOLATILE,
                    KEY_WRITE,
                    None,
                    &mut key,
                    None,
                );
                if r != ERROR_SUCCESS {
                    return Err(format!("{}: error {}", v.key, r.0));
                }
                let wide: Vec<u16> = v.value.encode_utf16().chain(std::iter::once(0)).collect();
                let bytes = std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2);
                let r = RegSetValueExW(key, &HSTRING::from(&v.name), 0, REG_SZ, Some(bytes));
                let _ = RegCloseKey(key);
                if r != ERROR_SUCCESS {
                    return Err(format!("{}\\{}: error {}", v.key, v.name, r.0));
                }
            }
        }
        notify();
        Ok(())
    }

    pub(super) fn delete(keys: &[String]) {
        for k in keys {
            unsafe {
                let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(k));
            }
        }
        notify();
    }

    /// Tells Explorer the associations changed.
    fn notify() {
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    }
}

/// The thumbnail DLL carried inside this build, if it was built with one.
#[cfg(all(windows, shell_dll))]
const SHELL_DLL: Option<&[u8]> = Some(include_bytes!(concat!(env!("OUT_DIR"), "/painter_shell.dll")));
#[cfg(not(all(windows, shell_dll)))]
const SHELL_DLL: Option<&[u8]> = None;

/// Sets the file types up for this user. Returns what was done.
pub(crate) fn register() -> Result<String, String> {
    let exe = program_path().ok_or("can't tell where Painter is")?;
    if cfg!(windows) {
        // The DLL goes in the user's app data, versioned (Explorer may hold an older one open).
        let dll = match SHELL_DLL {
            Some(bytes) => {
                let dir = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).ok_or("no LOCALAPPDATA")?.join("Painter");
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let path = dir.join(format!("painter_shell-{}.dll", crate::welcome::VERSION));
                if std::fs::read(&path).ok().as_deref() != Some(bytes) {
                    std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
                }
                Some(path.to_string_lossy().into_owned())
            }
            None => None,
        };
        #[cfg(windows)]
        windows_registry::write(&windows_entries(&exe, dll.as_deref()))?;
        return Ok(match dll {
            Some(_) => "Painter projects now open in Painter and show their picture in Explorer; PSD, Krita, OpenRaster and Clip Studio files list Painter under Open with".into(),
            None => "Painter projects now open in Painter; PSD, Krita, OpenRaster and Clip Studio files list Painter under Open with (this build has no Explorer thumbnails)".into(),
        });
    }
    if cfg!(target_os = "linux") {
        let home = std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".local/share")))
            .ok_or("no home folder")?;
        for (rel, bytes) in linux_files(&exe) {
            let path = home.join(rel);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        // Refresh the caches when the tools are there (most desktops have them).
        let _ = std::process::Command::new("update-mime-database").arg(home.join("mime")).status();
        let _ = std::process::Command::new("update-desktop-database").arg(home.join("applications")).status();
        let _ = std::process::Command::new("gtk-update-icon-cache").arg("-q").arg(home.join("icons/hicolor")).status();
        return Ok("Painter is set up for .painter files, in the menus and under Open with; file managers show project thumbnails (after a restart of the file manager)".into());
    }
    Ok("On this system Painter's file types come with the app: nothing to set up".into())
}

/// Takes the file types away again.
pub(crate) fn unregister() -> Result<String, String> {
    #[cfg(windows)]
    windows_registry::delete(&windows_owned_keys());
    if cfg!(target_os = "linux") {
        if let Some(home) = std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".local/share")) {
            for (rel, _) in linux_files("painter") {
                let _ = std::fs::remove_file(home.join(rel));
            }
            let _ = std::process::Command::new("update-mime-database").arg(home.join("mime")).status();
        }
    }
    Ok("Painter's file types were removed".into())
}

impl PainterApp {
    /// File → Set Up File Types.
    pub(crate) fn file_types_window(&mut self, ctx: &eframe::egui::Context) {
        if !self.file_types_open {
            return;
        }
        let mut open = true;
        let mut action = None;
        eframe::egui::Window::new("File Types").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.set_max_width(420.0);
            if cfg!(any(windows, target_os = "linux")) {
                ui.label("Show Painter's files in the system's file manager: .painter projects open in Painter, with their picture as the thumbnail, and Photoshop, Krita, OpenRaster and Clip Studio files list Painter under \"Open with\" (their default app doesn't change).");
                ui.small("For your user only; Remove takes it all away.");
                if cfg!(windows) && SHELL_DLL.is_none() {
                    ui.small("This build has no Explorer thumbnails (made without painter-shell).");
                }
                ui.horizontal(|ui| {
                    if ui.button("Set Up").clicked() {
                        action = Some(true);
                    }
                    if ui.button("Remove").clicked() {
                        action = Some(false);
                    }
                });
            } else {
                ui.label("Painter's file types come with the app on this system: .painter projects show their thumbnail and open in Painter, and Painter is offered for PSD, Krita, OpenRaster and Clip Studio files.");
            }
        });
        if let Some(set_up) = action {
            let result = if set_up { register() } else { unregister() };
            self.status = result.unwrap_or_else(|e| format!("Couldn't set up the file types: {e}"));
            self.file_types_open = false;
        }
        if !open {
            self.file_types_open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_registration_is_per_user_and_complete() {
        let values = windows_entries(r"C:\Apps\painter.exe", Some(r"C:\Users\me\AppData\Local\Painter\painter_shell-1.dll"));
        assert!(values.iter().all(|v| v.key.starts_with(r"Software\Classes\")));
        let get = |key: &str, name: &str| {
            values.iter().find(|v| v.key == format!(r"Software\Classes\{key}") && v.name == name).map(|v| v.value.clone())
        };
        assert_eq!(get(".painter", "").as_deref(), Some("Painter.Project"));
        assert_eq!(get(r"Painter.Project\shell\open\command", "").as_deref(), Some(r#""C:\Apps\painter.exe" "%1""#));
        // Other formats: only "Open with", never their default.
        assert!(get(".psd", "").is_none());
        assert_eq!(get(r".psd\OpenWithProgids", "Painter.Document").as_deref(), Some(""));
        // The thumbnail provider: its class and the shell extension slot.
        assert_eq!(SHELL_CLSID, painter_shell::CLSID);
        assert_eq!(clsid_string(), "{5A1C9A3E-2F4B-4C8E-9D3A-7E612B0C7F41}");
        assert_eq!(get(r".painter\ShellEx\{e357fccd-a995-4576-b01f-234630154e96}", "").as_deref(), Some(clsid_string().as_str()));
        assert!(get(&format!(r"CLSID\{}\InprocServer32", clsid_string()), "").unwrap().ends_with("painter_shell-1.dll"));
        // Without the DLL, no thumbnail keys.
        assert!(windows_entries("p.exe", None).iter().all(|v| !v.key.contains("CLSID") && !v.key.contains("ShellEx")));
    }

    #[test]
    fn linux_files_point_at_the_program() {
        let files = linux_files("/home/me/Apps/Painter.AppImage");
        let text = |name: &str| String::from_utf8(files.iter().find(|f| f.0.ends_with(name)).unwrap().1.clone()).unwrap();
        assert!(text("painter.thumbnailer").contains("Exec=/home/me/Apps/Painter.AppImage --thumbnail %i %o %s"));
        assert!(text("painter.desktop").contains("MimeType=application/x-painter;"));
        assert!(text("painter.xml").contains(r#"<glob pattern="*.painter"/>"#));
    }

    #[test]
    fn the_thumbnail_command_writes_a_png() {
        let dir = std::env::temp_dir().join(format!("painter-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("a.painter");
        std::fs::write(&input, painter_core::project::to_bytes(&painter_core::Document::new(600, 300)).unwrap()).unwrap();
        let output = dir.join("a.png");
        let args: Vec<String> =
            ["painter", "--thumbnail", input.to_str().unwrap(), output.to_str().unwrap(), "128"].map(String::from).to_vec();
        assert_eq!(thumbnail_command(&args), Some(0));
        let img = image::open(&output).unwrap();
        assert_eq!((img.width(), img.height()), (128, 64));
        assert_eq!(thumbnail_command(&["painter".to_string()]), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
