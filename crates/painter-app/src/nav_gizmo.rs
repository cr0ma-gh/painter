//! The navigation gizmo of a 3D layer (as in Blender's viewport): in the
//! canvas's top-right corner, the scene's X, Y and Z axes as the camera sees
//! them, so you always know which way you're facing. Click an axis ball to
//! look along it (X: from the right, −Y: from the front, Z: from the top…);
//! drag on the gizmo to orbit.

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use crate::i18n::tr;
use scene_3d::camera::ViewPreset;
use scene_3d::math::Vec3;

use crate::app::PainterApp;

/// The gizmo's radius, in screen points.
const RADIUS: f32 = 42.0;
const BALL: f32 = 9.0;

/// One end of an axis on the gizmo.
struct AxisEnd {
    at: Pos2,
    /// Towards the viewer: drawn on top.
    depth: f32,
    color: Color32,
    label: Option<&'static str>,
    view: ViewPreset,
}

/// The six axis ends around `centre` for a camera with these axes.
fn axis_ends(centre: Pos2, right: Vec3, up: Vec3, forward: Vec3) -> Vec<AxisEnd> {
    let axes = [
        (Vec3::X, Color32::from_rgb(232, 72, 85), "X", ViewPreset::Right, ViewPreset::Left),
        (Vec3::Y, Color32::from_rgb(120, 190, 50), "Y", ViewPreset::Back, ViewPreset::Front),
        (Vec3::Z, Color32::from_rgb(64, 144, 240), "Z", ViewPreset::Top, ViewPreset::Bottom),
    ];
    let mut ends = Vec::new();
    for (axis, color, label, plus, minus) in axes {
        for (sign, view) in [(1.0, plus), (-1.0, minus)] {
            let a = axis * sign;
            let at = centre + Vec2::new(a.dot(right), -a.dot(up)) * (RADIUS - BALL);
            let faded = if sign > 0.0 { color } else { color.gamma_multiply(0.55) };
            ends.push(AxisEnd { at, depth: -a.dot(forward), color: faded, label: (sign > 0.0).then_some(label), view });
        }
    }
    // Far ones first, so the near ones cover them.
    ends.sort_by(|a, b| a.depth.total_cmp(&b.depth));
    ends
}

impl PainterApp {
    /// Where the gizmo is on screen, when it shows (the 3D tool on a 3D layer).
    pub(crate) fn nav_gizmo_rect(&self, canvas: Rect) -> Option<Rect> {
        if self.tool != crate::tools::Tool::Scene3D || !self.scenes.contains_key(&self.scene_target()) {
            return None;
        }
        let centre = Pos2::new(canvas.right() - RADIUS - 12.0, canvas.top() + RADIUS + 12.0);
        Some(Rect::from_center_size(centre, Vec2::splat(RADIUS * 2.0)))
    }

    /// Draws the gizmo over the canvas and handles clicks and drags on it.
    pub(crate) fn nav_gizmo(&mut self, ui: &mut egui::Ui, canvas: Rect) {
        let id = self.scene_target();
        let Some(area) = self.nav_gizmo_rect(canvas) else { return };
        let Some(layer) = self.scenes.get(&id) else { return };
        let cam = &layer.scene.camera;
        let centre = area.center();
        let response = ui.interact(area, ui.id().with("nav_gizmo"), Sense::click_and_drag());
        let ends = axis_ends(centre, cam.right(), cam.up(), cam.forward());
        let painter = ui.painter_at(area.expand(2.0));
        let backdrop = if response.hovered() || response.dragged() { 60 } else { 30 };
        painter.circle_filled(centre, RADIUS, Color32::from_white_alpha(backdrop));
        let pointer = response.hover_pos();
        let mut hovered_end = None;
        for (k, end) in ends.iter().enumerate() {
            if end.label.is_some() {
                painter.line_segment([centre, end.at], Stroke::new(2.0_f32, end.color));
            }
            let over = pointer.is_some_and(|p| p.distance(end.at) <= BALL);
            if over {
                hovered_end = Some(k);
            }
            let r = if end.label.is_some() { BALL } else { BALL * 0.8 };
            painter.circle_filled(end.at, r, end.color);
            if over {
                painter.circle_stroke(end.at, r + 1.0, Stroke::new(1.5_f32, Color32::WHITE));
            }
            if let Some(label) = end.label {
                painter.text(end.at, Align2::CENTER_CENTER, label, FontId::proportional(11.0), Color32::from_black_alpha(220));
            }
        }
        let response = response.on_hover_text(tr("Click an axis to look along it; drag to orbit"));
        let mut changed = false;
        if let Some(layer) = self.scenes.get_mut(&id) {
            let cam = &mut layer.scene.camera;
            if response.dragged() {
                let d = response.drag_delta();
                cam.orbit(d.x, d.y);
                changed = d != Vec2::ZERO;
            } else if response.clicked() {
                if let Some(k) = hovered_end {
                    cam.set_view(ends[k].view);
                    changed = true;
                }
            }
        }
        if changed {
            self.scene_changed(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axes_show_as_the_camera_sees_them() {
        // Front view: X to the right, Z up, Y into the screen (away, so drawn first).
        let mut cam = scene_3d::camera::ViewCamera::default();
        cam.set_view(ViewPreset::Front);
        let c = Pos2::new(100.0, 100.0);
        let ends = axis_ends(c, cam.right(), cam.up(), cam.forward());
        let find = |l: &str| ends.iter().find(|e| e.label == Some(l)).unwrap();
        assert!(find("X").at.x > c.x + 20.0 && (find("X").at.y - c.y).abs() < 1e-3);
        assert!(find("Z").at.y < c.y - 20.0);
        assert!((find("Y").at - c).length() < 1e-3, "Y points straight away");
        assert_eq!(find("X").view, ViewPreset::Right);
        // The nearest end (−Y, towards the viewer) is drawn last.
        assert_eq!(ends.last().unwrap().view, ViewPreset::Front);
    }
}
