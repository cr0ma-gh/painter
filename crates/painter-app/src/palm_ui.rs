//! Preferences > Input & Tablet (palm rejection) and its diagnostic overlay
//! on the canvas. The rejection itself is `crate::palm`.

use eframe::egui::{self, Color32, Rect, Shape, Stroke};

use crate::app::PainterApp;
use crate::i18n::tr;
use crate::palm::{ContactClass, ContactKind, Handedness, PalmMode};

/// Contact sizes the sensitivity sliders offer, millimetres.
const CONTACT_RANGE: std::ops::RangeInclusive<f32> = 0.5..=40.0;

impl PainterApp {
    /// Preferences > Input & Tablet: palm rejection mode, drawing hand,
    /// contact size sensitivity and the diagnostic overlay.
    pub(crate) fn palm_window(&mut self, ctx: &egui::Context) {
        if !self.palm_open {
            return;
        }
        let mut open = true;
        let before = self.ws.palm;
        egui::Window::new(tr("Input & Tablet")).open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.set_max_width(400.0);
            let palm = &mut self.ws.palm;
            ui.strong(tr("Palm Rejection"));
            for mode in PalmMode::ALL {
                let hint = match mode {
                    PalmMode::Standard => tr("The pen paints at once; a finger paints after a short check. The system's pen and finger signals and the contact size keep the resting hand from painting."),
                    PalmMode::StylusOnly => tr("Only the pen paints; one finger pans and more fingers zoom, rotate, undo and redo."),
                    PalmMode::Off => tr("Every touch paints, for painting with your fingers without a pen."),
                };
                ui.radio_value(&mut palm.mode, mode, tr(mode.label())).on_hover_text(hint);
            }
            if palm.mode == PalmMode::StylusOnly && !crate::tablet::CONTACT_KINDS_KNOWN {
                ui.small(tr("This device doesn't tell the pen from a finger: Standard rejection is used instead."));
            }
            ui.add_enabled_ui(palm.mode != PalmMode::Off, |ui| {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(tr("Drawing hand"));
                    ui.selectable_value(&mut palm.hand, Handedness::Right, tr("Right-handed"));
                    ui.selectable_value(&mut palm.hand, Handedness::Left, tr("Left-handed"));
                })
                .response
                .on_hover_text(tr("Touches below the pen tip on this side are the hand that holds it"));
                ui.separator();
                ui.label(tr("Contact size (fingertip, mm)"));
                ui.add(egui::Slider::new(&mut palm.min_contact_mm, CONTACT_RANGE).text(tr("Smallest")))
                    .on_hover_text(tr("Smaller contacts are ignored as noise"));
                ui.add(egui::Slider::new(&mut palm.max_contact_mm, CONTACT_RANGE).text(tr("Largest")))
                    .on_hover_text(tr("Wider contacts are a palm. Raise it for matte (paper-like) films or thick glass protectors."));
                if palm.max_contact_mm < palm.min_contact_mm {
                    palm.max_contact_mm = palm.min_contact_mm;
                }
                ui.add(egui::Slider::new(&mut palm.grace_ms, 50.0..=80.0).text(tr("Finger delay (ms)")))
                    .on_hover_text(tr("How long a finger waits before it paints, so a palm landing just before the pen leaves no mark"));
                ui.separator();
                ui.checkbox(&mut palm.diagnostics, tr("Show touch classification (developer)"))
                    .on_hover_text(tr("On the canvas: green for the pen, blue for gestures, shaded red for a rejected palm, amber while a finger waits"));
            });
            if ui.button(tr("Restore Defaults")).clicked() {
                *palm = Default::default();
            }
        });
        // "Draw With the Pen Only" follows the mode.
        if self.ws.palm.mode != before.mode {
            self.tools.pencil_only = self.ws.palm.pencil_only(self.tools.pencil_only);
        }
        if !open {
            self.palm_open = false;
        }
    }

    /// The palm arbiter's view of the touches on the canvas, when the
    /// diagnostic overlay is on: each contact in its class's colour, and the
    /// cone where touches are taken for the drawing hand.
    pub(crate) fn palm_overlay(&self, ui: &egui::Ui, rect: Rect) {
        if !self.ws.palm.diagnostics || self.ws.palm.mode == PalmMode::Off {
            return;
        }
        let painter = ui.painter().with_clip_rect(rect);
        let now = self.started.elapsed().as_secs_f64();
        if let Some((tip, hand)) = self.palm.cone(now, &self.ws.palm) {
            let cone = crate::palm::cone_outline(tip, hand);
            painter.add(Shape::closed_line(cone, Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(230, 60, 60, 140))));
        }
        for (pos, kind, class) in self.palm.contacts() {
            let color = match class {
                ContactClass::Ink if kind == ContactKind::Stylus || kind == ContactKind::Eraser => Color32::from_rgb(40, 200, 90),
                ContactClass::Ink => Color32::from_rgb(140, 220, 90),
                ContactClass::Navigation => Color32::from_rgb(60, 130, 240),
                ContactClass::Pending => Color32::from_rgb(240, 180, 40),
                ContactClass::Palm => Color32::from_rgb(230, 60, 60),
            };
            let fill = if class == ContactClass::Palm { color.gamma_multiply(0.35) } else { Color32::TRANSPARENT };
            painter.circle(pos, 22.0, fill, Stroke::new(2.0_f32, color));
        }
        if self.palm.engaged() {
            ui.ctx().request_repaint();
        }
    }
}
