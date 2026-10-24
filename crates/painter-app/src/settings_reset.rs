//! Help → Reset Settings: Painter's settings back to how they come, chosen by
//! kind. Starting Painter with `--reset-settings` resets the interface and
//! the tool options before anything is shown (for when a setting gets in the
//! way, like an interface scaled too big to reach the menus).

use eframe::egui::{self, RichText};
use crate::i18n::tr;

use crate::app::PainterApp;
use crate::theme;

/// What to reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ResetChoice {
    /// Layout, panels, toolbars, theme, interface size, screen mode, welcome screen.
    pub interface: bool,
    /// Tool options (fill, selection, shapes, stamps…), channels, the tool in hand.
    pub tools: bool,
    /// Built-in brushes back to their defaults; imported and own brushes stay.
    pub brushes: bool,
    /// Colour sets and colour history.
    pub colors: bool,
    /// The recent files list.
    pub recent: bool,
    /// Update checks.
    pub updates: bool,
}

impl Default for ResetChoice {
    fn default() -> Self {
        Self { interface: true, tools: true, brushes: false, colors: false, recent: false, updates: false }
    }
}

impl PainterApp {
    pub(crate) fn reset_settings(&mut self, ctx: &egui::Context, choice: ResetChoice) {
        let mut done = Vec::new();
        if choice.interface {
            self.ws.reset();
            // Window positions, collapsed sections, scroll positions…
            ctx.memory_mut(|m| *m = Default::default());
            self.set_screen_mode(ctx, 0);
            done.push("interface");
        }
        if choice.tools {
            self.tools = Default::default();
            self.channels = Default::default();
            self.tool = crate::tools::Tool::Brush;
            done.push("tool options");
        }
        if choice.brushes {
            self.reset_builtin_brushes();
            done.push("brushes");
        }
        if choice.colors {
            self.colors.data = Default::default();
            if self.colors.data.sets.is_empty() {
                self.colors.data.sets.push(crate::color_panels::ColorSet::default_set());
            }
            done.push("colours");
        }
        if choice.recent {
            self.recent.clear();
            done.push("recent files");
        }
        if choice.updates {
            self.updater.reset_settings();
            done.push("update checks");
        }
        self.status = if done.is_empty() { "Nothing reset".into() } else { format!("Reset: {}", done.join(", ")) };
    }

    /// The Reset Settings window.
    pub(crate) fn reset_settings_window(&mut self, ctx: &egui::Context) {
        let Some(mut choice) = self.reset_dialog else { return };
        let mut open = true;
        let mut go = false;
        let mut cancel = false;
        egui::Window::new(tr("Reset Settings"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(tr("Put back as Painter comes:"));
                ui.add_space(4.0);
                ui.checkbox(&mut choice.interface, tr("Interface"))
                    .on_hover_text(tr("Workspaces and panels, toolbars, theme, interface size, screen mode, the welcome screen"));
                ui.checkbox(&mut choice.tools, tr("Tool options"))
                    .on_hover_text(tr("Fill, selection, shape, stamp and other tool settings; channels"));
                ui.checkbox(&mut choice.brushes, tr("Built-in brushes"))
                    .on_hover_text(tr("Their settings back to the defaults; brushes you imported or made stay"));
                ui.checkbox(&mut choice.colors, tr("Colour sets and colour history"));
                ui.checkbox(&mut choice.recent, tr("Recent files list"));
                ui.checkbox(&mut choice.updates, tr("Update checks"));
                ui.add_space(6.0);
                ui.label(
                    RichText::new(tr("Open documents, your files, actions and patterns aren't touched. This can't be undone."))
                        .small()
                        .color(theme::text_dim()),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button(tr("Reset")).clicked() {
                        go = true;
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        cancel = true;
                    }
                });
            });
        if go {
            self.reset_dialog = None;
            self.reset_settings(ctx, choice);
        } else if cancel || !open {
            self.reset_dialog = None;
        } else {
            self.reset_dialog = Some(choice);
        }
    }
}
