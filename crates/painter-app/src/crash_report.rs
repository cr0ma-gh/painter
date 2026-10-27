//! Crash reports. A panic writes `crash-<time>.txt` to the `crash-reports`
//! folder of the app's data folder (version, system, renderer, time, the
//! panic and its backtrace). The next start finds the reports not yet seen
//! and shows them in a window that doesn't block the canvas, with a
//! prefilled GitHub issue, a copy of the report, and a way to delete it.
//! Help → View Crash Logs shows every report kept; Help → Report a Bug opens
//! an issue with the system details.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use eframe::egui;

use crate::app::PainterApp;
use crate::i18n::tr;

/// Where a new issue is opened.
const NEW_ISSUE: &str = "https://github.com/cr0ma-gh/painter/issues/new";
/// Browsers and GitHub turn away longer links; the report is cut to fit.
const MAX_URL: usize = 7000;
/// Reports kept; older ones go.
const KEEP: usize = 20;
/// A report already shown at a start (still listed in Help → View Crash Logs).
const SEEN: &str = ".seen.txt";

/// The GPU and graphics API drawing the app, once known (see [`set_renderer`]).
static RENDERER: OnceLock<String> = OnceLock::new();

/// Records the GPU drawing the app, for the reports.
pub(crate) fn set_renderer(info: &eframe::wgpu::AdapterInfo) {
    let _ = RENDERER.set(format!("{} ({:?})", info.name, info.backend));
}

/// The folder crash reports go to, `None` without a data folder.
fn reports_dir() -> Option<PathBuf> {
    Some(crate::files::app_data_dir()?.join("crash-reports"))
}

/// What a panic says: its message, else a note that it had none.
fn panic_message(info: &std::panic::PanicHookInfo) -> String {
    let payload = info.payload();
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(no message)".to_string())
}

/// The details a report and a bug report start with.
fn system_lines(renderer: Option<&str>) -> String {
    format!(
        "Version: {}\nOS: {} ({})\nRenderer: {}\n",
        crate::welcome::VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH,
        renderer.unwrap_or("unknown"),
    )
}

/// A crash report's text.
fn report_text(time: &str, thread: &str, message: &str, location: &str, backtrace: &str, renderer: Option<&str>) -> String {
    format!(
        "Painter crash report\n{}Time: {time}\nThread: {thread}\nPanic: {message}\nLocation: {location}\n\nBacktrace:\n{backtrace}\n",
        system_lines(renderer),
    )
}

/// `secs` since 1970 as `YYYY-MM-DD HH:MM:SS UTC`.
fn utc_time(secs: u64) -> String {
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC", rest / 3600, rest / 60 % 60, rest % 60)
}

/// Writes every panic to a crash report (and, on iPadOS and Android, to
/// painter-crash.txt in the app's documents folder, where it can be seen),
/// then lets the usual hook print it.
pub(crate) fn install() {
    let dir = reports_dir();
    let documents = crate::files::documents_dir();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let thread = std::thread::current();
        let location = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_default();
        let text = report_text(
            &utc_time(now.as_secs()),
            thread.name().unwrap_or("unnamed"),
            &panic_message(info),
            &location,
            &backtrace.to_string(),
            RENDERER.get().map(String::as_str),
        );
        if let Some(dir) = &dir {
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(dir.join(format!("crash-{}.txt", now.as_millis())), &text);
        }
        if let Some(documents) = &documents {
            let _ = std::fs::write(documents.join("painter-crash.txt"), &text);
        }
        previous(info);
    }));
}

/// A crash report not yet shown at a start.
fn is_pending(path: &Path) -> bool {
    !path.to_string_lossy().ends_with(SEEN)
}

/// The reports in `dir`, newest first; past the newest [`KEEP`] they're deleted.
fn reports_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut reports: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("crash-") && n.ends_with(".txt")))
        .collect();
    // `crash-<milliseconds>`: the number orders them.
    let time = |p: &PathBuf| -> u128 {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        name.trim_start_matches("crash-").split('.').next().and_then(|t| t.parse().ok()).unwrap_or(0)
    };
    reports.sort_by_key(|p| std::cmp::Reverse(time(p)));
    for old in reports.drain(reports.len().min(KEEP)..) {
        let _ = std::fs::remove_file(old);
    }
    reports
}

/// Marks a report as shown: it stays listed, but isn't offered at the next start.
fn mark_seen(path: &Path) -> PathBuf {
    if !is_pending(path) {
        return path.to_path_buf();
    }
    let seen = path.with_extension("seen.txt");
    match std::fs::rename(path, &seen) {
        Ok(()) => seen,
        Err(_) => path.to_path_buf(),
    }
}

/// `text` for a link: everything but letters, digits and `-._~` percent-encoded.
fn url_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A new GitHub issue's link, titled `title`, with `report` in a code block,
/// cut (from the end, by lines) so the link stays under [`MAX_URL`].
fn issue_url(title: &str, report: &str) -> String {
    let body = |report: &str, cut: bool| {
        let note = if cut { "\n… (cut to fit the link: use Copy Diagnostics in Painter for the whole report)" } else { "" };
        format!(
            "**What happened**\n\n<!-- What were you doing when it happened? Steps to see it again help. -->\n\n**Report**\n\n```text\n{}{note}\n```\n",
            report.trim_end()
        )
    };
    let url = |body: &str| format!("{NEW_ISSUE}?labels=bug&title={}&body={}", url_encode(title), url_encode(body));
    let mut lines: Vec<&str> = report.lines().collect();
    let mut link = url(&body(report, false));
    while link.len() > MAX_URL && !lines.is_empty() {
        let excess = link.len() - MAX_URL;
        // Encoding at most triples a byte: drop at least a third of the excess, a line at a time.
        let mut dropped = 0;
        while dropped * 3 < excess && !lines.is_empty() {
            dropped += lines.pop().map_or(0, |l| l.len() + 1);
        }
        link = url(&body(&lines.join("\n"), true));
    }
    link
}

/// The issue title for a report: its panic message.
fn issue_title(report: &str) -> String {
    let message = report.lines().find_map(|l| l.strip_prefix("Panic: ")).unwrap_or("Painter closed unexpectedly");
    let message: String = message.chars().take(80).collect();
    format!("Crash: {message}")
}

/// The crash report window (after a crash, and Help → View Crash Logs).
#[derive(Default)]
pub(crate) struct CrashReports {
    /// The window is open.
    open: bool,
    /// Opened at the start, after a crash.
    after_crash: bool,
    /// The reports, newest first, and the one shown with its text.
    reports: Vec<PathBuf>,
    selected: usize,
    text: String,
}

impl CrashReports {
    /// At the start: open, on the newest report, when one isn't seen yet.
    pub(crate) fn at_start() -> Self {
        let mut reports = Self::default();
        reports.load();
        if reports.reports.first().is_some_and(|p| is_pending(p)) {
            reports.open = true;
            reports.after_crash = true;
        }
        reports
    }

    /// Help → View Crash Logs.
    pub(crate) fn show(&mut self) {
        self.load();
        self.open = true;
        self.after_crash = false;
    }

    /// Reads the folder, showing the newest report.
    fn load(&mut self) {
        self.reports = reports_dir().map(|d| reports_in(&d)).unwrap_or_default();
        self.select(0);
    }

    fn select(&mut self, i: usize) {
        self.selected = i.min(self.reports.len().saturating_sub(1));
        self.text = self.reports.get(self.selected).and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    }

    /// Closing: the reports offered at this start won't be offered again.
    fn close(&mut self) {
        for path in &mut self.reports {
            *path = mark_seen(path);
        }
        self.open = false;
        self.after_crash = false;
    }
}

/// Help → Report a Bug: a new issue with the system details.
pub(crate) fn report_bug(ctx: &egui::Context) {
    let body = format!(
        "**What happened**\n\n<!-- What did you do, what did you expect, what happened instead? -->\n\n**System**\n\n```text\n{}```\n",
        system_lines(RENDERER.get().map(String::as_str))
    );
    let url = format!("{NEW_ISSUE}?labels=bug&body={}", url_encode(&body));
    crate::updater::open_in_browser(ctx, &url);
}

/// A human-readable label for a crash report: its UTC date and time, plus
/// a marker if it hasn't been viewed yet.
fn report_label(path: &Path) -> String {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let ms = name.trim_start_matches("crash-").split('.').next().and_then(|t| t.parse::<u64>().ok());
    if let Some(ms) = ms {
        let is_new = is_pending(path);
        let time_str = utc_time(ms / 1000);
        if is_new {
            format!("{time_str} ({})", tr("new"))
        } else {
            time_str
        }
    } else {
        name.to_string()
    }
}

impl PainterApp {
    /// The crash report window, while open.
    pub(crate) fn crash_report_window(&mut self, ctx: &egui::Context) {
        let reports = &mut self.crash_reports;
        if !reports.open {
            return;
        }
        let mut open = true;
        let mut close = false;
        egui::Window::new(tr("Crash Reports"))
            .open(&mut open)
            .collapsible(false)
            .default_width(560.0)
            .show(ctx, |ui| {
                if reports.after_crash {
                    ui.label(egui::RichText::new(tr("Painter closed unexpectedly in the last session.")).strong().size(15.0));
                    ui.label(tr("Reporting it on GitHub, with the report below, helps get it fixed."));
                    ui.add_space(4.0);
                }
                if reports.reports.is_empty() {
                    ui.label(tr("No crash reports."));
                    if ui.button(tr("Close")).clicked() {
                        close = true;
                    }
                    return;
                }
                let chosen_label = report_label(&reports.reports[reports.selected]);
                let mut chosen = reports.selected;
                egui::ComboBox::from_id_salt("crash-report")
                    .selected_text(chosen_label)
                    .width(320.0)
                    .show_ui(ui, |ui| {
                        for (i, path) in reports.reports.iter().enumerate() {
                            ui.selectable_value(&mut chosen, i, report_label(path));
                        }
                    });
                if chosen != reports.selected {
                    reports.select(chosen);
                }
                egui::ScrollArea::both().max_height(280.0).show(ui, |ui| {
                    let mut text = reports.text.as_str();
                    ui.add(egui::TextEdit::multiline(&mut text).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY));
                });
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.button(tr("Open Issue on GitHub")).clicked() {
                        crate::updater::open_in_browser(ctx, &issue_url(&issue_title(&reports.text), &reports.text));
                    }
                    if ui.button(tr("Copy Diagnostics")).clicked() {
                        ctx.copy_text(reports.text.clone());
                    }
                    if ui.button(tr("Delete Log")).clicked() {
                        let path = reports.reports.remove(reports.selected);
                        let _ = std::fs::remove_file(path);
                        reports.select(reports.selected);
                        if reports.reports.is_empty() && reports.after_crash {
                            close = true;
                        }
                    }
                    if ui.button(tr("Close")).clicked() {
                        close = true;
                    }
                });
            });
        if !open || close {
            reports.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(text: &str) -> String {
        let b = text.as_bytes();
        let (mut out, mut i) = (Vec::new(), 0);
        while i < b.len() {
            if b[i] == b'%' {
                out.push(u8::from_str_radix(&text[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn times_are_utc_dates() {
        assert_eq!(utc_time(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc_time(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(utc_time(1_793_101_234), "2026-10-27 11:40:34 UTC");
    }

    #[test]
    fn reports_carry_the_details() {
        let text = report_text("2026-10-27 11:00:34 UTC", "main", "boom", "src/app.rs:1:2", "0: frame", Some("GPU (Vulkan)"));
        for part in [crate::welcome::VERSION, std::env::consts::OS, std::env::consts::ARCH, "GPU (Vulkan)", "Panic: boom", "src/app.rs:1:2", "0: frame"] {
            assert!(text.contains(part), "{part} missing from {text}");
        }
        assert_eq!(issue_title(&text), "Crash: boom");
        assert_eq!(issue_title("nothing"), "Crash: Painter closed unexpectedly");
    }

    #[test]
    fn issue_links_are_encoded_and_fit() {
        assert_eq!(url_encode("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
        let url = issue_url("Crash: boom", "Panic: boom\nline 2");
        assert!(url.starts_with("https://github.com/cr0ma-gh/painter/issues/new?labels=bug&title=Crash%3A%20boom&body="));
        let body = decode(url.split("&body=").nth(1).unwrap());
        assert!(body.contains("```text\nPanic: boom\nline 2\n```"));
        // A long backtrace is cut, from the end, to keep the link short enough.
        let long: String = (0..2000).map(|i| format!("{i}: some::frame::name at src/file.rs:{i}\n")).collect();
        let url = issue_url("Crash: boom", &format!("Panic: boom\n{long}"));
        assert!(url.len() <= MAX_URL, "{}", url.len());
        let body = decode(url.split("&body=").nth(1).unwrap());
        assert!(body.contains("Panic: boom\n0: some::frame"));
        assert!(body.contains("cut to fit the link"));
    }

    #[test]
    fn reports_are_listed_newest_first_and_marked_seen() {
        let dir = std::env::temp_dir().join(format!("painter-crash-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for t in 1..=(KEEP + 3) {
            std::fs::write(dir.join(format!("crash-{t}.txt")), "x").unwrap();
        }
        std::fs::write(dir.join("other.txt"), "x").unwrap();
        let reports = reports_in(&dir);
        assert_eq!(reports.len(), KEEP);
        assert!(reports[0].ends_with(format!("crash-{}.txt", KEEP + 3)));
        assert!(!dir.join("crash-1.txt").exists(), "the oldest go");
        assert!(dir.join("other.txt").exists());
        let seen = mark_seen(&reports[0]);
        assert!(!is_pending(&seen) && seen.exists());
        assert_eq!(mark_seen(&seen), seen);
        // Still listed, in the same place.
        assert_eq!(reports_in(&dir)[0], seen);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn report_labels_show_utc_dates_and_new_marker() {
        let pending = PathBuf::from("crash-1793101234000.txt");
        assert_eq!(report_label(&pending), format!("{} ({})", utc_time(1_793_101_234), tr("new")));
        let seen = PathBuf::from("crash-1793101234000.seen.txt");
        assert_eq!(report_label(&seen), utc_time(1_793_101_234));
        let other = PathBuf::from("other.txt");
        assert_eq!(report_label(&other), "other.txt");
    }
}
