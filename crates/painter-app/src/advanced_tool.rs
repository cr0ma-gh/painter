//! Advanced Tool Settings: every setting of the current brush, by category
//! (as in Clip Studio's window of that name), over one engine that reads and
//! writes both Clip Studio and Photoshop brushes. Export from here as
//! Painter's `.rustbrush`, Photoshop `.abr` or Clip Studio `.sut`.

use eframe::egui::{self, Color32, Sense, Vec2};
use crate::i18n::tr;
use painter_core::{AntiAlias, BrushMode, PressureCurve, SmoothingMethod};

use crate::app::PainterApp;
use crate::panels::{dynamics_row, percent_slider, preset_buttons, rotation_presets};
use crate::theme;
use crate::previews::DETAIL_PREVIEW_SIZE;
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
    /// Pressure curves and speed, tilt and random responses in one place.
    Dynamics,
    Correction,
    StartingEnding,
    Export,
}

impl Section {
    /// The sidebar: macro areas (as in Clip Studio's Sub Tool Detail), each with its categories.
    const AREAS: [(&'static str, &'static [Section]); 6] = [
        ("General", &[Section::BrushSize, Section::Ink, Section::ColorJitter]),
        ("Tip", &[Section::BrushTip, Section::AntiAliasing, Section::Stroke, Section::Spraying]),
        ("Dynamics", &[Section::Dynamics]),
        ("Texture", &[Section::Texture, Section::SecondBrush]),
        ("Inking", &[Section::Correction, Section::StartingEnding]),
        ("Brush File", &[Section::Export]),
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
            Section::Dynamics => "Pressure and Dynamics",
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
            Section::Dynamics => "Pressure curves for size and density, and how pressure, speed, tilt and randomness change size, density, opacity and the tip.",
            Section::Correction => "Stabilisation of the stroke.",
            Section::StartingEnding => "Thin, light starts of strokes.",
            Section::Export => "Save this brush for Painter, Photoshop or Clip Studio.",
        }
    }
}

/// Calls `$do!` with the preset fields a category of the window edits, as
/// paths from a [`brush_io::BrushPreset`].
macro_rules! section_fields {
    ($section:expr, $do:ident) => {
        match $section {
            Section::BrushSize => $do!(
                settings.size,
                settings.size_relative,
                settings.pressure_size,
                settings.min_size_ratio,
                settings.size_curve,
                settings.size_dynamics
            ),
            Section::Ink => $do!(
                settings.mode,
                settings.blend,
                settings.opacity,
                settings.density,
                settings.pressure_density,
                settings.density_curve,
                settings.density_dynamics,
                settings.opacity_dynamics
            ),
            Section::ColorJitter => $do!(settings.color_jitter),
            Section::AntiAliasing => $do!(settings.antialias, settings.hardness),
            Section::BrushTip => $do!(tip, thickness, angle, settings.thickness_dynamics, settings.rotation, settings.tilt_side),
            Section::Spraying => $do!(settings.scatter),
            Section::Stroke => $do!(settings.spacing),
            Section::Texture => $do!(texture),
            Section::SecondBrush => $do!(dual),
            Section::Dynamics => $do!(
                settings.pressure_size,
                settings.pressure_density,
                settings.size_curve,
                settings.density_curve,
                settings.size_dynamics,
                settings.density_dynamics,
                settings.opacity_dynamics,
                settings.thickness_dynamics
            ),
            Section::Correction => $do!(settings.stabilization, settings.smoothing),
            Section::StartingEnding => $do!(settings.taper_start),
            Section::Export => $do!(),
        }
    };
}

impl Section {
    /// Whether this category's settings differ between `a` and `b`.
    fn differs(self, a: &brush_io::BrushPreset, b: &brush_io::BrushPreset) -> bool {
        macro_rules! any_differs {
            ($($($p:ident).+),*) => { false $(|| a.$($p).+ != b.$($p).+)* };
        }
        section_fields!(self, any_differs)
    }

    /// Puts this category's settings of `to` back to `from`'s. The caller
    /// then calls [`brush_io::BrushPreset::apply_tip`].
    fn reset(self, to: &mut brush_io::BrushPreset, from: &brush_io::BrushPreset) {
        macro_rules! copy {
            ($($($p:ident).+),*) => {{ $(to.$($p).+ = Clone::clone(&from.$($p).+);)* }};
        }
        section_fields!(self, copy)
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

    /// The window (Sub Tool Detail): macro areas and their categories on the
    /// left, the category's settings on the right, the brush's stroke at the
    /// foot, redrawn live as the settings change.
    pub(crate) fn advanced_tool_window(&mut self, ctx: &egui::Context) {
        let Some(mut section) = self.advanced_tool else { return };
        let mut open = true;
        egui::Window::new(tr("Advanced Tool Settings")).open(&mut open).default_size(Vec2::new(600.0, 560.0)).resizable(true).show(
            ctx,
            |ui| {
                let index = self.brush.min(self.brushes.len() - 1);
                // A fixed body height: a window has the whole screen available, which would push the preview off it.
                let body_h = 420.0;
                let base = crate::brush_ui::baseline_of(&mut self.brush_baselines, &self.brushes, index).clone();
                let mut reset = false;
                let body = Vec2::new(ui.available_width(), body_h);
                ui.allocate_ui_with_layout(body, egui::Layout::left_to_right(egui::Align::Min), |ui| {
                    ui.set_height(body_h);
                    ui.vertical(|ui| {
                        ui.set_width(150.0);
                        egui::ScrollArea::vertical().id_salt("advanced_tool_sidebar").max_height(body_h).show(ui, |ui| {
                            for (area, sections) in Section::AREAS {
                                ui.add_space(4.0);
                                ui.label(egui::RichText::new(tr(area)).small().strong().color(theme::text_dim()));
                                for &s in sections {
                                    let edited = s.differs(&self.brushes[index], &base);
                                    // A dot marks the categories changed from the preset.
                                    let title = if edited { format!("{} •", tr(s.title())) } else { tr(s.title()).to_string() };
                                    let item = ui.selectable_value(&mut section, s, title);
                                    if edited {
                                        item.on_hover_text(tr("Changed from the preset"));
                                    }
                                }
                            }
                        });
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(tr(section.title())).strong());
                            if section != Section::Export {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let edited = section.differs(&self.brushes[index], &base);
                                    reset = crate::brush_ui::reset_glyph(ui, edited, tr("Back to the preset's values for this category"))
                                        .clicked();
                                });
                            }
                        });
                        ui.small(section.help());
                        ui.separator();
                        egui::ScrollArea::vertical().id_salt("advanced_tool_scroll").max_height(body_h - 50.0).auto_shrink([false, true]).show(ui, |ui| {
                            self.advanced_section(ui, section);
                        });
                    });
                });
                if reset {
                    section.reset(&mut self.brushes[index], &base);
                    self.brushes[index].apply_tip();
                    self.brush_px = None;
                }
                ui.separator();
                self.detail_preview_strip(ui, index);
            },
        );
        self.advanced_tool = open.then_some(section);
    }

    /// The stroke preview at the foot of Sub Tool Detail: the brush's name and
    /// its stroke in the main colour on paper, re-rendered as settings change.
    fn detail_preview_strip(&mut self, ui: &mut egui::Ui, index: usize) {
        let total = self.brushes.len();
        let height = DETAIL_PREVIEW_SIZE[1] as f32 + 8.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 3.0, Color32::from_gray(235));
        let texture = self.detail_preview.get(ui.ctx(), index, &self.brushes[index].settings, total);
        let size = texture.size_vec2();
        let size = size * (rect.width() / size.x).min(1.0);
        let shown = egui::Rect::from_center_size(rect.center(), size);
        let [r, g, b] = {
            let c = Color32::from(self.color);
            [c.r(), c.g(), c.b()]
        };
        painter.image(texture.id(), shown, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::from_rgb(r, g, b));
        let name = &self.brushes[index].settings.name;
        painter.text(rect.left_top() + Vec2::new(6.0, 4.0), egui::Align2::LEFT_TOP, name, egui::FontId::proportional(11.5), Color32::from_gray(60));
    }

    fn advanced_section(&mut self, ui: &mut egui::Ui, section: Section) {
        let index = self.brush.min(self.brushes.len() - 1);
        let mut tip_changed = false;
        let preset = &mut self.brushes[index];
        let brush = &mut preset.settings;
        match section {
            Section::BrushSize => {
                prop_slider(ui, "Brush size", &mut brush.size, 0.5..=2000.0, " px", 1, true);
                ui.checkbox(&mut brush.pressure_size, tr("Pen pressure"));
                if brush.pressure_size {
                    prop_percent(ui, "Minimum", &mut brush.min_size_ratio, 0.0..=100.0);
                    pressure_curve(ui, "size_curve", &mut brush.size_curve);
                }
                dynamics_row(ui, "Size with speed, tilt, random", &mut brush.size_dynamics, false);
            }
            Section::Ink => {
                ui.horizontal(|ui| {
                    ui.label(tr("Mode"));
                    ui.selectable_value(&mut brush.mode, BrushMode::Paint, tr("Paint"));
                    ui.selectable_value(&mut brush.mode, BrushMode::Erase, tr("Erase"));
                });
                prop_percent(ui, "Opacity", &mut brush.opacity, 0.0..=100.0);
                prop_percent(ui, "Density (flow)", &mut brush.density, 0.0..=100.0);
                ui.checkbox(&mut brush.pressure_density, tr("Density from pen pressure"));
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
                ui.small(tr("One variation per stroke."));
            }
            Section::AntiAliasing => {
                ui.horizontal(|ui| {
                    for aa in AntiAlias::ALL {
                        ui.selectable_value(&mut brush.antialias, aa, tr(aa.name()));
                    }
                });
                prop_percent(ui, "Hardness", &mut brush.hardness, 0.0..=100.0);
            }
            Section::BrushTip => {
                ui.horizontal(|ui| {
                    ui.label(if preset.tip.is_some() { "Image tip" } else { "Round tip" });
                    if ui.button(tr("Load image...")).on_hover_text(tr("A PNG: transparency (or dark pixels) is the ink")).clicked() {
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
                    if preset.tip.is_some() && ui.button(tr("Round")).clicked() {
                        preset.tip = None;
                        tip_changed = true;
                    }
                });
                let mut thickness = preset.thickness * 100.0;
                if ui.add(egui::Slider::new(&mut thickness, 1.0..=100.0).suffix("%").text(tr("Thickness"))).changed() {
                    preset.thickness = thickness / 100.0;
                    tip_changed = true;
                }
                if ui.add(egui::Slider::new(&mut preset.angle, -180.0..=180.0).suffix("°").text(tr("Angle"))).changed() {
                    tip_changed = true;
                }
                let brush = &mut preset.settings;
                dynamics_row(ui, "Thickness with pressure, speed, tilt, random", &mut brush.thickness_dynamics, true);
                ui.label(tr("Rotation"));
                ui.indent("adv_rotation", |ui| {
                    ui.checkbox(&mut brush.rotation.follow_direction, tr("Follows the stroke direction"));
                    ui.checkbox(&mut brush.rotation.follow_tilt, tr("Follows the pen's lean"))
                        .on_hover_text(tr("Turns the tip towards where the pen leans (tablets with tilt, Apple Pencil)"));
                    ui.horizontal(|ui| {
                        ui.label(tr("Random flip"));
                        ui.checkbox(&mut brush.rotation.random_flip_x, tr("horizontal"));
                        ui.checkbox(&mut brush.rotation.random_flip_y, tr("vertical"));
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
                percent_slider(ui, &mut brush.tilt_side, "Pencil side with tilt", 0.0..=100.0);
                ui.small(tr("As the pen leans the tip lies down along it, like a pencil's side: wider, flatter, lighter."));
            }
            Section::Spraying => {
                let s = &mut brush.scatter;
                let mut on = s.is_on();
                if ui.checkbox(&mut on, tr("Spray")).changed() {
                    *s = if on {
                        painter_core::Scatter { amount: 1.0, count: 4, count_jitter: 0.5, both_axes: true }
                    } else {
                        Default::default()
                    };
                }
                if on {
                    percent_slider(ui, &mut s.amount, "Spread (of the size)", 0.0..=400.0);
                    let mut count = s.count.max(1) as f32;
                    if ui.add(egui::Slider::new(&mut count, 1.0..=16.0).step_by(1.0).text(tr("Dabs per point"))).changed() {
                        s.count = count as u32;
                    }
                    percent_slider(ui, &mut s.count_jitter, "Count random", 0.0..=100.0);
                    ui.checkbox(&mut s.both_axes, tr("Along the stroke too"));
                }
            }
            Section::Stroke => {
                prop_percent(ui, "Spacing (of the size)", &mut brush.spacing, 1.0..=200.0);
            }
            Section::Texture => {
                ui.horizontal(|ui| {
                    ui.label(if preset.texture.is_some() { "Texture on" } else { "No texture" });
                    if ui.button(tr("Load image...")).on_hover_text(tr("A PNG; light areas hold the ink back")).clicked() {
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
                    if preset.texture.is_some() && ui.button(tr("Remove")).clicked() {
                        preset.texture = None;
                        tip_changed = true;
                    }
                });
                if let Some(t) = &mut preset.texture {
                    tip_changed |=
                        ui.add(egui::Slider::new(&mut t.scale, 5.0..=1000.0).suffix("%").text(tr("Scale")).logarithmic(true)).changed();
                    tip_changed |= percent_slider(ui, &mut t.density, "Density", 0.0..=100.0);
                    tip_changed |= ui.checkbox(&mut t.invert, tr("Invert")).changed();
                }
            }
            Section::SecondBrush => {
                ui.horizontal(|ui| {
                    ui.label(if preset.dual.is_some() { "Second tip on" } else { "No second tip" });
                    if ui.button(tr("Load image...")).clicked() {
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
                    if preset.dual.is_some() && ui.button(tr("Remove")).clicked() {
                        preset.dual = None;
                        tip_changed = true;
                    }
                });
                if let Some(d) = &mut preset.dual {
                    tip_changed |= percent_slider(ui, &mut d.scale, "Size (of the main tip)", 5.0..=800.0);
                }
            }
            Section::Dynamics => {
                // The two pressure curves side by side when there's room, stacked otherwise.
                ui.columns(2, |cols| {
                    cols[0].checkbox(&mut brush.pressure_size, tr("Size from pen pressure"));
                    if brush.pressure_size {
                        pressure_curve(&mut cols[0], "dyn_size_curve", &mut brush.size_curve);
                    }
                    cols[1].checkbox(&mut brush.pressure_density, tr("Density (flow) from pen pressure"));
                    if brush.pressure_density {
                        pressure_curve(&mut cols[1], "dyn_density_curve", &mut brush.density_curve);
                    }
                });
                ui.separator();
                dynamics_row(ui, "Size with speed, tilt, random", &mut brush.size_dynamics, false);
                dynamics_row(ui, "Density with speed, tilt, random", &mut brush.density_dynamics, false);
                dynamics_row(ui, "Opacity per dab", &mut brush.opacity_dynamics, true);
                dynamics_row(ui, "Tip thickness", &mut brush.thickness_dynamics, true);
            }
            Section::Correction => {
                let mut stabilization = brush.stabilization as f32;
                if prop_slider(ui, "Stabilization", &mut stabilization, 0.0..=100.0, "", 0, false) {
                    brush.stabilization = stabilization.round() as u32;
                }
                egui::ComboBox::from_label(tr("Smoothing")).selected_text(tr(brush.smoothing.name())).show_ui(ui, |ui| {
                    for method in SmoothingMethod::ALL {
                        ui.selectable_value(&mut brush.smoothing, method, tr(method.name()));
                    }
                });
            }
            Section::StartingEnding => {
                let mut on = brush.taper_start > 0.0;
                if ui.checkbox(&mut on, tr("Starting: grow from thin and light")).changed() {
                    brush.taper_start = if on { 60.0 } else { 0.0 };
                }
                if on {
                    prop_slider(ui, "Length", &mut brush.taper_start, 1.0..=1000.0, " px", 0, true);
                }
                ui.small(tr("Endings need the whole stroke and aren't drawn yet."));
            }
            Section::Export => {
                ui.label(tr("This brush, as:"));
                if ui.button(tr("Painter brush (.rustbrush)...")).clicked() {
                    self.export_brushes(Some(index));
                }
                if ui.button(tr("Photoshop brush (.abr)...")).on_hover_text(tr("Tip, texture, dual brush and the main dynamics")).clicked() {
                    self.export_abr(index);
                }
                if ui
                    .button(tr("Clip Studio sub tool (.sut)..."))
                    .on_hover_text(tr("Written into a sub tool exported from Clip Studio, used as a template"))
                    .clicked()
                {
                    self.export_sut(index);
                }
                if ui.button(tr("Krita bundle (.bundle)...")).on_hover_text(tr("Preset with its tip; open it in Krita with Settings → Manage Resources → Import")).clicked() {
                    self.export_brush_as(index, BrushFormat::Krita);
                }
                if ui.button(tr("Krita preset (.kpp)...")).on_hover_text(tr("One preset with its tip and texture inside (Krita 5)")).clicked() {
                    self.export_brush_as(index, BrushFormat::KritaPreset);
                }
                if ui.button(tr("MyPaint brush (.myb)...")).clicked() {
                    self.export_brush_as(index, BrushFormat::MyPaint);
                }
                if ui.button(tr("GIMP brush (.gbr / .vbr)...")).on_hover_text(tr("The tip: .gbr for a bitmap tip, .vbr for a round one")).clicked() {
                    self.export_brush_as(index, BrushFormat::Gimp);
                }
                let template = crate::files::app_data_dir().map(|d| d.join(SUT_TEMPLATE));
                match template.filter(|t| t.is_file()) {
                    Some(_) => {
                        if ui.small_button(tr("Choose another Clip Studio template...")).clicked() {
                            self.choose_sut_template();
                        }
                    }
                    None => {
                        ui.small(tr("Clip Studio export needs one sub tool exported from Clip Studio (any brush) as a template; you'll be asked for it."));
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

    /// Exports one brush for Krita, MyPaint or GIMP.
    pub(crate) fn export_brush_as(&mut self, index: usize, format: BrushFormat) {
        let preset = self.brushes[index].clone();
        let encoded = match format {
            BrushFormat::Krita => brush_io::krita::write_bundle(std::slice::from_ref(&preset), preset.name()).map(|b| ("bundle", b)),
            BrushFormat::KritaPreset => brush_io::krita::write_kpp(&preset).map(|(b, _)| ("kpp", b)),
            BrushFormat::MyPaint => brush_io::mypaint::write(&preset).map(|b| ("myb", b)),
            BrushFormat::Gimp => brush_io::gimp::write(&preset),
        };
        let (ext, bytes) = match encoded {
            Ok(x) => x,
            Err(err) => {
                self.status = format!("Export failed: {err}");
                return;
            }
        };
        let label = format.label();
        let name = format!("{}.{ext}", preset.name().replace(['/', '\\', ':'], "_"));
        let Some(path) = crate::files::FileDialog::new().add_filter(label, &[ext]).set_file_name(name).save_file() else {
            return;
        };
        self.status = match std::fs::write(&path, bytes) {
            Ok(()) => {
                crate::files::offer_export(&path);
                format!("Exported {} for {label} ({})", path.display(), format.caveat())
            }
            Err(err) => format!("{label} export failed: {err}"),
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
            self.status = tr("Clip Studio export needs a sub tool exported from Clip Studio as a template").into();
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
        ui.label(tr("Curve"));
        for (label, gamma) in [("Soft", 0.6), ("Linear", 1.0), ("Firm", 1.7)] {
            if ui.small_button(label).clicked() {
                *curve = PressureCurve::gamma(gamma);
            }
        }
    });
    crate::curve::curve_editor(ui, id, curve);
    ui.label(egui::RichText::new(tr("Drag points · double-click to add · right-click to remove")).small().color(theme::text_dim()));
}

/// Other painting programs' brush formats a brush can be exported to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BrushFormat {
    Krita,
    /// One `.kpp`, its tip and texture embedded (Krita 5).
    KritaPreset,
    MyPaint,
    Gimp,
}

impl BrushFormat {
    fn label(self) -> &'static str {
        match self {
            BrushFormat::Krita | BrushFormat::KritaPreset => "Krita",
            BrushFormat::MyPaint => "MyPaint",
            BrushFormat::Gimp => "GIMP",
        }
    }

    /// What the other program's engine does differently.
    fn caveat(self) -> &'static str {
        match self {
            BrushFormat::Krita | BrushFormat::KritaPreset => "pixel engine; colour jitter and starting taper aren't carried over",
            BrushFormat::MyPaint => "MyPaint's engine; texture, dual tip and bitmap tips aren't carried over",
            BrushFormat::Gimp => "the tip and spacing; GIMP keeps dynamics separately",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brush_io::BrushPreset;
    use painter_core::BrushSettings;

    #[test]
    fn every_category_is_in_one_area() {
        let all: Vec<Section> = Section::AREAS.iter().flat_map(|(_, s)| s.iter().copied()).collect();
        assert_eq!(all.len(), 13);
        for (i, s) in all.iter().enumerate() {
            assert!(!all[i + 1..].contains(s), "{s:?} listed twice");
        }
    }

    #[test]
    fn category_reset_restores_only_its_settings() {
        let base = BrushPreset::new(BrushSettings::g_pen());
        let mut edited = base.clone();
        edited.settings.spacing = 1.7;
        edited.settings.stabilization = 80;
        edited.thickness = 0.3;
        assert!(Section::Stroke.differs(&edited, &base));
        assert!(Section::BrushTip.differs(&edited, &base));
        assert!(!Section::Ink.differs(&edited, &base));
        assert!(!Section::Export.differs(&edited, &base));
        Section::Stroke.reset(&mut edited, &base);
        Section::BrushTip.reset(&mut edited, &base);
        assert_eq!(edited.settings.spacing, base.settings.spacing);
        assert_eq!(edited.thickness, base.thickness);
        assert_eq!(edited.settings.stabilization, 80, "Correction untouched");
        assert!(Section::Correction.differs(&edited, &base));
    }
}
