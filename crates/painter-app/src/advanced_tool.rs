//! Advanced Tool Settings: every setting of the current brush, by category
//! (as in Clip Studio's window of that name), over one engine that reads and
//! writes both Clip Studio and Photoshop brushes. Export from here as
//! Painter's `.rustbrush`, Photoshop `.abr` or Clip Studio `.sut`.

use eframe::egui::{self, Color32, Sense, Vec2};
use painter_core::{AntiAlias, BrushMode, PressureCurve, SmoothingMethod};

use crate::app::PainterApp;
use crate::panels::{dynamics_row, percent_slider, preset_buttons, rotation_presets};
use crate::theme;
use crate::widgets::{prop_percent, prop_slider};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Section {
    BrushSize,
    Ink,
    ColorJitter,
    AntiAliasing,
    BrushTip,
    Spraying,
    Stroke,
    Texture,
    SecondBrush,
    Correction,
    StartingEnding,
    Export,
}

impl Section {
    const ALL: [Section; 12] = [
        Section::BrushSize,
        Section::Ink,
        Section::ColorJitter,
        Section::AntiAliasing,
        Section::BrushTip,
        Section::Spraying,
        Section::Stroke,
        Section::Texture,
        Section::SecondBrush,
        Section::Correction,
        Section::StartingEnding,
        Section::Export,
    ];

    fn title(self) -> &'static str {
        match self {
            Section::BrushSize => "Brush Size",
            Section::Ink => "Ink",
            Section::ColorJitter => "Color Jitter",
            Section::AntiAliasing => "Anti-aliasing",
            Section::BrushTip => "Brush Tip",
            Section::Spraying => "Spraying Effect",
            Section::Stroke => "Stroke",
            Section::Texture => "Texture",
            Section::SecondBrush => "2-Brush Shape",
            Section::Correction => "Correction",
            Section::StartingEnding => "Starting and Ending",
            Section::Export => "Export",
        }
    }

    fn help(self) -> &'static str {
        match self {
            Section::BrushSize => "Line width, and how pressure, speed, tilt and randomness change it.",
            Section::Ink => "How much paint each dab and each stroke lays down, and whether the tool paints or erases.",
            Section::ColorJitter => {
                "Changes the main colour a little for every stroke (Clip Studio's color jitter, Photoshop's color dynamics)."
            }
            Section::AntiAliasing => "How soft the edge of the line is.",
            Section::BrushTip => "The shape stamped along the stroke: round or an image, flattened and turned.",
            Section::Spraying => "Scatters dabs around the stroke (Clip Studio's spraying effect, Photoshop's scattering).",
            Section::Stroke => "How far apart the dabs are.",
            Section::Texture => "A paper texture the ink follows.",
            Section::SecondBrush => "A second tip that masks every dab (Clip Studio's 2-brush shape, Photoshop's dual brush).",
            Section::Correction => "Stabilisation of the stroke.",
            Section::StartingEnding => "Thin, light starts of strokes.",
            Section::Export => "Save this brush for Painter, Photoshop or Clip Studio.",
        }
    }
}

/// A grayscale bitmap from a PNG: coverage (alpha, or dark = ink) for tips,
/// luminance for textures.
fn load_bitmap(path: &std::path::Path, as_texture: bool) -> Result<brush_io::BrushTip, String> {
    let image = image::open(path).map_err(|e| e.to_string())?.to_rgba8();
    let has_alpha = image.pixels().any(|p| p[3] < 255);
    let values = image
        .pixels()
        .map(|p| {
            let luma = ((p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000) as u8;
            match (as_texture, has_alpha) {
                (true, _) => luma,
                (false, true) => p[3],
                (false, false) => 255 - luma,
            }
        })
        .collect();
    brush_io::BrushTip::new(image.width(), image.height(), values).map_err(|e| e.to_string())
}

impl PainterApp {
    pub(crate) fn open_advanced_tool_settings(&mut self) {
        self.advanced_tool = Some(self.advanced_tool.unwrap_or(Section::BrushSize));
    }

    /// The window: the brush's stroke on top, categories on the left, their settings on the right.
    pub(crate) fn advanced_tool_window(&mut self, ctx: &egui::Context) {
        let Some(mut section) = self.advanced_tool else { return };
        let mut open = true;
        egui::Window::new("Advanced Tool Settings").open(&mut open).default_size(Vec2::new(560.0, 520.0)).resizable(true).show(
            ctx,
            |ui| {
                // The stroke preview, as in Tool Settings.
                let index = self.brush.min(self.brushes.len() - 1);
                let total = self.brushes.len();
                let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 56.0), Sense::hover());
                ui.painter().rect_filled(rect, 3.0, Color32::from_gray(205));
                let texture = self.previews.get(ui.ctx(), index, &self.brushes[index].settings, total);
                let shown = egui::Rect::from_min_size(rect.left_top() + Vec2::new(rect.width() * 0.2, 8.0), texture.size_vec2());
                ui.painter().image(
                    texture.id(),
                    shown,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::from_gray(20),
                );
                ui.painter().text(
                    rect.left_top() + Vec2::new(6.0, 4.0),
                    egui::Align2::LEFT_TOP,
                    &self.brushes[index].settings.name,
                    egui::FontId::proportional(12.0),
                    Color32::from_gray(30),
                );
                ui.add_space(4.0);
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(150.0);
                        for s in Section::ALL {
                            ui.selectable_value(&mut section, s, s.title());
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(section.title()).strong());
                        ui.small(section.help());
                        ui.separator();
                        egui::ScrollArea::vertical().id_salt("advanced_tool_scroll").max_height(380.0).show(ui, |ui| {
                            self.advanced_section(ui, section);
                        });
                    });
                });
            },
        );
        self.advanced_tool = open.then_some(section);
    }

    fn advanced_section(&mut self, ui: &mut egui::Ui, section: Section) {
        let index = self.brush.min(self.brushes.len() - 1);
        let mut tip_changed = false;
        let preset = &mut self.brushes[index];
        let brush = &mut preset.settings;
        match section {
            Section::BrushSize => {
                prop_slider(ui, "Brush size", &mut brush.size, 0.5..=2000.0, " px", 1, true);
                ui.checkbox(&mut brush.pressure_size, "Pen pressure");
                if brush.pressure_size {
                    prop_percent(ui, "Minimum", &mut brush.min_size_ratio, 0.0..=100.0);
                    pressure_curve(ui, "size_curve", &mut brush.size_curve);
                }
                dynamics_row(ui, "Size with speed, tilt, random", &mut brush.size_dynamics, false);
            }
            Section::Ink => {
                ui.horizontal(|ui| {
                    ui.label("Mode");
                    ui.selectable_value(&mut brush.mode, BrushMode::Paint, "Paint");
                    ui.selectable_value(&mut brush.mode, BrushMode::Erase, "Erase");
                });
                prop_percent(ui, "Opacity", &mut brush.opacity, 0.0..=100.0);
                prop_percent(ui, "Density (flow)", &mut brush.density, 0.0..=100.0);
                ui.checkbox(&mut brush.pressure_density, "Density from pen pressure");
                if brush.pressure_density {
                    pressure_curve(ui, "density_curve", &mut brush.density_curve);
                }
                dynamics_row(ui, "Density with speed, tilt, random", &mut brush.density_dynamics, false);
                dynamics_row(ui, "Opacity per dab", &mut brush.opacity_dynamics, true);
            }
            Section::ColorJitter => {
                let j = &mut brush.color_jitter;
                percent_slider(ui, &mut j.hue, "Hue", 0.0..=100.0);
                percent_slider(ui, &mut j.saturation, "Saturation", 0.0..=100.0);
                percent_slider(ui, &mut j.value, "Brightness", 0.0..=100.0);
                ui.small("One variation per stroke.");
            }
            Section::AntiAliasing => {
                ui.horizontal(|ui| {
                    for aa in AntiAlias::ALL {
                        ui.selectable_value(&mut brush.antialias, aa, aa.name());
                    }
                });
                prop_percent(ui, "Hardness", &mut brush.hardness, 0.0..=100.0);
            }
            Section::BrushTip => {
                ui.horizontal(|ui| {
                    ui.label(if preset.tip.is_some() { "Image tip" } else { "Round tip" });
                    if ui.button("Load image...").on_hover_text("A PNG: transparency (or dark pixels) is the ink").clicked() {
                        if let Some(path) = crate::files::FileDialog::new().add_filter("PNG image", &["png"]).pick_file() {
                            match load_bitmap(&path, false) {
                                Ok(tip) => {
                                    preset.tip = Some(tip);
                                    tip_changed = true;
                                }
                                Err(err) => self.status = format!("Couldn't load the tip: {err}"),
                            }
                        }
                    }
                    if preset.tip.is_some() && ui.button("Round").clicked() {
                        preset.tip = None;
                        tip_changed = true;
                    }
                });
                let mut thickness = preset.thickness * 100.0;
                if ui.add(egui::Slider::new(&mut thickness, 1.0..=100.0).suffix("%").text("Thickness")).changed() {
                    preset.thickness = thickness / 100.0;
                    tip_changed = true;
                }
                if ui.add(egui::Slider::new(&mut preset.angle, -180.0..=180.0).suffix("°").text("Angle")).changed() {
                    tip_changed = true;
                }
                let brush = &mut preset.settings;
                dynamics_row(ui, "Thickness with pressure, speed, tilt, random", &mut brush.thickness_dynamics, true);
                ui.label("Rotation");
                ui.indent("adv_rotation", |ui| {
                    ui.checkbox(&mut brush.rotation.follow_direction, "Follows the stroke direction");
                    ui.horizontal(|ui| {
                        ui.label("Random flip");
                        ui.checkbox(&mut brush.rotation.random_flip_x, "horizontal");
                        ui.checkbox(&mut brush.rotation.random_flip_y, "vertical");
                    });
                    let rotation = &mut brush.rotation;
                    for (source, curve) in [
                        ("with pressure", &mut rotation.pressure),
                        ("with speed", &mut rotation.velocity),
                        ("with tilt", &mut rotation.tilt),
                    ] {
                        preset_buttons(ui, source, curve, &rotation_presets());
                    }
                    percent_slider(ui, &mut brush.rotation.jitter, "Random", 0.0..=100.0);
                });
            }
            Section::Spraying => {
                let s = &mut brush.scatter;
                let mut on = s.is_on();
                if ui.checkbox(&mut on, "Spray").changed() {
                    *s = if on {
                        painter_core::Scatter { amount: 1.0, count: 4, count_jitter: 0.5, both_axes: true }
                    } else {
                        Default::default()
                    };
                }
                if on {
                    percent_slider(ui, &mut s.amount, "Spread (of the size)", 0.0..=400.0);
                    let mut count = s.count.max(1) as f32;
                    if ui.add(egui::Slider::new(&mut count, 1.0..=16.0).step_by(1.0).text("Dabs per point")).changed() {
                        s.count = count as u32;
                    }
                    percent_slider(ui, &mut s.count_jitter, "Count random", 0.0..=100.0);
                    ui.checkbox(&mut s.both_axes, "Along the stroke too");
                }
            }
            Section::Stroke => {
                prop_percent(ui, "Spacing (of the size)", &mut brush.spacing, 1.0..=200.0);
            }
            Section::Texture => {
                ui.horizontal(|ui| {
                    ui.label(if preset.texture.is_some() { "Texture on" } else { "No texture" });
                    if ui.button("Load image...").on_hover_text("A PNG; light areas hold the ink back").clicked() {
                        if let Some(path) = crate::files::FileDialog::new().add_filter("PNG image", &["png"]).pick_file() {
                            match load_bitmap(&path, true) {
                                Ok(image) => {
                                    let (scale, density, invert) =
                                        preset.texture.as_ref().map_or((100.0, 0.6, false), |t| (t.scale, t.density, t.invert));
                                    preset.texture = Some(brush_io::BrushTexture { image, scale, density, invert });
                                    tip_changed = true;
                                }
                                Err(err) => self.status = format!("Couldn't load the texture: {err}"),
                            }
                        }
                    }
                    if preset.texture.is_some() && ui.button("Remove").clicked() {
                        preset.texture = None;
                        tip_changed = true;
                    }
                });
                if let Some(t) = &mut preset.texture {
                    tip_changed |=
                        ui.add(egui::Slider::new(&mut t.scale, 5.0..=1000.0).suffix("%").text("Scale").logarithmic(true)).changed();
                    tip_changed |= percent_slider(ui, &mut t.density, "Density", 0.0..=100.0);
                    tip_changed |= ui.checkbox(&mut t.invert, "Invert").changed();
                }
            }
            Section::SecondBrush => {
                ui.horizontal(|ui| {
                    ui.label(if preset.dual.is_some() { "Second tip on" } else { "No second tip" });
                    if ui.button("Load image...").clicked() {
                        if let Some(path) = crate::files::FileDialog::new().add_filter("PNG image", &["png"]).pick_file() {
                            match load_bitmap(&path, false) {
                                Ok(tip) => {
                                    let scale = preset.dual.as_ref().map_or(1.0, |d| d.scale);
                                    preset.dual = Some(brush_io::DualBrush { tip, scale });
                                    tip_changed = true;
                                }
                                Err(err) => self.status = format!("Couldn't load the tip: {err}"),
                            }
                        }
                    }
                    if preset.dual.is_some() && ui.button("Remove").clicked() {
                        preset.dual = None;
                        tip_changed = true;
                    }
                });
                if let Some(d) = &mut preset.dual {
                    tip_changed |= percent_slider(ui, &mut d.scale, "Size (of the main tip)", 5.0..=800.0);
                }
            }
            Section::Correction => {
                let mut stabilization = brush.stabilization as f32;
                if prop_slider(ui, "Stabilization", &mut stabilization, 0.0..=100.0, "", 0, false) {
                    brush.stabilization = stabilization.round() as u32;
                }
                egui::ComboBox::from_label("Smoothing").selected_text(brush.smoothing.name()).show_ui(ui, |ui| {
                    for method in SmoothingMethod::ALL {
                        ui.selectable_value(&mut brush.smoothing, method, method.name());
                    }
                });
            }
            Section::StartingEnding => {
                let mut on = brush.taper_start > 0.0;
                if ui.checkbox(&mut on, "Starting: grow from thin and light").changed() {
                    brush.taper_start = if on { 60.0 } else { 0.0 };
                }
                if on {
                    prop_slider(ui, "Length", &mut brush.taper_start, 1.0..=1000.0, " px", 0, true);
                }
                ui.small("Endings need the whole stroke and aren't drawn yet.");
            }
            Section::Export => {
                ui.label("This brush, as:");
                if ui.button("Painter brush (.rustbrush)...").clicked() {
                    self.export_brushes(Some(index));
                }
                if ui.button("Photoshop brush (.abr)...").on_hover_text("Tip, texture, dual brush and the main dynamics").clicked() {
                    self.export_abr(index);
                }
                if ui
                    .button("Clip Studio sub tool (.sut)...")
                    .on_hover_text("Written into a sub tool exported from Clip Studio, used as a template")
                    .clicked()
                {
                    self.export_sut(index);
                }
                let template = crate::files::app_data_dir().map(|d| d.join(SUT_TEMPLATE));
                match template.filter(|t| t.is_file()) {
                    Some(_) => {
                        if ui.small_button("Choose another Clip Studio template...").clicked() {
                            self.choose_sut_template();
                        }
                    }
                    None => {
                        ui.small("Clip Studio export needs one sub tool exported from Clip Studio (any brush) as a template; you'll be asked for it.");
                    }
                }
            }
        }
        if tip_changed {
            self.brushes[index].apply_tip();
        }
    }

    pub(crate) fn export_abr(&mut self, index: usize) {
        let preset = self.brushes[index].clone();
        let name = format!("{}.abr", preset.settings.name);
        let Some(path) = crate::files::FileDialog::new().add_filter("Photoshop brushes", &["abr"]).set_file_name(name).save_file()
        else {
            return;
        };
        self.status = match brush_io::abr_write::write(&[preset])
            .map_err(|e| e.to_string())
            .and_then(|b| std::fs::write(&path, b).map_err(|e| e.to_string()))
        {
            Ok(()) => {
                crate::files::offer_export(&path);
                format!("Exported {} for Photoshop (no stabilisation, spray, colour jitter or starting taper there)", path.display())
            }
            Err(err) => format!("Photoshop export failed: {err}"),
        };
    }

    /// Asks for a Clip Studio sub tool to use as the export template and keeps a copy.
    pub(crate) fn choose_sut_template(&mut self) -> bool {
        let Some(dir) = crate::files::app_data_dir() else { return false };
        let Some(source) = crate::files::FileDialog::new().add_filter("Clip Studio sub tool", &["sut"]).pick_file() else {
            return false;
        };
        let _ = std::fs::create_dir_all(&dir);
        match std::fs::copy(&source, dir.join(SUT_TEMPLATE)) {
            Ok(_) => {
                self.status = format!("Clip Studio template: {}", source.display());
                true
            }
            Err(err) => {
                self.status = format!("Couldn't keep the template: {err}");
                false
            }
        }
    }

    pub(crate) fn export_sut(&mut self, index: usize) {
        let Some(template) = crate::files::app_data_dir().map(|d| d.join(SUT_TEMPLATE)) else { return };
        if !template.is_file() && !self.choose_sut_template() {
            self.status = "Clip Studio export needs a sub tool exported from Clip Studio as a template".into();
            return;
        }
        let preset = self.brushes[index].clone();
        let name = format!("{}.sut", preset.settings.name);
        let Some(path) = crate::files::FileDialog::new().add_filter("Clip Studio sub tool", &["sut"]).set_file_name(name).save_file()
        else {
            return;
        };
        self.status = match brush_io::sut::write_with_template(&template, &preset, &path) {
            Ok(notes) => {
                crate::files::offer_export(&path);
                let notes = if notes.is_empty() { String::new() } else { format!(" ({})", notes.join("; ")) };
                format!("Exported {} for Clip Studio{notes}", path.display())
            }
            Err(err) => format!("Clip Studio export failed: {err}"),
        };
    }
}

/// The Clip Studio sub tool kept as the export template, in the app's data folder.
const SUT_TEMPLATE: &str = "clip-studio-template.sut";

/// Soft / linear / firm presets and the curve editor.
fn pressure_curve(ui: &mut egui::Ui, id: &str, curve: &mut PressureCurve) {
    ui.horizontal(|ui| {
        ui.label("Curve");
        for (label, gamma) in [("Soft", 0.6), ("Linear", 1.0), ("Firm", 1.7)] {
            if ui.small_button(label).clicked() {
                *curve = PressureCurve::gamma(gamma);
            }
        }
    });
    crate::curve::curve_editor(ui, id, curve);
    ui.label(egui::RichText::new("Drag points · double-click to add · right-click to remove").small().color(theme::text_dim()));
}
