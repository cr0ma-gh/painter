//! File → Import → From Scanner and File → Export → SVG dialogs.

use std::thread::JoinHandle;
use crate::i18n::tr;

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
    /// Split the page into its drawn panels, a layer each (and their order for manga).
    panels: bool,
    panels_rtl: bool,
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
    /// Where a scan goes: a new document, or a layer here (and what happens
    /// when it's bigger than the canvas).
    oversize: Oversize,
    /// The canvas a layer would go on, in pixels (for the size warning).
    canvas: (u32, u32),
    /// A network scanner's address being typed in.
    address: String,
    /// Network scanners were found while a job ran: look again after it.
    refresh_pending: bool,
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
            panels: false,
            panels_rtl: false,
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
            canvas: (0, 0),
            address: String::new(),
            refresh_pending: false,
        };
        // iPad: Bonjour looks for network (AirScan) scanners; they arrive in `scanner_window`.
        crate::scan_source::find_network_scanners();
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
        dialog.canvas = self.doc.canvas();
        let import = dialog.poll(ctx);
        // Pages from the iPad's document camera.
        let camera_pages = crate::scan_source::take_pages();
        if !camera_pages.is_empty() {
            dialog.add_pages(camera_pages);
        }
        if let Some(err) = crate::scan_source::take_error() {
            dialog.message = err;
        }
        // Network scanners Bonjour found (iPad): list them.
        let network = crate::scan_source::take_network_scanners();
        if !network.is_empty() {
            scanner_io::add_targets(network);
            dialog.refresh_pending = true;
        }
        if dialog.refresh_pending && dialog.job.is_none() {
            dialog.refresh_pending = false;
            dialog.refresh();
        }
        if crate::scan_source::camera_active() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        dialog.show_page(ctx);
        let mut pick = false;
        egui::Window::new(tr("Import from Scanner or Camera")).collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
            ui.set_width(520.0);
            if let Some(job) = &dialog.job {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tr(job.label()));
                });
            }
            let busy = dialog.job.is_some();
            // Where the pages come from: a scanner, the camera, or image files.
            ui.horizontal(|ui| {
                if crate::scan_source::camera_supported()
                    && ui
                        .add_enabled(!busy && !crate::scan_source::camera_active(), egui::Button::new(tr("Scan with camera")))
                        .on_hover_text(tr("The camera finds the sheet and straightens it; take as many pages as you like"))
                        .clicked()
                {
                    dialog.message.clear();
                    crate::scan_source::start_camera();
                }
                if ui
                    .add_enabled(!busy, egui::Button::new(tr("From photo or file...")))
                    .on_hover_text(tr("A photo or a scan of a page, cleaned up like a scan"))
                    .clicked()
                {
                    pick = true;
                }
                if !dialog.pages.is_empty() && ui.add_enabled(!busy, egui::Button::new(tr("Use a scanner"))).on_hover_text(tr("Forget these pages")).clicked()
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
            // Each scan (each page) its own document, or a layer in this one.
            let oversize = if dialog.oversize == Oversize::NewDocument { self.new_scan_document(image) } else { dialog.oversize };
            if dialog.panels {
                self.import_scan_panels(image, oversize, dialog.panels_rtl);
            } else {
                self.import_scan(image, oversize);
            }
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

    /// A scanned page split into its drawn panels: a layer each, where it is on
    /// the page, in reading order, in a "Panels" folder. Without panels, the page.
    pub(crate) fn import_scan_panels(&mut self, image: &ScannedImage, oversize: Oversize, right_to_left: bool) {
        let panels = scanner_io::find_panels(image, right_to_left);
        if panels.is_empty() {
            self.import_scan(image, oversize);
            self.status += " (no drawn panels found: imported the whole page)";
            return;
        }
        let mut ids = Vec::new();
        for (i, panel) in panels.iter().enumerate() {
            self.import_scan(&scanner_io::isolate(image, panel), oversize);
            let id = self.doc.active;
            if let Some(layer) = self.doc.find_mut(id) {
                layer.name = format!("Panel {}", i + 1);
            }
            ids.push(id);
        }
        if let Some(folder) = self.doc.group_layers(&ids, "Panels") {
            self.doc.active = folder;
        }
        self.resync_gpu();
        self.status = format!("Imported {} panels, a layer each, in reading order", panels.len());
    }

    /// A new document the size and resolution of `image` (on white paper), for
    /// the scan to go into; returns how to place it there. A scan past the
    /// largest canvas is scaled down to it.
    pub(crate) fn new_scan_document(&mut self, image: &ScannedImage) -> Oversize {
        let max = painter_core::MAX_CANVAS_SIZE;
        let (w, h) = (image.width.min(max), image.height.min(max));
        let mut doc = painter_core::Document::new(w, h);
        doc.dpi = image.dpi.max(1);
        // Only the paper: the scan comes in as its own layer.
        let empty = doc.active;
        doc.delete_layers(&[empty]);
        self.set_document(doc, None);
        if self.doc.canvas() != (w, h) {
            // No new tab (the current document couldn't be put aside): a layer
            // here, the canvas grown to keep every pixel.
            return Oversize::Enlarge;
        }
        let active = self.docs.active;
        self.docs.slots[active].name = Some(format!("Scan {} dpi", image.dpi));
        if image.width > max || image.height > max { Oversize::Fit } else { Oversize::Cut }
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
        egui::Window::new(tr("Export SVG")).collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut dialog.active_layer_only, false, tr("Whole canvas"));
                ui.selectable_value(&mut dialog.active_layer_only, true, tr("Active layer"));
            });
            ui.separator();
            ui.label(egui::RichText::new(tr("Raster tracing")).strong());
            let trace = &mut dialog.options.trace;
            ui.horizontal(|ui| {
                ui.label(tr("Ink from"));
                ui.selectable_value(&mut trace.source, ThresholdSource::Alpha, tr("Opacity"));
                ui.selectable_value(&mut trace.source, ThresholdSource::Luminance, tr("Darkness"));
            });
            let mut threshold = trace.threshold as f32;
            if prop_slider(ui, "Threshold", &mut threshold, 0.0..=254.0, "", 0, false) {
                trace.threshold = threshold.round() as u8;
            }
            prop_slider(ui, "Path simplification", &mut trace.simplify, 0.0..=4.0, " px", 2, false);
            prop_slider(ui, "Corner sensitivity", &mut trace.corner_angle, 10.0..=170.0, "°", 0, false);
            ui.small(tr("Turns sharper than this angle stay corners; lower is more angular."));
            prop_slider(ui, "Remove specks under", &mut trace.despeckle, 0.0..=200.0, " px²", 0, false);
            ui.separator();
            ui.label(egui::RichText::new(tr("Vector strokes")).strong());
            ui.horizontal(|ui| {
                ui.selectable_value(&mut dialog.options.strokes, StrokeMode::FilledOutline, tr("Pressure outline"));
                ui.selectable_value(&mut dialog.options.strokes, StrokeMode::Centerline, tr("Centre line"));
            });
            ui.separator();
            ui.checkbox(&mut dialog.options.include_paper, tr("Include paper as background"));
            ui.checkbox(&mut dialog.options.skip_hidden, tr("Skip hidden layers"));
            ui.add_space(4.0);
            export = ui.button(tr("Export…")).clicked();
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
            ui.small(tr("Network scanners (AirScan) on your Wi-Fi show up here; or type the scanner's address below, scan with the camera, or pick a photo of the page."));
        } else if dialog.errors.is_empty() {
            ui.small(tr("Connect and switch on a scanner, then refresh; or pick a photo of the page."));
        }
        if ui.add_enabled(!busy, egui::Button::new(tr("Look again"))).clicked() {
            crate::scan_source::find_network_scanners();
            dialog.refresh();
        }
        network_row(ui, dialog, busy);
        message(ui, dialog);
        return;
    }
    ui.add_enabled_ui(!busy, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width(220.0);
                let names: Vec<String> = dialog.devices.iter().map(|d| format!("{} ({})", d.name, d.backend.name())).collect();
                egui::ComboBox::from_label(tr("Device")).selected_text(&names[dialog.device]).show_ui(ui, |ui| {
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
                egui::ComboBox::from_label(tr("Resolution")).selected_text(format!("{} dpi", dialog.settings.dpi)).show_ui(ui, |ui| {
                    for dpi in &caps.resolutions {
                        ui.selectable_value(&mut dialog.settings.dpi, *dpi, format!("{dpi} dpi"));
                    }
                });
                egui::ComboBox::from_label(tr("Mode")).selected_text(tr(dialog.settings.color_mode.name())).show_ui(ui, |ui| {
                    for mode in ColorMode::ALL.into_iter().filter(|m| caps.color_modes.contains(m)) {
                        ui.selectable_value(&mut dialog.settings.color_mode, mode, tr(mode.name()));
                    }
                });
                ui.separator();
                cleanup_controls(ui, dialog);
            });
            preview_area(ui, dialog, "Preview, then drag to crop");
        })
    });
    ui.collapsing("Network scanner by address", |ui| network_row(ui, dialog, busy));
    message(ui, dialog);
    ui.separator();
    ui.add_enabled_ui(!busy, |ui| {
        ui.horizontal(|ui| {
            if ui.button(tr("Preview")).clicked() {
                dialog.message.clear();
                dialog.start_preview();
            }
            if ui.button(tr("Scan and import")).clicked() {
                dialog.message.clear();
                dialog.start_scan();
            }
            if ui.button(tr("Refresh devices")).clicked() {
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
                    egui::ComboBox::from_label(tr("Page")).selected_text(format!("{} of {count}", dialog.page + 1)).show_ui(ui, |ui| {
                        for i in 0..count {
                            ui.selectable_value(&mut dialog.page, i, format!("Page {}", i + 1));
                        }
                    });
                }
                egui::ComboBox::from_label(tr("Paper")).selected_text(PAGE_SIZES[dialog.paper].0).show_ui(ui, |ui| {
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
            if ui.button(tr("Import page")).on_hover_text(tr("As a new layer")).clicked() {
                dialog.message.clear();
                dialog.start_pages(false);
            }
            if dialog.pages.len() > 1 && ui.button(format!("Import all {} pages", dialog.pages.len())).on_hover_text(tr("One layer each, same crop and cleanup")).clicked() {
                dialog.message.clear();
                dialog.start_pages(true);
            }
            if ui.button(tr("Remove page")).clicked() {
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

/// A network (AirScan / eSCL) scanner by its address, for when it isn't found by itself.
fn network_row(ui: &mut egui::Ui, dialog: &mut ScannerDialog, busy: bool) {
    ui.horizontal(|ui| {
        ui.label(tr("Address"));
        let field = ui.add(egui::TextEdit::singleline(&mut dialog.address).hint_text(tr("192.168.1.30")).desired_width(150.0));
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.add_enabled(!busy, egui::Button::new(tr("Add"))).on_hover_text(tr("A scanner on your network that scans over AirScan (eSCL)")).clicked() || enter) && !busy {
            match scanner_io::add_address(&dialog.address) {
                Ok(_) => {
                    dialog.message.clear();
                    dialog.refresh();
                }
                Err(err) => dialog.message = err,
            }
        }
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
        ui.label(tr("Rotate"));
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
        ui.label(tr("Flip"));
        ui.checkbox(&mut dialog.orientation.flip_horizontal, tr("horizontal"));
        ui.checkbox(&mut dialog.orientation.flip_vertical, tr("vertical"));
    });
    ui.separator();
    egui::ComboBox::from_label(tr("Put it in")).selected_text(tr(dialog.oversize.label())).show_ui(ui, |ui| {
        for o in Oversize::ALL {
            ui.selectable_value(&mut dialog.oversize, o, tr(o.label()));
        }
    });
    // How big the scan will be, from the device's bed and the crop, or the page in hand.
    let expected = match (dialog.devices.get(dialog.device), dialog.pages.get(dialog.page)) {
        (_, Some(page)) => Some((page.width, page.height, page.dpi)),
        (Some(d), None) if d.capabilities.bed_mm[0] > 0.0 => {
            let a = dialog.crop.clamped();
            let px = |mm: f32, k: f32| (mm * k / 25.4 * dialog.settings.dpi as f32).round() as u32;
            Some((px(d.capabilities.bed_mm[0], a.width), px(d.capabilities.bed_mm[1], a.height), dialog.settings.dpi))
        }
        _ => None,
    };
    if let Some(text) = expected.and_then(|(w, h, dpi)| size_warning((w, h), dpi, dialog.canvas, dialog.oversize)) {
        ui.colored_label(theme::warning(), text);
    }
    ui.checkbox(&mut dialog.auto_contrast, tr("Auto contrast"));
    let mut extract = dialog.line_art.is_some();
    if ui.checkbox(&mut extract, tr("Extract line art (paper → transparent)")).changed() {
        dialog.line_art = extract.then(LineArtOptions::default);
    }
    if let Some(line_art) = &mut dialog.line_art {
        prop_percent(ui, "Paper above", &mut line_art.paper, 0.0..=100.0);
        prop_percent(ui, "Solid ink below", &mut line_art.ink, 0.0..=100.0);
        ui.checkbox(&mut line_art.keep_color, tr("Keep line colour"));
    }
    ui.checkbox(&mut dialog.panels, tr("Extract panels"))
        .on_hover_text(tr("Rectangles drawn as frames become a layer each, in a Panels folder"));
    if dialog.panels {
        ui.checkbox(&mut dialog.panels_rtl, tr("Right to left (manga order)"));
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

/// Where a scan goes: its own document, or a layer here (and what happens
/// when it's bigger than the canvas).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Oversize {
    /// A new document of the scan's size and resolution, as Photoshop opens scans.
    #[default]
    NewDocument,
    /// A layer here, scaled down to fit inside the canvas, centred (fewer pixels: a lower resolution).
    Fit,
    /// A layer here; the canvas grows to hold it.
    Enlarge,
    /// A layer here at the top left; what is past the canvas is deleted.
    Cut,
}

impl Oversize {
    const ALL: [Oversize; 4] = [Oversize::NewDocument, Oversize::Fit, Oversize::Enlarge, Oversize::Cut];

    fn label(self) -> &'static str {
        match self {
            Oversize::NewDocument => "New document (full resolution)",
            Oversize::Fit => "Layer, fit in the canvas",
            Oversize::Enlarge => "Layer, enlarge the canvas",
            Oversize::Cut => "Layer, cut what's outside",
        }
    }
}

/// What a layer scan of `scan` px does to a canvas of `canvas` px with `oversize`,
/// when it isn't simply placed: the warning the dialog shows.
fn size_warning(scan: (u32, u32), dpi: u32, canvas: (u32, u32), oversize: Oversize) -> Option<String> {
    let (w, h) = scan;
    let (cw, ch) = canvas;
    if oversize == Oversize::NewDocument || cw == 0 || (w <= cw && h <= ch) {
        return None;
    }
    let size = format!("The scan is about {w} × {h} px; the canvas is {cw} × {ch} px");
    Some(match oversize {
        Oversize::Fit => {
            let k = (cw as f32 / w as f32).min(ch as f32 / h as f32);
            format!("{size}: it will be scaled to {:.0}%, about {:.0} dpi. Choose New document to keep every pixel.", k * 100.0, dpi as f32 * k)
        }
        Oversize::Cut => format!("{size}: what's past the canvas is cut."),
        Oversize::Enlarge => format!("{size}: the canvas grows to hold it."),
        Oversize::NewDocument => unreachable!(),
    })
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

/// Edit → Image Size: the whole document resampled to a new pixel size.
pub(crate) struct ImageSizeDialog {
    width: u32,
    height: u32,
    keep_ratio: bool,
    filter: transform_tools::Interpolation,
    /// The resolution follows, so the page keeps its size on paper.
    keep_print_size: bool,
}

impl ImageSizeDialog {
    pub(crate) fn open(canvas: (u32, u32), filter: transform_tools::Interpolation) -> Self {
        Self { width: canvas.0, height: canvas.1, keep_ratio: true, filter, keep_print_size: false }
    }
}

impl PainterApp {
    pub(crate) fn image_size_window(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.image_size.take() else { return };
        let (cw, ch) = self.doc.canvas();
        let mut open = true;
        let mut apply = false;
        egui::Window::new(tr("Image Size")).collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
            ui.label(format!("Now {cw} × {ch} px, {} dpi", self.doc.dpi));
            ui.separator();
            let max = painter_core::MAX_CANVAS_SIZE;
            let (w0, h0) = (dialog.width, dialog.height);
            egui::Grid::new("image_size_grid").num_columns(2).show(ui, |ui| {
                ui.label(tr("Width"));
                ui.add(egui::DragValue::new(&mut dialog.width).range(1..=max).suffix(" px"));
                ui.end_row();
                ui.label(tr("Height"));
                ui.add(egui::DragValue::new(&mut dialog.height).range(1..=max).suffix(" px"));
                ui.end_row();
                ui.label(tr("Scale"));
                let mut percent = dialog.width as f32 / cw as f32 * 100.0;
                if ui.add(egui::DragValue::new(&mut percent).range(1.0..=10_000.0).suffix(" %").max_decimals(1)).changed() {
                    dialog.width = ((cw as f32 * percent / 100.0).round() as u32).clamp(1, max);
                }
                ui.end_row();
            });
            // Keep the proportions: the other side follows the one edited.
            if dialog.keep_ratio {
                if dialog.width != w0 {
                    dialog.height = ((dialog.width as f64 * ch as f64 / cw as f64).round() as u32).clamp(1, max);
                } else if dialog.height != h0 {
                    dialog.width = ((dialog.height as f64 * cw as f64 / ch as f64).round() as u32).clamp(1, max);
                }
            }
            ui.horizontal(|ui| {
                for (label, k) in [("50%", 0.5), ("200%", 2.0), ("300%", 3.0), ("400%", 4.0)] {
                    if ui.small_button(label).clicked() {
                        dialog.width = ((cw as f64 * k).round() as u32).clamp(1, max);
                        dialog.height = ((ch as f64 * k).round() as u32).clamp(1, max);
                    }
                }
            });
            ui.checkbox(&mut dialog.keep_ratio, tr("Keep proportions"));
            ui.separator();
            egui::ComboBox::from_label(tr("Resampling")).selected_text(filter_label(dialog.filter)).show_ui(ui, |ui| {
                for f in transform_tools::Interpolation::ALL {
                    ui.selectable_value(&mut dialog.filter, f, filter_label(f));
                }
            });
            if dialog.filter == transform_tools::Interpolation::Nearest {
                let whole = |n: u32, d: u32| n.is_multiple_of(d) || d.is_multiple_of(n);
                if !(whole(dialog.width, cw) && whole(dialog.height, ch)) {
                    ui.colored_label(
                        Color32::from_rgb(240, 190, 90),
                        "Not a whole multiple (×2, ×3, ½…): some pixels become wider than others",
                    );
                }
            }
            ui.checkbox(&mut dialog.keep_print_size, tr("Keep the size on paper (the resolution changes)"));
            if dialog.keep_print_size {
                let dpi = (self.doc.dpi as f32 * dialog.width as f32 / cw as f32).round();
                ui.small(format!("{dpi} dpi"));
            }
            ui.separator();
            ui.horizontal(|ui| {
                let same = (dialog.width, dialog.height) == (cw, ch);
                if ui.add_enabled(!same, egui::Button::new(tr("Resize"))).clicked() {
                    apply = true;
                }
            });
        });
        if apply {
            self.commit_scenes(true);
            self.checkpoint();
            transform_tools::resize_document(&mut self.doc, dialog.width, dialog.height, dialog.filter, dialog.keep_print_size);
            self.resync_gpu();
            self.fit_pending = true;
            self.last_resize_filter = dialog.filter;
            self.status = format!(
                "Resized to {} × {} px ({}; Ctrl+Z restores it)",
                self.doc.width,
                self.doc.height,
                filter_label(dialog.filter)
            );
        } else if open {
            self.image_size = Some(dialog);
        }
    }
}

fn filter_label(f: transform_tools::Interpolation) -> &'static str {
    match f {
        transform_tools::Interpolation::Nearest => "Nearest neighbour (keep pixels)",
        transform_tools::Interpolation::Bilinear => "Bilinear (smooth)",
        transform_tools::Interpolation::Bicubic => "Bicubic (smooth, sharper)",
        transform_tools::Interpolation::Lanczos3 => "Lanczos3 (sharpest)",
    }
}

/// Save As on iPad, where there's no save panel: the name and the format;
/// the file goes into the app's own folder (Files → On My iPad → Painter),
/// and "Save and copy to..." then hands a copy to the system picker.
pub(crate) struct SaveAsPrompt {
    stem: String,
    format: usize,
}

/// Formats Save As writes: label and extension.
const SAVE_FORMATS: [(&str, &str); 3] = [
    ("Painter project", painter_core::project::EXTENSION),
    ("Krita (.kra)", "kra"),
    ("OpenRaster (.ora)", "ora"),
];

impl SaveAsPrompt {
    pub(crate) fn new(file_name: &str) -> Self {
        let path = std::path::Path::new(file_name);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let format = SAVE_FORMATS.iter().position(|(_, e)| *e == ext).unwrap_or(0);
        let stem = path.file_stem().map_or_else(|| "painting".into(), |s| s.to_string_lossy().into_owned());
        Self { stem, format }
    }
}

/// Where `stem.ext` goes in `dir`: characters files can't have are replaced,
/// and a different existing file isn't overwritten (" 2", " 3"… are added);
/// `current` (the file being saved over) may be reused.
pub(crate) fn save_target(dir: &std::path::Path, stem: &str, ext: &str, current: Option<&std::path::Path>) -> std::path::PathBuf {
    let clean: String = stem.trim().chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c }).collect();
    let clean = if clean.is_empty() { "painting".to_string() } else { clean };
    let mut path = dir.join(format!("{clean}.{ext}"));
    let mut n = 2;
    while path.exists() && Some(path.as_path()) != current {
        path = dir.join(format!("{clean} {n}.{ext}"));
        n += 1;
    }
    path
}

impl PainterApp {
    pub(crate) fn save_as_window(&mut self, ctx: &egui::Context) {
        let Some(mut prompt) = self.save_as_prompt.take() else { return };
        let mut open = true;
        let mut action = None;
        egui::Window::new(tr("Save As")).collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(tr("Name"));
                let field = ui.add(egui::TextEdit::singleline(&mut prompt.stem).desired_width(260.0));
                if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    action = Some(false);
                }
            });
            egui::ComboBox::from_label(tr("Format")).selected_text(SAVE_FORMATS[prompt.format].0).show_ui(ui, |ui| {
                for (i, (label, _)) in SAVE_FORMATS.iter().enumerate() {
                    ui.selectable_value(&mut prompt.format, i, *label);
                }
            });
            if prompt.format != 0 {
                ui.small(tr("Krita and OpenRaster flatten masks and clipping into the pixels."));
            }
            ui.small(tr("Saved in the Painter folder (Files app → On My iPad → Painter)."));
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button(tr("Save")).clicked() {
                    action = Some(false);
                }
                if ui.button(tr("Save and copy to...")).on_hover_text(tr("Then choose where a copy goes: iCloud Drive, another folder or app")).clicked() {
                    action = Some(true);
                }
            });
        });
        match action {
            Some(copy) => match crate::files::documents_dir() {
                Some(dir) => {
                    let ext = SAVE_FORMATS[prompt.format].1;
                    let path = save_target(&dir, &prompt.stem, ext, self.file_path.as_deref());
                    if copy {
                        self.export_after_save = Some(path.clone());
                    }
                    self.save_to(path);
                }
                None => self.status = tr("No folder to save into").into(),
            },
            None if open => self.save_as_prompt = Some(prompt),
            None => {}
        }
    }
}

/// Edit → Define Brush Preset dialog state.
pub(crate) struct DefineBrushDialog {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) alpha: Vec<u8>,
    pub(crate) name: String,
    pub(crate) preview_texture: Option<egui::TextureHandle>,
    pub(crate) focus_requested: bool,
}

impl DefineBrushDialog {
    pub(crate) fn new(width: u32, height: u32, alpha: Vec<u8>, name: String) -> Self {
        Self { width, height, alpha, name, preview_texture: None, focus_requested: false }
    }
}

impl PainterApp {
    /// Edit → Define Brush Preset: samples a custom tip from the selection or layer
    /// and opens the definition dialog with an interactive preview.
    pub(crate) fn define_brush_preset_dialog(&mut self) {
        let (cw, ch) = self.doc.canvas();
        if cw == 0 || ch == 0 {
            self.status = tr("No canvas open").into();
            return;
        }

        let active_layer = self.doc.active_layer();
        let active_tiles = active_layer.and_then(|l| l.tiles());

        let area = match &self.doc.selection {
            Some(sel) => painter_core::selection::bounds(sel),
            None => active_tiles.and_then(painter_core::selection::bounds).or_else(|| {
                let has_any = self.doc.layers.iter().any(|l| l.tiles().is_some_and(|t| !t.is_empty()));
                if has_any {
                    Some([0, 0, (cw.saturating_sub(1)) as i32, (ch.saturating_sub(1)) as i32])
                } else {
                    None
                }
            }),
        };

        let [x0, y0, x1, y1] = match area {
            Some(b) => b,
            None => {
                self.status = tr("Define Brush Preset: no artwork found in active layer or selection").into();
                return;
            }
        };

        let x0 = (x0.max(0) as u32).min(cw.saturating_sub(1));
        let y0 = (y0.max(0) as u32).min(ch.saturating_sub(1));
        let x1 = (x1.max(x0 as i32) as u32).min(cw.saturating_sub(1));
        let y1 = (y1.max(y0 as i32) as u32).min(ch.saturating_sub(1));

        let w = x1 - x0 + 1;
        let h = y1 - y0 + 1;
        if w == 0 || h == 0 {
            self.status = tr("Define Brush Preset: selected area is empty").into();
            return;
        }

        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        if let Some(tiles) = active_tiles {
            for y in 0..h {
                for x in 0..w {
                    let px = painter_core::blend::unpremultiply(tiles.pixel((x0 + x) as i32, (y0 + y) as i32));
                    rgba.extend_from_slice(&px);
                }
            }
        } else {
            let flat = painter_core::io::flatten_region(&self.doc, x0, y0, w, h);
            for chunk in flat.as_chunks::<4>().0 {
                rgba.extend_from_slice(&painter_core::blend::unpremultiply(*chunk));
            }
        }

        let sel_alpha = self.doc.selection.as_ref().map(|sel| {
            let mut alpha = Vec::with_capacity((w * h) as usize);
            for y in 0..h {
                for x in 0..w {
                    alpha.push(sel.pixel((x0 + x) as i32, (y0 + y) as i32)[3]);
                }
            }
            alpha
        });

        let sampled = painter_core::sample_tip_coverage(&rgba, w, h, sel_alpha.as_deref(), 2500, true);
        let Some((tip_w, tip_h, tip_alpha)) = sampled else {
            self.status = tr("Define Brush Preset: area contains no dark markings to sample").into();
            return;
        };

        let default_name = active_layer
            .map(|l| format!("{} Tip", l.name))
            .unwrap_or_else(|| format!("Sampled Brush {}", self.brushes.len() + 1));

        self.define_brush = Some(DefineBrushDialog::new(tip_w, tip_h, tip_alpha, default_name));
    }

    /// Renders the Define Brush Preset modal window if active.
    pub(crate) fn define_brush_window(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.define_brush.take() else { return };
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;

        if dialog.preview_texture.is_none() {
            let (pw, ph) = (dialog.width as usize, dialog.height as usize);
            let mut pixels = Vec::with_capacity(pw * ph);
            for &a in &dialog.alpha {
                pixels.push(Color32::from_rgba_unmultiplied(20, 20, 25, a));
            }
            let image = egui::ColorImage { size: [pw, ph], pixels };
            dialog.preview_texture = Some(ctx.load_texture("define_brush_preview", image, egui::TextureOptions::LINEAR));
        }

        egui::Window::new(tr("Define Brush Preset"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    if let Some(tex) = &dialog.preview_texture {
                        let max_side = 160.0;
                        let longest = (dialog.width.max(dialog.height) as f32).max(1.0);
                        let w = (dialog.width as f32 / longest * max_side).max(16.0);
                        let h = (dialog.height as f32 / longest * max_side).max(16.0);

                        let (rect, _response) = ui.allocate_exact_size(Vec2::new(w, h), Sense::hover());
                        ui.painter().rect_filled(rect, 4.0, Color32::from_gray(248));
                        ui.painter().rect_stroke(rect, 4.0, Stroke::new(1.0_f32, Color32::from_gray(180)), egui::StrokeKind::Inside);
                        ui.painter().image(
                            tex.id(),
                            rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    ui.add_space(4.0);
                    ui.label(format!("{} × {} px", dialog.width, dialog.height));
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(tr("Brush Name:"));
                    let field = egui::TextEdit::singleline(&mut dialog.name).desired_width(220.0);
                    let edit = ui.add(field);
                    if !dialog.focus_requested {
                        edit.request_focus();
                        dialog.focus_requested = true;
                    }
                    if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        apply = true;
                    }
                });
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancel = true;
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(tr("Cancel")).clicked() {
                        cancel = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(tr("OK")).clicked() {
                            apply = true;
                        }
                    });
                });
            });

        if cancel {
            open = false;
        }

        if apply {
            let brush_name = if dialog.name.trim().is_empty() {
                format!("Sampled Brush {}", self.brushes.len() + 1)
            } else {
                dialog.name.trim().to_string()
            };
            let (bw, bh) = (dialog.width, dialog.height);
            let brush_tip = brush_io::BrushTip::new(bw, bh, dialog.alpha)
                .expect("valid sampled tip");
            let mut preset = brush_io::BrushPreset::new(painter_core::BrushSettings {
                name: brush_name.clone(),
                size: (bw.max(bh) as f32).clamp(1.0, 5000.0),
                spacing: 0.15,
                hardness: 1.0,
                pressure_size: true,
                min_size_ratio: 0.0,
                stabilization: 0,
                ..painter_core::BrushSettings::g_pen()
            });
            preset.tip = Some(brush_tip);
            preset.apply_tip();

            self.brushes.push(preset);
            let new_index = self.brushes.len() - 1;
            self.select_brush(new_index);
            self.tool = crate::tools::Tool::Brush;
            self.brush_library.autosave(&self.brushes, false);
            self.status = format!(
                "{} “{}” ({} × {} px)",
                tr("Defined brush preset"),
                brush_name,
                bw,
                bh
            );
        } else if open {
            self.define_brush = Some(dialog);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_open_at_full_resolution_unless_asked() {
        // The default is a new document: an A4 at 300 dpi keeps every pixel.
        assert_eq!(Oversize::default(), Oversize::NewDocument);
        let a4 = (2550, 3507);
        assert!(size_warning(a4, 300, (1654, 2339), Oversize::NewDocument).is_none());
        // Fit into a smaller canvas says how much resolution is lost, before scanning.
        let w = size_warning(a4, 300, (1654, 2339), Oversize::Fit).unwrap();
        assert!(w.contains("65%") && w.contains("195 dpi"), "{w}");
        assert!(size_warning(a4, 300, (2550, 3507), Oversize::Fit).is_none(), "it fits: nothing to say");
        assert!(size_warning(a4, 300, (1000, 1000), Oversize::Cut).unwrap().contains("cut"));
    }

    #[test]
    fn fitting_keeps_proportions() {
        let px = [255u8, 0, 0, 255].repeat(400 * 200);
        let (out, w, h) = fit_within(&px, 400, 200, 100, 100);
        assert_eq!((w, h, out.len()), (100, 50, 100 * 50 * 4));
        assert!(out.as_chunks::<4>().0.iter().all(|p| p[3] > 250 && p[0] > 250));
        let (_, w, h) = fit_within(&px, 400, 200, 1000, 1000);
        assert_eq!((w, h), (400, 200), "smaller ones stay as they are");
    }

    #[test]
    fn save_targets_avoid_other_files() {
        let dir = std::env::temp_dir().join(format!("painter-saveas-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(save_target(&dir, " a/b ", "kra", None), dir.join("a_b.kra"));
        std::fs::write(dir.join("x.painter"), b"").unwrap();
        assert_eq!(save_target(&dir, "x", "painter", None), dir.join("x 2.painter"), "a different file isn't overwritten");
        let current = dir.join("x.painter");
        assert_eq!(save_target(&dir, "x", "painter", Some(&current)), current, "saving over itself is fine");
        assert_eq!(save_target(&dir, "  ", "ora", None), dir.join("painting.ora"));
        let p = SaveAsPrompt::new("Sketch.KRA");
        assert_eq!((p.stem.as_str(), p.format), ("Sketch", 1));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn define_brush_dialog_and_preset_generation() {
        let (w, h) = (8, 8);
        let alpha = vec![255u8; (w * h) as usize];
        let dialog = DefineBrushDialog::new(w, h, alpha.clone(), "Sampled Tip".to_string());
        assert_eq!((dialog.width, dialog.height), (8, 8));
        assert_eq!(dialog.name, "Sampled Tip");
        assert_eq!(dialog.alpha, alpha);
        assert!(dialog.preview_texture.is_none());

        let brush_tip = brush_io::BrushTip::new(w, h, dialog.alpha).expect("valid brush tip");
        let mut preset = brush_io::BrushPreset::new(painter_core::BrushSettings {
            name: dialog.name,
            size: 8.0,
            spacing: 0.15,
            hardness: 1.0,
            pressure_size: true,
            stabilization: 0,
            ..painter_core::BrushSettings::g_pen()
        });
        preset.tip = Some(brush_tip);
        preset.apply_tip();

        assert_eq!(preset.name(), "Sampled Tip");
        assert!(matches!(preset.settings.tip.shape, painter_core::TipShape::Sampled(_)));
        let bitmap = preset.settings.tip.bitmap().expect("bitmap exists");
        assert_eq!((bitmap.width(), bitmap.height()), (8, 8));
        assert_eq!(bitmap.alpha().len(), 64);
    }
}
