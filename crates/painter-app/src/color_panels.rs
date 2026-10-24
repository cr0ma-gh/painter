//! Colour palettes after Clip Studio's: Color Slider (RGB, HSV, CMYK),
//! Color Set, Color History, Intermediate Color, Approximate Color and
//! Color Mixing. Written from how those palettes behave, not their code.
//!
//! Sets, history and the palettes' settings are kept between launches in
//! `colors.json` in the app's data folder; the mixing canvas lives for the
//! session.

use std::time::{Duration, Instant};
use crate::i18n::tr;

use eframe::egui::{self, ecolor::Hsva, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use serde::{Deserialize, Serialize};

use crate::app::PainterApp;
use crate::theme;

pub(crate) type Rgb = [u8; 3];

/// Most colours Color History keeps.
const HISTORY: usize = 64;

/// CMYK (0..1 each) from sRGB (0..1 each): the plain device formula, no
/// colour profile. K is what no ink can add; C, M, Y are what's left.
pub(crate) fn rgb_to_cmyk([r, g, b]: [f32; 3]) -> [f32; 4] {
    let k = 1.0 - r.max(g).max(b);
    if k >= 1.0 - 1e-6 {
        return [0.0, 0.0, 0.0, 1.0];
    }
    let d = 1.0 - k;
    [(1.0 - r - k) / d, (1.0 - g - k) / d, (1.0 - b - k) / d, k]
}

pub(crate) fn cmyk_to_rgb([c, m, y, k]: [f32; 4]) -> [f32; 3] {
    [(1.0 - c) * (1.0 - k), (1.0 - m) * (1.0 - k), (1.0 - y) * (1.0 - k)]
}

fn to_rgb(c: Hsva) -> Rgb {
    let [r, g, b, _] = c.to_srgba_unmultiplied();
    [r, g, b]
}

fn from_rgb(c: Rgb) -> Hsva {
    Hsva::from_srgb(c)
}

fn color32(c: Rgb) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

fn lerp(a: Rgb, b: Rgb, t: f32) -> Rgb {
    std::array::from_fn(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}

/// Which sliders the Color Slider palette shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum SliderMode {
    #[default]
    Rgb,
    Hsv,
    Cmyk,
}

/// A named list of colours (Color Set).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ColorSet {
    pub(crate) name: String,
    pub(crate) colors: Vec<Rgb>,
}

impl ColorSet {
    /// Greys, saturated hues, pastels, darks and skin tones.
    pub(crate) fn default_set() -> Self {
        let mut colors = vec![[0, 0, 0], [255, 255, 255]];
        let hues = [0.0, 0.04, 0.08, 0.12, 0.17, 0.25, 0.33, 0.45, 0.5, 0.55, 0.6, 0.67, 0.75, 0.83, 0.92];
        for (s, v) in [(0.9, 0.95), (0.35, 1.0), (0.85, 0.55)] {
            for h in hues {
                colors.push(to_rgb(Hsva::new(h, s, v, 1.0)));
            }
        }
        for g in [32, 64, 96, 128, 160, 192, 224] {
            colors.push([g, g, g]);
        }
        colors.extend([
            [255, 228, 214],
            [250, 210, 190],
            [240, 190, 160],
            [225, 165, 130],
            [200, 140, 105],
            [165, 110, 80],
            [120, 80, 60],
            [80, 55, 40],
        ]);
        Self { name: "Default color set".into(), colors }
    }
}

/// Everything the colour palettes keep between launches.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ColorData {
    /// Newest first.
    pub(crate) history: Vec<Rgb>,
    pub(crate) sets: Vec<ColorSet>,
    pub(crate) active_set: usize,
    /// Intermediate Color's corners: top left, top right, bottom left, bottom right.
    pub(crate) corners: [Rgb; 4],
    /// Intermediate Color's cells per side.
    pub(crate) steps: u32,
    /// Approximate Color's reach in value (across) and saturation (down), 0..1.
    pub(crate) approx_value: f32,
    pub(crate) approx_saturation: f32,
    pub(crate) slider_mode: SliderMode,
    /// Colours kept under the mixing canvas.
    pub(crate) mixing_palette: Vec<Rgb>,
    /// Extended Color Selector: shape and colour model.
    pub(crate) gamut_shape: crate::gamut_selector::GamutShape,
    pub(crate) gamut_model: crate::gamut_selector::ColorModel,
}

impl Default for ColorData {
    fn default() -> Self {
        Self {
            history: Vec::new(),
            sets: vec![ColorSet::default_set()],
            active_set: 0,
            corners: [[255, 255, 255], [255, 0, 0], [255, 255, 255], [0, 0, 0]],
            steps: 15,
            approx_value: 0.4,
            approx_saturation: 0.4,
            slider_mode: SliderMode::Rgb,
            mixing_palette: vec![
                [230, 40, 50],
                [245, 150, 50],
                [250, 230, 70],
                [70, 200, 90],
                [20, 100, 110],
                [50, 110, 230],
                [40, 200, 230],
                [140, 60, 230],
            ],
            gamut_shape: Default::default(),
            gamut_model: Default::default(),
        }
    }
}

/// The colour palettes' state.
pub(crate) struct ColorPanels {
    pub(crate) data: ColorData,
    written: Option<String>,
    checked: Option<Instant>,
    mixing: MixingCanvas,
}

impl ColorPanels {
    pub(crate) fn load() -> Self {
        let path = crate::files::app_data_dir().map(|d| d.join("colors.json"));
        let text = path.and_then(|p| std::fs::read_to_string(p).ok());
        let mut data: ColorData = text.as_deref().and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default();
        if data.sets.is_empty() {
            data.sets.push(ColorSet::default_set());
        }
        data.steps = data.steps.clamp(2, 41);
        Self { data, written: text, checked: None, mixing: MixingCanvas::new() }
    }

    /// Writes `colors.json` when something changed (checked every couple of seconds).
    pub(crate) fn autosave(&mut self) {
        if self.checked.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
            return;
        }
        self.checked = Some(Instant::now());
        let Some(dir) = crate::files::app_data_dir() else { return };
        let Ok(json) = serde_json::to_string_pretty(&self.data) else { return };
        if self.written.as_deref() == Some(json.as_str()) {
            return;
        }
        let _ = std::fs::create_dir_all(&dir);
        let tmp = dir.join("colors.tmp");
        if std::fs::write(&tmp, &json).is_ok() && std::fs::rename(&tmp, dir.join("colors.json")).is_ok() {
            self.written = Some(json);
        }
    }

    /// Remembers `color` as just used (Color History).
    pub(crate) fn used(&mut self, color: Rgb) {
        let h = &mut self.data.history;
        if h.first() == Some(&color) {
            return;
        }
        h.retain(|c| *c != color);
        h.insert(0, color);
        h.truncate(HISTORY);
    }
}

/// A row of square swatches that wraps; returns the clicked one and the right-clicked one.
fn swatch_grid(ui: &mut egui::Ui, colors: &[Rgb], cell: f32, current: Rgb) -> (Option<usize>, Option<usize>) {
    let per_row = ((ui.available_width() + 1.0) / (cell + 1.0)).floor().max(1.0) as usize;
    let (mut clicked, mut menu) = (None, None);
    for (r, row) in colors.chunks(per_row).enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::splat(1.0);
            for (k, &c) in row.iter().enumerate() {
                let i = r * per_row + k;
                let (rect, response) = ui.allocate_exact_size(Vec2::splat(cell), Sense::click());
                ui.painter().rect_filled(rect, 1.0, color32(c));
                if c == current || response.hovered() {
                    let ring = if c == current { theme::accent() } else { theme::strong() };
                    ui.painter().rect_stroke(rect.shrink(0.5), 1.0, Stroke::new(1.5_f32, ring), egui::StrokeKind::Inside);
                }
                let response = response.on_hover_text(format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]));
                if response.clicked() {
                    clicked = Some(i);
                }
                if response.secondary_clicked() {
                    menu = Some(i);
                }
            }
        });
    }
    (clicked, menu)
}

/// A horizontal slider drawn over the colour gradient it changes. `paint(t)`
/// gives the colour at position `t`; returns the new value when dragged.
fn gradient_slider(ui: &mut egui::Ui, value: f32, paint: impl Fn(f32) -> Rgb) -> Option<f32> {
    let height = ui.spacing().interact_size.y * 0.8;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(60.0), height), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let n = 48;
    let mut mesh = egui::Mesh::default();
    for k in 0..=n {
        let t = k as f32 / n as f32;
        let x = rect.left() + rect.width() * t;
        let c = color32(paint(t));
        mesh.colored_vertex(Pos2::new(x, rect.top()), c);
        mesh.colored_vertex(Pos2::new(x, rect.bottom()), c);
        if k > 0 {
            let i = (k * 2) as u32;
            mesh.add_triangle(i - 2, i - 1, i);
            mesh.add_triangle(i - 1, i + 1, i);
        }
    }
    painter.add(mesh);
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0_f32, theme::line()), egui::StrokeKind::Inside);
    let x = rect.left() + rect.width() * value.clamp(0.0, 1.0);
    painter.rect_stroke(
        Rect::from_center_size(Pos2::new(x, rect.center().y), Vec2::new(5.0, rect.height() + 2.0)),
        1.0,
        Stroke::new(1.5_f32, Color32::WHITE),
        egui::StrokeKind::Middle,
    );
    if response.is_pointer_button_down_on() {
        let p = response.interact_pointer_pos()?;
        return Some(((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0));
    }
    None
}

impl PainterApp {
    /// Sets the main colour from a palette, remembering it.
    fn pick_color(&mut self, c: Rgb) {
        self.color = from_rgb(c);
    }

    /// Color Slider: RGB, HSV or CMYK sliders over their gradients, with
    /// numbers, and the main / sub colour chips.
    pub(crate) fn color_slider_panel(&mut self, ui: &mut egui::Ui) {
        let mode = &mut self.colors.data.slider_mode;
        ui.horizontal(|ui| {
            for (m, label) in [(SliderMode::Rgb, "RGB"), (SliderMode::Hsv, "HSV"), (SliderMode::Cmyk, "CMYK")] {
                ui.selectable_value(mode, m, label);
            }
        });
        let mode = *mode;
        let rgb = to_rgb(self.color);
        let rgbf = rgb.map(|v| v as f32 / 255.0);
        let mut changed: Option<Hsva> = None;
        let row = |ui: &mut egui::Ui, label: &str, value: f32, max: f32, paint: &dyn Fn(f32) -> Rgb| -> Option<f32> {
            let mut out = None;
            ui.horizontal(|ui| {
                ui.add_sized(Vec2::new(14.0, 0.0), egui::Label::new(egui::RichText::new(label).small()));
                let mut shown = (value * max).round();
                let field = ui.add(egui::DragValue::new(&mut shown).range(0.0..=max).speed(1.0));
                if field.changed() {
                    out = Some(shown / max);
                }
                if let Some(t) = gradient_slider(ui, value, paint) {
                    out = Some(t);
                }
            });
            out
        };
        match mode {
            SliderMode::Rgb => {
                for (i, name) in ["R", "G", "B"].iter().enumerate() {
                    let paint = |t: f32| {
                        let mut c = rgb;
                        c[i] = (t * 255.0).round() as u8;
                        c
                    };
                    if let Some(t) = row(ui, name, rgbf[i], 255.0, &paint) {
                        let mut c = rgb;
                        c[i] = (t * 255.0).round() as u8;
                        changed = Some(from_rgb(c));
                    }
                }
            }
            SliderMode::Hsv => {
                let c = self.color;
                let parts = [c.h, c.s, c.v];
                for (i, (name, max)) in [("H", 360.0), ("S", 100.0), ("V", 100.0)].iter().enumerate() {
                    let with = |t: f32| {
                        let mut p = parts;
                        p[i] = t;
                        Hsva::new(p[0], p[1], p[2], 1.0)
                    };
                    let paint = |t: f32| to_rgb(with(t));
                    if let Some(t) = row(ui, name, parts[i], *max, &paint) {
                        changed = Some(Hsva { a: c.a, ..with(t) });
                    }
                }
            }
            SliderMode::Cmyk => {
                let cmyk = rgb_to_cmyk(rgbf);
                for (i, name) in ["C", "M", "Y", "K"].iter().enumerate() {
                    let with = |t: f32| {
                        let mut p = cmyk;
                        p[i] = t;
                        cmyk_to_rgb(p).map(|v| (v * 255.0).round() as u8)
                    };
                    if let Some(t) = row(ui, name, cmyk[i], 100.0, &with) {
                        changed = Some(from_rgb(with(t)));
                    }
                }
                ui.small(tr("CMYK without a colour profile (device values)."));
            }
        }
        if let Some(c) = changed {
            self.color = c;
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            self.color_chips(ui, 24.0);
            if crate::widgets::transparent_chip(ui, self.transparent, 22.0).clicked() {
                self.toggle_transparent();
            }
            let [r, g, b] = rgb;
            let mut hex = format!("{r:02X}{g:02X}{b:02X}");
            ui.label(tr("#"));
            if ui.add(egui::TextEdit::singleline(&mut hex).desired_width(60.0).font(egui::TextStyle::Monospace)).changed() {
                if let Ok(v) = u32::from_str_radix(hex.trim().trim_start_matches('#'), 16) {
                    if hex.trim().trim_start_matches('#').len() == 6 {
                        self.color = from_rgb([(v >> 16) as u8, (v >> 8) as u8, v as u8]);
                    }
                }
            }
        });
    }

    /// Color History: the colours drawn with lately, newest first.
    pub(crate) fn color_history_panel(&mut self, ui: &mut egui::Ui) {
        if self.colors.data.history.is_empty() {
            ui.label(egui::RichText::new(tr("Colours you draw with appear here.")).color(theme::text_dim()));
            return;
        }
        let history = self.colors.data.history.clone();
        let (clicked, _) = swatch_grid(ui, &history, 18.0, to_rgb(self.color));
        if let Some(i) = clicked {
            self.pick_color(history[i]);
        }
        if ui.small_button(tr("Clear")).clicked() {
            self.colors.data.history.clear();
        }
    }

    /// Color Set: named palettes; click a swatch to use it, right-click to
    /// replace or delete it; add the current colour; import / export GIMP palettes.
    pub(crate) fn color_set_panel(&mut self, ui: &mut egui::Ui) {
        let current = to_rgb(self.color);
        let data = &mut self.colors.data;
        data.active_set = data.active_set.min(data.sets.len() - 1);
        let mut import = false;
        let mut export = false;
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("color_set")
                .selected_text(&data.sets[data.active_set].name)
                .width(ui.available_width() - 60.0)
                .show_ui(ui, |ui| {
                    for i in 0..data.sets.len() {
                        ui.selectable_value(&mut data.active_set, i, &data.sets[i].name);
                    }
                });
            ui.menu_button(tr("☰"), |ui| {
                if ui.button(tr("New Color Set")).clicked() {
                    data.sets.push(ColorSet { name: format!("Color set {}", data.sets.len() + 1), colors: Vec::new() });
                    data.active_set = data.sets.len() - 1;
                    ui.close_menu();
                }
                let set = &mut data.sets[data.active_set];
                ui.horizontal(|ui| {
                    ui.label(tr("Name"));
                    ui.text_edit_singleline(&mut set.name);
                });
                if ui.add_enabled(data.sets.len() > 1, egui::Button::new(tr("Delete Color Set"))).clicked() {
                    data.sets.remove(data.active_set);
                    data.active_set = 0;
                    ui.close_menu();
                }
                ui.separator();
                import = ui.button(tr("Import GIMP Palette (.gpl)...")).clicked();
                export = ui.button(tr("Export as GIMP Palette (.gpl)...")).clicked();
            });
        });
        let set = &mut data.sets[data.active_set];
        let (clicked, menu) = swatch_grid(ui, &set.colors, 20.0, current);
        if let Some(i) = menu {
            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("color_set_menu"), i));
        }
        let mut pick = clicked.map(|i| set.colors[i]);
        let menu_for = ui.memory(|m| m.data.get_temp::<usize>(egui::Id::new("color_set_menu")));
        if let Some(i) = menu_for.filter(|&i| i < set.colors.len()) {
            egui::Window::new(tr("Swatch"))
                .id(egui::Id::new("color_set_menu_window"))
                .title_bar(false)
                .resizable(false)
                .fixed_pos(ui.ctx().pointer_hover_pos().unwrap_or_default())
                .show(ui.ctx(), |ui| {
                    let mut done = false;
                    if ui.button(tr("Replace with the current colour")).clicked() {
                        set.colors[i] = current;
                        done = true;
                    }
                    if ui.button(tr("Delete")).clicked() {
                        set.colors.remove(i);
                        done = true;
                    }
                    if ui.button(tr("Cancel")).clicked() || done {
                        ui.memory_mut(|m| m.data.remove::<usize>(egui::Id::new("color_set_menu")));
                    }
                });
        }
        ui.horizontal(|ui| {
            if ui.button(tr("+ Add current colour")).clicked() && !set.colors.contains(&current) {
                set.colors.push(current);
            }
        });
        if let Some(c) = pick.take() {
            self.pick_color(c);
        }
        if import {
            if let Some(path) = crate::files::FileDialog::new().add_filter("GIMP palette", &["gpl"]).pick_file() {
                self.import_color_set(&path);
            }
        }
        if export {
            let set = self.colors.data.sets[self.colors.data.active_set].clone();
            if let Some(path) = crate::files::FileDialog::new()
                .add_filter("GIMP palette", &["gpl"])
                .set_file_name(format!("{}.gpl", set.name))
                .save_file()
            {
                self.status = match std::fs::write(&path, write_gpl(&set)) {
                    Ok(()) => {
                        crate::files::offer_export(&path);
                        format!("Exported {}", path.display())
                    }
                    Err(err) => format!("Couldn't export the colour set: {err}"),
                };
            }
        }
    }

    pub(crate) fn import_color_set(&mut self, path: &std::path::Path) {
        match std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|t| read_gpl(&t)) {
            Ok(mut set) => {
                if set.name.is_empty() {
                    set.name = path.file_stem().map_or("Imported".into(), |s| s.to_string_lossy().into_owned());
                }
                self.status = format!("Imported {} colours into \"{}\"", set.colors.len(), set.name);
                self.colors.data.sets.push(set);
                self.colors.data.active_set = self.colors.data.sets.len() - 1;
            }
            Err(err) => self.status = format!("Couldn't import {}: {err}", path.display()),
        }
    }

    /// Intermediate Color: a grid blending four corner colours. Click a
    /// corner to set it to the current colour, a cell to use it.
    pub(crate) fn intermediate_color_panel(&mut self, ui: &mut egui::Ui) {
        let data = &mut self.colors.data;
        let n = data.steps.clamp(2, 41) as usize;
        let side = (ui.available_width() - 4.0).clamp(120.0, 600.0);
        let corner = (side / 10.0).clamp(14.0, 26.0);
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme::well());
        let grid = rect.shrink(corner * 0.6);
        let cell = grid.width() / n as f32;
        let at = |i: usize, j: usize| {
            let (u, v) = (i as f32 / (n - 1) as f32, j as f32 / (n - 1) as f32);
            lerp(lerp(data.corners[0], data.corners[1], u), lerp(data.corners[2], data.corners[3], u), v)
        };
        let current = to_rgb(self.color);
        for j in 0..n {
            for i in 0..n {
                let r = Rect::from_min_size(grid.min + Vec2::new(i as f32 * cell, j as f32 * cell), Vec2::splat(cell)).shrink(0.5);
                painter.rect_filled(r, 0.0, color32(at(i, j)));
            }
        }
        let corners = [
            rect.left_top(),
            rect.right_top() - Vec2::new(corner, 0.0),
            rect.left_bottom() - Vec2::new(0.0, corner),
            rect.right_bottom() - Vec2::splat(corner),
        ];
        let corner_rects: Vec<Rect> = corners.iter().map(|p| Rect::from_min_size(*p, Vec2::splat(corner))).collect();
        for (k, r) in corner_rects.iter().enumerate() {
            painter.rect_filled(*r, 1.0, color32(data.corners[k]));
            painter.rect_stroke(*r, 1.0, Stroke::new(1.0_f32, theme::strong()), egui::StrokeKind::Inside);
        }
        let hover = response.hover_pos();
        if let Some(p) = hover {
            if grid.contains(p) {
                let (i, j) = (((p.x - grid.left()) / cell) as usize, ((p.y - grid.top()) / cell) as usize);
                let r = Rect::from_min_size(
                    grid.min + Vec2::new(i.min(n - 1) as f32 * cell, j.min(n - 1) as f32 * cell),
                    Vec2::splat(cell),
                );
                painter.rect_stroke(r, 0.0, Stroke::new(1.5_f32, Color32::WHITE), egui::StrokeKind::Middle);
            }
        }
        if response.clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                if let Some(k) = corner_rects.iter().position(|r| r.contains(p)) {
                    data.corners[k] = current;
                } else if grid.contains(p) {
                    let (i, j) = (((p.x - grid.left()) / cell) as usize, ((p.y - grid.top()) / cell) as usize);
                    let c = at(i.min(n - 1), j.min(n - 1));
                    self.pick_color(c);
                }
            }
        }
        let data = &mut self.colors.data;
        ui.horizontal(|ui| {
            let mut steps = data.steps;
            if ui.add(egui::Slider::new(&mut steps, 2..=41).text(tr("Steps"))).changed() {
                data.steps = steps;
            }
        });
        ui.small(tr("Click a corner to set it to the current colour."));
    }

    /// Approximate Color: colours near the current one, value changing
    /// across and saturation down; the sliders set how far they reach.
    pub(crate) fn approximate_color_panel(&mut self, ui: &mut egui::Ui) {
        let data = &mut self.colors.data;
        ui.horizontal(|ui| {
            let mut v = data.approx_value * 100.0;
            if ui.add(egui::Slider::new(&mut v, 5.0..=100.0).suffix("%").text(tr("V"))).changed() {
                data.approx_value = v / 100.0;
            }
            let mut s = data.approx_saturation * 100.0;
            if ui.add(egui::Slider::new(&mut s, 5.0..=100.0).suffix("%").text(tr("S"))).changed() {
                data.approx_saturation = s / 100.0;
            }
        });
        let (rv, rs) = (data.approx_value, data.approx_saturation);
        let n = 15usize;
        let half = (n / 2) as f32;
        let base = self.color;
        let at = |i: usize, j: usize| {
            let v = (base.v + (i as f32 - half) / half * rv * 0.5).clamp(0.0, 1.0);
            let s = (base.s - (j as f32 - half) / half * rs * 0.5).clamp(0.0, 1.0);
            Hsva::new(base.h, s, v, base.a)
        };
        let side = (ui.available_width() - 4.0).clamp(120.0, 600.0);
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
        let painter = ui.painter_at(rect);
        let cell = side / n as f32;
        for j in 0..n {
            for i in 0..n {
                let r = Rect::from_min_size(rect.min + Vec2::new(i as f32 * cell, j as f32 * cell), Vec2::splat(cell)).shrink(0.5);
                painter.rect_filled(r, 0.0, color32(to_rgb(at(i, j))));
            }
        }
        let mid = Rect::from_min_size(rect.min + Vec2::splat(half * cell), Vec2::splat(cell));
        painter.rect_stroke(mid, 0.0, Stroke::new(2.0_f32, Color32::from_rgb(230, 30, 30)), egui::StrokeKind::Middle);
        if response.clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                let (i, j) = (((p.x - rect.left()) / cell) as usize, ((p.y - rect.top()) / cell) as usize);
                self.color = at(i.min(n - 1), j.min(n - 1));
            }
        }
    }

    /// Color Mixing: a small canvas to mix colours on, like paint on a palette.
    pub(crate) fn color_mixing_panel(&mut self, ui: &mut egui::Ui) {
        let main = to_rgb(self.color);
        let mut picked = None;
        let mix = &mut self.colors.mixing;
        let width = ui.available_width().max(120.0);
        let shown = Vec2::new(width, width * MIX_H as f32 / MIX_W as f32);
        let (rect, response) = ui.allocate_exact_size(shown, Sense::click_and_drag());
        crate::widgets::checkerboard(ui.painter(), rect, 8.0);
        let texture = mix.texture(ui.ctx());
        ui.painter().image(texture.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        let to_canvas =
            |p: Pos2| [(p.x - rect.left()) / rect.width() * MIX_W as f32, (p.y - rect.top()) / rect.height() * MIX_H as f32];
        if let Some(p) = response.interact_pointer_pos() {
            let at = to_canvas(p);
            if response.drag_started() || (response.clicked() && mix.tool != MixTool::Picker) {
                mix.begin(at);
            }
            if mix.tool == MixTool::Picker {
                if response.clicked() || response.dragged() {
                    picked = mix.sample(at);
                }
            } else if response.dragged() || response.clicked() {
                mix.stroke_to(at, main);
            }
        }
        if response.drag_stopped() || response.clicked() {
            mix.end();
        }
        if let Some(p) = response.hover_pos() {
            let r = mix.size * rect.width() / MIX_W as f32 * 0.5;
            ui.painter().circle_stroke(p, r.max(2.0), Stroke::new(1.0_f32, Color32::from_white_alpha(180)));
        }
        // The palette strip.
        let palette = self.colors.data.mixing_palette.clone();
        let mut remove = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::splat(2.0);
            for (i, c) in palette.iter().enumerate() {
                let (r, resp) = ui.allocate_exact_size(Vec2::new(24.0, 14.0), Sense::click());
                ui.painter().rect_filled(r, 3.0, color32(*c));
                let resp = resp.on_hover_text(tr("Click to use, right-click to remove"));
                if resp.clicked() {
                    picked = Some(*c);
                }
                if resp.secondary_clicked() {
                    remove = Some(i);
                }
            }
            if ui.small_button(tr("+")).on_hover_text(tr("Keep the current colour here")).clicked() && !palette.contains(&main) {
                self.colors.data.mixing_palette.push(main);
            }
        });
        if let Some(i) = remove {
            self.colors.data.mixing_palette.remove(i);
        }
        let mix = &mut self.colors.mixing;
        ui.horizontal(|ui| {
            for (tool, label, hint) in [
                (MixTool::Brush, "Brush", "Paint with the current colour"),
                (MixTool::Blend, "Blend", "Smear and mix the paint"),
                (MixTool::Eraser, "Erase", "Wipe paint off"),
                (MixTool::Picker, "Pick", "Take a colour from the mixing area"),
            ] {
                ui.selectable_value(&mut mix.tool, tool, label).on_hover_text(hint);
            }
        });
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut mix.size, 2.0..=80.0).text(tr("Size")));
            if ui
                .add_enabled(!mix.undo.is_empty(), egui::Button::image(crate::icons::Icon::Undo.image(14.0, theme::text())))
                .on_hover_text(tr("Undo"))
                .clicked()
            {
                mix.undo();
            }
            if ui
                .add_enabled(!mix.redo.is_empty(), egui::Button::image(crate::icons::Icon::Redo.image(14.0, theme::text())))
                .on_hover_text(tr("Redo"))
                .clicked()
            {
                mix.redo();
            }
            if ui.button(tr("Clear")).clicked() {
                mix.clear();
            }
        });
        if let Some(c) = picked {
            self.pick_color(c);
        }
    }
}

// ---- Color Mixing canvas -----------------------------------------------------------------

const MIX_W: usize = 320;
const MIX_H: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MixTool {
    Brush,
    Blend,
    Eraser,
    Picker,
}

/// Premultiplied RGBA, 0..1.
type Px = [f32; 4];

struct MixingCanvas {
    pixels: Vec<Px>,
    tool: MixTool,
    size: f32,
    last: Option<[f32; 2]>,
    /// The paint the blend tool carries along.
    carried: Option<Px>,
    undo: Vec<Vec<Px>>,
    redo: Vec<Vec<Px>>,
    texture: Option<egui::TextureHandle>,
    dirty: bool,
}

impl MixingCanvas {
    fn new() -> Self {
        Self {
            pixels: vec![[0.0; 4]; MIX_W * MIX_H],
            tool: MixTool::Brush,
            size: 24.0,
            last: None,
            carried: None,
            undo: Vec::new(),
            redo: Vec::new(),
            texture: None,
            dirty: true,
        }
    }

    fn texture(&mut self, ctx: &egui::Context) -> egui::TextureHandle {
        if self.dirty || self.texture.is_none() {
            let rgba: Vec<Color32> = self
                .pixels
                .iter()
                .map(|p| {
                    Color32::from_rgba_premultiplied(
                        (p[0] * 255.0) as u8,
                        (p[1] * 255.0) as u8,
                        (p[2] * 255.0) as u8,
                        (p[3] * 255.0) as u8,
                    )
                })
                .collect();
            let image = egui::ColorImage { size: [MIX_W, MIX_H], pixels: rgba };
            match &mut self.texture {
                Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                None => self.texture = Some(ctx.load_texture("color_mixing", image, egui::TextureOptions::LINEAR)),
            }
            self.dirty = false;
        }
        self.texture.clone().expect("made above")
    }

    fn begin(&mut self, at: [f32; 2]) {
        self.undo.push(self.pixels.clone());
        if self.undo.len() > 20 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.last = Some(at);
        self.carried = None;
    }

    fn end(&mut self) {
        self.last = None;
        self.carried = None;
    }

    /// Dabs from the last point to `to`, a quarter of the size apart.
    fn stroke_to(&mut self, to: [f32; 2], color: Rgb) {
        let from = self.last.unwrap_or(to);
        let d = ((to[0] - from[0]).powi(2) + (to[1] - from[1]).powi(2)).sqrt();
        let step = (self.size * 0.25).max(1.0);
        let n = (d / step).ceil().max(1.0) as usize;
        for k in 1..=n {
            let t = k as f32 / n as f32;
            self.dab([from[0] + (to[0] - from[0]) * t, from[1] + (to[1] - from[1]) * t], color);
        }
        if n == 0 || d == 0.0 {
            self.dab(to, color);
        }
        self.last = Some(to);
        self.dirty = true;
    }

    fn dab(&mut self, c: [f32; 2], color: Rgb) {
        let r = self.size * 0.5;
        let (x0, x1) = ((c[0] - r).floor().max(0.0) as usize, ((c[0] + r).ceil() as usize).min(MIX_W));
        let (y0, y1) = ((c[1] - r).floor().max(0.0) as usize, ((c[1] + r).ceil() as usize).min(MIX_H));
        let falloff = |x: usize, y: usize| {
            let d = ((x as f32 + 0.5 - c[0]).powi(2) + (y as f32 + 0.5 - c[1]).powi(2)).sqrt() / r.max(0.5);
            (1.0 - d).clamp(0.0, 1.0).powf(0.7)
        };
        match self.tool {
            MixTool::Brush => {
                let paint = [color[0] as f32 / 255.0, color[1] as f32 / 255.0, color[2] as f32 / 255.0, 1.0];
                for y in y0..y1 {
                    for x in x0..x1 {
                        let a = falloff(x, y) * 0.6;
                        let p = &mut self.pixels[y * MIX_W + x];
                        for i in 0..4 {
                            p[i] = paint[i] * a + p[i] * (1.0 - a);
                        }
                    }
                }
            }
            MixTool::Eraser => {
                for y in y0..y1 {
                    for x in x0..x1 {
                        let a = falloff(x, y) * 0.7;
                        let p = &mut self.pixels[y * MIX_W + x];
                        for v in p.iter_mut() {
                            *v *= 1.0 - a;
                        }
                    }
                }
            }
            MixTool::Blend => {
                // What's under the brush, averaged by weight.
                let (mut sum, mut weight) = ([0.0f32; 4], 0.0f32);
                for y in y0..y1 {
                    for x in x0..x1 {
                        let w = falloff(x, y);
                        let p = self.pixels[y * MIX_W + x];
                        for i in 0..4 {
                            sum[i] += p[i] * w;
                        }
                        weight += w;
                    }
                }
                if weight <= 0.0 {
                    return;
                }
                let under = sum.map(|v| v / weight);
                // The brush carries paint and picks some up as it goes.
                let carried = match self.carried {
                    Some(c) => std::array::from_fn(|i| c[i] * 0.7 + under[i] * 0.3),
                    None => under,
                };
                self.carried = Some(carried);
                for y in y0..y1 {
                    for x in x0..x1 {
                        let a = falloff(x, y) * 0.5;
                        let p = &mut self.pixels[y * MIX_W + x];
                        for i in 0..4 {
                            p[i] = carried[i] * a + p[i] * (1.0 - a);
                        }
                    }
                }
            }
            MixTool::Picker => {}
        }
    }

    /// The colour at `at`, if there's paint there.
    fn sample(&self, at: [f32; 2]) -> Option<Rgb> {
        let (x, y) = (at[0].floor() as isize, at[1].floor() as isize);
        if x < 0 || y < 0 || x as usize >= MIX_W || y as usize >= MIX_H {
            return None;
        }
        let p = self.pixels[y as usize * MIX_W + x as usize];
        (p[3] > 0.02).then(|| std::array::from_fn(|i| (p[i] / p[3] * 255.0).round().clamp(0.0, 255.0) as u8))
    }

    fn undo(&mut self) {
        if let Some(previous) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.pixels, previous));
            self.dirty = true;
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.pixels, next));
            self.dirty = true;
        }
    }

    fn clear(&mut self) {
        self.begin([0.0, 0.0]);
        self.pixels.iter_mut().for_each(|p| *p = [0.0; 4]);
        self.end();
        self.dirty = true;
    }
}

// ---- GIMP palettes ---------------------------------------------------------------------

/// A GIMP palette (`GIMP Palette`, `Name:`, then `R G B name` lines).
pub(crate) fn read_gpl(text: &str) -> Result<ColorSet, String> {
    let mut lines = text.lines();
    if !lines.next().is_some_and(|l| l.trim().starts_with("GIMP Palette")) {
        return Err("not a GIMP palette".into());
    }
    let mut set = ColorSet { name: String::new(), colors: Vec::new() };
    for line in lines {
        let line = line.trim();
        if let Some(name) = line.strip_prefix("Name:") {
            set.name = name.trim().to_string();
            continue;
        }
        if line.is_empty() || line.starts_with('#') || line.starts_with("Columns:") {
            continue;
        }
        let v: Vec<u8> = line.split_whitespace().take(3).filter_map(|s| s.parse().ok()).collect();
        if v.len() == 3 {
            set.colors.push([v[0], v[1], v[2]]);
        }
    }
    if set.colors.is_empty() {
        return Err("no colours in the palette".into());
    }
    Ok(set)
}

pub(crate) fn write_gpl(set: &ColorSet) -> String {
    let mut out = format!("GIMP Palette\nName: {}\nColumns: 8\n#\n", set.name);
    for c in &set.colors {
        out += &format!("{:3} {:3} {:3}\t#{:02X}{:02X}{:02X}\n", c[0], c[1], c[2], c[0], c[1], c[2]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmyk_round_trips_and_has_the_usual_values() {
        assert_eq!(rgb_to_cmyk([0.0, 0.0, 0.0]), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(rgb_to_cmyk([1.0, 1.0, 1.0]), [0.0, 0.0, 0.0, 0.0]);
        let red = rgb_to_cmyk([1.0, 0.0, 0.0]);
        assert!((red[1] - 1.0).abs() < 1e-6 && (red[2] - 1.0).abs() < 1e-6 && red[0] == 0.0 && red[3] == 0.0);
        for rgb in [[0.2, 0.5, 0.9], [0.7, 0.1, 0.3], [0.5, 0.5, 0.5]] {
            let back = cmyk_to_rgb(rgb_to_cmyk(rgb));
            assert!((0..3).all(|i| (back[i] - rgb[i]).abs() < 1e-5), "{rgb:?} → {back:?}");
        }
    }

    #[test]
    fn gimp_palettes_read_back() {
        let set = ColorSet { name: "Skin".into(), colors: vec![[255, 228, 214], [80, 55, 40]] };
        let back = read_gpl(&write_gpl(&set)).unwrap();
        assert_eq!(back, set);
        assert!(read_gpl("hello").is_err());
    }

    #[test]
    fn history_keeps_recent_unique_colours() {
        let mut panels = ColorPanels { data: ColorData::default(), written: None, checked: None, mixing: MixingCanvas::new() };
        panels.used([1, 2, 3]);
        panels.used([4, 5, 6]);
        panels.used([1, 2, 3]);
        assert_eq!(panels.data.history, vec![[1, 2, 3], [4, 5, 6]]);
        for k in 0..100u8 {
            panels.used([k, 0, 0]);
        }
        assert_eq!(panels.data.history.len(), HISTORY);
    }

    #[test]
    fn mixing_paints_blends_and_undoes() {
        let mut mix = MixingCanvas::new();
        mix.begin([50.0, 50.0]);
        mix.stroke_to([100.0, 50.0], [255, 0, 0]);
        mix.end();
        assert_eq!(mix.sample([75.0, 50.0]), Some([255, 0, 0]));
        mix.begin([150.0, 50.0]);
        mix.stroke_to([200.0, 50.0], [0, 0, 255]);
        mix.end();
        // Smearing red into blue gives something in between.
        mix.tool = MixTool::Blend;
        mix.begin([90.0, 50.0]);
        mix.stroke_to([170.0, 50.0], [0, 0, 0]);
        mix.end();
        let c = mix.sample([160.0, 50.0]).unwrap();
        assert!(c[0] > 20 && c[2] > 20, "mixed: {c:?}");
        mix.undo();
        assert_eq!(mix.sample([160.0, 50.0]), Some([0, 0, 255]));
        mix.redo();
        assert_ne!(mix.sample([160.0, 50.0]), Some([0, 0, 255]));
        mix.clear();
        assert_eq!(mix.sample([75.0, 50.0]), None);
    }
}
