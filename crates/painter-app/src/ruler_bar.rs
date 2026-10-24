//! The ruler bar (View → Ruler Bar, Ctrl+R; as in Clip Studio): measuring
//! strips along the canvas's top and left edges in pixels or millimetres,
//! with the pointer's position marked. Dragging out of a strip pulls a guide
//! onto the canvas; dropping it back on the strip removes it.

use eframe::egui::{self, Align2, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use crate::i18n::tr;
use painter_core::rulers::{Ruler, RulerKind, RulerScope};

use crate::app::PainterApp;
use crate::theme;

/// The strips' thickness, in points.
pub(crate) const BAR: f32 = 18.0;

/// A guide being pulled out of a strip: its index in the rulers.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GuidePull {
    pub ruler: usize,
}

/// The tick step (in canvas units) that leaves at least `min_gap` points
/// between labelled ticks at `points_per_unit`: 1, 2 or 5 × a power of ten.
pub(crate) fn tick_step(points_per_unit: f32, min_gap: f32) -> f32 {
    let raw = min_gap / points_per_unit.max(1e-6);
    let base = 10f32.powf(raw.log10().floor());
    [1.0, 2.0, 5.0, 10.0].into_iter().map(|k| base * k).find(|s| *s >= raw * 0.999).unwrap_or(base * 10.0)
}

impl PainterApp {
    /// Splits the canvas area for the ruler bar: (canvas, top strip, left
    /// strip), the strips empty when it's off.
    pub(crate) fn ruler_bar_split(&self, full: Rect) -> (Rect, Rect, Rect) {
        if !self.ws.chrome.ruler_bar {
            return (full, Rect::NOTHING, Rect::NOTHING);
        }
        let canvas = Rect::from_min_max(full.min + Vec2::splat(BAR), full.max);
        let top = Rect::from_min_max(Pos2::new(canvas.left(), full.top()), Pos2::new(full.right(), canvas.top()));
        let left = Rect::from_min_max(Pos2::new(full.left(), canvas.top()), Pos2::new(canvas.left(), full.bottom()));
        (canvas, top, left)
    }

    /// Draws the strips and handles guides pulled out of them.
    pub(crate) fn ruler_bar_ui(&mut self, ui: &mut egui::Ui, full: Rect, canvas: Rect, top: Rect, left: Rect) {
        if !self.ws.chrome.ruler_bar {
            return;
        }
        let painter = ui.painter_at(full);
        let corner_rect = Rect::from_min_max(full.min, canvas.min);
        painter.rect_filled(corner_rect, 0.0, theme::panel());
        let mm = self.ws.chrome.ruler_bar_mm;
        // Canvas pixels per unit shown.
        let unit = if mm { self.doc.dpi.max(1) as f32 / 25.4 } else { 1.0 };
        let doc_size = self.doc_size();
        let view = self.view;
        let to_doc = |p: Pos2| view.screen_to_doc(canvas, doc_size, p);
        let hover = ui.input(|i| i.pointer.hover_pos());
        let dim = theme::text_dim();
        for (strip, horizontal) in [(top, true), (left, false)] {
            painter.rect_filled(strip, 0.0, theme::panel());
            let edge = if horizontal { strip.bottom() } else { strip.right() };
            let line = |a: Pos2, b: Pos2| {
                painter.line_segment([a, b], Stroke::new(1.0_f32, dim));
            };
            if horizontal {
                line(Pos2::new(strip.left(), edge - 0.5), Pos2::new(strip.right(), edge - 0.5));
            } else {
                line(Pos2::new(edge - 0.5, strip.top()), Pos2::new(edge - 0.5, strip.bottom()));
            }
            // Along the strip the canvas coordinate changes linearly.
            let (s0, s1) = if horizontal { (canvas.left(), canvas.right()) } else { (canvas.top(), canvas.bottom()) };
            let value = |s: f32| {
                let d = to_doc(if horizontal { Pos2::new(s, canvas.top()) } else { Pos2::new(canvas.left(), s) });
                (if horizontal { d.x } else { d.y }) / unit
            };
            let (v0, v1) = (value(s0), value(s1));
            if (v1 - v0).abs() < 1e-3 {
                continue; // A quarter turn: this edge runs across the other axis.
            }
            let per_point = (v1 - v0) / (s1 - s0);
            let step = tick_step(1.0 / per_point.abs(), 60.0);
            let minor = step / 10.0;
            let (lo, hi) = (v0.min(v1), v0.max(v1));
            let mut k = (lo / minor).floor() as i64;
            while (k as f32) * minor <= hi {
                let v = k as f32 * minor;
                let s = s0 + (v - v0) / per_point;
                let len = if k % 10 == 0 {
                    BAR * 0.6
                } else if k % 5 == 0 {
                    BAR * 0.35
                } else {
                    BAR * 0.2
                };
                if horizontal {
                    line(Pos2::new(s, edge - len), Pos2::new(s, edge));
                } else {
                    line(Pos2::new(edge - len, s), Pos2::new(edge, s));
                }
                if k % 10 == 0 {
                    let text = format!("{}", v.round() as i64);
                    let font = FontId::proportional(9.0);
                    if horizontal {
                        painter.text(Pos2::new(s + 2.0, strip.top() + 1.0), Align2::LEFT_TOP, text, font, dim);
                    } else {
                        // Digits stacked: the strip is narrow.
                        let stacked: Vec<String> = text.chars().map(String::from).collect();
                        painter.text(Pos2::new(strip.left() + 3.0, s + 2.0), Align2::LEFT_TOP, stacked.join("\n"), font, dim);
                    }
                }
                k += 1;
            }
            // The pointer's position.
            if let Some(p) = hover.filter(|p| canvas.contains(*p)) {
                let mark = Stroke::new(1.0_f32, theme::accent());
                if horizontal {
                    painter.line_segment([Pos2::new(p.x, strip.top()), Pos2::new(p.x, edge)], mark);
                } else {
                    painter.line_segment([Pos2::new(strip.left(), p.y), Pos2::new(edge, p.y)], mark);
                }
            }
        }
        // The corner switches units.
        let corner = ui.interact(corner_rect, ui.id().with("ruler_bar_corner"), Sense::click());
        painter.text(corner_rect.center(), Align2::CENTER_CENTER, if mm { "mm" } else { "px" }, FontId::proportional(8.0), dim);
        if corner.on_hover_text(tr("Ruler bar units: click to switch pixels / millimetres")).clicked() {
            self.ws.chrome.ruler_bar_mm = !mm;
        }
        // Pulling guides out.
        for (strip, horizontal) in [(top, true), (left, false)] {
            let cursor = if horizontal { egui::CursorIcon::ResizeVertical } else { egui::CursorIcon::ResizeHorizontal };
            let response = ui.interact(strip, ui.id().with(("ruler_bar", horizontal)), Sense::drag()).on_hover_cursor(cursor);
            if response.drag_started() {
                self.checkpoint();
                let kind = RulerKind::Guide { vertical: !horizontal, position: 0.0 };
                // Guides belong to the document, for every layer.
                self.doc.rulers.rulers.push(Ruler { scope: RulerScope::AllLayers, ..Ruler::new(kind) });
                self.guide_pull = Some(GuidePull { ruler: self.doc.rulers.rulers.len() - 1 });
            }
            if response.dragged() {
                if let (Some(pull), Some(p)) = (self.guide_pull, response.interact_pointer_pos()) {
                    let d = to_doc(p);
                    if let Some(Ruler { kind: RulerKind::Guide { vertical, position }, .. }) =
                        self.doc.rulers.rulers.get_mut(pull.ruler)
                    {
                        *position = if *vertical { d.x.round() } else { d.y.round() };
                    }
                    self.dirty = true;
                }
            }
            if response.drag_stopped() {
                if let Some(pull) = self.guide_pull.take() {
                    // Dropped back on the bar: no guide.
                    let back = response.interact_pointer_pos().or(hover).is_some_and(|p| !canvas.contains(p));
                    if back && pull.ruler < self.doc.rulers.rulers.len() {
                        self.doc.rulers.rulers.remove(pull.ruler);
                    } else {
                        self.status = tr("Guide added: move it with the Ruler tool, or drag it back onto the ruler bar").into();
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_are_round_and_far_enough_apart() {
        // 1 point per pixel: a label every 100 px.
        assert_eq!(tick_step(1.0, 60.0), 100.0);
        // Zoomed in 8×: every 10 px.
        assert_eq!(tick_step(8.0, 60.0), 10.0);
        // Zoomed out to 5 %: every 2000 px.
        assert_eq!(tick_step(0.05, 60.0), 2000.0);
        assert_eq!(tick_step(3.0, 60.0), 20.0);
    }
}
