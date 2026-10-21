//! Search Layer: finds layers by name and kind across folders (like Clip
//! Studio's palette of that name). A click makes the layer the active one.

use eframe::egui::{self, Sense, Vec2};
use painter_core::{Layer, LayerId, LayerKind};

use crate::app::PainterApp;
use crate::icons::Icon;
use crate::theme;

/// What kind of layer to look for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum KindFilter {
    #[default]
    All,
    Raster,
    Vector,
    Folder,
    Correction,
    ThreeD,
    Fill,
    Masked,
    Reference,
    Hidden,
    Locked,
}

impl KindFilter {
    const ALL: [KindFilter; 11] = [
        KindFilter::All,
        KindFilter::Raster,
        KindFilter::Vector,
        KindFilter::Folder,
        KindFilter::Correction,
        KindFilter::ThreeD,
        KindFilter::Fill,
        KindFilter::Masked,
        KindFilter::Reference,
        KindFilter::Hidden,
        KindFilter::Locked,
    ];

    fn label(self) -> &'static str {
        match self {
            KindFilter::All => "All",
            KindFilter::Raster => "Raster",
            KindFilter::Vector => "Vector",
            KindFilter::Folder => "Folders",
            KindFilter::Correction => "Correction",
            KindFilter::ThreeD => "3D",
            KindFilter::Fill => "Paper, gradient, tone",
            KindFilter::Masked => "With a mask",
            KindFilter::Reference => "Reference",
            KindFilter::Hidden => "Hidden",
            KindFilter::Locked => "Locked",
        }
    }
}

#[derive(Default)]
pub(crate) struct SearchLayer {
    pub(crate) query: String,
    pub(crate) kind: KindFilter,
}

/// Whether `layer` matches the filter and every word of `query` (any case).
pub(crate) fn matches(layer: &Layer, kind: KindFilter, is_3d: bool, query: &str) -> bool {
    let kind_ok = match kind {
        KindFilter::All => true,
        KindFilter::Raster => matches!(layer.kind, LayerKind::Raster(_)) && !is_3d,
        KindFilter::Vector => matches!(layer.kind, LayerKind::Vector(_)),
        KindFilter::Folder => matches!(layer.kind, LayerKind::Folder(_)),
        KindFilter::Correction => matches!(layer.kind, LayerKind::Adjustment(_)),
        KindFilter::ThreeD => is_3d,
        KindFilter::Fill => matches!(layer.kind, LayerKind::Fill { .. } | LayerKind::Gradient(_) | LayerKind::Tone(_)),
        KindFilter::Masked => layer.mask.is_some(),
        KindFilter::Reference => layer.reference,
        KindFilter::Hidden => !layer.visible,
        KindFilter::Locked => layer.locked,
    };
    let name = layer.name.to_lowercase();
    kind_ok && query.split_whitespace().all(|w| name.contains(&w.to_lowercase()))
}

fn kind_icon(layer: &Layer) -> Icon {
    match layer.kind {
        LayerKind::Folder(_) => Icon::GroupLayer,
        LayerKind::Vector(_) => Icon::Path,
        LayerKind::Gradient(_) => Icon::GradientLinear,
        LayerKind::Adjustment(_) => Icon::Curve,
        _ => Icon::Layer,
    }
}

impl PainterApp {
    pub(crate) fn search_layer_panel(&mut self, ui: &mut egui::Ui) {
        let search = &mut self.layer_search;
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("search_layer_kind").selected_text(search.kind.label()).show_ui(ui, |ui| {
                for k in KindFilter::ALL {
                    ui.selectable_value(&mut search.kind, k, k.label());
                }
            });
            ui.add(egui::TextEdit::singleline(&mut search.query).hint_text("Type search keywords").desired_width(f32::INFINITY));
        });
        let (kind, query) = (search.kind, search.query.clone());
        if kind == KindFilter::All && query.trim().is_empty() {
            ui.label(egui::RichText::new("Type a name, or choose a kind of layer.").color(theme::text_dim()));
            return;
        }
        // Every layer, top first, with the folders it sits in.
        let rows = self.doc.rows_top_down();
        let mut path: Vec<String> = Vec::new();
        let mut found: Vec<(LayerId, String)> = Vec::new();
        for (id, depth) in rows {
            let Some(layer) = self.doc.find(id) else { continue };
            path.truncate(depth);
            if matches(layer, kind, self.scenes.contains_key(&id), &query) {
                found.push((id, path.join(" / ")));
            }
            if matches!(layer.kind, LayerKind::Folder(_)) {
                path.push(layer.name.clone());
            }
        }
        ui.small(format!("{} found", found.len()));
        let mut select = None;
        let mut toggle = None;
        for (id, folders) in &found {
            let Some(layer) = self.doc.find(*id) else { continue };
            let active = *id == self.doc.active;
            let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::click());
            let fill = if active {
                theme::selected()
            } else if response.hovered() {
                theme::hover()
            } else {
                theme::well()
            };
            ui.painter().rect_filled(rect, 2.0, fill);
            let eye = egui::Rect::from_min_size(rect.min + Vec2::new(4.0, 7.0), Vec2::splat(16.0));
            let tint = if layer.visible { theme::text() } else { theme::text_dim().gamma_multiply(0.4) };
            Icon::Visible.paint(ui, eye, 14.0, tint);
            if ui.interact(eye, ui.id().with(("search_eye", id.0)), Sense::click()).clicked() {
                toggle = Some(*id);
            }
            let icon = egui::Rect::from_min_size(rect.min + Vec2::new(26.0, 7.0), Vec2::splat(16.0));
            kind_icon(layer).paint(ui, icon, 14.0, theme::text());
            let text_at = rect.min + Vec2::new(48.0, 3.0);
            ui.painter().text(text_at, egui::Align2::LEFT_TOP, &layer.name, egui::FontId::proportional(12.0), theme::strong());
            if !folders.is_empty() {
                ui.painter().text(
                    text_at + Vec2::new(0.0, 14.0),
                    egui::Align2::LEFT_TOP,
                    folders,
                    egui::FontId::proportional(10.0),
                    theme::text_dim(),
                );
            }
            if response.clicked() {
                select = Some(*id);
            }
        }
        if let Some(id) = toggle {
            if let Some(layer) = self.doc.find_mut(id) {
                layer.visible = !layer.visible;
                self.recomposite = true;
                self.dirty = true;
            }
        }
        if let Some(id) = select {
            self.doc.active = id;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_by_words_and_kind() {
        let mut layer = Layer::raster(LayerId(3), "Line Art – Face");
        assert!(matches(&layer, KindFilter::All, false, "face line"));
        assert!(matches(&layer, KindFilter::Raster, false, "ART"));
        assert!(!matches(&layer, KindFilter::Raster, true, ""), "3D layers aren't plain raster");
        assert!(matches(&layer, KindFilter::ThreeD, true, ""));
        assert!(!matches(&layer, KindFilter::All, false, "hair"));
        assert!(!matches(&layer, KindFilter::Hidden, false, ""));
        layer.visible = false;
        assert!(matches(&layer, KindFilter::Hidden, false, "face"));
        assert!(!matches(&layer, KindFilter::Folder, false, ""));
    }
}
