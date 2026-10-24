//! Comic pages in the app: the New Comic Page wizard, page guides (trim,
//! inner frame, crop marks, webtoon slices), the Frame tool (rectangle,
//! polygon, divide, edit, with snapping) and the Panels section of the
//! Layers panel. The page and its panels live in `comic_layout::ComicPage`;
//! undo keeps a copy of it next to every document snapshot.

use comic_layout::frame_tool::{drag_handle, hit_test};
use crate::i18n::tr;
use comic_layout::paper::{STANDARD_BLEED_MM, STANDARD_SAFE_MM};
use comic_layout::{
    Binding, BorderStyle, Category, ComicPage, Cut, FrameHandle, FrameMode, GutterMetrics, Orientation, PageGeometry, PagePreset,
    Panel, PanelId, PaperSize, SnapTargets, Template, Unit,
};
use eframe::egui::{self, Align2, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use painter_core::{Document, SelectionOp, MAX_CANVAS_SIZE};
use painter_render::Plane;

use crate::app::{PainterApp, UNDO_LIMIT};
use crate::icons::{icon_button, Icon};
use crate::print_ui::{dpi_edit, length_edit, orientation_buttons, paper_combo, unit_combo};
use crate::theme;
use crate::tools::{Tool, ToolDrag};

type Point = [f64; 2];

/// Snap and grab radius in screen pixels.
const SNAP: f32 = 8.0;

pub(crate) struct FrameTool {
    pub(crate) mode: FrameMode,
    pub(crate) snap: bool,
    /// Border width for new panels, in pixels.
    pub(crate) border_width: f64,
    /// Corners of the polygon being drawn.
    polygon: Vec<Point>,
    drag: Option<FrameDrag>,
}

impl Default for FrameTool {
    fn default() -> Self {
        Self { mode: FrameMode::Rectangle, snap: true, border_width: 6.0, polygon: Vec::new(), drag: None }
    }
}

impl FrameTool {
    /// Drops a half-drawn polygon or drag (tool switched away).
    pub(crate) fn cancel(&mut self) {
        self.polygon.clear();
        self.drag = None;
    }
}

struct FrameDrag {
    start: Point,
    current: Point,
    /// Edit mode: the grabbed handle, the panel as it was, and the page before the edit (for undo).
    edit: Option<(FrameHandle, Panel, ComicPage)>,
}

fn mode_icon(mode: FrameMode) -> Icon {
    match mode {
        FrameMode::Rectangle => Icon::RectSelect,
        FrameMode::Polygon => Icon::FreeSelect,
        FrameMode::Divide => Icon::Grid,
        FrameMode::Edit => Icon::Path,
    }
}

impl PainterApp {
    fn snap_radius(&self) -> f64 {
        (SNAP / self.view.zoom.max(1e-3)) as f64
    }

    /// The comic page, created over the whole canvas on first use.
    fn ensure_comic(&mut self) -> &mut ComicPage {
        let (w, h) = self.doc.canvas();
        self.comic.get_or_insert_with(|| {
            let full = [0.0, 0.0, w as f64, h as f64];
            let geometry =
                PageGeometry { canvas: (w, h), bleed: full, trim: full, inner: full, binding: Binding::LeftToRight, webtoon: None };
            ComicPage::new(geometry, GutterMetrics::uniform(24.0))
        })
    }

    fn snapped(&self, p: Point, skip: Option<PanelId>) -> Point {
        let Some(comic) = &self.comic else { return p };
        if !self.frame.snap {
            return p;
        }
        let g = &comic.geometry;
        let targets = SnapTargets::collect(&comic.panels, skip, &[g.bleed, g.trim, g.inner], &comic.gutter);
        targets.snap(p, self.snap_radius())
    }

    /// Records the comic page (and the 3D scenes as they are) for undo,
    /// next to a document snapshot the caller pushed.
    pub(crate) fn push_comic_history(&mut self, page: Option<ComicPage>) {
        let scenes = crate::scene_ui::snapshot(&self.scenes);
        self.push_side_history(page, scenes);
    }

    /// App state kept beside each document snapshot: the comic page and the 3D scenes.
    pub(crate) fn push_side_history(&mut self, page: Option<ComicPage>, scenes: crate::scene_ui::SceneSnapshot) {
        self.comic_undo.push(page);
        self.scene_undo.push(scenes);
        if self.comic_undo.len() > UNDO_LIMIT {
            self.comic_undo.remove(0);
            self.scene_undo.remove(0);
        }
        self.comic_redo.clear();
        self.scene_redo.clear();
    }

    fn add_frame_panel(&mut self, shape: Vec<Point>) {
        if shape.len() < 3 || comic_layout::frame_tool::signed_area(&shape).abs() < 16.0 {
            return;
        }
        // Clockwise on screen, like every other panel.
        let shape = if comic_layout::frame_tool::signed_area(&shape) < 0.0 { shape.into_iter().rev().collect() } else { shape };
        self.ensure_comic();
        self.checkpoint();
        let width = self.frame.border_width;
        let Some(comic) = &mut self.comic else { return };
        let id = comic.next_id();
        let panel = Panel { id, shape, border: BorderStyle { width, ..BorderStyle::default() }, full_bleed: false };
        let layers = comic.add_panel(&mut self.doc, panel);
        self.doc.active = layers.art;
        self.selected_panel = Some(id);
        self.resync_gpu();
        self.status = format!("Added panel {}", id.0);
    }

    // ---- frame tool pointer ---------------------------------------------------------

    pub(crate) fn frame_press(&mut self, at: Pos2) {
        let raw = [at.x as f64, at.y as f64];
        match self.frame.mode {
            FrameMode::Polygon => {
                let p = self.snapped(raw, None);
                let close = self.frame.polygon.first().is_some_and(|f| (f[0] - p[0]).hypot(f[1] - p[1]) <= self.snap_radius());
                if close && self.frame.polygon.len() >= 3 {
                    let shape = std::mem::take(&mut self.frame.polygon);
                    self.add_frame_panel(shape);
                } else {
                    self.frame.polygon.push(p);
                }
                return;
            }
            FrameMode::Edit => {
                let radius = self.snap_radius();
                let Some(comic) = &self.comic else { return };
                let Some(handle) = hit_test(&comic.panels, raw, radius) else {
                    self.selected_panel = None;
                    return;
                };
                let id = match handle {
                    FrameHandle::Vertex(id, _) | FrameHandle::Edge(id, _) | FrameHandle::Body(id) => id,
                };
                let Some(panel) = comic.panel(id).cloned() else {
                    return;
                };
                self.selected_panel = Some(id);
                if let Some(layers) = comic.layers.get(&id) {
                    self.doc.active = layers.art;
                }
                self.frame.drag = Some(FrameDrag { start: raw, current: raw, edit: Some((handle, panel, comic.clone())) });
            }
            FrameMode::Rectangle | FrameMode::Divide => {
                let p = self.snapped(raw, None);
                self.frame.drag = Some(FrameDrag { start: p, current: p, edit: None });
            }
        }
        self.drag = Some(ToolDrag { start: at, current: at, points: Vec::new(), op: SelectionOp::Replace });
    }

    pub(crate) fn frame_move(&mut self, at: Pos2) {
        let raw = [at.x as f64, at.y as f64];
        let mode = self.frame.mode;
        let shift = self.pointer_mods.shift;
        let Some(drag) = &self.frame.drag else { return };
        let start = drag.start;
        match (&drag.edit, mode) {
            (Some((handle, original, _)), _) => {
                let (handle, original) = (*handle, original.clone());
                let delta = [raw[0] - start[0], raw[1] - start[1]];
                // Snap the grabbed corner (or the first corner of what moves).
                let anchor = match handle {
                    FrameHandle::Vertex(_, i) | FrameHandle::Edge(_, i) => original.shape[i],
                    FrameHandle::Body(_) => original.shape[0],
                };
                let target = self.snapped([anchor[0] + delta[0], anchor[1] + delta[1]], Some(original.id));
                let moved = drag_handle(&original, handle, [target[0] - anchor[0], target[1] - anchor[1]]);
                if let Some(comic) = &mut self.comic {
                    if let Some(panel) = comic.panels.iter_mut().find(|p| p.id == original.id) {
                        panel.shape = moved.shape;
                    }
                }
                if let Some(drag) = &mut self.frame.drag {
                    drag.current = raw;
                }
            }
            (None, FrameMode::Divide) => {
                let mut p = raw;
                if shift {
                    // Straight cuts: lock to the dominant axis.
                    if (p[0] - start[0]).abs() > (p[1] - start[1]).abs() {
                        p[1] = start[1];
                    } else {
                        p[0] = start[0];
                    }
                }
                if let Some(drag) = &mut self.frame.drag {
                    drag.current = p;
                }
            }
            (None, _) => {
                let p = self.snapped(raw, None);
                if let Some(drag) = &mut self.frame.drag {
                    drag.current = p;
                }
            }
        }
    }

    pub(crate) fn frame_release(&mut self) {
        let Some(drag) = self.frame.drag.take() else {
            return;
        };
        let (a, b) = (drag.start, drag.current);
        match (drag.edit, self.frame.mode) {
            (Some((_, original, before)), _) => {
                let changed = self.comic.as_ref().and_then(|c| c.panel(original.id)).is_some_and(|p| p.shape != original.shape);
                if !changed {
                    return;
                }
                // The page already holds the new shape; undo goes back to `before`.
                self.history.push(&self.doc);
                self.dirty = true;
                self.push_comic_history(Some(before));
                if let Some(comic) = &self.comic {
                    if let Some(layers) = comic.refresh(&mut self.doc, original.id) {
                        self.upload_plane(Plane::Mask(layers.folder));
                        self.upload_plane(Plane::Color(layers.border));
                    }
                }
            }
            (None, FrameMode::Rectangle) => {
                let (x0, y0, x1, y1) = (a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1]));
                if x1 - x0 >= 4.0 && y1 - y0 >= 4.0 {
                    self.add_frame_panel(vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]);
                }
            }
            (None, FrameMode::Divide) => {
                if (b[0] - a[0]).hypot(b[1] - a[1]) < 4.0 {
                    return;
                }
                let mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
                let Some(id) = self.comic.as_ref().and_then(|c| c.panels.iter().rev().find(|p| p.contains(mid)).map(|p| p.id)) else {
                    self.status = tr("Drag the divide line across a panel").into();
                    return;
                };
                // Check the cut first so a miss leaves no undo step behind.
                let feasible = self.comic.as_ref().is_some_and(|c| {
                    let gap = c.gutter.for_cut([b[0] - a[0], b[1] - a[1]]);
                    c.panel(id).is_some_and(|p| comic_layout::frame_tool::split(p, Cut { a, b }, gap, PanelId(u32::MAX)).is_some())
                });
                if !feasible {
                    self.status = tr("That line doesn't split the panel").into();
                    return;
                }
                self.checkpoint();
                if let Some(comic) = &mut self.comic {
                    comic.split_panel(&mut self.doc, id, Cut { a, b });
                }
                self.resync_gpu();
                self.status = tr("Panel divided").into();
            }
            _ => {}
        }
    }

    pub(crate) fn frame_shortcuts(&mut self, ctx: &egui::Context) {
        let (enter, escape, u) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::U) && !i.modifiers.command && !i.modifiers.alt && !i.modifiers.shift,
            )
        });
        if u {
            self.tool = Tool::Frame;
        }
        if self.tool != Tool::Frame {
            self.frame.polygon.clear();
            return;
        }
        if enter && self.frame.polygon.len() >= 3 {
            let shape = std::mem::take(&mut self.frame.polygon);
            self.add_frame_panel(shape);
        }
        if escape {
            self.frame.polygon.clear();
            self.frame.drag = None;
        }
    }

    // ---- overlays ---------------------------------------------------------------------

    pub(crate) fn draw_comic_overlay(&self, painter: &egui::Painter, rect: Rect) {
        let Some(comic) = &self.comic else { return };
        let doc = self.doc_size();
        let to_screen = |p: Point| self.view.doc_to_screen(rect, doc, Pos2::new(p[0] as f32, p[1] as f32));
        let rect_points = |[x, y, w, h]: [f64; 4]| {
            vec![to_screen([x, y]), to_screen([x + w, y]), to_screen([x + w, y + h]), to_screen([x, y + h]), to_screen([x, y])]
        };
        let g = &comic.geometry;
        // Page guides.
        if g.trim != g.bleed {
            painter.extend(egui::Shape::dashed_line(
                &rect_points(g.trim),
                Stroke::new(1.0_f32, Color32::from_rgb(220, 80, 80)),
                6.0,
                4.0,
            ));
        }
        if g.inner != g.trim {
            painter.add(egui::Shape::line(rect_points(g.inner), Stroke::new(1.0_f32, Color32::from_rgb(70, 140, 230))));
        }
        for (a, b) in g.crop_marks((g.trim[0]).max(8.0) * 0.8) {
            painter.line_segment([to_screen(a), to_screen(b)], Stroke::new(1.0_f32, Color32::from_gray(40)));
        }
        for y in g.slice_markers() {
            let line = [to_screen([0.0, y]), to_screen([g.canvas.0 as f64, y])];
            painter.extend(egui::Shape::dashed_line(&line, Stroke::new(1.0_f32, Color32::from_rgb(230, 60, 160)), 8.0, 6.0));
            painter.text(
                line[0] + Vec2::new(4.0, -2.0),
                Align2::LEFT_BOTTOM,
                format!("slice {:.0}", y),
                egui::FontId::proportional(10.0),
                Color32::from_rgb(230, 60, 160),
            );
        }
        if self.tool != Tool::Frame {
            return;
        }
        // Panel outlines, the selected one highlighted, with its handles in Edit mode.
        let order = comic.reading_order();
        for panel in &comic.panels {
            let mut points: Vec<Pos2> = panel.shape.iter().map(|&p| to_screen(p)).collect();
            points.push(points[0]);
            let selected = self.selected_panel == Some(panel.id);
            let color = if selected { Color32::from_rgb(255, 170, 40) } else { theme::accent() };
            painter.add(egui::Shape::line(points.clone(), Stroke::new(if selected { 2.0_f32 } else { 1.0 }, color)));
            if let Some(k) = order.iter().position(|id| *id == panel.id) {
                let b = panel.bounds();
                painter.text(
                    to_screen([b[0], b[1]]) + Vec2::new(4.0, 4.0),
                    Align2::LEFT_TOP,
                    format!("{}", k + 1),
                    egui::FontId::proportional(13.0),
                    color,
                );
            }
            if self.frame.mode == FrameMode::Edit {
                for &p in &points[..points.len() - 1] {
                    painter.circle_filled(p, if selected { 4.5 } else { 3.0 }, Color32::WHITE);
                    painter.circle_stroke(p, if selected { 4.5 } else { 3.0 }, Stroke::new(1.0_f32, color));
                }
            }
        }
        // What's being drawn.
        let hover = self.hover_doc.map(|p| self.snapped([p.x as f64, p.y as f64], None));
        if !self.frame.polygon.is_empty() {
            let mut points: Vec<Pos2> = self.frame.polygon.iter().map(|&p| to_screen(p)).collect();
            if let Some(h) = hover {
                points.push(to_screen(h));
            }
            painter.add(egui::Shape::line(points.clone(), Stroke::new(1.5_f32, Color32::from_rgb(255, 170, 40))));
            for p in &points {
                painter.circle_filled(*p, 3.0, Color32::WHITE);
            }
        }
        if let Some(drag) = &self.frame.drag {
            let (a, b) = (drag.start, drag.current);
            match (drag.edit.is_some(), self.frame.mode) {
                (false, FrameMode::Rectangle) => {
                    let r = [a[0].min(b[0]), a[1].min(b[1]), (a[0] - b[0]).abs(), (a[1] - b[1]).abs()];
                    painter.add(egui::Shape::line(rect_points(r), Stroke::new(1.5_f32, Color32::from_rgb(255, 170, 40))));
                }
                (false, FrameMode::Divide) => {
                    painter.line_segment([to_screen(a), to_screen(b)], Stroke::new(2.0_f32, Color32::from_rgb(255, 80, 80)));
                }
                _ => {}
            }
        }
        // Snap indicator.
        if let (Some(h), Some(raw)) = (hover, self.hover_doc) {
            if self.frame.snap && ((h[0] - raw.x as f64).abs() > 1e-6 || (h[1] - raw.y as f64).abs() > 1e-6) {
                painter.circle_stroke(to_screen(h), 6.0, Stroke::new(1.5_f32, Color32::from_rgb(255, 170, 40)));
            }
        }
    }

    // ---- tool properties --------------------------------------------------------------

    pub(crate) fn frame_tool_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for mode in FrameMode::ALL {
                if icon_button(ui, mode_icon(mode), 18.0, self.frame.mode == mode, mode.label()).clicked() {
                    self.frame.mode = mode;
                    self.frame.polygon.clear();
                }
            }
            ui.label(tr(self.frame.mode.label()));
        });
        ui.small(tr(self.frame.mode.hint()));
        ui.checkbox(&mut self.frame.snap, tr("Snap to panels, frames and gutters"));
        ui.horizontal(|ui| {
            ui.label(tr("New panel border"));
            ui.add(egui::DragValue::new(&mut self.frame.border_width).range(0.0..=200.0).speed(0.2).suffix(" px"));
        });
        let comic = self.ensure_comic();
        ui.horizontal(|ui| {
            ui.label(tr("Gutters"));
            ui.add(egui::DragValue::new(&mut comic.gutter.horizontal).range(0.0..=2000.0).speed(0.5).prefix("↔ ").suffix(" px"));
            ui.add(egui::DragValue::new(&mut comic.gutter.vertical).range(0.0..=2000.0).speed(0.5).prefix("↕ ").suffix(" px"));
        });
        ui.horizontal(|ui| {
            ui.label(tr("Reading"));
            ui.selectable_value(&mut comic.geometry.binding, Binding::LeftToRight, tr("Left to right"));
            ui.selectable_value(&mut comic.geometry.binding, Binding::RightToLeft, tr("Right to left"));
        });
        ui.small(tr("Each panel is a folder clipped to its frame: draw on the panel's art layer and nothing spills out."));
    }

    // ---- panels in the Layers panel -------------------------------------------------

    pub(crate) fn panels_section(&mut self, ui: &mut egui::Ui) {
        let Some(comic) = &mut self.comic else { return };
        comic.prune(&self.doc);
        if comic.panels.is_empty() {
            return;
        }
        let order = comic.reading_order();
        let stack = comic.stack_order(&self.doc);
        let direction = if comic.geometry.binding == Binding::RightToLeft { "right to left" } else { "left to right" };
        let mut action: Option<(PanelId, PanelAction)> = None;
        egui::CollapsingHeader::new(format!("Panels ({}, reading {direction})", comic.panels.len()))
            .id_salt("panels_section")
            .default_open(true)
            .show(ui, |ui| {
                for id in &stack {
                    let Some(panel) = comic.panel(*id) else {
                        continue;
                    };
                    let reading = order.iter().position(|o| o == id).map_or(0, |k| k + 1);
                    let selected = self.selected_panel == Some(*id);
                    let visible = comic.layers.get(id).and_then(|l| self.doc.find(l.folder)).is_some_and(|f| f.visible);
                    let clip = comic.clip_enabled(&self.doc, *id);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 3.0;
                        let eye = if visible { Icon::Visible } else { Icon::Transparency };
                        if icon_button(ui, eye, 13.0, false, "Show / hide the panel").clicked() {
                            action = Some((*id, PanelAction::Visible(!visible)));
                        }
                        if ui
                            .selectable_label(selected, format!("#{reading}  Panel {}", id.0))
                            .on_hover_text(tr("Select its art layer"))
                            .clicked()
                        {
                            action = Some((*id, PanelAction::Select));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 1.0;
                            if icon_button(ui, Icon::Delete, 12.0, false, "Delete the panel and its layers").clicked() {
                                action = Some((*id, PanelAction::Delete));
                            }
                            if icon_button(ui, Icon::Down, 12.0, false, "Move down").clicked() {
                                action = Some((*id, PanelAction::Move(false)));
                            }
                            if icon_button(ui, Icon::Up, 12.0, false, "Move up").clicked() {
                                action = Some((*id, PanelAction::Move(true)));
                            }
                            let tint = if clip { Icon::LayerMask } else { Icon::Transparency };
                            if icon_button(ui, tint, 12.0, clip, "Clip the art to the frame").clicked() {
                                action = Some((*id, PanelAction::Clip(!clip)));
                            }
                        });
                    });
                    if selected {
                        // Border of the selected panel.
                        let mut border = panel.border.clone();
                        let mut changed = false;
                        let (mut start, mut dragging) = (false, false);
                        ui.indent(("panel_border", id.0), |ui| {
                            ui.horizontal(|ui| {
                                changed |= ui.checkbox(&mut border.visible, tr("Border")).changed();
                                let w = ui.add_enabled(
                                    border.visible,
                                    egui::DragValue::new(&mut border.width).range(0.0..=200.0).speed(0.2).suffix(" px"),
                                );
                                start |= w.drag_started();
                                dragging |= w.dragged();
                                changed |= w.changed();
                                let mut c = Color32::from_rgb(border.color[0], border.color[1], border.color[2]);
                                if ui.color_edit_button_srgba(&mut c).changed() {
                                    border.color = [c.r(), c.g(), c.b()];
                                    changed = true;
                                }
                            });
                            ui.horizontal(|ui| {
                                let mut dashed = border.dash.is_some();
                                if ui.checkbox(&mut dashed, tr("Dashed")).changed() {
                                    border.dash = dashed.then_some([border.width.max(1.0) * 4.0, border.width.max(1.0) * 3.0]);
                                    changed = true;
                                }
                                changed |= ui.checkbox(&mut border.antialias, tr("Anti-aliased")).changed();
                            });
                        });
                        if changed {
                            // One undo step per edit: a drag on the width counts once.
                            action = Some((*id, PanelAction::Border(border, start || !dragging)));
                        }
                    }
                }
            });
        if let Some((id, action)) = action {
            self.apply_panel_action(id, action);
        }
    }

    fn apply_panel_action(&mut self, id: PanelId, action: PanelAction) {
        let Some(layers) = self.comic.as_ref().and_then(|c| c.layers.get(&id).copied()) else {
            return;
        };
        match action {
            PanelAction::Select => {
                self.selected_panel = Some(id);
                self.doc.active = layers.art;
                self.edit_mask = false;
            }
            PanelAction::Visible(on) => {
                self.checkpoint();
                for layer in [layers.folder, layers.border] {
                    if let Some(l) = self.doc.find_mut(layer) {
                        l.visible = on;
                    }
                }
                self.recomposite = true;
            }
            PanelAction::Clip(on) => {
                self.checkpoint();
                if let Some(comic) = &self.comic {
                    comic.set_clip(&mut self.doc, id, on);
                }
                self.recomposite = true;
            }
            PanelAction::Move(up) => {
                self.checkpoint();
                if let Some(comic) = &self.comic {
                    comic.move_panel(&mut self.doc, id, up);
                }
                self.recomposite = true;
            }
            PanelAction::Delete => {
                self.checkpoint();
                if let Some(comic) = &mut self.comic {
                    comic.remove_panel(&mut self.doc, id);
                }
                if self.doc.find(self.doc.active).is_none() {
                    if let Some(first) = self.doc.layers.last() {
                        self.doc.active = first.id;
                    }
                }
                self.selected_panel = None;
                self.resync_gpu();
            }
            PanelAction::Border(border, checkpoint) => {
                if checkpoint {
                    self.checkpoint();
                }
                if let Some(comic) = &mut self.comic {
                    comic.set_border(&mut self.doc, id, border);
                }
                self.upload_plane(Plane::Color(layers.border));
            }
        }
    }

    // ---- wizard -----------------------------------------------------------------------

    pub(crate) fn comic_wizard_window(&mut self, ctx: &egui::Context) {
        let Some(mut wizard) = self.comic_wizard.take() else {
            return;
        };
        let mut open = true;
        let mut create = false;
        egui::Window::new(tr("New Comic Page / Storyboard")).collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
            ui.set_width(980.0);
            ui.horizontal_top(|ui| {
                // Formats.
                ui.vertical(|ui| {
                    ui.set_width(330.0);
                    ui.label(egui::RichText::new(tr("Format")).strong());
                    egui::ScrollArea::vertical().id_salt("wizard_formats").max_height(470.0).show(ui, |ui| {
                        for category in [Category::Oriental, Category::Western, Category::Webtoon, Category::Custom] {
                            let name = match category {
                                Category::Oriental => "Manga / doujinshi",
                                Category::Western => "Western / European",
                                Category::Webtoon => "Webtoon",
                                Category::Custom => "Custom",
                            };
                            ui.label(egui::RichText::new(name).color(theme::text_dim()));
                            let mut picked = None;
                            ui.horizontal_wrapped(|ui| {
                                for (i, preset) in wizard.presets.iter().enumerate().filter(|(_, p)| p.category == category) {
                                    let selected = wizard.preset == i;
                                    if preset_card(ui, preset, selected).clicked() && !selected {
                                        picked = Some(i);
                                    }
                                }
                            });
                            if let Some(i) = picked {
                                wizard.select(i);
                            }
                            ui.add_space(4.0);
                        }
                    });
                });
                ui.separator();
                // Templates on the chosen format.
                ui.vertical(|ui| {
                    ui.set_width(330.0);
                    ui.label(egui::RichText::new(tr("Layout")).strong());
                    let geometry = wizard.settings.geometry();
                    let gutter = wizard.settings.gutter();
                    egui::ScrollArea::vertical().id_salt("wizard_templates").max_height(470.0).show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            for (i, template) in wizard.templates.iter().enumerate() {
                                let panels = template.for_page(&geometry, &gutter, 1);
                                let (rect, response) = ui.allocate_exact_size(Vec2::new(96.0, 128.0), Sense::click());
                                let selected = wizard.template == i;
                                let fill = if selected {
                                    theme::selected()
                                } else if response.hovered() {
                                    Color32::from_gray(62)
                                } else {
                                    theme::well()
                                };
                                ui.painter().rect_filled(rect, 4.0, fill);
                                draw_page(
                                    ui.painter(),
                                    Rect::from_min_size(rect.min + Vec2::new(6.0, 6.0), Vec2::new(84.0, 96.0)),
                                    &geometry,
                                    &panels,
                                );
                                ui.painter().text(
                                    Pos2::new(rect.center().x, rect.bottom() - 4.0),
                                    Align2::CENTER_BOTTOM,
                                    template.name,
                                    egui::FontId::proportional(10.0),
                                    theme::text(),
                                );
                                if response.on_hover_text(template.description).clicked() {
                                    wizard.template = i;
                                }
                            }
                        });
                    });
                });
                ui.separator();
                // Settings and the big preview.
                ui.vertical(|ui| {
                    ui.set_width(280.0);
                    ui.label(egui::RichText::new(tr("Page")).strong());
                    let s = &mut wizard.settings;
                    if let Some(webtoon) = &mut s.webtoon {
                        ui.horizontal(|ui| {
                            ui.label(tr("Width"));
                            ui.add(egui::DragValue::new(&mut webtoon.width_px).range(200..=2000).suffix(" px"));
                        });
                        ui.horizontal(|ui| {
                            ui.label(tr("Length"));
                            ui.add(egui::DragValue::new(&mut webtoon.length_px).range(500..=MAX_CANVAS_SIZE).suffix(" px"));
                        });
                        ui.horizontal(|ui| {
                            ui.label(tr("Slices every"));
                            ui.add(egui::DragValue::new(&mut webtoon.slice_px).range(100..=4000).suffix(" px"));
                        });
                    } else {
                        let unit = wizard.unit;
                        egui::Grid::new("wizard_page_grid").num_columns(2).spacing([8.0, 5.0]).show(ui, |ui| {
                            ui.label(tr("Units"));
                            unit_combo(ui, "wizard_unit", &mut wizard.unit);
                            ui.end_row();
                            if s.category == Category::Custom {
                                ui.label(tr("Paper"));
                                if let Some(size) = paper_combo(ui, "wizard_paper", s.trim_mm) {
                                    s.trim_mm = size;
                                }
                                ui.end_row();
                                ui.label(tr("Orientation"));
                                ui.horizontal(|ui| orientation_buttons(ui, &mut s.trim_mm));
                                ui.end_row();
                                ui.label(tr("Trim"));
                                ui.horizontal(|ui| {
                                    length_edit(ui, &mut s.trim_mm[0], unit, s.dpi, 20.0..=600.0);
                                    ui.label(tr("×"));
                                    length_edit(ui, &mut s.trim_mm[1], unit, s.dpi, 20.0..=600.0);
                                });
                            } else {
                                ui.label(tr("Trim"));
                                ui.label(format!(
                                    "{} × {} {}",
                                    show(s.trim_mm[0], unit, s.dpi),
                                    show(s.trim_mm[1], unit, s.dpi),
                                    unit.label()
                                ));
                            }
                            ui.end_row();
                            ui.label(tr("Resolution"));
                            ui.horizontal(|ui| dpi_edit(ui, "wizard_dpi", &mut s.dpi));
                            ui.end_row();
                            ui.label(tr("Bleed"));
                            length_edit(ui, &mut s.bleed_mm, unit, s.dpi, 0.0..=20.0);
                            ui.end_row();
                            if let Some(inner) = &mut s.inner_frame_mm {
                                ui.label(tr("Inner frame"));
                                ui.horizontal(|ui| {
                                    length_edit(ui, &mut inner[0], unit, s.dpi, 20.0..=600.0);
                                    ui.label(tr("×"));
                                    length_edit(ui, &mut inner[1], unit, s.dpi, 20.0..=600.0);
                                });
                            } else {
                                ui.label(tr("Safe margin"));
                                length_edit(ui, &mut s.safe_margin_mm, unit, s.dpi, 0.0..=60.0);
                            }
                            ui.end_row();
                            ui.label(tr("Gutters"));
                            ui.horizontal(|ui| {
                                length_edit(ui, &mut s.gutter_mm[0], unit, s.dpi, 0.0..=30.0)
                                    .on_hover_text(tr("Between side-by-side panels"));
                                length_edit(ui, &mut s.gutter_mm[1], unit, s.dpi, 0.0..=30.0).on_hover_text(tr("Between tiers"));
                            });
                            ui.end_row();
                        });
                        if ui.small_button(tr("Standard print margins")).on_hover_text(tr("3 mm bleed, 5 mm safe margin")).clicked() {
                            s.bleed_mm = STANDARD_BLEED_MM;
                            s.inner_frame_mm = None;
                            s.safe_margin_mm = STANDARD_SAFE_MM;
                        }
                        ui.horizontal(|ui| {
                            ui.label(tr("Binding"));
                            ui.selectable_value(&mut s.binding, Binding::LeftToRight, tr("Left (western)"));
                            ui.selectable_value(&mut s.binding, Binding::RightToLeft, tr("Right (manga)"));
                        });
                    }
                    let geometry = wizard.settings.geometry();
                    let panels = wizard.templates[wizard.template].for_page(&geometry, &wizard.settings.gutter(), 1);
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(280.0, 300.0), Sense::hover());
                    ui.painter().rect_filled(rect, 4.0, theme::well());
                    draw_page(ui.painter(), rect.shrink(8.0), &geometry, &panels);
                    let (w, h) = geometry.canvas;
                    let fits = w <= MAX_CANVAS_SIZE && h <= MAX_CANVAS_SIZE;
                    ui.label(format!("{w} × {h} px · {} panels", panels.len()));
                    if !fits {
                        ui.colored_label(
                            Color32::from_rgb(240, 120, 100),
                            format!("Too big: the canvas limit is {MAX_CANVAS_SIZE} px. Lower the DPI."),
                        );
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        create = ui.add_enabled(fits, egui::Button::new(tr("Create"))).clicked();
                        if ui.button(tr("Cancel")).clicked() {
                            wizard.cancelled = true;
                        }
                    });
                });
            });
        });
        if create {
            self.create_comic_page(&wizard);
        } else if open && !wizard.cancelled {
            self.comic_wizard = Some(wizard);
        }
    }

    fn create_comic_page(&mut self, wizard: &ComicWizard) {
        let geometry = wizard.settings.geometry();
        let gutter = wizard.settings.gutter();
        let (w, h) = geometry.canvas;
        let mut doc = Document::new(w, h);
        doc.dpi = wizard.settings.dpi;
        let mut page = ComicPage::new(geometry.clone(), gutter);
        let template = &wizard.templates[wizard.template];
        // Border width ~0.5 mm, at least 2 px.
        let width = (comic_layout::page_preset::mm_to_px(0.5, wizard.settings.dpi)).max(2.0);
        for mut panel in template.for_page(&geometry, &gutter, 1) {
            panel.border.width = width;
            page.add_panel(&mut doc, panel);
        }
        if let Some(first) = page.reading_order().first().and_then(|id| page.layers.get(id)) {
            doc.active = first.art;
        }
        self.set_document(doc, None);
        self.frame.border_width = width;
        self.comic = Some(page);
        self.tool = Tool::Frame;
        self.frame.mode = FrameMode::Edit;
        self.status = format!("New {} page: {} × {} px, {}", wizard.settings.name, w, h, template.name);
    }
}

enum PanelAction {
    Select,
    Visible(bool),
    Clip(bool),
    Move(bool),
    Delete,
    /// New border; whether this edit gets its own undo step.
    Border(BorderStyle, bool),
}

/// State of the New Comic Page wizard.
pub(crate) struct ComicWizard {
    presets: Vec<PagePreset>,
    preset: usize,
    /// The chosen preset, editable.
    settings: PagePreset,
    templates: Vec<Template>,
    template: usize,
    /// What lengths are shown in.
    unit: Unit,
    cancelled: bool,
}

impl ComicWizard {
    pub(crate) fn open() -> Self {
        let mut presets = PagePreset::builtin();
        // A starting point for the custom page.
        if let Some(custom) = PagePreset::custom("Custom", [210.0, 297.0], 3.0, 10.0, 600, Binding::LeftToRight, [3.0, 4.0]) {
            presets.push(custom);
        }
        // Standard papers with print margins, at the finest usual resolution that fits.
        for name in ["A3", "A4", "A5", "B6"] {
            let Some(paper) = PaperSize::get(name) else { continue };
            let fitting = [600, 350, 300]
                .into_iter()
                .map(|dpi| PagePreset::from_paper(paper, Orientation::Portrait, dpi, Binding::LeftToRight))
                .find(|p| {
                    let (w, h) = p.geometry().canvas;
                    w <= MAX_CANVAS_SIZE && h <= MAX_CANVAS_SIZE
                });
            presets.extend(fitting);
        }
        let settings = presets[0].clone();
        let mut wizard =
            Self { presets, preset: 0, settings, templates: Template::builtin(), template: 0, unit: Unit::Mm, cancelled: false };
        wizard.select(0);
        wizard
    }

    fn select(&mut self, i: usize) {
        self.preset = i;
        self.settings = self.presets[i].clone();
        if let Some(webtoon) = &mut self.settings.webtoon {
            // The GPU canvas tops out at MAX_CANVAS_SIZE.
            webtoon.length_px = webtoon.length_px.min(MAX_CANVAS_SIZE);
        }
        // The first template meant for this kind of page.
        self.template =
            self.templates.iter().position(|t| t.suits.contains(&self.settings.category) && t.name != "Splash").unwrap_or(0);
    }
}

/// A length in unit, with the unit's usual decimals.
fn show(mm: f64, unit: Unit, dpi: u32) -> String {
    format!("{:.*}", unit.decimals(), unit.from_mm(mm, dpi))
}

/// A preset as a small card: page shape with bleed and inner frame, name, size.
fn preset_card(ui: &mut egui::Ui, preset: &PagePreset, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(100.0, 120.0), Sense::click());
    let fill = if selected {
        theme::selected()
    } else if response.hovered() {
        Color32::from_gray(62)
    } else {
        theme::well()
    };
    ui.painter().rect_filled(rect, 4.0, fill);
    let geometry = preset.geometry();
    draw_page(ui.painter(), Rect::from_min_size(rect.min + Vec2::new(8.0, 6.0), Vec2::new(84.0, 66.0)), &geometry, &[]);
    // Long names wrap onto two lines.
    let name = ui.painter().layout(preset.name.clone(), egui::FontId::proportional(10.0), theme::text(), rect.width() - 6.0);
    let name_pos = Pos2::new(rect.center().x - name.size().x * 0.5, rect.bottom() - 18.0 - name.size().y);
    ui.painter().galley(name_pos, name, theme::text());
    let size = match preset.webtoon {
        Some(w) => format!("{} px wide", w.width_px),
        None => format!("{:.0} × {:.0} mm", preset.trim_mm[0], preset.trim_mm[1]),
    };
    ui.painter().text(
        Pos2::new(rect.center().x, rect.bottom() - 6.0),
        Align2::CENTER_BOTTOM,
        size,
        egui::FontId::proportional(9.5),
        theme::text_dim(),
    );
    response.on_hover_text(match preset.binding {
        Binding::RightToLeft => "Reads right to left",
        Binding::LeftToRight => "Reads left to right",
        Binding::Vertical => "One vertical strip",
    })
}

/// A page thumbnail fitted into `area`: bleed (grey), trim (white), inner
/// frame (blue) and panel outlines.
pub(crate) fn draw_page(painter: &egui::Painter, area: Rect, geometry: &PageGeometry, panels: &[Panel]) {
    let (cw, ch) = (geometry.canvas.0 as f32, geometry.canvas.1 as f32);
    let scale = (area.width() / cw).min(area.height() / ch);
    let origin = area.center() - Vec2::new(cw, ch) * scale * 0.5;
    let at = |p: Point| origin + Vec2::new(p[0] as f32, p[1] as f32) * scale;
    let rect = |r: [f64; 4]| Rect::from_min_max(at([r[0], r[1]]), at([r[0] + r[2], r[1] + r[3]]));
    painter.rect_filled(rect(geometry.bleed), 0.0, Color32::from_gray(200));
    painter.rect_filled(rect(geometry.trim), 0.0, Color32::WHITE);
    if geometry.inner != geometry.trim {
        painter.rect_stroke(
            rect(geometry.inner),
            0.0,
            Stroke::new(1.0_f32, Color32::from_rgb(70, 140, 230)),
            egui::StrokeKind::Middle,
        );
    }
    for panel in panels {
        let mut points: Vec<Pos2> = panel.shape.iter().map(|&p| at(p)).collect();
        points.push(points[0]);
        painter.add(egui::Shape::line(points, Stroke::new(1.0_f32, Color32::from_gray(20))));
    }
    if let Some(w) = geometry.webtoon {
        for k in 1..(w.length_px / w.slice_px.max(1)) {
            let y = (k * w.slice_px) as f64;
            painter.line_segment([at([0.0, y]), at([cw as f64, y])], Stroke::new(1.0_f32, Color32::from_rgb(230, 60, 160)));
        }
    }
}
