//! The look: compact panels with thin title bars, flat list rows and a
//! muted selection, in the spirit of Clip Studio Paint. Colours come from
//! the active [`ui_workspace::Theme`] (dark by default) and can change while
//! the app runs, so they're read through the functions below.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle};
use ui_workspace::themes::{Rgba, Theme, ThemeColors};

static COLORS: RwLock<ThemeColors> = RwLock::new(ThemeColors::DARK);
static DARK: AtomicBool = AtomicBool::new(true);

fn color(pick: impl Fn(&ThemeColors) -> Rgba) -> Color32 {
    let c = COLORS.read().map(|c| pick(&c)).unwrap_or_else(|_| pick(&ThemeColors::DARK));
    let [r, g, b, a] = c.0;
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// Gap between areas.
pub fn backdrop() -> Color32 {
    color(|c| c.backdrop)
}
/// Panel bodies.
pub fn panel() -> Color32 {
    color(|c| c.panel)
}
/// Panel title bars and inactive tabs.
pub fn title() -> Color32 {
    color(|c| c.title)
}
/// Lists, wells and slider tracks.
pub fn well() -> Color32 {
    color(|c| c.well)
}
/// Row separators and outlines.
pub fn line() -> Color32 {
    color(|c| c.line)
}
pub fn text() -> Color32 {
    color(|c| c.text)
}
pub fn text_dim() -> Color32 {
    color(|c| c.text_dim)
}
/// Selected rows and pressed buttons.
pub fn selected() -> Color32 {
    color(|c| c.selected)
}
/// Slider fills and focus accents.
pub fn accent() -> Color32 {
    color(|c| c.accent)
}
/// Around the canvas in the viewport and the navigator.
pub fn canvas_backdrop() -> Color32 {
    color(|c| c.canvas_backdrop)
}

/// Emphasised text and icons: white on dark themes, black on light ones.
pub fn strong() -> Color32 {
    if DARK.load(Ordering::Relaxed) { Color32::WHITE } else { Color32::BLACK }
}

/// Warnings and errors.
pub fn warning() -> Color32 {
    color(|c| c.warning)
}

/// Hovered rows: between the panel and the text colour.
pub fn hover() -> Color32 {
    mix(panel(), text(), 0.12)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Makes `theme` the current look.
pub fn apply(ctx: &egui::Context, theme: &Theme) {
    if let Ok(mut c) = COLORS.write() {
        *c = theme.colors;
    }
    DARK.store(theme.base.is_dark(), Ordering::Relaxed);
    ctx.set_visuals(visuals(theme));
    let body = theme.font_size;
    ctx.style_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::new(body + 2.5, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(body, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(body, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(body - 2.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(body - 0.5, FontFamily::Monospace)),
        ]
        .into();
        let spacing = &mut style.spacing;
        spacing.item_spacing = egui::vec2(6.0, 4.0);
        spacing.button_padding = egui::vec2(6.0, 2.0);
        spacing.interact_size.y = (body + 7.5).max(20.0);
        spacing.window_margin = Margin::same(8);
        spacing.menu_margin = Margin::same(4);
        spacing.slider_rail_height = 4.0;
        spacing.scroll = egui::style::ScrollStyle::thin();
    });
}

fn visuals(theme: &Theme) -> egui::Visuals {
    let mut v = if theme.base.is_dark() { egui::Visuals::dark() } else { egui::Visuals::light() };
    let (panel, text) = (panel(), text());
    let strong = if theme.base.is_dark() { Color32::WHITE } else { Color32::BLACK };
    v.panel_fill = panel;
    v.window_fill = panel;
    v.window_stroke = Stroke::new(1.0_f32, line());
    v.window_corner_radius = CornerRadius::same(theme.rounding + 1);
    v.menu_corner_radius = CornerRadius::same(theme.rounding);
    v.extreme_bg_color = well();
    v.faint_bg_color = mix(panel, backdrop(), 0.5);
    v.code_bg_color = well();
    v.selection.bg_fill = selected();
    v.selection.stroke = Stroke::new(1.0_f32, strong);
    v.hyperlink_color = accent();
    v.warn_fg_color = color(|c| c.warning);
    v.slider_trailing_fill = true;
    v.striped = false;
    v.override_text_color = None;

    let radius = CornerRadius::same(theme.rounding);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = panel;
    w.noninteractive.weak_bg_fill = panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, line());
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, text);
    w.inactive.bg_fill = mix(panel, text, 0.10);
    w.inactive.weak_bg_fill = mix(panel, text, 0.075);
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0_f32, text);
    w.hovered.bg_fill = mix(panel, text, 0.18);
    w.hovered.weak_bg_fill = mix(panel, text, 0.14);
    w.hovered.bg_stroke = Stroke::new(1.0_f32, mix(panel, text, 0.30));
    w.hovered.fg_stroke = Stroke::new(1.0_f32, strong);
    w.active.bg_fill = selected();
    w.active.weak_bg_fill = selected();
    w.active.bg_stroke = Stroke::new(1.0_f32, accent());
    w.active.fg_stroke = Stroke::new(1.0_f32, strong);
    w.open.weak_bg_fill = mix(panel, text, 0.11);
    for state in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        state.corner_radius = radius;
    }
    v
}
