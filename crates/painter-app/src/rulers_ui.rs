//! Rulers in the app: drawing them (and the grid) over the canvas, the
//! Ruler tool that places and edits them, its settings panel, and what they
//! do to strokes (snapping, symmetric copies). The geometry is in
//! `painter_core::rulers`.

use eframe::egui::{self, Color32, Key, Modifiers, Painter, Pos2, Rect, Stroke};
use painter_core::rulers::{symmetry_copies, Copy2D, Ruler, RulerKind, RulerSet, StrokeSnap, P};
use painter_core::Dab;

use crate::app::PainterApp;
use crate::tools::{Tool, ToolDrag};

/// Screen points within which a handle or guide is grabbed.
const GRAB: f32 = 9.0;
/// How far a stroke goes (screen points) before its guide is chosen.
pub(crate) const DECIDE: f32 = 7.0;
/// How near (screen points) a stroke has to start to catch a straight ruler or guide.
const CATCH: f32 = 14.0;

const ACTIVE: Color32 = Color32::from_rgb(176, 96, 255);

/// What the Ruler tool is dragging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RulerDrag {
    Handle { ruler: usize, handle: usize },
    Guide { ruler: usize },
}

/// Snapping and symmetry for one stroke.
pub(crate) struct StrokeRulers {
    pub(crate) snap: Option<StrokeSnap>,
    pub(crate) copies: Vec<Copy2D>,
    pub(crate) tip_angle: f32,
}

impl StrokeRulers {
    /// The dabs plus their symmetric copies.
    pub(crate) fn replicate(&self, dabs: Vec<Dab>) -> Vec<Dab> {
        if self.copies.is_empty() || dabs.is_empty() {
            return dabs;
        }
        let mut out = Vec::with_capacity(dabs.len() * (self.copies.len() + 1));
        out.extend_from_slice(&dabs);
        for c in &self.copies {
            out.extend(dabs.iter().map(|d| c.dab(d, self.tip_angle)));
        }
        out
    }
}

fn pos(p: P) -> Pos2 {
    Pos2::new(p[0], p[1])
}

impl PainterApp {
    /// Snapping and symmetry for a stroke starting at `p` (canvas pixels).
    pub(crate) fn stroke_rulers(&self, p: P) -> StrokeRulers {
        // Only the rulers of the layer being painted (or shared with it).
        let set = self.doc.rulers_in_effect();
        let zoom = self.view.zoom.max(1e-3);
        StrokeRulers {
            snap: StrokeSnap::new(&set, p, CATCH / zoom),
            copies: symmetry_copies(&set),
            tip_angle: self.current_brush().tip.angle,
        }
    }

    /// Ctrl+1 snapping, Ctrl+' grid (Clip Studio's keys).
    pub(crate) fn ruler_shortcuts(&mut self, ctx: &egui::Context) {
        let (snap, grid) =
            ctx.input_mut(|i| (i.consume_key(Modifiers::COMMAND, Key::Num1), i.consume_key(Modifiers::COMMAND, Key::Quote)));
        if snap {
            self.toggle_ruler_snapping();
        }
        if grid {
            self.doc.rulers.grid.visible = !self.doc.rulers.grid.visible;
        }
    }

    pub(crate) fn toggle_ruler_snapping(&mut self) {
        let set = &mut self.doc.rulers;
        set.snapping = !set.snapping;
        self.status = if set.snapping { "Snapping to rulers on (Ctrl+1)".into() } else { "Snapping to rulers off (Ctrl+1)".into() };
    }

    // ---- overlay ------------------------------------------------------------------------

    /// The grid and the visible rulers, over the canvas.
    pub(crate) fn draw_rulers_overlay(&self, painter: &Painter, rect: Rect) {
        let set = &self.doc.rulers;
        let doc = self.doc_size();
        let (w, h) = (doc.x, doc.y);
        let to_screen = |p: P| self.view.doc_to_screen(rect, doc, pos(p));
        let line = |a: P, b: P, color: Color32, width: f32| {
            painter.line_segment([to_screen(a), to_screen(b)], Stroke::new(width, color));
        };

        // Grid, with subdivisions when they're far enough apart on screen.
        let g = set.grid;
        if g.visible && g.spacing >= 1.0 {
            let major = Color32::from_rgba_unmultiplied(80, 150, 255, 110);
            let minor = Color32::from_rgba_unmultiplied(80, 150, 255, 45);
            let sub = g.subdivisions.max(1) as f32;
            let step = if g.spacing / sub * self.view.zoom >= 6.0 { g.spacing / sub } else { g.spacing };
            if step * self.view.zoom >= 3.0 {
                let mut k = ((0.0 - g.origin[0]) / step).ceil();
                while g.origin[0] + k * step <= w {
                    let x = g.origin[0] + k * step;
                    let is_major = (k * step / g.spacing).fract().abs() < 1e-3;
                    line([x, 0.0], [x, h], if is_major { major } else { minor }, 1.0);
                    k += 1.0;
                }
                let mut k = ((0.0 - g.origin[1]) / step).ceil();
                while g.origin[1] + k * step <= h {
                    let y = g.origin[1] + k * step;
                    let is_major = (k * step / g.spacing).fract().abs() < 1e-3;
                    line([0.0, y], [w, y], if is_major { major } else { minor }, 1.0);
                    k += 1.0;
                }
            }
        }

        let reach = (w * w + h * h).sqrt() * 1.5;
        let editing = self.tool == Tool::Ruler;
        for (i, r) in set.rulers.iter().enumerate() {
            if !r.visible || !self.doc.ruler_applies(r) {
                continue;
            }
            let on = set.snapping && r.snap;
            let selected = editing && self.selected_ruler == Some(i);
            let color = if on { ACTIVE } else { Color32::from_rgba_unmultiplied(150, 150, 150, 150) };
            let color = if selected { color } else { color.gamma_multiply(0.75) };
            match &r.kind {
                RulerKind::Perspective { vanishing } => {
                    // A fan of lines from each vanishing point across the canvas.
                    for v in vanishing {
                        for k in 0..24 {
                            let t = k as f32 / 24.0;
                            // Targets around the canvas border.
                            let target = border_point(t, w, h);
                            let d = [target[0] - v[0], target[1] - v[1]];
                            let l = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1e-3);
                            line(*v, [v[0] + d[0] / l * reach, v[1] + d[1] / l * reach], color.gamma_multiply(0.5), 1.0);
                        }
                    }
                    // The horizon (eye level).
                    let (a, b) = match vanishing.as_slice() {
                        [v] => ([v[0] - reach, v[1]], [v[0] + reach, v[1]]),
                        [a, b, ..] => {
                            let d = [b[0] - a[0], b[1] - a[1]];
                            let l = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1e-3);
                            ([a[0] - d[0] / l * reach, a[1] - d[1] / l * reach], [b[0] + d[0] / l * reach, b[1] + d[1] / l * reach])
                        }
                        [] => continue,
                    };
                    line(a, b, color, 1.5);
                }
                RulerKind::Symmetry { origin, angle, lines, mirror } => {
                    let n = (*lines).max(1);
                    for k in 0..n {
                        let a = if *mirror { angle + 180.0 * k as f32 / n as f32 } else { angle + 360.0 * k as f32 / n as f32 };
                        let (s, c) = a.to_radians().sin_cos();
                        let start = if *mirror { [origin[0] - c * reach, origin[1] - s * reach] } else { *origin };
                        line(start, [origin[0] + c * reach, origin[1] + s * reach], color, 1.5);
                    }
                }
                RulerKind::ParallelLines { angle } => {
                    let (s, c) = angle.to_radians().sin_cos();
                    let gap = w.min(h) / 8.0;
                    for k in -6..=6 {
                        let o = [w * 0.5 - s * gap * k as f32, h * 0.5 + c * gap * k as f32];
                        line(
                            [o[0] - c * reach, o[1] - s * reach],
                            [o[0] + c * reach, o[1] + s * reach],
                            color.gamma_multiply(0.6),
                            1.0,
                        );
                    }
                }
                RulerKind::Concentric { center, ratio, angle } => {
                    let (s, c) = angle.to_radians().sin_cos();
                    for k in 1..=6 {
                        let radius = w.min(h) * 0.08 * k as f32;
                        let points: Vec<Pos2> = (0..=72)
                            .map(|j| {
                                let t = j as f32 / 72.0 * std::f32::consts::TAU;
                                let (u, v) = (t.cos() * radius, t.sin() * radius * ratio);
                                to_screen([center[0] + u * c - v * s, center[1] + u * s + v * c])
                            })
                            .collect();
                        painter.add(egui::Shape::line(points, Stroke::new(1.0_f32, color.gamma_multiply(0.7))));
                    }
                }
                RulerKind::RadialLines { center } => {
                    for k in 0..32 {
                        let (s, c) = (k as f32 / 32.0 * std::f32::consts::TAU).sin_cos();
                        line(*center, [center[0] + c * reach, center[1] + s * reach], color.gamma_multiply(0.6), 1.0);
                    }
                }
                RulerKind::Curvilinear { center, radius_x, radius_y, angle, fifth } => {
                    let (s, c) = angle.to_radians().sin_cos();
                    let (u, v) = ([c, s], [-s, c]);
                    let at = |a: f32, b: f32| [center[0] + u[0] * a + v[0] * b, center[1] + u[1] * a + v[1] * b];
                    // An arc family: circles through the pair of points on `axis`,
                    // crossing the other axis at evenly spaced places.
                    let family = |axis: P, reach: f32, spread: f32, horizontal: bool| {
                        for k in -7..=7 {
                            let through = if horizontal { at(0.0, spread * k as f32 / 7.0) } else { at(spread * k as f32 / 7.0, 0.0) };
                            let (width, tint) = if k == 0 { (1.5, color) } else { (1.0, color.gamma_multiply(0.55)) };
                            match painter_core::rulers::arc_through(*center, axis, reach, through) {
                                painter_core::rulers::Guide::Ellipse { center: cc, radius, .. } => {
                                    // Enough segments to stay smooth on screen.
                                    let n = ((radius * self.view.zoom) / 6.0).clamp(96.0, 4000.0) as usize;
                                    let points: Vec<Pos2> = (0..=n)
                                        .map(|j| {
                                            let t = j as f32 / n as f32 * std::f32::consts::TAU;
                                            to_screen([cc[0] + t.cos() * radius, cc[1] + t.sin() * radius])
                                        })
                                        .collect();
                                    painter.add(egui::Shape::line(points, Stroke::new(width, tint)));
                                }
                                painter_core::rulers::Guide::Line { origin, dir } => line(
                                    [origin[0] - dir[0] * reach * 3.0, origin[1] - dir[1] * reach * 3.0],
                                    [origin[0] + dir[0] * reach * 3.0, origin[1] + dir[1] * reach * 3.0],
                                    tint,
                                    width,
                                ),
                            }
                        }
                    };
                    family(u, *radius_x, *radius_y * 1.4, true);
                    family(v, *radius_y, *radius_x * 1.4, false);
                    if *fifth {
                        for k in 0..24 {
                            let (ks, kc) = (k as f32 / 24.0 * std::f32::consts::TAU).sin_cos();
                            line(*center, [center[0] + kc * reach, center[1] + ks * reach], color.gamma_multiply(0.45), 1.0);
                        }
                    }
                    // The four vanishing points.
                    for p in painter_core::rulers::curvilinear_points(*center, *radius_x, *radius_y, *angle) {
                        painter.circle_stroke(to_screen(p), 5.0, Stroke::new(1.5_f32, color));
                    }
                }
                RulerKind::Line { a, b } => line(*a, *b, color, 2.0),
                RulerKind::Guide { vertical, position } => {
                    if *vertical {
                        line([*position, -reach], [*position, reach], color, 1.0);
                    } else {
                        line([-reach, *position], [reach, *position], color, 1.0);
                    }
                }
            }
            if editing {
                for (k, hp) in r.handles().iter().enumerate() {
                    let s = to_screen(*hp);
                    let radius = if selected { 6.0 } else { 4.5 };
                    painter.circle_filled(s, radius, Color32::WHITE);
                    painter.circle_stroke(s, radius, Stroke::new(1.5_f32, color));
                    if matches!(r.kind, RulerKind::Symmetry { .. }) && k == 1 {
                        line(r.handles()[0], *hp, color, 1.0);
                    }
                }
            }
        }
    }

    // ---- the Ruler tool -----------------------------------------------------------------

    fn grab_ruler(&self, at: Pos2) -> Option<RulerDrag> {
        let zoom = self.view.zoom.max(1e-3);
        let grab = GRAB / zoom;
        let p = [at.x, at.y];
        let dist = |a: P| ((a[0] - p[0]).powi(2) + (a[1] - p[1]).powi(2)).sqrt();
        let mut best: Option<(f32, RulerDrag)> = None;
        for (i, r) in self.doc.rulers.rulers.iter().enumerate().filter(|(_, r)| r.visible && self.doc.ruler_applies(r)) {
            for (k, h) in r.handles().iter().enumerate() {
                let d = dist(*h);
                if d <= grab && best.is_none_or(|(b, _)| d < b) {
                    best = Some((d, RulerDrag::Handle { ruler: i, handle: k }));
                }
            }
            if let RulerKind::Guide { vertical, position } = r.kind {
                let d = if vertical { (p[0] - position).abs() } else { (p[1] - position).abs() };
                if d <= grab * 0.7 && best.is_none_or(|(b, _)| d < b) {
                    best = Some((d, RulerDrag::Guide { ruler: i }));
                }
            }
        }
        best.map(|(_, d)| d)
    }

    pub(crate) fn ruler_press(&mut self, at: Pos2) {
        match self.grab_ruler(at) {
            Some(grab) => {
                let (RulerDrag::Handle { ruler, .. } | RulerDrag::Guide { ruler }) = grab;
                self.selected_ruler = Some(ruler);
                self.ruler_drag = Some(grab);
                self.drag = Some(ToolDrag { start: at, current: at, points: Vec::new(), op: self.tools.selection_op });
            }
            None => self.selected_ruler = None,
        }
    }

    pub(crate) fn ruler_move(&mut self, at: Pos2) {
        let Some(grab) = self.ruler_drag else { return };
        match grab {
            RulerDrag::Handle { ruler, handle } => {
                if let Some(r) = self.doc.rulers.rulers.get_mut(ruler) {
                    r.drag_handle(handle, [at.x, at.y]);
                }
            }
            RulerDrag::Guide { ruler } => {
                if let Some(Ruler { kind: RulerKind::Guide { vertical, position }, .. }) = self.doc.rulers.rulers.get_mut(ruler) {
                    *position = if *vertical { at.x.round() } else { at.y.round() };
                }
            }
        }
        self.dirty = true;
    }

    pub(crate) fn ruler_release(&mut self) {
        self.ruler_drag = None;
    }

    /// Adds a ruler where the view is centred and selects it.
    fn add_ruler(&mut self, kind: RulerKind) {
        // It belongs to the layer being edited, like in Clip Studio.
        self.doc.rulers.rulers.push(Ruler::on_layer(kind, self.doc.active.0));
        self.selected_ruler = Some(self.doc.rulers.rulers.len() - 1);
        self.doc.rulers.snapping = true;
        self.tool = Tool::Ruler;
        self.dirty = true;
    }

    fn view_center(&self) -> P {
        let c = self.view.screen_to_doc(self.canvas_rect, self.doc_size(), self.canvas_rect.center());
        [c.x.round(), c.y.round()]
    }

    /// The Ruler tool's settings.
    pub(crate) fn ruler_panel(&mut self, ui: &mut egui::Ui) {
        let (w, h) = (self.doc.width as f32, self.doc.height as f32);
        let center = self.view_center();
        let mut snapping = self.doc.rulers.snapping;
        if ui.checkbox(&mut snapping, "Snap to rulers  (Ctrl+1)").changed() {
            self.toggle_ruler_snapping();
        }
        let mut add: Option<RulerKind> = None;
        ui.horizontal_wrapped(|ui| {
            ui.label("Add");
            ui.menu_button("Perspective", |ui| {
                for (n, label) in [(1, "1-point"), (2, "2-point"), (3, "3-point")] {
                    if ui.button(label).clicked() {
                        add = Some(RulerSet::perspective(n, w, h).kind);
                        ui.close_menu();
                    }
                }
                ui.separator();
                for (n, label, tip) in [
                    (4, "4-point (curvilinear)", "Lines bend into arcs through left / right and top / bottom vanishing points"),
                    (5, "5-point (fisheye)", "Four points on a circle plus straight lines out of the centre"),
                ] {
                    if ui.button(label).on_hover_text(tip).clicked() {
                        add = Some(RulerSet::curvilinear(n, w, h).kind);
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("Symmetry", |ui| {
                for (label, lines, mirror) in
                    [("Mirror (left / right)", 1, true), ("Rotational ×6", 6, false), ("Kaleidoscope ×6", 6, true)]
                {
                    if ui.button(label).clicked() {
                        add = Some(RulerKind::Symmetry { origin: center, angle: 90.0, lines, mirror });
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("Special", |ui| {
                let items: [(&str, RulerKind); 4] = [
                    ("Parallel lines", RulerKind::ParallelLines { angle: 0.0 }),
                    ("Concentric circles", RulerKind::Concentric { center, ratio: 1.0, angle: 0.0 }),
                    ("Radial lines", RulerKind::RadialLines { center }),
                    ("Straight ruler", RulerKind::Line { a: [center[0] - w * 0.2, center[1]], b: [center[0] + w * 0.2, center[1]] }),
                ];
                for (label, kind) in items {
                    if ui.button(label).clicked() {
                        add = Some(kind);
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("Guide", |ui| {
                if ui.button("Horizontal").clicked() {
                    add = Some(RulerKind::Guide { vertical: false, position: center[1] });
                    ui.close_menu();
                }
                if ui.button("Vertical").clicked() {
                    add = Some(RulerKind::Guide { vertical: true, position: center[0] });
                    ui.close_menu();
                }
            });
        });
        if let Some(kind) = add {
            self.add_ruler(kind);
        }
        ui.separator();
        if self.doc.rulers.rulers.is_empty() {
            ui.weak("No rulers yet. Add one above, then drag its handles on the canvas.");
        }
        let mut remove = None;
        let mut changed = false;
        // Every ruler, with the layer it belongs to; those not acting on the
        // edited layer are dimmed.
        let owners: Vec<(String, bool)> = self
            .doc
            .rulers
            .rulers
            .iter()
            .map(|r| {
                let owner = match r.layer {
                    None => "whole document".to_string(),
                    Some(id) => self.doc.find(painter_core::LayerId(id)).map_or("deleted layer".into(), |l| l.name.clone()),
                };
                (owner, self.doc.ruler_applies(r))
            })
            .collect();
        for (i, r) in self.doc.rulers.rulers.iter_mut().enumerate() {
            let (owner, applies) = &owners[i];
            ui.horizontal(|ui| {
                changed |= ui.checkbox(&mut r.visible, "").on_hover_text("Show").changed();
                changed |= ui.checkbox(&mut r.snap, "").on_hover_text("Snap strokes to it").changed();
                let text = egui::RichText::new(format!("{}  ·  {owner}", r.label()));
                let text = if *applies { text } else { text.weak() };
                let row = ui.selectable_label(self.selected_ruler == Some(i), text);
                let row = if *applies { row } else { row.on_hover_text("Not active on the layer being edited") };
                if row.clicked() {
                    self.selected_ruler = Some(i);
                }
                if ui.small_button("x").on_hover_text("Delete").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            self.doc.rulers.rulers.remove(i);
            self.selected_ruler = None;
            changed = true;
        }
        // The selected ruler's settings.
        let active = self.doc.active;
        let active_name = self.doc.active_layer().map_or(String::new(), |l| l.name.clone());
        if let Some(r) = self.selected_ruler.and_then(|i| self.doc.rulers.rulers.get_mut(i)) {
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Applies to");
                ui.add_enabled_ui(r.layer.is_some(), |ui| {
                    egui::ComboBox::from_id_salt("ruler_scope").selected_text(r.scope.label()).show_ui(ui, |ui| {
                        for scope in painter_core::RulerScope::ALL {
                            changed |= ui.selectable_value(&mut r.scope, scope, scope.label()).changed();
                        }
                    });
                });
            });
            if r.layer != Some(active.0) && ui.button(format!("Move to \"{active_name}\"")).on_hover_text("Give the ruler to the layer being edited").clicked() {
                r.layer = Some(active.0);
                changed = true;
            }
            match &mut r.kind {
                RulerKind::Perspective { vanishing } => {
                    ui.horizontal(|ui| {
                        ui.label(format!("{} vanishing point(s)", vanishing.len()));
                        if ui.add_enabled(vanishing.len() < 3, egui::Button::new("+")).clicked() {
                            vanishing.push([w * 0.5, h * 2.6]);
                            changed = true;
                        }
                        if ui.add_enabled(vanishing.len() > 1, egui::Button::new("-")).clicked() {
                            vanishing.pop();
                            changed = true;
                        }
                    });
                    ui.small("Drag the points on the canvas. Strokes follow the line towards the point they head for; one and two points also give vertical (and horizontal) lines.");
                }
                RulerKind::Symmetry { angle, lines, mirror, .. } => {
                    changed |= ui.add(egui::Slider::new(lines, 1..=32).text("Lines")).changed();
                    changed |= ui.checkbox(mirror, "Mirror").changed();
                    changed |= ui.add(egui::DragValue::new(angle).speed(0.5).suffix("°").prefix("Angle ")).changed();
                    ui.small("Drag the centre, and the second handle to turn the axis.");
                }
                RulerKind::ParallelLines { angle } => {
                    changed |= ui.add(egui::DragValue::new(angle).speed(0.5).suffix("°").prefix("Angle ")).changed();
                }
                RulerKind::Concentric { ratio, angle, .. } => {
                    changed |= ui.add(egui::Slider::new(ratio, 0.05..=1.0).text("Ellipse ratio")).changed();
                    changed |= ui.add(egui::DragValue::new(angle).speed(0.5).suffix("°").prefix("Angle ")).changed();
                }
                RulerKind::Guide { position, .. } => {
                    changed |= ui.add(egui::DragValue::new(position).speed(1.0).suffix(" px").prefix("Position ")).changed();
                }
                RulerKind::Curvilinear { radius_x, radius_y, angle, fifth, .. } => {
                    changed |= ui.checkbox(fifth, "Fifth point (fisheye)").on_hover_text("Lines also run straight out of the centre").changed();
                    ui.horizontal(|ui| {
                        changed |= ui.add(egui::DragValue::new(radius_x).speed(2.0).range(1.0..=1e6).prefix("Left / right ").suffix(" px")).changed();
                        changed |= ui.add(egui::DragValue::new(radius_y).speed(2.0).range(1.0..=1e6).prefix("Up / down ").suffix(" px")).changed();
                    });
                    changed |= ui.add(egui::DragValue::new(angle).speed(0.5).suffix("°").prefix("Horizon angle ")).changed();
                    ui.small("Drag the centre, the right point (turns the horizon) and the lower point. Strokes bend into arcs through the pair of points they head for.");
                }
                RulerKind::RadialLines { .. } | RulerKind::Line { .. } => {
                    ui.small("Drag its handles on the canvas.");
                }
            }
        }
        ui.separator();
        let grid = &mut self.doc.rulers.grid;
        changed |= ui.checkbox(&mut grid.visible, "Show grid  (Ctrl+')").changed();
        ui.add_enabled_ui(grid.visible, |ui| {
            changed |= ui
                .add(egui::DragValue::new(&mut grid.spacing).range(4.0..=4000.0).speed(1.0).suffix(" px").prefix("Spacing "))
                .changed();
            changed |= ui.add(egui::Slider::new(&mut grid.subdivisions, 1..=16).text("Subdivisions")).changed();
        });
        if changed {
            self.dirty = true;
        }
    }
}

/// A point around the canvas border, `t` in 0..1 going clockwise from the top-left.
fn border_point(t: f32, w: f32, h: f32) -> P {
    let perimeter = 2.0 * (w + h);
    let d = t * perimeter;
    if d < w {
        [d, 0.0]
    } else if d < w + h {
        [w, d - w]
    } else if d < 2.0 * w + h {
        [w - (d - w - h), h]
    } else {
        [0.0, h - (d - 2.0 * w - h)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn border_walks_around() {
        assert_eq!(border_point(0.0, 100.0, 50.0), [0.0, 0.0]);
        assert_eq!(border_point(0.5, 100.0, 50.0), [100.0, 50.0]);
        assert_eq!(border_point(0.25, 100.0, 50.0), [75.0, 0.0]);
    }

    #[test]
    fn replicate_adds_one_set_per_copy() {
        let mut set = RulerSet::default();
        set.rulers.push(Ruler::new(RulerKind::Symmetry { origin: [50.0, 0.0], angle: 90.0, lines: 1, mirror: true }));
        let r = StrokeRulers { snap: None, copies: symmetry_copies(&set), tip_angle: 0.0 };
        let dab = Dab { x: 40.0, y: 5.0, radius: 2.0, alpha: 1.0, angle: 0.0, thickness: 1.0, mirror: false };
        let out = r.replicate(vec![dab]);
        assert_eq!(out.len(), 2);
        assert!((out[1].x - 60.0).abs() < 1e-3 && out[1].mirror);
    }
}
