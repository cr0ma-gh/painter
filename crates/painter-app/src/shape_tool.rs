//! The Shape tool (R; Clip Studio's Figure, Photoshop's shapes): rectangles,
//! ellipses, lines and polygons, drawn as an outline with the current brush
//! (its dynamics, size, blend mode…), filled with the main colour, or both.
//!
//! Drag from corner to corner (Shift: square / circle, lines in 15° steps;
//! Alt: from the centre). Polygons: click their corners, then click the first
//! one again, double-click or press Enter to close; Esc drops it.

use eframe::egui::{self, Color32, Painter, Pos2, Rect, Stroke};
use painter_core::{selection, BrushMode, StrokePoint};
use painter_render::Plane;

use crate::app::PainterApp;
use crate::smart_shape::Shape;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShapeKind {
    Rectangle,
    Ellipse,
    Line,
    Polygon,
}

impl ShapeKind {
    pub const ALL: [ShapeKind; 4] = [ShapeKind::Rectangle, ShapeKind::Ellipse, ShapeKind::Line, ShapeKind::Polygon];

    pub fn name(self) -> &'static str {
        match self {
            ShapeKind::Rectangle => "Rectangle",
            ShapeKind::Ellipse => "Ellipse",
            ShapeKind::Line => "Line",
            ShapeKind::Polygon => "Polygon",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShapeStyle {
    Outline,
    Fill,
    Both,
}

impl ShapeStyle {
    pub const ALL: [ShapeStyle; 3] = [ShapeStyle::Outline, ShapeStyle::Fill, ShapeStyle::Both];

    pub fn name(self) -> &'static str {
        match self {
            ShapeStyle::Outline => "Outline",
            ShapeStyle::Fill => "Fill",
            ShapeStyle::Both => "Fill and outline",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ShapeOptions {
    pub kind: ShapeKind,
    pub style: ShapeStyle,
    /// Corner rounding of rectangles, in canvas pixels.
    pub corner: f32,
}

impl Default for ShapeOptions {
    fn default() -> Self {
        Self { kind: ShapeKind::Rectangle, style: ShapeStyle::Outline, corner: 0.0 }
    }
}

/// A shape being placed: a drag, or a polygon's corners so far.
#[derive(Clone, Debug, Default)]
pub(crate) struct ShapeDraft {
    pub start: Option<Pos2>,
    pub current: Pos2,
    pub corners: Vec<Pos2>,
}

/// The shape a drag from `a` to `b` makes, with Shift / Alt.
pub(crate) fn dragged_shape(kind: ShapeKind, a: Pos2, b: Pos2, constrain: bool, centred: bool, corner: f32) -> Option<Shape> {
    let mut d = b - a;
    if constrain {
        match kind {
            ShapeKind::Line => {
                let snapped = crate::lines::snap_angle(a, b, 15.0);
                d = snapped - a;
            }
            _ => {
                let s = d.x.abs().max(d.y.abs());
                d = egui::vec2(s * d.x.signum(), s * d.y.signum());
            }
        }
    }
    let (p0, p1) = if centred && kind != ShapeKind::Line { (a - d, a + d) } else { (a, a + d) };
    if (p1 - p0).length() < 1.0 {
        return None;
    }
    let (x0, y0, x1, y1) = (p0.x.min(p1.x), p0.y.min(p1.y), p0.x.max(p1.x), p0.y.max(p1.y));
    Some(match kind {
        ShapeKind::Line => Shape::Line([p0.x, p0.y], [p1.x, p1.y]),
        ShapeKind::Ellipse => {
            Shape::Ellipse { center: [(x0 + x1) / 2.0, (y0 + y1) / 2.0], radii: [(x1 - x0) / 2.0, (y1 - y0) / 2.0], angle: 0.0 }
        }
        ShapeKind::Rectangle => Shape::Polygon { corners: rounded_rect([x0, y0, x1, y1], corner), closed: true },
        ShapeKind::Polygon => return None,
    })
}

/// A rectangle's outline, its corners rounded by `r` pixels (8 points per corner).
pub(crate) fn rounded_rect([x0, y0, x1, y1]: [f32; 4], r: f32) -> Vec<[f32; 2]> {
    let r = r.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0).max(0.0);
    if r < 0.5 {
        return vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
    }
    let mut out = Vec::new();
    for (cx, cy, start) in [(x1 - r, y0 + r, -90.0f32), (x1 - r, y1 - r, 0.0), (x0 + r, y1 - r, 90.0), (x0 + r, y0 + r, 180.0)] {
        for k in 0..=8 {
            let a = (start + k as f32 * 90.0 / 8.0).to_radians();
            out.push([cx + r * a.cos(), cy + r * a.sin()]);
        }
    }
    out
}

impl PainterApp {
    pub(crate) fn shape_press(&mut self, at: Pos2, double: bool) {
        let kind = self.tools.shapes.kind;
        if kind == ShapeKind::Polygon {
            let reach = 8.0 / self.view.zoom.max(1e-3);
            let closes = self.shape.corners.len() >= 3 && self.shape.corners[0].distance(at) <= reach;
            if double || closes {
                self.finish_polygon();
            } else {
                self.shape.corners.push(at);
                self.shape.current = at;
            }
            return;
        }
        self.shape.start = Some(at);
        self.shape.current = at;
    }

    pub(crate) fn shape_move(&mut self, at: Pos2) {
        self.shape.current = at;
    }

    pub(crate) fn shape_release(&mut self) {
        let Some(start) = self.shape.start.take() else { return };
        let o = &self.tools.shapes;
        let m = self.pointer_mods;
        if let Some(shape) = dragged_shape(o.kind, start, self.shape.current, m.shift, m.alt, o.corner) {
            self.draw_shape(&shape);
        }
    }

    /// Enter, or a click on the first corner: the polygon is drawn.
    pub(crate) fn finish_polygon(&mut self) {
        let corners: Vec<[f32; 2]> = std::mem::take(&mut self.shape.corners).iter().map(|p| [p.x, p.y]).collect();
        if corners.len() >= 2 {
            self.draw_shape(&Shape::Polygon { closed: corners.len() >= 3, corners });
        }
    }

    pub(crate) fn shape_keys(&mut self, ctx: &egui::Context) {
        if self.tool != crate::tools::Tool::Shape || self.shape.corners.is_empty() || ctx.wants_keyboard_input() {
            return;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.finish_polygon();
        } else if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.shape.corners.clear();
        }
    }

    /// Fills and / or outlines `shape` on the edited layer (one undo step each).
    fn draw_shape(&mut self, shape: &Shape) {
        let style = self.tools.shapes.style;
        let canvas = self.doc.canvas();
        if matches!(style, ShapeStyle::Fill | ShapeStyle::Both) {
            if let Some(plane) = self.fill_target() {
                let mask = match shape {
                    Shape::Ellipse { center, radii, .. } => selection::ellipse_mask(
                        [center[0] - radii[0], center[1] - radii[1]],
                        [center[0] + radii[0], center[1] + radii[1]],
                        canvas,
                    ),
                    other => selection::polygon_mask(&other.outline(2.0), canvas),
                };
                let color = if matches!(plane, Plane::Mask(_)) { [1.0; 3] } else { self.main_rgb() };
                let stroke = painter_core::fill_tool::region_to_stroke(&mask, color);
                self.next_label = Some(format!("{} (fill)", self.tools.shapes.kind.name()));
                self.commit_region(plane, &stroke, BrushMode::Paint);
            }
        }
        if matches!(style, ShapeStyle::Outline | ShapeStyle::Both) || matches!(shape, Shape::Line(..)) {
            // With the current brush, as if drawn by a steady hand at full pressure.
            let t0 = self.started.elapsed().as_secs_f32();
            let mut points = shape
                .outline(2.0)
                .into_iter()
                .enumerate()
                .map(|(i, [x, y])| StrokePoint::new(x, y, 1.0).at_time(t0 + i as f32 * 2.0 / 600.0));
            let Some(first) = points.next() else { return };
            self.next_label = Some(self.tools.shapes.kind.name().into());
            self.begin_stroke(first);
            for p in points {
                self.continue_stroke(p);
            }
            self.end_stroke();
        }
    }

    /// The plane a fill goes on (raster layers and masks; not vector layers).
    fn fill_target(&mut self) -> Option<Plane> {
        let plane = self.target_plane()?;
        if matches!(plane, Plane::Color(id) if self.doc.find(id).is_some_and(painter_core::Layer::is_vector)) {
            self.status = "Vector layers take outlines; fills go on raster layers".into();
            return None;
        }
        Some(plane)
    }

    /// The shape being placed, over the canvas.
    pub(crate) fn draw_shape_draft(&self, painter: &Painter, rect: Rect) {
        if self.tool != crate::tools::Tool::Shape {
            return;
        }
        let doc = self.doc_size();
        let screen = |[x, y]: [f32; 2]| self.view.doc_to_screen(rect, doc, Pos2::new(x, y));
        let line = |pts: Vec<Pos2>| {
            painter.add(egui::Shape::line(pts.clone(), Stroke::new(3.0_f32, Color32::from_black_alpha(110))));
            painter.add(egui::Shape::line(pts, Stroke::new(1.5_f32, crate::theme::accent())));
        };
        let o = &self.tools.shapes;
        if let Some(start) = self.shape.start {
            let m = self.pointer_mods;
            if let Some(shape) = dragged_shape(o.kind, start, self.shape.current, m.shift, m.alt, o.corner) {
                line(shape.outline(3.0).into_iter().map(screen).collect());
            }
        }
        if !self.shape.corners.is_empty() {
            let mut pts: Vec<Pos2> = self.shape.corners.iter().map(|p| screen([p.x, p.y])).collect();
            pts.push(screen([self.shape.current.x, self.shape.current.y]));
            line(pts);
            for p in &self.shape.corners {
                painter.circle_filled(screen([p.x, p.y]), 3.5, Color32::WHITE);
            }
        }
    }

    /// For screenshots (`dev.shape_demo`): one shape of each style.
    pub(crate) fn shape_demo(&mut self) {
        let (w, h) = self.doc.canvas();
        let (w, h) = (w as f32, h as f32);
        let at = |x: f32, y: f32| Pos2::new(w * x, h * y);
        self.tools.shapes = ShapeOptions { kind: ShapeKind::Rectangle, style: ShapeStyle::Both, corner: w / 20.0 };
        let rect = dragged_shape(ShapeKind::Rectangle, at(0.1, 0.1), at(0.5, 0.35), false, false, w / 20.0);
        let ellipse = dragged_shape(ShapeKind::Ellipse, at(0.55, 0.4), at(0.9, 0.7), false, false, 0.0);
        let line = dragged_shape(ShapeKind::Line, at(0.1, 0.8), at(0.9, 0.9), false, false, 0.0);
        for (shape, style) in [(rect, ShapeStyle::Both), (ellipse, ShapeStyle::Outline), (line, ShapeStyle::Outline)] {
            self.tools.shapes.style = style;
            if let Some(shape) = shape {
                self.draw_shape(&shape);
            }
        }
        self.draw_shape(&Shape::Polygon {
            corners: vec![[w * 0.15, h * 0.7], [w * 0.3, h * 0.45], [w * 0.45, h * 0.7]],
            closed: true,
        });
        self.tool = crate::tools::Tool::Shape;
    }

    pub(crate) fn shape_panel(&mut self, ui: &mut egui::Ui) {
        let o = &mut self.tools.shapes;
        ui.horizontal_wrapped(|ui| {
            for k in ShapeKind::ALL {
                ui.selectable_value(&mut o.kind, k, k.name());
            }
        });
        if o.kind != ShapeKind::Line {
            ui.horizontal_wrapped(|ui| {
                for s in ShapeStyle::ALL {
                    ui.selectable_value(&mut o.style, s, s.name());
                }
            });
        }
        if o.kind == ShapeKind::Rectangle {
            ui.add(
                egui::Slider::new(&mut o.corner, 0.0..=500.0).logarithmic(true).max_decimals(0).text("Rounded corners").suffix(" px"),
            );
        }
        ui.small(match o.kind {
            ShapeKind::Polygon => "Click the corners; click the first again, double-click or Enter to close. Esc drops it.",
            _ => "Drag on the canvas. Shift: square / circle (lines: 15° steps). Alt: from the centre.",
        });
        ui.small("Outlines use the current brush; fills the main colour.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drags_make_shapes() {
        let a = Pos2::new(10.0, 10.0);
        // Shift: a square, whichever way the drag went.
        let Some(Shape::Polygon { corners, closed: true }) =
            dragged_shape(ShapeKind::Rectangle, a, Pos2::new(50.0, 30.0), true, false, 0.0)
        else {
            panic!()
        };
        assert_eq!(corners, vec![[10.0, 10.0], [50.0, 10.0], [50.0, 50.0], [10.0, 50.0]]);
        // Alt: from the centre.
        let Some(Shape::Ellipse { center, radii, .. }) = dragged_shape(ShapeKind::Ellipse, a, Pos2::new(20.0, 15.0), false, true, 0.0)
        else {
            panic!()
        };
        assert_eq!((center, radii), ([10.0, 10.0], [10.0, 5.0]));
        // Lines snap to 15°.
        let Some(Shape::Line(_, b)) = dragged_shape(ShapeKind::Line, a, Pos2::new(110.0, 14.0), true, false, 0.0) else { panic!() };
        assert!((b[1] - 10.0).abs() < 1e-3);
        assert!(dragged_shape(ShapeKind::Ellipse, a, a, false, false, 0.0).is_none());
        // Rounded corners stay within the box.
        let r = rounded_rect([0.0, 0.0, 100.0, 50.0], 10.0);
        assert!(r.iter().all(|p| (0.0..=100.0).contains(&p[0]) && (0.0..=50.0).contains(&p[1])));
        assert_eq!(r.len(), 36);
    }
}
