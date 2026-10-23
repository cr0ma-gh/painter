//! GIMP's symbolic icons (`assets/icons`, CC BY-SA, see `assets/icons/COPYING`).
//!
//! The SVGs are compiled in. Their theme greys (#bebebe, #b4b4b4) are swapped for white when they're
//! loaded, so `tint` gives them any colour.

use std::borrow::Cow;
use std::sync::OnceLock;

use eframe::egui::{self, Color32, ImageSource, Response, Sense, Ui, Vec2};

macro_rules! icons {
    ($($name:ident = $file:literal,)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[allow(non_camel_case_types, dead_code)]
        pub enum Icon { $($name,)* }

        const ICONS: &[(&str, &[u8])] = &[$(($file, include_bytes!(concat!("../assets/icons/", $file, ".svg"))),)*];
    };
}

icons! {
    Ink = "gimp-tool-ink",
    Pencil = "gimp-tool-pencil",
    Paintbrush = "gimp-tool-paintbrush",
    Airbrush = "gimp-tool-airbrush",
    Eraser = "gimp-tool-eraser",
    Bucket = "gimp-tool-bucket-fill",
    Gradient = "gimp-tool-gradient",
    RectSelect = "gimp-tool-rect-select",
    EllipseSelect = "gimp-tool-ellipse-select",
    FreeSelect = "gimp-tool-free-select",
    MagicSelect = "painter-magic-wand",
    Shapes = "painter-shapes",
    Clone = "painter-clone",
    PatternStamp = "painter-pattern-stamp",
    HistoryBrush = "painter-history-brush",
    Text = "painter-text",
    Balloon = "painter-balloon",
    MagneticLasso = "painter-magnetic-lasso",
    ObjectSelect = "painter-object-select",
    Navigation = "gimp-navigation",
    Move = "gimp-tool-move",
    ColorPicker = "gimp-tool-color-picker",
    Close = "gimp-close",
    Menu = "gimp-menu-left",
    Add = "list-add",
    Remove = "list-remove",
    Delete = "edit-delete",
    New = "document-new",
    Duplicate = "gimp-duplicate",
    Visible = "gimp-visible",
    Lock = "gimp-lock",
    LockAlpha = "gimp-lock-alpha",
    Layer = "gimp-layer",
    GroupLayer = "gimp-group-layer",
    Path = "gimp-path",
    LayerMask = "gimp-layer-mask",
    SwapColors = "gimp-swap-colors",
    DefaultColors = "gimp-default-colors",
    Up = "go-up",
    Down = "go-down",
    Save = "document-save",
    Open = "document-open",
    SaveAs = "document-save-as",
    Undo = "edit-undo",
    Redo = "edit-redo",
    ZoomFit = "zoom-fit-best",
    ZoomOriginal = "zoom-original",
    RotateLeft = "object-rotate-left",
    RotateRight = "object-rotate-right",
    Reset = "gimp-reset",
    Dynamics = "gimp-dynamics",
    MergeDown = "gimp-merge-down",
    Preset = "gimp-tool-preset",
    Brushes = "gimp-prefs-folders-brushes",
    GradientLinear = "gimp-gradient-linear",
    Zoom = "gimp-tool-zoom",
    Curve = "gimp-curve-smooth",
    Smudge = "gimp-tool-smudge",
    Transparency = "gimp-transparency",
    ColorTriangle = "gimp-color-triangle",
    Options = "gimp-tool-options",
    Transform = "gimp-tool-unified-transform",
    Crop = "gimp-tool-crop",
    Grid = "gimp-grid",
    Template = "gimp-template",
    Transform3d = "gimp-tool-transform-3d",
}

/// The recoloured SVG sources, built once.
fn sources() -> &'static [Vec<u8>] {
    static SOURCES: OnceLock<Vec<Vec<u8>>> = OnceLock::new();
    SOURCES.get_or_init(|| {
        ICONS
            .iter()
            .map(|(_, svg)| String::from_utf8_lossy(svg).replace("#bebebe", "#ffffff").replace("#b4b4b4", "#ffffff").into_bytes())
            .collect()
    })
}

impl Icon {
    pub fn source(self) -> ImageSource<'static> {
        let index = self as usize;
        ImageSource::Bytes {
            uri: Cow::Owned(format!("bytes://icons/{}.svg", ICONS[index].0)),
            bytes: egui::load::Bytes::Static(&sources()[index]),
        }
    }

    pub fn image(self, size: f32, tint: Color32) -> egui::Image<'static> {
        egui::Image::new(self.source()).fit_to_exact_size(Vec2::splat(size)).tint(tint)
    }

    /// Draws the icon centred in `rect`.
    pub fn paint(self, ui: &Ui, rect: egui::Rect, size: f32, tint: Color32) {
        self.image(size, tint).paint_at(ui, egui::Rect::from_center_size(rect.center(), Vec2::splat(size)));
    }
}

/// The icon of each kind of area.
pub fn editor_icon(editor: ui_workspace::tiles::EditorType) -> Icon {
    use ui_workspace::tiles::EditorType as E;
    match editor {
        E::Canvas => Icon::Template,
        E::Reference => Icon::ZoomOriginal,
        E::Viewport3D => Icon::Transform3d,
        E::Layers => Icon::Layer,
        E::ToolSettings => Icon::Options,
        E::ToolGroup => Icon::Preset,
        E::BrushSize => Icon::Paintbrush,
        E::ColorWheel => Icon::ColorTriangle,
        E::Navigator => Icon::Navigation,
        E::ComicPanels => Icon::Grid,
        E::Timeline => Icon::Curve,
        E::NodeGraph => Icon::Path,
        E::ColorSlider => Icon::GradientLinear,
        E::GamutSelector => Icon::ColorTriangle,
        E::ColorSet => Icon::DefaultColors,
        E::ColorHistory => Icon::Undo,
        E::IntermediateColor => Icon::Transparency,
        E::ApproximateColor => Icon::ColorPicker,
        E::ColorMixing => Icon::Smudge,
        E::SearchLayer => Icon::Zoom,
        E::History => Icon::Redo,
    }
}

/// Small line icons drawn in code (no image needed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    /// Fold an area (chevron up).
    Fold,
    /// Unfold it (chevron down).
    Unfold,
    /// Unfold a narrow column (chevron right).
    UnfoldSide,
    Search,
    /// Zen mode: four outward corners.
    FullScreen,
    /// Interface size: a small and a large square.
    Scale,
    /// Put a floating panel back: a window with an arrow into it.
    Dock,
}

/// Paints `glyph` centred in `rect`, `size` points across.
pub fn paint_glyph(painter: &egui::Painter, rect: egui::Rect, size: f32, glyph: Glyph, color: Color32) {
    let c = rect.center();
    let s = size * 0.5;
    let stroke = egui::Stroke::new((size / 9.0).max(1.2), color);
    let p = |x: f32, y: f32| c + Vec2::new(x * s, y * s);
    match glyph {
        Glyph::Fold => painter.add(egui::Shape::line(vec![p(-0.6, 0.3), p(0.0, -0.3), p(0.6, 0.3)], stroke)),
        Glyph::Unfold => painter.add(egui::Shape::line(vec![p(-0.6, -0.3), p(0.0, 0.3), p(0.6, -0.3)], stroke)),
        Glyph::UnfoldSide => painter.add(egui::Shape::line(vec![p(-0.3, -0.6), p(0.3, 0.0), p(-0.3, 0.6)], stroke)),
        Glyph::Search => {
            painter.circle_stroke(p(-0.15, -0.15), s * 0.5, stroke);
            painter.line_segment([p(0.22, 0.22), p(0.7, 0.7)], stroke);
            egui::layers::ShapeIdx(0)
        }
        Glyph::FullScreen => {
            for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let corner = p(sx * 0.75, sy * 0.75);
                painter.line_segment([corner, corner - Vec2::new(sx * s * 0.45, 0.0)], stroke);
                painter.line_segment([corner, corner - Vec2::new(0.0, sy * s * 0.45)], stroke);
            }
            egui::layers::ShapeIdx(0)
        }
        Glyph::Dock => {
            painter.rect_stroke(egui::Rect::from_min_max(p(-0.75, -0.2), p(0.75, 0.75)), 1.0, stroke, egui::StrokeKind::Middle);
            painter.line_segment([p(0.0, -0.85), p(0.0, 0.35)], stroke);
            painter.add(egui::Shape::line(vec![p(-0.3, 0.05), p(0.0, 0.35), p(0.3, 0.05)], stroke));
            egui::layers::ShapeIdx(0)
        }
        Glyph::Scale => {
            painter.rect_stroke(egui::Rect::from_min_max(p(-0.8, 0.0), p(-0.2, 0.6)), 1.0, stroke, egui::StrokeKind::Middle);
            painter.rect_stroke(egui::Rect::from_min_max(p(0.0, -0.75), p(0.8, 0.6)), 1.0, stroke, egui::StrokeKind::Middle);
            egui::layers::ShapeIdx(0)
        }
    };
}

/// How much bigger this part of the interface is than the rest: a panel's
/// own scale (see `workspace_ui::scaled_style`), read off its widget height.
pub fn panel_factor(ui: &Ui) -> f32 {
    let base = ui.ctx().style().spacing.interact_size.y;
    if base > 0.0 { (ui.spacing().interact_size.y / base).clamp(0.25, 4.0) } else { 1.0 }
}

/// A square button showing a [`Glyph`].
pub fn glyph_button(ui: &mut Ui, glyph: Glyph, size: f32, hint: &str) -> Response {
    let size = size * panel_factor(ui);
    let side = size + 6.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    let visuals = ui.visuals();
    if response.hovered() {
        ui.painter().rect_filled(rect, 3.0, visuals.widgets.hovered.weak_bg_fill);
    }
    let tint = if response.hovered() { crate::theme::strong() } else { visuals.text_color() };
    paint_glyph(ui.painter(), rect, size, glyph, tint);
    if hint.is_empty() {
        response
    } else {
        response.on_hover_text(hint)
    }
}

/// A square icon button; `selected` draws it pressed.
pub fn icon_button(ui: &mut Ui, icon: Icon, size: f32, selected: bool, hint: &str) -> Response {
    let size = size * panel_factor(ui);
    let side = size + 10.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    let visuals = ui.visuals();
    let fill = if selected {
        visuals.selection.bg_fill
    } else if response.hovered() {
        visuals.widgets.hovered.weak_bg_fill
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 3.0, fill);
    let tint = if !ui.is_enabled() {
        visuals.weak_text_color()
    } else if selected || response.hovered() {
        crate::theme::strong()
    } else {
        visuals.text_color()
    };
    icon.paint(ui, rect, size, tint);
    if hint.is_empty() {
        response
    } else {
        response.on_hover_text(hint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_an_svg_with_the_theme_grey_replaced() {
        for (name, svg) in ICONS.iter().zip(sources()) {
            let text = std::str::from_utf8(svg).unwrap();
            assert!(text.contains("<svg"), "{}", name.0);
            assert!(!text.contains("#bebebe"), "{}", name.0);
        }
    }
}
