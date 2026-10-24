//! Alt + Space zooms out on the canvas (as in Clip Studio), so Windows must
//! not open the window's system menu for it: a window subclass drops that
//! one menu request and leaves every other message alone.

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};

const WM_SYSCOMMAND: u32 = 0x0112;
const SC_KEYMENU: usize = 0xF100;

/// Installs the filter on the main window; safe to call more than once.
pub(crate) fn install(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    // The same id again just updates the existing subclass.
    unsafe {
        let _ = SetWindowSubclass(HWND(hwnd as *mut _), Some(filter), 0x5041_494E, 0);
    }
}

unsafe extern "system" fn filter(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
    if is_alt_space_menu(msg, wparam.0, lparam.0) {
        return LRESULT(0);
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// The system menu opened from the keyboard with Space (Alt + Space).
fn is_alt_space_menu(msg: u32, wparam: usize, lparam: isize) -> bool {
    msg == WM_SYSCOMMAND && wparam & 0xFFF0 == SC_KEYMENU && lparam == b' ' as isize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_alt_space_is_dropped() {
        assert!(is_alt_space_menu(WM_SYSCOMMAND, SC_KEYMENU, 32));
        assert!(!is_alt_space_menu(WM_SYSCOMMAND, SC_KEYMENU, 0), "Alt alone (the menu bar) still works");
        assert!(!is_alt_space_menu(WM_SYSCOMMAND, 0xF020, 32), "minimise is untouched");
    }
}
