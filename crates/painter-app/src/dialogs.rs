//! File → Import → From Scanner and File → Export → SVG dialogs.

use std::thread::JoinHandle;

use eframe::egui::{self, Color32, Sense, Stroke, Vec2};
use painter_core::{Layer, TileStore};
use painter_render::Plane;
use scanner_io::{
    backends, list_devices, prepare_for_import, ColorMode, DeviceInfo, ImportOptions, LineArtOptions, Orientation, Rotation, ScanArea,
    ScanError, ScanSettings, ScannedImage, ScannerBackend,
};
use vector_export::{export_svg, ExportOptions, ExportScope, StrokeMode, ThresholdSource};

use crate::app::PainterApp;
use crate::icons::{icon_button, Icon};
use crate::theme;
use crate::widgets::{prop_percent, prop_slider};

/// What a device search hands back: the backends, the devices, and why some backends failed.
type DeviceSearch = (Vec<Box<dyn ScannerBackend>>, Vec<DeviceInfo>, Vec<ScanError>);

/// Work running on a scanner thread. Backends move into the thread and come back with the result.
enum Job {
    Devices(JoinHandle<DeviceSearch>),
    Preview(JoinHandle<(Box<dyn ScannerBackend>, Result<ScannedImage, ScanError>)>),
    /// The result is already cleaned up for import.
    Scan(JoinHandle<(Box<dyn ScannerBackend>, Result<ScannedImage, ScanError>)>),
}

impl Job {
    fn is_finished(&self) -> bool {
        match self {
            Job::Devices(h) => h.is_finished(),
            Job::Preview(h) | Job::Scan(h) => h.is_finished(),
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Job::Devices(_) => "Looking for scanners…",
            Job::Preview(_) => "Scanning a preview…",
            Job::Scan(_) => "Scanning…",
        }
    }
}

/// State of the scanner dialog while it's open.
pub struct ScannerDialog {
    backends: Vec<Box<dyn ScannerBackend>>,
    devices: Vec<DeviceInfo>,
    /// Why backends found nothing, shown when there are no devices.
    errors: Vec<ScanError>,
    device: usize,
    settings: ScanSettings,
    auto_contrast: bool,
    line_art: Option<LineArtOptions>,
    preview: Option<(ScannedImage, egui::TextureHandle)>,
    /// Crop region on the preview, as bed fractions.
    crop: ScanArea,
    drag_from: Option<egui::Pos2>,
    /// Rotation and mirroring applied before import.
    orientation: Orientation,
    message: String,
    job: Option<Job>,
}

impl ScannerDialog {
    /// Opens the dialog and starts looking for devices in the background.
    pub fn open() -> Self {
        let mut dialog = Self {
            backends: Vec::new(),
            devices: Vec::new(),
            errors: Vec::new(),
            device: 0,
            settings: ScanSettings::default(),
            auto_contrast: true,
            line_art: Some(LineArtOptions::default()),
            preview: None,
            crop: ScanArea::FULL,
            drag_from: None,
            orientation: Orientation::default(),
            message: String::new(),
            job: None,
        };
        dialog.refresh();
        dialog
    }

    fn refresh(&mut self) {
        let mut found = std::mem::take(&mut self.backends);
        if found.is_empty() {
            found = backends();
        }
        self.job = Some(Job::Devices(std::thread::spawn(move || {
            let (devices, errors) = list_devices(&mut found);
            (found, devices, errors)
        })));
    }

    /// Takes the selected device's backend out for a job.
    fn take_backend(&mut self) -> Result<(DeviceInfo, Box<dyn ScannerBackend>), ScanError> {
        let device = self.devices.get(self.device).cloned().ok_or_else(|| ScanError::Device("no scanner selected".into()))?;
        let index = self
            .backends
            .iter()
            .position(|b| b.backend() == device.backend)
            .ok_or_else(|| ScanError::Device("the scanner's backend is busy".into()))?;
        Ok((device, self.backends.swap_remove(index)))
    }

    fn start_preview(&mut self) {
        let started = self.take_backend().and_then(|(device, backend)| {
            let settings = self.settings.validated(&device.capabilities)?;
            Ok((device, backend, settings))
        });
        match started {
            Ok((device, mut backend, settings)) => {
                self.job = Some(Job::Preview(std::thread::spawn(move || {
                    let result = backend.preview(&device, &settings);
                    (backend, result)
                })));
            }
            Err(err) => self.message = err.to_string(),
        }
    }

    fn start_scan(&mut self) {
        let options = ImportOptions {
            // The device scans just the crop region.
            crop: ScanArea::FULL,
            orientation: self.orientation,
            auto_contrast: self.auto_contrast.then_some(0.005),
            extract_line_art: self.line_art,
        };
        let area = self.crop;
        let started = self.take_backend().and_then(|(device, backend)| {
            let settings = ScanSettings { area, ..self.settings.clone() }.validated(&device.capabilities)?;
            Ok((device, backend, settings))
        });
        match started {
            Ok((device, mut backend, settings)) => {
                self.job = Some(Job::Scan(std::thread::spawn(move || {
                    let result = backend.scan(&device, &settings).map(|image| prepare_for_import(&image, &options));
                    (backend, result)
                })));
            }
            Err(err) => self.message = err.to_string(),
        }
    }

    /// Collects a finished job; returns a scan ready to import.
    fn poll(&mut self, ctx: &egui::Context) -> Option<ScannedImage> {
        if !self.job.as_ref().is_some_and(Job::is_finished) {
            if self.job.is_some() {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            return None;
        }
        let lost = || ScanError::Device("the scanner thread stopped unexpectedly".into());
        match self.job.take()? {
            Job::Devices(handle) => match handle.join() {
                Ok((backends, devices, errors)) => {
                    self.backends = backends;
                    self.devices = devices;
                    self.errors = errors;
                    self.device = self.device.min(self.devices.len().saturating_sub(1));
                    // Start at a resolution the device has, near 300 dpi.
                    if let Some(caps) = self.devices.get(self.device).map(|d| &d.capabilities) {
                        self.settings.dpi = caps.nearest_resolution(300).unwrap_or(300);
                        if let Some(&mode) = caps.color_modes.iter().find(|m| **m == ColorMode::Grayscale).or(caps.color_modes.first())
                        {
                            self.settings.color_mode = mode;
                        }
                    }
                }
                Err(_) => self.errors = vec![lost()],
            },
            Job::Preview(handle) => match handle.join() {
                Ok((backend, result)) => {
                    self.backends.push(backend);
                    match result {
                        Ok(image) => {
                            let pixels = image.rgba.as_chunks::<4>().0.iter().map(|p| Color32::from_rgb(p[0], p[1], p[2])).collect();
                            let color = egui::ColorImage { size: [image.width as usize, image.height as usize], pixels };
                            let texture = ctx.load_texture("scan_preview", color, egui::TextureOptions::LINEAR);
                            self.preview = Some((image, texture));
                            self.crop = ScanArea::FULL;
                            self.message.clear();
                        }
                        Err(err) => self.message = err.to_string(),
                    }
                }
                Err(_) => self.message = lost().to_string(),
            },
            Job::Scan(handle) => match handle.join() {
                Ok((backend, result)) => {
                    self.backends.push(backend);
                    match result {
                        Ok(image) => return Some(image),
                        Err(err) => self.message = err.to_string(),
                    }
                }
                Err(_) => self.message = lost().to_string(),
            },
        }
        None
    }
}
/// State of the SVG export dialog while it's open.
#[derive(Default)]
pub struct SvgExportDialog {
    pub options: ExportOptions,
    pub active_layer_only: bool,
}

impl PainterApp {
    pub(crate) fn scanner_window(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.scanner.take() else {
            return;
        };
        let mut open = true;
        let import = dialog.poll(ctx);
        egui::Window::new("Import from Scanner").collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
            ui.set_width(520.0);
            if let Some(job) = &dialog.job {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(job.label());
                });
            }
            let busy = dialog.job.is_some();
            if dialog.devices.is_empty() {
                if matches!(dialog.job, Some(Job::Devices(_))) {
                    return;
                }
                ui.colored_label(Color32::from_rgb(240, 190, 90), "No scanner found.");
                for err in &dialog.errors {
                    ui.small(err.to_string());
                }
                if dialog.errors.is_empty() {
                    ui.small("Connect and switch on a scanner, then refresh.");
                }
                if ui.add_enabled(!busy, egui::Button::new("Refresh")).clicked() {
                    dialog.refresh();
                }
                return;
            }
            ui.add_enabled_ui(!busy, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(220.0);
                        let names: Vec<String> = dialog.devices.iter().map(|d| format!("{} ({})", d.name, d.backend.name())).collect();
                        egui::ComboBox::from_label("Device").selected_text(&names[dialog.device]).show_ui(ui, |ui| {
                            for (i, name) in names.iter().enumerate() {
                                ui.selectable_value(&mut dialog.device, i, name);
                            }
                        });
                        let caps = dialog.devices[dialog.device].capabilities.clone();
                        if caps.resolutions.is_empty() {
                            ui.colored_label(Color32::from_rgb(240, 190, 90), "The scanner didn't answer; is it on?");
                        } else if !caps.resolutions.contains(&dialog.settings.dpi) {
                            dialog.settings.dpi = caps.nearest_resolution(300).unwrap_or(300);
                        }
                        egui::ComboBox::from_label("Resolution").selected_text(format!("{} dpi", dialog.settings.dpi)).show_ui(
                            ui,
                            |ui| {
                                for dpi in &caps.resolutions {
                                    ui.selectable_value(&mut dialog.settings.dpi, *dpi, format!("{dpi} dpi"));
                                }
                            },
                        );
                        egui::ComboBox::from_label("Mode").selected_text(dialog.settings.color_mode.name()).show_ui(ui, |ui| {
                            for mode in ColorMode::ALL.into_iter().filter(|m| caps.color_modes.contains(m)) {
                                ui.selectable_value(&mut dialog.settings.color_mode, mode, mode.name());
                            }
                        });
                        ui.separator();
                        // How the page lies on the glass; the preview shows the result.
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            ui.label("Rotate");
                            let o = &mut dialog.orientation;
                            if icon_button(ui, Icon::RotateLeft, 16.0, false, "Rotate 90° anticlockwise").clicked() {
                                o.rotation = o.rotation.turned_ccw();
                            }
                            if icon_button(ui, Icon::RotateRight, 16.0, false, "Rotate 90° clockwise").clicked() {
                                o.rotation = o.rotation.turned_cw();
                            }
                            ui.add_space(4.0);
                            for rotation in Rotation::ALL {
                                ui.selectable_value(&mut o.rotation, rotation, format!("{}°", rotation.degrees()));
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Flip");
                            ui.checkbox(&mut dialog.orientation.flip_horizontal, "horizontal");
                            ui.checkbox(&mut dialog.orientation.flip_vertical, "vertical");
                        });
                        ui.separator();
                        ui.checkbox(&mut dialog.auto_contrast, "Auto contrast");
                        let mut extract = dialog.line_art.is_some();
                        if ui.checkbox(&mut extract, "Extract line art (paper → transparent)").changed() {
                            dialog.line_art = extract.then(LineArtOptions::default);
                        }
                        if let Some(line_art) = &mut dialog.line_art {
                            prop_percent(ui, "Paper above", &mut line_art.paper, 0.0..=100.0);
                            prop_percent(ui, "Solid ink below", &mut line_art.ink, 0.0..=100.0);
                            ui.checkbox(&mut line_art.keep_color, "Keep line colour");
                        }
                    });
                    // Preview with a draggable crop rectangle.
                    let (rect, response) = ui.allocate_exact_size(Vec2::new(280.0, 360.0), Sense::drag());
                    ui.painter().rect_filled(rect, 2.0, theme::well());
                    if let Some((_, texture)) = &dialog.preview {
                        // The preview is drawn oriented; the crop stays in bed coordinates.
                        let o = dialog.orientation;
                        let [ow, oh] = o.size(texture.size_vec2().x, texture.size_vec2().y);
                        let size = Vec2::new(ow, oh);
                        let shown = egui::Rect::from_center_size(rect.center(), size * (rect.size() / size).min_elem());
                        let mut mesh = egui::Mesh::with_texture(texture.id());
                        for corner in [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]] {
                            let [u, v] = o.to_source(corner);
                            mesh.vertices.push(egui::epaint::Vertex {
                                pos: shown.min + Vec2::new(corner[0], corner[1]) * shown.size(),
                                uv: egui::pos2(u, v),
                                color: Color32::WHITE,
                            });
                        }
                        mesh.indices.extend([0, 1, 2, 0, 2, 3]);
                        ui.painter().add(egui::Shape::mesh(mesh));
                        let to_bed = |p: egui::Pos2| {
                            let f = ((p - shown.min) / shown.size()).clamp(Vec2::ZERO, Vec2::splat(1.0));
                            o.to_source([f.x, f.y])
                        };
                        if response.drag_started() {
                            dialog.drag_from = response.interact_pointer_pos();
                        }
                        if let (Some(from), Some(to)) = (dialog.drag_from, response.interact_pointer_pos()) {
                            let (a, b) = (to_bed(from), to_bed(to));
                            dialog.crop = ScanArea {
                                x: a[0].min(b[0]),
                                y: a[1].min(b[1]),
                                width: (a[0] - b[0]).abs(),
                                height: (a[1] - b[1]).abs(),
                            }
                            .clamped();
                        }
                        let c = dialog.crop;
                        let (p, q) = (o.from_source([c.x, c.y]), o.from_source([c.x + c.width, c.y + c.height]));
                        let at = |f: [f32; 2]| shown.min + Vec2::new(f[0], f[1]) * shown.size();
                        let crop = egui::Rect::from_two_pos(at(p), at(q));
                        ui.painter().rect_stroke(crop, 0.0, Stroke::new(1.5_f32, theme::accent()), egui::StrokeKind::Middle);
                    } else {
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "Preview, then drag to crop",
                            egui::FontId::proportional(12.0),
                            theme::text_dim(),
                        );
                    }
                })
            });
            if !dialog.message.is_empty() {
                ui.colored_label(Color32::from_rgb(240, 120, 100), &dialog.message);
            }
            ui.separator();
            ui.add_enabled_ui(!busy, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Preview").clicked() {
                        dialog.message.clear();
                        dialog.start_preview();
                    }
                    if ui.button("Scan and import").clicked() {
                        dialog.message.clear();
                        dialog.start_scan();
                    }
                    if ui.button("Refresh devices").clicked() {
                        dialog.refresh();
                    }
                });
            });
        });
        if let Some(image) = import {
            self.import_scan(&image);
        } else if open {
            self.scanner = Some(dialog);
        }
    }

    /// A cleaned-up scan as a new raster layer at the top-left of the canvas.
    pub(crate) fn import_scan(&mut self, image: &ScannedImage) {
        let tiles = TileStore::from_rgba(image.width, image.height, &image.to_premultiplied());
        let id = self.doc.alloc_id();
        self.checkpoint();
        self.edit_mask = false;
        let layer_id = self.doc.add_layer(Layer::raster_with(id, format!("Scan {} dpi", image.dpi), tiles));
        self.upload_plane(Plane::Color(layer_id));
        self.recomposite = true;
        let [w, h] = image.size_mm();
        self.status = format!("Imported a {}×{} px scan ({w:.0}×{h:.0} mm)", image.width, image.height);
    }

    pub(crate) fn svg_export_window(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.svg_export.take() else {
            return;
        };
        let mut open = true;
        let mut export = false;
        egui::Window::new("Export SVG").collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut dialog.active_layer_only, false, "Whole canvas");
                ui.selectable_value(&mut dialog.active_layer_only, true, "Active layer");
            });
            ui.separator();
            ui.label(egui::RichText::new("Raster tracing").strong());
            let trace = &mut dialog.options.trace;
            ui.horizontal(|ui| {
                ui.label("Ink from");
                ui.selectable_value(&mut trace.source, ThresholdSource::Alpha, "Opacity");
                ui.selectable_value(&mut trace.source, ThresholdSource::Luminance, "Darkness");
            });
            let mut threshold = trace.threshold as f32;
            if prop_slider(ui, "Threshold", &mut threshold, 0.0..=254.0, "", 0, false) {
                trace.threshold = threshold.round() as u8;
            }
            prop_slider(ui, "Path simplification", &mut trace.simplify, 0.0..=4.0, " px", 2, false);
            prop_slider(ui, "Corner sensitivity", &mut trace.corner_angle, 10.0..=170.0, "°", 0, false);
            ui.small("Turns sharper than this angle stay corners; lower is more angular.");
            prop_slider(ui, "Remove specks under", &mut trace.despeckle, 0.0..=200.0, " px²", 0, false);
            ui.separator();
            ui.label(egui::RichText::new("Vector strokes").strong());
            ui.horizontal(|ui| {
                ui.selectable_value(&mut dialog.options.strokes, StrokeMode::FilledOutline, "Pressure outline");
                ui.selectable_value(&mut dialog.options.strokes, StrokeMode::Centerline, "Centre line");
            });
            ui.separator();
            ui.checkbox(&mut dialog.options.include_paper, "Include paper as background");
            ui.checkbox(&mut dialog.options.skip_hidden, "Skip hidden layers");
            ui.add_space(4.0);
            export = ui.button("Export…").clicked();
        });
        if export {
            dialog.options.scope = if dialog.active_layer_only { ExportScope::Layer(self.doc.active) } else { ExportScope::Canvas };
            self.export_svg(&dialog.options);
        }
        if open && !export {
            self.svg_export = Some(dialog);
        }
    }

    fn export_svg(&mut self, options: &ExportOptions) {
        let name = match options.scope {
            ExportScope::Canvas => "painting.svg".to_string(),
            ExportScope::Layer(id) => format!("{}.svg", self.doc.find(id).map_or("layer", |l| l.name.as_str())),
        };
        let Some(path) = crate::files::FileDialog::new().add_filter("SVG", &["svg"]).set_file_name(name).save_file() else {
            return;
        };
        self.status = match export_svg(&self.doc, options) {
            Ok(report) => match std::fs::write(&path, report.to_svg()) {
                Ok(()) if report.warnings.is_empty() => {
                    format!("Exported {} paths to {}", report.paths, path.display())
                }
                Ok(()) => format!("Exported {} paths to {} ({})", report.paths, path.display(), report.warnings.join("; ")),
                Err(err) => format!("Couldn't write {}: {err}", path.display()),
            },
            Err(err) => format!("SVG export failed: {err}"),
        };
    }
}
