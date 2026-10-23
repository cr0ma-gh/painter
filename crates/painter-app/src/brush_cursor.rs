//! The brush cursor and tip preview: the dab the pen would stamp right now
//! (`StrokeEngine::preview_dab`: size, pressure, tilt, the pen's lean,
//! rotation and thickness dynamics), drawn with the tip's real shape: an
//! ellipse for round tips, the bitmap itself for image tips.
//!
//! Ctrl + Alt resizes the brush: while both are held, moving the pen or the
//! mouse sideways makes it bigger (right) or smaller (left), the outline's
//! edge following the pointer.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use eframe::egui::{self, Color32, Mesh, Pos2, Rect, Shape, Stroke, Vec2};
use painter_core::{BrushSettings, Dab, StrokeEngine, StrokePoint, TipBitmap, TipShape};

use crate::app::PainterApp;
use crate::tools::Tool;

/// Textures of image tips for the cursor, by bitmap.
#[derive(Default)]
pub(crate) struct TipTextures {
    cache: HashMap<usize, (Weak<TipBitmap>, egui::TextureHandle)>,
}

impl TipTextures {
    fn get(&mut self, ctx: &egui::Context, bitmap: &Arc<TipBitmap>) -> egui::TextureId {
        self.cache.retain(|_, (weak, _)| weak.strong_count() > 0);
        let key = Arc::as_ptr(bitmap) as usize;
        self.cache
            .entry(key)
            .or_insert_with(|| {
                let (w, h) = (bitmap.width() as usize, bitmap.height() as usize);
                // White where the tip paints, its alpha the tip's.
                let pixels: Vec<Color32> = bitmap.alpha()[..w * h].iter().map(|&a| Color32::from_white_alpha(a)).collect();
                let image = egui::ColorImage { size: [w, h], pixels };
                (Arc::downgrade(bitmap), ctx.load_texture("brush-tip", image, egui::TextureOptions::LINEAR))
            })
            .1
            .id()
    }
}

/// Ctrl + Alt resizing: where it started (screen) and the size then.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SizeDrag {
    anchor: Pos2,
    start: f32,
}

/// The tip's outline around the origin, in canvas pixels: from tip space
/// (rotation, then flattening) the way `raster::stamp_dab` reads it.
fn to_canvas(settings: &BrushSettings, dab: &Dab, u: f32, v: f32) -> Vec2 {
    let tip = &settings.tip;
    let [cos, sin] = tip.rotation(dab.angle);
    let flat = tip.flatness_for(dab.thickness);
    let u = if dab.mirror { -u } else { u };
    let v = v * flat;
    Vec2::new(cos * u + sin * v, -sin * u + cos * v)
}

/// Draws the dab's tip centred on canvas point `centre`; `to_screen` maps canvas points.
pub(crate) fn paint_tip(
    painter: &egui::Painter,
    textures: &mut TipTextures,
    settings: &BrushSettings,
    dab: &Dab,
    centre: Pos2,
    to_screen: &dyn Fn(Pos2) -> Pos2,
    filled: bool,
) {
    let r = dab.radius.max(0.5);
    match &settings.tip.shape {
        TipShape::Sampled(bitmap) => {
            let [ax, ay] = bitmap.aspect();
            let (hw, hh) = (r * ax, r * ay);
            let corners =
                [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)].map(|(u, v)| to_screen(centre + to_canvas(settings, dab, u, v)));
            let texture = textures.get(painter.ctx(), bitmap);
            let mut mesh = Mesh::with_texture(texture);
            let tint = if filled { Color32::from_gray(30) } else { Color32::from_rgba_unmultiplied(40, 40, 40, 150) };
            for (p, uv) in corners.iter().zip([[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]) {
                mesh.vertices.push(egui::epaint::Vertex { pos: *p, uv: Pos2::new(uv[0], uv[1]), color: tint });
            }
            mesh.indices = vec![0, 1, 2, 0, 2, 3];
            painter.add(Shape::mesh(mesh));
            if !filled {
                let outline: Vec<Pos2> = corners.to_vec();
                painter.add(Shape::closed_line(outline, Stroke::new(1.0_f32, Color32::from_white_alpha(110))));
            }
        }
        TipShape::Round => {
            let points: Vec<Pos2> = (0..64)
                .map(|i| {
                    let t = i as f32 / 64.0 * std::f32::consts::TAU;
                    to_screen(centre + to_canvas(settings, dab, r * t.cos(), r * t.sin()))
                })
                .collect();
            if filled {
                painter.add(Shape::convex_polygon(points, Color32::from_gray(30), Stroke::NONE));
            } else {
                painter.add(Shape::closed_line(points.clone(), Stroke::new(2.0_f32, Color32::from_black_alpha(140))));
                painter.add(Shape::closed_line(points, Stroke::new(1.0_f32, Color32::from_white_alpha(210))));
            }
        }
    }
}

impl PainterApp {
    /// Ctrl + Alt held with the brush: sideways pointer motion sets the size.
    /// `true` while resizing (the canvas then starts no stroke).
    pub(crate) fn brush_resize(&mut self, pointer: Option<Pos2>) -> bool {
        let mods = self.pointer_mods;
        let held = mods.ctrl && mods.alt && !mods.shift;
        if self.tool != Tool::Brush || !held || self.stroke.is_some() || self.line.is_some() {
            self.size_drag = None;
            return false;
        }
        let Some(pos) = pointer else { return self.size_drag.is_some() };
        let size = self.current_brush().size;
        let drag = *self.size_drag.get_or_insert(SizeDrag { anchor: pos, start: size });
        // The outline's edge follows the pointer: the diameter changes by twice the motion.
        let zoom = self.view.zoom.max(1e-3);
        let new = (drag.start + 2.0 * (pos.x - drag.anchor.x) / zoom).clamp(0.5, 2000.0);
        if (new - size).abs() > 1e-3 {
            self.current_brush_mut().size = new;
            self.status = format!("Brush size {new:.1} px");
        }
        true
    }

    /// The brush's tip at the pointer, as the pen would stamp it now.
    pub(crate) fn draw_brush_cursor(&mut self, painter: &egui::Painter, rect: Rect, response: &egui::Response) {
        if self.tool != Tool::Brush {
            return;
        }
        let doc_size = self.doc_size();
        // While resizing, the tip stays where Ctrl + Alt began.
        let at = match self.size_drag {
            Some(drag) => drag.anchor,
            // iPad: the Pencil hovering over the screen, or the trackpad pointer.
            None => match response.hover_pos().or_else(|| crate::tablet::pencil_hover(painter.ctx())) {
                Some(p) if rect.contains(p) => p,
                _ => return,
            },
        };
        let centre = self.view.screen_to_doc(rect, doc_size, at);
        let point = StrokePoint::new(centre.x, centre.y, self.tablet.pressure())
            .with_tilt(self.tablet.tilt())
            .with_azimuth(self.tablet.azimuth());
        let settings = self.current_brush().clone();
        // At least the size the brush has at full pressure while resizing, so the change shows.
        let mut dab = StrokeEngine::preview_dab(&settings, point);
        if self.size_drag.is_some() {
            dab.radius = settings.size * 0.5;
        }
        let view = self.view;
        let to_screen = move |p: Pos2| view.doc_to_screen(rect, doc_size, p);
        // Tiny on screen: a plain marker instead.
        if dab.radius * self.view.zoom < 2.0 {
            painter.circle_stroke(at, 3.0, Stroke::new(1.0_f32, Color32::from_white_alpha(200)));
            painter.circle_stroke(at, 4.0, Stroke::new(1.0_f32, Color32::from_black_alpha(140)));
            return;
        }
        paint_tip(painter, &mut self.tip_textures, &settings, &dab, centre, &to_screen, false);
    }

    /// A box showing the tip as the pen would stamp it now (Tool Settings).
    pub(crate) fn tip_preview(&mut self, ui: &mut egui::Ui) {
        let side = 72.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width().min(220.0), side), egui::Sense::hover());
        ui.painter().rect_filled(rect, 4.0, Color32::from_gray(235));
        let settings = self.current_brush().clone();
        let point =
            StrokePoint::new(0.0, 0.0, self.tablet.pressure()).with_tilt(self.tablet.tilt()).with_azimuth(self.tablet.azimuth());
        let dab = StrokeEngine::preview_dab(&settings, point);
        // Scaled to fit the box, keeping the shape and turn.
        let scale = (side * 0.42 / dab.radius.max(0.5)).min(4.0);
        let c = rect.center();
        let to_screen = move |p: Pos2| c + p.to_vec2() * scale;
        let painter = ui.painter_at(rect);
        paint_tip(&painter, &mut self.tip_textures, &settings, &dab, Pos2::ZERO, &to_screen, true);
        ui.small(format!(
            "{:.1} px · pressure {:.0}% · tilt {:.0}% · lean {:.0}°",
            dab.radius * 2.0,
            self.tablet.pressure() * 100.0,
            self.tablet.tilt() * 100.0,
            self.tablet.azimuth()
        ));
    }
}
