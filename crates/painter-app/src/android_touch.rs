//! What Android says about each touch, for palm rejection (`crate::palm`):
//! the pointer's tool type (pen tip, eraser end, finger, palm), the major
//! axis of its contact ellipse, and whether the system revoked it
//! (`FLAG_CANCELED` on its up event) — the `MotionEvent` details winit drops
//! when it turns input into `Touch` events.
//!
//! winit reads the activity's input queue through `android-activity` and the
//! `ndk` crate, one event at a time with `AInputQueue_getEvent`. This crate
//! defines a function of that name, so the link binds that call here: it
//! fetches the event from libandroid's own function, notes its pointers and
//! hands it on unchanged. Pointer ids are the ids of winit's (and egui's)
//! touches, so [`pointer`] and [`take_revoked`] answer for an egui touch of
//! the same frame.
//!
//! [`Pointers`] is pure logic, tested on every platform; the hook itself is
//! Android only.

use crate::palm::ContactKind;

const ACTION_MASK: i32 = 0xff;
const ACTION_POINTER_INDEX_SHIFT: i32 = 8;
const ACTION_DOWN: i32 = 0;
const ACTION_UP: i32 = 1;
const ACTION_POINTER_DOWN: i32 = 5;
const ACTION_POINTER_UP: i32 = 6;
/// `AMOTION_EVENT_FLAG_CANCELED`: the up event ends a touch the system
/// decided was unintended (a palm), Android 13+.
const FLAG_CANCELED: i32 = 0x20;

const TOOL_TYPE_FINGER: i32 = 1;
const TOOL_TYPE_STYLUS: i32 = 2;
const TOOL_TYPE_ERASER: i32 = 4;
const TOOL_TYPE_PALM: i32 = 5;

/// One pointer of a `MotionEvent`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PointerSample {
    /// The pointer id (winit's and egui's touch id).
    pub(crate) id: u64,
    /// `AMotionEvent_getToolType`.
    pub(crate) tool: i32,
    /// `AMOTION_EVENT_AXIS_TOUCH_MAJOR`, in screen pixels; 0 when not reported.
    pub(crate) touch_major: f32,
}

/// What is known of a pointer: its latest sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PointerInfo {
    /// `AMotionEvent_getToolType`.
    pub(crate) tool: i32,
    /// Major axis of the contact ellipse in screen pixels; 0 when not reported.
    pub(crate) touch_major: f32,
}

/// The pointers seen in the input queue, by id.
#[derive(Debug, Default)]
pub(crate) struct Pointers {
    info: Vec<(u64, PointerInfo)>,
    /// Pointers whose up event carried `FLAG_CANCELED`, until their touch's
    /// end is read ([`Pointers::take_revoked`]) or the id goes down again.
    revoked: Vec<u64>,
}

impl Pointers {
    /// Notes one `MotionEvent`: its `action` (with the pointer index),
    /// `flags`, and every pointer in it.
    pub(crate) fn observe(&mut self, action: i32, flags: i32, pointers: &[PointerSample]) {
        let kind = action & ACTION_MASK;
        let index = ((action >> ACTION_POINTER_INDEX_SHIFT) & 0xff) as usize;
        let acting = pointers.get(index).map(|p| p.id);
        if matches!(kind, ACTION_DOWN | ACTION_POINTER_DOWN) {
            // A new touch with an old id: an unread revocation was another touch's.
            if let Some(id) = acting {
                self.revoked.retain(|r| *r != id);
            }
        }
        for p in pointers {
            let info = PointerInfo { tool: p.tool, touch_major: p.touch_major };
            match self.info.iter_mut().find(|(id, _)| *id == p.id) {
                Some(entry) => entry.1 = info,
                None => self.info.push((p.id, info)),
            }
        }
        if matches!(kind, ACTION_UP | ACTION_POINTER_UP) && flags & FLAG_CANCELED != 0 {
            if let Some(id) = acting.filter(|id| !self.revoked.contains(id)) {
                self.revoked.push(id);
            }
        }
    }

    /// The latest sample of pointer `id`.
    pub(crate) fn info(&self, id: u64) -> Option<PointerInfo> {
        self.info.iter().find(|(i, _)| *i == id).map(|(_, info)| *info)
    }

    /// Whether the system revoked pointer `id` as it lifted; true once.
    pub(crate) fn take_revoked(&mut self, id: u64) -> bool {
        let before = self.revoked.len();
        self.revoked.retain(|r| *r != id);
        self.revoked.len() != before
    }
}

/// What a pointer is for the palm arbiter, and its contact's major axis in
/// millimetres, on a screen of `dpi` dots per inch.
pub(crate) fn contact(info: PointerInfo, dpi: Option<f32>) -> (ContactKind, Option<f32>) {
    let major_mm = dpi.filter(|d| *d > 0.0 && info.touch_major > 0.0).map(|d| info.touch_major * 25.4 / d);
    match info.tool {
        TOOL_TYPE_STYLUS => (ContactKind::Stylus, None),
        TOOL_TYPE_ERASER => (ContactKind::Eraser, None),
        TOOL_TYPE_FINGER => (ContactKind::Finger, major_mm),
        // The system recognised the resting hand: wider than any fingertip.
        TOOL_TYPE_PALM => (ContactKind::Finger, Some(f32::INFINITY)),
        // A mouse or an unknown tool.
        _ => (ContactKind::Unknown, major_mm),
    }
}

#[cfg(target_os = "android")]
pub(crate) use hook::{bridged, pointer, take_revoked};

#[cfg(target_os = "android")]
mod hook {
    use super::{PointerInfo, PointerSample, Pointers};
    use std::ffi::{c_char, c_int, c_void};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, OnceLock};

    const AINPUT_EVENT_TYPE_MOTION: i32 = 2;
    const AMOTION_EVENT_AXIS_TOUCH_MAJOR: i32 = 4;
    /// `dlopen` flags, the same on every Android ABI.
    const RTLD_LAZY: c_int = 1;
    const RTLD_NOLOAD: c_int = 4;

    #[link(name = "android")]
    extern "C" {
        fn AInputEvent_getType(event: *const c_void) -> i32;
        fn AMotionEvent_getAction(event: *const c_void) -> i32;
        fn AMotionEvent_getFlags(event: *const c_void) -> i32;
        fn AMotionEvent_getPointerCount(event: *const c_void) -> usize;
        fn AMotionEvent_getPointerId(event: *const c_void, pointer_index: usize) -> i32;
        fn AMotionEvent_getToolType(event: *const c_void, pointer_index: usize) -> i32;
        fn AMotionEvent_getAxisValue(event: *const c_void, axis: i32, pointer_index: usize) -> f32;
    }

    extern "C" {
        fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }

    type GetEvent = unsafe extern "C" fn(queue: *mut c_void, out_event: *mut *mut c_void) -> i32;

    static POINTERS: Mutex<Pointers> = Mutex::new(Pointers { info: Vec::new(), revoked: Vec::new() });
    /// The hook has seen the input queue.
    static SEEN: AtomicBool = AtomicBool::new(false);

    /// libandroid's `AInputQueue_getEvent`.
    fn real() -> Option<GetEvent> {
        static REAL: OnceLock<Option<GetEvent>> = OnceLock::new();
        *REAL.get_or_init(|| {
            // SAFETY: C strings; the symbol has `GetEvent`'s signature (NDK `input.h`).
            unsafe {
                let name = c"libandroid.so".as_ptr();
                let mut lib = dlopen(name, RTLD_LAZY | RTLD_NOLOAD);
                if lib.is_null() {
                    lib = dlopen(name, RTLD_LAZY);
                }
                if lib.is_null() {
                    return None;
                }
                let f = dlsym(lib, c"AInputQueue_getEvent".as_ptr());
                (!f.is_null()).then(|| std::mem::transmute::<*mut c_void, GetEvent>(f))
            }
        })
    }

    /// The input queue's next event, as libandroid's `AInputQueue_getEvent`
    /// returns it, after noting its pointers. winit (through `ndk`) calls this
    /// in its place.
    ///
    /// # Safety
    /// As `AInputQueue_getEvent`: `queue` is the activity's input queue and
    /// `out_event` is writable.
    #[no_mangle]
    pub unsafe extern "C" fn AInputQueue_getEvent(queue: *mut c_void, out_event: *mut *mut c_void) -> i32 {
        let Some(real) = real() else { return -1 };
        let status = real(queue, out_event);
        if status >= 0 && !out_event.is_null() && !(*out_event).is_null() {
            SEEN.store(true, Ordering::Relaxed);
            observe(*out_event);
        }
        status
    }

    /// Notes the pointers of `event` when it is a motion event.
    ///
    /// # Safety
    /// `event` is an event just read from the input queue.
    unsafe fn observe(event: *const c_void) {
        if AInputEvent_getType(event) != AINPUT_EVENT_TYPE_MOTION {
            return;
        }
        let samples: Vec<PointerSample> = (0..AMotionEvent_getPointerCount(event))
            .map(|i| PointerSample {
                id: AMotionEvent_getPointerId(event, i) as u64,
                tool: AMotionEvent_getToolType(event, i),
                touch_major: AMotionEvent_getAxisValue(event, AMOTION_EVENT_AXIS_TOUCH_MAJOR, i),
            })
            .collect();
        let (action, flags) = (AMotionEvent_getAction(event), AMotionEvent_getFlags(event));
        if let Ok(mut pointers) = POINTERS.lock() {
            pointers.observe(action, flags, &samples);
        }
    }

    /// Whether the hook reads the input queue: tool types are known.
    pub(crate) fn bridged() -> bool {
        SEEN.load(Ordering::Relaxed)
    }

    /// The latest sample of pointer `id`.
    pub(crate) fn pointer(id: u64) -> Option<PointerInfo> {
        POINTERS.lock().ok()?.info(id)
    }

    /// Whether the system revoked pointer `id` as it lifted; true once.
    pub(crate) fn take_revoked(id: u64) -> bool {
        POINTERS.lock().is_ok_and(|mut p| p.take_revoked(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: u64, tool: i32, touch_major: f32) -> PointerSample {
        PointerSample { id, tool, touch_major }
    }

    #[test]
    fn tool_types_name_the_contact() {
        let mut p = Pointers::default();
        p.observe(ACTION_DOWN, 0, &[sample(0, TOOL_TYPE_STYLUS, 0.0)]);
        p.observe(ACTION_POINTER_DOWN | (1 << ACTION_POINTER_INDEX_SHIFT), 0, &[sample(0, TOOL_TYPE_STYLUS, 0.0), sample(1, TOOL_TYPE_FINGER, 40.0)]);
        assert_eq!(contact(p.info(0).unwrap(), Some(400.0)), (ContactKind::Stylus, None));
        let (kind, major) = contact(p.info(1).unwrap(), Some(400.0));
        assert_eq!(kind, ContactKind::Finger);
        assert!((major.unwrap() - 2.54).abs() < 1e-4);
        assert_eq!(p.info(2), None);
    }

    #[test]
    fn eraser_palm_and_unknown_tools() {
        let info = |tool| PointerInfo { tool, touch_major: 0.0 };
        assert_eq!(contact(info(TOOL_TYPE_ERASER), Some(400.0)), (ContactKind::Eraser, None));
        assert_eq!(contact(info(TOOL_TYPE_PALM), Some(400.0)), (ContactKind::Finger, Some(f32::INFINITY)));
        assert_eq!(contact(info(0), Some(400.0)), (ContactKind::Unknown, None));
        // No size without a density, or when the screen doesn't report it.
        assert_eq!(contact(PointerInfo { tool: TOOL_TYPE_FINGER, touch_major: 40.0 }, None), (ContactKind::Finger, None));
    }

    #[test]
    fn the_latest_sample_wins() {
        let mut p = Pointers::default();
        p.observe(ACTION_DOWN, 0, &[sample(0, TOOL_TYPE_FINGER, 20.0)]);
        p.observe(2, 0, &[sample(0, TOOL_TYPE_FINGER, 90.0)]);
        assert_eq!(p.info(0).unwrap().touch_major, 90.0);
    }

    #[test]
    fn a_canceled_up_revokes_its_pointer_once() {
        let mut p = Pointers::default();
        p.observe(ACTION_DOWN, 0, &[sample(0, TOOL_TYPE_STYLUS, 0.0)]);
        p.observe(ACTION_POINTER_DOWN | (1 << ACTION_POINTER_INDEX_SHIFT), 0, &[sample(0, TOOL_TYPE_STYLUS, 0.0), sample(1, TOOL_TYPE_FINGER, 120.0)]);
        p.observe(ACTION_POINTER_UP | (1 << ACTION_POINTER_INDEX_SHIFT), FLAG_CANCELED, &[sample(0, TOOL_TYPE_STYLUS, 0.0), sample(1, TOOL_TYPE_FINGER, 120.0)]);
        assert!(!p.take_revoked(0));
        assert!(p.take_revoked(1));
        assert!(!p.take_revoked(1));
        // A plain up revokes nothing.
        p.observe(ACTION_UP, 0, &[sample(0, TOOL_TYPE_STYLUS, 0.0)]);
        assert!(!p.take_revoked(0));
    }

    #[test]
    fn a_new_touch_drops_an_unread_revocation() {
        let mut p = Pointers::default();
        p.observe(ACTION_DOWN, 0, &[sample(0, TOOL_TYPE_FINGER, 100.0)]);
        p.observe(ACTION_UP, FLAG_CANCELED, &[sample(0, TOOL_TYPE_FINGER, 100.0)]);
        p.observe(ACTION_DOWN, 0, &[sample(0, TOOL_TYPE_STYLUS, 0.0)]);
        assert!(!p.take_revoked(0));
        assert_eq!(p.info(0).unwrap().tool, TOOL_TYPE_STYLUS);
    }
}
