//! Page standards: trim size, bleed, safe area (the manga "inner frame",
//! kihon-waku), DPI and binding side, and the page geometry derived from them
//! (canvas size, frame rectangles, crop marks / tombo, webtoon slices).
//!
//! The canvas covers trim + bleed on every side. Crop marks are guides drawn
//! by the app over the bleed area; they aren't pixels.

/// `(x, y, w, h)` in canvas pixels.
use serde::{Deserialize, Serialize};

use crate::paper::{Orientation, PaperSize, STANDARD_BLEED_MM, STANDARD_SAFE_MM};

pub type RectPx = [f64; 4];
pub type Point = [f64; 2];

pub const MM_PER_INCH: f64 = 25.4;

pub fn mm_to_px(mm: f64, dpi: u32) -> f64 {
    mm / MM_PER_INCH * dpi as f64
}

pub fn inch_to_mm(inches: f64) -> f64 {
    inches * MM_PER_INCH
}

/// Which side the book is bound on, i.e. the reading direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Binding {
    /// Western: pages and panels read left to right.
    LeftToRight,
    /// Manga: right to left.
    RightToLeft,
    /// Webtoon: one vertical strip, top to bottom.
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Oriental,
    Western,
    Webtoon,
    Custom,
}

/// A continuous vertical canvas cut into upload-sized slices.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WebtoonSpec {
    pub width_px: u32,
    pub length_px: u32,
    /// Height of each exported slice; markers are drawn at multiples of it.
    pub slice_px: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PagePreset {
    pub name: String,
    pub category: Category,
    /// Finished page size in millimetres (width, height).
    pub trim_mm: [f64; 2],
    /// Extra art past the trim on every side, cut off when printing.
    pub bleed_mm: f64,
    /// Inner frame (safe area) size in millimetres, centred on the trim; text
    /// and important art stay inside it. `None` = trim minus `safe_margin_mm`.
    pub inner_frame_mm: Option<[f64; 2]>,
    /// Margin inside the trim when `inner_frame_mm` isn't given.
    pub safe_margin_mm: f64,
    pub dpi: u32,
    pub binding: Binding,
    /// Default gap between panels.
    pub gutter_mm: [f64; 2],
    pub webtoon: Option<WebtoonSpec>,
}

impl PagePreset {
    /// Built-in formats. Inner frames follow the common Japanese submission
    /// guides (B5 manuscript: 150 × 220 mm; A4 doujinshi: 180 × 270 mm).
    pub fn builtin() -> Vec<PagePreset> {
        let page =
            |name: &str, category, trim: [f64; 2], bleed, inner: Option<[f64; 2]>, margin, binding, gutter: [f64; 2]| PagePreset {
                name: name.into(),
                category,
                trim_mm: trim,
                bleed_mm: bleed,
                inner_frame_mm: inner,
                safe_margin_mm: margin,
                dpi: 600,
                binding,
                gutter_mm: gutter,
                webtoon: None,
            };
        let us_trim = [inch_to_mm(6.625), inch_to_mm(10.25)];
        vec![
            page("Manga B5", Category::Oriental, [182.0, 257.0], 3.0, Some([150.0, 220.0]), 0.0, Binding::RightToLeft, [2.0, 5.0]),
            page("Doujinshi A4", Category::Oriental, [210.0, 297.0], 3.0, Some([180.0, 270.0]), 0.0, Binding::RightToLeft, [2.0, 5.0]),
            page("Doujinshi B5", Category::Oriental, [182.0, 257.0], 3.0, Some([150.0, 220.0]), 0.0, Binding::RightToLeft, [2.0, 5.0]),
            page("Franco-Belgian A4", Category::Western, [210.0, 297.0], 5.0, None, 12.0, Binding::LeftToRight, [4.0, 4.0]),
            page("Franco-Belgian album 24 × 32", Category::Western, [240.0, 320.0], 5.0, None, 12.0, Binding::LeftToRight, [4.0, 4.0]),
            // 1/8" bleed and a 5/16" safe margin, the usual US printer specs.
            page(
                "US comic book",
                Category::Western,
                us_trim,
                inch_to_mm(0.125),
                None,
                inch_to_mm(0.3125),
                Binding::LeftToRight,
                [3.0, 3.0],
            ),
            page("Bonelli 16 × 21", Category::Western, [160.0, 210.0], 3.0, None, 10.0, Binding::LeftToRight, [3.0, 3.0]),
            PagePreset {
                name: "Webtoon".into(),
                category: Category::Webtoon,
                trim_mm: [0.0, 0.0],
                bleed_mm: 0.0,
                inner_frame_mm: None,
                safe_margin_mm: 0.0,
                dpi: 72,
                binding: Binding::Vertical,
                gutter_mm: [0.0, 0.0],
                webtoon: Some(WebtoonSpec { width_px: 800, length_px: 12_800, slice_px: 1_280 }),
            },
        ]
    }

    /// A custom page; `None` when the sizes don't make a page.
    #[allow(clippy::too_many_arguments)]
    pub fn custom(
        name: &str,
        trim_mm: [f64; 2],
        bleed_mm: f64,
        safe_margin_mm: f64,
        dpi: u32,
        binding: Binding,
        gutter_mm: [f64; 2],
    ) -> Option<PagePreset> {
        let ok = trim_mm.iter().all(|&v| v > 0.0)
            && bleed_mm >= 0.0
            && safe_margin_mm >= 0.0
            && safe_margin_mm * 2.0 < trim_mm[0].min(trim_mm[1])
            && (72..=2400).contains(&dpi);
        ok.then(|| PagePreset {
            name: name.into(),
            category: Category::Custom,
            trim_mm,
            bleed_mm,
            inner_frame_mm: None,
            safe_margin_mm,
            dpi,
            binding,
            gutter_mm,
            webtoon: None,
        })
    }

    /// A custom page on a standard paper size, with the usual 3 mm bleed and
    /// 5 mm safe margin.
    pub fn from_paper(paper: PaperSize, orientation: Orientation, dpi: u32, binding: Binding) -> PagePreset {
        let name = match orientation {
            Orientation::Portrait => paper.name.to_string(),
            Orientation::Landscape => format!("{} landscape", paper.name),
        };
        PagePreset {
            name,
            category: Category::Custom,
            trim_mm: paper.oriented(orientation),
            bleed_mm: STANDARD_BLEED_MM,
            inner_frame_mm: None,
            safe_margin_mm: STANDARD_SAFE_MM,
            dpi,
            binding,
            gutter_mm: [3.0, 4.0],
            webtoon: None,
        }
    }

    pub fn geometry(&self) -> PageGeometry {
        if let Some(w) = self.webtoon {
            let full = [0.0, 0.0, w.width_px as f64, w.length_px as f64];
            return PageGeometry {
                canvas: (w.width_px, w.length_px),
                bleed: full,
                trim: full,
                inner: full,
                binding: self.binding,
                webtoon: Some(w),
            };
        }
        let px = |mm: f64| mm_to_px(mm, self.dpi);
        let bleed = px(self.bleed_mm);
        let (tw, th) = (px(self.trim_mm[0]), px(self.trim_mm[1]));
        let canvas = ((tw + 2.0 * bleed).round() as u32, (th + 2.0 * bleed).round() as u32);
        let trim = [bleed, bleed, tw, th];
        let inner = match self.inner_frame_mm {
            Some([w, h]) => {
                let (iw, ih) = (px(w), px(h));
                [bleed + (tw - iw) * 0.5, bleed + (th - ih) * 0.5, iw, ih]
            }
            None => {
                let m = px(self.safe_margin_mm);
                [bleed + m, bleed + m, tw - 2.0 * m, th - 2.0 * m]
            }
        };
        PageGeometry { canvas, bleed: [0.0, 0.0, canvas.0 as f64, canvas.1 as f64], trim, inner, binding: self.binding, webtoon: None }
    }

    pub fn gutter(&self) -> GutterMetrics {
        GutterMetrics { horizontal: mm_to_px(self.gutter_mm[0], self.dpi), vertical: mm_to_px(self.gutter_mm[1], self.dpi) }
    }
}

/// Gaps between panels in pixels: `horizontal` between side-by-side panels,
/// `vertical` between tiers (manga pages use a wider vertical gap).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GutterMetrics {
    pub horizontal: f64,
    pub vertical: f64,
}

impl GutterMetrics {
    pub fn uniform(px: f64) -> Self {
        Self { horizontal: px, vertical: px }
    }

    /// The gap for a cut whose line runs along `direction`: a mostly vertical
    /// line separates side-by-side panels.
    pub fn for_cut(&self, direction: Point) -> f64 {
        if direction[1].abs() >= direction[0].abs() {
            self.horizontal
        } else {
            self.vertical
        }
    }
}

/// Where everything is on the canvas, in pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageGeometry {
    pub canvas: (u32, u32),
    /// Outer frame: the whole canvas, trim plus bleed.
    pub bleed: RectPx,
    /// The finished page edge.
    pub trim: RectPx,
    /// The inner frame / safe area.
    pub inner: RectPx,
    pub binding: Binding,
    pub webtoon: Option<WebtoonSpec>,
}

impl PageGeometry {
    /// Crop marks (tombo) as line segments: at each trim corner a mark along
    /// the trim edge and one along the bleed edge, plus centre marks. They
    /// sit in the bleed area, `length` pixels long.
    pub fn crop_marks(&self, length: f64) -> Vec<(Point, Point)> {
        if self.webtoon.is_some() {
            return Vec::new();
        }
        let [x, y, w, h] = self.trim;
        let b = x; // bleed width
        let mut out = Vec::new();
        for (cx, sx) in [(x, -1.0), (x + w, 1.0)] {
            for (cy, sy) in [(y, -1.0), (y + h, 1.0)] {
                // Trim marks: extensions of the trim edges into the margin.
                out.push(([cx, cy + sy * b], [cx, cy + sy * (b + length)]));
                out.push(([cx + sx * b, cy], [cx + sx * (b + length), cy]));
                // Bleed marks: the double line at the bleed edge.
                out.push(([cx + sx * b, cy + sy * b], [cx + sx * b, cy + sy * (b + length)]));
                out.push(([cx + sx * b, cy + sy * b], [cx + sx * (b + length), cy + sy * b]));
            }
        }
        // Centre marks on each side.
        let (mx, my) = (x + w * 0.5, y + h * 0.5);
        out.push(([mx, y - b], [mx, y - b - length]));
        out.push(([mx, y + h + b], [mx, y + h + b + length]));
        out.push(([x - b, my], [x - b - length, my]));
        out.push(([x + w + b, my], [x + w + b + length, my]));
        out
    }

    /// Webtoon slice boundaries (y in pixels), excluding the canvas ends.
    pub fn slice_markers(&self) -> Vec<f64> {
        match self.webtoon {
            Some(w) if w.slice_px > 0 => (1..).map(|k| (k * w.slice_px) as f64).take_while(|&y| y < w.length_px as f64).collect(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(name: &str) -> PagePreset {
        PagePreset::builtin().into_iter().find(|p| p.name == name).unwrap()
    }

    #[test]
    fn manga_b5_at_600_dpi() {
        let g = preset("Manga B5").geometry();
        // (182 + 6) mm × (257 + 6) mm at 600 dpi.
        assert_eq!(g.canvas, (4441, 6213));
        let bleed = mm_to_px(3.0, 600);
        assert!((g.trim[0] - bleed).abs() < 1e-9);
        // The 150 × 220 inner frame is centred on the trim.
        assert!((g.inner[2] - mm_to_px(150.0, 600)).abs() < 1e-9);
        assert!((g.inner[0] - (g.trim[0] + (g.trim[2] - g.inner[2]) / 2.0)).abs() < 1e-9);
        assert_eq!(g.binding, Binding::RightToLeft);
        assert_eq!(g.crop_marks(40.0).len(), 4 * 4 + 4);
    }

    #[test]
    fn us_comic_and_custom_pages() {
        let us = preset("US comic book");
        assert!((us.trim_mm[0] - 168.275).abs() < 1e-9);
        let g = us.geometry();
        assert!((g.inner[0] - mm_to_px(inch_to_mm(0.125 + 0.3125), 600)).abs() < 1e-6);
        assert!(
            PagePreset::custom("x", [100.0, 100.0], 3.0, 60.0, 300, Binding::LeftToRight, [3.0, 3.0]).is_none(),
            "margins too big"
        );
        let c = PagePreset::custom("Square", [200.0, 200.0], 3.0, 10.0, 300, Binding::LeftToRight, [3.0, 5.0]).unwrap();
        assert_eq!(c.geometry().canvas.0, (206.0 / 25.4 * 300.0_f64).round() as u32);
        let gutter = c.gutter();
        assert_eq!(gutter.for_cut([0.0, 1.0]), gutter.horizontal);
        assert_eq!(gutter.for_cut([1.0, 0.1]), gutter.vertical);
    }

    #[test]
    fn pages_on_iso_paper() {
        let a5 = PagePreset::from_paper(PaperSize::get("A5").unwrap(), Orientation::Landscape, 300, Binding::LeftToRight);
        assert_eq!(a5.trim_mm, [210.0, 148.0]);
        assert_eq!(a5.name, "A5 landscape");
        let g = a5.geometry();
        // (210 + 6) × (148 + 6) mm at 300 dpi.
        assert_eq!(g.canvas, (2551, 1819));
        assert!((g.inner[0] - mm_to_px(3.0 + 5.0, 300)).abs() < 1e-9);
    }

    #[test]
    fn webtoon_slices() {
        let g = preset("Webtoon").geometry();
        assert_eq!(g.canvas, (800, 12_800));
        let slices = g.slice_markers();
        assert_eq!(slices.len(), 9);
        assert_eq!(slices[0], 1280.0);
        assert!(g.crop_marks(10.0).is_empty());
    }
}
