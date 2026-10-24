//! Printing on Windows: the system's Print dialog (printer, copies, its own
//! preferences), then the picture drawn on the printer's page through GDI,
//! placed with the Print dialog's layout on the paper the printer reports.

use painter_core::print::{place, PrintLayout};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{GlobalFree, HGLOBAL, HWND};
use windows::Win32::Graphics::Gdi::{
    DeleteDC, GetDeviceCaps, SetBrushOrgEx, SetStretchBltMode, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DEVMODEW,
    DIB_RGB_COLORS, DM_ORIENTATION, DM_PAPERLENGTH, DM_PAPERSIZE, DM_PAPERWIDTH, DM_SPECVERSION, HALFTONE, HDC, LOGPIXELSX,
    LOGPIXELSY, PHYSICALHEIGHT, PHYSICALOFFSETX, PHYSICALOFFSETY, PHYSICALWIDTH, SRCCOPY,
};
use windows::Win32::Storage::Xps::{EndDoc, EndPage, StartDocW, StartPage, DOCINFOW};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT};
use windows::Win32::UI::Controls::Dialogs::{
    PrintDlgExW, PD_NOCURRENTPAGE, PD_NOPAGENUMS, PD_NOSELECTION, PD_RESULT_PRINT, PD_RETURNDC, PD_USEDEVMODECOPIESANDCOLLATE,
    PRINTDLGEXW, START_PAGE_GENERAL,
};

/// Windows' paper number for a portrait size in millimetres (0: none, use the size itself).
fn paper_code(mm: [f64; 2]) -> i16 {
    let near = |w: f64, h: f64| (mm[0] - w).abs() < 1.0 && (mm[1] - h).abs() < 1.0;
    match () {
        _ if near(215.9, 279.4) => 1,  // Letter
        _ if near(279.4, 431.8) => 3,  // Tabloid
        _ if near(215.9, 355.6) => 5,  // Legal
        _ if near(297.0, 420.0) => 8,  // A3
        _ if near(210.0, 297.0) => 9,  // A4
        _ if near(148.0, 210.0) => 11, // A5
        _ if near(257.0, 364.0) => 12, // B4 (JIS)
        _ if near(182.0, 257.0) => 13, // B5 (JIS)
        _ if near(250.0, 353.0) => 42, // B4 (ISO)
        _ if near(105.0, 148.0) => 70, // A6
        _ => 0,
    }
}

/// Shows the Print dialog and prints `rgb` (`w` × `h`, at `dpi`) on the chosen
/// printer. `Ok(false)`: the dialog was cancelled.
pub(crate) fn print(hwnd: isize, rgb: &[u8], w: u32, h: u32, layout: &PrintLayout, dpi: u32, title: &str) -> Result<bool, String> {
    // SAFETY: plain Win32 calls on memory this function owns; every handle the
    // dialog hands back is freed before returning.
    unsafe {
        let devmode = initial_devmode(layout)?;
        let mut pd: PRINTDLGEXW = std::mem::zeroed();
        pd.lStructSize = std::mem::size_of::<PRINTDLGEXW>() as u32;
        pd.hwndOwner = HWND(hwnd as *mut _);
        pd.hDevMode = devmode;
        pd.Flags = PD_RETURNDC | PD_NOPAGENUMS | PD_NOSELECTION | PD_NOCURRENTPAGE | PD_USEDEVMODECOPIESANDCOLLATE;
        pd.nCopies = 1;
        pd.nStartPage = START_PAGE_GENERAL;
        let shown = PrintDlgExW(&mut pd);
        let result = match shown {
            Err(e) => Err(format!("The Print dialog didn't open: {e}")),
            Ok(()) if pd.dwResultAction != PD_RESULT_PRINT || pd.hDC.is_invalid() => Ok(false),
            Ok(()) => draw(pd.hDC, rgb, w, h, layout, dpi, title, None).map(|()| true),
        };
        if !pd.hDC.is_invalid() {
            let _ = DeleteDC(pd.hDC);
        }
        for handle in [pd.hDevMode, pd.hDevNames] {
            if !handle.is_invalid() {
                let _ = GlobalFree(handle);
            }
        }
        result
    }
}

/// The paper and its turn from the Print dialog's layout, as the Windows
/// dialog's starting choice (the printer's own preferences can still change them).
unsafe fn initial_devmode(layout: &PrintLayout) -> Result<HGLOBAL, String> {
    let handle = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, std::mem::size_of::<DEVMODEW>()).map_err(|e| e.to_string())?;
    let dm = GlobalLock(handle) as *mut DEVMODEW;
    if dm.is_null() {
        let _ = GlobalFree(handle);
        return Err("Out of memory".into());
    }
    let landscape = layout.page_mm[0] > layout.page_mm[1];
    let portrait = if landscape { [layout.page_mm[1], layout.page_mm[0]] } else { layout.page_mm };
    let code = paper_code(portrait);
    (*dm).dmSpecVersion = DM_SPECVERSION.0 as u16;
    (*dm).dmSize = std::mem::size_of::<DEVMODEW>() as u16;
    (*dm).dmFields = DM_ORIENTATION | if code != 0 { DM_PAPERSIZE } else { DM_PAPERLENGTH | DM_PAPERWIDTH };
    let fields = &mut (*dm).Anonymous1.Anonymous1;
    fields.dmOrientation = if landscape { 2 } else { 1 };
    fields.dmPaperSize = code;
    // Tenths of a millimetre.
    fields.dmPaperWidth = (portrait[0] * 10.0).round() as i16;
    fields.dmPaperLength = (portrait[1] * 10.0).round() as i16;
    let _ = GlobalUnlock(handle);
    Ok(handle)
}

/// One page with the picture, placed on the printer's real paper.
/// `output`: a file instead of the printer (printing to a file, as the tests do).
#[allow(clippy::too_many_arguments)]
unsafe fn draw(
    hdc: HDC,
    rgb: &[u8],
    w: u32,
    h: u32,
    layout: &PrintLayout,
    dpi: u32,
    title: &str,
    output: Option<&str>,
) -> Result<(), String> {
    let cap = |i| GetDeviceCaps(hdc, i) as f64;
    let (lx, ly) = (cap(LOGPIXELSX).max(1.0), cap(LOGPIXELSY).max(1.0));
    let page_mm = [cap(PHYSICALWIDTH) / lx * 25.4, cap(PHYSICALHEIGHT) / ly * 25.4];
    let at = place(&PrintLayout { page_mm, ..*layout }, (w, h), dpi);
    // Device units count from the printable area's corner, not the paper's.
    let dev = |mm: f64, per_inch: f64| (mm / 25.4 * per_inch).round() as i32;
    let (x, y) = (dev(at.x, lx) - cap(PHYSICALOFFSETX) as i32, dev(at.y, ly) - cap(PHYSICALOFFSETY) as i32);
    let (dw, dh) = (dev(at.w, lx).max(1), dev(at.h, ly).max(1));

    // 24-bit BGR rows, padded to whole 32-bit words, top row first.
    let stride = (w as usize * 3 + 3) & !3;
    let mut bits = vec![0u8; stride * h as usize];
    for (row, src) in bits.chunks_exact_mut(stride).zip(rgb.chunks_exact(w as usize * 3)) {
        for (d, s) in row.as_chunks_mut::<3>().0.iter_mut().zip(src.as_chunks::<3>().0) {
            *d = [s[2], s[1], s[0]];
        }
    }
    let mut info: BITMAPINFO = std::mem::zeroed();
    info.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: w as i32,
        biHeight: -(h as i32),
        biPlanes: 1,
        biBitCount: 24,
        biCompression: BI_RGB.0,
        ..Default::default()
    };

    let name: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let file: Option<Vec<u16>> = output.map(|o| o.encode_utf16().chain(Some(0)).collect());
    let doc = DOCINFOW {
        cbSize: std::mem::size_of::<DOCINFOW>() as i32,
        lpszDocName: PCWSTR(name.as_ptr()),
        lpszOutput: file.as_ref().map_or(PCWSTR::null(), |f| PCWSTR(f.as_ptr())),
        ..Default::default()
    };
    if StartDocW(hdc, &doc) <= 0 {
        return Err("The printer didn't take the job".into());
    }
    let ok = StartPage(hdc) > 0 && {
        SetStretchBltMode(hdc, HALFTONE);
        let _ = SetBrushOrgEx(hdc, 0, 0, None);
        let lines = StretchDIBits(
            hdc,
            x,
            y,
            dw,
            dh,
            0,
            0,
            w as i32,
            h as i32,
            Some(bits.as_ptr() as *const _),
            &info,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
        lines > 0 && EndPage(hdc) > 0
    };
    EndDoc(hdc);
    if ok {
        Ok(())
    } else {
        Err("The printer couldn't draw the picture".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Gdi::CreateDCW;

    /// Prints a checkerboard with "Microsoft Print to PDF" into `PAINTER_PRINT_TEST`
    /// (a .pdf path). Ignored: it needs that printer, which build machines may lack.
    #[test]
    #[ignore]
    fn prints_to_microsoft_print_to_pdf() {
        let out = std::env::var("PAINTER_PRINT_TEST").expect("PAINTER_PRINT_TEST=out.pdf");
        let (w, h) = (400u32, 300u32);
        let rgb: Vec<u8> =
            (0..w * h).flat_map(|i| if (i % w / 50 + i / w / 50) % 2 == 0 { [220, 0, 0] } else { [0, 0, 200] }).collect();
        let printer: Vec<u16> = "Microsoft Print to PDF".encode_utf16().chain(Some(0)).collect();
        // SAFETY: a device context made and deleted here.
        unsafe {
            let hdc = CreateDCW(PCWSTR::null(), PCWSTR(printer.as_ptr()), PCWSTR::null(), None);
            assert!(!hdc.is_invalid(), "no Microsoft Print to PDF here");
            let layout = PrintLayout::default();
            let printed = draw(hdc, &rgb, w, h, &layout, 72, "Painter test", Some(&out));
            let _ = DeleteDC(hdc);
            printed.unwrap();
        }
        assert!(std::fs::read(&out).unwrap().starts_with(b"%PDF"));
    }
}
