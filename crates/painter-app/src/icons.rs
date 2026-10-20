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

/// A square icon button; `selected` draws it pressed.
pub fn icon_button(ui: &mut Ui, icon: Icon, size: f32, selected: bool, hint: &str) -> Response {
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
