//! The Layer Properties panel (Window → Panels → Layer Properties), as Clip
//! Studio's Layer Property palette, for the active layer:
//! - Effect: Border effect (edge or watercolour edge), Extract line, Tone and
//!   Layer color, each a button that turns it on and shows its settings;
//! - Expression color: Color, Gray or Monochrome (which colours the layer
//!   holds; Monochrome can hide its black or its white);
//! - the layer's blend mode and opacity.
//!
//! Effects are drawn from the layer's pixels, which stay as painted: they
//! show on the canvas and in exports and turn off again (`painter_core::effects`).

use std::collections::HashSet;

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use painter_core::effects::{self, Border, BorderKind, DotShape, Expression, ExtractLine, ToneEffect};
use painter_core::{LayerId, TileCoord};
use painter_render::{plane_uploads, Plane, Upload};

use crate::app::PainterApp;
use crate::theme;

const DEFAULT_LAYER_COLOR: [u8; 3] = [0, 150, 255];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Effect {
    Border,
    Extract,
    Tone,
    LayerColor,
}

/// A small drawing for an effect's button, as Clip Studio's icons.
fn draw_effect_icon(painter: &egui::Painter, r: Rect, effect: Effect, ink: Color32) {
    let c = r.center();
    let s = Stroke::new(1.4_f32, ink);
    match effect {
        Effect::Border => {
            painter.circle_stroke(c, 6.5, s);
            painter.circle_stroke(c, 3.5, Stroke::new(1.0_f32, ink.gamma_multiply(0.6)));
        }
        Effect::Extract => {
            painter.circle_stroke(c, 6.5, s);
            let half = [c + Vec2::new(0.0, -6.5), c + Vec2::new(-6.5, 0.0), c + Vec2::new(0.0, 6.5)];
            painter.add(egui::Shape::convex_polygon(
                (0..=12)
                    .map(|i| {
                        let a = std::f32::consts::FRAC_PI_2 + i as f32 / 12.0 * std::f32::consts::PI;
                        c + Vec2::new(a.cos(), a.sin()) * 6.5
                    })
                    .collect(),
                ink,
                Stroke::NONE,
            ));
            let _ = half;
        }
        Effect::Tone => {
            for (i, j) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (0, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                let r = if (i + j) % 2 == 0 { 1.8 } else { 1.1 };
                painter.circle_filled(c + Vec2::new(i as f32 * 4.5, j as f32 * 4.5), r, ink);
            }
        }
        Effect::LayerColor => {
            painter.rect_stroke(Rect::from_center_size(c + Vec2::new(-2.0, -2.0), Vec2::splat(9.0)), 1.0, s, egui::StrokeKind::Middle);
            painter.rect_filled(Rect::from_center_size(c + Vec2::new(2.5, 2.5), Vec2::splat(9.0)), 1.0, ink);
        }
    }
}

/// An effect's toggle button; true when clicked.
fn effect_button(ui: &mut egui::Ui, effect: Effect, on: bool, hint: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(28.0, 24.0), Sense::click());
    let bg = if on {
        theme::selected()
    } else if resp.hovered() {
        theme::hover()
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 3.0, bg);
    let ink = if on { theme::strong() } else { theme::text() };
    draw_effect_icon(ui.painter(), rect, effect, ink);
    resp.on_hover_text(hint).clicked()
}

/// A section title with a line after it, as the palette's group headers.
fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).strong());
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 10.0), Sense::hover());
        ui.painter().line_segment(
            [Pos2::new(rect.left() + 4.0, rect.center().y), Pos2::new(rect.right(), rect.center().y)],
            Stroke::new(1.0_f32, theme::line()),
        );
    });
}

impl PainterApp {
    pub(crate) fn layer_properties_panel(&mut self, ui: &mut egui::Ui) {
        let id = self.doc.active;
        let Some(layer) = self.doc.find(id) else {
            ui.label("No layer");
            return;
        };
        if !layer.has_pixels() {
            ui.label(RichText::new(format!("“{}”", layer.name)).strong());
            ui.small("Effects and expression colour work on raster and vector layers.");
            return;
        }
        let dpi = self.doc.dpi.max(1) as f32;
        let mut fx = layer.effects;
        let mut tint = layer.layer_color;
        let (before_fx, before_tint) = (fx, tint);
        ui.label(RichText::new(format!("“{}”", layer.name)).small().color(theme::text_dim()));

        section(ui, "Effect");
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if effect_button(
                    ui,
                    Effect::LayerColor,
                    tint.is_some(),
                    "Layer color: the layer's lines in one colour, on screen only",
                ) {
                    tint = if tint.is_some() { None } else { Some(DEFAULT_LAYER_COLOR) };
                }
                if effect_button(ui, Effect::Tone, fx.tone.is_some(), "Tone: the layer as halftone dots") {
                    fx.tone =
                        if fx.tone.is_some() { None } else { Some(ToneEffect { cell: (dpi / 60.0).max(2.0), ..Default::default() }) };
                }
                if effect_button(ui, Effect::Extract, fx.extract.is_some(), "Extract line: the picture's edges as black lines") {
                    fx.extract = if fx.extract.is_some() { None } else { Some(ExtractLine::default()) };
                }
                if effect_button(
                    ui,
                    Effect::Border,
                    fx.border.is_some(),
                    "Border effect: an edge round the shapes, or a watercolour edge",
                ) {
                    fx.border = if fx.border.is_some() { None } else { Some(Border::default()) };
                }
            });
        });

        let grid = |ui: &mut egui::Ui, salt: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
            egui::Grid::new(salt).num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| add(ui));
        };
        if let Some(b) = &mut fx.border {
            section(ui, "Border effect");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut b.kind, BorderKind::Edge, "Edge");
                ui.selectable_value(&mut b.kind, BorderKind::Watercolor, "Watercolor edge");
            });
            grid(ui, "fx_border", &mut |ui| match b.kind {
                BorderKind::Edge => {
                    ui.label("Thickness of edge");
                    ui.add(egui::Slider::new(&mut b.width, 1.0..=100.0).logarithmic(true).suffix(" px"));
                    ui.end_row();
                    ui.label("Edge color");
                    ui.color_edit_button_srgb(&mut b.color);
                    ui.end_row();
                }
                BorderKind::Watercolor => {
                    ui.label("Range");
                    ui.add(egui::Slider::new(&mut b.width, 1.0..=100.0).logarithmic(true).suffix(" px"));
                    ui.end_row();
                    ui.label("Transparency");
                    crate::panels::percent_slider(ui, &mut b.transparency, "", 0.0..=100.0);
                    ui.end_row();
                    ui.label("Darkness");
                    crate::panels::percent_slider(ui, &mut b.darkness, "", 0.0..=100.0);
                    ui.end_row();
                    ui.label("Blur");
                    ui.add(egui::Slider::new(&mut b.blur, 0.0..=30.0).suffix(" px"));
                    ui.end_row();
                }
            });
        }
        if let Some(e) = &mut fx.extract {
            section(ui, "Extract line");
            grid(ui, "fx_extract", &mut |ui| {
                ui.label("Edge threshold");
                crate::panels::percent_slider(ui, &mut e.edge, "", 1.0..=100.0);
                ui.end_row();
                ui.label("Line width");
                ui.add(egui::Slider::new(&mut e.width, 1.0..=20.0).suffix(" px"));
                ui.end_row();
                ui.label("Black fill");
                crate::panels::percent_slider(ui, &mut e.black, "", 0.0..=100.0);
                ui.end_row();
            });
        }
        if let Some(t) = &mut fx.tone {
            section(ui, "Tone");
            grid(ui, "fx_tone", &mut |ui| {
                ui.label("Frequency");
                // Lines per inch at the document's resolution.
                let mut lpi = dpi / t.cell.max(1.0);
                if ui.add(egui::Slider::new(&mut lpi, 5.0..=200.0).logarithmic(true).suffix(" lpi")).changed() {
                    t.cell = (dpi / lpi).max(2.0);
                }
                ui.end_row();
                ui.label("Type of dot");
                egui::ComboBox::from_id_salt("fx_dot").selected_text(t.dot.name()).show_ui(ui, |ui| {
                    for d in DotShape::ALL {
                        ui.selectable_value(&mut t.dot, d, d.name());
                    }
                });
                ui.end_row();
                ui.label("Angle");
                ui.add(egui::Slider::new(&mut t.angle, 0.0..=180.0).suffix("°"));
                ui.end_row();
                ui.label("Density");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut t.from_alpha, false, "Brightness");
                    ui.selectable_value(&mut t.from_alpha, true, "Opacity");
                });
                ui.end_row();
                ui.label("Color");
                ui.horizontal(|ui| {
                    let mut own = t.color.is_none();
                    if ui.selectable_value(&mut own, true, "Image").clicked() {
                        t.color = None;
                    }
                    if ui.selectable_value(&mut own, false, "Set").clicked() && t.color.is_none() {
                        t.color = Some([0, 0, 0]);
                    }
                    if let Some(c) = &mut t.color {
                        ui.color_edit_button_srgb(c);
                    }
                });
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut t.paper, "White paper between dots");
                ui.end_row();
            });
        }
        if let Some(c) = &mut tint {
            section(ui, "Layer color");
            ui.horizontal(|ui| {
                ui.label("Color");
                ui.color_edit_button_srgb(c);
                ui.small("On screen only");
            });
        }

        section(ui, "Expression color");
        let expression_before = fx.expression;
        egui::ComboBox::from_id_salt("fx_expression").selected_text(fx.expression.name()).width(160.0).show_ui(ui, |ui| {
            for e in Expression::ALL {
                ui.selectable_value(&mut fx.expression, e, e.name());
            }
        });
        if fx.expression == Expression::Monochrome {
            ui.horizontal(|ui| {
                ui.label("Display color");
                ui.checkbox(&mut fx.show_black, "Black");
                ui.checkbox(&mut fx.show_white, "White");
            });
            ui.horizontal(|ui| {
                ui.label("Threshold");
                crate::panels::percent_slider(ui, &mut fx.threshold, "", 1.0..=99.0);
            });
        }

        if fx != before_fx || tint != before_tint {
            // One undo step per drag of a slider; each other change gets its own.
            let dragging = ui.input(|i| i.pointer.any_down());
            if !(dragging && self.layer_props_drag) {
                self.next_label = Some("Layer Property".into());
                self.checkpoint();
            }
            self.layer_props_drag = dragging;
            let threshold_moved = fx.threshold != before_fx.threshold;
            if let Some(layer) = self.doc.find_mut(id) {
                layer.effects = fx;
                layer.layer_color = tint;
                // A new expression (or threshold) turns the pixels into its colours.
                if fx.expression != expression_before || (fx.expression == Expression::Monochrome && threshold_moved) {
                    if let Some(tiles) = layer.tiles_mut() {
                        effects::keep_expression(tiles, None, &fx);
                    }
                }
            }
            self.upload_plane(Plane::Color(id));
            self.dirty = true;
        } else if !ui.input(|i| i.pointer.any_down()) {
            self.layer_props_drag = false;
        }
    }

    /// Uploads for layers with effects carry what they show, not their pixels:
    /// their planes are drawn again, whole, from `painter_core::effects`.
    pub(crate) fn effect_uploads(&self, uploads: Vec<Upload>) -> Vec<Upload> {
        let with_effects = |id: LayerId| self.doc.find(id).is_some_and(|l| l.effects.is_active() && l.tiles().is_some());
        let touched: HashSet<LayerId> = uploads
            .iter()
            .filter_map(|u| match u {
                Upload::Tile { plane: Plane::Color(id), .. } | Upload::Clear(Plane::Color(id)) => Some(*id),
                _ => None,
            })
            .filter(|id| with_effects(*id))
            .collect();
        if touched.is_empty() {
            return uploads;
        }
        let mut out: Vec<Upload> = uploads
            .into_iter()
            .filter(|u| !matches!(u, Upload::Tile { plane: Plane::Color(id), .. } | Upload::Clear(Plane::Color(id)) if touched.contains(id)))
            .collect();
        let canvas = self.doc.canvas();
        for id in touched {
            if let Some(layer) = self.doc.find(id) {
                let shown = effects::render(layer.tiles().expect("checked above"), &layer.effects, canvas);
                out.extend(plane_uploads(Plane::Color(id), &shown));
            }
        }
        out
    }

    /// After an edit of a Gray or Monochrome layer: back to its colours.
    pub(crate) fn keep_layer_expression(&mut self, plane: Plane, coords: &[TileCoord]) {
        let Plane::Color(id) = plane else { return };
        let Some(layer) = self.doc.find_mut(id) else { return };
        let fx = layer.effects;
        if fx.expression != Expression::Color {
            if let Some(tiles) = layer.tiles_mut() {
                effects::keep_expression(tiles, Some(coords), &fx);
            }
        }
    }

    /// The active layer's expression colour applied to a brush colour.
    pub(crate) fn layer_expression_color(&self, rgb: [f32; 3]) -> [f32; 3] {
        match self.doc.find(self.doc.active) {
            Some(l) if l.effects.expression != Expression::Color => effects::expression_color(&l.effects, rgb),
            _ => rgb,
        }
    }
}
