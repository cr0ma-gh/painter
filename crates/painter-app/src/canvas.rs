//! The canvas widget (input and the wgpu paint callback) and the navigator thumbnail.

use eframe::egui::{self, Align2, Color32, CursorIcon, Event, FontId, Key, Modifiers, PointerButton, Pos2, Rect, Sense, Stroke, Vec2};
use eframe::egui_wgpu;
use eframe::wgpu;
use painter_core::StrokePoint;
use painter_render::{build_plan, live_planes, CanvasFrame, GpuCanvas, RenderProfile, ViewUniform};

use crate::app::PainterApp;
use crate::palm::{ContactClass, ContactInput, ContactPhase, InkEvent, InkSample, NavGesture, PalmMode, PalmSettings};
use crate::tools::Tool;
use crate::view::ViewTransform;

/// The palm arbiter's navigation, as egui's multi-touch.
fn multi_touch(g: NavGesture) -> egui::MultiTouchInfo {
    egui::MultiTouchInfo {
        start_time: g.start_time,
        start_pos: g.start_pos,
        center_pos: g.center_pos,
        num_touches: g.num_touches,
        zoom_delta: g.zoom_delta,
        zoom_delta_2d: Vec2::splat(g.zoom_delta),
        rotation_delta: g.rotation_delta,
        translation_delta: g.translation_delta,
        force: 0.0,
    }
}

/// A multi-finger touch in progress: when it began, the most fingers seen,
/// and how much it has moved (to tell taps from pans).
pub(crate) struct TouchGesture {
    start: f64,
    touches: usize,
    travel: f32,
}

/// Carries one frame of canvas work to the GPU canvas stored in egui's callback resources,
/// and a new render profile to switch it to first, if any.
struct CanvasCallback(CanvasFrame, Option<RenderProfile>);

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
            if let Some(profile) = self.1 {
                canvas.set_profile(profile);
            }
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
        let (full, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
        // The ruler bar, when on, takes strips off the top and left.
        let (rect, bar_top, bar_left) = self.ruler_bar_split(full);
        let response = ui.interact(rect, ui.id().with("canvas"), Sense::click_and_drag());
        ui.painter().rect_filled(rect, 0.0, crate::theme::canvas_backdrop());
        self.canvas_rect = rect;
        // Wait for a real canvas size: the first frames of a window that opens
        // small or minimised would fit the image to a few pixels.
        if self.fit_pending && rect.width() > 200.0 && rect.height() > 200.0 {
            self.view.fit(rect.size(), self.doc_size());
            self.fit_pending = false;
        }
        self.canvas_input(ui, rect, &response);
        self.smart_tick(ui.ctx());
        self.pose_grab_keys(ui.ctx());
        self.shape_keys(ui.ctx());
        self.magnetic_keys(ui.ctx());
        self.text_keys(ui.ctx());

        if !self.uploads.is_empty() {
            self.doc_touched = true;
        }
        let stroke = self.take_stroke_preview();
        if self.gpu_ready {
            let time = self.started.elapsed().as_secs_f32();
            // Layers with Layer Property effects show what the effects draw.
            let pending = std::mem::take(&mut self.uploads);
            let uploads = self.effect_uploads(pending);
            let recomposite = std::mem::take(&mut self.recomposite);
            self.sent_uploads = (uploads.clone(), recomposite);
            let frame = CanvasFrame {
                epoch: self.epoch,
                doc_size: self.doc.canvas(),
                plan: build_plan(&self.doc),
                uploads,
                live_planes: live_planes(&self.doc),
                selection: self.doc.selection.is_some(),
                recomposite,
                stroke,
                transform: self.transform_preview(),
                scenes: self.scene_previews(),
                view: {
                    let v = self.view.uniform(rect.size(), self.doc_size()).with_selection_outline(time).with_channels(self.channels.rgb_view());
                    // A CMYK document showing some of its inks; Duotone, Lab and
                    // Multichannel documents through their display table.
                    match (self.channels.ink_view(), &self.proofer.lut, &self.proofer.display) {
                        (Some(inks), Some(_), _) => v.with_inks(inks),
                        (_, _, Some(_)) => v.with_display_lut(),
                        _ => v,
                    }
                },
                ink_lut: if self.proofer.display.is_some() { self.proofer.display.clone() } else { self.proofer.lut.clone() },
            };
            ui.painter().add(egui_wgpu::Callback::new_paint_callback(rect, CanvasCallback(frame, self.pending_render_profile.take())));
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
        self.draw_smart_shape(&painter, rect);
        self.draw_vector_edit(&painter, rect);
        self.draw_shape_draft(&painter, rect);
        self.draw_stamp_overlay(&painter, rect);
        self.draw_magnetic(&painter, rect);
        self.draw_text_overlay(&painter, rect);
        self.draw_session_cursors(&painter, rect);
        self.draw_clock(ui, rect);
        self.draw_brush_cursor(&painter, rect, &response);
        self.draw_eyedropper(&painter);
        self.palm_overlay(ui, rect);
        self.transform_hud(ui.ctx(), rect);
        self.nav_gizmo(ui, rect);
        self.ruler_bar_ui(ui, full, rect, bar_top, bar_left);
    }

    fn canvas_input(&mut self, ui: &egui::Ui, rect: Rect, response: &egui::Response) {
        let doc_size = self.doc_size();
        let (events, space, mut zoom_delta, scroll) =
            ui.input(|i| (i.events.clone(), i.key_down(Key::Space), i.zoom_delta(), i.raw_scroll_delta));
        // Not over the 3D navigation gizmo (it takes its own clicks and drags).
        let over_gizmo = self.nav_gizmo_rect(rect).zip(ui.input(|i| i.pointer.hover_pos())).is_some_and(|(g, p)| g.contains(p));
        let hovering = response.contains_pointer() && !over_gizmo;
        self.pointer_mods = ui.input(|i| i.modifiers);
        self.hover_doc = response.hover_pos().filter(|p| rect.contains(*p)).map(|p| self.view.screen_to_doc(rect, doc_size, p));

        // Touch: see this frame's touches first, so a finger press is known
        // before it turns into a pointer press.
        for event in events.iter().filter(|e| matches!(e, Event::Touch { .. })) {
            self.tablet.observe(event);
        }
        // Palm rejection: touches go through the arbiter, which says which
        // contact paints and which fingers navigate; egui's pointer (the first
        // touch, maybe the palm) and its multi-touch (every touch) are set aside.
        let touch_driven = self.palm_touches(ui, &events, rect);
        let multi = if touch_driven {
            let gesture = self.palm.take_gesture().map(multi_touch);
            // A pinch among the navigating fingers only (egui's would count the palm).
            zoom_delta = 1.0;
            if let Some(mt) = gesture.filter(|mt| mt.zoom_delta != 1.0) {
                self.view.zoom_about(rect, doc_size, mt.center_pos, mt.zoom_delta);
            }
            gesture
        } else {
            ui.input(|i| i.multi_touch())
        };
        self.touch_gestures(multi, rect, !touch_driven);
        // A finger resting still picks a colour instead of panning (never the palm).
        let long_press = if touch_driven {
            let palm = |id: u64| self.palm.class_of(id) == Some(ContactClass::Palm);
            let events: Vec<Event> = events.iter().filter(|e| !matches!(e, Event::Touch { id, .. } if palm(id.0))).cloned().collect();
            self.eyedropper_touches(ui.ctx(), &events, rect)
        } else {
            self.eyedropper_touches(ui.ctx(), &events, rect)
        };

        // Navigation. On a 3D layer with the 3D tool, the middle button and the
        // wheel steer the scene's camera instead (Space + drag still pans the canvas).
        // With "Pencil only", a finger pans instead of painting (the palm
        // arbiter's navigating fingers move the view in `touch_gestures`).
        let finger = !touch_driven && self.tools.pencil_only && self.tablet.finger_down() && !long_press;
        // Clip Studio's keys: Space pans, Shift + Space rotates, Ctrl + Space
        // zooms in and Alt + Space out; H, R and / pick the same as tools.
        let mods = self.pointer_mods;
        let nav = if finger || (!touch_driven && multi.is_some()) {
            Some(Nav::Pan)
        } else if space {
            Some(if mods.shift {
                Nav::Rotate
            } else if mods.command {
                Nav::Zoom { out: false }
            } else if mods.alt {
                Nav::Zoom { out: true }
            } else {
                Nav::Pan
            })
        } else {
            match self.tool {
                Tool::Hand => Some(Nav::Pan),
                Tool::Zoom => Some(Nav::Zoom { out: mods.alt }),
                Tool::Rotate => Some(Nav::Rotate),
                _ => None,
            }
        };
        let hand = nav.is_some();
        let scene_nav = self.scene_navigation_active();
        if scene_nav {
            self.scene_navigate(ui, rect, response);
        }
        let primary_drag = response.dragged_by(PointerButton::Primary);
        if (!scene_nav && response.dragged_by(PointerButton::Middle)) || (nav == Some(Nav::Pan) && primary_drag) {
            self.view.pan += response.drag_delta();
        }
        match nav {
            Some(Nav::Rotate) if primary_drag => {
                if let Some(now) = response.interact_pointer_pos() {
                    let (a, b) = (now - response.drag_delta() - rect.center(), now - rect.center());
                    if a.length() > 4.0 && b.length() > 4.0 {
                        self.view.set_rotation(self.view.rotation + b.angle() - a.angle());
                    }
                }
            }
            // A drag right zooms in, left out, round where it started; a click steps.
            Some(Nav::Zoom { .. }) if primary_drag => {
                let anchor = ui.input(|i| i.pointer.press_origin()).unwrap_or(rect.center());
                self.view.zoom_about(rect, doc_size, anchor, (response.drag_delta().x * 0.01).exp());
            }
            Some(Nav::Zoom { out }) if response.clicked_by(PointerButton::Primary) => {
                if let Some(at) = response.interact_pointer_pos() {
                    let factor = crate::view::zoom_step(self.view.zoom, !out) / self.view.zoom;
                    self.view.zoom_about(rect, doc_size, at, factor);
                }
            }
            _ => {}
        }
        // Shift + wheel rotates (egui turns it into sideways scrolling): a notch is about 5°.
        if hovering && mods.shift && !mods.command && scroll.x != 0.0 {
            let notches = scroll.x / 40.0;
            self.view.set_rotation(self.view.rotation - notches * crate::view::ROTATE_STEP.to_radians());
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
        // Ctrl + Alt: sideways motion resizes the brush; no stroke starts meanwhile.
        let resizing = !hand && self.brush_resize(if hovering { response.hover_pos().or(pointer) } else { None });
        let packets = self.tablet.packet_points(ui.ctx().pixels_per_point(), now, pointer);
        let mut packets_fed = packets.is_empty();
        let feed_packets = |app: &mut PainterApp, fed: &mut bool| {
            if *fed {
                return;
            }
            *fed = true;
            for pp in &packets {
                let p = app.view.screen_to_doc(rect, doc_size, pp.pos);
                app.continue_stroke(StrokePoint::new(p.x, p.y, pp.pressure).with_tilt(pp.tilt).with_azimuth(pp.azimuth).at_time(pp.time));
            }
        };
        for event in &events {
            self.tablet.observe(event);
            if is_pointer(event) {
                self.tablet.next_event();
            }
            // Touches press, move and release through the palm arbiter (below).
            if touch_driven && (is_pointer(event) || matches!(event, Event::PointerGone)) {
                continue;
            }
            match event {
                // Alt with a brush, fill or gradient: the eyedropper while the button is held.
                Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers, .. }
                    if !hand && !resizing && !modifiers.ctrl && hovering && rect.contains(*pos) && modifiers.alt && self.alt_eyedropper() && self.drag.is_none() =>
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
                    if !hand && !resizing && hovering && rect.contains(*pos) && self.stroke.is_none() && self.drag.is_none() && self.line.is_none() =>
                {
                    let p = to_doc(&self.view, *pos);
                    self.press_at(*pos, self.stroke_point(p), *modifiers);
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
                // A joint following the pointer (a click on it started it): no button held.
                Event::PointerMoved(pos) if self.pose_grabbing() && self.drag.is_none() => {
                    let p = to_doc(&self.view, *pos);
                    self.pose_move(p);
                }
                Event::PointerMoved(pos) if self.drag.is_some() => {
                    let p = to_doc(&self.view, *pos);
                    self.tool_move(p);
                }
                // The magnetic lasso follows the pointer between clicks.
                Event::PointerMoved(pos) if self.magnetic.active() => {
                    let p = to_doc(&self.view, *pos);
                    self.magnetic_move(p);
                }
                // A polygon's next side follows the pointer between clicks.
                Event::PointerMoved(pos) if self.tool == Tool::Shape && !self.shape.corners.is_empty() => {
                    self.shape.current = to_doc(&self.view, *pos);
                }
                Event::PointerButton { button: PointerButton::Primary, pressed: false, .. } | Event::PointerGone => {
                    if self.line.is_some() {
                        self.finish_line();
                    }
                    self.smart_release();
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
        if touch_driven {
            self.palm_ink(rect, hand || resizing, long_press);
        } else if !ui.input(|i| i.pointer.primary_down()) {
            if self.line.is_some() {
                self.finish_line();
            }
            self.smart_release();
            if self.stroke.is_some() {
                self.end_stroke();
            }
            if self.drag.is_some() {
                self.tool_release();
            }
        }

        if hovering {
            let icon = match (hand, response.dragged(), self.tool) {
                _ if nav == Some(Nav::Zoom { out: false }) => CursorIcon::ZoomIn,
                _ if nav == Some(Nav::Zoom { out: true }) => CursorIcon::ZoomOut,
                (true, true, _) => CursorIcon::Grabbing,
                (true, false, _) => CursorIcon::Grab,
                (_, _, Tool::Fill) => CursorIcon::Cell,
                // The brush outline alone, when the crosshair is turned off.
                (_, _, Tool::Brush | Tool::Stamp(_)) if !self.ws.chrome.brush_crosshair => CursorIcon::None,
                _ => CursorIcon::Crosshair,
            };
            ui.ctx().set_cursor_icon(icon);
        }
    }

    /// Two or more fingers: pan and rotate the view (pinch zoom comes through
    /// `zoom_delta`), drop a stroke a stray finger just started (`discard_young`;
    /// the palm arbiter does that itself), and turn quick taps into undo (two
    /// fingers), redo (three) and zen mode (four).
    fn touch_gestures(&mut self, multi: Option<egui::MultiTouchInfo>, rect: Rect, discard_young: bool) {
        let now = self.started.elapsed().as_secs_f64();
        match multi {
            Some(mt) => {
                if discard_young && self.stroke.as_ref().is_some_and(|s| s.age() < 0.3) {
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
        StrokePoint::new(p.x, p.y, self.tablet.pressure()).with_tilt(self.tablet.tilt()).with_azimuth(self.tablet.azimuth()).at_time(self.started.elapsed().as_secs_f32())
    }

    /// The primary button, or the palm arbiter's ink contact, goes down at
    /// window point `pos`; `p` is the sample on the canvas.
    fn press_at(&mut self, pos: Pos2, p: StrokePoint, modifiers: Modifiers) {
        let at = Pos2::new(p.x, p.y);
        if self.tool == Tool::Brush && modifiers.shift {
            // Shift: a straight line (see `crate::lines`).
            self.begin_line(p);
        } else if self.tool == Tool::Brush {
            self.begin_stroke(p);
            self.smart_begin(p, pos);
        } else if self.tool == Tool::Stamp(crate::stamp_tool::StampKind::Clone) && modifiers.alt {
            self.set_clone_source(at);
        } else if self.tool.paints() {
            self.begin_stroke(p);
        } else {
            self.tool_press(at, modifiers);
        }
    }

    /// Feeds this frame's touches to the palm arbiter (when palm rejection is
    /// on) and promotes fingers past their grace period. True when touches
    /// drive the canvas through the arbiter this frame.
    fn palm_touches(&mut self, ui: &egui::Ui, events: &[Event], rect: Rect) -> bool {
        let mode = self.ws.palm.effective_mode(crate::tablet::contact_kinds_known());
        if mode == PalmMode::Off {
            if self.palm.engaged() {
                self.palm = Default::default();
            }
            return false;
        }
        let settings = PalmSettings { mode, ..self.ws.palm };
        let ctx = ui.ctx();
        let now = self.started.elapsed().as_secs_f64();
        if let Some(pos) = crate::tablet::pen_hover(ctx) {
            self.palm.hover(pos, now);
        }
        let gizmo = self.nav_gizmo_rect(rect);
        let mut touched = false;
        for event in events {
            let Event::Touch { id, phase, pos, force, .. } = event else { continue };
            // The system lifted it as a palm: roll its stroke back.
            let phase = crate::tablet::contact_phase(*phase, || crate::tablet::touch_revoked(id.0));
            // Only touches that land on the canvas (not on a panel, a menu
            // or the navigation gizmo over it) are the canvas's.
            let on_canvas = rect.contains(*pos)
                && ctx.layer_id_at(*pos).is_none_or(|layer| layer == ui.layer_id())
                && !gizmo.is_some_and(|g| g.contains(*pos));
            if phase == ContactPhase::Start && !on_canvas {
                continue;
            }
            if phase != ContactPhase::Start && self.palm.class_of(id.0).is_none() {
                continue;
            }
            touched = true;
            let (kind, major_mm) = crate::tablet::touch_contact(id.0, *force);
            let pressure = force.map_or(1.0, |f| f.clamp(0.0, 1.0));
            self.palm.feed(ContactInput { id: id.0, phase, pos: *pos, time: now, kind, pressure, major_mm }, &settings);
        }
        self.palm.tick(now, &settings);
        if self.palm.pending() {
            // Nothing else wakes the app while a staged finger rests still.
            ctx.request_repaint_after(std::time::Duration::from_millis(8));
        }
        touched || self.palm.engaged()
    }

    /// Strokes from the palm arbiter's ink contact: it presses, moves and
    /// releases like the primary button; a cancelled stroke is rolled back.
    /// `blocked`: navigation or brush resizing has the pointer.
    fn palm_ink(&mut self, rect: Rect, blocked: bool, long_press: bool) {
        let doc_size = self.doc_size();
        for event in self.palm.take_ink() {
            match event {
                InkEvent::Begin(s) => {
                    if blocked || self.stroke.is_some() || self.drag.is_some() || self.line.is_some() {
                        continue;
                    }
                    let p = self.ink_point(rect, doc_size, s);
                    let mods = self.pointer_mods;
                    if mods.alt && !mods.ctrl && self.alt_eyedropper() {
                        self.begin_temporary_pick(Pos2::new(p.x, p.y));
                    } else {
                        self.press_at(s.pos, p, mods);
                    }
                }
                InkEvent::Move(s) => {
                    let p = self.ink_point(rect, doc_size, s);
                    if self.eyedropper_held() && !long_press {
                        self.eyedrop_at(Pos2::new(p.x, p.y));
                    } else if self.line.is_some() {
                        let snap = self.pointer_mods.command || self.pointer_mods.ctrl;
                        self.move_line(p, snap);
                    } else if self.stroke.is_some() {
                        self.continue_stroke(p);
                    } else if self.drag.is_some() {
                        self.tool_move(Pos2::new(p.x, p.y));
                    }
                }
                InkEvent::End | InkEvent::Cancel => {
                    if event == InkEvent::Cancel {
                        if self.stroke.is_some() {
                            // A palm, or the start of a gesture: as if it never touched.
                            self.smart = None;
                            self.discard_stroke();
                        }
                        if self.eyedropper_held() && !long_press {
                            self.end_pick();
                        }
                        self.line = None;
                        self.smart = None;
                        self.drag = None;
                    } else {
                        if self.eyedropper_held() && !long_press {
                            self.end_pick();
                        }
                        if self.line.is_some() {
                            self.finish_line();
                        }
                        self.smart_release();
                        if self.stroke.is_some() {
                            self.end_stroke();
                        }
                        if self.drag.is_some() {
                            self.tool_release();
                        }
                    }
                }
            }
        }
    }

    /// An ink sample on the canvas; the pen's tilt applies to the pen only.
    fn ink_point(&self, rect: Rect, doc_size: Vec2, s: InkSample) -> StrokePoint {
        let p = self.view.screen_to_doc(rect, doc_size, s.pos);
        let (tilt, azimuth) = if s.pen { (self.tablet.tilt(), self.tablet.azimuth()) } else { (0.0, 0.0) };
        StrokePoint::new(p.x, p.y, s.pressure).with_tilt(tilt).with_azimuth(azimuth).at_time(s.time as f32)
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
            let mut uniform = thumb.uniform(rect.size(), doc);
            // Duotone shows its inks here too (its display table is in place for the canvas).
            if self.doc.mode == painter_core::color_mode::ColorMode::Duotone && self.proofer.display.is_some() {
                uniform = uniform.with_display_lut();
            }
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

/// How a primary drag moves the view instead of using the tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Nav {
    Pan,
    Rotate,
    Zoom { out: bool },
}
