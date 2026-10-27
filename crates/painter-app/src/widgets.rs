//! Compact panel widgets: property sliders, segmented choices, the colour
//! wheel and the main / sub colour chips.

use std::ops::RangeInclusive;
use crate::i18n::tr;

use eframe::egui::{
    self, ecolor::Hsva, ecolor::HsvaGamma, epaint::Mesh, Color32, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2,
};

use crate::theme;

/// A Clip Studio style property row: the name and an editable value on one
/// line, a thin bar below that can be dragged to set the value.
pub fn prop_slider(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    range: RangeInclusive<f32>,
    suffix: &str,
    decimals: usize,
    logarithmic: bool,
) -> bool {
    prop_row(ui, label, value, range, suffix, decimals, logarithmic, None)
}

/// [`prop_slider`] with a reset button at the end of the row that puts the
/// value back to `default` (dimmed while the value is already there).
#[allow(clippy::too_many_arguments)]
pub fn prop_slider_reset(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    default: f32,
    range: RangeInclusive<f32>,
    suffix: &str,
    decimals: usize,
    logarithmic: bool,
) -> bool {
    prop_row(ui, label, value, range, suffix, decimals, logarithmic, Some(default))
}

/// [`prop_percent`] with a reset button back to `default` (a 0..1 value).
pub fn prop_percent_reset(ui: &mut Ui, label: &str, value: &mut f32, default: f32, range: RangeInclusive<f32>) -> bool {
    let mut pct = *value * 100.0;
    let changed = prop_slider_reset(ui, label, &mut pct, default * 100.0, range, "%", 0, false);
    if changed {
        *value = pct / 100.0;
    }
    changed
}

#[allow(clippy::too_many_arguments)]
fn prop_row(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    range: RangeInclusive<f32>,
    suffix: &str,
    decimals: usize,
    logarithmic: bool,
    default: Option<f32>,
) -> bool {
    let label = crate::i18n::tr_str(label);
    let (lo, hi) = (*range.start(), *range.end());
    let to_t = |v: f32| {
        if logarithmic {
            ((v.max(lo) / lo).ln() / (hi / lo).ln()).clamp(0.0, 1.0)
        } else {
            ((v - lo) / (hi - lo)).clamp(0.0, 1.0)
        }
    };
    let from_t = |t: f32| {
        if logarithmic {
            lo * (hi / lo).powf(t)
        } else {
            lo + (hi - lo) * t
        }
    };

    // One row: the name, a bar to drag, the value to type.
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let row_h = 18.0;
        let full = ui.available_width();
        let label_w = (full * 0.38).clamp(56.0, 120.0);
        let (label_rect, label_response) = ui.allocate_exact_size(Vec2::new(label_w, row_h), Sense::hover());
        let font = egui::FontId::proportional(12.0);
        let galley = ui.painter().layout(label.to_string(), font, theme::text(), label_w);
        let shown = ui.painter().with_clip_rect(label_rect);
        shown.galley(label_rect.left_center() - Vec2::new(0.0, galley.size().y / 2.0), galley.clone(), theme::text());
        if galley.size().x > label_w {
            label_response.on_hover_text(label);
        }
        let value_w = 58.0;
        let reset_w = if default.is_some() { 20.0 } else { 0.0 };
        let bar_w = (ui.available_width() - value_w - reset_w - 4.0).max(24.0);
        let (rect, response) = ui.allocate_exact_size(Vec2::new(bar_w, row_h), Sense::click_and_drag());
        let bar = Rect::from_center_size(rect.center(), Vec2::new(rect.width(), 6.0));
        if let Some(pos) = response.interact_pointer_pos() {
            if response.dragged() || response.clicked() {
                let t = ((pos.x - bar.left()) / bar.width()).clamp(0.0, 1.0);
                let new = from_t(t);
                if (new - *value).abs() > f32::EPSILON {
                    *value = new;
                    changed = true;
                }
            }
        }
        let painter = ui.painter();
        painter.rect_filled(bar, 1.0, theme::well());
        let filled = Rect::from_min_size(bar.min, Vec2::new(bar.width() * to_t(*value), bar.height()));
        let fill = if response.hovered() || response.dragged() { theme::accent() } else { Color32::from_gray(120) };
        painter.rect_filled(filled, 1.0, fill);
        let drag = egui::DragValue::new(value).range(range.clone()).suffix(suffix).max_decimals(decimals).speed((hi - lo) / 400.0);
        changed |= ui.add_sized([value_w, row_h], drag).changed();
        if let Some(default) = default {
            let differs = (*value - default).abs() > 1e-4 * default.abs().max(1.0);
            if crate::brush_ui::reset_glyph(ui, differs, tr("Back to the preset's value")).clicked() {
                *value = default;
                changed = true;
            }
        }
    });
    changed
}

/// [`prop_slider`] for a 0..1 value shown as a percentage.
pub fn prop_percent(ui: &mut Ui, label: &str, value: &mut f32, range: RangeInclusive<f32>) -> bool {
    let mut pct = *value * 100.0;
    let changed = prop_slider(ui, label, &mut pct, range, "%", 0, false);
    if changed {
        *value = pct / 100.0;
    }
    changed
}

/// A row of equal cells, one selected; `paint` draws each cell's content.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    value: &mut T,
    options: &[(T, &str)],
    cell: Vec2,
    paint: impl Fn(&egui::Painter, Rect, T, bool),
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 1.0;
        for (option, hint) in options {
            let (rect, response) = ui.allocate_exact_size(cell, Sense::click());
            let selected = *value == *option;
            let fill = if selected {
                theme::selected()
            } else if response.hovered() {
                theme::hover()
            } else {
                theme::title()
            };
            ui.painter().rect_filled(rect, 2.0, fill);
            paint(ui.painter(), rect, *option, selected);
            if response.on_hover_text(crate::i18n::tr_str(hint)).clicked() && !selected {
                *value = *option;
                changed = true;
            }
        }
    });
    changed
}

/// Hue ring around a saturation / value square. Edits `color` in gamma space
/// so the square looks even. Returns whether it changed.
pub fn color_wheel(ui: &mut Ui, color: &mut Hsva, size: f32) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click_and_drag());
    let center = rect.center();
    let outer = size * 0.5 - 2.0;
    let inner = outer * 0.80;
    let half = inner * 0.68;
    let square = Rect::from_center_size(center, Vec2::splat(half * 2.0));
    let mut hsv = HsvaGamma::from(*color);
    let id = response.id.with("part");
    let mut changed = false;

    if let Some(pos) = response.interact_pointer_pos() {
        // Where the press landed decides what the drag edits.
        if ui.input(|i| i.pointer.any_pressed()) {
            ui.data_mut(|d| d.insert_temp(id, (pos - center).length() > inner));
        }
        if ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false) {
            let d = pos - center;
            hsv.h = ((-d.y).atan2(d.x) / std::f32::consts::TAU).rem_euclid(1.0);
        } else {
            hsv.s = ((pos.x - square.left()) / square.width()).clamp(0.0, 1.0);
            hsv.v = 1.0 - ((pos.y - square.top()) / square.height()).clamp(0.0, 1.0);
        }
        hsv.a = 1.0;
        *color = Hsva::from(hsv);
        changed = true;
    }

    let painter = ui.painter_at(rect);
    let gamma = |h: f32, s: f32, v: f32| Color32::from(HsvaGamma { h, s, v, a: 1.0 });

    // Ring.
    let mut mesh = Mesh::default();
    let steps = 120;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let angle = t * std::f32::consts::TAU;
        let dir = Vec2::new(angle.cos(), -angle.sin());
        let c = gamma(t, 1.0, 1.0);
        mesh.colored_vertex(center + dir * inner, c);
        mesh.colored_vertex(center + dir * outer, c);
        if i > 0 {
            let b = (i * 2) as u32;
            mesh.add_triangle(b - 2, b - 1, b);
            mesh.add_triangle(b - 1, b + 1, b);
        }
    }
    painter.add(Shape::mesh(mesh));

    // Saturation / value square, bilinear in a grid so it follows the HSV model.
    let mut mesh = Mesh::default();
    let n = 16;
    for j in 0..=n {
        for i in 0..=n {
            let (s, v) = (i as f32 / n as f32, 1.0 - j as f32 / n as f32);
            let p = Pos2::new(square.left() + s * square.width(), square.top() + (1.0 - v) * square.height());
            mesh.colored_vertex(p, gamma(hsv.h, s, v));
            if i > 0 && j > 0 {
                let k = (j * (n + 1) + i) as u32;
                let row = (n + 1) as u32;
                mesh.add_triangle(k - row - 1, k - row, k - 1);
                mesh.add_triangle(k - row, k, k - 1);
            }
        }
    }
    painter.add(Shape::mesh(mesh));
    painter.rect_stroke(square, 0.0, Stroke::new(1.0_f32, theme::line()), StrokeKind::Outside);

    // Markers.
    let angle = hsv.h * std::f32::consts::TAU;
    let dir = Vec2::new(angle.cos(), -angle.sin());
    let mid = center + dir * (inner + outer) * 0.5;
    let marker = Rect::from_center_size(mid, Vec2::splat(outer - inner + 2.0));
    painter.rect_stroke(marker, 2.0, Stroke::new(2.0_f32, Color32::WHITE), StrokeKind::Middle);
    painter.rect_stroke(marker.expand(1.5), 2.0, Stroke::new(1.0_f32, Color32::BLACK), StrokeKind::Middle);
    let sv = Pos2::new(square.left() + hsv.s * square.width(), square.top() + (1.0 - hsv.v) * square.height());
    painter.circle_stroke(sv, 4.5, Stroke::new(1.5_f32, Color32::WHITE));
    painter.circle_stroke(sv, 6.0, Stroke::new(1.0_f32, Color32::BLACK));
    changed
}

/// Overlapping main / sub colour discs; clicking swaps them.
pub fn color_chips(ui: &mut Ui, main: &mut Hsva, sub: &mut Hsva, size: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(size * 1.45, size * 1.3), Sense::click());
    let painter = ui.painter_at(rect.expand(2.0));
    let r = size * 0.5;
    let main_c = rect.left_top() + Vec2::splat(r + 1.0);
    let sub_c = main_c + Vec2::new(r * 0.85, r * 0.6);
    painter.circle_filled(sub_c, r * 0.85, Color32::from(*sub));
    painter.circle_stroke(sub_c, r * 0.85, Stroke::new(1.5_f32, Color32::from_gray(150)));
    painter.circle_filled(main_c, r, Color32::from(*main));
    painter.circle_stroke(main_c, r, Stroke::new(2.0_f32, Color32::from_gray(200)));
    painter.circle_stroke(main_c, r + 1.5, Stroke::new(1.0_f32, Color32::BLACK));
    let response = response.on_hover_text(tr("Main / sub colour. Click or press X to swap; double-click one to pick it"));
    if response.clicked() {
        std::mem::swap(main, sub);
    }
    response
}

/// Draws a header strip like a panel tab: icon, then text.
pub fn group_header(ui: &mut Ui, icon: crate::icons::Icon, text: &str) {
    let text = crate::i18n::tr_str(text);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::hover());
    let tab = Rect::from_min_size(rect.min, Vec2::new(rect.width().min(150.0), rect.height()));
    ui.painter().rect_filled(tab, 3.0, theme::selected());
    let icon_rect = Rect::from_min_size(tab.min + Vec2::new(4.0, 2.0), Vec2::splat(20.0));
    icon.paint(ui, icon_rect, 16.0, Color32::WHITE);
    ui.painter().text(
        Pos2::new(icon_rect.right() + 4.0, tab.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::proportional(13.0),
        Color32::WHITE,
    );
}

/// A grey checkerboard (transparency) filling `rect`, `cell` points per square.
pub fn checkerboard(painter: &egui::Painter, rect: egui::Rect, cell: f32) {
    let painter = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    painter.rect_filled(rect, 0.0, Color32::from_gray(200));
    let (cols, rows) = ((rect.width() / cell).ceil() as usize, (rect.height() / cell).ceil() as usize);
    for j in 0..rows {
        for i in (j % 2..cols).step_by(2) {
            let min = rect.min + Vec2::new(i as f32 * cell, j as f32 * cell);
            painter.rect_filled(egui::Rect::from_min_size(min, Vec2::splat(cell)), 0.0, Color32::from_gray(150));
        }
    }
}

/// Clip Studio's transparent colour chip: a checkerboard square, ringed when in use.
pub fn transparent_chip(ui: &mut Ui, on: bool, size: f32) -> Response {
    let size = size * crate::icons::panel_factor(ui);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    checkerboard(ui.painter(), rect.shrink(2.0), size / 4.0);
    let ring = if on { crate::theme::accent() } else if response.hovered() { crate::theme::strong() } else { crate::theme::line() };
    ui.painter().rect_stroke(rect.shrink(1.0), 2.0, Stroke::new(if on { 2.5_f32 } else { 1.0 }, ring), StrokeKind::Inside);
    response.on_hover_text(tr("Transparent colour: brushes erase with their own shape (pick any colour to go back)"))
}
