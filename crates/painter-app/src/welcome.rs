//! The welcome screen shown at startup (what Painter does, how to begin,
//! where to find more) and the About window (version, licence, links).

use eframe::egui::{self, Align2, Color32, RichText, Vec2};

use crate::app::PainterApp;

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const REPOSITORY: &str = "https://github.com/cr0ma-gh/painter";
pub(crate) const WEBSITE: &str = "https://cr0ma-gh.github.io/painter/";
const ISSUES: &str = "https://github.com/cr0ma-gh/painter/issues";
const RELEASES: &str = "https://github.com/cr0ma-gh/painter/releases";

/// What the welcome screen lists: a title and one line each.
const FEATURES: [(&str, &str); 8] = [
    ("Brushes you already own", "Clip Studio, Photoshop, Krita, MyPaint and GIMP brushes in and out; pressure, tilt and textures."),
    ("Layers like the pros", "Folders, masks, clipping, 13 blend modes, screentones, vector and draft layers."),
    ("Comic pages", "Print presets, bleed, panel templates; every panel a masked folder."),
    ("Rulers & perspective", "Perspective, symmetry, parallel, radial and concentric rulers; Shift for straight lines."),
    ("3D you can paint", "Blender, FBX, glTF and OBJ models as layers; drop them into your perspective; paint their textures."),
    ("Your files open", "Clip Studio .clip, PSD, Krita, OpenRaster and PNG, with their layers; export to all of them."),
    ("References and scans", "A reference board beside the canvas; scanners and the camera, with clean-up and panel extraction."),
    ("Pen first, everywhere", "WinTab, XInput2, Apple Pencil and Android styluses; Windows, macOS, Linux, iPad and Android."),
];

impl PainterApp {
    /// The ensō, as a texture for these windows.
    fn logo(&mut self, ctx: &egui::Context) -> egui::TextureId {
        self.logo
            .get_or_insert_with(|| {
                let png = include_bytes!("../../../packaging/icons/painter-256.png");
                let image = image::load_from_memory(png).map(|i| i.to_rgba8()).unwrap_or_default();
                let size = [image.width() as usize, image.height() as usize];
                let pixels = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
                ctx.load_texture("painter-logo", pixels, egui::TextureOptions::LINEAR)
            })
            .id()
    }

    /// The welcome screen, while open (at startup unless turned off; Help → Welcome Screen).
    pub(crate) fn welcome_window(&mut self, ctx: &egui::Context) {
        if !self.welcome_open {
            return;
        }
        let logo = self.logo(ctx);
        // Dim the app behind it.
        let screen = ctx.screen_rect();
        egui::Area::new(egui::Id::new("welcome_dim")).order(egui::Order::Middle).fixed_pos(screen.min).show(ctx, |ui| {
            let (rect, response) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
            ui.painter().rect_filled(rect, 0.0, Color32::from_black_alpha(150));
            if response.clicked() {
                self.welcome_open = false;
            }
        });
        let mut action = None;
        let width = (screen.width() - 40.0).clamp(320.0, 760.0);
        egui::Window::new("Welcome to Painter")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size(Vec2::new(width, 0.0))
            .show(ctx, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add(egui::Image::new((logo, Vec2::splat(76.0))));
                    ui.vertical(|ui| {
                        ui.add_space(8.0);
                        ui.label(RichText::new("Painter").size(30.0).strong());
                        ui.label(RichText::new(format!("Version {VERSION} · free and open source")).weak());
                        ui.label("Paint, ink and lay out comics.");
                    });
                });
                ui.add_space(10.0);
                ui.separator();
                let columns = if width > 560.0 { 2 } else { 1 };
                egui::Grid::new("welcome_features").num_columns(columns).spacing(Vec2::new(18.0, 10.0)).show(ui, |ui| {
                    for (i, (title, line)) in FEATURES.iter().enumerate() {
                        ui.vertical(|ui| {
                            ui.set_width(width / columns as f32 - 30.0);
                            ui.label(RichText::new(*title).strong());
                            ui.label(RichText::new(*line).small().weak());
                        });
                        if (i + 1) % columns == 0 {
                            ui.end_row();
                        }
                    }
                });
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    if ui.add(egui::Button::new(RichText::new("New Canvas…").strong()).min_size(Vec2::new(120.0, 30.0))).clicked() {
                        action = Some("file.new");
                    }
                    if ui.add(egui::Button::new("Open…").min_size(Vec2::new(90.0, 30.0))).clicked() {
                        action = Some("file.open");
                    }
                    if ui.add(egui::Button::new("New Comic Page…").min_size(Vec2::new(130.0, 30.0))).clicked() {
                        action = Some("file.comic");
                    }
                    if ui.add(egui::Button::new("Start Painting").min_size(Vec2::new(120.0, 30.0))).clicked() {
                        self.welcome_open = false;
                    }
                });
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.hyperlink_to("Website", WEBSITE);
                    ui.label("·");
                    ui.hyperlink_to("Source on GitHub", REPOSITORY);
                    ui.label("·");
                    ui.hyperlink_to("Downloads", RELEASES);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.checkbox(&mut self.ws.welcome, "Show at startup");
                    });
                });
            });
        if let Some(command) = action {
            self.welcome_open = false;
            self.run_command(command);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.welcome_open = false;
        }
    }

    /// Help → About Painter.
    pub(crate) fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let logo = self.logo(ctx);
        let mut open = true;
        egui::Window::new("About Painter")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add(egui::Image::new((logo, Vec2::splat(96.0))));
                    ui.label(RichText::new("Painter").size(26.0).strong());
                    ui.label(format!("Version {VERSION}"));
                    ui.add_space(4.0);
                    ui.label("A painting app for illustration and comics, written in Rust with a GPU canvas.");
                    ui.label(RichText::new("MIT or Apache-2.0").small().weak());
                    ui.add_space(8.0);
                    ui.hyperlink_to("cr0ma-gh.github.io/painter", WEBSITE);
                    ui.hyperlink_to("github.com/cr0ma-gh/painter", REPOSITORY);
                    ui.hyperlink_to("Report a bug", ISSUES);
                    ui.add_space(6.0);
                    ui.label(RichText::new(format!("{} · {}", std::env::consts::OS, std::env::consts::ARCH)).small().weak());
                });
            });
        if !open {
            self.about_open = false;
        }
    }
}

impl PainterApp {
    /// "Reset whole brush list": asks first, since imported brushes go.
    pub(crate) fn brush_reset_prompt(&mut self, ctx: &egui::Context) {
        if !self.confirm_brush_reset {
            return;
        }
        let imported = self.brushes.iter().enumerate().filter(|(i, _)| self.default_of(*i).is_none()).count();
        let mut answer = None;
        egui::Window::new("Reset the brush list?")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("The brush list goes back to Painter's default brushes, with their default settings.");
                if imported > 0 {
                    ui.label(RichText::new(format!("{imported} imported or custom brushes will be removed.")).strong());
                    ui.small("Export them first (right-click the export button: all brushes) to keep a copy.");
                }
                ui.horizontal(|ui| {
                    if ui.button("Reset").clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        answer = Some(false);
                    }
                });
            });
        match answer {
            Some(true) => {
                self.confirm_brush_reset = false;
                self.reset_brush_list();
            }
            Some(false) => self.confirm_brush_reset = false,
            None => {}
        }
    }
}
