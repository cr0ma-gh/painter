//! The canvas widget (input and the wgpu paint callback) and the navigator thumbnail.

use eframe::egui::{self, Align2, Color32, CursorIcon, Event, FontId, Key, PointerButton, Pos2, Rect, Sense, Stroke, Vec2};
use eframe::egui_wgpu;
use eframe::wgpu;
use painter_core::StrokePoint;
use painter_render::{build_plan, live_planes, CanvasFrame, GpuCanvas, ViewUniform};

use crate::app::PainterApp;
use crate::tools::Tool;
use crate::view::ViewTransform;

/// A multi-finger touch in progress: when it began, the most fingers seen,
/// and how much it has moved (to tell taps from pans).
pub(crate) struct TouchGesture {
    start: f64,
    touches: usize,
    travel: f32,
}

/// Carries one frame of canvas work to the GPU canvas stored in egui's callback resources.
struct CanvasCallback(CanvasFrame);

impl egui_wgpu::CallbackTrait for CanvasCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(canvas) = resources.get_mut::<GpuCanvas>() {
            canvas.prepare(device, queue, encoder, &self.0);
        }
        Vec::new()
    }

    fn paint(&self, _info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &egui_wgpu::CallbackResources) {
        if let Some(canvas) = resources.get::<GpuCanvas>() {
            canvas.paint(pass);
        }
    }
}

/// Draws the latest composite again, scaled into the navigator.
struct ThumbnailCallback(ViewUniform);

impl egui_wgpu::CallbackTrait for ThumbnailCallback {
    // After every `prepare`, so it shows this frame's composite.
    fn finish_prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(canvas) = resources.get_mut::<GpuCanvas>() {
            canvas.prepare_thumbnail(device, queue, self.0);
        }
        Vec::new()
    }

    fn paint(&self, _info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &egui_wgpu::CallbackResources) {
        if let Some(canvas) = resources.get::<GpuCanvas>() {
            canvas.paint_thumbnail(pass);
        }
    }
}

impl PainterApp {
    pub(crate) fn canvas_ui(&mut self, ui: &mut egui::Ui) {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        ui.painter().rect_filled(rect, 0.0, crate::theme::canvas_backdrop());
        self.canvas_rect = rect;
        // Wait for a real canvas size: the first frames of a window that opens
        // small or minimised would fit the image to a few pixels.
        if self.fit_pending && rect.width() > 200.0 && rect.height() > 200.0 {
            self.view.fit(rect.size(), self.doc_size());
            self.fit_pending = false;
        }
        self.canvas_input(ui, rect, &response);

        if !self.uploads.is_empty() {
            self.doc_touched = true;
        }
        let stroke = self.take_stroke_preview();
        if self.gpu_ready {
            let time = self.started.elapsed().as_secs_f32();
            let frame = CanvasFrame {
                epoch: self.epoch,
                doc_size: self.doc.canvas(),
                plan: build_plan(&self.doc),
                uploads: std::mem::take(&mut self.uploads),
                live_planes: live_planes(&self.doc),
                selection: self.doc.selection.is_some(),
                recomposite: std::mem::take(&mut self.recomposite),
                stroke,
                transform: self.transform_preview(),
                scenes: self.scene_previews(),
                view: self.view.uniform(rect.size(), self.doc_size()).with_selection_outline(time),
            };
            ui.painter().add(egui_wgpu::Callback::new_paint_callback(rect, CanvasCallback(frame)));
        } else {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "GPU renderer unavailable: wgpu failed to initialise",
                FontId::proportional(16.0),
                Color32::LIGHT_RED,
            );
        }
        let painter = ui.painter_at(rect);
        self.draw_rulers_overlay(&painter, rect);
        self.draw_tool_overlay(&painter, rect);
        self.draw_line_guide(&painter, rect);
        self.draw_brush_cursor(&painter, rect, &response);
        self.draw_eyedropper(&painter);
        self.transform_hud(ui.ctx(), rect);
    }

    fn canvas_input(&mut self, ui: &egui::Ui, rect: Rect, response: &egui::Response) {
        let doc_size = self.doc_size();
        let (events, space, zoom_delta, scroll) =
            ui.input(|i| (i.events.clone(), i.key_down(Key::Space), i.zoom_delta(), i.raw_scroll_delta));
        let hovering = response.contains_pointer();
        self.pointer_mods = ui.input(|i| i.modifiers);
        self.hover_doc = response.hover_pos().filter(|p| rect.contains(*p)).map(|p| self.view.screen_to_doc(rect, doc_size, p));

        // Touch: see this frame's touches first, so a finger press is known
        // before it turns into a pointer press.
        for event in events.iter().filter(|e| matches!(e, Event::Touch { .. })) {
            self.tablet.observe(event);
        }
        let multi = ui.input(|i| i.multi_touch());
        self.touch_gestures(multi, rect);
        // A finger resting still picks a colour instead of panning.
        let long_press = self.eyedropper_touches(ui.ctx(), &events, rect);

        // Navigation. On a 3D layer with the 3D tool, the middle button and the
        // wheel steer the scene's camera instead (Space + drag still pans the canvas).
        // With "Pencil only", a finger pans instead of painting.
        let finger = self.tools.pencil_only && self.tablet.finger_down() && !long_press;
        let hand = self.tool == Tool::Hand || space || finger || multi.is_some();
        let scene_nav = self.scene_navigation_active();
        if scene_nav {
            self.scene_navigate(ui, rect, response);
        }
        if (!scene_nav && response.dragged_by(PointerButton::Middle)) || (hand && response.dragged_by(PointerButton::Primary)) {
            self.view.pan += response.drag_delta();
        }
        if let Some(anchor) = response.hover_pos().filter(|_| !scene_nav || zoom_delta != 1.0) {
            let factor = zoom_delta * (scroll.y * 0.002).exp();
            if factor != 1.0 {
                self.view.zoom_about(rect, doc_size, anchor, factor);
            }
        }

        // Walk raw events so fast pen motion keeps every sample.
        let to_doc = |view: &ViewTransform, pos: Pos2| view.screen_to_doc(rect, doc_size, pos);
        let is_pointer = |e: &Event| matches!(e, Event::PointerMoved(_) | Event::PointerButton { .. });
        self.tablet.begin_events(events.iter().filter(|e| is_pointer(e)).count());
        // WinTab: every pen packet of the frame instead of the few merged
        // pointer moves Windows sends, so fast strokes keep their shape.
        let now = self.started.elapsed().as_secs_f32();
        let pointer = ui.input(|i| i.pointer.latest_pos());
        let packets = self.tablet.packet_points(ui.ctx().pixels_per_point(), now, pointer);
        let mut packets_fed = packets.is_empty();
        let feed_packets = |app: &mut PainterApp, fed: &mut bool| {
            if *fed {
                return;
            }
            *fed = true;
            for pp in &packets {
                let p = app.view.screen_to_doc(rect, doc_size, pp.pos);
                app.continue_stroke(StrokePoint::new(p.x, p.y, pp.pressure).with_tilt(pp.tilt).at_time(pp.time));
            }
        };
        for event in &events {
            self.tablet.observe(event);
            if is_pointer(event) {
                self.tablet.next_event();
            }
            match event {
                // Alt with a brush, fill or gradient: the eyedropper while the button is held.
                Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers, .. }
                    if !hand && hovering && rect.contains(*pos) && modifiers.alt && self.alt_eyedropper() && self.drag.is_none() =>
                {
                    let p = to_doc(&self.view, *pos);
                    self.begin_temporary_pick(p);
                }
                Event::PointerMoved(pos) if self.eyedropper_held() && !long_press => {
                    let p = to_doc(&self.view, *pos);
                    self.eyedrop_at(p);
                }
                Event::PointerButton { button: PointerButton::Primary, pressed: false, .. } if self.eyedropper_held() && !long_press => {
                    self.end_pick();
                }
                Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers, .. }
                    if !hand && hovering && rect.contains(*pos) && self.stroke.is_none() && self.drag.is_none() && self.line.is_none() =>
                {
                    let p = to_doc(&self.view, *pos);
                    if self.tool == Tool::Brush && modifiers.shift {
                        // Shift: a straight line (see `crate::lines`).
                        self.begin_line(self.stroke_point(p));
                    } else if self.tool == Tool::Brush {
                        self.begin_stroke(self.stroke_point(p));
                    } else {
                        self.tool_press(p, *modifiers);
                    }
                }
                Event::PointerMoved(pos) if self.line.is_some() => {
                    let p = to_doc(&self.view, *pos);
                    let snap = self.pointer_mods.command || self.pointer_mods.ctrl;
                    self.move_line(self.stroke_point(p), snap);
                }
                Event::PointerMoved(_) if self.stroke.is_some() && !packets.is_empty() => feed_packets(self, &mut packets_fed),
                Event::PointerMoved(pos) if self.stroke.is_some() => {
                    let p = to_doc(&self.view, *pos);
                    self.continue_stroke(self.stroke_point(p));
                }
                Event::PointerMoved(pos) if self.drag.is_some() => {
                    let p = to_doc(&self.view, *pos);
                    self.tool_move(p);
                }
                Event::PointerButton { button: PointerButton::Primary, pressed: false, .. } | Event::PointerGone => {
                    if self.line.is_some() {
                        self.finish_line();
                    }
                    if self.stroke.is_some() {
                        feed_packets(self, &mut packets_fed);
                        self.end_stroke();
                    }
                    if self.drag.is_some() {
                        self.tool_release();
                    }
                }
                _ => {}
            }
        }
        // Packets in a frame with no pointer move (Windows merged them all away).
        if self.stroke.is_some() {
            feed_packets(self, &mut packets_fed);
        }
        if !ui.input(|i| i.pointer.primary_down()) {
            if self.line.is_some() {
                self.finish_line();
            }
            if self.stroke.is_some() {
                self.end_stroke();
            }
            if self.drag.is_some() {
                self.tool_release();
            }
        }

        if hovering {
            let icon = match (hand, response.dragged(), self.tool) {
                (true, true, _) => CursorIcon::Grabbing,
                (true, false, _) => CursorIcon::Grab,
                (_, _, Tool::Fill) => CursorIcon::Cell,
                _ => CursorIcon::Crosshair,
            };
            ui.ctx().set_cursor_icon(icon);
        }
    }

    /// Two or more fingers: pan and rotate the view (pinch zoom comes through
    /// `zoom_delta`), drop a stroke a stray finger just started, and turn quick
    /// taps into undo (two fingers), redo (three) and zen mode (four).
    fn touch_gestures(&mut self, multi: Option<egui::MultiTouchInfo>, rect: Rect) {
        let now = self.started.elapsed().as_secs_f64();
        match multi {
            Some(mt) => {
                if self.stroke.as_ref().is_some_and(|s| s.age() < 0.3) {
                    self.discard_stroke();
                }
                self.view.pan += mt.translation_delta;
                if mt.rotation_delta.abs() > 1e-4 {
                    // About the gesture's centre.
                    let pivot = self.view.screen_to_doc(rect, self.doc_size(), mt.center_pos);
                    self.view.set_rotation(self.view.rotation + mt.rotation_delta);
                    let moved = self.view.doc_to_screen(rect, self.doc_size(), pivot);
                    self.view.pan += mt.center_pos - moved;
                }
                let g = self.gesture.get_or_insert(TouchGesture { start: now, touches: 0, travel: 0.0 });
                g.touches = g.touches.max(mt.num_touches);
                g.travel += mt.translation_delta.length() + (mt.zoom_delta - 1.0).abs() * 200.0 + mt.rotation_delta.abs() * 100.0;
            }
            None => {
                if let Some(g) = self.gesture.take() {
                    let tap = now - g.start < 0.35 && g.travel < 12.0;
                    match g.touches {
                        2 if tap => self.undo(),
                        3 if tap => self.redo(),
                        4 if tap => self.toggle_zen(),
                        _ => {}
                    }
                }
            }
        }
    }

    /// A pointer sample at canvas point `p`, with pen pressure, tilt and time.
    fn stroke_point(&self, p: Pos2) -> StrokePoint {
        StrokePoint::new(p.x, p.y, self.tablet.pressure()).with_tilt(self.tablet.tilt()).at_time(self.started.elapsed().as_secs_f32())
    }

    fn draw_brush_cursor(&self, painter: &egui::Painter, rect: Rect, response: &egui::Response) {
        if self.tool != Tool::Brush {
            return;
        }
        let Some(pos) = response.hover_pos() else {
            return;
        };
        if !rect.contains(pos) {
            return;
        }
        let radius = (self.current_brush().size * 0.5 * self.view.zoom).max(1.5);
        painter.circle_stroke(pos, radius + 1.0, Stroke::new(1.0_f32, Color32::from_black_alpha(140)));
        painter.circle_stroke(pos, radius, Stroke::new(1.0_f32, Color32::from_white_alpha(200)));
    }

    /// The whole canvas at fit size, with the visible area outlined; click or drag to pan.
    pub(crate) fn navigator_thumbnail(&mut self, ui: &mut egui::Ui) {
        let doc = self.doc_size();
        let width = ui.available_width().max(60.0);
        let height = (width * doc.y / doc.x).min(240.0);
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click_and_drag());
        ui.painter().rect_filled(rect, 2.0, crate::theme::canvas_backdrop());
        let thumb = ViewTransform { pan: Vec2::ZERO, zoom: (rect.width() / doc.x).min(rect.height() / doc.y), rotation: 0.0, flip: false };
        if self.gpu_ready {
            let uniform = thumb.uniform(rect.size(), doc);
            ui.painter().add(egui_wgpu::Callback::new_paint_callback(rect, ThumbnailCallback(uniform)));
        }
        // Visible canvas area.
        if self.canvas_rect.is_positive() {
            let c = self.canvas_rect;
            let corners = [c.left_top(), c.right_top(), c.right_bottom(), c.left_bottom()]
                .map(|p| thumb.doc_to_screen(rect, doc, self.view.screen_to_doc(c, doc, p)));
            let painter = ui.painter_at(rect);
            painter.add(egui::Shape::closed_line(corners.to_vec(), Stroke::new(1.5_f32, Color32::from_rgb(255, 80, 80))));
        }
        if let Some(pos) = response.interact_pointer_pos() {
            if response.dragged() || response.clicked() {
                let target = thumb.screen_to_doc(rect, doc, pos);
                self.view.center_on(doc, target);
            }
        }
    }
}
