//! What the installer does on the disk, for the user only (no administrator):
//! - Painter goes in `%LOCALAPPDATA%\Programs\Painter` (or a folder chosen),
//!   with this installer beside it as `uninstall.exe`;
//! - shortcuts in the Start menu and on the desktop;
//! - Painter's file types (painter.exe `--register-file-types`);
//! - an entry in Settings → Apps (`HKCU\…\Uninstall\Painter`).
//!
//! Removing undoes each of these, and can also remove the settings.

use std::path::{Path, PathBuf};

use windows::core::{Interface, HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_DWORD,
    REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Programs, IShellLinkW, SHGetKnownFolderPath, ShellLink, KNOWN_FOLDER_FLAG,
};

/// The compressed `painter.exe` this installer carries (empty in builds without one).
const PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/painter.exe.deflate"));

const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Painter";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn has_payload() -> bool {
    !PAYLOAD.is_empty()
}

/// What to set up.
#[derive(Clone, Debug)]
pub struct Options {
    pub dir: PathBuf,
    pub start_menu: bool,
    pub desktop: bool,
    pub file_types: bool,
    /// Settings → Apps entry (off only for tests).
    pub register: bool,
}

/// Where Painter goes when nothing else is chosen.
pub fn default_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("Programs").join("Painter")
}

/// Where an earlier install put Painter, from its Settings → Apps entry.
pub fn installed_dir() -> Option<PathBuf> {
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: the buffer and its size in bytes go together.
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(UNINSTALL_KEY),
            &HSTRING::from("InstallLocation"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if err != ERROR_SUCCESS {
        return None;
    }
    let n = (size as usize / 2).saturating_sub(1);
    let dir = PathBuf::from(String::from_utf16_lossy(&buf[..n]));
    dir.join("painter.exe").is_file().then_some(dir)
}

fn known_folder(id: &windows::core::GUID) -> Option<PathBuf> {
    // SAFETY: the returned string is freed with CoTaskMemFree, as the call wants.
    unsafe {
        let p = SHGetKnownFolderPath(id, KNOWN_FOLDER_FLAG(0), None).ok()?;
        let path = p.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(p.0 as *const _));
        path
    }
}

fn start_menu_link() -> Option<PathBuf> {
    known_folder(&FOLDERID_Programs).map(|d| d.join("Painter.lnk"))
}

fn desktop_link() -> Option<PathBuf> {
    known_folder(&FOLDERID_Desktop).map(|d| d.join("Painter.lnk"))
}

/// A shortcut at `link` to `target`.
fn shortcut(link: &Path, target: &Path) -> Result<(), String> {
    // SAFETY: COM objects used on this thread after initialising it.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let shell: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.message())?;
        shell.SetPath(&HSTRING::from(target.as_os_str())).map_err(|e| e.message())?;
        if let Some(dir) = target.parent() {
            shell.SetWorkingDirectory(&HSTRING::from(dir.as_os_str())).map_err(|e| e.message())?;
        }
        shell.SetDescription(&HSTRING::from("Paint, ink and lay out comics")).map_err(|e| e.message())?;
        shell.SetIconLocation(&HSTRING::from(target.as_os_str()), 0).map_err(|e| e.message())?;
        let file: IPersistFile = shell.cast().map_err(|e| e.message())?;
        file.Save(&HSTRING::from(link.as_os_str()), true).map_err(|e| e.message())?;
    }
    Ok(())
}

fn check(err: WIN32_ERROR, what: &str) -> Result<(), String> {
    if err == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("{what}: {}", windows::core::Error::from(err.to_hresult()).message()))
    }
}

/// The Settings → Apps entry.
fn register(dir: &Path, size_kb: u32) -> Result<(), String> {
    let exe = dir.join("painter.exe");
    let uninstall = dir.join("uninstall.exe");
    let strings = [
        ("DisplayName", "Painter".to_string()),
        ("DisplayVersion", VERSION.to_string()),
        ("Publisher", "cr0ma-gh".to_string()),
        ("DisplayIcon", exe.display().to_string()),
        ("InstallLocation", dir.display().to_string()),
        ("UninstallString", format!("\"{}\" --uninstall", uninstall.display())),
        ("URLInfoAbout", "https://cr0ma-gh.github.io/painter/".to_string()),
    ];
    // SAFETY: a key opened here, written and closed.
    unsafe {
        let mut key = HKEY::default();
        check(
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                &HSTRING::from(UNINSTALL_KEY),
                0,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut key,
                None,
            ),
            "Settings → Apps entry",
        )?;
        let mut result = Ok(());
        for (name, value) in strings {
            let wide: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
            let bytes = std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2);
            result = result.and(check(RegSetValueExW(key, &HSTRING::from(name), 0, REG_SZ, Some(bytes)), name));
        }
        for (name, value) in [("NoModify", 1u32), ("NoRepair", 1), ("EstimatedSize", size_kb)] {
            result = result.and(check(RegSetValueExW(key, &HSTRING::from(name), 0, REG_DWORD, Some(&value.to_le_bytes())), name));
        }
        let _ = RegCloseKey(key);
        result
    }
}

/// Installs Painter. `progress` hears each step (0–1 and what's going on).
pub fn install(opts: &Options, progress: &dyn Fn(f32, &'static str)) -> Result<(), String> {
    if !has_payload() {
        return Err("This installer was built without Painter inside it".into());
    }
    progress(0.05, "files");
    std::fs::create_dir_all(&opts.dir).map_err(|e| format!("{}: {e}", opts.dir.display()))?;
    let exe = opts.dir.join("painter.exe");
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::DeflateDecoder::new(PAYLOAD), &mut bytes).map_err(|e| e.to_string())?;
    progress(0.4, "files");
    // Written beside it first, so a running Painter is noticed before anything changes.
    let fresh = opts.dir.join("painter.exe.new");
    std::fs::write(&fresh, &bytes).map_err(|e| format!("{}: {e}", fresh.display()))?;
    if let Err(e) = std::fs::rename(&fresh, &exe) {
        let _ = std::fs::remove_file(&fresh);
        return Err(format!("Painter is running ({e}): close it and try again"));
    }
    // This installer stays beside it, to remove it later.
    if let Ok(me) = std::env::current_exe() {
        let uninstall = opts.dir.join("uninstall.exe");
        if me != uninstall {
            std::fs::copy(&me, &uninstall).map_err(|e| format!("{}: {e}", uninstall.display()))?;
        }
    }
    progress(0.6, "shortcuts");
    if opts.start_menu {
        if let Some(link) = start_menu_link() {
            shortcut(&link, &exe)?;
        }
    }
    if opts.desktop {
        if let Some(link) = desktop_link() {
            shortcut(&link, &exe)?;
        }
    }
    progress(0.75, "file types");
    if opts.file_types {
        let ok = std::process::Command::new(&exe).arg("--register-file-types").status().map(|s| s.success()).unwrap_or(false);
        if !ok {
            return Err("Painter couldn't set up its file types".into());
        }
    }
    progress(0.9, "apps");
    if opts.register {
        register(&opts.dir, (bytes.len() / 1024) as u32)?;
    }
    progress(1.0, "done");
    Ok(())
}

/// Removes Painter from `dir`: file types, shortcuts, the files, the Settings
/// → Apps entry; with `settings`, also Painter's settings, brushes and workspaces.
pub fn uninstall(dir: &Path, settings: bool, progress: &dyn Fn(f32, &'static str)) -> Result<(), String> {
    let exe = dir.join("painter.exe");
    progress(0.1, "file types");
    if exe.is_file() {
        let _ = std::process::Command::new(&exe).arg("--unregister-file-types").status();
    }
    progress(0.3, "shortcuts");
    for link in [start_menu_link(), desktop_link()].into_iter().flatten() {
        let _ = std::fs::remove_file(link);
    }
    progress(0.5, "files");
    for name in ["painter.exe", "uninstall.exe", "painter.exe.new"] {
        let path = dir.join(name);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("Painter is running ({e}): close it and try again"))?;
        }
    }
    let _ = std::fs::remove_dir(dir);
    if settings {
        progress(0.7, "settings");
        for var in ["APPDATA", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var) {
                let _ = std::fs::remove_dir_all(Path::new(&base).join("Painter"));
            }
        }
    }
    progress(0.9, "apps");
    // SAFETY: deletes this app's own key under the current user.
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(UNINSTALL_KEY));
    }
    progress(1.0, "done");
    Ok(())
}

/// Starts Painter.
pub fn launch(dir: &Path) {
    let _ = std::process::Command::new(dir.join("painter.exe")).current_dir(dir).spawn();
}

/// `uninstall.exe` can't delete itself while it runs: a copy in the temp
/// folder does the removing. Returns whether that copy was started.
pub fn hand_over_uninstall(dir: &Path) -> bool {
    let Ok(me) = std::env::current_exe() else { return false };
    if me.parent() != Some(dir) {
        return false;
    }
    let copy = std::env::temp_dir().join(format!("painter-uninstall-{}.exe", std::process::id()));
    std::fs::copy(&me, &copy).is_ok() && std::process::Command::new(&copy).arg("--uninstall").arg("--from").arg(dir).spawn().is_ok()
}

/// After removing: the temp copy deletes itself once it has closed.
pub fn delete_self_later() {
    let Ok(me) = std::env::current_exe() else { return };
    if me.starts_with(std::env::temp_dir()) {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("cmd")
            .args(["/C", "ping", "127.0.0.1", "-n", "3", ">", "nul", "&", "del", "/f", "/q"])
            .arg(&me)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
}

/// Whether the system speaks Italian (the installer speaks English or Italian).
pub fn italian() -> bool {
    let mut buf = [0u16; 85];
    // SAFETY: the buffer is LOCALE_NAME_MAX_LENGTH long.
    let n = unsafe { windows::Win32::Globalization::GetUserDefaultLocaleName(&mut buf) };
    n > 1 && String::from_utf16_lossy(&buf[..n as usize - 1]).to_ascii_lowercase().starts_with("it")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_and_removes_in_a_folder() {
        // Built without Painter inside: it says so and touches nothing.
        let dir = std::env::temp_dir().join(format!("painter-setup-test-{}", std::process::id()));
        let opts = Options { dir: dir.clone(), start_menu: false, desktop: false, file_types: false, register: false };
        if !has_payload() {
            assert!(install(&opts, &|_, _| {}).is_err());
            assert!(!dir.exists());
            return;
        }
        install(&opts, &|_, _| {}).unwrap();
        assert!(dir.join("painter.exe").metadata().unwrap().len() > 1_000_000);
        assert!(dir.join("uninstall.exe").is_file());
        uninstall(&dir, false, &|_, _| {}).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn default_folder_is_per_user() {
        assert!(default_dir().ends_with("Programs\\Painter") || default_dir().ends_with("Programs/Painter"));
    }
}
