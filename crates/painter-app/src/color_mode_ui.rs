//! Edit → Mode (Photoshop's Image → Mode): RGB Color, Grayscale, Bitmap.
//! Grayscale turns every colour grey; Bitmap flattens to black and white
//! (asking how); while in one of them, painting stays grey or black and
//! white. See `painter_core::color_mode`.

use eframe::egui;
use painter_core::color_mode::{self, BitmapMethod, ColorMode};
use painter_core::{Layer, LayerKind, TileCoord, TileStore};
use painter_render::Plane;

use crate::app::PainterApp;

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

impl PainterApp {
    pub(crate) fn set_color_mode(&mut self, mode: ColorMode) {
        if mode == self.doc.mode && mode != ColorMode::Cmyk {
            return;
        }
        match mode {
            ColorMode::Bitmap => self.bitmap_dialog = Some(BitmapDialog::default()),
            // CMYK asks for the press profile (also to change it on a CMYK document).
            ColorMode::Cmyk => self.cmyk_dialog = Some(crate::cmyk_ui::CmykDialog::new(self.doc.cmyk.as_ref())),
            ColorMode::Grayscale => {
                self.next_label = Some("Mode: Grayscale".into());
                self.checkpoint();
                let canvas = self.doc.canvas();
                color_mode::gray_layers(&mut self.doc.layers, canvas);
                self.doc.mode = ColorMode::Grayscale;
                self.doc.profile = painter_core::color_profile::ColorProfile::GrayGamma22;
                self.resync_gpu();
                self.status = "Grayscale: colours are grey from now on (Edit > Mode > RGB Color brings colour back)".into();
            }
            ColorMode::Rgb => {
                self.next_label = Some("Mode: RGB Color".into());
                self.checkpoint();
                self.doc.mode = ColorMode::Rgb;
                if self.doc.profile.is_gray() {
                    self.doc.profile = painter_core::color_profile::ColorProfile::Srgb;
                }
                self.status = "RGB Color: colour can be used again".into();
            }
        }
    }

    /// The image flattened into one black-and-white layer.
    pub(crate) fn convert_to_bitmap(&mut self, method: BitmapMethod) {
        let (w, h) = self.doc.canvas();
        let started = std::time::Instant::now();
        let flat = painter_core::io::flatten(&self.doc);
        let bw = color_mode::to_bitmap(&flat, w as usize, h as usize, method);
        self.next_label = Some("Mode: Bitmap".into());
        self.checkpoint();
        let id = self.doc.alloc_id();
        let mut layer = Layer::raster(id, "Bitmap");
        layer.kind = LayerKind::Raster(TileStore::from_rgba(w, h, &bw));
        self.doc.layers = vec![layer];
        self.doc.active = id;
        self.doc.mode = ColorMode::Bitmap;
        self.doc.profile = painter_core::color_profile::ColorProfile::GrayGamma22;
        self.resync_gpu();
        self.status = format!("Bitmap: flattened to black and white in {} ms", started.elapsed().as_millis());
    }

    /// After an edit of `coords` on a colour plane: grey or black-and-white
    /// again in those modes, and only the channels being edited changed.
    pub(crate) fn finish_edit(&mut self, plane: Plane, before: &TileStore, coords: impl IntoIterator<Item = TileCoord>) {
        let coords: Vec<TileCoord> = coords.into_iter().collect();
        self.restrict_channels(plane, before, coords.iter().copied());
        if !matches!(plane, Plane::Color(_)) {
            return;
        }
        let mode = self.doc.mode;
        // Out while the tiles are borrowed; back below.
        let proofer = self.proofer.proofer.take();
        if let Some(tiles) = self.plane_tiles_mut(plane) {
            match mode {
                ColorMode::Rgb => {}
                ColorMode::Grayscale => color_mode::gray_tiles(tiles, Some(&coords)),
                ColorMode::Bitmap => color_mode::threshold_tiles(tiles, &coords),
                // Only the new pixels, so colours don't drift from edit to edit.
                ColorMode::Cmyk => {
                    if let Some(p) = &proofer {
                        p.proof_changed(tiles, before, &coords);
                    }
                }
            }
        }
        self.proofer.proofer = proofer;
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

    /// Edit → Mode.
    pub(crate) fn mode_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Mode", |ui| {
            for mode in ColorMode::ALL {
                let label = if matches!(mode, ColorMode::Bitmap | ColorMode::Cmyk) { format!("{}...", mode.name()) } else { mode.name().to_string() };
                if ui.radio(self.doc.mode == mode, label).clicked() {
                    ui.close_menu();
                    self.set_color_mode(mode);
                }
            }
            ui.separator();
            ui.add_enabled(false, egui::Label::new("8 Bits/Channel"));
            ui.separator();
            // Photoshop's Assign Profile: the colours' meaning in exports; pixels don't change.
            ui.menu_button("Assign Profile", |ui| {
                use painter_core::color_profile::ColorProfile;
                let choices: &[ColorProfile] = if matches!(self.doc.mode, ColorMode::Rgb | ColorMode::Cmyk) { &ColorProfile::RGB } else { &ColorProfile::GRAY };
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
