//! File → Print (Ctrl+P): the picture on a page, as Photoshop's and Clip
//! Studio's print dialogs. Paper and its turn, margins, and how big it prints
//! (fit to the page, actual size at the document's resolution, or a
//! percentage), with a preview of the sheet. Windows prints through its own
//! Print dialog; macOS and Linux send a PDF to the printers CUPS knows; every
//! platform can save the page as a PDF (on iPad and Android it goes on to the
//! share sheet, which prints too).

use std::sync::Arc;

use comic_layout::Unit;
use eframe::egui::{self, Color32, Rect, Sense, Stroke, Vec2};
use painter_core::print::{self, Placement, PrintLayout, PrintScale};

use crate::app::PainterApp;
use crate::print_ui::{length_edit, orientation_buttons, paper_combo, unit_combo};

/// How big it prints, with the percentage kept while another choice is on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Size {
    Fit,
    Actual,
    Percent,
}

/// The last choices, for the next time the dialog opens.
#[derive(Clone, Copy)]
pub(crate) struct PrintSettings {
    layout: PrintLayout,
    size: Size,
    percent: f64,
    unit: Unit,
}

impl Default for PrintSettings {
    fn default() -> Self {
        Self { layout: PrintLayout::default(), size: Size::Fit, percent: 100.0, unit: Unit::Mm }
    }
}

pub(crate) struct PrintDialog {
    settings: PrintSettings,
    /// The flattened picture on white paper, RGB.
    rgb: Arc<Vec<u8>>,
    px: (u32, u32),
    dpi: u32,
    title: String,
    preview: egui::TextureHandle,
    /// CUPS printers (macOS, Linux) and the chosen one.
    printers: Vec<String>,
    printer: usize,
    copies: u32,
}

impl PrintSettings {
    fn layout(&self) -> PrintLayout {
        let scale = match self.size {
            Size::Fit => PrintScale::Fit,
            Size::Actual => PrintScale::Actual,
            Size::Percent => PrintScale::Percent(self.percent),
        };
        PrintLayout { scale, ..self.layout }
    }
}

/// A small copy of the picture for the preview (at most 640 px on its long side).
fn preview_image(rgb: &[u8], (w, h): (u32, u32)) -> egui::ColorImage {
    let k = (640.0 / w.max(h) as f32).min(1.0);
    let (pw, ph) = (((w as f32 * k) as usize).max(1), ((h as f32 * k) as usize).max(1));
    let mut pixels = Vec::with_capacity(pw * ph);
    for y in 0..ph {
        let sy = (y * h as usize / ph).min(h as usize - 1);
        for x in 0..pw {
            let sx = (x * w as usize / pw).min(w as usize - 1);
            let i = (sy * w as usize + sx) * 3;
            pixels.push(Color32::from_rgb(rgb[i], rgb[i + 1], rgb[i + 2]));
        }
    }
    egui::ColorImage { size: [pw, ph], pixels }
}

impl PainterApp {
    /// File → Print: the document as it would export (drafts left out), on paper.
    pub(crate) fn open_print(&mut self) {
        let ctx = self.egui_ctx.clone();
        self.commit_scenes(true);
        let doc = self.doc.without_drafts();
        let rgb = Arc::new(print::on_paper(&painter_core::io::flatten(&doc)));
        let px = doc.canvas();
        let preview = ctx.load_texture("print_preview", preview_image(&rgb, px), egui::TextureOptions::LINEAR);
        let (printers, printer) = cups::printers();
        // The tab's name without its extension: "Untitled 1", "page-03".
        let tab = self.tab_title(self.docs.active);
        let title = std::path::Path::new(&tab).file_stem().map_or(tab.clone(), |s| s.to_string_lossy().into_owned());
        self.print = Some(PrintDialog {
            settings: self.print_settings,
            rgb,
            px,
            dpi: doc.dpi.max(1),
            title,
            preview,
            printers,
            printer,
            copies: 1,
        });
    }

    pub(crate) fn print_window(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.print.take() else { return };
        let mut open = true;
        let mut action = None;
        egui::Window::new("Print").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                let at = print::place(&d.settings.layout(), d.px, d.dpi);
                sheet_preview(ui, &d, &at);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.set_width(300.0);
                    settings_ui(ui, &mut d);
                    ui.separator();
                    // What it comes to.
                    let at = print::place(&d.settings.layout(), d.px, d.dpi);
                    let unit = d.settings.unit;
                    let shown = |mm: f64| format!("{:.*}", unit.decimals().min(1), unit.from_mm(mm, at.dpi.round() as u32));
                    ui.label(format!("Prints {} × {} {} at {:.0} dpi", shown(at.w), shown(at.h), unit.label(), at.dpi));
                    if at.cut {
                        ui.colored_label(crate::theme::warning(), "It's larger than the space inside the margins: the edges are cut.");
                    }
                    if at.dpi < 150.0 {
                        ui.colored_label(crate::theme::warning(), "Under 150 dpi: it may print soft.");
                    }
                    ui.separator();
                    action = buttons_ui(ui, &mut d);
                });
            });
        });
        self.print_settings = d.settings;
        match action {
            Some(Action::Print) => {
                let layout = d.settings.layout();
                let printed = self.send_to_printer(&d, &layout);
                if printed {
                    return;
                }
            }
            Some(Action::Pdf) => {
                if self.save_print_pdf(&d) {
                    return;
                }
            }
            Some(Action::Cancel) | None => {}
        }
        if open && action != Some(Action::Cancel) {
            self.print = Some(d);
        }
    }

    /// Prints with the platform's printing; whether the dialog can close.
    fn send_to_printer(&mut self, d: &PrintDialog, layout: &PrintLayout) -> bool {
        #[cfg(windows)]
        {
            match crate::win_print::print(self.hwnd, &d.rgb, d.px.0, d.px.1, layout, d.dpi, &d.title) {
                Ok(true) => {
                    self.status = format!("Sent {} to the printer", d.title);
                    true
                }
                Ok(false) => false,
                Err(e) => {
                    self.status = format!("Couldn't print: {e}");
                    false
                }
            }
        }
        #[cfg(not(windows))]
        {
            let at = print::place(layout, d.px, d.dpi);
            let file = print::pdf(&d.rgb, d.px.0, d.px.1, layout.page_mm, &at, &d.title);
            let printer = d.printers.get(d.printer).cloned().unwrap_or_default();
            match cups::print(&file, &printer, d.copies, &d.title, layout.page_mm) {
                Ok(()) => {
                    self.status = format!("Sent {} to {printer}", d.title);
                    true
                }
                Err(e) => {
                    self.status = format!("Couldn't print: {e}");
                    false
                }
            }
        }
    }

    /// Writes the page as a PDF; whether it was saved.
    fn save_print_pdf(&mut self, d: &PrintDialog) -> bool {
        let name = format!("{}.pdf", d.title);
        let Some(path) = crate::files::FileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(name).save_file() else {
            return false;
        };
        let layout = d.settings.layout();
        let at = print::place(&layout, d.px, d.dpi);
        match std::fs::write(&path, print::pdf(&d.rgb, d.px.0, d.px.1, layout.page_mm, &at, &d.title)) {
            Ok(()) => {
                crate::files::offer_export(&path);
                self.status = format!("Saved {}", path.display());
                true
            }
            Err(e) => {
                self.status = format!("Couldn't save the PDF: {e}");
                false
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Print,
    Pdf,
    Cancel,
}

/// The sheet, its margins and the picture on it, to scale.
fn sheet_preview(ui: &mut egui::Ui, d: &PrintDialog, at: &Placement) {
    let (area, _) = ui.allocate_exact_size(Vec2::new(300.0, 340.0), Sense::hover());
    let page = d.settings.layout.page_mm;
    let k = ((area.width() - 24.0) as f64 / page[0]).min((area.height() - 24.0) as f64 / page[1]) as f32;
    let sheet = Rect::from_center_size(area.center(), Vec2::new(page[0] as f32 * k, page[1] as f32 * k));
    let painter = ui.painter();
    painter.rect_filled(sheet.translate(Vec2::splat(3.0)), 0.0, Color32::from_black_alpha(90));
    painter.rect_filled(sheet, 0.0, Color32::WHITE);
    let mm = |x: f64, y: f64| sheet.min + Vec2::new(x as f32 * k, y as f32 * k);
    let picture = Rect::from_min_max(mm(at.x, at.y), mm(at.x + at.w, at.y + at.h));
    // Whatever falls off the paper doesn't print.
    let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    ui.painter_at(sheet).image(d.preview.id(), picture, uv, Color32::WHITE);
    let m = d.settings.layout.margin_mm;
    if m > 0.0 {
        let inner = Rect::from_min_max(mm(m, m), mm(page[0] - m, page[1] - m));
        let line = Stroke::new(1.0_f32, crate::theme::accent().gamma_multiply(0.7));
        let c = [inner.left_top(), inner.right_top(), inner.right_bottom(), inner.left_bottom(), inner.left_top()];
        for w in c.windows(2) {
            painter.extend(egui::Shape::dashed_line(&[w[0], w[1]], line, 4.0, 3.0));
        }
    }
}

/// Paper, turn, margins and size.
fn settings_ui(ui: &mut egui::Ui, d: &mut PrintDialog) {
    let s = &mut d.settings;
    egui::Grid::new("print_settings").num_columns(2).spacing([8.0, 8.0]).show(ui, |ui| {
        ui.label("Paper");
        ui.horizontal(|ui| {
            if let Some(size) = paper_combo(ui, "print_paper", s.layout.page_mm) {
                s.layout.page_mm = size;
            }
            unit_combo(ui, "print_unit", &mut s.unit);
        });
        ui.end_row();
        ui.label("");
        ui.horizontal(|ui| {
            length_edit(ui, &mut s.layout.page_mm[0], s.unit, d.dpi, 20.0..=2000.0);
            ui.label("×");
            length_edit(ui, &mut s.layout.page_mm[1], s.unit, d.dpi, 20.0..=2000.0);
        });
        ui.end_row();
        ui.label("Orientation");
        ui.horizontal(|ui| {
            orientation_buttons(ui, &mut s.layout.page_mm);
        });
        ui.end_row();
        ui.label("Margins");
        let most = s.layout.page_mm[0].min(s.layout.page_mm[1]) / 2.0 - 5.0;
        length_edit(ui, &mut s.layout.margin_mm, s.unit, d.dpi, 0.0..=most.max(0.0));
        ui.end_row();
        ui.label("Size");
        ui.vertical(|ui| {
            ui.radio_value(&mut s.size, Size::Fit, "Fit to page");
            ui.radio_value(&mut s.size, Size::Actual, format!("Actual size ({} dpi)", d.dpi))
                .on_hover_text("As big as the document's resolution says");
            ui.horizontal(|ui| {
                ui.radio_value(&mut s.size, Size::Percent, "Scale");
                let r = ui.add(egui::DragValue::new(&mut s.percent).range(1.0..=1000.0).speed(0.5).suffix(" %"));
                if r.changed() {
                    s.size = Size::Percent;
                }
            });
        });
        ui.end_row();
    });
}

fn buttons_ui(ui: &mut egui::Ui, d: &mut PrintDialog) -> Option<Action> {
    let mut action = None;
    let desktop = cfg!(any(windows, target_os = "macos", target_os = "linux"));
    if desktop && !cfg!(windows) {
        if d.printers.is_empty() {
            ui.label("No printer found. Add one in the system settings, or save a PDF.");
        } else {
            ui.horizontal(|ui| {
                ui.label("Printer");
                egui::ComboBox::from_id_salt("print_printer").width(170.0).selected_text(d.printers[d.printer].clone()).show_ui(
                    ui,
                    |ui| {
                        for (i, p) in d.printers.iter().enumerate() {
                            ui.selectable_value(&mut d.printer, i, p);
                        }
                    },
                );
            });
            ui.horizontal(|ui| {
                ui.label("Copies");
                ui.add(egui::DragValue::new(&mut d.copies).range(1..=999));
            });
        }
    }
    ui.horizontal(|ui| {
        let can_print = desktop && (cfg!(windows) || !d.printers.is_empty());
        if can_print {
            let hint =
                if cfg!(windows) { "Choose the printer and copies in Windows' Print dialog" } else { "Send the page to the printer" };
            if ui.button("Print...").on_hover_text(hint).clicked() {
                action = Some(Action::Print);
            }
        }
        let pdf_hint = if desktop { "The page as a PDF file" } else { "The page as a PDF, then share or print it" };
        if ui.button("Save as PDF...").on_hover_text(pdf_hint).clicked() {
            action = Some(Action::Pdf);
        }
        if ui.button("Cancel").clicked() {
            action = Some(Action::Cancel);
        }
    });
    action
}

/// The printers CUPS knows (macOS, Linux), through its `lpstat` and `lp` commands.
mod cups {
    /// Every printer and the default one's place in the list.
    pub(super) fn printers() -> (Vec<String>, usize) {
        if !cfg!(any(target_os = "macos", target_os = "linux")) {
            return (Vec::new(), 0);
        }
        let run = |args: &[&str]| {
            std::process::Command::new("lpstat")
                .args(args)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default()
        };
        let mut names: Vec<String> = run(&["-e"]).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        if names.is_empty() {
            // Older CUPS: "NAME accepting requests since …".
            names = run(&["-a"]).lines().filter_map(|l| l.split_whitespace().next().map(str::to_string)).collect();
        }
        // "system default destination: NAME".
        let default = run(&["-d"]).rsplit(':').next().map(|s| s.trim().to_string()).unwrap_or_default();
        let at = names.iter().position(|n| *n == default).unwrap_or(0);
        (names, at)
    }

    /// The paper as CUPS names it.
    pub(super) fn media(page_mm: [f64; 2]) -> String {
        let (w, h) = (page_mm[0].min(page_mm[1]), page_mm[0].max(page_mm[1]));
        let near = |a: f64, b: f64| (w - a).abs() < 1.0 && (h - b).abs() < 1.0;
        let named = [
            (297.0, 420.0, "A3"),
            (210.0, 297.0, "A4"),
            (148.0, 210.0, "A5"),
            (105.0, 148.0, "A6"),
            (215.9, 279.4, "Letter"),
            (215.9, 355.6, "Legal"),
            (279.4, 431.8, "Tabloid"),
        ];
        match named.iter().find(|(a, b, _)| near(*a, *b)) {
            Some((_, _, name)) => name.to_string(),
            None => format!("Custom.{:.0}x{:.0}mm", w, h),
        }
    }

    #[cfg_attr(windows, allow(dead_code))]
    pub(super) fn print(pdf: &[u8], printer: &str, copies: u32, title: &str, page_mm: [f64; 2]) -> Result<(), String> {
        let path = std::env::temp_dir().join("painter-print.pdf");
        std::fs::write(&path, pdf).map_err(|e| e.to_string())?;
        let out = std::process::Command::new("lp")
            .args(["-d", printer, "-n", &copies.to_string(), "-t", title, "-o", &format!("media={}", media(page_mm))])
            .args(["-o", "print-scaling=none"])
            .arg(&path)
            .output()
            .map_err(|e| format!("lp: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn paper_names_for_cups() {
            assert_eq!(super::media([210.0, 297.0]), "A4");
            assert_eq!(super::media([297.0, 210.0]), "A4");
            assert_eq!(super::media([215.9, 279.4]), "Letter");
            assert_eq!(super::media([182.0, 257.0]), "Custom.182x257mm");
        }
    }
}
