//! Straight lines with Shift, as in Clip Studio and Photoshop.
//!
//! With a brush (pen, pencil, brush, eraser…), pressing with Shift starts a
//! straight line: from where the last stroke in this document ended (so
//! Shift-clicking point after point joins them), or from the press when
//! there's none. The line follows the pointer as a guide and is drawn when
//! the button is released, as an ordinary stroke along it: the brush's own
//! dynamics, the eraser, vector layers, symmetry and the selection all apply.
//! With Ctrl as well, its angle snaps to 15° steps. The Gradient tool snaps
//! its direction the same way with Shift.

use eframe::egui::{Color32, Painter, Pos2, Rect, Stroke};
use painter_core::StrokePoint;

use crate::app::PainterApp;

/// A Shift line being placed.
#[derive(Clone, Debug)]
pub(crate) struct LineDraft {
    pub(crate) start: StrokePoint,
    pub(crate) end: Pos2,
    /// Pressure while placing it, averaged: the line's pressure.
    pressure_sum: f32,
    samples: u32,
    tilt: f32,
    azimuth: f32,
}

/// `p` turned about `from` to the nearest multiple of `step_degrees`.
pub(crate) fn snap_angle(from: Pos2, p: Pos2, step_degrees: f32) -> Pos2 {
    let d = p - from;
    let len = d.length();
    if len < 1e-3 {
        return p;
    }
    let step = step_degrees.to_radians();
    let angle = (d.y.atan2(d.x) / step).round() * step;
    from + eframe::egui::vec2(angle.cos(), angle.sin()) * len
}

/// The points a line from `a` to `b` is fed as, every `spacing` canvas pixels,
/// at a steady pace from `t0`.
pub(crate) fn line_points(a: [f32; 2], b: [f32; 2], pressure: f32, tilt: f32, t0: f32, spacing: f32) -> Vec<StrokePoint> {
    let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
    let steps = (len / spacing.max(0.25)).ceil().max(1.0) as usize;
    // About 600 px/s: what a steady hand draws, for speed dynamics.
    let dt = spacing / 600.0;
    (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            StrokePoint::new(a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, pressure).with_tilt(tilt).at_time(t0 + dt * i as f32)
        })
        .collect()
}

impl PainterApp {
    /// Shift-press with a brush at canvas point `p` (`sample` carries the pen).
    pub(crate) fn begin_line(&mut self, sample: StrokePoint) {
        let here = self.docs.active_id();
        let start = match self.last_stroke_end {
            Some((doc, end)) if doc == here => end,
            _ => sample,
        };
        let mut line = LineDraft { start, end: Pos2::new(sample.x, sample.y), pressure_sum: 0.0, samples: 0, tilt: sample.tilt, azimuth: sample.azimuth };
        line.pressure_sum += sample.pressure;
        line.samples += 1;
        self.line = Some(line);
    }

    /// The pointer moved while placing a line.
    pub(crate) fn move_line(&mut self, sample: StrokePoint, snap: bool) {
        let Some(line) = &mut self.line else { return };
        let start = Pos2::new(line.start.x, line.start.y);
        let p = Pos2::new(sample.x, sample.y);
        line.end = if snap { snap_angle(start, p, 15.0) } else { p };
        line.pressure_sum += sample.pressure;
        line.samples += 1;
        line.tilt = sample.tilt;
        line.azimuth = sample.azimuth;
    }

    /// Draws the line as a stroke.
    pub(crate) fn finish_line(&mut self) {
        let Some(line) = self.line.take() else { return };
        let pressure = (line.pressure_sum / line.samples.max(1) as f32).clamp(0.0, 1.0);
        let t0 = self.started.elapsed().as_secs_f32();
        let points = line_points([line.start.x, line.start.y], [line.end.x, line.end.y], pressure, line.tilt, t0, 2.0);
        let mut points = points.into_iter().map(|p| p.with_azimuth(line.azimuth));
        let Some(first) = points.next() else { return };
        self.begin_stroke(first);
        for p in points {
            self.continue_stroke(p);
        }
        self.end_stroke();
    }

    /// The guide for the line being placed, in screen space.
    pub(crate) fn draw_line_guide(&self, painter: &Painter, rect: Rect) {
        let Some(line) = &self.line else { return };
        let doc = self.doc_size();
        let a = self.view.doc_to_screen(rect, doc, Pos2::new(line.start.x, line.start.y));
        let b = self.view.doc_to_screen(rect, doc, line.end);
        let width = (self.current_brush().size * self.view.zoom).clamp(1.0, 200.0);
        painter.line_segment([a, b], Stroke::new(width, crate::theme::accent().gamma_multiply(0.35)));
        painter.line_segment([a, b], Stroke::new(1.0_f32, Color32::from_black_alpha(160)));
        painter.circle_stroke(a, 4.0, Stroke::new(1.5_f32, crate::theme::accent()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angles_snap_and_lines_are_evenly_fed() {
        let o = Pos2::new(10.0, 10.0);
        let p = snap_angle(o, Pos2::new(110.0, 14.0), 15.0);
        assert!((p.y - 10.0).abs() < 1e-3 && (p.x - o.x - 100.08).abs() < 0.1, "flat: {p:?}");
        let q = snap_angle(o, Pos2::new(60.0, 58.0), 15.0);
        assert!(((q.x - o.x) - (q.y - o.y)).abs() < 1e-3, "45°: {q:?}");
        assert_eq!(snap_angle(o, o, 15.0), o);

        let pts = line_points([0.0, 0.0], [10.0, 0.0], 0.7, 0.2, 5.0, 2.0);
        assert_eq!(pts.len(), 6);
        assert_eq!((pts[0].x, pts[5].x), (0.0, 10.0));
        assert!(pts.iter().all(|p| p.y == 0.0 && p.pressure == 0.7 && p.tilt == 0.2));
        assert!(pts.windows(2).all(|w| w[1].time > w[0].time));
        assert_eq!(line_points([3.0, 3.0], [3.0, 3.0], 1.0, 0.0, 0.0, 2.0).len(), 2, "a dot");
    }
}
