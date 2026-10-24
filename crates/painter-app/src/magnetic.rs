//! The magnetic lasso (Shift+L) and object selection (Shift+W), as in
//! Photoshop, on the visible image.
//!
//! Magnetic lasso: click to start, move along an outline (no button held):
//! the path snaps to the strongest edges and drops anchors as it goes. A click
//! adds an anchor, Backspace takes the last one back, a click on the start, a
//! double-click or Enter closes it, Esc drops it.
//!
//! Object selection: drag a rectangle round something; what stands out from
//! the background reaching in from the rectangle's border is selected.

use eframe::egui::{self, Color32, Modifiers, Painter, Pos2, Rect, Stroke};
use crate::i18n::tr;
use painter_core::smart_select::{edge_strength, live_wire, object_in_rect};
use painter_core::{selection, SelectionOp};

use crate::app::PainterApp;
use crate::tools::{SelectShape, Tool};

pub(crate) struct MagneticOptions {
    /// Screen pixels between automatic anchors.
    pub spacing: f32,
    /// Object selection: how different from the background counts as the object (0..=255).
    pub tolerance: u8,
}

impl Default for MagneticOptions {
    fn default() -> Self {
        Self { spacing: 40.0, tolerance: 32 }
    }
}

/// The visible image's edges around the last anchor.
struct EdgeCrop {
    origin: (i32, i32),
    w: usize,
    h: usize,
    edges: Vec<f32>,
}

#[derive(Default)]
pub(crate) struct MagneticLasso {
    /// The fixed path so far, canvas pixels; anchors are indices into it.
    path: Vec<Pos2>,
    anchors: Vec<usize>,
    /// From the last anchor to the pointer.
    live: Vec<Pos2>,
    op: Option<SelectionOp>,
    crop: Option<EdgeCrop>,
}

impl MagneticLasso {
    pub fn active(&self) -> bool {
        !self.path.is_empty()
    }
}

impl PainterApp {
    fn edge_crop_around(&self, at: Pos2, radius: f32) -> EdgeCrop {
        let (cw, ch) = self.doc.canvas();
        let r = radius.ceil() as i32;
        let x0 = (at.x as i32 - r).clamp(0, cw as i32 - 1);
        let y0 = (at.y as i32 - r).clamp(0, ch as i32 - 1);
        let x1 = (at.x as i32 + r).clamp(x0 + 1, cw as i32);
        let y1 = (at.y as i32 + r).clamp(y0 + 1, ch as i32);
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let rgba = painter_core::io::flatten_region(&self.doc, x0 as u32, y0 as u32, w as u32, h as u32);
        EdgeCrop { origin: (x0, y0), w, h, edges: edge_strength(&rgba, w, h) }
    }

    /// Canvas pixels between automatic anchors at the current zoom.
    fn anchor_spacing(&self) -> f32 {
        (self.tools.magnetic.spacing / self.view.zoom.max(1e-3)).clamp(6.0, 400.0)
    }

    fn start_anchor(&mut self, at: Pos2) {
        self.magnetic.anchors.push(self.magnetic.path.len());
        self.magnetic.path.push(at);
        self.magnetic.live.clear();
        let crop = self.edge_crop_around(at, self.anchor_spacing() * 1.5 + 16.0);
        self.magnetic.crop = Some(crop);
    }

    pub(crate) fn magnetic_press(&mut self, at: Pos2, op: SelectionOp, double: bool) {
        if !self.magnetic.active() {
            self.magnetic.op = Some(op);
            self.start_anchor(at);
            return;
        }
        let reach = 8.0 / self.view.zoom.max(1e-3);
        let near_start = self.magnetic.path.len() > 2 && self.magnetic.path[0].distance(at) <= reach;
        if double || near_start {
            self.finish_magnetic();
            return;
        }
        // Fix the live path, anchor where it ends.
        self.magnetic_move(at);
        let live = std::mem::take(&mut self.magnetic.live);
        let end = live.last().copied().unwrap_or(at);
        self.magnetic.path.extend(live.iter().skip(1).take(live.len().saturating_sub(2)));
        self.start_anchor(end);
    }

    /// The pointer moved: the live path follows it along the edges.
    pub(crate) fn magnetic_move(&mut self, at: Pos2) {
        let Some(&start) = self.magnetic.path.last() else { return };
        let Some(crop) = &self.magnetic.crop else { return };
        let local = |p: Pos2| {
            let x = (p.x as i32 - crop.origin.0).clamp(0, crop.w as i32 - 1) as usize;
            let y = (p.y as i32 - crop.origin.1).clamp(0, crop.h as i32 - 1) as usize;
            (x, y)
        };
        let path = live_wire(&crop.edges, crop.w, crop.h, local(start), local(at));
        let (ox, oy) = (crop.origin.0 as f32 + 0.5, crop.origin.1 as f32 + 0.5);
        self.magnetic.live = path.into_iter().map(|(x, y)| Pos2::new(x as f32 + ox, y as f32 + oy)).collect();
        // Far enough: an anchor three quarters of the way, where the path is on the edge.
        if start.distance(at) > self.anchor_spacing() && self.magnetic.live.len() > 4 {
            let live = std::mem::take(&mut self.magnetic.live);
            let k = live.len() * 3 / 4;
            self.magnetic.path.extend(&live[1..k]);
            self.start_anchor(live[k]);
            self.magnetic_move(at);
        }
    }

    pub(crate) fn finish_magnetic(&mut self) {
        let mut lasso = std::mem::take(&mut self.magnetic);
        lasso.path.extend(lasso.live.iter().skip(1));
        if lasso.path.len() < 3 {
            return;
        }
        let points: Vec<[f32; 2]> = lasso.path.iter().map(|p| [p.x, p.y]).collect();
        let mask = selection::polygon_mask(&points, self.doc.canvas());
        self.combine_selection(mask, lasso.op.unwrap_or(SelectionOp::Replace));
    }

    pub(crate) fn magnetic_keys(&mut self, ctx: &egui::Context) {
        if !self.magnetic.active() || ctx.wants_keyboard_input() {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, egui::Key::Enter)) {
            self.finish_magnetic();
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, egui::Key::Escape))
            || self.tool != Tool::Select(SelectShape::Magnetic)
        {
            self.magnetic = Default::default();
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, egui::Key::Backspace)) {
            // Back to the anchor before the last.
            self.magnetic.anchors.pop();
            match self.magnetic.anchors.pop() {
                Some(k) => {
                    let at = self.magnetic.path[k];
                    self.magnetic.path.truncate(k);
                    self.start_anchor(at);
                }
                None => self.magnetic = Default::default(),
            }
        }
    }

    /// Object selection over the rectangle `a`–`b`.
    pub(crate) fn select_object(&mut self, a: [f32; 2], b: [f32; 2], op: SelectionOp) {
        let (cw, ch) = self.doc.canvas();
        let x0 = (a[0].min(b[0]).floor() as i32).clamp(0, cw as i32);
        let y0 = (a[1].min(b[1]).floor() as i32).clamp(0, ch as i32);
        let x1 = (a[0].max(b[0]).ceil() as i32).clamp(0, cw as i32);
        let y1 = (a[1].max(b[1]).ceil() as i32).clamp(0, ch as i32);
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let rgba = painter_core::io::flatten_region(&self.doc, x0 as u32, y0 as u32, w as u32, h as u32);
        let Some(found) = object_in_rect(&rgba, w, h, self.tools.magnetic.tolerance) else {
            self.status = tr("Object selection: nothing stands out from the background there").into();
            return;
        };
        let mask = selection::mask_from_fn((cw, ch), [x0, y0, x1 - 1, y1 - 1], |x, y| {
            if found[(y - y0) as usize * w + (x - x0) as usize] {
                1.0
            } else {
                0.0
            }
        });
        let count = found.iter().filter(|f| **f).count();
        self.combine_selection(mask, op);
        self.status = format!("Selected an object of {count} px");
    }

    pub(crate) fn draw_magnetic(&self, painter: &Painter, rect: Rect) {
        if !self.magnetic.active() {
            return;
        }
        let doc = self.doc_size();
        let screen = |p: &Pos2| self.view.doc_to_screen(rect, doc, *p);
        let mut pts: Vec<Pos2> = self.magnetic.path.iter().map(screen).collect();
        pts.extend(self.magnetic.live.iter().skip(1).map(screen));
        painter.add(egui::Shape::line(pts.clone(), Stroke::new(3.0_f32, Color32::from_black_alpha(150))));
        painter.add(egui::Shape::line(pts, Stroke::new(1.0_f32, Color32::WHITE)));
        for &k in &self.magnetic.anchors {
            let c = screen(&self.magnetic.path[k]);
            let r = Rect::from_center_size(c, egui::vec2(5.0, 5.0));
            painter.rect_filled(r, 0.0, Color32::WHITE);
            painter.rect_stroke(r, 0.0, Stroke::new(1.0_f32, Color32::BLACK), egui::StrokeKind::Outside);
        }
    }

    pub(crate) fn magnetic_panel(&mut self, ui: &mut egui::Ui, shape: SelectShape) {
        let o = &mut self.tools.magnetic;
        match shape {
            SelectShape::Magnetic => {
                ui.add(egui::Slider::new(&mut o.spacing, 10.0..=150.0).text(tr("Anchor spacing")).suffix(" px"));
                ui.small(tr("Click to start, then move along an outline. Click: anchor, Backspace: back one, Enter or click the start: close, Esc: drop."));
            }
            _ => {
                ui.add(egui::Slider::new(&mut o.tolerance, 0..=255).text(tr("Background tolerance")));
                ui.small(tr("Drag a rectangle round the object: what differs from the background at the rectangle's edge is selected."));
            }
        }
    }
}

impl PainterApp {
    /// For screenshots (`dev.select_demo`): a ring traced loosely with the
    /// magnetic lasso, and a disc picked out by object selection.
    pub(crate) fn select_demo(&mut self) {
        let (w, h) = self.doc.canvas();
        let (w, h) = (w as f32, h as f32);
        let (c, r) = (Pos2::new(w * 0.3, h * 0.3), w * 0.18);
        self.current_brush_mut().size = w / 150.0;
        self.tools.shapes.style = crate::shape_tool::ShapeStyle::Outline;
        self.draw_shape(&crate::smart_shape::Shape::Ellipse { center: [c.x, c.y], radii: [r, r], angle: 0.0 });
        self.tools.shapes.style = crate::shape_tool::ShapeStyle::Fill;
        let d = Pos2::new(w * 0.7, h * 0.7);
        self.draw_shape(&crate::smart_shape::Shape::Ellipse { center: [d.x, d.y], radii: [w * 0.12, w * 0.08], angle: 0.0 });
        // The pointer wobbles up to 4 % of the radius off the ring; the path should stay on it.
        self.tool = Tool::Select(SelectShape::Magnetic);
        let at = |t: f32| {
            let rr = r * (1.0 + 0.04 * (t * 7.0).sin());
            Pos2::new(c.x + rr * t.cos(), c.y + rr * t.sin())
        };
        self.magnetic_press(at(0.0), SelectionOp::Replace, false);
        for i in 1..=60 {
            self.magnetic_move(at(i as f32 / 60.0 * std::f32::consts::TAU * 0.97));
        }
        self.magnetic_press(at(0.0), SelectionOp::Replace, false);
        self.select_object([d.x - w * 0.16, d.y - w * 0.12], [d.x + w * 0.16, d.y + w * 0.12], SelectionOp::Add);
    }
}
