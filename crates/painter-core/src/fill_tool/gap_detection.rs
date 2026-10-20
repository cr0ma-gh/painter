//! Closing small gaps in line art, in a working copy of the boundary.
//!
//! Rough inks rarely close: a stroke ends a few pixels short of the next and
//! a plain flood runs out through the opening. The boundary (every pixel the
//! fill may not cover) is closed morphologically: dilated by half the gap
//! size, which joins line ends closer than the gap, then eroded by the same
//! amount, which gives the lines back their own width everywhere else. What
//! the closing adds is the set of bridges across gaps (and the inside of sharp
//! corners); [`super::flood_fill::fill`] floods inside the closed boundary and
//! then lets the fill into those added pixels from its own side only.
//!
//! Each pass is linear in the canvas size whatever the gap.

use super::flood_fill::{dilate, erode};
use super::Bitmap;

/// Gap sizes offered in the tool options, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GapPreset {
    None,
    Small,
    Medium,
    Large,
}

impl GapPreset {
    pub const ALL: [GapPreset; 4] = [GapPreset::None, GapPreset::Small, GapPreset::Medium, GapPreset::Large];

    pub fn pixels(self) -> u32 {
        match self {
            GapPreset::None => 0,
            GapPreset::Small => 4,
            GapPreset::Medium => 10,
            GapPreset::Large => 20,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GapPreset::None => "None",
            GapPreset::Small => "Small",
            GapPreset::Medium => "Medium",
            GapPreset::Large => "Large",
        }
    }

    /// The preset for a pixel size, if it is one.
    pub fn of(pixels: u32) -> Option<GapPreset> {
        GapPreset::ALL.into_iter().find(|p| p.pixels() == pixels)
    }
}

/// `boundary` with gaps up to `gap` pixels wide closed.
pub fn close_gaps(boundary: &Bitmap, gap: u32) -> Bitmap {
    let r = gap.div_ceil(2) as usize;
    if r == 0 {
        return boundary.clone();
    }
    erode(&dilate(boundary, r), r)
}

#[cfg(test)]
mod tests {
    use super::super::{fill, FillOptions};
    use super::*;

    /// A 1 px square outline from (20, 20) to (60, 60) on 80 × 80, with a
    /// `gap` px opening in its right side.
    fn open_box(gap: usize) -> Bitmap {
        let mut open = Bitmap::filled(80, 80);
        for i in 20..=60 {
            for (x, y) in [(i, 20), (i, 60), (20, i), (60, i)] {
                open.set(x, y, false);
            }
        }
        for y in 38..38 + gap {
            open.set(60, y, true);
        }
        open
    }

    #[test]
    fn closing_bridges_gaps_without_thickening_lines() {
        let open = open_box(6);
        let closed = close_gaps(&open.inverted(), 8);
        assert!(closed.get(60, 40), "the gap is bridged");
        assert!(!closed.get(59, 30) && !closed.get(61, 30), "lines keep their width");
        assert_eq!(close_gaps(&open.inverted(), 0), open.inverted());
    }

    #[test]
    fn fill_stays_inside_a_box_with_a_gap() {
        let open = open_box(6);
        let plain = fill(&open, (40, 40), &FillOptions { expand: 0, ..FillOptions::default() }).unwrap();
        assert!(plain.region.get(5, 5), "without gap closing the fill escapes");

        let f = fill(&open, (40, 40), &FillOptions { expand: 0, gap: 8, ..FillOptions::default() }).unwrap();
        assert!(f.gaps_closed);
        assert!(!f.region.get(5, 5) && !f.region.get(70, 40), "nothing outside");
        assert!(f.region.get(21, 21) && f.region.get(59, 59), "right into the corners");
        assert!(f.region.get(60, 40), "the gap itself is coloured");
        assert!(!f.region.get(61, 40), "but not past it");
        assert_eq!(f.region.count(), 39 * 39 + 6, "the whole inside plus the gap");

        // Gaps bigger than the setting still leak.
        let wide = fill(&open_box(14), (40, 40), &FillOptions { expand: 0, gap: 8, ..FillOptions::default() }).unwrap();
        assert!(wide.region.get(5, 5));

        // Outside the box the closing doesn't get in the way either.
        let out = fill(&open, (5, 5), &FillOptions { expand: 0, gap: 8, ..FillOptions::default() }).unwrap();
        assert!(!out.region.get(40, 40));
        assert!(out.region.get(61, 40) && out.region.get(79, 79));
    }

    #[test]
    fn presets_round_trip() {
        for p in GapPreset::ALL {
            assert_eq!(GapPreset::of(p.pixels()), Some(p));
        }
        assert_eq!(GapPreset::of(7), None);
    }
}
