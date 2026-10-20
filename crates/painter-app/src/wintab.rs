//! Minimal WinTab (wintab32.dll) client: pressure, tilt and the active cursor
//! (pen tip or eraser end), polled once per frame.
//!
//! The DLL ships with tablet drivers (Wacom, Huion, XP-Pen, ...). It is loaded
//! at runtime; without it `WinTab::open` returns `None` and Windows Ink is used.
//! Struct layouts and constants follow the public `wintab.h` / `pktdef.h`.

#![cfg(windows)]

use std::ffi::c_void;

use libloading::Library;

type Hctx = *mut c_void;

const WTI_DEFSYSCTX: u32 = 4;
const WTI_DEVICES: u32 = 100;
const DVC_NPRESSURE: u32 = 15;

const PK_CURSOR: u32 = 0x0020;
const PK_BUTTONS: u32 = 0x0040;
const PK_X: u32 = 0x0080;
const PK_Y: u32 = 0x0100;
const PK_NORMAL_PRESSURE: u32 = 0x0400;
const PK_ORIENTATION: u32 = 0x1000;
const PACKET_DATA: u32 = PK_CURSOR | PK_BUTTONS | PK_X | PK_Y | PK_NORMAL_PRESSURE | PK_ORIENTATION;

#[repr(C)]
#[derive(Clone, Copy)]
struct LogContextA {
    name: [u8; 40],
    options: u32,
    status: u32,
    locks: u32,
    msg_base: u32,
    device: u32,
    pkt_rate: u32,
    pkt_data: u32,
    pkt_mode: u32,
    move_mask: u32,
    btn_dn_mask: u32,
    btn_up_mask: u32,
    in_org: [i32; 3],
    in_ext: [i32; 3],
    out_org: [i32; 3],
    out_ext: [i32; 3],
    sens: [u32; 3],
    sys_mode: i32,
    sys_org: [i32; 2],
    sys_ext: [i32; 2],
    sys_sens: [u32; 2],
}

/// Field order follows the `PK_*` bit order of `PACKET_DATA`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Packet {
    cursor: u32,
    buttons: u32,
    x: i32,
    y: i32,
    pressure: u32,
    azimuth: i32,
    altitude: i32,
    twist: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Axis {
    min: i32,
    max: i32,
    units: u32,
    resolution: u32,
}

type WtInfoA = unsafe extern "system" fn(u32, u32, *mut c_void) -> u32;
type WtOpenA = unsafe extern "system" fn(isize, *mut LogContextA, i32) -> Hctx;
type WtClose = unsafe extern "system" fn(Hctx) -> i32;
type WtPacketsGet = unsafe extern "system" fn(Hctx, i32, *mut c_void) -> i32;

/// The latest pen state reported by the driver.
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
}

pub struct WinTab {
    // Keeps the DLL loaded for the function pointers below.
    _lib: Library,
    close: WtClose,
    packets_get: WtPacketsGet,
    ctx: Hctx,
    pressure_max: f32,
}

// The context handle is only used from the UI thread.
unsafe impl Send for WinTab {}

impl WinTab {
    /// Opens a polling context on `hwnd`, or `None` when no WinTab driver is installed.
    pub fn open(hwnd: isize) -> Option<Self> {
        // SAFETY: loading a system tablet driver DLL by its documented name; the
        // functions are resolved with their documented signatures.
        unsafe {
            let lib = Library::new("wintab32.dll").ok()?;
            let info: WtInfoA = *lib.get(b"WTInfoA\0").ok()?;
            let open: WtOpenA = *lib.get(b"WTOpenA\0").ok()?;
            let close: WtClose = *lib.get(b"WTClose\0").ok()?;
            let packets_get: WtPacketsGet = *lib.get(b"WTPacketsGet\0").ok()?;

            // WTInfo(0, 0, NULL) returns non-zero when a tablet service is available.
            if info(0, 0, std::ptr::null_mut()) == 0 {
                return None;
            }
            let mut ctx: LogContextA = std::mem::zeroed();
            if info(WTI_DEFSYSCTX, 0, (&mut ctx as *mut LogContextA).cast()) == 0 {
                return None;
            }
            ctx.name[..14].copy_from_slice(b"Painter WinTab");
            ctx.pkt_data = PACKET_DATA;
            ctx.pkt_mode = 0;
            ctx.move_mask = PACKET_DATA;

            let mut axis = Axis::default();
            info(WTI_DEVICES, DVC_NPRESSURE, (&mut axis as *mut Axis).cast());
            let pressure_max = if axis.max > axis.min { (axis.max - axis.min) as f32 } else { 1023.0 };

            let handle = open(hwnd, &mut ctx, 1);
            if handle.is_null() {
                return None;
            }
            Some(Self { _lib: lib, close, packets_get, ctx: handle, pressure_max })
        }
    }

    /// Drains the packet queue: every sample since the last poll, oldest first.
    pub fn poll(&mut self) -> Vec<PenSample> {
        let mut buf = [Packet::default(); 64];
        let mut packets = Vec::new();
        loop {
            // SAFETY: `buf` holds 64 packets laid out as requested in `PACKET_DATA`.
            let n = unsafe { (self.packets_get)(self.ctx, buf.len() as i32, buf.as_mut_ptr().cast()) };
            if n <= 0 {
                break;
            }
            packets.extend_from_slice(&buf[..n as usize]);
            if (n as usize) < buf.len() {
                break;
            }
        }
        packets.into_iter().map(|p| self.sample(p)).collect()
    }

    fn sample(&self, p: Packet) -> PenSample {
        PenSample {
            pressure: (p.pressure as f32 / self.pressure_max).clamp(0.0, 1.0),
            // Altitude is in tenths of a degree: 900 = upright.
            tilt: (1.0 - (p.altitude.unsigned_abs() as f32 / 900.0)).clamp(0.0, 1.0),
            azimuth: p.azimuth as f32 / 10.0,
            // Common driver convention: cursor 1 = pen tip, 2 = eraser (per tablet).
            eraser: p.cursor % 3 == 2,
            touching: p.pressure > 0,
        }
    }
}

impl Drop for WinTab {
    fn drop(&mut self) {
        // SAFETY: the context was opened by this instance and is closed once.
        unsafe {
            (self.close)(self.ctx);
        }
    }
}
