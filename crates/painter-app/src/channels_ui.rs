//! The Channels panel (Window → Panels → Channels), as in Photoshop:
//! - RGB, Red, Green, Blue: the eye shows or hides a channel on the canvas
//!   (one alone shows as grey); clicking a channel makes painting, fills and
//!   stamps change only that one (Shift-click adds or removes channels);
//!   RGB goes back to all of them;
//! - saved selections (alpha channels): save the selection under a name, load
//!   it back (replacing, adding or subtracting), rename, delete. They're saved
//!   with the project.

use eframe::egui::{self, Color32, RichText};
use painter_core::{selection, SavedChannel, SelectionOp, TileCoord, TileStore};
use painter_render::Plane;

use crate::app::PainterApp;
use crate::icons::Icon;
use crate::theme;

pub(crate) struct ChannelState {
    /// Red, green, blue shown on the canvas.
    pub visible: [bool; 3],
    /// Red, green, blue that edits change.
    pub edit: [bool; 3],
    /// The saved channel whose name is being edited.
    renaming: Option<usize>,
}

impl Default for ChannelState {
    fn default() -> Self {
        Self { visible: [true; 3], edit: [true; 3], renaming: None }
    }
}

const NAMES: [&str; 3] = ["Red", "Green", "Blue"];
const SWATCHES: [Color32; 3] = [Color32::from_rgb(220, 60, 60), Color32::from_rgb(60, 180, 80), Color32::from_rgb(70, 110, 230)];

impl PainterApp {
    /// After an edit of `coords` on `plane`: the colour channels not being
    /// edited go back to `before`.
    pub(crate) fn restrict_channels(&mut self, plane: Plane, before: &TileStore, coords: impl IntoIterator<Item = TileCoord>) {
        let edit = self.channels.edit;
        if edit == [true; 3] || !matches!(plane, Plane::Color(_)) {
            return;
        }
        if let Some(tiles) = self.plane_tiles_mut(plane) {
            painter_core::channels::keep_channels(tiles, before, coords, edit);
        }
    }

    /// Selection → saved channel ("Alpha 1", "Alpha 2"…).
    pub(crate) fn save_selection_channel(&mut self) {
        let Some(mask) = self.doc.selection.clone() else {
            self.status = "Save Selection: nothing is selected".into();
            return;
        };
        let n = (1..).find(|n| !self.doc.channels.iter().any(|c| c.name == format!("Alpha {n}"))).unwrap_or(1);
        self.next_label = Some("Save Selection".into());
        self.checkpoint();
        self.doc.channels.push(SavedChannel { name: format!("Alpha {n}"), mask });
        self.status = format!("Selection saved as “Alpha {n}”");
    }

    /// Saved channel `i` → the selection, combined by `op`.
    pub(crate) fn load_selection_channel(&mut self, i: usize, op: SelectionOp) {
        let Some(mask) = self.doc.channels.get(i).map(|c| c.mask.clone()) else { return };
        self.combine_selection(mask, op);
    }

    /// The visible image's red, green or blue → the selection (bright = selected).
    pub(crate) fn load_colour_channel(&mut self, c: usize, op: SelectionOp) {
        let (w, h) = self.doc.canvas();
        let rgba = painter_core::io::flatten(&self.doc);
        let mask = selection::mask_from_fn((w, h), [0, 0, w as i32 - 1, h as i32 - 1], |x, y| {
            let px = &rgba[((y as u32 * w + x as u32) * 4) as usize..][..4];
            // Over white, like the canvas shows it.
            (px[c] as f32 + 255.0 - px[3] as f32) / 255.0
        });
        self.combine_selection(mask, op);
    }

    pub(crate) fn channels_panel(&mut self, ui: &mut egui::Ui) {
        let all = self.channels.visible == [true; 3] && self.channels.edit == [true; 3];
        let shift = ui.input(|i| i.modifiers.shift);
        let ctrl = ui.input(|i| i.modifiers.command);
        // RGB: everything back.
        ui.horizontal(|ui| {
            let mut v = self.channels.visible == [true; 3];
            if eye(ui, &mut v) {
                self.channels.visible = [v; 3];
            }
            let r = ui.add_sized([ui.available_width(), 22.0], egui::SelectableLabel::new(all, "RGB"));
            if r.on_hover_text("Show and edit every channel").clicked() {
                self.channels.visible = [true; 3];
                self.channels.edit = [true; 3];
            }
        });
        for c in 0..3 {
            ui.horizontal(|ui| {
                if eye(ui, &mut self.channels.visible[c]) && !self.channels.visible.contains(&true) {
                    // Hiding the last one shows them all again, as nothing would be left.
                    self.channels.visible = [true; 3];
                }
                let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().rect_filled(dot, 2.0, SWATCHES[c]);
                let selected = self.channels.edit[c] && !all;
                let label = format!("{}   Ctrl+{}", NAMES[c], c + 3);
                let r = ui.add_sized([ui.available_width(), 22.0], egui::SelectableLabel::new(selected, label));
                let r = r.on_hover_text(
                    "Click: edit and show only this channel. Shift-click: add or remove it. Ctrl-click: load it as a selection",
                );
                if r.clicked() {
                    if ctrl {
                        self.load_colour_channel(c, SelectionOp::Replace);
                    } else if shift && !all {
                        self.channels.edit[c] = !self.channels.edit[c];
                        if !self.channels.edit.contains(&true) {
                            self.channels.edit = [true; 3];
                        }
                        self.channels.visible = self.channels.edit;
                    } else {
                        self.channels.edit = [false; 3];
                        self.channels.edit[c] = true;
                        self.channels.visible = self.channels.edit;
                    }
                }
            });
        }
        if !all && self.channels.edit != [true; 3] {
            let names: Vec<&str> = (0..3).filter(|&c| self.channels.edit[c]).map(|c| NAMES[c]).collect();
            ui.label(RichText::new(format!("Painting changes only {}", names.join(" and "))).small().color(theme::accent()));
        }
        ui.separator();
        ui.label(RichText::new("Saved selections").small().color(theme::text_dim()));
        let mut load = None;
        let mut delete = None;
        let count = self.doc.channels.len();
        for i in 0..count {
            ui.horizontal(|ui| {
                ui.add(Icon::LayerMask.image(16.0, theme::text_dim()));
                if self.channels.renaming == Some(i) {
                    let r =
                        ui.add(egui::TextEdit::singleline(&mut self.doc.channels[i].name).desired_width(ui.available_width() - 60.0));
                    if r.lost_focus() {
                        self.channels.renaming = None;
                    } else {
                        r.request_focus();
                    }
                } else {
                    let name = self.doc.channels[i].name.clone();
                    let r = ui.add(egui::SelectableLabel::new(false, name));
                    let r = r.on_hover_text("Click: load as the selection (Shift adds, Alt subtracts). Double-click: rename");
                    if r.double_clicked() {
                        self.channels.renaming = Some(i);
                    } else if r.clicked() {
                        let m = ui.input(|i| i.modifiers);
                        load = Some((
                            i,
                            match (m.shift, m.alt) {
                                (true, true) => SelectionOp::Intersect,
                                (true, false) => SelectionOp::Add,
                                (false, true) => SelectionOp::Subtract,
                                _ => SelectionOp::Replace,
                            },
                        ));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(egui::ImageButton::new(Icon::Delete.image(14.0, theme::text_dim())).frame(false))
                        .on_hover_text("Delete")
                        .clicked()
                    {
                        delete = Some(i);
                    }
                });
            });
        }
        if count == 0 {
            ui.label(RichText::new("None yet: make a selection, then Save Selection.").small().color(theme::text_dim()));
        }
        if let Some((i, op)) = load {
            self.load_selection_channel(i, op);
        }
        if let Some(i) = delete {
            self.next_label = Some("Delete Channel".into());
            self.checkpoint();
            self.doc.channels.remove(i);
        }
        ui.add_enabled_ui(self.doc.selection.is_some(), |ui| {
            if ui.button("Save Selection").on_hover_text("Keep the selection as a channel, saved with the project").clicked() {
                self.save_selection_channel();
            }
        });
    }

    /// Ctrl+2 shows every channel, Ctrl+3 / 4 / 5 red, green or blue alone (as in Photoshop).
    pub(crate) fn channel_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        let pressed = |key| ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, key));
        if pressed(egui::Key::Num2) {
            self.channels.visible = [true; 3];
            self.channels.edit = [true; 3];
        }
        for (c, key) in [egui::Key::Num3, egui::Key::Num4, egui::Key::Num5].into_iter().enumerate() {
            if pressed(key) {
                self.channels.edit = [false; 3];
                self.channels.edit[c] = true;
                self.channels.visible = self.channels.edit;
            }
        }
    }
}

/// An eye toggle; true when clicked.
fn eye(ui: &mut egui::Ui, on: &mut bool) -> bool {
    let tint = if *on { theme::text() } else { theme::text_dim().gamma_multiply(0.4) };
    let r = ui.add(egui::ImageButton::new(Icon::Visible.image(16.0, tint)).frame(false));
    if r.clicked() {
        *on = !*on;
        return true;
    }
    false
}
