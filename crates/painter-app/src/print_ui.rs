//! Physical sizes in dialogs: paper formats, px / mm / in fields, resolution,
//! and the New Canvas dialog built on them.
//!
//! Lengths are held in millimetres and shown in the chosen unit, so switching
//! units never changes the page; pixels follow from the resolution.

use std::ops::RangeInclusive;

use comic_layout::paper::{mm_to_whole_px, px_to_mm, STANDARD_BLEED_MM, STANDARD_SAFE_MM};
use comic_layout::{Binding, ComicPage, Orientation, PagePreset, PaperSize, Unit, DPI_PRESETS, PAPER_SIZES};
use eframe::egui::{self, Align2, Color32, Sense, Vec2};
use painter_core::{Document, MAX_CANVAS_SIZE};

use crate::app::PainterApp;
use crate::comic_ui::draw_page;
use crate::theme;

/// A length field: shows `mm` in `unit`, writes edits back as millimetres.
pub(crate) fn length_edit(ui: &mut egui::Ui, mm: &mut f64, unit: Unit, dpi: u32, range_mm: RangeInclusive<f64>) -> egui::Response {
    let mut value = unit.from_mm(*mm, dpi);
    let range = unit.from_mm(*range_mm.start(), dpi)..=unit.from_mm(*range_mm.end(), dpi);
    let speed = match unit {
        Unit::Px => 1.0,
        Unit::Mm => 0.1,
        Unit::In => 0.01,
    };
    let response = ui.add(
        egui::DragValue::new(&mut value)
            .range(range)
            .speed(speed)
            .min_decimals(unit.decimals())
            .max_decimals(unit.decimals())
            .suffix(format!(" {}", unit.label())),
    );
    if response.changed() {
        if unit == Unit::Px {
            value = value.round();
        }
        *mm = unit.to_mm(value, dpi).clamp(*range_mm.start(), *range_mm.end());
    }
    response
}

pub(crate) fn unit_combo(ui: &mut egui::Ui, id: &str, unit: &mut Unit) {
    egui::ComboBox::from_id_salt(id).width(52.0).selected_text(unit.label()).show_ui(ui, |ui| {
        for u in Unit::ALL {
            ui.selectable_value(unit, u, u.label());
        }
    });
}

/// Resolution: the usual values in a list, any value typed beside it.
/// Returns whether it changed.
pub(crate) fn dpi_edit(ui: &mut egui::Ui, id: &str, dpi: &mut u32) -> bool {
    let before = *dpi;
    egui::ComboBox::from_id_salt(id).width(64.0).selected_text(format!("{dpi} dpi")).show_ui(ui, |ui| {
        for value in DPI_PRESETS {
            let hint = match value {
                72 | 96 => "screen",
                150 => "draft",
                300 | 350 => "print",
                _ => "line art",
            };
            ui.selectable_value(dpi, value, format!("{value} dpi  ({hint})"));
        }
    });
    ui.add(egui::DragValue::new(dpi).range(72..=1200).suffix(" dpi"));
    *dpi != before
}

/// The paper size list. Returns the size picked, in `orientation`.
pub(crate) fn paper_combo(ui: &mut egui::Ui, id: &str, size_mm: [f64; 2]) -> Option<[f64; 2]> {
    let current = PaperSize::matching(size_mm);
    let orientation = Orientation::of(size_mm);
    let mut picked = None;
    egui::ComboBox::from_id_salt(id).width(110.0).selected_text(current.map_or("Custom", |p| p.name)).show_ui(ui, |ui| {
        for paper in PAPER_SIZES {
            let label = format!("{}   {:.0} × {:.0} mm", paper.name, paper.mm[0], paper.mm[1]);
            if ui.selectable_label(current == Some(paper), label).clicked() {
                picked = Some(paper.oriented(orientation));
            }
        }
    });
    picked
}

/// Portrait / landscape buttons; turns `size` when one is clicked.
pub(crate) fn orientation_buttons(ui: &mut egui::Ui, size: &mut [f64; 2]) -> bool {
    let current = Orientation::of(*size);
    let mut changed = false;
    for (o, label) in [(Orientation::Portrait, "Portrait"), (Orientation::Landscape, "Landscape")] {
        if ui.selectable_label(current == o && size[0] != size[1], label).clicked() && current != o {
            *size = o.apply(*size);
            changed = true;
        }
    }
    changed
}

/// Screen sizes, in pixels.
const SCREEN_PRESETS: [(&str, u32, u32); 3] = [("HD", 1920, 1080), ("4K", 3840, 2160), ("Square 2K", 2048, 2048)];

/// The New Canvas dialog's settings; the last ones are offered next time.
#[derive(Clone)]
pub(crate) struct NewCanvas {
    /// Finished size (trim), millimetres.
    size_mm: [f64; 2],
    dpi: u32,
    unit: Unit,
    /// Bleed past the trim and a safe area inside it, shown as guides.
    print: bool,
    bleed_mm: f64,
    safe_mm: f64,
}

impl NewCanvas {
    /// Starts from a pixel size at a resolution.
    pub(crate) fn from_px(width: u32, height: u32, dpi: u32) -> Self {
        Self {
            size_mm: [px_to_mm(width as f64, dpi), px_to_mm(height as f64, dpi)],
            dpi,
            unit: Unit::Px,
            print: false,
            bleed_mm: STANDARD_BLEED_MM,
            safe_mm: STANDARD_SAFE_MM,
        }
    }

    /// The print page, when print guides are on and the margins fit.
    fn preset(&self) -> Option<PagePreset> {
        if !self.print {
            return None;
        }
        PagePreset::custom("Print page", self.size_mm, self.bleed_mm, self.safe_mm, self.dpi, Binding::LeftToRight, [3.0, 4.0])
    }

    fn canvas_px(&self) -> (u32, u32) {
        match self.preset() {
            Some(p) => p.geometry().canvas,
            None => (mm_to_whole_px(self.size_mm[0], self.dpi), mm_to_whole_px(self.size_mm[1], self.dpi)),
        }
    }
}

impl PainterApp {
    pub(crate) fn open_new_canvas(&mut self) {
        self.new_canvas = Some(self.new_canvas_last.clone().unwrap_or_else(|| NewCanvas::from_px(self.doc.width, self.doc.height, self.doc.dpi)));
    }

    pub(crate) fn new_canvas_window(&mut self, ctx: &egui::Context) {
        let Some(mut s) = self.new_canvas.take() else {
            return;
        };
        let (mut open, mut create, mut cancel) = (true, false, false);
        egui::Window::new("New Canvas")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(330.0);
                        egui::Grid::new("new_canvas_grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                            ui.label("Paper");
                            ui.horizontal(|ui| {
                                if let Some(size) = paper_combo(ui, "new_canvas_paper", s.size_mm) {
                                    s.size_mm = size;
                                }
                                orientation_buttons(ui, &mut s.size_mm);
                            });
                            ui.end_row();
                            ui.label("Width");
                            ui.horizontal(|ui| {
                                length_edit(
                                    ui,
                                    &mut s.size_mm[0],
                                    s.unit,
                                    s.dpi,
                                    1.0..=px_to_mm(MAX_CANVAS_SIZE as f64, s.dpi).max(1.0),
                                );
                                unit_combo(ui, "new_canvas_unit", &mut s.unit);
                            });
                            ui.end_row();
                            ui.label("Height");
                            length_edit(ui, &mut s.size_mm[1], s.unit, s.dpi, 1.0..=px_to_mm(MAX_CANVAS_SIZE as f64, s.dpi).max(1.0));
                            ui.end_row();
                            ui.label("Resolution");
                            ui.horizontal(|ui| {
                                let before = s.dpi;
                                if dpi_edit(ui, "new_canvas_dpi", &mut s.dpi) && s.unit == Unit::Px {
                                    // Typing in pixels: the pixels stay, the physical size follows.
                                    s.size_mm = s.size_mm.map(|mm| px_to_mm(Unit::Px.from_mm(mm, before).round(), s.dpi));
                                }
                            });
                            ui.end_row();
                            ui.label("Screen");
                            ui.horizontal(|ui| {
                                for (label, w, h) in SCREEN_PRESETS {
                                    if ui.small_button(label).clicked() {
                                        s.unit = Unit::Px;
                                        s.print = false;
                                        s.size_mm = [px_to_mm(w as f64, s.dpi), px_to_mm(h as f64, s.dpi)];
                                    }
                                }
                            });
                            ui.end_row();
                        });
                        ui.add_space(4.0);
                        ui.checkbox(&mut s.print, "Print bleed and safe area").on_hover_text(
                            "The canvas grows by the bleed on every side; trim, safe area and crop marks are drawn as guides",
                        );
                        ui.add_enabled_ui(s.print, |ui| {
                            egui::Grid::new("new_canvas_print").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                                ui.label("Bleed");
                                length_edit(ui, &mut s.bleed_mm, s.unit, s.dpi, 0.0..=20.0);
                                ui.end_row();
                                ui.label("Safe margin");
                                length_edit(ui, &mut s.safe_mm, s.unit, s.dpi, 0.0..=60.0);
                                ui.end_row();
                            });
                        });
                    });
                    // Preview.
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(150.0, 190.0), Sense::hover());
                    ui.painter().rect_filled(rect, 4.0, theme::well());
                    let geometry = s.preset().map(|p| p.geometry()).unwrap_or_else(|| {
                        let (w, h) = s.canvas_px();
                        let full = [0.0, 0.0, w as f64, h as f64];
                        comic_layout::PageGeometry {
                            canvas: (w, h),
                            bleed: full,
                            trim: full,
                            inner: full,
                            binding: Binding::LeftToRight,
                            webtoon: None,
                        }
                    });
                    draw_page(ui.painter(), rect.shrink(10.0), &geometry, &[]);
                });
                ui.separator();
                let (w, h) = s.canvas_px();
                let fits = w <= MAX_CANVAS_SIZE && h <= MAX_CANVAS_SIZE;
                let valid = !s.print || s.preset().is_some();
                // The other unit, for reference.
                let size = if s.unit == Unit::Px {
                    let mm = [px_to_mm(w as f64, s.dpi), px_to_mm(h as f64, s.dpi)];
                    format!("{w} × {h} px  ·  {:.1} × {:.1} mm at {} dpi", mm[0], mm[1], s.dpi)
                } else {
                    format!("{w} × {h} px at {} dpi", s.dpi)
                };
                ui.label(size);
                if !fits {
                    ui.colored_label(
                        Color32::from_rgb(240, 120, 100),
                        format!("Too big: the canvas limit is {MAX_CANVAS_SIZE} px. Lower the resolution."),
                    );
                } else if !valid {
                    ui.colored_label(Color32::from_rgb(240, 120, 100), "The safe margin leaves no page.");
                }
                ui.horizontal(|ui| {
                    create = ui.add_enabled(fits && valid, egui::Button::new("Create")).clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if create {
            let (w, h) = s.canvas_px();
            let preset = s.preset();
            let mut doc = Document::new(w, h);
            doc.dpi = s.dpi;
            self.set_document(doc, None);
            // Print guides: a comic page without panels draws trim, safe area and crop marks.
            if let Some(preset) = preset {
                self.comic = Some(ComicPage::new(preset.geometry(), preset.gutter()));
            }
            self.status = format!("New canvas {w} × {h} px at {} dpi", s.dpi);
            self.new_canvas_last = Some(s);
        } else if !cancel && open {
            self.new_canvas = Some(s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_size_follows_paper_and_bleed() {
        let mut s = NewCanvas::from_px(2480, 3508, 300);
        assert_eq!(s.canvas_px(), (2480, 3508));
        assert_eq!(PaperSize::matching(s.size_mm).map(|p| p.name), Some("A4"));
        s.size_mm = PaperSize::get("A4").unwrap().mm;
        s.dpi = 600;
        assert_eq!(s.canvas_px(), (4961, 7016));
        s.print = true;
        // 3 mm bleed on each side.
        assert_eq!(s.canvas_px(), (mm_to_whole_px(216.0, 600), mm_to_whole_px(303.0, 600)));
        s.safe_mm = 200.0;
        assert!(s.preset().is_none());
    }
}
