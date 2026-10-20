//! Interactive pressure-curve editor.
//!
//! Drag points to reshape the curve, double-click empty space to add a point,
//! right-click a point to remove it. The end points stay at x = 0 and x = 1.

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use painter_core::PressureCurve;

const HANDLE: f32 = 5.0;

/// Returns whether the curve changed.
pub fn curve_editor(ui: &mut egui::Ui, id_salt: &str, curve: &mut PressureCurve) -> bool {
    let width = ui.available_width().clamp(120.0, 260.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, width * 0.6), Sense::click());
    let painter = ui.painter_at(rect.expand(HANDLE));
    let plot = rect.shrink(HANDLE);
    let to_screen = |p: [f32; 2]| Pos2::new(plot.left() + p[0] * plot.width(), plot.bottom() - p[1] * plot.height());
    let to_curve =
        |s: Pos2| [((s.x - plot.left()) / plot.width()).clamp(0.0, 1.0), ((plot.bottom() - s.y) / plot.height()).clamp(0.0, 1.0)];

    painter.rect_filled(rect, 3.0, Color32::from_gray(22));
    for i in 1..4 {
        let t = i as f32 / 4.0;
        let grid = Stroke::new(1.0_f32, Color32::from_gray(40));
        painter.line_segment([to_screen([t, 0.0]), to_screen([t, 1.0])], grid);
        painter.line_segment([to_screen([0.0, t]), to_screen([1.0, t])], grid);
    }

    let mut points = curve.points().to_vec();
    let original = points.clone();
    let mut changed = false;
    let last = points.len() - 1;
    let mut remove = None;
    for (i, point) in points.iter_mut().enumerate() {
        // Keep points in order so handles don't swap identity mid-drag.
        let (lo, hi) = match i {
            0 => (0.0, 0.0),
            i if i == last => (1.0, 1.0),
            i => (original[i - 1][0] + 0.01, original[i + 1][0] - 0.01),
        };
        let id = ui.make_persistent_id((id_salt, i));
        let handle = Rect::from_center_size(to_screen(*point), Vec2::splat(HANDLE * 2.5));
        let handle_response = ui.interact(handle, id, Sense::click_and_drag());
        if handle_response.dragged() {
            if let Some(pos) = handle_response.interact_pointer_pos() {
                let [x, y] = to_curve(pos);
                point[0] = x.clamp(lo, hi.max(lo));
                point[1] = y;
                changed = true;
            }
        }
        if handle_response.secondary_clicked() && i != 0 && i != last {
            remove = Some(i);
        }
        let hot = handle_response.hovered() || handle_response.dragged();
        painter.circle_filled(to_screen(*point), if hot { HANDLE } else { HANDLE - 1.5 }, Color32::WHITE);
    }
    if let Some(i) = remove {
        points.remove(i);
        changed = true;
    }
    if response.double_clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            points.push(to_curve(pos));
            changed = true;
        }
    }
    if changed {
        *curve = PressureCurve::from_points(points);
    }

    let line: Vec<Pos2> = (0..=48)
        .map(|i| {
            let x = i as f32 / 48.0;
            to_screen([x, curve.eval(x)])
        })
        .collect();
    painter.add(egui::Shape::line(line, Stroke::new(1.5_f32, Color32::from_rgb(110, 170, 255))));
    changed
}
