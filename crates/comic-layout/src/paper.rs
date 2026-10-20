//! Standard paper sizes and length units for print documents.
//!
//! Physical sizes are kept in millimetres; pixels follow from the resolution:
//! `px = mm × dpi / 25.4`. Dialogs keep the millimetre value as the source of
//! truth, so switching the displayed unit never changes the page.

pub use crate::page_preset::mm_to_px;
use crate::page_preset::{inch_to_mm, MM_PER_INCH};

/// A named paper size, portrait (width ≤ height), in millimetres.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaperSize {
    pub name: &'static str,
    pub mm: [f64; 2],
}

/// ISO 216 A and B series, JIS B (Japanese manga / doujinshi B4–B5) and the
/// common North American sizes.
pub const PAPER_SIZES: [PaperSize; 12] = [
    PaperSize { name: "A3", mm: [297.0, 420.0] },
    PaperSize { name: "A4", mm: [210.0, 297.0] },
    PaperSize { name: "A5", mm: [148.0, 210.0] },
    PaperSize { name: "A6", mm: [105.0, 148.0] },
    PaperSize { name: "B4", mm: [250.0, 353.0] },
    PaperSize { name: "B5", mm: [176.0, 250.0] },
    PaperSize { name: "B6", mm: [125.0, 176.0] },
    PaperSize { name: "B4 (JIS)", mm: [257.0, 364.0] },
    PaperSize { name: "B5 (JIS)", mm: [182.0, 257.0] },
    PaperSize { name: "US Letter", mm: [215.9, 279.4] },
    PaperSize { name: "US Legal", mm: [215.9, 355.6] },
    PaperSize { name: "Tabloid", mm: [279.4, 431.8] },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Portrait,
    Landscape,
}

impl Orientation {
    /// The orientation of a `[width, height]` size; squares count as portrait.
    pub fn of(size: [f64; 2]) -> Orientation {
        if size[0] > size[1] {
            Orientation::Landscape
        } else {
            Orientation::Portrait
        }
    }

    /// `size` turned to this orientation.
    pub fn apply(self, size: [f64; 2]) -> [f64; 2] {
        let (short, long) = (size[0].min(size[1]), size[0].max(size[1]));
        match self {
            Orientation::Portrait => [short, long],
            Orientation::Landscape => [long, short],
        }
    }
}

impl PaperSize {
    pub fn get(name: &str) -> Option<PaperSize> {
        PAPER_SIZES.iter().copied().find(|p| p.name == name)
    }

    pub fn oriented(&self, orientation: Orientation) -> [f64; 2] {
        orientation.apply(self.mm)
    }

    /// The paper size `size_mm` is, in either orientation (to 0.05 mm, so
    /// sizes that went through pixels still match).
    pub fn matching(size_mm: [f64; 2]) -> Option<PaperSize> {
        let portrait = Orientation::Portrait.apply(size_mm);
        PAPER_SIZES.iter().copied().find(|p| (p.mm[0] - portrait[0]).abs() < 0.05 && (p.mm[1] - portrait[1]).abs() < 0.05)
    }
}

/// Resolutions offered for print and screen work.
pub const DPI_PRESETS: [u32; 6] = [72, 96, 150, 300, 350, 600];

/// What a length is shown and typed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Unit {
    #[default]
    Px,
    Mm,
    In,
}

impl Unit {
    pub const ALL: [Unit; 3] = [Unit::Px, Unit::Mm, Unit::In];

    pub fn label(self) -> &'static str {
        match self {
            Unit::Px => "px",
            Unit::Mm => "mm",
            Unit::In => "in",
        }
    }

    /// Decimals worth showing.
    pub fn decimals(self) -> usize {
        match self {
            Unit::Px => 0,
            Unit::Mm => 1,
            Unit::In => 2,
        }
    }

    /// `value` in this unit as millimetres.
    pub fn to_mm(self, value: f64, dpi: u32) -> f64 {
        match self {
            Unit::Px => px_to_mm(value, dpi),
            Unit::Mm => value,
            Unit::In => inch_to_mm(value),
        }
    }

    /// Millimetres as a value in this unit.
    pub fn from_mm(self, mm: f64, dpi: u32) -> f64 {
        match self {
            Unit::Px => mm_to_px(mm, dpi),
            Unit::Mm => mm,
            Unit::In => mm / MM_PER_INCH,
        }
    }

    /// `value` from this unit into `to`.
    pub fn convert(self, value: f64, to: Unit, dpi: u32) -> f64 {
        to.from_mm(self.to_mm(value, dpi), dpi)
    }
}

pub fn px_to_mm(px: f64, dpi: u32) -> f64 {
    px * MM_PER_INCH / dpi.max(1) as f64
}

/// Whole pixels for a physical length (what a canvas gets).
pub fn mm_to_whole_px(mm: f64, dpi: u32) -> u32 {
    mm_to_px(mm, dpi).round().max(1.0) as u32
}

/// Usual print allowances: art runs 3 mm past the trim, and text and
/// important art stay 5 mm inside it. The canvas covers trim plus bleed.
pub const STANDARD_BLEED_MM: f64 = 3.0;
pub const STANDARD_SAFE_MM: f64 = 5.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_sizes_and_orientation() {
        let a4 = PaperSize::get("A4").unwrap();
        assert_eq!(a4.oriented(Orientation::Landscape), [297.0, 210.0]);
        assert_eq!(PaperSize::get("A3").unwrap().mm, [297.0, 420.0]);
        assert_eq!(PaperSize::get("A5").unwrap().mm, [148.0, 210.0]);
        assert_eq!(PaperSize::get("B6").unwrap().mm, [125.0, 176.0]);
        assert_eq!(PaperSize::matching([297.0, 210.0]).map(|p| p.name), Some("A4"));
        // Through pixels and back at 300 dpi: 2480 × 3508 px is A4.
        let back = [px_to_mm(2480.0, 300), px_to_mm(3508.0, 300)];
        assert_eq!(PaperSize::matching(back).map(|p| p.name), Some("A4"));
        assert_eq!(PaperSize::matching([200.0, 200.0]), None);
        assert_eq!(Orientation::of([297.0, 210.0]), Orientation::Landscape);
    }

    #[test]
    fn units_convert_both_ways() {
        // The classic numbers: A4 is 2480 × 3508 px at 300 dpi, 4961 × 7016 at 600.
        assert_eq!(mm_to_whole_px(210.0, 300), 2480);
        assert_eq!(mm_to_whole_px(297.0, 300), 3508);
        assert_eq!(mm_to_whole_px(210.0, 600), 4961);
        assert_eq!(mm_to_whole_px(297.0, 600), 7016);
        assert_eq!(mm_to_whole_px(210.0, 72), 595);
        assert!((Unit::In.convert(1.0, Unit::Px, 96) - 96.0).abs() < 1e-9);
        assert!((Unit::Mm.convert(25.4, Unit::In, 300) - 1.0).abs() < 1e-12);
        for unit in Unit::ALL {
            for dpi in DPI_PRESETS {
                let v = unit.from_mm(123.4, dpi);
                assert!((unit.to_mm(v, dpi) - 123.4).abs() < 1e-9, "{unit:?} at {dpi}");
            }
        }
    }
}
