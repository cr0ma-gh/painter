//! The eyedropper, after Clip Studio's: it takes the colour shown on the
//! canvas (every visible layer composited) or only the current layer's, as one
//! pixel or the average of a small square. Besides its own tool (I), it comes
//! up while Alt is held with a brush, fill or gradient (Windows), and when a
//! finger rests still on the canvas for half a second (iPad, with "Draw With
//! the Pen Only"). A loupe shows the new colour over the previous one.

use eframe::egui::{self, ecolor::Hsva, Color32, Event, Pos2, Rect, Stroke, TouchPhase, Vec2};
use painter_core::LayerKind;

use crate::app::PainterApp;
use crate::tools::Tool;

/// How long a finger must rest still to pick, and how far it may drift meanwhile.
const LONG_PRESS_SECONDS: f64 = 0.5;
const LONG_PRESS_SLOP: f32 = 10.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PickSource {
    /// The colour you see: all visible layers.
    #[default]
    Canvas,
    /// The current layer only.
    Layer,
}

/// A finger that may become a long press.
#[derive(Clone, Copy, Debug)]
struct LongPress {
    id: egui::TouchId,
    started: f64,
    at: Pos2,
    active: bool,
}

pub(crate) struct Eyedropper {
    pub(crate) source: PickSource,
    /// Side of the averaged square, in canvas pixels (1 = one pixel).
    pub(crate) size: u32,
    /// Picking with Alt or a long press while the pointer is held.
    temporary: bool,
    long_press: Option<LongPress>,
    /// The loupe: where (screen), the picked colour and the one before.
    loupe: Option<(Pos2, [u8; 3], [u8; 3])>,
}

impl Default for Eyedropper {
    fn default() -> Self {
        Self { source: PickSource::Canvas, size: 1, temporary: false, long_press: None, loupe: None }
    }
}

/// Unpremultiplied average of premultiplied RGBA8 pixels; `None` where there's no paint.
fn average(pixels: impl Iterator<Item = [u8; 4]>) -> Option<[u8; 3]> {
    let (mut sum, mut n) = ([0u64; 4], 0u64);
    for p in pixels {
        for i in 0..4 {
            sum[i] += p[i] as u64;
        }
        n += 1;
    }
    if n == 0 || sum[3] * 2 < n {
        return None;
    }
    Some(std::array::from_fn(|i| ((sum[i] * 255 + sum[3] / 2) / sum[3]).min(255) as u8))
}

impl PainterApp {
    /// The colour at canvas point `p` from the chosen source, if there is paint there.
    pub(crate) fn sample_color(&self, p: Pos2) -> Option<[u8; 3]> {
        let (w, h) = self.doc.canvas();
        let (x, y) = (p.x.floor() as i64, p.y.floor() as i64);
        if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
            return None;
        }
        let r = (self.eyedropper.size.clamp(1, 31) / 2) as i64;
        let (x0, y0) = ((x - r).max(0) as u32, (y - r).max(0) as u32);
        let (x1, y1) = ((x + r).min(w as i64 - 1) as u32, (y + r).min(h as i64 - 1) as u32);
        let (rw, rh) = (x1 - x0 + 1, y1 - y0 + 1);
        let from_canvas = || {
            let buf = painter_core::io::flatten_region(&self.doc, x0, y0, rw, rh);
            average(buf.as_chunks::<4>().0.iter().copied())
        };
        match self.eyedropper.source {
            PickSource::Canvas => from_canvas(),
            PickSource::Layer => {
                let layer = self.doc.active_layer()?;
                let coords = (y0..=y1).flat_map(|y| (x0..=x1).map(move |x| (x as i32, y as i32)));
                match &layer.kind {
                    _ if layer.tiles().is_some() => {
                        let tiles = layer.tiles()?;
                        average(coords.map(|(x, y)| tiles.pixel(x, y)))
                    }
                    LayerKind::Fill { color } => Some(*color),
                    LayerKind::Gradient(g) => average(coords.map(|(x, y)| {
                        let c = g.sample(x as f32 + 0.5, y as f32 + 0.5);
                        c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
                    })),
                    LayerKind::Tone(t) => average(coords.map(|(x, y)| {
                        let c = t.sample(x as f32 + 0.5, y as f32 + 0.5);
                        c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
                    })),
                    // Folders and correction layers have no colour of their own: what's shown.
                    _ => from_canvas(),
                }
            }
        }
    }

    /// Picks the colour at canvas point `at` into the main colour.
    pub(crate) fn eyedrop_at(&mut self, at: Pos2) {
        let before = {
            let [r, g, b, _] = self.color.to_srgba_unmultiplied();
            [r, g, b]
        };
        let screen = self.view.doc_to_screen(self.canvas_rect, self.doc_size(), at);
        match self.sample_color(at) {
            Some(c) => {
                self.color = Hsva::from_srgb(c);
                let previous = self.eyedropper.loupe.map_or(before, |l| l.2);
                self.eyedropper.loupe = Some((screen, c, previous));
            }
            None => {
                if let Some(l) = &mut self.eyedropper.loupe {
                    l.0 = screen;
                }
            }
        }
    }

    /// Alt held with a painting tool: the press picks instead of painting.
    pub(crate) fn alt_eyedropper(&self) -> bool {
        self.pointer_mods.alt && matches!(self.tool, Tool::Brush | Tool::Fill | Tool::Gradient) && self.stroke.is_none()
    }

    pub(crate) fn eyedropper_held(&self) -> bool {
        self.eyedropper.temporary
    }

    /// Starts temporary picking at `at` (Alt press, long press).
    pub(crate) fn begin_temporary_pick(&mut self, at: Pos2) {
        self.eyedropper.temporary = true;
        self.eyedropper.loupe = None;
        self.eyedrop_at(at);
    }

    pub(crate) fn end_pick(&mut self) {
        self.eyedropper.temporary = false;
        self.eyedropper.loupe = None;
    }

    /// Fingers (touches without force): a finger resting still becomes the
    /// eyedropper. Returns whether a long press is picking (the caller then
    /// doesn't pan with that finger).
    pub(crate) fn eyedropper_touches(&mut self, ctx: &egui::Context, events: &[Event], rect: Rect) -> bool {
        let now = ctx.input(|i| i.time);
        let doc_size = self.doc_size();
        // Only where a finger doesn't paint.
        if !self.tools.pencil_only {
            self.eyedropper.long_press = None;
            return false;
        }
        for event in events {
            let Event::Touch { id, phase, pos, force, .. } = event else { continue };
            if force.is_some() {
                continue; // a pen
            }
            match phase {
                TouchPhase::Start if rect.contains(*pos) && self.stroke.is_none() => {
                    // A second finger is a gesture, not a long press.
                    self.eyedropper.long_press = match self.eyedropper.long_press {
                        None => Some(LongPress { id: *id, started: now, at: *pos, active: false }),
                        Some(_) => None,
                    };
                }
                TouchPhase::Move => {
                    if let Some(lp) = &mut self.eyedropper.long_press {
                        if lp.id == *id {
                            if lp.active {
                                lp.at = *pos;
                                let at = self.view.screen_to_doc(rect, doc_size, *pos);
                                self.eyedrop_at(at);
                            } else if lp.at.distance(*pos) > LONG_PRESS_SLOP {
                                self.eyedropper.long_press = None;
                            }
                        }
                    }
                }
                TouchPhase::End | TouchPhase::Cancel if self.eyedropper.long_press.is_some_and(|lp| lp.id == *id) => {
                    if self.eyedropper.long_press.is_some_and(|lp| lp.active) {
                        self.end_pick();
                    }
                    self.eyedropper.long_press = None;
                }
                _ => {}
            }
        }
        if let Some(lp) = self.eyedropper.long_press {
            if !lp.active {
                if now - lp.started >= LONG_PRESS_SECONDS {
                    if let Some(l) = &mut self.eyedropper.long_press {
                        l.active = true;
                    }
                    let at = self.view.screen_to_doc(rect, doc_size, lp.at);
                    self.begin_temporary_pick(at);
                } else {
                    // Nothing else wakes the app while a finger rests still.
                    ctx.request_repaint_after(std::time::Duration::from_millis(50));
                }
            }
        }
        self.eyedropper.long_press.is_some_and(|lp| lp.active)
    }

    /// The loupe: a ring with the new colour on top and the previous one below.
    pub(crate) fn draw_eyedropper(&self, painter: &egui::Painter) {
        let picking = self.eyedropper.temporary || (self.tool == Tool::Eyedropper && self.drag.is_some());
        let Some((at, new, old)) = self.eyedropper.loupe.filter(|_| picking) else { return };
        // Above the finger or pen, so it isn't hidden under it.
        let centre = at - Vec2::new(0.0, 70.0);
        let (outer, inner) = (46.0, 30.0);
        let ring = |from: f32, to: f32, color: [u8; 3]| {
            let n = 40;
            let mut points = Vec::with_capacity(2 * n + 2);
            for k in 0..=n {
                let a = from + (to - from) * k as f32 / n as f32;
                points.push(centre + Vec2::angled(a) * outer);
            }
            for k in (0..=n).rev() {
                let a = from + (to - from) * k as f32 / n as f32;
                points.push(centre + Vec2::angled(a) * inner);
            }
            painter.add(egui::Shape::convex_polygon(points, Color32::from_rgb(color[0], color[1], color[2]), Stroke::NONE));
        };
        // Drawn as thin slices so each stays convex.
        let pi = std::f32::consts::PI;
        for k in 0..8 {
            let (a, b) = (pi + pi * k as f32 / 8.0, pi + pi * (k + 1) as f32 / 8.0);
            ring(a, b + 0.01, new);
            ring(a - pi, b - pi + 0.01, old);
        }
        painter.circle_stroke(centre, outer, Stroke::new(1.5_f32, Color32::from_gray(40)));
        painter.circle_stroke(centre, inner, Stroke::new(1.5_f32, Color32::from_gray(40)));
        painter.circle_stroke(at, 3.0, Stroke::new(1.5_f32, Color32::WHITE));
    }

    /// The Eyedropper's Tool Settings.
    pub(crate) fn eyedropper_panel(&mut self, ui: &mut egui::Ui) {
        let e = &mut self.eyedropper;
        ui.label("Obtain colour from");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut e.source, PickSource::Canvas, "Canvas").on_hover_text("The colour you see: all visible layers");
            ui.selectable_value(&mut e.source, PickSource::Layer, "Current layer").on_hover_text("Only the layer being edited");
        });
        ui.label("Range");
        ui.horizontal(|ui| {
            for (size, label) in [(1, "1 px"), (3, "3×3"), (5, "5×5"), (11, "11×11")] {
                ui.selectable_value(&mut e.size, size, label);
            }
        });
        ui.small("Also with Alt while painting, filling or drawing a gradient, and by resting a finger on the canvas (iPad).");
    }
}

#[cfg(test)]
mod tests {
    use super::average;

    #[test]
    fn averages_unpremultiplied_and_skips_empty() {
        assert_eq!(average([[100, 50, 0, 255]].into_iter()), Some([100, 50, 0]));
        // Half-transparent red, premultiplied: still full red.
        assert_eq!(average([[128, 0, 0, 128]].into_iter()), Some([255, 0, 0]));
        assert_eq!(average([[0, 0, 0, 0], [0, 0, 0, 0]].into_iter()), None);
        // A little paint in the square is enough.
        assert_eq!(average([[255, 255, 255, 255], [0; 4], [0; 4], [0; 4]].into_iter()), Some([255, 255, 255]));
        assert_eq!(average([[200, 0, 0, 255], [0, 0, 200, 255]].into_iter()), Some([100, 0, 100]));
    }
}
