//! Palm rejection for touch screens with a pen (iPad and Apple Pencil,
//! Android with an S-Pen, USI or Wacom EMR stylus): only the pen tip, or a
//! finger meant to paint, puts ink down; a resting hand stays inert and
//! two- and three-finger gestures keep working.
//!
//! Every touch contact goes through three stages:
//!
//! 1. **Hardware**: what the system says the contact is (a pen, its eraser
//!    end or a finger: `UITouch.type` on iPadOS, the `MotionEvent` tool
//!    type on Android) and whether it revoked the touch (UIKit's
//!    `touchesCancelled`, Android's `ACTION_CANCEL` and `FLAG_CANCELED`).
//! 2. **Geometry**: a contact wider than a fingertip is a palm; while the pen
//!    is down, hovers or has just lifted, new touches in the cone below and
//!    beside the tip on the drawing hand's side are the hand; touches next to
//!    a known palm belong to it.
//! 3. **Time**: a finger that could still be a palm is staged for a short
//!    grace period before it paints; if the pen lands or a second finger
//!    joins meanwhile, the staged samples are dropped without a trace. A
//!    contact the system revokes after it began painting rolls its stroke
//!    back ([`InkEvent::Cancel`]).
//!
//! [`PalmArbiter`] is pure logic, fed one [`ContactInput`] at a time; the
//! canvas (`canvas.rs`) turns its [`InkEvent`]s into strokes and its
//! [`NavGesture`]s into view changes.

use eframe::egui::{Pos2, Vec2};
use serde::{Deserialize, Serialize};

/// How touches on the canvas are told apart (Preferences > Input & Tablet).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum PalmMode {
    /// The pen paints at once; fingers paint after a short check, and the
    /// system's pen / finger signals and the contact size reject palms.
    #[default]
    Standard,
    /// Only the pen paints; one finger pans, more fingers navigate.
    StylusOnly,
    /// No rejection: every touch paints as the first finger (finger painting
    /// without a pen). The canvas input works as it always has.
    Off,
}

impl PalmMode {
    pub(crate) const ALL: [PalmMode; 3] = [PalmMode::Standard, PalmMode::StylusOnly, PalmMode::Off];

    /// The name in the preferences.
    pub(crate) fn label(self) -> &'static str {
        match self {
            PalmMode::Standard => "Standard (Recommended)",
            PalmMode::StylusOnly => "Stylus Only",
            PalmMode::Off => "Off",
        }
    }
}

/// The hand that holds the pen: the palm rests below the tip on its side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Handedness {
    #[default]
    Right,
    Left,
}

/// The palm rejection preferences, saved with the workspace settings.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PalmSettings {
    pub(crate) mode: PalmMode,
    pub(crate) hand: Handedness,
    /// Contacts narrower than this (millimetres, major axis) are noise.
    pub(crate) min_contact_mm: f32,
    /// Contacts wider than this (millimetres, major axis) are a palm, not a fingertip.
    pub(crate) max_contact_mm: f32,
    /// How long an ambiguous finger is held back before it paints, milliseconds.
    pub(crate) grace_ms: f32,
    /// Show how each contact was classified on the canvas (developer mode).
    pub(crate) diagnostics: bool,
}

impl Default for PalmSettings {
    fn default() -> Self {
        Self {
            // Desktops keep their mouse / Windows Ink input exactly as it was;
            // the iPad keeps its pencil-only default.
            mode: if cfg!(target_os = "ios") {
                PalmMode::StylusOnly
            } else if cfg!(target_os = "android") {
                PalmMode::Standard
            } else {
                PalmMode::Off
            },
            hand: Handedness::Right,
            min_contact_mm: 1.0,
            max_contact_mm: 18.0,
            grace_ms: 65.0,
            diagnostics: false,
        }
    }
}

impl PalmSettings {
    /// The mode in effect: "Stylus Only" needs the system to tell a pen from
    /// a finger (`kinds_known`); without that it is the standard rejection,
    /// or nothing would paint.
    pub(crate) fn effective_mode(&self, kinds_known: bool) -> PalmMode {
        match self.mode {
            PalmMode::StylusOnly if !kinds_known => PalmMode::Standard,
            mode => mode,
        }
    }

    /// "Draw With the Pen Only" (View menu) to match: on in "Stylus Only",
    /// off in "Standard"; with rejection off, `otherwise`.
    pub(crate) fn pencil_only(&self, otherwise: bool) -> bool {
        match self.mode {
            PalmMode::StylusOnly => true,
            PalmMode::Standard => false,
            PalmMode::Off => otherwise,
        }
    }

    /// The grace period clamped to the 50–80 ms where it hides a palm
    /// landing before the pen without making finger strokes feel late.
    fn grace(&self) -> f64 {
        f64::from(self.grace_ms.clamp(50.0, 80.0)) / 1000.0
    }
}

/// What the system says touched the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContactKind {
    /// An active pen's tip (Apple Pencil, S-Pen, USI, Wacom EMR).
    Stylus,
    /// The pen's eraser end.
    Eraser,
    /// A finger, or any other capacitive contact (a palm, a knuckle).
    Finger,
    /// The system doesn't say: handled as a finger.
    #[cfg_attr(not(any(target_os = "android", target_os = "ios", test)), allow(dead_code))]
    Unknown,
}

impl ContactKind {
    fn is_pen(self) -> bool {
        matches!(self, ContactKind::Stylus | ContactKind::Eraser)
    }
}

/// The stage of a contact, as in `egui::TouchPhase`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContactPhase {
    Start,
    Move,
    End,
    /// The system revoked the touch (it decided it was a palm, or another
    /// gesture took it).
    Cancel,
}

/// One touch sample for the arbiter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ContactInput {
    pub(crate) id: u64,
    pub(crate) phase: ContactPhase,
    /// Window points.
    pub(crate) pos: Pos2,
    /// Seconds, on the app clock.
    pub(crate) time: f64,
    pub(crate) kind: ContactKind,
    /// 0..=1; 1 when the contact has no pressure.
    pub(crate) pressure: f32,
    /// Major axis of the contact ellipse in millimetres, when the system reports it.
    pub(crate) major_mm: Option<f32>,
}

/// How the arbiter sees a contact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContactClass {
    /// A finger in its grace period: samples staged, nothing drawn yet.
    Pending,
    /// Puts ink down (the pen, or a finger that passed the checks).
    Ink,
    /// Part of a navigation gesture (pan, pinch, rotate, undo / redo taps).
    Navigation,
    /// Rejected: the resting hand, or noise.
    Palm,
}

/// A point of an ink stroke, in window points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct InkSample {
    pub(crate) pos: Pos2,
    pub(crate) pressure: f32,
    pub(crate) time: f64,
    /// The contact is the pen (its tilt and azimuth apply).
    pub(crate) pen: bool,
}

/// What the canvas does with the ink contact.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum InkEvent {
    /// A stroke starts here.
    Begin(InkSample),
    /// The stroke goes on.
    Move(InkSample),
    /// The stroke is done: commit it.
    End,
    /// The stroke was a mistake (a palm the system caught late, a finger that
    /// turned into a gesture): drop it without an undo step.
    Cancel,
}

/// Navigation from the fingers classed [`ContactClass::Navigation`] since the
/// last [`PalmArbiter::take_gesture`], in the shape of `egui::MultiTouchInfo`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct NavGesture {
    /// When the gesture began, seconds.
    pub(crate) start_time: f64,
    pub(crate) start_pos: Pos2,
    pub(crate) center_pos: Pos2,
    pub(crate) num_touches: usize,
    pub(crate) zoom_delta: f32,
    pub(crate) rotation_delta: f32,
    pub(crate) translation_delta: Vec2,
}

/// A contact being tracked.
#[derive(Clone, Debug)]
struct Contact {
    id: u64,
    kind: ContactKind,
    class: ContactClass,
    start: f64,
    pos: Pos2,
    /// Samples held back while [`ContactClass::Pending`].
    staged: Vec<InkSample>,
}

/// Contacts within this distance (points) of a known palm belong to it.
const PALM_CLUSTER: f32 = 70.0;
/// How long the pen counts as "just there" after it lifts or stops hovering, seconds.
const PEN_MEMORY: f64 = 0.6;
/// Radius of the exclusion cone below the pen tip, points.
const CONE_REACH: f32 = 420.0;
/// A second finger this soon after the first makes both a gesture, seconds.
const GESTURE_PAIR: f64 = 0.3;
/// A finger stroke younger than this is rolled back when the pen or a second
/// finger arrives (it was most likely the palm or the start of a gesture), seconds.
const YOUNG_STROKE: f64 = 0.3;

/// The palm rejection state: see the module docs.
#[derive(Default)]
pub(crate) struct PalmArbiter {
    contacts: Vec<Contact>,
    /// Where and when the pen was last seen (down or hovering).
    pen_seen: Option<(Pos2, f64)>,
    /// The contact that owns the stroke the canvas is drawing.
    ink: Option<u64>,
    /// Ink events not yet taken by the canvas.
    events: Vec<InkEvent>,
    /// Centroid, mean spread and mean angle of the navigation fingers at the last `take_gesture`.
    nav_last: Option<NavFrame>,
    nav_start: Option<(f64, Pos2)>,
    /// The most navigation fingers since the gesture began.
    nav_peak: usize,
}

#[derive(Clone, Copy, Debug)]
struct NavFrame {
    ids: [u64; 4],
    count: usize,
    center: Pos2,
    spread: f32,
    angle: f32,
}

impl PalmArbiter {
    /// Whether any contact is being tracked (the canvas then takes its touch
    /// input from the arbiter rather than from egui's emulated pointer).
    pub(crate) fn engaged(&self) -> bool {
        !self.contacts.is_empty() || self.ink.is_some() || !self.events.is_empty()
    }

    /// Whether a finger is in its grace period (the canvas then keeps
    /// repainting, so [`PalmArbiter::tick`] promotes it on time).
    pub(crate) fn pending(&self) -> bool {
        self.contacts.iter().any(|c| c.class == ContactClass::Pending)
    }

    /// Notes the pen hovering at `pos` (window points) at `time`: new single
    /// touches in its palm cone are rejected before they land.
    pub(crate) fn hover(&mut self, pos: Pos2, time: f64) {
        self.pen_seen = Some((pos, time));
    }

    /// Classifies one touch sample.
    pub(crate) fn feed(&mut self, input: ContactInput, settings: &PalmSettings) {
        if input.kind.is_pen() && matches!(input.phase, ContactPhase::Start | ContactPhase::Move) {
            self.pen_seen = Some((input.pos, input.time));
        }
        match input.phase {
            ContactPhase::Start => self.start(input, settings),
            ContactPhase::Move => self.moved(input),
            ContactPhase::End => self.end(input, settings),
            ContactPhase::Cancel => self.revoke(input.id),
        }
    }

    /// Promotes fingers whose grace period is over; call once a frame.
    pub(crate) fn tick(&mut self, now: f64, settings: &PalmSettings) {
        let grace = settings.grace();
        let ripe: Vec<u64> = self.contacts.iter().filter(|c| c.class == ContactClass::Pending && now - c.start >= grace).map(|c| c.id).collect();
        for id in ripe {
            self.promote(id, now);
        }
    }

    /// The ink events since the last call, in order.
    pub(crate) fn take_ink(&mut self) -> Vec<InkEvent> {
        std::mem::take(&mut self.events)
    }

    /// The class of contact `id`, if it is down.
    pub(crate) fn class_of(&self, id: u64) -> Option<ContactClass> {
        self.contacts.iter().find(|c| c.id == id).map(|c| c.class)
    }

    /// Every contact down: where, what the system says it is, and its class (for the overlay).
    pub(crate) fn contacts(&self) -> impl Iterator<Item = (Pos2, ContactKind, ContactClass)> + '_ {
        self.contacts.iter().map(|c| (c.pos, c.kind, c.class))
    }

    /// The pen's palm cone right now (tip, and the hand's side), for the overlay.
    pub(crate) fn cone(&self, now: f64, settings: &PalmSettings) -> Option<(Pos2, Handedness)> {
        self.pen_recent(now).map(|p| (p, settings.hand))
    }

    /// Fingers navigating: their motion since the last call. `None` once they lift.
    pub(crate) fn take_gesture(&mut self) -> Option<NavGesture> {
        let fingers: Vec<&Contact> = self.contacts.iter().filter(|c| c.class == ContactClass::Navigation).take(4).collect();
        if fingers.is_empty() {
            self.nav_last = None;
            self.nav_start = None;
            self.nav_peak = 0;
            return None;
        }
        let now = NavFrame::of(&fingers);
        let (start_time, start_pos) = *self.nav_start.get_or_insert((fingers.iter().map(|c| c.start).fold(f64::MAX, f64::min), now.center));
        self.nav_peak = self.nav_peak.max(now.count);
        // The same fingers as last time: their motion; a finger joined or left: start over from here.
        let (translation, zoom, rotation) = match self.nav_last {
            Some(last) if last.count == now.count && last.ids == now.ids => {
                let zoom = if last.spread > 1.0 && now.count > 1 { now.spread / last.spread } else { 1.0 };
                let mut rotation = if now.count > 1 { now.angle - last.angle } else { 0.0 };
                rotation = (rotation + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
                (now.center - last.center, zoom, rotation)
            }
            _ => (Vec2::ZERO, 1.0, 0.0),
        };
        self.nav_last = Some(now);
        Some(NavGesture {
            start_time,
            start_pos,
            center_pos: now.center,
            num_touches: self.nav_peak,
            zoom_delta: zoom,
            rotation_delta: rotation,
            translation_delta: translation,
        })
    }

    fn start(&mut self, input: ContactInput, settings: &PalmSettings) {
        // A repeated start for a contact already down: treat as a move.
        if self.contacts.iter().any(|c| c.id == input.id) {
            self.moved(input);
            return;
        }
        let class = self.classify(&input, settings);
        let sample = InkSample { pos: input.pos, pressure: input.pressure, time: input.time, pen: input.kind.is_pen() };
        let mut contact = Contact { id: input.id, kind: input.kind, class, start: input.time, pos: input.pos, staged: Vec::new() };
        match class {
            ContactClass::Ink => {
                // The pen wins: a finger stroke still young was the palm landing first.
                self.yield_ink(input.time, true);
                if self.ink.is_some() {
                    // A second pen contact while the first draws.
                    contact.class = ContactClass::Palm;
                } else {
                    self.ink = Some(input.id);
                    self.events.push(InkEvent::Begin(sample));
                }
            }
            ContactClass::Pending => contact.staged.push(sample),
            ContactClass::Navigation | ContactClass::Palm => {}
        }
        if input.kind.is_pen() {
            // Fingers still in their grace period are the hand that holds the pen.
            for c in self.contacts.iter_mut().filter(|c| c.class == ContactClass::Pending) {
                c.class = ContactClass::Palm;
                c.staged.clear();
            }
        }
        self.contacts.push(contact);
    }

    /// What a new contact is.
    fn classify(&mut self, input: &ContactInput, settings: &PalmSettings) -> ContactClass {
        if input.kind.is_pen() {
            return ContactClass::Ink;
        }
        // Too wide for a fingertip, or too small to be one.
        if let Some(major) = input.major_mm {
            if major > settings.max_contact_mm.max(settings.min_contact_mm) || major < settings.min_contact_mm {
                return ContactClass::Palm;
            }
        }
        // Next to the resting hand.
        let near_palm = self.contacts.iter().any(|c| c.class == ContactClass::Palm && (c.pos - input.pos).length() < PALM_CLUSTER);
        if near_palm {
            return ContactClass::Palm;
        }
        // Joining a gesture in progress (the third finger of a redo tap).
        if self.contacts.iter().any(|c| c.class == ContactClass::Navigation) {
            return ContactClass::Navigation;
        }
        // A second finger soon after a first one that hasn't painted much: both navigate.
        let partner = self
            .contacts
            .iter()
            .position(|c| matches!(c.class, ContactClass::Pending | ContactClass::Ink) && !c.kind.is_pen() && input.time - c.start < GESTURE_PAIR);
        if let Some(i) = partner {
            if self.pen_recent(input.time).is_none() || !self.in_cone(input.pos, input.time, settings) {
                let other = self.contacts[i].id;
                self.make_navigation(other, input.time);
                return ContactClass::Navigation;
            }
        }
        // While the pen is around: the hand that holds it.
        if self.pen_recent(input.time).is_some() {
            let pen_down = self.contacts.iter().any(|c| c.kind.is_pen());
            if pen_down || self.in_cone(input.pos, input.time, settings) {
                return ContactClass::Palm;
            }
            // Away from the pen's hand: a finger moving the view. In Standard it
            // waits, and can only turn into a gesture: it doesn't paint while
            // the pen is around (see `promote`).
            return match settings.mode {
                PalmMode::StylusOnly => ContactClass::Navigation,
                _ => ContactClass::Pending,
            };
        }
        match settings.mode {
            PalmMode::StylusOnly => ContactClass::Navigation,
            // A finger stroke on top of another stroke: the hand.
            _ if self.ink.is_some() => ContactClass::Palm,
            _ => ContactClass::Pending,
        }
    }

    /// The pen's last place, when it was seen recently.
    fn pen_recent(&self, now: f64) -> Option<Pos2> {
        self.pen_seen.filter(|(_, t)| now - t <= PEN_MEMORY).map(|(p, _)| p)
    }

    /// `pos` lies in the palm cone below the pen tip on the drawing hand's side.
    fn in_cone(&self, pos: Pos2, now: f64, settings: &PalmSettings) -> bool {
        let Some(tip) = self.pen_recent(now) else { return false };
        in_palm_cone(tip, pos, settings.hand)
    }

    fn moved(&mut self, input: ContactInput) {
        let Some(c) = self.contacts.iter_mut().find(|c| c.id == input.id) else { return };
        c.pos = input.pos;
        let sample = InkSample { pos: input.pos, pressure: input.pressure, time: input.time, pen: input.kind.is_pen() || c.kind.is_pen() };
        match c.class {
            ContactClass::Pending => c.staged.push(sample),
            ContactClass::Ink if self.ink == Some(input.id) => self.events.push(InkEvent::Move(sample)),
            _ => {}
        }
    }

    fn end(&mut self, input: ContactInput, settings: &PalmSettings) {
        self.moved(input);
        let Some(i) = self.contacts.iter().position(|c| c.id == input.id) else { return };
        // A quick finger tap with no pen around paints its dot.
        if self.contacts[i].class == ContactClass::Pending && settings.mode == PalmMode::Standard {
            self.promote(input.id, input.time);
        }
        let c = self.contacts.remove(i);
        if c.kind.is_pen() {
            self.pen_seen = Some((c.pos, input.time));
        }
        if self.ink == Some(c.id) {
            self.ink = None;
            self.events.push(InkEvent::End);
        }
    }

    /// The system took the touch back: a stroke it drew is rolled back.
    fn revoke(&mut self, id: u64) {
        let Some(i) = self.contacts.iter().position(|c| c.id == id) else { return };
        self.contacts.remove(i);
        if self.ink == Some(id) {
            self.ink = None;
            self.events.push(InkEvent::Cancel);
        }
    }

    /// A pending finger passed its grace period: it paints, from its first staged sample.
    fn promote(&mut self, id: u64, now: f64) {
        if self.ink.is_some() || self.pen_recent(now).is_some() {
            // Another contact is drawing, or the pen is around: this one is the hand.
            if let Some(c) = self.contacts.iter_mut().find(|c| c.id == id) {
                c.class = ContactClass::Palm;
                c.staged.clear();
            }
            return;
        }
        let Some(c) = self.contacts.iter_mut().find(|c| c.id == id) else { return };
        c.class = ContactClass::Ink;
        let mut staged = std::mem::take(&mut c.staged).into_iter();
        if let Some(first) = staged.next() {
            self.ink = Some(id);
            self.events.push(InkEvent::Begin(first));
            self.events.extend(staged.map(InkEvent::Move));
        }
    }

    /// The finger stroke in progress makes way for the pen (`pen`) or a
    /// gesture: rolled back when young (it was the palm landing first, or
    /// the first finger of the gesture), else finished as it is.
    fn yield_ink(&mut self, now: f64, pen: bool) {
        let Some(id) = self.ink else { return };
        let Some(c) = self.contacts.iter_mut().find(|c| c.id == id) else { return };
        if c.kind.is_pen() && pen {
            // A second pen contact: the first one keeps drawing until it lifts.
            return;
        }
        let young = now - c.start < YOUNG_STROKE;
        c.class = if pen { ContactClass::Palm } else { ContactClass::Navigation };
        self.ink = None;
        self.events.push(if young { InkEvent::Cancel } else { InkEvent::End });
    }

    /// Contact `id` (pending, or a young finger stroke) becomes part of a gesture.
    fn make_navigation(&mut self, id: u64, now: f64) {
        if self.ink == Some(id) {
            self.yield_ink(now, false);
        }
        if let Some(c) = self.contacts.iter_mut().find(|c| c.id == id) {
            c.class = ContactClass::Navigation;
            c.staged.clear();
        }
    }
}

impl NavFrame {
    fn of(fingers: &[&Contact]) -> NavFrame {
        let count = fingers.len();
        let mut ids = [0; 4];
        for (slot, c) in ids.iter_mut().zip(fingers) {
            *slot = c.id;
        }
        let sum = fingers.iter().fold(Vec2::ZERO, |s, c| s + c.pos.to_vec2());
        let center = (sum / count as f32).to_pos2();
        let spread = fingers.iter().map(|c| (c.pos - center).length()).sum::<f32>() / count as f32;
        // The angle of the first finger about the centre (the same finger every frame).
        let angle = (fingers[0].pos - center).angle();
        NavFrame { ids, count, center, spread, angle }
    }
}

/// Half the opening of the palm cone, degrees.
const CONE_HALF_ANGLE: f32 = 60.0;

/// The palm cone's axis: down and out to the hand's side, 45° from vertical.
fn cone_axis(hand: Handedness) -> Vec2 {
    let side = match hand {
        Handedness::Right => 1.0,
        Handedness::Left => -1.0,
    };
    Vec2::new(side, 1.0).normalized()
}

/// `pos` lies where the hand holding a pen at `tip` rests: below the tip
/// (screen y grows down), within 120° opening towards the hand's side
/// (right for a right hand), and within reach of the wrist.
pub(crate) fn in_palm_cone(tip: Pos2, pos: Pos2, hand: Handedness) -> bool {
    let d = pos - tip;
    let len = d.length();
    if len > CONE_REACH {
        return false;
    }
    // Right at the tip: the side of the hand curls round it.
    if len < 24.0 {
        return true;
    }
    d.normalized().dot(cone_axis(hand)) >= CONE_HALF_ANGLE.to_radians().cos()
}

/// The palm cone below a pen at `tip` as a closed outline (the tip, then
/// its far arc), for the diagnostic overlay.
pub(crate) fn cone_outline(tip: Pos2, hand: Handedness) -> Vec<Pos2> {
    let axis = cone_axis(hand).angle();
    let half = CONE_HALF_ANGLE.to_radians();
    let steps = 16;
    std::iter::once(tip)
        .chain((0..=steps).map(|i| {
            let a = axis - half + 2.0 * half * i as f32 / steps as f32;
            tip + Vec2::angled(a) * CONE_REACH
        }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(mode: PalmMode) -> PalmSettings {
        PalmSettings { mode, ..PalmSettings::default() }
    }

    fn touch(id: u64, phase: ContactPhase, x: f32, y: f32, ms: f64, kind: ContactKind) -> ContactInput {
        ContactInput { id, phase, pos: Pos2::new(x, y), time: ms / 1000.0, kind, pressure: 0.5, major_mm: None }
    }

    fn finger(id: u64, phase: ContactPhase, x: f32, y: f32, ms: f64) -> ContactInput {
        touch(id, phase, x, y, ms, ContactKind::Finger)
    }

    fn pen(id: u64, phase: ContactPhase, x: f32, y: f32, ms: f64) -> ContactInput {
        touch(id, phase, x, y, ms, ContactKind::Stylus)
    }

    use ContactPhase::{Cancel, End, Move, Start};

    #[test]
    fn pen_paints_at_once() {
        let s = settings(PalmMode::Standard);
        let mut a = PalmArbiter::default();
        a.feed(pen(1, Start, 10.0, 10.0, 0.0), &s);
        a.feed(pen(1, Move, 12.0, 10.0, 8.0), &s);
        a.feed(pen(1, End, 14.0, 10.0, 16.0), &s);
        let ink = a.take_ink();
        assert!(matches!(ink[0], InkEvent::Begin(p) if p.pen && p.pos == Pos2::new(10.0, 10.0)));
        assert!(matches!(ink[1], InkEvent::Move(_)));
        assert!(matches!(ink[3], InkEvent::End), "{ink:?}");
        assert!(!a.engaged());
    }

    #[test]
    fn palm_before_pen_leaves_nothing() {
        // The palm lands 30 ms before the tip: its staged samples are dropped.
        let s = settings(PalmMode::Standard);
        let mut a = PalmArbiter::default();
        a.feed(finger(1, Start, 300.0, 400.0, 0.0), &s);
        a.feed(finger(1, Move, 302.0, 401.0, 16.0), &s);
        a.tick(0.016, &s);
        assert!(a.take_ink().is_empty(), "nothing drawn during the grace period");
        assert_eq!(a.class_of(1), Some(ContactClass::Pending));
        a.feed(pen(2, Start, 200.0, 250.0, 30.0), &s);
        assert_eq!(a.class_of(1), Some(ContactClass::Palm));
        a.tick(0.2, &s);
        a.feed(finger(1, Move, 320.0, 420.0, 200.0), &s);
        a.feed(finger(1, End, 320.0, 420.0, 400.0), &s);
        let ink = a.take_ink();
        assert!(ink.iter().all(|e| match e {
            InkEvent::Begin(p) | InkEvent::Move(p) => p.pen,
            _ => true,
        }));
        assert!(matches!(ink[0], InkEvent::Begin(p) if p.pen));
    }

    #[test]
    fn finger_paints_after_grace_from_its_first_sample() {
        let s = settings(PalmMode::Standard);
        let mut a = PalmArbiter::default();
        a.feed(finger(1, Start, 10.0, 10.0, 0.0), &s);
        a.feed(finger(1, Move, 20.0, 10.0, 30.0), &s);
        a.tick(0.03, &s);
        assert!(a.take_ink().is_empty());
        a.tick(0.07, &s);
        let ink = a.take_ink();
        assert_eq!(ink.len(), 2);
        assert!(matches!(ink[0], InkEvent::Begin(p) if p.pos == Pos2::new(10.0, 10.0) && !p.pen));
        a.feed(finger(1, Move, 30.0, 10.0, 80.0), &s);
        a.feed(finger(1, End, 30.0, 10.0, 90.0), &s);
        let ink = a.take_ink();
        assert!(matches!(ink.last(), Some(InkEvent::End)));
    }

    #[test]
    fn quick_tap_paints_a_dot() {
        let s = settings(PalmMode::Standard);
        let mut a = PalmArbiter::default();
        a.feed(finger(1, Start, 10.0, 10.0, 0.0), &s);
        a.feed(finger(1, End, 10.0, 10.0, 20.0), &s);
        let ink = a.take_ink();
        assert!(matches!(ink[0], InkEvent::Begin(_)));
        assert!(matches!(ink.last(), Some(InkEvent::End)));
    }

    #[test]
    fn wide_contacts_are_palms() {
        let s = settings(PalmMode::Standard);
        let mut a = PalmArbiter::default();
        a.feed(ContactInput { major_mm: Some(32.0), ..finger(1, Start, 10.0, 10.0, 0.0) }, &s);
        assert_eq!(a.class_of(1), Some(ContactClass::Palm));
        a.feed(ContactInput { major_mm: Some(9.0), ..finger(2, Start, 500.0, 10.0, 400.0) }, &s);
        assert_eq!(a.class_of(2), Some(ContactClass::Pending), "a fingertip far from the palm");
        // Next to the palm: part of the hand.
        a.feed(finger(3, Start, 40.0, 40.0, 800.0), &s);
        assert_eq!(a.class_of(3), Some(ContactClass::Palm));
        // A stricter threshold for thick screen protectors.
        let strict = PalmSettings { max_contact_mm: 8.0, ..s };
        let mut b = PalmArbiter::default();
        b.feed(ContactInput { major_mm: Some(9.0), ..finger(1, Start, 10.0, 10.0, 0.0) }, &strict);
        assert_eq!(b.class_of(1), Some(ContactClass::Palm));
    }

    #[test]
    fn hover_blocks_touches_in_the_hand_cone() {
        let s = settings(PalmMode::Standard);
        let mut a = PalmArbiter::default();
        a.hover(Pos2::new(400.0, 300.0), 0.0);
        // Right hand: below and to the right of the tip.
        a.feed(finger(1, Start, 520.0, 450.0, 50.0), &s);
        assert_eq!(a.class_of(1), Some(ContactClass::Palm));
        // Left-handed artists rest on the other side.
        let left = PalmSettings { hand: Handedness::Left, ..s };
        let mut b = PalmArbiter::default();
        b.hover(Pos2::new(400.0, 300.0), 0.0);
        b.feed(finger(1, Start, 520.0, 450.0, 50.0), &left);
        assert_eq!(b.class_of(1), Some(ContactClass::Pending), "outside the cone: could still be a gesture");
        b.tick(0.2, &left);
        assert_eq!(b.class_of(1), Some(ContactClass::Palm), "no finger ink while the pen is around");
        assert!(b.take_ink().is_empty());
        assert!(!in_palm_cone(Pos2::new(400.0, 300.0), Pos2::new(520.0, 450.0), Handedness::Left));
        assert!(in_palm_cone(Pos2::new(400.0, 300.0), Pos2::new(280.0, 450.0), Handedness::Left));
        assert!(!in_palm_cone(Pos2::new(400.0, 300.0), Pos2::new(400.0, 100.0), Handedness::Right), "above the tip");
    }

    #[test]
    fn stylus_only_fingers_navigate_away_from_the_hand() {
        let s = settings(PalmMode::StylusOnly);
        let mut a = PalmArbiter::default();
        a.feed(finger(1, Start, 100.0, 100.0, 0.0), &s);
        assert_eq!(a.class_of(1), Some(ContactClass::Navigation));
        assert!(a.take_gesture().is_some());
        a.feed(finger(1, Move, 130.0, 110.0, 16.0), &s);
        let g = a.take_gesture().unwrap();
        assert_eq!(g.translation_delta, Vec2::new(30.0, 10.0));
        assert_eq!(g.num_touches, 1);
        a.feed(finger(1, End, 130.0, 110.0, 30.0), &s);
        assert!(a.take_gesture().is_none());
        assert!(a.take_ink().is_empty());
        // The pen draws; a touch in its palm cone is the hand.
        a.feed(pen(2, Start, 300.0, 300.0, 100.0), &s);
        a.feed(finger(3, Start, 400.0, 420.0, 120.0), &s);
        assert_eq!(a.class_of(3), Some(ContactClass::Palm));
        assert!(a.take_gesture().is_none(), "the palm doesn't pan");
    }

    #[test]
    fn two_fingers_pinch_instead_of_painting() {
        let s = settings(PalmMode::Standard);
        let mut a = PalmArbiter::default();
        a.feed(finger(1, Start, 100.0, 100.0, 0.0), &s);
        a.feed(finger(2, Start, 200.0, 100.0, 40.0), &s);
        assert_eq!(a.class_of(1), Some(ContactClass::Navigation));
        assert_eq!(a.class_of(2), Some(ContactClass::Navigation));
        a.tick(0.5, &s);
        assert!(a.take_ink().is_empty(), "the staged first finger left nothing");
        a.take_gesture();
        a.feed(finger(1, Move, 50.0, 100.0, 60.0), &s);
        a.feed(finger(2, Move, 250.0, 100.0, 60.0), &s);
        let g = a.take_gesture().unwrap();
        assert!((g.zoom_delta - 2.0).abs() < 1e-4, "{g:?}");
        assert_eq!(g.translation_delta, Vec2::ZERO);
        assert_eq!(g.num_touches, 2);
        // Rotating a quarter turn about the centre.
        a.feed(finger(1, Move, 150.0, 0.0, 80.0), &s);
        a.feed(finger(2, Move, 150.0, 200.0, 80.0), &s);
        let g = a.take_gesture().unwrap();
        assert!((g.rotation_delta - std::f32::consts::FRAC_PI_2).abs() < 1e-4, "{g:?}");
    }

    #[test]
    fn pinch_right_after_the_pen_lifts() {
        let s = settings(PalmMode::Standard);
        let mut a = PalmArbiter::default();
        a.feed(pen(1, Start, 300.0, 300.0, 0.0), &s);
        a.feed(pen(1, End, 310.0, 300.0, 100.0), &s);
        a.take_ink();
        // The other hand, above the tip.
        a.feed(finger(2, Start, 200.0, 100.0, 200.0), &s);
        a.feed(finger(3, Start, 400.0, 100.0, 230.0), &s);
        assert_eq!(a.class_of(2), Some(ContactClass::Navigation));
        assert_eq!(a.class_of(3), Some(ContactClass::Navigation));
        a.tick(0.5, &s);
        assert!(a.take_ink().is_empty());
    }

    #[test]
    fn a_long_finger_stroke_is_kept_when_the_pen_lands() {
        let s = settings(PalmMode::Standard);
        let mut a = PalmArbiter::default();
        a.feed(finger(1, Start, 10.0, 10.0, 0.0), &s);
        a.tick(0.1, &s);
        a.take_ink();
        a.feed(pen(2, Start, 600.0, 100.0, 2000.0), &s);
        let ink = a.take_ink();
        assert_eq!(ink[0], InkEvent::End);
        assert!(matches!(ink[1], InkEvent::Begin(p) if p.pen));
        // A second pen contact while the first draws is ignored.
        a.feed(pen(3, Start, 50.0, 50.0, 2100.0), &s);
        assert_eq!(a.class_of(3), Some(ContactClass::Palm));
        assert!(a.take_ink().is_empty());
    }

    #[test]
    fn three_finger_tap_counts_three() {
        let s = settings(PalmMode::Standard);
        let mut a = PalmArbiter::default();
        a.feed(finger(1, Start, 100.0, 100.0, 0.0), &s);
        a.feed(finger(2, Start, 200.0, 100.0, 20.0), &s);
        a.feed(finger(3, Start, 300.0, 100.0, 30.0), &s);
        assert_eq!(a.take_gesture().unwrap().num_touches, 3);
    }

    #[test]
    fn a_young_finger_stroke_turning_into_a_gesture_is_rolled_back() {
        let s = settings(PalmMode::Standard);
        let mut a = PalmArbiter::default();
        a.feed(finger(1, Start, 100.0, 100.0, 0.0), &s);
        a.tick(0.07, &s);
        assert!(matches!(a.take_ink()[0], InkEvent::Begin(_)));
        a.feed(finger(2, Start, 300.0, 100.0, 150.0), &s);
        assert_eq!(a.take_ink(), vec![InkEvent::Cancel]);
        assert_eq!(a.class_of(1), Some(ContactClass::Navigation));
    }

    #[test]
    fn the_pen_rolls_back_a_palm_stroke() {
        let s = settings(PalmMode::Standard);
        let mut a = PalmArbiter::default();
        a.feed(finger(1, Start, 500.0, 500.0, 0.0), &s);
        a.tick(0.1, &s);
        a.feed(finger(1, Move, 505.0, 500.0, 120.0), &s);
        assert!(matches!(a.take_ink()[0], InkEvent::Begin(_)));
        a.feed(pen(2, Start, 300.0, 300.0, 200.0), &s);
        let ink = a.take_ink();
        assert_eq!(ink[0], InkEvent::Cancel);
        assert!(matches!(ink[1], InkEvent::Begin(p) if p.pen));
        assert_eq!(a.class_of(1), Some(ContactClass::Palm));
    }

    #[test]
    fn a_revoked_touch_rolls_its_stroke_back() {
        let s = settings(PalmMode::Standard);
        let mut a = PalmArbiter::default();
        a.feed(finger(1, Start, 10.0, 10.0, 0.0), &s);
        a.tick(0.1, &s);
        a.feed(finger(1, Move, 15.0, 10.0, 110.0), &s);
        a.feed(finger(1, Cancel, 15.0, 10.0, 130.0), &s);
        let ink = a.take_ink();
        assert_eq!(ink.last(), Some(&InkEvent::Cancel));
        assert!(!a.engaged());
        // A revoked pending touch leaves nothing at all.
        a.feed(finger(2, Start, 10.0, 10.0, 200.0), &s);
        a.feed(finger(2, Cancel, 10.0, 10.0, 220.0), &s);
        a.tick(1.0, &s);
        assert!(a.take_ink().is_empty());
    }

    #[test]
    fn a_second_finger_on_an_old_stroke_is_the_hand() {
        let s = settings(PalmMode::Standard);
        let mut a = PalmArbiter::default();
        a.feed(finger(1, Start, 10.0, 10.0, 0.0), &s);
        a.tick(0.1, &s);
        a.take_ink();
        a.feed(finger(2, Start, 600.0, 600.0, 900.0), &s);
        assert_eq!(a.class_of(2), Some(ContactClass::Palm));
        assert!(a.take_ink().is_empty(), "the stroke goes on");
    }

    #[test]
    fn classification_is_cheap() {
        // Well under the 0.5 ms budget per event, even unoptimised.
        let s = settings(PalmMode::Standard);
        let mut a = PalmArbiter::default();
        let started = std::time::Instant::now();
        let n = 20_000;
        for i in 0..n {
            let t = i as f64 * 4.0;
            let id = (i / 50) as u64;
            let phase = match i % 50 {
                0 => Start,
                49 => End,
                _ => Move,
            };
            a.feed(finger(id, phase, (i % 700) as f32, 300.0, t), &s);
            a.feed(finger(id + 10_000, phase, 900.0, (i % 500) as f32, t), &s);
            a.tick(t / 1000.0, &s);
            a.take_ink();
            a.take_gesture();
        }
        let per_event = started.elapsed().as_secs_f64() / (2 * n) as f64;
        assert!(per_event < 0.000_5, "{per_event} s per event");
    }

    #[test]
    fn the_overlay_cone_matches_the_test() {
        let tip = Pos2::new(400.0, 300.0);
        for hand in [Handedness::Right, Handedness::Left] {
            let outline = cone_outline(tip, hand);
            assert_eq!(outline[0], tip);
            // Just inside the arc, on the rays between the cone's edges.
            for p in &outline[2..outline.len() - 1] {
                assert!(in_palm_cone(tip, tip + (*p - tip) * 0.99, hand), "{hand:?} {p:?}");
            }
        }
    }

    #[test]
    fn pending_fingers_are_reported() {
        let s = settings(PalmMode::Standard);
        let mut a = PalmArbiter::default();
        assert!(!a.pending());
        a.feed(finger(1, Start, 10.0, 10.0, 0.0), &s);
        assert!(a.pending());
        a.tick(0.1, &s);
        assert!(!a.pending());
    }

    #[test]
    fn stylus_only_needs_to_tell_pen_from_finger() {
        let s = settings(PalmMode::StylusOnly);
        assert_eq!(s.effective_mode(true), PalmMode::StylusOnly);
        assert_eq!(s.effective_mode(false), PalmMode::Standard);
        assert_eq!(settings(PalmMode::Off).effective_mode(false), PalmMode::Off);
        assert!(s.pencil_only(false));
        assert!(!settings(PalmMode::Standard).pencil_only(true));
        assert!(settings(PalmMode::Off).pencil_only(true));
    }

    #[test]
    fn settings_round_trip_and_fill_in() {
        let s = PalmSettings { mode: PalmMode::StylusOnly, hand: Handedness::Left, diagnostics: true, ..PalmSettings::default() };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<PalmSettings>(&json).unwrap(), s);
        let partial: PalmSettings = serde_json::from_str(r#"{"hand":"Left"}"#).unwrap();
        assert_eq!(partial.hand, Handedness::Left);
        assert_eq!(partial.grace_ms, PalmSettings::default().grace_ms);
    }
}
