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
    /// Photographed or scanned pages cleaned up for import.
    Pages(JoinHandle<Vec<ScannedImage>>),
}

impl Job {
    fn is_finished(&self) -> bool {
        match self {
            Job::Devices(h) => h.is_finished(),
            Job::Preview(h) | Job::Scan(h) => h.is_finished(),
            Job::Pages(h) => h.is_finished(),
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Job::Devices(_) => "Looking for scanners…",
            Job::Preview(_) => "Scanning a preview…",
            Job::Scan(_) => "Scanning…",
            Job::Pages(_) => "Cleaning up the pages…",
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
    /// Pages from the document camera or image files (instead of a scanner).
    pages: Vec<ScannedImage>,
    page: usize,
    /// Index into [`crate::scan_source::PAGE_SIZES`]: gives the pages their resolution.
    paper: usize,
    /// Which page the preview shows.
    previewed: Option<usize>,
    /// What happens to a scan bigger than the canvas.
    oversize: Oversize,
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
            pages: Vec::new(),
            page: 0,
            paper: 0,
            previewed: None,
            oversize: Oversize::default(),
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

    /// Pages to add (camera or image files); the first new one is shown.
    pub(crate) fn add_pages(&mut self, paths: Vec<std::path::PathBuf>) {
        let paper = crate::scan_source::PAGE_SIZES[self.paper].1;
        let first = self.pages.len();
        for path in paths {
            match crate::scan_source::load_page(&path, paper) {
                Ok(page) => self.pages.push(page),
                Err(err) => self.message = err,
            }
        }
        if self.pages.len() > first {
            self.page = first;
            self.previewed = None;
            self.message.clear();
        }
    }

    /// Cleans up the shown page (or every page) on a worker thread, with the
    /// crop drawn on the preview.
    fn start_pages(&mut self, all: bool) {
        let options = ImportOptions {
            crop: self.crop,
            orientation: self.orientation,
            auto_contrast: self.auto_contrast.then_some(0.005),
            extract_line_art: self.line_art,
        };
        let paper = crate::scan_source::PAGE_SIZES[self.paper].1;
        let pages: Vec<ScannedImage> =
            if all { self.pages.clone() } else { self.pages.get(self.page).cloned().into_iter().collect() };
        self.job = Some(Job::Pages(std::thread::spawn(move || {
            pages
                .into_iter()
                .map(|mut page| {
                    page.dpi = crate::scan_source::page_dpi(page.width, page.height, paper);
                    prepare_for_import(&page, &options)
                })
                .collect()
        })));
    }

    /// Shows the current page in the preview (a smaller copy).
    fn show_page(&mut self, ctx: &egui::Context) {
        if self.previewed == Some(self.page) {
            return;
        }
        let Some(page) = self.pages.get(self.page) else { return };
        let step = page.width.max(page.height).div_ceil(1024).max(1) as usize;
        let (w, h) = ((page.width as usize).div_ceil(step), (page.height as usize).div_ceil(step));
        let mut pixels = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let i = ((y * step) * page.width as usize + x * step) * 4;
                pixels.push(Color32::from_rgb(page.rgba[i], page.rgba[i + 1], page.rgba[i + 2]));
            }
        }
        let texture = ctx.load_texture("scan_preview", egui::ColorImage { size: [w, h], pixels }, egui::TextureOptions::LINEAR);
        self.preview = Some((page.clone(), texture));
        self.crop = ScanArea::FULL;
        self.previewed = Some(self.page);
    }

    /// Collects a finished job; returns scans ready to import.
    fn poll(&mut self, ctx: &egui::Context) -> Vec<ScannedImage> {
        if !self.job.as_ref().is_some_and(Job::is_finished) {
            if self.job.is_some() {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            return Vec::new();
        }
        let lost = || ScanError::Device("the scanner thread stopped unexpectedly".into());
        let Some(job) = self.job.take() else { return Vec::new() };
        match job {
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
                        Ok(image) => return vec![image],
                        Err(err) => self.message = err.to_string(),
                    }
                }
                Err(_) => self.message = lost().to_string(),
            },
            Job::Pages(handle) => match handle.join() {
                Ok(pages) => return pages,
                Err(_) => self.message = lost().to_string(),
            },
        }
        Vec::new()
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
        // Pages from the iPad's document camera.
        let camera_pages = crate::scan_source::take_pages();
        if !camera_pages.is_empty() {
            dialog.add_pages(camera_pages);
        }
        if let Some(err) = crate::scan_source::take_error() {
            dialog.message = err;
        }
        if crate::scan_source::camera_active() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        dialog.show_page(ctx);
        let mut pick = false;
        egui::Window::new("Import from Scanner or Camera").collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
            ui.set_width(520.0);
            if let Some(job) = &dialog.job {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(job.label());
                });
            }
            let busy = dialog.job.is_some();
            // Where the pages come from: a scanner, the camera, or image files.
            ui.horizontal(|ui| {
                if crate::scan_source::camera_supported()
                    && ui
                        .add_enabled(!busy && !crate::scan_source::camera_active(), egui::Button::new("Scan with camera"))
                        .on_hover_text("The camera finds the sheet and straightens it; take as many pages as you like")
                        .clicked()
                {
                    dialog.message.clear();
                    crate::scan_source::start_camera();
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("From photo or file..."))
                    .on_hover_text("A photo or a scan of a page, cleaned up like a scan")
                    .clicked()
                {
                    pick = true;
                }
                if !dialog.pages.is_empty() && ui.add_enabled(!busy, egui::Button::new("Use a scanner")).on_hover_text("Forget these pages").clicked()
                {
                    dialog.pages.clear();
                    dialog.preview = None;
                    dialog.previewed = None;
                    dialog.crop = ScanArea::FULL;
                }
            });
            ui.separator();
            if dialog.pages.is_empty() {
                scanner_controls(ui, &mut dialog, busy);
            } else {
                page_controls(ui, &mut dialog, busy);
            }
        });
        if pick {
            if let Some(paths) = crate::files::FileDialog::new()
                .add_filter("Images", &["png", "jpg", "jpeg", "bmp"])
                .purpose(crate::files::Purpose::ScanPage)
                .pick_files()
            {
                dialog.add_pages(paths);
            }
        }
        let imported = !import.is_empty();
        for image in &import {
            self.import_scan(image, dialog.oversize);
        }
        if import.len() > 1 {
            self.status = format!("Imported {} pages as layers", import.len());
        }
        // A scanner scan closes the dialog; pages keep it open for the next ones.
        let close = imported && dialog.pages.is_empty();
        if open && !close {
            self.scanner = Some(dialog);
        }
    }

    /// Pages from a picker that answers later (iPad): into the scanner
    /// dialog, opening it if needed.
    pub(crate) fn add_scan_pages(&mut self, paths: Vec<std::path::PathBuf>) {
        self.scanner.get_or_insert_with(ScannerDialog::open).add_pages(paths);
    }

    /// A cleaned-up scan as a new raster layer at the top-left of the canvas.
    pub(crate) fn import_scan(&mut self, image: &ScannedImage, oversize: Oversize) {
        let (cw, ch) = self.doc.canvas();
        let (w, h) = (image.width, image.height);
        let bigger = w > cw || h > ch;
        self.checkpoint();
        self.edit_mask = false;
        let mut note = String::new();
        let mut dpi = image.dpi;
        let (pixels, w, h, x, y) = match oversize {
            Oversize::Fit if bigger => {
                // Scaled down to fit, centred.
                let (pixels, fw, fh) = fit_within(&image.to_premultiplied(), w, h, cw, ch);
                note = format!(", scaled to {}%", (fw as f32 / w as f32 * 100.0).round());
                // Fewer pixels for the same paper: a lower resolution.
                dpi = ((image.dpi as f32 * fw as f32 / w as f32).round() as u32).max(1);
                (pixels, fw, fh, (cw - fw) / 2, (ch - fh) / 2)
            }
            Oversize::Enlarge if bigger => {
                let size = (cw.max(w).min(painter_core::MAX_CANVAS_SIZE), ch.max(h).min(painter_core::MAX_CANVAS_SIZE));
                transform_tools::crop::apply_crop(&mut self.doc, transform_tools::crop::CropPlan { offset: [0, 0], size });
                self.resync_gpu();
                self.fit_pending = true;
                note = format!(", canvas enlarged to {}×{} px", size.0, size.1);
                (image.to_premultiplied(), w, h, 0, 0)
            }
            _ => (image.to_premultiplied(), w, h, 0, 0),
        };
        let mut tiles = TileStore::from_rgba(w, h, &pixels);
        if x != 0 || y != 0 {
            tiles = transform_tools::crop::shift(&tiles, x as i32, y as i32);
        }
        // Nothing is kept past the canvas: what doesn't show is deleted.
        let (cw, ch) = self.doc.canvas();
        if tiles.clip_to_canvas(cw, ch) {
            note = ", cut to the canvas".into();
        }
        let id = self.doc.alloc_id();
        let layer_id = self.doc.add_layer(Layer::raster_with(id, format!("Scan {dpi} dpi"), tiles));
        self.upload_plane(Plane::Color(layer_id));
        self.recomposite = true;
        let [mw, mh] = image.size_mm();
        self.status = format!("Imported a {}×{} px scan ({mw:.0}×{mh:.0} mm){note}", image.width, image.height);
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
        self.status = match export_svg(&self.doc.without_drafts(), options) {
            Ok(report) => match std::fs::write(&path, report.to_svg()) {
                Ok(()) => {
                    crate::files::offer_export(&path);
                    if report.warnings.is_empty() {
                        format!("Exported {} paths to {}", report.paths, path.display())
                    } else {
                        format!("Exported {} paths to {} ({})", report.paths, path.display(), report.warnings.join("; "))
                    }
                }
                Err(err) => format!("Couldn't write {}: {err}", path.display()),
            },
            Err(err) => format!("SVG export failed: {err}"),
        };
    }
}

/// The scanner part of the dialog: device, resolution, mode, cleanup and
/// the preview, then Preview / Scan and import.
fn scanner_controls(ui: &mut egui::Ui, dialog: &mut ScannerDialog, busy: bool) {
    if dialog.devices.is_empty() {
        if matches!(dialog.job, Some(Job::Devices(_))) {
            return;
        }
        ui.colored_label(Color32::from_rgb(240, 190, 90), "No scanner found.");
        for err in &dialog.errors {
            ui.small(err.to_string());
        }
        if cfg!(target_os = "ios") {
            ui.small("Scan the page with the camera, or pick a photo of it.");
        } else if dialog.errors.is_empty() {
            ui.small("Connect and switch on a scanner, then refresh; or pick a photo of the page.");
        }
        if !cfg!(target_os = "ios") && ui.add_enabled(!busy, egui::Button::new("Refresh")).clicked() {
            dialog.refresh();
        }
        message(ui, dialog);
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
                egui::ComboBox::from_label("Resolution").selected_text(format!("{} dpi", dialog.settings.dpi)).show_ui(ui, |ui| {
                    for dpi in &caps.resolutions {
                        ui.selectable_value(&mut dialog.settings.dpi, *dpi, format!("{dpi} dpi"));
                    }
                });
                egui::ComboBox::from_label("Mode").selected_text(dialog.settings.color_mode.name()).show_ui(ui, |ui| {
                    for mode in ColorMode::ALL.into_iter().filter(|m| caps.color_modes.contains(m)) {
                        ui.selectable_value(&mut dialog.settings.color_mode, mode, mode.name());
                    }
                });
                ui.separator();
                cleanup_controls(ui, dialog);
            });
            preview_area(ui, dialog, "Preview, then drag to crop");
        })
    });
    message(ui, dialog);
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
}

/// The pages part: which page, its paper size (its resolution), cleanup and
/// the preview with the crop, then Import page / Import all pages.
fn page_controls(ui: &mut egui::Ui, dialog: &mut ScannerDialog, busy: bool) {
    use crate::scan_source::{page_dpi, PAGE_SIZES};
    ui.add_enabled_ui(!busy, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width(220.0);
                let count = dialog.pages.len();
                if count > 1 {
                    egui::ComboBox::from_label("Page").selected_text(format!("{} of {count}", dialog.page + 1)).show_ui(ui, |ui| {
                        for i in 0..count {
                            ui.selectable_value(&mut dialog.page, i, format!("Page {}", i + 1));
                        }
                    });
                }
                egui::ComboBox::from_label("Paper").selected_text(PAGE_SIZES[dialog.paper].0).show_ui(ui, |ui| {
                    for (i, (name, mm)) in PAGE_SIZES.iter().enumerate() {
                        ui.selectable_value(&mut dialog.paper, i, format!("{name}  ({:.0}×{:.0} mm)", mm[0], mm[1]));
                    }
                });
                if let Some(page) = dialog.pages.get(dialog.page) {
                    let dpi = page_dpi(page.width, page.height, PAGE_SIZES[dialog.paper].1);
                    ui.small(format!("{}×{} px, about {dpi} dpi", page.width, page.height));
                }
                ui.separator();
                cleanup_controls(ui, dialog);
            });
            preview_area(ui, dialog, "Drag to crop");
        })
    });
    message(ui, dialog);
    ui.separator();
    ui.add_enabled_ui(!busy, |ui| {
        ui.horizontal(|ui| {
            if ui.button("Import page").on_hover_text("As a new layer").clicked() {
                dialog.message.clear();
                dialog.start_pages(false);
            }
            if dialog.pages.len() > 1 && ui.button(format!("Import all {} pages", dialog.pages.len())).on_hover_text("One layer each, same crop and cleanup").clicked() {
                dialog.message.clear();
                dialog.start_pages(true);
            }
            if ui.button("Remove page").clicked() {
                dialog.pages.remove(dialog.page);
                dialog.page = dialog.page.min(dialog.pages.len().saturating_sub(1));
                dialog.previewed = None;
                if dialog.pages.is_empty() {
                    dialog.preview = None;
                    dialog.crop = ScanArea::FULL;
                }
            }
        });
    });
}

fn message(ui: &mut egui::Ui, dialog: &ScannerDialog) {
    if !dialog.message.is_empty() {
        ui.colored_label(Color32::from_rgb(240, 120, 100), &dialog.message);
    }
}

/// Rotation, mirroring, auto contrast and line art.
fn cleanup_controls(ui: &mut egui::Ui, dialog: &mut ScannerDialog) {
    // How the page lies; the preview shows the result.
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
    egui::ComboBox::from_label("If bigger than the canvas").selected_text(dialog.oversize.label()).show_ui(ui, |ui| {
        for o in Oversize::ALL {
            ui.selectable_value(&mut dialog.oversize, o, o.label());
        }
    });
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
}

/// The preview, drawn oriented, with a crop rectangle to drag (kept in the
/// picture's own coordinates).
fn preview_area(ui: &mut egui::Ui, dialog: &mut ScannerDialog, hint: &str) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(280.0, 360.0), Sense::drag());
    ui.painter().rect_filled(rect, 2.0, theme::well());
    let Some((_, texture)) = &dialog.preview else {
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, hint, egui::FontId::proportional(12.0), theme::text_dim());
        return;
    };
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
        dialog.crop = ScanArea { x: a[0].min(b[0]), y: a[1].min(b[1]), width: (a[0] - b[0]).abs(), height: (a[1] - b[1]).abs() }.clamped();
    }
    let c = dialog.crop;
    let (p, q) = (o.from_source([c.x, c.y]), o.from_source([c.x + c.width, c.y + c.height]));
    let at = |f: [f32; 2]| shown.min + Vec2::new(f[0], f[1]) * shown.size();
    let crop = egui::Rect::from_two_pos(at(p), at(q));
    ui.painter().rect_stroke(crop, 0.0, Stroke::new(1.5_f32, theme::accent()), egui::StrokeKind::Middle);
}

/// What happens to a scan bigger than the canvas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Oversize {
    /// Scaled down to fit inside the canvas, centred.
    #[default]
    Fit,
    /// The canvas grows to hold it.
    Enlarge,
    /// Placed at the top left; what is past the canvas is deleted.
    Cut,
}

impl Oversize {
    const ALL: [Oversize; 3] = [Oversize::Fit, Oversize::Enlarge, Oversize::Cut];

    fn label(self) -> &'static str {
        match self {
            Oversize::Fit => "Fit it in the canvas",
            Oversize::Enlarge => "Enlarge the canvas",
            Oversize::Cut => "Cut what's outside",
        }
    }
}

/// Premultiplied RGBA `w` × `h` scaled down (keeping its proportions) to fit
/// in `max_w` × `max_h`; returns the pixels and their size.
fn fit_within(pixels: &[u8], w: u32, h: u32, max_w: u32, max_h: u32) -> (Vec<u8>, u32, u32) {
    let scale = (max_w as f32 / w as f32).min(max_h as f32 / h as f32).min(1.0);
    let (fw, fh) = (((w as f32 * scale).round() as u32).clamp(1, max_w), ((h as f32 * scale).round() as u32).clamp(1, max_h));
    if (fw, fh) == (w, h) {
        return (pixels.to_vec(), w, h);
    }
    let img = image::RgbaImage::from_raw(w, h, pixels.to_vec()).expect("buffer of the stated size");
    // Premultiplied pixels resample without dark fringes along transparent edges.
    let out = image::imageops::resize(&img, fw, fh, image::imageops::FilterType::Lanczos3);
    (out.into_raw(), fw, fh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitting_keeps_proportions() {
        let px = [255u8, 0, 0, 255].repeat(400 * 200);
        let (out, w, h) = fit_within(&px, 400, 200, 100, 100);
        assert_eq!((w, h, out.len()), (100, 50, 100 * 50 * 4));
        assert!(out.as_chunks::<4>().0.iter().all(|p| p[3] > 250 && p[0] > 250));
        let (_, w, h) = fit_within(&px, 400, 200, 1000, 1000);
        assert_eq!((w, h), (400, 200), "smaller ones stay as they are");
    }
}
