//! Page layout templates as split trees: the inner frame is cut recursively,
//! exactly like the Divide tool does, so templates get the same gutters and
//! diagonal cuts as hand-made layouts.

use crate::frame_tool::{split, Cut, Panel, PanelId};
use crate::page_preset::{Category, GutterMetrics, PageGeometry, RectPx};

/// One node of a layout: a panel, or a cut whose two sides are laid out further.
#[derive(Debug, Clone, PartialEq)]
pub enum Layout {
    Panel,
    /// Side-by-side (or stacked) parts sized by weights after the gutters
    /// are taken out, so equal weights give equal panels.
    Split {
        horizontal: bool,
        weights: Vec<f64>,
        children: Vec<Layout>,
    },
    /// A cut from `a` to `b`, in fractions of the node's bounding box. The
    /// first child is above / left of the line.
    Cut {
        a: [f64; 2],
        b: [f64; 2],
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

impl Layout {
    /// Two tiers, the top one taking `t` of the height (after the gutter).
    pub fn rows(t: f64, top: Layout, bottom: Layout) -> Layout {
        Layout::Split { horizontal: true, weights: vec![t, 1.0 - t], children: vec![top, bottom] }
    }

    /// Two columns, the left one taking `t` of the width (after the gutter).
    pub fn columns(t: f64, left: Layout, right: Layout) -> Layout {
        Layout::Split { horizontal: false, weights: vec![t, 1.0 - t], children: vec![left, right] }
    }

    /// `n` equal tiers.
    pub fn tiers(n: usize, each: impl Fn(usize) -> Layout) -> Layout {
        Layout::Split { horizontal: true, weights: vec![1.0; n.max(1)], children: (0..n.max(1)).map(each).collect() }
    }

    /// `n` equal columns.
    pub fn strip(n: usize, each: impl Fn(usize) -> Layout) -> Layout {
        Layout::Split { horizontal: false, weights: vec![1.0; n.max(1)], children: (0..n.max(1)).map(each).collect() }
    }
    pub fn panel_count(&self) -> usize {
        match self {
            Layout::Panel => 1,
            Layout::Cut { first, second, .. } => first.panel_count() + second.panel_count(),
            Layout::Split { children, .. } => children.iter().map(Layout::panel_count).sum(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    pub name: &'static str,
    pub description: &'static str,
    /// Formats it's meant for (shown first in the wizard for them).
    pub suits: &'static [Category],
    pub layout: Layout,
}

impl Template {
    pub fn builtin() -> Vec<Template> {
        use Category::*;
        let p = || Layout::Panel;
        vec![
            Template { name: "Splash", description: "One full-page panel", suits: &[Oriental, Western, Custom], layout: p() },
            Template {
                name: "Franco-Belgian 4 tiers",
                description: "Four tiers of two to three panels (the classic album page)",
                suits: &[Western],
                layout: Layout::tiers(4, |i| if i % 2 == 0 { Layout::strip(3, |_| p()) } else { Layout::strip(2, |_| p()) }),
            },
            Template {
                name: "US 6-panel grid",
                description: "Three tiers of two",
                suits: &[Western],
                layout: Layout::tiers(3, |_| Layout::strip(2, |_| p())),
            },
            Template {
                name: "Waffle 9",
                description: "A 3 × 3 grid",
                suits: &[Western, Custom],
                layout: Layout::tiers(3, |_| Layout::strip(3, |_| p())),
            },
            Template {
                name: "Bonelli 3 tiers",
                description: "Three tiers of two (the Italian 'gabbia')",
                suits: &[Western],
                layout: Layout::tiers(3, |_| Layout::strip(2, |_| p())),
            },
            Template {
                name: "Manga dynamic 6",
                description: "Wide opener, a diagonal pair, a tall panel beside two",
                suits: &[Oriental],
                layout: Layout::rows(
                    0.3,
                    p(),
                    Layout::rows(
                        0.45,
                        Layout::Cut { a: [0.62, 0.0], b: [0.48, 1.0], first: Box::new(p()), second: Box::new(p()) },
                        Layout::columns(0.4, p(), Layout::rows(0.5, p(), p())),
                    ),
                ),
            },
            Template {
                name: "4-koma",
                description: "Four equal vertical panels (yonkoma strip)",
                suits: &[Oriental],
                layout: Layout::tiers(4, |_| p()),
            },
            Template {
                name: "Webtoon stack",
                description: "Tall panels down the strip",
                suits: &[Webtoon],
                layout: Layout::tiers(5, |_| p()),
            },
        ]
    }

    /// The panels for `geometry`: the layout cut out of `area` (the inner
    /// frame by default) with the page gutters. Ids count from `first_id`.
    pub fn instantiate(&self, area: RectPx, gutter: &GutterMetrics, first_id: u32) -> Vec<Panel> {
        let mut next = first_id;
        let mut out = Vec::new();
        let root = Panel::rect(PanelId(next), area);
        next += 1;
        place(&self.layout, root, gutter, &mut next, &mut out);
        out
    }

    pub fn for_page(&self, geometry: &PageGeometry, gutter: &GutterMetrics, first_id: u32) -> Vec<Panel> {
        self.instantiate(geometry.inner, gutter, first_id)
    }
}

fn place(layout: &Layout, panel: Panel, gutter: &GutterMetrics, next: &mut u32, out: &mut Vec<Panel>) {
    match layout {
        Layout::Panel => out.push(panel),
        Layout::Split { horizontal, weights, children } => {
            let [x, y, w, h] = panel.bounds();
            let (start, length) = if *horizontal { (y, h) } else { (x, w) };
            let gap = if *horizontal { gutter.vertical } else { gutter.horizontal };
            let total: f64 = weights.iter().sum::<f64>().max(1e-9);
            let available = length - gap * (children.len().saturating_sub(1)) as f64;
            let mut rest = panel;
            let mut edge = start;
            for (i, child) in children.iter().enumerate() {
                if i + 1 == children.len() {
                    place(child, rest, gutter, next, out);
                    break;
                }
                edge += available * weights[i] / total;
                let line = edge + gap * 0.5;
                let cut = if *horizontal {
                    Cut { a: [x - 1.0, line], b: [x + w + 1.0, line] }
                } else {
                    Cut { a: [line, y - 1.0], b: [line, y + h + 1.0] }
                };
                edge += gap;
                let new_id = PanelId(*next);
                *next += 1;
                match split(&rest, cut, gap, new_id) {
                    Some((one, two)) => {
                        place(child, one, gutter, next, out);
                        rest = two;
                    }
                    None => {
                        place(child, rest, gutter, next, out);
                        break;
                    }
                }
            }
        }
        Layout::Cut { a, b, first, second } => {
            let [x, y, w, h] = panel.bounds();
            let at = |f: [f64; 2]| [x + w * f[0], y + h * f[1]];
            // Extend the cut past the box so it always crosses it.
            let (pa, pb) = (at(*a), at(*b));
            let d = [pb[0] - pa[0], pb[1] - pa[1]];
            let cut = Cut { a: [pa[0] - d[0], pa[1] - d[1]], b: [pb[0] + d[0], pb[1] + d[1]] };
            let gap = gutter.for_cut(d);
            let new_id = PanelId(*next);
            *next += 1;
            match split(&panel, cut, gap, new_id) {
                Some((one, two)) => {
                    place(first, one, gutter, next, out);
                    place(second, two, gutter, next, out);
                }
                // Too small to cut: keep it whole.
                None => out.push(panel),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame_tool::reading_order;
    use crate::page_preset::{Binding, PagePreset};

    #[test]
    fn templates_fill_the_frame_with_gutters() {
        let gutter = GutterMetrics { horizontal: 10.0, vertical: 20.0 };
        let grid = Template::builtin().into_iter().find(|t| t.name == "US 6-panel grid").unwrap();
        let panels = grid.instantiate([0.0, 0.0, 210.0, 340.0], &gutter, 1);
        assert_eq!(panels.len(), grid.layout.panel_count());
        assert_eq!(panels.len(), 6);
        let first = panels[0].bounds();
        assert!((first[2] - 100.0).abs() < 1e-9, "(210 − 10) / 2 wide: {first:?}");
        assert!((first[3] - 100.0).abs() < 1e-6, "(340 − 2 × 20) / 3 high: {first:?}");
        // Ids are unique; reading order runs row by row.
        let mut ids: Vec<u32> = panels.iter().map(|p| p.id.0).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 6);
        let order = reading_order(&panels, Binding::RightToLeft);
        assert_eq!(panels.iter().find(|p| p.id == order[0]).unwrap().bounds()[0], 110.0, "manga starts top right");
    }

    #[test]
    fn every_template_instantiates_on_its_format() {
        for preset in PagePreset::builtin() {
            let geometry = preset.geometry();
            for template in Template::builtin().iter().filter(|t| t.suits.contains(&preset.category)) {
                let panels = template.for_page(&geometry, &preset.gutter(), 1);
                assert_eq!(panels.len(), template.layout.panel_count(), "{} on {}", template.name, preset.name);
                let [ix, iy, iw, ih] = geometry.inner;
                for p in &panels {
                    let [x, y, w, h] = p.bounds();
                    assert!(x >= ix - 1e-6 && y >= iy - 1e-6 && x + w <= ix + iw + 1e-6 && y + h <= iy + ih + 1e-6);
                }
            }
        }
    }
}
