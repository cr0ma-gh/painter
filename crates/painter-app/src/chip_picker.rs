//! Double-clicking the main or sub colour chip (toolbar, Color Wheel, Color
//! Slider) opens a picker for that colour beside it: wheel, hex and RGB.

use eframe::egui::{self, ecolor::Hsva, Color32, Pos2, Vec2};

use crate::app::PainterApp;

/// The open chip picker: which colour, and where it opened.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChipPicker {
    pub sub: bool,
    pub at: Pos2,
}

/// Whether a double-click at `p` on chips drawn in `rect` (at `size`) hit the sub
/// colour: outside the main disc, which overlaps the sub one.
fn hits_sub(rect: egui::Rect, size: f32, p: Pos2) -> bool {
    let r = size * 0.5;
    let main = rect.left_top() + Vec2::splat(r + 1.0);
    p.distance(main) > r
}

impl PainterApp {
    /// The main / sub chips: a click swaps them, a double-click opens the picker.
    pub(crate) fn color_chips(&mut self, ui: &mut egui::Ui, size: f32) {
        let response = crate::widgets::color_chips(ui, &mut self.color, &mut self.sub_color, size);
        if response.double_clicked() {
            // Both clicks swapped: the colours are back where they were.
            let sub = response.interact_pointer_pos().is_some_and(|p| hits_sub(response.rect, size, p));
            self.chip_picker = Some(ChipPicker { sub, at: response.rect.right_top() + Vec2::new(8.0, 0.0) });
        }
    }

    /// The chip picker window, while open.
    pub(crate) fn chip_picker_window(&mut self, ctx: &egui::Context) {
        let Some(picker) = self.chip_picker else { return };
        let mut open = true;
        let mut done = false;
        let title = if picker.sub { "Sub colour" } else { "Main colour" };
        egui::Window::new(title)
            .id(egui::Id::new("chip_picker"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos(picker.at)
            .show(ctx, |ui| {
                let color: &mut Hsva = if picker.sub { &mut self.sub_color } else { &mut self.color };
                crate::widgets::color_wheel(ui, color, 200.0);
                let [r, g, b, _] = Color32::from(*color).to_array();
                let mut rgb = [r, g, b];
                let mut changed = false;
                ui.horizontal(|ui| {
                    for (label, v) in ["R", "G", "B"].iter().zip(rgb.iter_mut()) {
                        ui.label(*label);
                        changed |= ui.add(egui::DragValue::new(v).range(0..=255)).changed();
                    }
                });
                let mut hex = format!("{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]);
                ui.horizontal(|ui| {
                    ui.label("#");
                    let edit = ui.add(egui::TextEdit::singleline(&mut hex).desired_width(70.0).font(egui::TextStyle::Monospace));
                    if edit.changed() {
                        if let Some(parsed) = parse_hex(&hex) {
                            rgb = parsed;
                            changed = true;
                        }
                    }
                    if ui.button("OK").clicked() {
                        done = true;
                    }
                });
                if changed {
                    *color = Hsva::from(Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
                }
            });
        if !open || done || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.chip_picker = None;
        }
    }
}

/// "RRGGBB" (with or without #) as bytes.
fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let t = text.trim().trim_start_matches('#');
    if t.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(t, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_which_chip() {
        assert_eq!(parse_hex("#FF8000"), Some([255, 128, 0]));
        assert_eq!(parse_hex("12345"), None);
        let rect = egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(24.0 * 1.45, 24.0 * 1.3));
        assert!(!hits_sub(rect, 24.0, Pos2::new(13.0, 13.0)));
        assert!(hits_sub(rect, 24.0, Pos2::new(32.0, 28.0)));
    }
}
