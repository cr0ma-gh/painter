//! The History panel (as in Clip Studio): every undo step of the document by
//! name, oldest at the top. Clicking a step goes back (or forward) to the
//! state after it; steps undone stay listed, dimmed, until something new is
//! done.

use eframe::egui::{self, RichText};

use crate::app::PainterApp;
use crate::theme;

impl PainterApp {
    pub(crate) fn history_panel(&mut self, ui: &mut egui::Ui) {
        self.history.name_last(&self.doc);
        let done: Vec<String> = self.history.done().map(str::to_string).collect();
        let undone: Vec<String> = self.history.undone().map(str::to_string).collect();
        // State k: after k steps (0 = as opened).
        let current = done.len();
        let mut target = None;
        ui.horizontal(|ui| {
            if ui.add_enabled(self.history.can_undo(), egui::Button::new("Undo")).clicked() {
                target = Some(current.saturating_sub(1));
            }
            if ui.add_enabled(self.history.can_redo(), egui::Button::new("Redo")).clicked() {
                target = Some(current + 1);
            }
            let mut limit = self.history.limit();
            let r = ui.add(egui::DragValue::new(&mut limit).range(10..=1000).speed(1.0).suffix(" steps"));
            if r.on_hover_text("How many steps are kept (the oldest go first)").changed() {
                self.history.set_limit(limit);
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            let row = |ui: &mut egui::Ui, k: usize, label: &str, dim: bool| -> bool {
                let text = RichText::new(label);
                let text = if dim { text.color(theme::text_dim()) } else { text };
                ui.add_sized([ui.available_width(), 20.0], egui::SelectableLabel::new(k == current, text)).clicked()
            };
            if row(ui, 0, "Opened", false) {
                target = Some(0);
            }
            for (i, label) in done.iter().chain(undone.iter()).enumerate() {
                let k = i + 1;
                if row(ui, k, label, k > current) {
                    target = Some(k);
                }
            }
        });
        if let Some(k) = target {
            self.go_to_history(k);
        }
    }

    /// Undoes or redoes until `k` steps are done.
    pub(crate) fn go_to_history(&mut self, k: usize) {
        let mut current = self.history.done().count();
        while current > k && self.history.can_undo() {
            self.undo();
            current -= 1;
        }
        while current < k && self.history.can_redo() {
            self.redo();
            current += 1;
        }
    }
}
