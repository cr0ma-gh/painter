//! Canvas navigation: pan, zoom and rotation between canvas pixels and screen points.

use std::f32::consts::{PI, TAU};

use eframe::egui::{vec2, Pos2, Rect, Vec2};
use painter_render::ViewUniform;

pub const MIN_ZOOM: f32 = 0.01;
pub const MAX_ZOOM: f32 = 64.0;

/// `screen = rect.center + R(rotation) · F · (doc − doc_size/2) · zoom + pan`,
/// where F mirrors x when the view is flipped.
#[derive(Clone, Copy, Debug)]
pub struct ViewTransform {
    pub pan: Vec2,
    /// Screen points per canvas pixel.
    pub zoom: f32,
    /// Radians, clockwise on screen.
    pub rotation: f32,
    /// Mirrored left-right (a view setting: the pixels don't change).
    pub flip: bool,
}

impl Default for ViewTransform {
    fn default() -> Self {
        Self { pan: Vec2::ZERO, zoom: 1.0, rotation: 0.0, flip: false }
    }
}

impl ViewTransform {
    pub fn doc_to_screen(&self, rect: Rect, doc_size: Vec2, p: Pos2) -> Pos2 {
        rect.center() + rotate(self.mirror((p.to_vec2() - doc_size * 0.5) * self.zoom), self.rotation) + self.pan
    }

    pub fn screen_to_doc(&self, rect: Rect, doc_size: Vec2, s: Pos2) -> Pos2 {
        let local = self.mirror(rotate((s - rect.center() - self.pan) / self.zoom, -self.rotation));
        (local + doc_size * 0.5).to_pos2()
    }

    fn mirror(&self, v: Vec2) -> Vec2 {
        if self.flip { vec2(-v.x, v.y) } else { v }
    }

    /// Zooms while keeping the canvas point under `anchor` fixed.
    pub fn zoom_about(&mut self, rect: Rect, doc_size: Vec2, anchor: Pos2, factor: f32) {
        let pinned = self.screen_to_doc(rect, doc_size, anchor);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.pan += anchor - self.doc_to_screen(rect, doc_size, pinned);
    }

    /// Sets zoom around the view centre.
    pub fn set_zoom(&mut self, zoom: f32) {
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        self.pan *= zoom / self.zoom;
        self.zoom = zoom;
    }

    /// Sets rotation around the view centre.
    pub fn set_rotation(&mut self, rotation: f32) {
        let rotation = (rotation + PI).rem_euclid(TAU) - PI;
        self.pan = rotate(self.pan, rotation - self.rotation);
        self.rotation = rotation;
    }

    /// Pans so canvas point `p` sits at the view centre.
    pub fn center_on(&mut self, doc_size: Vec2, p: Pos2) {
        self.pan = -rotate(self.mirror((p.to_vec2() - doc_size * 0.5) * self.zoom), self.rotation);
    }

    pub fn fit(&mut self, viewport: Vec2, doc_size: Vec2) {
        let zoom = (viewport.x / doc_size.x).min(viewport.y / doc_size.y) * 0.92;
        *self = Self { pan: Vec2::ZERO, zoom: zoom.clamp(MIN_ZOOM, MAX_ZOOM), rotation: 0.0, flip: self.flip };
    }

    pub fn uniform(&self, viewport: Vec2, doc_size: Vec2) -> ViewUniform {
        // Checker cells stay about 8 points on screen at any zoom.
        ViewUniform::new(
            [viewport.x, viewport.y],
            [doc_size.x, doc_size.y],
            [self.pan.x, self.pan.y],
            self.zoom,
            self.rotation,
            8.0 / self.zoom,
        )
        .with_flip(self.flip)
    }
}

fn rotate(v: Vec2, angle: f32) -> Vec2 {
    let (s, c) = angle.sin_cos();
    vec2(v.x * c - v.y * s, v.x * s + v.y * c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::pos2;

    #[test]
    fn round_trips_and_pins_zoom_anchor() {
        let rect = Rect::from_min_size(pos2(100.0, 50.0), vec2(800.0, 600.0));
        let doc = vec2(1000.0, 700.0);
        let mut view = ViewTransform { pan: vec2(13.0, -7.0), zoom: 0.7, rotation: 0.4, flip: true };
        let p = pos2(321.0, 123.0);
        let back = view.screen_to_doc(rect, doc, view.doc_to_screen(rect, doc, p));
        assert!((back - p).length() < 1e-3);

        let anchor = pos2(400.0, 300.0);
        let before = view.screen_to_doc(rect, doc, anchor);
        view.zoom_about(rect, doc, anchor, 2.5);
        assert!((view.screen_to_doc(rect, doc, anchor) - before).length() < 1e-3);
    }
}
