//! Updates (Help → Check for Updates; checked by itself once a day unless
//! turned off): the latest release on GitHub against this version.
//!
//! - Windows, macOS, Linux (AppImage): "Update and restart" downloads this
//!   platform's build from the release, checks it (size, and the SHA-256
//!   GitHub lists for it), puts it in place of the running program and
//!   starts it once Painter has closed (unsaved documents are asked about
//!   first, as when quitting).
//! - Android: the APK downloads in the browser; open it to install.
//! - iPad (sideloaded): the release page opens, to sideload the new IPA.
//!
//! The network goes through what each system has: `curl` on the desktop
//! (part of Windows 10 / 11 and macOS, and of nearly every Linux), Java's
//! HTTPS on Android, `NSURLSession` on iPadOS.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::app::PainterApp;

const LATEST: &str = "https://api.github.com/repos/cr0ma-gh/painter/releases/latest";
const RELEASES: &str = "https://github.com/cr0ma-gh/painter/releases/latest";
/// Automatic checks: at most this often.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);

/// This platform's file in a release.
pub(crate) fn asset_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "Painter-windows-x86_64.exe"
    } else if cfg!(target_os = "macos") {
        "Painter-macos.dmg"
    } else if cfg!(target_os = "android") {
        "Painter-android-arm64.apk"
    } else if cfg!(target_os = "ios") {
        "Painter-ipad-unsigned.ipa"
    } else {
        "Painter-linux-x86_64.AppImage"
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// Lowercase hex, when GitHub lists one.
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Release {
    /// Without the leading "v".
    pub version: String,
    pub notes: String,
    pub page: String,
    pub asset: Option<Asset>,
}

/// The latest release from GitHub's JSON, with this platform's file.
pub(crate) fn parse_release(json: &[u8], wanted: &str) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_slice(json).map_err(|e| format!("GitHub's answer: {e}"))?;
    let tag = v["tag_name"].as_str().ok_or("no release found")?;
    let asset = v["assets"].as_array().into_iter().flatten().find(|a| a["name"].as_str() == Some(wanted)).map(|a| Asset {
        name: wanted.to_string(),
        url: a["browser_download_url"].as_str().unwrap_or_default().to_string(),
        size: a["size"].as_u64().unwrap_or(0),
        sha256: a["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(str::to_ascii_lowercase),
    });
    Ok(Release {
        version: tag.trim_start_matches(['v', 'V']).to_string(),
        notes: v["body"].as_str().unwrap_or_default().to_string(),
        page: v["html_url"].as_str().unwrap_or(RELEASES).to_string(),
        asset,
    })
}

/// Whether version `a` ("0.4.10") is newer than `b`.
pub(crate) fn newer(a: &str, b: &str) -> bool {
    let parts = |s: &str| -> Vec<u64> { s.split(['.', '-', '+']).map(|p| p.parse().unwrap_or(0)).collect() };
    let (x, y) = (parts(a), parts(b));
    for i in 0..x.len().max(y.len()) {
        let (p, q) = (x.get(i).copied().unwrap_or(0), y.get(i).copied().unwrap_or(0));
        if p != q {
            return p > q;
        }
    }
    false
}

/// SHA-256 (FIPS 180-4), for checking downloads.
pub(crate) fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01,
        0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
        0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
        0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08,
        0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend(bits.to_be_bytes());
    for block in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().expect("4 bytes"));
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut s = h;
        for i in 0..64 {
            let s1 = s[4].rotate_right(6) ^ s[4].rotate_right(11) ^ s[4].rotate_right(25);
            let ch = (s[4] & s[5]) ^ (!s[4] & s[6]);
            let t1 = s[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = s[0].rotate_right(2) ^ s[0].rotate_right(13) ^ s[0].rotate_right(22);
            let maj = (s[0] & s[1]) ^ (s[0] & s[2]) ^ (s[1] & s[2]);
            let t2 = s0.wrapping_add(maj);
            s = [t1.wrapping_add(t2), s[0], s[1], s[2], s[3].wrapping_add(t1), s[4], s[5], s[6]];
        }
        for (a, b) in h.iter_mut().zip(s) {
            *a = a.wrapping_add(b);
        }
    }
    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---- the network ------------------------------------------------------------------------

/// `curl`, without a console window flashing up on Windows.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn curl() -> std::process::Command {
    let mut c = std::process::Command::new("curl");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW
        c.creation_flags(0x0800_0000);
    }
    c.args(["-fsSL", "--retry", "2", "--connect-timeout", "20", "-A", "Painter-updater"]);
    c
}

/// The body of an HTTPS GET.
fn get(url: &str) -> Result<Vec<u8>, String> {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let out =
            curl().arg(url).output().map_err(|_| "curl isn't available here: get the new version from the website".to_string())?;
        if !out.status.success() {
            return Err(format!("couldn't reach GitHub ({})", String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(out.stdout)
    }
    #[cfg(target_os = "android")]
    {
        crate::android::https_get(url).ok_or_else(|| "couldn't reach GitHub".to_string())
    }
    #[cfg(target_os = "ios")]
    {
        crate::ios_ui::https_get(url).ok_or_else(|| "couldn't reach GitHub".to_string())
    }
}

/// Opens a web page (or a download) in the system's browser.
pub(crate) fn open_in_browser(ctx: &egui::Context, url: &str) {
    #[cfg(target_os = "android")]
    {
        let _ = ctx;
        crate::android::open_url(url);
    }
    #[cfg(target_os = "ios")]
    {
        let _ = ctx;
        crate::ios_ui::open_url(url);
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    ctx.open_url(egui::OpenUrl::new_tab(url));
}

// ---- installing (desktop) -------------------------------------------------------------------

/// What to start once Painter has closed.
#[derive(Clone, Debug)]
pub(crate) struct Restart {
    program: PathBuf,
    args: Vec<String>,
}

impl Restart {
    pub(crate) fn run(&self) {
        let _ = std::process::Command::new(&self.program).args(&self.args).spawn();
    }
}

/// Where the new version goes, and how it starts: `None` where this build
/// can't replace itself (it wasn't installed from a release).
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn install_target() -> Result<PathBuf, String> {
    if cfg!(target_os = "linux") {
        return std::env::var_os("APPIMAGE")
            .map(PathBuf::from)
            .ok_or_else(|| "this copy isn't the AppImage: get the update from the website".to_string());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if cfg!(target_os = "macos") {
        return exe
            .ancestors()
            .find(|p| p.extension().is_some_and(|e| e == "app"))
            .map(Path::to_path_buf)
            .ok_or_else(|| "this copy isn't inside Painter.app".to_string());
    }
    Ok(exe)
}

/// Downloads `asset` beside `target` (counting bytes into `progress`),
/// checks it and puts it in place. Returns how to start the new version.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn download_and_install(asset: &Asset, target: &Path, progress: &AtomicU64) -> Result<Restart, String> {
    let target = target.to_path_buf();
    let dir = target.parent().ok_or("no folder")?.to_path_buf();
    // Beside the target, so it's moved in place on the same disk (on macOS: a temporary file).
    let download =
        if cfg!(target_os = "macos") { std::env::temp_dir().join(&asset.name) } else { dir.join(format!("{}.download", asset.name)) };
    let _ = std::fs::remove_file(&download);
    let mut child =
        curl().args(["-o"]).arg(&download).arg(&asset.url).spawn().map_err(|e| format!("couldn't start the download: {e}"))?;
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            if !status.success() {
                return Err(if dir.metadata().is_ok_and(|m| m.permissions().readonly()) {
                    format!("{} can't be written: update from the website instead", dir.display())
                } else {
                    "the download failed".into()
                });
            }
            break;
        }
        progress.store(std::fs::metadata(&download).map_or(0, |m| m.len()), Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(200));
    }
    let bytes = std::fs::read(&download).map_err(|e| format!("the download: {e}"))?;
    progress.store(bytes.len() as u64, Ordering::Relaxed);
    if asset.size > 0 && bytes.len() as u64 != asset.size {
        let _ = std::fs::remove_file(&download);
        return Err(format!("the download is incomplete ({} of {} bytes)", bytes.len(), asset.size));
    }
    if let Some(want) = &asset.sha256 {
        if &hex(&sha256(&bytes)) != want {
            let _ = std::fs::remove_file(&download);
            return Err("the download doesn't match the release (checksum): not installed".into());
        }
    }
    put_in_place(&download, &target)
}

/// Windows: the running .exe moves aside (Windows lets it be renamed), the new one takes its name.
#[cfg(windows)]
fn put_in_place(download: &Path, target: &Path) -> Result<Restart, String> {
    let old = target.with_extension("old.exe");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(target, &old).map_err(|e| format!("couldn't move the old version aside: {e}"))?;
    if let Err(e) = std::fs::rename(download, target) {
        let _ = std::fs::rename(&old, target);
        return Err(format!("couldn't put the new version in place: {e}"));
    }
    Ok(Restart { program: target.to_path_buf(), args: Vec::new() })
}

/// Linux: the new AppImage, executable, renamed over the old one (the running copy keeps its file).
#[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_os = "ios"))))]
fn put_in_place(download: &Path, target: &Path) -> Result<Restart, String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(download, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    std::fs::rename(download, target).map_err(|e| format!("couldn't put the new version in place: {e}"))?;
    Ok(Restart { program: target.to_path_buf(), args: Vec::new() })
}

/// macOS: the disk image mounted, its Painter.app copied beside the running one, then swapped in.
#[cfg(target_os = "macos")]
fn put_in_place(download: &Path, target: &Path) -> Result<Restart, String> {
    let run = |program: &str, args: &[&std::ffi::OsStr]| -> Result<(), String> {
        let status = std::process::Command::new(program).args(args).status().map_err(|e| format!("{program}: {e}"))?;
        status.success().then_some(()).ok_or_else(|| format!("{program} failed"))
    };
    let mount = std::env::temp_dir().join("painter-update-mount");
    let _ = std::fs::create_dir_all(&mount);
    run(
        "hdiutil",
        &[
            "attach".as_ref(),
            "-nobrowse".as_ref(),
            "-readonly".as_ref(),
            "-mountpoint".as_ref(),
            mount.as_os_str(),
            download.as_os_str(),
        ],
    )?;
    let fresh = target.with_extension("app.new");
    let _ = std::fs::remove_dir_all(&fresh);
    let copied = run("ditto", &[mount.join("Painter.app").as_os_str(), fresh.as_os_str()]);
    let _ = run("hdiutil", &["detach".as_ref(), mount.as_os_str(), "-quiet".as_ref()]);
    let _ = std::fs::remove_file(download);
    copied?;
    let old = target.with_extension("app.old");
    let _ = std::fs::remove_dir_all(&old);
    std::fs::rename(target, &old).map_err(|e| format!("couldn't move the old version aside: {e}"))?;
    if let Err(e) = std::fs::rename(&fresh, target) {
        let _ = std::fs::rename(&old, target);
        return Err(format!("couldn't put the new version in place: {e}"));
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(Restart { program: "open".into(), args: vec!["-n".into(), target.to_string_lossy().into_owned()] })
}

/// What an update before this run left behind (the old .exe on Windows).
pub(crate) fn clean_up_after_update() {
    #[cfg(windows)]
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(exe.with_extension("old.exe"));
    }
}

// ---- state and window -------------------------------------------------------------------

/// Kept between launches in the app's data folder.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct UpdateSettings {
    pub check_automatically: bool,
    /// Seconds since 1970 of the last check.
    pub last_check: u64,
    /// A version the user chose to skip.
    pub skipped: String,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self { check_automatically: true, last_check: 0, skipped: String::new() }
    }
}

fn settings_path() -> Option<PathBuf> {
    crate::files::app_data_dir().map(|d| d.join("updates.json"))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

pub(crate) enum UpdateState {
    Idle,
    /// `true`: the user asked (show the answer even when there's nothing new).
    Checking(Receiver<Result<Release, String>>, bool),
    UpToDate,
    Available(Release),
    Downloading(Release, Arc<AtomicU64>, Receiver<Result<Restart, String>>),
    Ready(Release),
    Failed(String),
}

pub(crate) struct Updater {
    pub settings: UpdateSettings,
    pub state: UpdateState,
    pub open: bool,
    /// Started once Painter has closed.
    pub restart: Option<Restart>,
    started: bool,
}

impl Default for Updater {
    fn default() -> Self {
        let settings =
            settings_path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { settings, state: UpdateState::Idle, open: false, restart: None, started: false }
    }
}

impl Updater {
    /// Help → Reset Settings: update checks as they come.
    pub(crate) fn reset_settings(&mut self) {
        self.settings = UpdateSettings::default();
        self.save();
    }

    fn save(&self) {
        if let (Some(path), Ok(json)) = (settings_path(), serde_json::to_vec_pretty(&self.settings)) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path, json);
        }
    }

    /// Asks GitHub on a thread of its own.
    pub(crate) fn check(&mut self, asked: bool) {
        if matches!(self.state, UpdateState::Checking(..) | UpdateState::Downloading(..)) {
            return;
        }
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let result = get(LATEST).and_then(|json| parse_release(&json, asset_name()));
            let _ = tx.send(result);
            #[cfg(target_os = "android")]
            crate::android::detach_thread();
        });
        self.state = UpdateState::Checking(rx, asked);
        self.settings.last_check = now();
        self.save();
    }
}

impl PainterApp {
    /// Each frame: the daily check, and answers from the threads.
    pub(crate) fn updater_tick(&mut self, ctx: &egui::Context) {
        let u = &mut self.updater;
        if !u.started {
            u.started = true;
            clean_up_after_update();
            let due = now().saturating_sub(u.settings.last_check) >= CHECK_EVERY.as_secs();
            if u.settings.check_automatically && due && self.dev_shot.is_none() {
                u.check(false);
            }
        }
        let u = &mut self.updater;
        match &u.state {
            UpdateState::Checking(rx, asked) => {
                let asked = *asked;
                if let Ok(result) = rx.try_recv() {
                    u.state = match result {
                        Ok(r) if newer(&r.version, crate::welcome::VERSION) => {
                            // Skipped versions don't come up by themselves.
                            if asked || r.version != u.settings.skipped {
                                u.open = true;
                            }
                            UpdateState::Available(r)
                        }
                        Ok(_) => UpdateState::UpToDate,
                        Err(e) => UpdateState::Failed(e),
                    };
                    if !asked && !matches!(u.state, UpdateState::Available(_)) {
                        u.state = UpdateState::Idle;
                    }
                } else {
                    ctx.request_repaint_after(Duration::from_millis(200));
                }
            }
            UpdateState::Downloading(release, _, rx) => {
                if let Ok(result) = rx.try_recv() {
                    let release = release.clone();
                    u.state = match result {
                        Ok(restart) => {
                            u.restart = Some(restart);
                            UpdateState::Ready(release)
                        }
                        Err(e) => UpdateState::Failed(e),
                    };
                } else {
                    ctx.request_repaint_after(Duration::from_millis(200));
                }
            }
            _ => {}
        }
    }

    /// Help → Check for Updates.
    pub(crate) fn check_for_updates(&mut self) {
        self.updater.open = true;
        self.updater.check(true);
    }

    pub(crate) fn updater_window(&mut self, ctx: &egui::Context) {
        if !self.updater.open {
            return;
        }
        let mut open = true;
        let mut action = None;
        egui::Window::new("Updates")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                ui.label(format!("This is Painter {}.", crate::welcome::VERSION));
                match &self.updater.state {
                    UpdateState::Idle => {}
                    UpdateState::Checking(..) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Looking for a new version…");
                        });
                    }
                    UpdateState::UpToDate => {
                        ui.label(egui::RichText::new("It's the latest version.").strong());
                    }
                    UpdateState::Available(r) => {
                        ui.label(egui::RichText::new(format!("Painter {} is out.", r.version)).strong().size(16.0));
                        if !r.notes.trim().is_empty() {
                            egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                                ui.label(egui::RichText::new(r.notes.trim()).small());
                            });
                        }
                        ui.horizontal_wrapped(|ui| {
                            let mobile = cfg!(any(target_os = "android", target_os = "ios"));
                            let label = if cfg!(target_os = "ios") {
                                "Open the release"
                            } else if mobile {
                                "Download"
                            } else {
                                "Update and restart"
                            };
                            if ui.button(egui::RichText::new(label).strong()).clicked() {
                                action = Some("install");
                            }
                            if ui.button("Later").clicked() {
                                action = Some("later");
                            }
                            if ui.button("Skip this version").clicked() {
                                action = Some("skip");
                            }
                        });
                        if cfg!(target_os = "android") {
                            ui.small("The new APK downloads in the browser: open it to install over this one.");
                        } else if cfg!(target_os = "ios") {
                            ui.small("Sideload the new IPA as you did this one (AltStore, Sideloadly…).");
                        }
                    }
                    UpdateState::Downloading(r, done, _) => {
                        let total = r.asset.as_ref().map_or(0, |a| a.size).max(1);
                        let got = done.load(Ordering::Relaxed);
                        ui.label(format!("Downloading Painter {}…", r.version));
                        ui.add(egui::ProgressBar::new(got as f32 / total as f32).show_percentage());
                    }
                    UpdateState::Ready(r) => {
                        ui.label(egui::RichText::new(format!("Painter {} is installed.", r.version)).strong());
                        ui.label("It starts when Painter closes (you're asked about unsaved documents first).");
                        if ui.button(egui::RichText::new("Restart now").strong()).clicked() {
                            action = Some("restart");
                        }
                    }
                    UpdateState::Failed(e) => {
                        ui.colored_label(egui::Color32::from_rgb(240, 150, 90), e);
                        if ui.button("Open the website").clicked() {
                            action = Some("website");
                        }
                    }
                }
                ui.separator();
                let mut auto = self.updater.settings.check_automatically;
                if ui.checkbox(&mut auto, "Check for updates once a day").changed() {
                    self.updater.settings.check_automatically = auto;
                    self.updater.save();
                }
            });
        match action {
            Some("install") => self.start_update(ctx),
            Some("later") => self.updater.open = false,
            Some("skip") => {
                if let UpdateState::Available(r) = &self.updater.state {
                    self.updater.settings.skipped = r.version.clone();
                    self.updater.save();
                }
                self.updater.open = false;
            }
            Some("restart") => self.quit_requested = true,
            Some("website") => open_in_browser(ctx, RELEASES),
            _ => {}
        }
        if !open {
            self.updater.open = false;
        }
    }

    fn start_update(&mut self, ctx: &egui::Context) {
        let UpdateState::Available(release) = &self.updater.state else { return };
        let release = release.clone();
        if cfg!(target_os = "ios") {
            open_in_browser(ctx, &release.page);
            self.updater.open = false;
            return;
        }
        let Some(asset) = release.asset.clone() else {
            open_in_browser(ctx, &release.page);
            return;
        };
        if cfg!(target_os = "android") {
            // The browser downloads it; the system installs it when opened.
            open_in_browser(ctx, &asset.url);
            self.updater.open = false;
            return;
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        {
            let progress = Arc::new(AtomicU64::new(0));
            let (tx, rx) = channel();
            let counter = progress.clone();
            std::thread::spawn(move || {
                let _ = tx.send(install_target().and_then(|target| download_and_install(&asset, &target, &counter)));
            });
            self.updater.state = UpdateState::Downloading(release, progress, rx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("0.4.10", "0.4.9"));
        assert!(newer("1.0.0", "0.9.9"));
        assert!(newer("0.5", "0.4.1"));
        assert!(!newer("0.4.1", "0.4.1"));
        assert!(!newer("0.4.0", "0.4.1"));
    }

    #[test]
    fn sha256_matches_known_values() {
        assert_eq!(hex(&sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        // Two blocks.
        let long = vec![b'a'; 1000];
        assert_eq!(hex(&sha256(&long)), "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3");
    }

    #[test]
    fn releases_are_read_from_githubs_json() {
        let json = br#"{"tag_name":"v0.5.0","html_url":"https://github.com/cr0ma-gh/painter/releases/tag/v0.5.0","body":"Notes",
            "assets":[{"name":"Painter-macos.dmg","size":10,"browser_download_url":"https://x/mac","digest":"sha256:AB"},
                      {"name":"Painter-windows-x86_64.exe","size":20,"browser_download_url":"https://x/win"}]}"#;
        let r = parse_release(json, "Painter-windows-x86_64.exe").unwrap();
        assert_eq!(r.version, "0.5.0");
        assert_eq!(r.asset.as_ref().unwrap().url, "https://x/win");
        assert_eq!(r.asset.as_ref().unwrap().sha256, None);
        let mac = parse_release(json, "Painter-macos.dmg").unwrap();
        assert_eq!(mac.asset.unwrap().sha256.as_deref(), Some("ab"));
        assert!(parse_release(json, "nothing").unwrap().asset.is_none());
        assert!(parse_release(b"{}", "x").is_err());
    }
}

#[cfg(all(test, not(any(target_os = "android", target_os = "ios", target_os = "macos"))))]
mod install_tests {
    use super::*;

    /// The whole desktop path on a local "download" (curl reads file:// URLs):
    /// fetched, checked, swapped in; a wrong checksum changes nothing.
    #[test]
    fn downloads_are_checked_and_swapped_in() {
        let dir = std::env::temp_dir().join(format!("painter-update-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join(if cfg!(windows) { "painter.exe" } else { "Painter.AppImage" });
        std::fs::write(&target, b"old").unwrap();
        let source = dir.join("release.bin");
        std::fs::write(&source, b"new version").unwrap();
        let url = format!("file:///{}", source.to_string_lossy().replace('\\', "/").trim_start_matches('/'));
        let good = Asset { name: "Painter-test.bin".into(), url, size: 11, sha256: Some(hex(&sha256(b"new version"))) };

        // A wrong checksum: refused, the old version stays.
        let bad = Asset { sha256: Some("00".repeat(32)), ..good.clone() };
        let progress = AtomicU64::new(0);
        assert!(download_and_install(&bad, &target, &progress).unwrap_err().contains("checksum"));
        assert_eq!(std::fs::read(&target).unwrap(), b"old");

        let restart = download_and_install(&good, &target, &progress).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new version");
        assert_eq!(restart.program, target);
        assert_eq!(progress.load(Ordering::Relaxed), 11);
        if cfg!(windows) {
            assert_eq!(std::fs::read(target.with_extension("old.exe")).unwrap(), b"old", "the old .exe set aside");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// `cargo test -p painter-app live_release_check -- --ignored`: asks GitHub
/// for real (only the release's description, no download).
#[cfg(test)]
#[test]
#[ignore]
fn live_release_check() {
    let json = get(LATEST).expect("GitHub answered");
    let r = parse_release(&json, asset_name()).expect("a release");
    let a = r.asset.expect("this platform's file in it");
    eprintln!("latest {} · {} {} bytes · sha256 {:?}", r.version, a.name, a.size, a.sha256);
    assert!(a.size > 1_000_000);
}
