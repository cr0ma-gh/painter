//! The Vector Points tool (O): edit the lines of a vector layer after
//! drawing them, as Clip Studio's Object and Correct Line tools do.
//!
//! - Click a line to pick it: its control points show (the points that
//!   keep its shape, from the many the pen recorded).
//! - Drag a control point: the line bends there, easing out to the
//!   neighbouring control points. Alt + click removes it (straight between
//!   its neighbours).
//! - Drag the line itself to move it; Delete removes it.
//! - Tool Settings: its width and colour, simplify it, or delete it.
//!
//! The line follows the pointer as an outline while dragging and is drawn
//! again when released, with its brush; one undo step per change.

use eframe::egui::{self, Color32, Painter, Pos2, Rect, Stroke};
use crate::i18n::tr;
use painter_core::{vector, LayerId, LayerKind, SelectionOp, StrokePoint};
use painter_render::Plane;

use crate::app::PainterApp;
use crate::tools::ToolDrag;

/// Pick distance, in screen points.
const REACH: f32 = 8.0;
/// Control points keep the line within this many screen points.
const DETAIL: f32 = 2.5;

#[derive(Clone, Debug, Default)]
pub(crate) struct VectorEditState {
    pub picked: Option<(LayerId, usize)>,
    drag: Option<EditDrag>,
}

#[derive(Clone, Debug)]
struct EditDrag {
    /// `Some(c)`: a control point; `None`: the whole line.
    control: Option<usize>,
    controls: Vec<usize>,
    start: Pos2,
    original: Vec<StrokePoint>,
    /// The undo step is made on the first real move (a click only picks).
    recorded: bool,
}

impl PainterApp {
    /// The active layer when it's a vector layer.
    fn vector_layer(&self) -> Option<(LayerId, &painter_core::VectorLayer)> {
        let layer = self.doc.active_layer()?;
        match &layer.kind {
            LayerKind::Vector(v) => Some((layer.id, v)),
            _ => None,
        }
    }

    /// The picked line's points and control points, on the active vector layer.
    fn picked_line(&self) -> Option<(&[StrokePoint], Vec<usize>)> {
        let (id, layer) = self.vector_layer()?;
        let (lid, k) = self.vector_edit.picked?;
        let stroke = layer.strokes.get(k).filter(|_| lid == id)?;
        Some((&stroke.points, vector::control_points(stroke, DETAIL / self.view.zoom.max(1e-3))))
    }

    pub(crate) fn vector_edit_press(&mut self, at: Pos2, alt: bool) {
        let Some((id, layer)) = self.vector_layer() else {
            self.status = tr("Vector Points works on vector layers").into();
            return;
        };
        let reach = REACH / self.view.zoom.max(1e-3);
        // A control point of the picked line first.
        if let Some((points, controls)) = self.picked_line() {
            let hit = controls
                .iter()
                .enumerate()
                .map(|(c, &i)| (c, Pos2::new(points[i].x, points[i].y).distance(at)))
                .filter(|x| x.1 <= reach)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((c, _)) = hit {
                let k = self.vector_edit.picked.expect("picked").1;
                if alt {
                    self.checkpoint();
                    if let Some(LayerKind::Vector(v)) = self.doc.find_mut(id).map(|l| &mut l.kind) {
                        if vector::remove_control(&mut v.strokes[k].points, &controls, c) {
                            self.redraw_vector(id);
                        }
                    }
                    return;
                }
                let original = points.to_vec();
                self.vector_edit.drag = Some(EditDrag { control: Some(c), controls, start: at, original, recorded: false });
                self.drag = Some(ToolDrag { start: at, current: at, points: Vec::new(), op: SelectionOp::Replace });
                return;
            }
        }
        match vector::stroke_at(layer, [at.x, at.y], reach) {
            Some((k, _)) => {
                let original = layer.strokes[k].points.clone();
                self.vector_edit.picked = Some((id, k));
                self.vector_edit.drag = Some(EditDrag { control: None, controls: Vec::new(), start: at, original, recorded: false });
                self.drag = Some(ToolDrag { start: at, current: at, points: Vec::new(), op: SelectionOp::Replace });
            }
            None => self.vector_edit.picked = None,
        }
    }

    pub(crate) fn vector_edit_move(&mut self, at: Pos2) {
        let (Some(drag), Some((id, k))) = (self.vector_edit.drag.clone(), self.vector_edit.picked) else { return };
        let delta = [at.x - drag.start.x, at.y - drag.start.y];
        if !drag.recorded {
            if delta[0].hypot(delta[1]) < 1.0 / self.view.zoom.max(1e-3) {
                return;
            }
            self.checkpoint();
            if let Some(d) = &mut self.vector_edit.drag {
                d.recorded = true;
            }
        }
        if let Some(LayerKind::Vector(v)) = self.doc.find_mut(id).map(|l| &mut l.kind) {
            let Some(stroke) = v.strokes.get_mut(k) else { return };
            stroke.points.clone_from(&drag.original);
            match drag.control {
                Some(c) => vector::move_control(&mut stroke.points, &drag.controls, c, delta),
                None => stroke.points.iter_mut().for_each(|p| {
                    p.x += delta[0];
                    p.y += delta[1];
                }),
            }
        }
    }

    pub(crate) fn vector_edit_release(&mut self) {
        let Some(drag) = self.vector_edit.drag.take() else { return };
        let Some((id, k)) = self.vector_edit.picked else { return };
        // A click without a drag only picked the line: no change to keep.
        let moved = self.doc.find(id).and_then(|l| match &l.kind {
            LayerKind::Vector(v) => v.strokes.get(k).map(|s| s.points != drag.original),
            _ => None,
        });
        if drag.recorded && moved == Some(true) {
            self.redraw_vector(id);
        }
    }

    /// Draws a vector layer again from its lines.
    fn redraw_vector(&mut self, id: LayerId) {
        let canvas = self.doc.canvas();
        if let Some(LayerKind::Vector(v)) = self.doc.find_mut(id).map(|l| &mut l.kind) {
            vector::rasterize(v, canvas);
        }
        self.upload_plane(Plane::Color(id));
        self.dirty = true;
    }

    /// Delete with a line picked.
    pub(crate) fn vector_edit_delete(&mut self) -> bool {
        let Some((id, k)) = self.vector_edit.picked else { return false };
        if self.tool != crate::tools::Tool::VectorEdit || self.vector_layer().map(|v| v.0) != Some(id) {
            return false;
        }
        self.checkpoint();
        if let Some(LayerKind::Vector(v)) = self.doc.find_mut(id).map(|l| &mut l.kind) {
            if k < v.strokes.len() {
                v.strokes.remove(k);
            }
        }
        self.vector_edit.picked = None;
        self.redraw_vector(id);
        true
    }

    /// The picked line and its control points over the canvas.
    pub(crate) fn draw_vector_edit(&self, painter: &Painter, rect: Rect) {
        if self.tool != crate::tools::Tool::VectorEdit {
            return;
        }
        let doc = self.doc_size();
        let screen = |p: &StrokePoint| self.view.doc_to_screen(rect, doc, Pos2::new(p.x, p.y));
        // The line under the pointer, faintly.
        if let (Some((_, layer)), Some(hover)) = (self.vector_layer(), self.hover_doc) {
            if let Some((k, _)) = vector::stroke_at(layer, [hover.x, hover.y], REACH / self.view.zoom.max(1e-3)) {
                if self.vector_edit.picked.map(|p| p.1) != Some(k) {
                    let line: Vec<Pos2> = layer.strokes[k].points.iter().map(screen).collect();
                    painter.add(egui::Shape::line(line, Stroke::new(1.0_f32, crate::theme::accent().gamma_multiply(0.6))));
                }
            }
        }
        let Some((points, controls)) = self.picked_line() else { return };
        let line: Vec<Pos2> = points.iter().map(screen).collect();
        painter.add(egui::Shape::line(line.clone(), Stroke::new(3.0_f32, Color32::from_black_alpha(120))));
        painter.add(egui::Shape::line(line, Stroke::new(1.5_f32, crate::theme::accent())));
        let dragged = self.vector_edit.drag.as_ref().and_then(|d| d.control);
        for (c, &i) in controls.iter().enumerate() {
            let at = screen(&points[i]);
            let r = if dragged == Some(c) { 5.5 } else { 4.0 };
            painter.rect_filled(Rect::from_center_size(at, egui::Vec2::splat(r * 2.0)), 1.0, Color32::WHITE);
            painter.rect_stroke(
                Rect::from_center_size(at, egui::Vec2::splat(r * 2.0)),
                1.0,
                Stroke::new(1.5_f32, crate::theme::accent()),
                egui::StrokeKind::Middle,
            );
        }
    }

    /// Tool Settings for Vector Points.
    pub(crate) fn vector_edit_panel(&mut self, ui: &mut egui::Ui) {
        ui.label(tr("Click a vector line to pick it. Drag its square points to reshape it, drag the line to move it. Alt + click removes a point; Delete removes the line."));
        let Some((id, k)) = self.vector_edit.picked else { return };
        let Some(LayerKind::Vector(v)) = self.doc.find(id).map(|l| &l.kind) else { return };
        let Some(stroke) = v.strokes.get(k) else { return };
        let (mut size, mut color, count) = (stroke.settings.size, stroke.color, stroke.points.len());
        ui.separator();
        ui.label(format!("Line {} of {} · {count} points", k + 1, v.strokes.len()));
        let (mut changed, mut undo) = (false, false);
        let r = ui.add(egui::Slider::new(&mut size, 0.5..=200.0).logarithmic(true).text(tr("Width")));
        changed |= r.changed();
        undo |= r.drag_started() || (r.changed() && !r.dragged());
        let r = ui.color_edit_button_rgb(&mut color);
        changed |= r.changed();
        undo |= r.changed();
        let (mut simplify, mut delete) = (false, false);
        ui.horizontal(|ui| {
            simplify =
                ui.button(tr("Simplify")).on_hover_text(tr("Keep only the points that make its shape (smoother, easier to edit)")).clicked();
            delete = ui.button(tr("Delete line")).clicked();
        });
        if delete {
            self.vector_edit_delete();
            return;
        }
        if undo || simplify {
            self.checkpoint();
        }
        if changed || simplify {
            let detail = DETAIL / self.view.zoom.max(1e-3);
            if let Some(LayerKind::Vector(v)) = self.doc.find_mut(id).map(|l| &mut l.kind) {
                if let Some(s) = v.strokes.get_mut(k) {
                    s.settings.size = size;
                    s.color = color;
                    if simplify {
                        let keep = vector::control_points(s, detail);
                        // Points along the kept outline at most ~4 px apart, so the brush stays smooth.
                        let mut out = Vec::new();
                        for w in keep.windows(2) {
                            let (a, b) = (s.points[w[0]], s.points[w[1]]);
                            let n = ((b.x - a.x).hypot(b.y - a.y) / 4.0).ceil().max(1.0) as usize;
                            for j in 0..n {
                                let t = j as f32 / n as f32;
                                out.push(StrokePoint {
                                    x: a.x + (b.x - a.x) * t,
                                    y: a.y + (b.y - a.y) * t,
                                    pressure: a.pressure + (b.pressure - a.pressure) * t,
                                    ..a
                                });
                            }
                        }
                        out.extend(keep.last().map(|&i| s.points[i]));
                        s.points = out;
                    }
                }
            }
            self.redraw_vector(id);
        }
    }
}
