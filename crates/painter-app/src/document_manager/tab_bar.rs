//! The document tabs above the areas: name, `*` for unsaved changes, a
//! close button, documents still loading, and how much memory the open
//! documents take. Ctrl+W closes, Ctrl+Tab / Ctrl+Shift+Tab cycle. Layers
//! dragged from the Layers panel onto a tab are copied into that document.

use eframe::egui::{self, Align, Align2, Key, Layout, Modifiers};

use super::layer_transfer::LayerDrag;
use super::session_store::ClosePrompt;
use crate::app::PainterApp;
use crate::theme;

/// Above this, the tab bar warns that open documents use a lot of memory.
const MEMORY_WARNING: usize = 6 << 30;

impl PainterApp {
    /// Tab shortcuts; runs before the other shortcuts so Ctrl+Tab isn't taken
    /// for focus moves or zen mode.
    pub(crate) fn document_shortcuts(&mut self, ctx: &egui::Context) {
        // Not text-editing keys, so they work whatever has keyboard focus.
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::W)) {
            let id = self.docs.active_id();
            self.request_close_tab(id);
        }
        let step = std::mem::take(&mut self.docs.pending_cycle);
        if step != 0 {
            self.cycle_document(step.signum());
        }
    }

    /// Takes Ctrl+Tab / Ctrl+Shift+Tab out of the input before egui sees it,
    /// so it cycles documents instead of moving keyboard focus.
    pub(crate) fn intercept_document_keys(&mut self, raw: &mut egui::RawInput) {
        raw.events.retain(|e| match e {
            egui::Event::Key { key: Key::Tab, pressed, modifiers, .. } if modifiers.command || modifiers.ctrl => {
                if *pressed {
                    self.docs.pending_cycle += if modifiers.shift { -1 } else { 1 };
                }
                false
            }
            _ => true,
        });
    }

    /// The name a tab shows.
    pub(crate) fn tab_title(&self, i: usize) -> String {
        let slot = &self.docs.slots[i];
        let path = match &slot.parked {
            Some(s) => s.file_path.as_ref(),
            None => self.file_path.as_ref(),
        };
        match path.and_then(|p| p.file_name()) {
            Some(n) => n.to_string_lossy().into_owned(),
            // Imported images keep their file name until saved as a project.
            None => slot.name.clone().unwrap_or_else(|| format!("Untitled {}", slot.untitled)),
        }
    }

    pub(crate) fn document_tabs_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("document_tabs")
            .frame(egui::Frame::new().fill(theme::title()).inner_margin(egui::Margin::symmetric(6, 2)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let mut switch = None;
                    let mut close = None;
                    let mut menu: Option<(usize, &str)> = None;
                    let mut dropped: Option<(usize, LayerDrag)> = None;
                    for i in 0..self.docs.slots.len() {
                        let id = self.docs.slots[i].id;
                        let active = i == self.docs.active;
                        let dirty = self.slot_dirty(i);
                        let title = format!("{}{}", self.tab_title(i), if dirty { " *" } else { "" });
                        let tab = ui.selectable_label(active, title);
                        let tab = match &self.docs.slots[i].parked {
                            Some(s) => tab.on_hover_text(format!(
                                "{} × {} px, {} dpi{}",
                                s.doc.width,
                                s.doc.height,
                                s.doc.dpi,
                                s.file_path.as_ref().map_or(String::new(), |p| format!("\n{}", p.display()))
                            )),
                            None => tab,
                        };
                        // A layer dragged from the Layers panel.
                        if !active {
                            if tab.dnd_hover_payload::<LayerDrag>().is_some() {
                                ui.painter().rect_stroke(
                                    tab.rect,
                                    3.0,
                                    egui::Stroke::new(2.0_f32, theme::accent()),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            if let Some(payload) = tab.dnd_release_payload::<LayerDrag>() {
                                dropped = Some((i, *payload));
                            }
                        }
                        if tab.clicked() {
                            switch = Some(i);
                        }
                        if tab.middle_clicked() {
                            close = Some(id);
                        }
                        tab.context_menu(|ui| {
                            let others = !active;
                            for (label, what, enabled) in [
                                ("Show in Reference View", "reference", others),
                                ("Copy Active Layer Here", "layer", others),
                                ("Copy Selection Here", "selection", others && self.doc.selection.is_some()),
                                ("Close", "close", true),
                                ("Close Others", "others", self.docs.slots.len() > 1),
                            ] {
                                if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                                    menu = Some((i, what));
                                    ui.close_menu();
                                }
                            }
                        });
                        let x = ui.add(egui::Button::new(egui::RichText::new("x").small()).frame(false));
                        if x.on_hover_text("Close (Ctrl+W)").clicked() {
                            close = Some(id);
                        }
                        ui.add_space(6.0);
                    }
                    for path in self.docs.loading() {
                        ui.spinner();
                        ui.weak(format!("Opening {}...", file_name(path)));
                    }
                    if let Some(i) = switch {
                        self.switch_document(i);
                    }
                    if let Some((i, payload)) = dropped {
                        self.transfer_layer_to(i, payload.layer);
                    }
                    if let Some((i, what)) = menu {
                        let id = self.docs.slots[i].id;
                        match what {
                            "reference" => self.show_in_reference(id),
                            "layer" => {
                                let layer = self.doc.active;
                                self.transfer_layer_to(i, layer);
                            }
                            "selection" => self.transfer_selection_to(i),
                            "close" => self.request_close_tab(id),
                            "others" => {
                                let keep = id;
                                let ids: Vec<u64> = self.docs.slots.iter().map(|s| s.id).filter(|x| *x != keep).collect();
                                for other in ids {
                                    let dirty = self.docs.index_of(other).is_some_and(|k| self.slot_dirty(k));
                                    if !dirty {
                                        self.close_tab_now(other);
                                    }
                                }
                                if self.docs.slots.len() > 1 {
                                    self.status = "Tabs with unsaved changes were kept open".into();
                                }
                            }
                            _ => {}
                        }
                    }
                    if let Some(id) = close {
                        self.request_close_tab(id);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let bytes = self.documents_memory();
                        let text = format!("{} open · {:.1} GB", self.docs.slots.len(), bytes as f64 / (1u64 << 30) as f64);
                        if bytes > MEMORY_WARNING {
                            ui.colored_label(theme::warning(), text).on_hover_text(
                                "The open documents take a lot of memory. Close the ones you don't need; \
                                 documents in other tabs keep their pixels in RAM but not on the GPU.",
                            );
                        } else {
                            ui.weak(text).on_hover_text("Memory used by the pixels of all open documents");
                        }
                    });
                });
            });
    }

    /// The "unsaved changes" questions: closing a tab, or quitting.
    pub(crate) fn close_prompts(&mut self, ctx: &egui::Context) {
        // Quitting: with any unsaved document, ask instead.
        let quitting = ctx.input(|i| i.viewport().close_requested()) && !self.allow_close;
        if quitting && (self.dirty_count() > 0 || self.docs.saving()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.dirty_count() > 0 {
                self.docs.prompt = Some(ClosePrompt::Quit);
            } else {
                self.docs.quit_after_saves = true;
                self.status = "Finishing saves before closing...".into();
            }
        }
        let Some(prompt) = self.docs.prompt else { return };
        let (title, message, names) = match prompt {
            ClosePrompt::Tab(id) => {
                let Some(i) = self.docs.index_of(id) else {
                    self.docs.prompt = None;
                    return;
                };
                ("Unsaved changes", format!("Save changes to {} before closing it?", self.tab_title(i)), Vec::new())
            }
            ClosePrompt::Quit => {
                let names: Vec<String> =
                    (0..self.docs.slots.len()).filter(|&i| self.slot_dirty(i)).map(|i| self.tab_title(i)).collect();
                ("Unsaved changes", format!("{} document(s) have unsaved changes:", names.len()), names)
            }
        };
        let mut choice = None;
        egui::Window::new(title).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
            ui.label(message);
            for name in &names {
                ui.label(format!("  • {name}"));
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let save = if names.len() > 1 { "Save All" } else { "Save" };
                if ui.button(save).clicked() {
                    choice = Some(0);
                }
                if ui.button("Don't Save").clicked() {
                    choice = Some(1);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(2);
                }
            });
        });
        match (prompt, choice) {
            (ClosePrompt::Tab(id), Some(0)) => {
                if let Some(i) = self.docs.index_of(id) {
                    self.switch_document(i);
                    self.save();
                    // Saving works on a copy, so the tab can go at once.
                    if !self.dirty {
                        self.close_tab_now(id);
                    }
                }
                self.docs.prompt = None;
            }
            (ClosePrompt::Tab(id), Some(1)) => {
                self.close_tab_now(id);
                self.docs.prompt = None;
            }
            (ClosePrompt::Quit, Some(0)) => {
                self.docs.prompt = None;
                if self.save_all() {
                    self.docs.quit_after_saves = true;
                    self.status = "Saving, then closing...".into();
                }
            }
            (ClosePrompt::Quit, Some(1)) => {
                self.docs.prompt = None;
                self.allow_quit(ctx);
            }
            (_, Some(_)) => self.docs.prompt = None,
            _ => {}
        }
    }

    pub(crate) fn allow_quit(&mut self, ctx: &egui::Context) {
        self.allow_close = true;
        self.docs.quit_after_saves = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}
