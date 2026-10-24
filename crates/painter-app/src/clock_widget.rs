//! A small clock and battery chip in the canvas's top-right corner, for
//! full-screen and zen painting (and the iPad, whose status bar Painter
//! hides). View → Clock and Battery, or right-click it → Hide.

use std::sync::{Arc, Mutex};
use crate::i18n::tr;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, Rect, Sense, Stroke, Vec2};

use crate::app::PainterApp;
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Battery {
    pub percent: u8,
    pub charging: bool,
}

#[derive(Default)]
pub(crate) struct ClockWidget {
    battery: Arc<Mutex<Option<Battery>>>,
    checked: Option<Instant>,
}

/// Hours and minutes, local time.
fn local_time() -> Option<(u32, u32)> {
    #[cfg(windows)]
    {
        #[repr(C)]
        #[derive(Default)]
        struct SystemTime {
            year: u16,
            month: u16,
            weekday: u16,
            day: u16,
            hour: u16,
            minute: u16,
            second: u16,
            millis: u16,
        }
        extern "system" {
            fn GetLocalTime(time: *mut SystemTime);
        }
        let mut t = SystemTime::default();
        // SAFETY: GetLocalTime fills the struct it's given.
        unsafe { GetLocalTime(&mut t) };
        Some((t.hour as u32, t.minute as u32))
    }
    #[cfg(unix)]
    {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as libc::time_t;
        // SAFETY: localtime_r writes into the zeroed struct it's given.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if unsafe { libc::localtime_r(&now, &mut tm) }.is_null() {
            return None;
        }
        Some((tm.tm_hour as u32, tm.tm_min as u32))
    }
    #[cfg(not(any(windows, unix)))]
    None
}

/// The battery's charge, when this computer has one.
fn read_battery() -> Option<Battery> {
    #[cfg(windows)]
    {
        #[repr(C)]
        #[derive(Default)]
        struct PowerStatus {
            ac_line: u8,
            flags: u8,
            percent: u8,
            saver: u8,
            life: u32,
            full_life: u32,
        }
        extern "system" {
            fn GetSystemPowerStatus(status: *mut PowerStatus) -> i32;
        }
        let mut s = PowerStatus::default();
        // SAFETY: GetSystemPowerStatus fills the struct it's given.
        if unsafe { GetSystemPowerStatus(&mut s) } == 0 || s.flags & 128 != 0 || s.percent > 100 {
            return None;
        }
        Some(Battery { percent: s.percent, charging: s.ac_line == 1 })
    }
    #[cfg(target_os = "macos")]
    {
        // "Now drawing from 'Battery Power' … 82%; discharging; …"
        let out = std::process::Command::new("pmset").args(["-g", "batt"]).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text.lines().find(|l| l.contains('%'))?;
        let pct = line.split('%').next()?.rsplit(|c: char| !c.is_ascii_digit()).next()?.parse::<u8>().ok()?;
        Some(Battery { percent: pct.min(100), charging: text.contains("AC Power") })
    }
    #[cfg(target_os = "ios")]
    {
        extern "C" {
            fn painter_battery_level(charging: *mut std::ffi::c_int) -> f64;
        }
        let mut charging = 0;
        // SAFETY: a plain UIKit read on the main thread; the pointer is to a local.
        let level = unsafe { painter_battery_level(&mut charging) };
        (level >= 0.0).then(|| Battery { percent: (level * 100.0).round() as u8, charging: charging != 0 })
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let dir = std::fs::read_dir("/sys/class/power_supply").ok()?;
        for entry in dir.flatten() {
            let p = entry.path();
            let kind = std::fs::read_to_string(p.join("type")).unwrap_or_default();
            if kind.trim() != "Battery" {
                continue;
            }
            let Some(pct) = std::fs::read_to_string(p.join("capacity")).ok().and_then(|c| c.trim().parse::<u8>().ok()) else {
                continue;
            };
            let status = std::fs::read_to_string(p.join("status")).unwrap_or_default();
            return Some(Battery { percent: pct.min(100), charging: matches!(status.trim(), "Charging" | "Full") });
        }
        None
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "ios", target_os = "linux", target_os = "android")))]
    None
}

impl PainterApp {
    /// The clock and battery in the canvas's top-right corner.
    pub(crate) fn draw_clock(&mut self, ui: &mut egui::Ui, canvas: Rect) {
        if !self.ws.chrome.clock {
            return;
        }
        // Battery: every half minute, off the UI thread (pmset takes a moment).
        if self.clock.checked.is_none_or(|t| t.elapsed() > Duration::from_secs(30)) {
            self.clock.checked = Some(Instant::now());
            if cfg!(target_os = "ios") {
                *self.clock.battery.lock().unwrap_or_else(|e| e.into_inner()) = read_battery();
            } else {
                let slot = self.clock.battery.clone();
                std::thread::Builder::new()
                    .name("battery".into())
                    .spawn(move || *slot.lock().unwrap_or_else(|e| e.into_inner()) = read_battery())
                    .ok();
            }
        }
        let battery = *self.clock.battery.lock().unwrap_or_else(|e| e.into_inner());
        let Some((h, m)) = local_time() else { return };
        let time = format!("{h:02}:{m:02}");
        let font = FontId::proportional(12.0);
        let painter = ui.painter_at(canvas);
        let time_w = painter.layout_no_wrap(time.clone(), font.clone(), theme::text()).size().x;
        let battery_text = battery.map(|b| format!("{}%", b.percent));
        let battery_w =
            battery_text.as_ref().map_or(0.0, |t| painter.layout_no_wrap(t.clone(), font.clone(), theme::text()).size().x + 26.0);
        let size = Vec2::new(time_w + battery_w + 16.0, 22.0);
        let chip = Rect::from_min_size(canvas.right_top() + Vec2::new(-size.x - 8.0, 8.0), size);
        let response = ui.interact(chip, ui.id().with("clock_widget"), Sense::click());
        painter.rect_filled(chip, 11.0, theme::panel().gamma_multiply(0.85));
        painter.rect_stroke(chip, 11.0, Stroke::new(1.0_f32, theme::line()), egui::StrokeKind::Inside);
        let mut x = chip.left() + 8.0;
        painter.text(egui::pos2(x, chip.center().y), Align2::LEFT_CENTER, &time, font.clone(), theme::text());
        x += time_w + 8.0;
        if let (Some(b), Some(text)) = (battery, battery_text) {
            // A battery outline filled to the charge; red when low, a bolt when charging.
            let body = Rect::from_min_size(egui::pos2(x, chip.center().y - 4.5), Vec2::new(16.0, 9.0));
            painter.rect_stroke(body, 1.5, Stroke::new(1.0_f32, theme::text()), egui::StrokeKind::Inside);
            painter.rect_filled(
                Rect::from_min_size(egui::pos2(body.right(), body.center().y - 2.0), Vec2::new(1.5, 4.0)),
                0.0,
                theme::text(),
            );
            let fill = if b.percent <= 15 && !b.charging { Color32::from_rgb(230, 80, 70) } else { theme::text() };
            let inner = body.shrink(2.0);
            painter.rect_filled(
                Rect::from_min_size(inner.min, Vec2::new(inner.width() * b.percent as f32 / 100.0, inner.height())),
                0.5,
                fill,
            );
            if b.charging {
                let c = body.center();
                let bolt = vec![
                    c + Vec2::new(1.0, -5.0),
                    c + Vec2::new(-3.0, 1.0),
                    c + Vec2::new(0.0, 1.0),
                    c + Vec2::new(-1.0, 5.0),
                    c + Vec2::new(3.0, -1.0),
                    c + Vec2::new(0.0, -1.0),
                ];
                painter.add(egui::Shape::closed_line(bolt, Stroke::new(1.2_f32, Color32::from_rgb(250, 210, 80))));
            }
            painter.text(egui::pos2(body.right() + 6.0, chip.center().y), Align2::LEFT_CENTER, text, font, theme::text());
        }
        let hint = match battery {
            Some(b) => format!(
                "Battery {}%{}. Right-click to hide (View > Clock and Battery)",
                b.percent,
                if b.charging { ", charging" } else { "" }
            ),
            None => "Right-click to hide (View > Clock and Battery)".to_string(),
        };
        response.on_hover_text(hint).context_menu(|ui| {
            if ui.button(tr("Hide Clock and Battery")).clicked() {
                self.ws.chrome.clock = false;
                ui.close_menu();
            }
        });
        // The minutes change.
        ui.ctx().request_repaint_after(Duration::from_secs(10));
    }
}
