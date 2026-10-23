//! Actions (Window → Panels → Actions), like Photoshop's actions and Clip
//! Studio's auto actions: record the menu and palette commands you run, play
//! them back in one go. Each action is also a command ("Play Action: …"), so
//! it can go on the toolbar or be found in the palette. Commands that open a
//! dialog open it when played. Saved in `actions.json` with the settings.

use std::path::PathBuf;

use eframe::egui::{self, RichText};
use serde::{Deserialize, Serialize};

use crate::app::PainterApp;
use crate::icons::Icon;
use crate::theme;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Action {
    pub name: String,
    /// Command ids, in order; unticked steps are skipped.
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Step {
    pub command: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

#[derive(Default)]
pub(crate) struct Actions {
    pub list: Vec<Action>,
    /// The action being recorded into.
    pub recording: Option<usize>,
    /// The action picked in the panel.
    pub selected: Option<usize>,
    renaming: Option<usize>,
}

fn actions_path() -> Option<PathBuf> {
    crate::files::app_data_dir().map(|d| d.join("actions.json"))
}

/// Commands not worth recording: playing actions, panels, quitting, help.
fn recordable(id: &str) -> bool {
    !["action.", "editor.", "help.", "workspace.", "theme.", "file.quit", "view.command_palette"].iter().any(|p| id.starts_with(p))
}

impl Actions {
    pub fn load() -> Self {
        let list: Vec<Action> =
            actions_path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { list, ..Default::default() }
    }

    fn save(&self) {
        if let (Some(path), Ok(json)) = (actions_path(), serde_json::to_vec_pretty(&self.list)) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path, json);
        }
    }

    /// `run_command` calls this: while recording, `id` becomes a step.
    pub fn record(&mut self, id: &str) {
        let Some(i) = self.recording else { return };
        if recordable(id) {
            if let Some(a) = self.list.get_mut(i) {
                a.steps.push(Step { command: id.to_string(), enabled: true });
                self.save();
            }
        }
    }
}

impl PainterApp {
    /// Runs action `i`'s steps (not while recording it).
    pub(crate) fn play_action(&mut self, i: usize) {
        if self.actions.recording == Some(i) {
            self.status = "Stop recording before playing this action".into();
            return;
        }
        let Some(action) = self.actions.list.get(i).cloned() else { return };
        let recording = self.actions.recording.take();
        let mut ran = 0;
        for step in action.steps.iter().filter(|s| s.enabled) {
            if self.ws.commands.get(&step.command).is_none() && !step.command.contains('.') {
                continue;
            }
            self.run_command(&step.command);
            ran += 1;
        }
        self.actions.recording = recording;
        self.status = format!("Played “{}” ({ran} steps)", action.name);
    }

    /// A command's name for the panel.
    fn step_label(&self, id: &str) -> String {
        self.ws.commands.get(id).map_or_else(|| id.to_string(), |c| c.label.clone())
    }

    pub(crate) fn actions_panel(&mut self, ui: &mut egui::Ui) {
        let recording = self.actions.recording;
        ui.horizontal(|ui| {
            let rec = recording.is_some();
            let label = if rec { RichText::new("■ Stop").color(theme::accent()) } else { RichText::new("● Record") };
            let can_record = rec || self.actions.selected.is_some();
            let r = ui.add_enabled(can_record, egui::Button::new(label));
            if r.on_hover_text("Record the commands you run into the selected action").clicked() {
                self.actions.recording = if rec { None } else { self.actions.selected };
                self.actions.save();
            }
            let can_play = self.actions.selected.is_some() && !rec;
            if ui.add_enabled(can_play, egui::Button::new("▶ Play")).clicked() {
                if let Some(i) = self.actions.selected {
                    self.play_action(i);
                }
            }
            if ui.button("New").on_hover_text("A new action, recording straight away").clicked() {
                let n = self.actions.list.len() + 1;
                self.actions.list.push(Action { name: format!("Action {n}"), steps: Vec::new() });
                let i = self.actions.list.len() - 1;
                self.actions.selected = Some(i);
                self.actions.recording = Some(i);
                self.actions.renaming = Some(i);
                self.actions.save();
            }
            if ui.add_enabled(self.actions.selected.is_some() && !rec, egui::Button::new("Delete")).clicked() {
                if let Some(i) = self.actions.selected.take() {
                    self.actions.list.remove(i);
                    self.actions.save();
                }
            }
        });
        if let Some(i) = recording {
            let name = self.actions.list.get(i).map_or("", |a| a.name.as_str());
            ui.label(
                RichText::new(format!("Recording into “{name}”: run commands from the menus or the palette"))
                    .small()
                    .color(theme::accent()),
            );
        }
        ui.separator();
        if self.actions.list.is_empty() {
            ui.label(
                RichText::new("No actions yet. Press New, run the commands to repeat, then Stop.").small().color(theme::text_dim()),
            );
        }
        let mut changed = false;
        let mut remove_step = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for i in 0..self.actions.list.len() {
                let selected = self.actions.selected == Some(i);
                ui.horizontal(|ui| {
                    if self.actions.renaming == Some(i) {
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut self.actions.list[i].name).desired_width(ui.available_width() - 30.0),
                        );
                        if r.lost_focus() {
                            self.actions.renaming = None;
                            changed = true;
                        } else {
                            r.request_focus();
                        }
                    } else {
                        let a = &self.actions.list[i];
                        let text = format!("{}  ({})", a.name, a.steps.len());
                        let r = ui.add_sized([ui.available_width() - 26.0, 20.0], egui::SelectableLabel::new(selected, text));
                        if r.double_clicked() {
                            self.actions.renaming = Some(i);
                        } else if r.clicked() {
                            self.actions.selected = Some(i);
                        }
                    }
                    let play = ui.add(egui::ImageButton::new(Icon::Redo.image(14.0, theme::text_dim())).frame(false));
                    if play.on_hover_text("Play").clicked() {
                        self.play_action(i);
                    }
                });
                if selected {
                    let steps: Vec<(String, bool)> =
                        self.actions.list[i].steps.iter().map(|s| (self.step_label(&s.command), s.enabled)).collect();
                    ui.indent(("action-steps", i), |ui| {
                        for (k, (label, mut enabled)) in steps.into_iter().enumerate() {
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut enabled, label).changed() {
                                    self.actions.list[i].steps[k].enabled = enabled;
                                    changed = true;
                                }
                                if ui.add(egui::ImageButton::new(Icon::Remove.image(12.0, theme::text_dim())).frame(false)).clicked() {
                                    remove_step = Some((i, k));
                                }
                            });
                        }
                        if self.actions.list[i].steps.is_empty() {
                            ui.label(RichText::new("No steps yet: press Record").small().color(theme::text_dim()));
                        }
                    });
                }
            }
        });
        if let Some((i, k)) = remove_step {
            self.actions.list[i].steps.remove(k);
            changed = true;
        }
        if changed {
            self.actions.save();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_commands_that_change_the_picture_are_recorded() {
        let kept: Vec<&str> = ["layer.duplicate", "editor.3", "action.0", "filter.2", "help.updates", "file.quit"]
            .into_iter()
            .filter(|id| recordable(id))
            .collect();
        assert_eq!(kept, ["layer.duplicate", "filter.2"]);
        // Not recording: nothing is added (and nothing written).
        let mut a = Actions { list: vec![Action::default()], ..Default::default() };
        a.record("layer.duplicate");
        assert!(a.list[0].steps.is_empty());
    }
}
