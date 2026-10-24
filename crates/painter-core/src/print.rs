//! Printing: where the picture goes on the paper ([`place`]) and a one-page
//! PDF of it ([`pdf`]), the file the printers on macOS and Linux take and the
//! one "Save as PDF" writes. Windows prints the same placement through GDI.
//!
//! Lengths are millimetres from the page's top left corner.

use std::io::Write;

/// How big the picture prints.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PrintScale {
    /// As large as fits inside the margins.
    Fit,
    /// At the document's resolution (a 300 dpi, 2480 px picture is 210 mm wide).
    Actual,
    /// That percentage of the actual size.
    Percent(f64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PrintLayout {
    /// Paper width and height, as it's turned.
    pub page_mm: [f64; 2],
    pub margin_mm: f64,
    pub scale: PrintScale,
}

impl Default for PrintLayout {
    fn default() -> Self {
        Self { page_mm: [210.0, 297.0], margin_mm: 10.0, scale: PrintScale::Fit }
    }
}

/// Where the picture lands, millimetres from the page's top left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Printed resolution, pixels per inch.
    pub dpi: f64,
    /// Part of the picture falls outside the margins.
    pub cut: bool,
}

/// The picture of `px` pixels at `dpi`, centred inside the margins.
pub fn place(layout: &PrintLayout, px: (u32, u32), dpi: u32) -> Placement {
    let natural = [px.0 as f64 / dpi.max(1) as f64 * 25.4, px.1 as f64 / dpi.max(1) as f64 * 25.4];
    let room = [(layout.page_mm[0] - 2.0 * layout.margin_mm).max(1.0), (layout.page_mm[1] - 2.0 * layout.margin_mm).max(1.0)];
    let k = match layout.scale {
        PrintScale::Fit => (room[0] / natural[0]).min(room[1] / natural[1]),
        PrintScale::Actual => 1.0,
        PrintScale::Percent(p) => p.max(1.0) / 100.0,
    };
    let (w, h) = (natural[0] * k, natural[1] * k);
    Placement {
        x: layout.margin_mm + (room[0] - w) / 2.0,
        y: layout.margin_mm + (room[1] - h) / 2.0,
        w,
        h,
        dpi: px.0 as f64 / (w / 25.4),
        // A sliver under a pixel (3508 px at 300 dpi is 297.01 mm) still counts as fitting.
        cut: w > room[0] + 0.2 || h > room[1] + 0.2,
    }
}

/// Premultiplied RGBA laid on white paper: RGB.
pub fn on_paper(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len() / 4 * 3);
    for p in rgba.as_chunks::<4>().0 {
        let paper = 255 - p[3];
        out.extend([p[0].saturating_add(paper), p[1].saturating_add(paper), p[2].saturating_add(paper)]);
    }
    out
}

const PT_PER_MM: f64 = 72.0 / 25.4;

/// A one-page PDF of the RGB picture `w` × `h` placed on `page_mm` paper
/// (anything past the paper's edge is cut by the page).
pub fn pdf(rgb: &[u8], w: u32, h: u32, page_mm: [f64; 2], at: &Placement, title: &str) -> Vec<u8> {
    let (pw, ph) = (page_mm[0] * PT_PER_MM, page_mm[1] * PT_PER_MM);
    let (x, iw, ih) = (at.x * PT_PER_MM, at.w * PT_PER_MM, at.h * PT_PER_MM);
    // PDF space starts at the bottom left.
    let y = ph - at.y * PT_PER_MM - ih;
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = z.write_all(rgb);
    let pixels = z.finish().unwrap_or_default();
    let content = format!("q {iw:.3} 0 0 {ih:.3} {x:.3} {y:.3} cm /Im0 Do Q\n");
    // Title as a PDF string: printable ASCII, brackets and backslashes escaped.
    let title: String = title
        .chars()
        .filter(|c| c.is_ascii() && !c.is_ascii_control())
        .flat_map(|c| if matches!(c, '(' | ')' | '\\') { vec!['\\', c] } else { vec![c] })
        .collect();

    let mut out: Vec<u8> = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::new();
    let mut object = |out: &mut Vec<u8>, head: String, stream: Option<&[u8]>| {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{head}\n", offsets.len()).as_bytes());
        if let Some(s) = stream {
            out.extend(b"stream\n");
            out.extend(s);
            out.extend(b"\nendstream\n");
        }
        out.extend(b"endobj\n");
    };
    object(&mut out, "<< /Type /Catalog /Pages 2 0 R >>".into(), None);
    object(&mut out, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(), None);
    object(
        &mut out,
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw:.3} {ph:.3}] /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>"),
        None,
    );
    object(&mut out, format!("<< /Length {} >>", content.len()), Some(content.as_bytes()));
    object(
        &mut out,
        format!(
            "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Interpolate true /Filter /FlateDecode /Length {} >>",
            pixels.len()
        ),
        Some(&pixels),
    );
    object(&mut out, format!("<< /Title ({title}) /Producer (Painter) >>"), None);
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes());
    for o in &offsets {
        out.extend(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R /Info 6 0 R >>\nstartxref\n{xref}\n%%EOF\n", offsets.len() + 1).as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_fit_or_print_at_their_resolution() {
        let a4 = PrintLayout::default();
        // An A4 page at 300 dpi, actual size, with no margins: the whole sheet.
        let p = place(&PrintLayout { margin_mm: 0.0, scale: PrintScale::Actual, ..a4 }, (2480, 3508), 300);
        assert!((p.w - 209.97).abs() < 0.1 && (p.h - 297.0).abs() < 0.1 && p.x.abs() < 0.1, "{p:?}");
        assert!(!p.cut && (p.dpi - 300.0).abs() < 0.5);
        // With margins it doesn't fit any more.
        assert!(place(&PrintLayout { scale: PrintScale::Actual, ..a4 }, (2480, 3508), 300).cut);
        // Fit: a wide picture fills the width inside the margins, centred up and down.
        let p = place(&a4, (4000, 2000), 72);
        assert!((p.w - 190.0).abs() < 1e-6 && (p.h - 95.0).abs() < 1e-6, "{p:?}");
        assert!((p.x - 10.0).abs() < 1e-6 && (p.y - (297.0 - 95.0) / 2.0).abs() < 1e-6);
        // Half size: twice the resolution.
        let p = place(&PrintLayout { scale: PrintScale::Percent(50.0), ..a4 }, (600, 600), 300);
        assert!((p.w - 25.4).abs() < 1e-6 && (p.dpi - 600.0).abs() < 1e-6);
    }

    #[test]
    fn a_pdf_with_the_picture_on_its_page() {
        let rgb = on_paper(&[255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 128, 128, 10, 20, 30, 255]);
        assert_eq!(rgb, vec![255, 0, 0, 255, 255, 255, 127, 127, 255, 10, 20, 30]);
        let at = place(&PrintLayout::default(), (2, 2), 72);
        let file = pdf(&rgb, 2, 2, [210.0, 297.0], &at, "Café (draft)");
        let find = |needle: &[u8]| file.windows(needle.len()).position(|w| w == needle);
        assert!(file.starts_with(b"%PDF-1.4") && file.ends_with(b"%%EOF\n"));
        assert!(find(b"/MediaBox [0 0 595.276 841.890]").is_some());
        assert!(find(b"/Title (Caf \\(draft\\))").is_some());
        // Every object is where the cross-reference table says.
        let tail = String::from_utf8_lossy(&file[find(b"startxref\n").unwrap() + 10..]).to_string();
        let xref: usize = tail.lines().next().unwrap().parse().unwrap();
        let table = String::from_utf8_lossy(&file[xref..]).to_string();
        for (n, line) in table.lines().skip(3).take(6).enumerate() {
            let at: usize = line[..10].parse().unwrap();
            assert!(file[at..].starts_with(format!("{} 0 obj", n + 1).as_bytes()), "object {}", n + 1);
        }
        // The pixels come back out of the stream.
        let start = find(b"/FlateDecode").unwrap();
        let stream = file[start..].windows(7).position(|w| w == b"stream\n").unwrap() + start + 7;
        let mut back = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(&file[stream..]), &mut back).unwrap();
        assert_eq!(back, rgb);
    }
}
