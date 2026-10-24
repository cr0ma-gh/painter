//! Pen tablets on Linux through XInput2: pressure, tilt and the eraser end,
//! from every packet the tablet sends, like WinTab on Windows.
//!
//! A second connection to the X server listens to the app's window for
//! XInput2 motion events from the tablet devices themselves
//! (slave devices, which carry the pen's valuators). Each device's valuators
//! are found by their labels ("Abs Pressure", "Abs Tilt X", "Abs Tilt Y")
//! and scaled by their ranges. Under Wayland the app runs through XWayland,
//! which reports tablets the same way. libX11 and libXi are loaded at run time.

use std::collections::HashMap;
use std::ffi::{c_int, CStr};

use x11_dl::xinput2::{self, XInput2};
use x11_dl::xlib::{self, Xlib};

use crate::tablet::{PenSample, PenSource};

/// A tablet device's valuators: (index, min, max) of each.
#[derive(Clone, Copy, Default, Debug)]
struct Axes {
    pressure: Option<(i32, f64, f64)>,
    tilt_x: Option<(i32, f64, f64)>,
    tilt_y: Option<(i32, f64, f64)>,
    eraser: bool,
}

pub struct XInputPen {
    xlib: Xlib,
    xi: XInput2,
    display: *mut xlib::Display,
    opcode: c_int,
    devices: HashMap<c_int, Axes>,
}

impl XInputPen {
    /// Listens for tablet input on X window `window`; `None` without X11, XInput 2.2 or a tablet.
    pub fn open(window: u64) -> Option<Self> {
        let xlib = Xlib::open().ok()?;
        let xi = XInput2::open().ok()?;
        // SAFETY: plain Xlib calls on a connection this struct owns; strings are NUL-terminated.
        unsafe {
            let display = (xlib.XOpenDisplay)(std::ptr::null());
            if display.is_null() {
                return None;
            }
            let (mut opcode, mut event, mut error) = (0, 0, 0);
            let name = c"XInputExtension";
            if (xlib.XQueryExtension)(display, name.as_ptr(), &mut opcode, &mut event, &mut error) == 0 {
                (xlib.XCloseDisplay)(display);
                return None;
            }
            let (mut major, mut minor) = (2, 2);
            if (xi.XIQueryVersion)(display, &mut major, &mut minor) != 0 {
                (xlib.XCloseDisplay)(display);
                return None;
            }
            let mut pen = XInputPen { xlib, xi, display, opcode, devices: HashMap::new() };
            pen.find_devices();
            if pen.devices.is_empty() {
                // No tablet: mouse input stays as it is.
                return None;
            }
            let mut mask = [0u8; ((xinput2::XI_LASTEVENT + 8) / 8) as usize];
            // Motion only: X allows one client per window to take button presses (winit has them),
            // and every motion event carries the button state anyway.
            xinput2::XISetMask(&mut mask, xinput2::XI_Motion);
            let mut events = xinput2::XIEventMask { deviceid: xinput2::XIAllDevices, mask_len: mask.len() as c_int, mask: mask.as_mut_ptr() };
            (pen.xi.XISelectEvents)(display, window as _, &mut events, 1);
            (pen.xlib.XFlush)(display);
            Some(pen)
        }
    }

    /// The devices with a pressure valuator: the pens (and erasers) of tablets.
    fn find_devices(&mut self) {
        // SAFETY: the device list is freed below; class pointers are valid while it lives.
        unsafe {
            let mut count = 0;
            let info = (self.xi.XIQueryDevice)(self.display, xinput2::XIAllDevices, &mut count);
            if info.is_null() {
                return;
            }
            for d in std::slice::from_raw_parts(info, count.max(0) as usize) {
                let name = if d.name.is_null() { String::new() } else { CStr::from_ptr(d.name).to_string_lossy().to_lowercase() };
                let mut axes = Axes { eraser: name.contains("eraser"), ..Axes::default() };
                for &class in std::slice::from_raw_parts(d.classes, d.num_classes.max(0) as usize) {
                    if class.is_null() || (*class)._type != xinput2::XIValuatorClass {
                        continue;
                    }
                    let v = &*(class as *const xinput2::XIValuatorClassInfo);
                    let label = self.atom_name(v.label);
                    let axis = Some((v.number, v.min, v.max));
                    match label.as_str() {
                        "Abs Pressure" => axes.pressure = axis,
                        "Abs Tilt X" => axes.tilt_x = axis,
                        "Abs Tilt Y" => axes.tilt_y = axis,
                        _ => {}
                    }
                }
                // Master pointers carry their slave's valuators too; only the devices themselves count.
                if axes.pressure.is_some() && d._use == xinput2::XISlavePointer {
                    self.devices.insert(d.deviceid, axes);
                }
            }
            (self.xi.XIFreeDeviceInfo)(info);
        }
    }

    fn atom_name(&self, atom: xlib::Atom) -> String {
        if atom == 0 {
            return String::new();
        }
        // SAFETY: the returned string is freed with XFree.
        unsafe {
            let p = (self.xlib.XGetAtomName)(self.display, atom);
            if p.is_null() {
                return String::new();
            }
            let s = CStr::from_ptr(p).to_string_lossy().into_owned();
            (self.xlib.XFree)(p.cast());
            s
        }
    }

    fn sample(&self, e: &xinput2::XIDeviceEvent) -> Option<PenSample> {
        let axes = *self.devices.get(&e.sourceid)?;
        // The valuators present in this event, in order.
        let mask = if e.valuators.mask.is_null() {
            &[][..]
        } else {
            // SAFETY: mask_len bytes of mask.
            unsafe { std::slice::from_raw_parts(e.valuators.mask, e.valuators.mask_len.max(0) as usize) }
        };
        let mut values = HashMap::new();
        let mut k = 0;
        for i in 0..(mask.len() * 8) as i32 {
            if xinput2::XIMaskIsSet(mask, i) {
                // SAFETY: one value for each set bit, in order.
                values.insert(i, unsafe { *e.valuators.values.add(k) });
                k += 1;
            }
        }
        let norm = |axis: Option<(i32, f64, f64)>| -> Option<f64> {
            let (i, lo, hi) = axis?;
            let v = *values.get(&i)?;
            Some(if hi > lo { (v - lo) / (hi - lo) } else { 0.0 })
        };
        let pressure = norm(axes.pressure).map(|p| p.clamp(0.0, 1.0) as f32);
        // Button 1 is the tip touching the tablet.
        let down = !e.buttons.mask.is_null()
            && e.buttons.mask_len > 0
            // SAFETY: mask_len bytes of mask.
            && xinput2::XIMaskIsSet(unsafe { std::slice::from_raw_parts(e.buttons.mask, e.buttons.mask_len as usize) }, 1);
        let pressure = pressure.unwrap_or(if down { 1.0 } else { 0.0 });
        // Tilt valuators span about ±60–64° on most tablets: -1..1 here.
        let tx = norm(axes.tilt_x).map_or(0.0, |t| t * 2.0 - 1.0) as f32;
        let ty = norm(axes.tilt_y).map_or(0.0, |t| t * 2.0 - 1.0) as f32;
        let lean = (tx * tx + ty * ty).sqrt().min(1.0);
        Some(PenSample {
            pressure,
            // ±1 ≈ 60° from upright, full tilt (see `painter_core::smoothing::FULL_TILT_DEGREES`).
            tilt: lean,
            azimuth: if lean > 0.0 { tx.atan2(-ty).to_degrees().rem_euclid(360.0) } else { 0.0 },
            eraser: axes.eraser,
            touching: down && pressure > 0.0,
            screen: [e.event_x as f32, e.event_y as f32],
            time_ms: e.time as u32,
        })
    }
}

impl PenSource for XInputPen {
    fn poll(&mut self) -> Vec<PenSample> {
        let mut out = Vec::new();
        // SAFETY: events are read from this struct's own connection; cookie data is freed after use.
        unsafe {
            while (self.xlib.XPending)(self.display) > 0 {
                let mut event: xlib::XEvent = std::mem::zeroed();
                (self.xlib.XNextEvent)(self.display, &mut event);
                let cookie = &mut event.generic_event_cookie;
                if cookie.type_ != xlib::GenericEvent || cookie.extension != self.opcode {
                    continue;
                }
                if (self.xlib.XGetEventData)(self.display, cookie) == 0 {
                    continue;
                }
                if cookie.evtype == xinput2::XI_Motion && !cookie.data.is_null() {
                    let e = &*(cookie.data as *const xinput2::XIDeviceEvent);
                    // Slave events only: the master repeats them.
                    if e.deviceid == e.sourceid {
                        if let Some(s) = self.sample(e) {
                            out.push(s);
                        }
                    }
                }
                (self.xlib.XFreeEventData)(self.display, cookie);
            }
        }
        out
    }

    /// Positions are already in the window.
    fn origin(&self) -> Option<[f32; 2]> {
        Some([0.0, 0.0])
    }

    fn name(&self) -> &'static str {
        "XInput2"
    }
}

impl Drop for XInputPen {
    fn drop(&mut self) {
        // SAFETY: the connection was opened by this struct.
        unsafe { (self.xlib.XCloseDisplay)(self.display) };
    }
}
