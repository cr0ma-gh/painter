//! Pen tablet input. A pen driver, when there is one, gives packets with
//! pressure, tilt and which end of the pen is in use, many per frame:
//! WinTab on Windows, XInput2 on Linux (X11, and Wayland through XWayland).
//! Otherwise the pen arrives as touch events with a normalised force
//! (Windows Ink, Apple Pencil, Android styluses), which egui forwards as
//! `Event::Touch`. Mouse input has no force and paints at full pressure.
//!
//! Flipping the pen to its eraser end switches to the eraser sub tool and back.

use eframe::egui::{Event, TouchPhase};

/// One pen packet from a driver.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PenSample {
    /// 0..=1.
    pub pressure: f32,
    /// Tilt away from vertical, 0 (upright) ..= 1 (flat).
    pub tilt: f32,
    /// Tilt direction in degrees, clockwise from north.
    pub azimuth: f32,
    pub eraser: bool,
    pub touching: bool,
    /// Where the pen is, in physical pixels (sub-pixel): on the desktop for
    /// WinTab, in the window for XInput2 (see [`PenSource::origin`]).
    pub screen: [f32; 2],
    /// Driver timestamp, milliseconds.
    pub time_ms: u32,
}

/// A pen driver polled once a frame.
pub trait PenSource {
    /// Every packet since the last poll, oldest first.
    fn poll(&mut self) -> Vec<PenSample>;
    /// Where the window's client area starts in the samples' coordinates.
    fn origin(&self) -> Option<[f32; 2]>;
    fn name(&self) -> &'static str;
}

#[derive(Default)]
pub struct TabletState {
    /// Pressure from touch events (Windows Ink, Apple Pencil, Android).
    ink_pressure: Option<f32>,
    pen: Option<Box<dyn PenSource>>,
    /// The pen driver was tried (it opens lazily once the window exists).
    pen_tried: bool,
    /// The newest packet.
    sample: Option<PenSample>,
    /// Every packet of this frame, oldest first.
    frame_samples: Vec<PenSample>,
    /// Packet positions agree with the pointer (checked every frame); after a
    /// few frames of disagreement they are no longer used.
    positions_ok: bool,
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

/// The pen driver for this platform and window, if one is there.
fn open_pen(frame: &eframe::Frame) -> Option<Box<dyn PenSource>> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = frame.window_handle().ok()?.as_raw();
    match handle {
        #[cfg(windows)]
        RawWindowHandle::Win32(h) => crate::wintab::WinTab::open(h.hwnd.get()).map(|p| Box::new(p) as Box<dyn PenSource>),
        #[cfg(target_os = "linux")]
        RawWindowHandle::Xlib(h) => crate::pen_x11::XInputPen::open(h.window as u64).map(|p| Box::new(p) as Box<dyn PenSource>),
        #[cfg(target_os = "linux")]
        RawWindowHandle::Xcb(h) => crate::pen_x11::XInputPen::open(h.window.get() as u64).map(|p| Box::new(p) as Box<dyn PenSource>),
        _ => None,
    }
}

impl TabletState {
    /// Polls the pen driver once per frame.
    pub fn poll(&mut self, frame: &eframe::Frame) {
        if !self.pen_tried {
            self.pen_tried = true;
            self.pen = open_pen(frame);
        }
        self.frame_samples = self.pen.as_mut().map(|p| p.poll()).unwrap_or_default();
        if let Some(&sample) = self.frame_samples.last() {
            if sample.eraser != self.eraser {
                self.eraser = sample.eraser;
                self.eraser_switch = Some(sample.eraser);
            }
            self.sample = Some(sample);
        }
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

    /// Starts handling this frame's `count` pointer events. Drivers deliver
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

    /// The packet for the current pointer event.
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
        if let Some(sample) = self.current().filter(|s| s.touching) {
            return sample.pressure;
        }
        self.ink_pressure.unwrap_or(1.0)
    }

    /// Pen tilt for the current pointer event, 0 (upright) ..= 1 (flat). Only pen drivers report it.
    pub fn tilt(&self) -> f32 {
        self.current().map_or(0.0, |s| s.tilt)
    }

    /// This frame's pen packets while touching, as window points (egui
    /// coordinates) with pressure, tilt and a time on the app clock (`now`
    /// is the app time of the newest packet). The pointer events the system
    /// sends for a pen are few and merged; driver packets come 100–200 times
    /// a second, so a fast stroke keeps its shape. Empty without a driver or
    /// when its positions don't match the pointer (`pointer`: egui's latest
    /// pointer position, checked every frame).
    pub fn packet_points(&mut self, pixels_per_point: f32, now: f32, pointer: Option<eframe::egui::Pos2>) -> Vec<PacketPoint> {
        let (Some(pen), Some(last)) = (self.pen.as_ref(), self.frame_samples.last()) else { return Vec::new() };
        let Some(origin) = pen.origin() else { return Vec::new() };
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
        self.frame_samples
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

    /// `Some(true)` once when the eraser end comes into use, `Some(false)` when the tip returns.
    pub fn take_eraser_switch(&mut self) -> Option<bool> {
        self.eraser_switch.take()
    }

    /// Status-bar summary of the input backend and current pen state.
    pub fn describe(&self) -> String {
        if let Some(pen) = &self.pen {
            return match self.sample {
                Some(s) => format!(
                    "{} · {} {:.0}% · tilt {:.0}%",
                    pen.name(),
                    if s.eraser { "eraser" } else { "pen" },
                    s.pressure * 100.0,
                    s.tilt * 100.0
                ),
                None => pen.name().into(),
            };
        }
        let pen = if cfg!(target_os = "ios") {
            "Apple Pencil"
        } else if cfg!(target_os = "android") {
            "Stylus"
        } else if cfg!(windows) {
            "Windows Ink · pen"
        } else {
            "Pen"
        };
        match self.ink_pressure {
            Some(p) => format!("{pen} {:.0}%", p * 100.0),
            None if cfg!(target_os = "ios") => "Touch / Apple Pencil".into(),
            None if cfg!(target_os = "android") => "Touch / stylus".into(),
            None if cfg!(windows) => "Mouse / Windows Ink".into(),
            None => "Mouse".into(),
        }
    }
}

/// One pen packet on the canvas: see [`TabletState::packet_points`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PacketPoint {
    pub pos: eframe::egui::Pos2,
    pub pressure: f32,
    pub tilt: f32,
    /// Seconds, on the app's clock.
    pub time: f32,
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
