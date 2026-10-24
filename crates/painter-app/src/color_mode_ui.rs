//! Edit → Mode (Photoshop's Image → Mode): Bitmap, Grayscale, Duotone,
//! Indexed Color, RGB Color, CMYK Color, Lab Color, Multichannel; the bit
//! depth (8 bits per channel); Color Table; Assign Profile.
//!
//! Bitmap, Duotone, Indexed Color and CMYK ask how; Bitmap, Indexed and
//! Multichannel merge the layers into one, as in Photoshop. While in a mode,
//! painting keeps to it: grey, black and white, the colour table, the press's
//! colours. See `painter_core::color_mode`.

use eframe::egui::{self, Color32, Rect, Sense, Vec2};
use painter_core::color_mode::{self, BitmapMethod, ColorMode};
use painter_core::duotone::{Duotone, DuotoneInk};
use painter_core::indexed::{self, Dither, Forced, IndexedOptions, PaletteKind};
use painter_core::{Layer, LayerKind, PressureCurve, TileCoord, TileStore};
use painter_render::Plane;

use crate::app::PainterApp;
use crate::theme;

/// The Bitmap dialog's choices.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BitmapDialog {
    /// 0 threshold, 1 diffusion, 2 halftone.
    method: usize,
    cell: f32,
    angle: f32,
}

impl Default for BitmapDialog {
    fn default() -> Self {
        Self { method: 1, cell: 8.0, angle: 45.0 }
    }
}

/// Duotone Options: the inks, each with its colour, name and curve.
#[derive(Clone)]
pub(crate) struct DuotoneDialog {
    duotone: Duotone,
    /// The ink whose curve is open.
    curve_open: Option<usize>,
}

/// Indexed Color's choices.
#[derive(Clone, Copy)]
pub(crate) struct IndexedDialog {
    options: IndexedOptions,
}

/// The colour table being edited, and the one the image has.
#[derive(Clone)]
pub(crate) struct ColorTableDialog {
    table: Vec<[u8; 3]>,
    original: Vec<[u8; 3]>,
    picked: Option<usize>,
}

/// Inks added when the type asks for more of them.
fn extra_ink(i: usize) -> DuotoneInk {
    let (name, color) =
        [("Brown", [160, 110, 60]), ("Tan", [215, 175, 120]), ("Grey", [140, 140, 140]), ("Blue", [30, 90, 170])][i % 4];
    DuotoneInk { name: name.into(), color, curve: PressureCurve::from_points(vec![[0.0, 0.0], [0.5, 0.5], [1.0, 0.7]]) }
}

impl PainterApp {
    pub(crate) fn set_color_mode(&mut self, mode: ColorMode) {
        // CMYK and Duotone ask again to change their profile or inks.
        if mode == self.doc.mode && !matches!(mode, ColorMode::Cmyk | ColorMode::Duotone) {
            return;
        }
        match mode {
            ColorMode::Bitmap => self.bitmap_dialog = Some(BitmapDialog::default()),
            // CMYK asks for the press profile (also to change it on a CMYK document).
            ColorMode::Cmyk => self.cmyk_dialog = Some(crate::cmyk_ui::CmykDialog::new(self.doc.cmyk.as_ref())),
            ColorMode::Duotone => {
                let duotone = self.doc.duotone.clone().unwrap_or_else(|| self.last_duotone.clone());
                self.duotone_dialog = Some(DuotoneDialog { duotone, curve_open: None });
            }
            ColorMode::Indexed => self.indexed_dialog = Some(IndexedDialog { options: self.last_indexed }),
            ColorMode::Grayscale => {
                self.next_label = Some("Mode: Grayscale".into());
                self.checkpoint();
                // Duotone's pixels are already its grey.
                if self.doc.mode != ColorMode::Duotone {
                    let canvas = self.doc.canvas();
                    color_mode::gray_layers(&mut self.doc.layers, canvas);
                }
                self.enter_mode(ColorMode::Grayscale);
                self.status = "Grayscale: colours are grey from now on (Edit > Mode > RGB Color brings colour back)".into();
            }
            ColorMode::Rgb => {
                self.next_label = Some("Mode: RGB Color".into());
                self.checkpoint();
                self.enter_mode(ColorMode::Rgb);
                self.status = "RGB Color: colour can be used again".into();
            }
            ColorMode::Lab => {
                self.next_label = Some("Mode: Lab Color".into());
                self.checkpoint();
                self.enter_mode(ColorMode::Lab);
                self.status = "Lab Color: channels Lightness, a and b (Channels panel); colours are kept".into();
            }
            ColorMode::Multichannel => {
                self.next_label = Some("Mode: Multichannel".into());
                self.checkpoint();
                self.flatten_into(ColorMode::Multichannel, "Multichannel", |rgba| {
                    // Spot inks print on white: nothing is see-through.
                    painter_core::print::on_paper(rgba).as_chunks::<3>().0.iter().flat_map(|c| [c[0], c[1], c[2], 255]).collect()
                });
                self.status = "Multichannel: cyan, magenta and yellow spot channels (layers merged)".into();
            }
        }
    }

    /// The document now in `mode`: Duotone's inks laid in when leaving it for
    /// colour, the colour table dropped outside Indexed, the right kind of profile.
    fn enter_mode(&mut self, mode: ColorMode) {
        if self.doc.mode == ColorMode::Duotone && !mode.is_gray() {
            color_mode::ink_duotone(&mut self.doc);
        }
        if mode != ColorMode::Indexed {
            self.doc.palette.clear();
        }
        self.doc.mode = mode;
        use painter_core::color_profile::ColorProfile;
        if mode.is_gray() && !self.doc.profile.is_gray() {
            self.doc.profile = ColorProfile::GrayGamma22;
        } else if !mode.is_gray() && self.doc.profile.is_gray() {
            self.doc.profile = ColorProfile::Srgb;
        }
        self.resync_gpu();
    }

    /// The visible image (inks laid in) through `make` into one layer named `name`, in `mode`.
    fn flatten_into(&mut self, mode: ColorMode, name: &str, make: impl FnOnce(&[u8]) -> Vec<u8>) {
        let (w, h) = self.doc.canvas();
        let mut look = self.doc.clone();
        color_mode::ink_duotone(&mut look);
        let pixels = make(&painter_core::io::flatten(&look));
        let id = self.doc.alloc_id();
        let mut layer = Layer::raster(id, name);
        layer.kind = LayerKind::Raster(TileStore::from_rgba(w, h, &pixels));
        self.doc.layers = vec![layer];
        self.doc.active = id;
        self.enter_mode(mode);
    }

    /// The image flattened into one black-and-white layer.
    pub(crate) fn convert_to_bitmap(&mut self, method: BitmapMethod) {
        let (w, h) = self.doc.canvas();
        let started = std::time::Instant::now();
        self.next_label = Some("Mode: Bitmap".into());
        self.checkpoint();
        self.flatten_into(ColorMode::Bitmap, "Bitmap", |flat| color_mode::to_bitmap(flat, w as usize, h as usize, method));
        self.status = format!("Bitmap: flattened to black and white in {} ms", started.elapsed().as_millis());
    }

    /// Grey printed with `duotone`'s inks (or new inks for a Duotone document).
    pub(crate) fn convert_to_duotone(&mut self, duotone: Duotone) {
        self.next_label = Some(if self.doc.mode == ColorMode::Duotone { "Duotone Options" } else { "Mode: Duotone" }.into());
        self.checkpoint();
        if !self.doc.mode.is_gray() {
            let canvas = self.doc.canvas();
            color_mode::gray_layers(&mut self.doc.layers, canvas);
        }
        self.status = format!("{} with {}: painting is grey, the inks colour it", duotone.kind_name(), ink_names(&duotone));
        self.last_duotone = duotone.clone();
        self.doc.duotone = Some(duotone);
        self.enter_mode(ColorMode::Duotone);
    }

    /// The image flattened into the colours of a table built from it.
    pub(crate) fn convert_to_indexed(&mut self, options: IndexedOptions) {
        let (w, h) = self.doc.canvas();
        let started = std::time::Instant::now();
        let mut look = self.doc.clone();
        color_mode::ink_duotone(&mut look);
        let flat = painter_core::io::flatten(&look);
        let table = match indexed::build_palette(&flat, &options) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("Indexed Color: {e}");
                return;
            }
        };
        self.next_label = Some("Mode: Indexed Color".into());
        self.checkpoint();
        self.last_indexed = options;
        let pixels = indexed::index_image(&flat, w as usize, h as usize, &table, &options);
        let id = self.doc.alloc_id();
        let mut layer = Layer::raster(id, "Index");
        layer.kind = LayerKind::Raster(TileStore::from_rgba(w, h, &pixels));
        self.doc.layers = vec![layer];
        self.doc.active = id;
        let count = table.len();
        self.enter_mode(ColorMode::Indexed);
        self.doc.palette = table;
        self.status = format!(
            "Indexed Color: {count} colours, layers merged, in {} ms (Edit > Mode > Color Table)",
            started.elapsed().as_millis()
        );
    }

    /// After an edit of `coords` on a colour plane: back to the mode's colours
    /// (grey, black and white, the table, the press's), and only the channels
    /// being edited changed.
    pub(crate) fn finish_edit(&mut self, plane: Plane, before: &TileStore, coords: impl IntoIterator<Item = TileCoord>) {
        let coords: Vec<TileCoord> = coords.into_iter().collect();
        self.restrict_channels(plane, before, coords.iter().copied());
        if !matches!(plane, Plane::Color(_)) {
            return;
        }
        let mode = self.doc.mode;
        let palette = std::mem::take(&mut self.doc.palette);
        // Out while the tiles are borrowed; back below.
        let proofer = self.proofer.proofer.take();
        if let Some(tiles) = self.plane_tiles_mut(plane) {
            match mode {
                ColorMode::Rgb | ColorMode::Lab | ColorMode::Multichannel => {}
                ColorMode::Grayscale | ColorMode::Duotone => color_mode::gray_tiles(tiles, Some(&coords)),
                ColorMode::Bitmap => color_mode::threshold_tiles(tiles, &coords),
                ColorMode::Indexed => indexed::snap_tiles(tiles, &coords, &palette),
                // Only the new pixels, so colours don't drift from edit to edit.
                ColorMode::Cmyk => {
                    if let Some(p) = &proofer {
                        p.proof_changed(tiles, before, &coords);
                    }
                }
            }
        }
        self.doc.palette = palette;
        self.proofer.proofer = proofer;
        // Gray and Monochrome layers keep to their colours too.
        self.keep_layer_expression(plane, &coords);
    }

    pub(crate) fn bitmap_window(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.bitmap_dialog else { return };
        let (mut open, mut go, mut cancel) = (true, false, false);
        egui::Window::new("Bitmap")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("The image becomes one black-and-white layer.");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut d.method, 0, "50% Threshold");
                    ui.selectable_value(&mut d.method, 1, "Diffusion Dither");
                    ui.selectable_value(&mut d.method, 2, "Halftone Screen");
                });
                if d.method == 2 {
                    ui.add(egui::Slider::new(&mut d.cell, 3.0..=64.0).text("Cell size").suffix(" px"));
                    ui.add(egui::Slider::new(&mut d.angle, 0.0..=90.0).text("Angle").suffix("°"));
                }
                ui.small("Layers are merged. Undo brings them back.");
                ui.horizontal(|ui| {
                    go = ui.button("OK").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        self.bitmap_dialog = if go || cancel || !open { None } else { Some(d) };
        if go {
            let method = match d.method {
                0 => BitmapMethod::Threshold,
                1 => BitmapMethod::Diffusion,
                _ => BitmapMethod::Halftone { cell: d.cell, angle: d.angle },
            };
            self.convert_to_bitmap(method);
        }
    }

    /// Duotone Options: type, preset, and each ink's colour, name and curve.
    pub(crate) fn duotone_window(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.duotone_dialog.take() else { return };
        let (mut open, mut ok, mut cancel) = (true, false, false);
        egui::Window::new("Duotone Options")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Preset");
                    egui::ComboBox::from_id_salt("duotone_preset").selected_text("Choose…").width(200.0).show_ui(ui, |ui| {
                        for (name, preset) in Duotone::presets() {
                            if ui.selectable_label(false, name).clicked() {
                                d.duotone = preset;
                                d.curve_open = None;
                            }
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.label("Type");
                    let mut count = d.duotone.inks.len();
                    for (n, name) in [(1, "Monotone"), (2, "Duotone"), (3, "Tritone"), (4, "Quadtone")] {
                        ui.selectable_value(&mut count, n, name);
                    }
                    while d.duotone.inks.len() < count {
                        d.duotone.inks.push(extra_ink(d.duotone.inks.len()));
                    }
                    d.duotone.inks.truncate(count.max(1));
                });
                ui.separator();
                for (i, ink) in d.duotone.inks.iter_mut().enumerate() {
                    ui.push_id(i, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(format!("Ink {}", i + 1));
                            let open = d.curve_open == Some(i);
                            if ui.add(curve_thumb(&ink.curve)).on_hover_text("Edit the ink's curve").clicked() {
                                d.curve_open = if open { None } else { Some(i) };
                            }
                            ui.color_edit_button_srgb(&mut ink.color);
                            ui.add(egui::TextEdit::singleline(&mut ink.name).desired_width(140.0));
                        });
                        if d.curve_open == Some(i) {
                            ui.small("Grey's darkness (across) → how much of this ink (up)");
                            crate::curve::curve_editor(ui, &format!("duotone_curve_{i}"), &mut ink.curve);
                        }
                    });
                }
                ui.separator();
                // The greys, white to black, as they'll print.
                let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(260.0), 22.0), Sense::hover());
                let table = d.duotone.table();
                let steps = 64;
                for s in 0..steps {
                    let v = 255 - s * 255 / (steps - 1);
                    let x0 = rect.left() + rect.width() * s as f32 / steps as f32;
                    let x1 = rect.left() + rect.width() * (s + 1) as f32 / steps as f32;
                    let c = table[v];
                    ui.painter().rect_filled(Rect::from_x_y_ranges(x0..=x1, rect.y_range()), 0.0, Color32::from_rgb(c[0], c[1], c[2]));
                }
                ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(1.0_f32, theme::line()), egui::StrokeKind::Inside);
                if !self.doc.mode.is_gray() {
                    ui.small("The image is made grey first; layers stay.");
                }
                ui.horizontal(|ui| {
                    ok = ui.button("OK").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if ok {
            self.convert_to_duotone(d.duotone);
        } else if open && !cancel {
            self.duotone_dialog = Some(d);
        }
    }

    /// Indexed Color: the table, how many colours, forced ones, transparency, dither.
    pub(crate) fn indexed_window(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.indexed_dialog else { return };
        let (mut open, mut ok, mut cancel) = (true, false, false);
        egui::Window::new("Indexed Color")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let o = &mut d.options;
                egui::Grid::new("indexed_options").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                    ui.label("Palette");
                    egui::ComboBox::from_id_salt("indexed_palette").selected_text(o.palette.name()).width(170.0).show_ui(ui, |ui| {
                        for p in PaletteKind::ALL {
                            ui.selectable_value(&mut o.palette, p, p.name());
                        }
                    });
                    ui.end_row();
                    ui.label("Colors");
                    ui.add_enabled(o.palette.takes_count(), egui::DragValue::new(&mut o.colors).range(2..=256));
                    ui.end_row();
                    ui.label("Forced");
                    egui::ComboBox::from_id_salt("indexed_forced")
                        .selected_text(match o.forced {
                            Forced::None => "None",
                            Forced::BlackAndWhite => "Black and White",
                            Forced::Primaries => "Primaries",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut o.forced, Forced::None, "None");
                            ui.selectable_value(&mut o.forced, Forced::BlackAndWhite, "Black and White");
                            ui.selectable_value(&mut o.forced, Forced::Primaries, "Primaries");
                        });
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut o.transparency, "Transparency").on_hover_text("Keep see-through pixels (else they become white)");
                    ui.end_row();
                    ui.label("Dither");
                    egui::ComboBox::from_id_salt("indexed_dither")
                        .selected_text(match o.dither {
                            Dither::None => "None",
                            Dither::Diffusion => "Diffusion",
                            Dither::Pattern => "Pattern",
                            Dither::Noise => "Noise",
                        })
                        .show_ui(ui, |ui| {
                            for (v, name) in [
                                (Dither::None, "None"),
                                (Dither::Diffusion, "Diffusion"),
                                (Dither::Pattern, "Pattern"),
                                (Dither::Noise, "Noise"),
                            ] {
                                ui.selectable_value(&mut o.dither, v, name);
                            }
                        });
                    ui.end_row();
                    ui.label("Amount");
                    let mut pct = o.amount * 100.0;
                    if ui.add_enabled(o.dither != Dither::None, egui::Slider::new(&mut pct, 1.0..=100.0).suffix(" %")).changed() {
                        o.amount = pct / 100.0;
                    }
                    ui.end_row();
                });
                ui.small("Layers are merged into one. Undo brings them back.");
                ui.horizontal(|ui| {
                    ok = ui.button("OK").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        self.indexed_dialog = if ok || cancel || !open { None } else { Some(d) };
        if ok {
            self.convert_to_indexed(d.options);
        }
    }

    pub(crate) fn open_color_table(&mut self) {
        if self.doc.mode != ColorMode::Indexed || self.doc.palette.is_empty() {
            self.status = "Color Table: for Indexed Color images (Edit > Mode > Indexed Color)".into();
            return;
        }
        let table = self.doc.palette.clone();
        self.color_table = Some(ColorTableDialog { original: table.clone(), table, picked: None });
    }

    /// Color Table: the image's colours; change one and it changes wherever it's used.
    pub(crate) fn color_table_window(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.color_table.take() else { return };
        let (mut open, mut ok, mut cancel) = (true, false, false);
        egui::Window::new("Color Table")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Table");
                    egui::ComboBox::from_id_salt("color_table_preset").selected_text("Custom").show_ui(ui, |ui| {
                        let n = d.original.len();
                        // By position, as in Photoshop: colour i takes the table's colour i.
                        let presets: [(&str, Vec<[u8; 3]>); 5] = [
                            ("Original", d.original.clone()),
                            ("Grayscale", indexed::grays(n)),
                            ("Spectrum", indexed::spectrum()),
                            ("Black Body", indexed::black_body()),
                            ("Web", indexed::web()),
                        ];
                        for (name, colors) in presets {
                            if ui.selectable_label(false, name).clicked() {
                                d.table = (0..n).map(|i| colors.get(i).copied().unwrap_or(d.table[i])).collect();
                            }
                        }
                    });
                    #[cfg(not(any(target_os = "ios", target_os = "android")))]
                    {
                        if ui.button("Load...").on_hover_text("A Photoshop colour table (.act)").clicked() {
                            if let Some(path) = rfd::FileDialog::new().add_filter("Color table", &["act"]).pick_file() {
                                if let Ok(bytes) = std::fs::read(&path) {
                                    let colors: Vec<[u8; 3]> = bytes.as_chunks::<3>().0.iter().take(256).copied().collect();
                                    let n = d.table.len();
                                    d.table = (0..n).map(|i| colors.get(i).copied().unwrap_or(d.table[i])).collect();
                                }
                            }
                        }
                        if ui.button("Save...").clicked() {
                            if let Some(path) =
                                rfd::FileDialog::new().add_filter("Color table", &["act"]).set_file_name("colors.act").save_file()
                            {
                                // 256 RGB entries, then the count and no transparent index.
                                let mut bytes: Vec<u8> = (0..256).flat_map(|i| d.table.get(i).copied().unwrap_or([0; 3])).collect();
                                bytes.extend((d.table.len() as u16).to_be_bytes());
                                bytes.extend([0xFF, 0xFF]);
                                let _ = std::fs::write(path, bytes);
                            }
                        }
                    }
                });
                ui.small(format!("{} colours. Click one to change it.", d.table.len()));
                let cell = 18.0;
                for row in d.table.chunks(16).enumerate() {
                    let (r, colors) = row;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        for (k, c) in colors.iter().enumerate() {
                            let i = r * 16 + k;
                            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(cell), Sense::click());
                            ui.painter().rect_filled(rect, 0.0, Color32::from_rgb(c[0], c[1], c[2]));
                            let edge = if d.picked == Some(i) {
                                egui::Stroke::new(2.0_f32, theme::accent())
                            } else {
                                egui::Stroke::new(1.0_f32, theme::line())
                            };
                            ui.painter().rect_stroke(rect, 0.0, edge, egui::StrokeKind::Inside);
                            if resp.on_hover_text(format!("{i}: #{:02X}{:02X}{:02X}", c[0], c[1], c[2])).clicked() {
                                d.picked = Some(i);
                            }
                        }
                    });
                }
                if let Some(i) = d.picked {
                    ui.horizontal(|ui| {
                        ui.label(format!("Colour {i}"));
                        ui.color_edit_button_srgb(&mut d.table[i]);
                    });
                }
                ui.horizontal(|ui| {
                    ok = ui.button("OK").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if ok {
            if d.table != d.original {
                self.next_label = Some("Color Table".into());
                self.checkpoint();
                indexed::recolor_layers(&mut self.doc.layers, &d.original, &d.table);
                self.doc.palette = d.table;
                self.resync_gpu();
            }
        } else if open && !cancel {
            self.color_table = Some(d);
        }
    }

    /// The main colour in the mode's own numbers: C M Y K (with the press's
    /// warning), L a b, or the colour table's index. `false` when there are none.
    pub(crate) fn mode_readout(&mut self, ui: &mut egui::Ui) -> bool {
        let [r, g, b, _] = self.color.to_srgba_unmultiplied();
        let text = match self.doc.mode {
            ColorMode::Cmyk => return self.cmyk_readout(ui),
            ColorMode::Lab => {
                let [l, a, bb] = painter_core::lab::from_rgb([r, g, b]);
                format!("L{:.0} a{:.0} b{:.0}", l, a, bb)
            }
            ColorMode::Indexed if !self.doc.palette.is_empty() => {
                let i = indexed::nearest(&self.doc.palette, [r, g, b]);
                let c = self.doc.palette[i];
                format!("#{:02X}{:02X}{:02X} [{i}]", c[0], c[1], c[2])
            }
            _ => return false,
        };
        ui.label(egui::RichText::new(text).monospace().small().color(theme::text_dim()));
        true
    }

    /// Edit → Mode.
    pub(crate) fn mode_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Mode", |ui| {
            for mode in ColorMode::ALL {
                let asks = matches!(mode, ColorMode::Bitmap | ColorMode::Cmyk | ColorMode::Duotone | ColorMode::Indexed);
                let label = if asks { format!("{}...", mode.name()) } else { mode.name().to_string() };
                let hint = match mode {
                    ColorMode::Bitmap => "Black and white only; layers are merged",
                    ColorMode::Grayscale => "Greys only",
                    ColorMode::Duotone => "Grey printed with one to four inks",
                    ColorMode::Indexed => "Up to 256 colours; layers are merged",
                    ColorMode::Rgb => "Any colour",
                    ColorMode::Cmyk => "Colours a press profile can print",
                    ColorMode::Lab => "Lightness, a and b channels",
                    ColorMode::Multichannel => "Cyan, magenta and yellow spot channels; layers are merged",
                };
                if ui.radio(self.doc.mode == mode, label).on_hover_text(hint).clicked() {
                    ui.close_menu();
                    self.set_color_mode(mode);
                }
            }
            ui.separator();
            let _ = ui.radio(true, "8 Bits/Channel");
            for bits in ["16 Bits/Channel", "32 Bits/Channel"] {
                ui.add_enabled(false, egui::RadioButton::new(false, bits))
                    .on_disabled_hover_text("Not yet: Painter paints and blends at 8 bits per channel");
            }
            ui.separator();
            if ui
                .add_enabled(self.doc.mode == ColorMode::Indexed, egui::Button::new("Color Table..."))
                .on_disabled_hover_text("For Indexed Color images")
                .clicked()
            {
                ui.close_menu();
                self.open_color_table();
            }
            ui.separator();
            // Photoshop's Assign Profile: the colours' meaning in exports; pixels don't change.
            ui.menu_button("Assign Profile", |ui| {
                use painter_core::color_profile::ColorProfile;
                let choices: &[ColorProfile] = if self.doc.mode.is_gray() { &ColorProfile::GRAY } else { &ColorProfile::RGB };
                for p in choices {
                    if ui.radio(self.doc.profile == *p, p.name()).clicked() {
                        self.doc.profile = *p;
                        self.dirty = true;
                        ui.close_menu();
                    }
                }
            });
        });
    }
}

fn ink_names(d: &Duotone) -> String {
    d.inks.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(", ")
}

/// A small drawing of an ink's curve, as Photoshop's curve box beside each ink.
fn curve_thumb(curve: &PressureCurve) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(28.0, 22.0), Sense::click());
        ui.painter().rect_filled(rect, 2.0, Color32::WHITE);
        let pts: Vec<egui::Pos2> = (0..=16)
            .map(|i| i as f32 / 16.0)
            .map(|x| egui::pos2(rect.left() + x * rect.width(), rect.bottom() - curve.eval(x) * rect.height()))
            .collect();
        ui.painter().add(egui::Shape::line(pts, egui::Stroke::new(1.2_f32, Color32::BLACK)));
        ui.painter().rect_stroke(rect, 2.0, egui::Stroke::new(1.0_f32, theme::line()), egui::StrokeKind::Inside);
        resp
    }
}
