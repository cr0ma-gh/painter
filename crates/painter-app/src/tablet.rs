//! Pen tablet input: WinTab when a tablet driver provides it, otherwise
//! Windows Ink (winit turns `WM_POINTER` pen input into touch events with
//! normalised force, which egui forwards as `Event::Touch`). Mouse input has
//! no force and paints at full pressure.
//!
//! WinTab also reports tilt and which end of the pen is in use; flipping to
//! the eraser end switches to the eraser sub tool and back.

use eframe::egui::{Event, TouchPhase};

#[cfg(windows)]
use crate::wintab::{PenSample, WinTab};

#[derive(Default)]
pub struct TabletState {
    /// Pressure from Windows Ink touch events.
    ink_pressure: Option<f32>,
    #[cfg(windows)]
    wintab: Option<WinTab>,
    /// WinTab was tried (it is opened lazily once the window handle exists).
    #[cfg(windows)]
    wintab_tried: bool,
    /// The newest WinTab sample.
    #[cfg(windows)]
    sample: Option<PenSample>,
    /// Every WinTab sample of this frame, oldest first.
    #[cfg(windows)]
    frame_samples: Vec<PenSample>,
    /// The window, to place WinTab's desktop positions in it.
    #[cfg(windows)]
    hwnd: Option<isize>,
    /// WinTab positions agree with the pointer (checked every frame); after a
    /// few frames of disagreement they are no longer used.
    #[cfg(windows)]
    positions_ok: bool,
    #[cfg(windows)]
    position_misses: u32,
    /// Pointer events this frame and the one being handled (see `begin_events`).
    events: usize,
    event: Option<usize>,
    eraser: bool,
    eraser_switch: Option<bool>,
    /// A touch is down, and whether it is a stylus (it reports force; fingers don't).
    touching: bool,
    stylus: bool,
}

impl TabletState {
    /// Polls WinTab once per frame.
    pub fn poll(&mut self, frame: &eframe::Frame) {
        #[cfg(windows)]
        {
            if !self.wintab_tried {
                self.wintab_tried = true;
                self.hwnd = window_handle(frame);
                self.wintab = self.hwnd.and_then(WinTab::open);
            }
            self.frame_samples = self.wintab.as_mut().map(WinTab::poll).unwrap_or_default();
            if let Some(&sample) = self.frame_samples.last() {
                if sample.eraser != self.eraser {
                    self.eraser = sample.eraser;
                    self.eraser_switch = Some(sample.eraser);
                }
                self.sample = Some(sample);
            }
        }
        #[cfg(not(windows))]
        let _ = frame;
    }

    pub fn observe(&mut self, event: &Event) {
        if let Event::Touch { phase, force, .. } = event {
            match phase {
                TouchPhase::Start | TouchPhase::Move => {
                    self.touching = true;
                    self.stylus = force.is_some();
                    if let Some(force) = force {
                        self.ink_pressure = Some(force.clamp(0.0, 1.0));
                    }
                }
                TouchPhase::End | TouchPhase::Cancel => {
                    self.ink_pressure = None;
                    self.touching = false;
                }
            }
        }
    }

    /// A finger (not a pen) is on the screen.
    pub fn finger_down(&self) -> bool {
        self.touching && !self.stylus
    }

    /// Starts handling this frame's `count` pointer events. WinTab delivers
    /// several packets per frame during fast strokes; they are spread over the
    /// events in order instead of all getting the newest packet.
    pub fn begin_events(&mut self, count: usize) {
        self.events = count;
        self.event = None;
    }

    /// Moves to the next pointer event of the frame.
    pub fn next_event(&mut self) {
        self.event = Some(self.event.map_or(0, |e| e + 1));
    }

    /// The WinTab sample for the current pointer event.
    #[cfg(windows)]
    fn current(&self) -> Option<PenSample> {
        match self.event {
            Some(event) if !self.frame_samples.is_empty() => {
                Some(self.frame_samples[sample_index(event, self.events, self.frame_samples.len())])
            }
            _ => self.sample,
        }
    }

    /// Pen pressure for the current pointer event, or 1.0 for a mouse.
    pub fn pressure(&self) -> f32 {
        #[cfg(windows)]
        if let Some(sample) = self.current().filter(|s| s.touching) {
            return sample.pressure;
        }
        self.ink_pressure.unwrap_or(1.0)
    }

    /// Pen tilt for the current pointer event, 0 (upright) ..= 1 (flat). Only WinTab reports it.
    pub fn tilt(&self) -> f32 {
        #[cfg(windows)]
        if let Some(sample) = self.current() {
            return sample.tilt;
        }
        0.0
    }

    /// This frame's pen samples while touching, as window points (egui
    /// coordinates) with pressure, tilt and a time on the app clock (`now`
    /// is the app time of the newest sample). The pointer events Windows
    /// sends for a pen are few and merged; WinTab's packets come 100–200 times
    /// a second, so a fast stroke keeps its shape. Empty when WinTab isn't in
    /// use or its positions don't match the pointer (`pointer`: egui's latest
    /// pointer position, checked every frame).
    pub fn packet_points(&mut self, pixels_per_point: f32, now: f32, pointer: Option<eframe::egui::Pos2>) -> Vec<PacketPoint> {
        #[cfg(windows)]
        {
            let (Some(hwnd), Some(last)) = (self.hwnd, self.frame_samples.last()) else { return Vec::new() };
            let Some(origin) = client_origin(hwnd) else { return Vec::new() };
            let ppp = pixels_per_point.max(0.1);
            let to_points = |s: &PenSample| eframe::egui::pos2((s.screen[0] - origin[0]) / ppp, (s.screen[1] - origin[1]) / ppp);
            // Does the newest packet sit where the pointer is? (Both are this frame's latest.)
            if let Some(pointer) = pointer {
                if (to_points(last) - pointer).length() < 24.0 {
                    self.positions_ok = true;
                    self.position_misses = 0;
                } else if self.position_misses < 3 {
                    self.position_misses += 1;
                    if self.position_misses == 3 {
                        self.positions_ok = false;
                    }
                }
            }
            if !self.positions_ok || self.position_misses >= 3 {
                return Vec::new();
            }
            let newest = last.time_ms;
            self
                .frame_samples
                .iter()
                .filter(|s| s.touching)
                .map(|s| PacketPoint {
                    pos: to_points(s),
                    pressure: s.pressure,
                    tilt: s.tilt,
                    time: now - newest.wrapping_sub(s.time_ms) as f32 / 1000.0,
                })
                .collect()
        }
        #[cfg(not(windows))]
        {
            let _ = (pixels_per_point, now, pointer);
            Vec::new()
        }
    }

    /// `Some(true)` once when the eraser end comes into use, `Some(false)` when the tip returns.
    pub fn take_eraser_switch(&mut self) -> Option<bool> {
        self.eraser_switch.take()
    }

    /// Status-bar summary of the input backend and current pen state.
    pub fn describe(&self) -> String {
        #[cfg(windows)]
        if self.wintab.is_some() {
            return match self.sample {
                Some(s) => format!(
                    "WinTab · {} {:.0}% · tilt {:.0}%",
                    if s.eraser { "eraser" } else { "pen" },
                    s.pressure * 100.0,
                    s.tilt * 100.0
                ),
                None => "WinTab".into(),
            };
        }
        let pen = if cfg!(target_os = "ios") { "Apple Pencil" } else { "Windows Ink · pen" };
        match self.ink_pressure {
            Some(p) => format!("{pen} {:.0}%", p * 100.0),
            None if cfg!(target_os = "ios") => "Touch / Apple Pencil".into(),
            None => "Mouse / Windows Ink".into(),
        }
    }
}

/// One WinTab sample on the canvas: see [`TabletState::packet_points`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PacketPoint {
    pub pos: eframe::egui::Pos2,
    pub pressure: f32,
    pub tilt: f32,
    /// Seconds, on the app's clock.
    pub time: f32,
}

/// The window's client area origin on the desktop, in physical pixels.
#[cfg(windows)]
fn client_origin(hwnd: isize) -> Option<[f32; 2]> {
    #[link(name = "user32")]
    extern "system" {
        fn ClientToScreen(hwnd: isize, point: *mut [i32; 2]) -> i32;
    }
    let mut p = [0i32; 2];
    // SAFETY: `hwnd` is this app's window; `p` is a POINT (two LONGs).
    (unsafe { ClientToScreen(hwnd, &mut p) } != 0).then_some([p[0] as f32, p[1] as f32])
}

#[cfg(windows)]
fn window_handle(frame: &eframe::Frame) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match frame.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

/// Which of `samples` (oldest first) belongs to pointer event `event` of
/// `events`: both sequences span the same frame, so they're matched by position.
fn sample_index(event: usize, events: usize, samples: usize) -> usize {
    if events == 0 || samples == 0 {
        return samples.saturating_sub(1);
    }
    (((event.min(events - 1) + 1) * samples).div_ceil(events)).clamp(1, samples) - 1
}

#[cfg(test)]
mod tests {
    use super::sample_index;

    #[test]
    fn samples_spread_over_events() {
        // More packets than events: each event takes the packet at its share of the frame.
        assert_eq!((0..3).map(|e| sample_index(e, 3, 9)).collect::<Vec<_>>(), [2, 5, 8]);
        // Fewer packets than events: packets repeat, the last event gets the newest.
        assert_eq!((0..4).map(|e| sample_index(e, 4, 2)).collect::<Vec<_>>(), [0, 0, 1, 1]);
        assert_eq!(sample_index(0, 1, 5), 4, "a single event gets the newest packet");
        assert_eq!(sample_index(9, 3, 3), 2, "out-of-range events clamp");
    }
}
