//! The Channels panel (Window → Panels → Channels), as in Photoshop. The
//! channels are the document's colour mode's:
//! - RGB: Red, Green, Blue; CMYK: Cyan, Magenta, Yellow, Black (shown from the
//!   press profile's inks: one alone as grey, white where there's no ink);
//!   Lab: Lightness, a, b; Multichannel: Cyan, Magenta, Yellow spot channels;
//!   Grayscale: Gray; Bitmap: Bitmap; Duotone: Duotone; Indexed Color: Index.
//! - The eye shows or hides a channel on the canvas; clicking a channel makes
//!   painting, fills and stamps change only that one (Shift-click adds or
//!   removes channels; Ctrl-click loads it as a selection); the composite row
//!   (RGB, CMYK) goes back to all of them.
//! - Saved selections (alpha channels): save the selection under a name, load
//!   it back (replacing, adding or subtracting), rename, delete. They're saved
//!   with the project.

use eframe::egui::{self, Color32, RichText};
use painter_core::color_mode::ColorMode;
use painter_core::{selection, SavedChannel, SelectionOp, TileCoord, TileStore};
use painter_render::Plane;

use crate::app::PainterApp;
use crate::icons::Icon;
use crate::theme;

pub(crate) struct ChannelState {
    /// The mode these channels are of; they reset when it changes.
    mode: ColorMode,
    /// The mode's channels shown on the canvas (the first `channel_count` count).
    pub visible: [bool; 4],
    /// The mode's channels that edits change.
    pub edit: [bool; 4],
    /// The saved channel whose name is being edited.
    renaming: Option<usize>,
}

impl Default for ChannelState {
    fn default() -> Self {
        Self { mode: ColorMode::Rgb, visible: [true; 4], edit: [true; 4], renaming: None }
    }
}

/// A mode's composite row (if it has one) and its channels with their swatches.
fn channels_of(mode: ColorMode) -> (Option<&'static str>, &'static [(&'static str, Color32)]) {
    const RGB: [(&str, Color32); 3] =
        [("Red", Color32::from_rgb(220, 60, 60)), ("Green", Color32::from_rgb(60, 180, 80)), ("Blue", Color32::from_rgb(70, 110, 230))];
    const CMYK: [(&str, Color32); 4] = [
        ("Cyan", Color32::from_rgb(0, 174, 239)),
        ("Magenta", Color32::from_rgb(236, 0, 140)),
        ("Yellow", Color32::from_rgb(255, 230, 0)),
        ("Black", Color32::from_rgb(20, 20, 20)),
    ];
    const GRAY: [(&str, Color32); 1] = [("Gray", Color32::from_rgb(150, 150, 150))];
    const BITMAP: [(&str, Color32); 1] = [("Bitmap", Color32::from_rgb(0, 0, 0))];
    const DUOTONE: [(&str, Color32); 1] = [("Duotone", Color32::from_rgb(150, 120, 90))];
    const INDEX: [(&str, Color32); 1] = [("Index", Color32::from_rgb(150, 150, 150))];
    const LAB: [(&str, Color32); 3] =
        [("Lightness", Color32::from_rgb(200, 200, 200)), ("a", Color32::from_rgb(200, 70, 120)), ("b", Color32::from_rgb(220, 190, 40))];
    // Spot channels, as Photoshop names them (no composite there).
    const MULTI: [(&str, Color32); 3] =
        [("Cyan", Color32::from_rgb(0, 174, 239)), ("Magenta", Color32::from_rgb(236, 0, 140)), ("Yellow", Color32::from_rgb(255, 230, 0))];
    match mode {
        ColorMode::Rgb => (Some("RGB"), &RGB),
        ColorMode::Cmyk => (Some("CMYK"), &CMYK),
        ColorMode::Lab => (Some("Lab"), &LAB),
        ColorMode::Multichannel => (None, &MULTI),
        ColorMode::Grayscale => (None, &GRAY),
        ColorMode::Bitmap => (None, &BITMAP),
        ColorMode::Duotone => (None, &DUOTONE),
        ColorMode::Indexed => (None, &INDEX),
    }
}

impl ChannelState {
    fn count(&self) -> usize {
        channels_of(self.mode).1.len()
    }

    /// Every channel shown and edited.
    fn all(&self) -> bool {
        let n = self.count();
        self.visible[..n].iter().all(|v| *v) && self.edit[..n].iter().all(|v| *v)
    }

    fn reset(&mut self) {
        self.visible = [true; 4];
        self.edit = [true; 4];
    }

    /// Only channel `c`, shown and edited.
    pub(crate) fn solo(&mut self, c: usize) {
        self.edit = [false; 4];
        self.edit[c] = true;
        self.visible = self.edit;
    }

    /// RGB channels for the canvas view (all of them in other modes).
    pub(crate) fn rgb_view(&self) -> [bool; 3] {
        if self.mode == ColorMode::Rgb { [self.visible[0], self.visible[1], self.visible[2]] } else { [true; 3] }
    }

    /// The inks to show in a CMYK document, when not all of them.
    pub(crate) fn ink_view(&self) -> Option<[bool; 4]> {
        (self.mode == ColorMode::Cmyk && self.visible != [true; 4]).then_some(self.visible)
    }

    /// The channels shown, for the display table of Lab and Multichannel documents.
    pub(crate) fn shown(&self) -> [bool; 4] {
        if matches!(self.mode, ColorMode::Lab | ColorMode::Multichannel) { self.visible } else { [true; 4] }
    }
}

impl PainterApp {
    /// The channels follow the document's colour mode. Every frame.
    pub(crate) fn sync_channels(&mut self) {
        if self.channels.mode != self.doc.mode {
            self.channels.mode = self.doc.mode;
            self.channels.reset();
        }
    }

    /// After an edit of `coords` on `plane`: the channels not being edited
    /// go back to `before` (RGB values, or CMYK inks through the press profile).
    pub(crate) fn restrict_channels(&mut self, plane: Plane, before: &TileStore, coords: impl IntoIterator<Item = TileCoord>) {
        if self.channels.all() || !matches!(plane, Plane::Color(_)) {
            return;
        }
        let edit = self.channels.edit;
        let coords: Vec<TileCoord> = coords.into_iter().collect();
        match self.doc.mode {
            ColorMode::Rgb => {
                if let Some(tiles) = self.plane_tiles_mut(plane) {
                    painter_core::channels::keep_channels(tiles, before, coords, [edit[0], edit[1], edit[2]]);
                }
            }
            ColorMode::Cmyk => {
                // Out while the tiles are borrowed; back below.
                let proofer = self.proofer.proofer.take();
                if let (Some(p), Some(tiles)) = (&proofer, self.plane_tiles_mut(plane)) {
                    p.keep_inks(tiles, before, &coords, edit);
                }
                self.proofer.proofer = proofer;
            }
            ColorMode::Lab => {
                if let Some(tiles) = self.plane_tiles_mut(plane) {
                    painter_core::lab::keep_channels(tiles, before, &coords, [edit[0], edit[1], edit[2]]);
                }
            }
            // Spot inks are the inverted red, green and blue.
            ColorMode::Multichannel => {
                if let Some(tiles) = self.plane_tiles_mut(plane) {
                    painter_core::channels::keep_channels(tiles, before, coords, [edit[0], edit[1], edit[2]]);
                }
            }
            ColorMode::Grayscale | ColorMode::Bitmap | ColorMode::Duotone | ColorMode::Indexed => {}
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

    /// Channel `c` of the visible image → the selection, as it shows in grey
    /// (bright = selected: much red, or little cyan ink…).
    pub(crate) fn load_colour_channel(&mut self, c: usize, op: SelectionOp) {
        let (w, h) = self.doc.canvas();
        let rgba = painter_core::io::flatten(&self.doc);
        // Over white, like the canvas shows it.
        let rgb: Vec<[u8; 3]> = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| {
                let white = 255 - p[3];
                [p[0].saturating_add(white), p[1].saturating_add(white), p[2].saturating_add(white)]
            })
            .collect();
        let grey: Vec<u8> = match (self.doc.mode, &self.proofer.proofer) {
            (ColorMode::Cmyk, Some(p)) => p.cmyk_many(&rgb).iter().map(|k| 255 - k[c.min(3)]).collect(),
            (ColorMode::Rgb | ColorMode::Multichannel, _) => rgb.iter().map(|p| p[c.min(2)]).collect(),
            (ColorMode::Lab, _) => rgb.iter().map(|p| painter_core::lab::channel_byte(painter_core::lab::from_rgb(*p), c)).collect(),
            _ => rgb.iter().map(|p| painter_core::color_mode::luma(*p)).collect(),
        };
        let mask = selection::mask_from_fn((w, h), [0, 0, w as i32 - 1, h as i32 - 1], |x, y| grey[(y as u32 * w + x as u32) as usize] as f32 / 255.0);
        self.combine_selection(mask, op);
    }

    pub(crate) fn channels_panel(&mut self, ui: &mut egui::Ui) {
        self.sync_channels();
        let (composite, list) = channels_of(self.doc.mode);
        let n = list.len();
        let all = self.channels.all();
        let shift = ui.input(|i| i.modifiers.shift);
        let ctrl = ui.input(|i| i.modifiers.command);
        if self.doc.mode == ColorMode::Cmyk && self.proofer.proofer.is_none() {
            ui.label(RichText::new("The press profile couldn't be used: inks can't be shown").small().color(theme::warning()));
        }
        // The composite (RGB, CMYK): everything back.
        if let Some(name) = composite {
            ui.horizontal(|ui| {
                let mut v = self.channels.visible[..n].iter().all(|v| *v);
                if eye(ui, &mut v) {
                    self.channels.visible = [true; 4];
                }
                let r = ui.add_sized([ui.available_width(), 22.0], egui::SelectableLabel::new(all, name));
                if r.on_hover_text("Show and edit every channel (Ctrl+2)").clicked() {
                    self.channels.reset();
                }
            });
        }
        for (c, (name, swatch)) in list.iter().enumerate() {
            ui.horizontal(|ui| {
                if n > 1 && eye(ui, &mut self.channels.visible[c]) && !self.channels.visible[..n].contains(&true) {
                    // Hiding the last one shows them all again, as nothing would be left.
                    self.channels.visible = [true; 4];
                }
                if n == 1 {
                    ui.add(Icon::Visible.image(16.0, theme::text()));
                }
                let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().rect_filled(dot, 2.0, *swatch);
                ui.painter().rect_stroke(dot, 2.0, egui::Stroke::new(1.0_f32, theme::line()), egui::StrokeKind::Inside);
                let selected = (self.channels.edit[c] && !all) || n == 1;
                let label = if n > 1 { format!("{name}   Ctrl+{}", c + 3) } else { name.to_string() };
                let r = ui.add_sized([ui.available_width(), 22.0], egui::SelectableLabel::new(selected, label));
                let hint = if n > 1 {
                    "Click: edit and show only this channel. Shift-click: add or remove it. Ctrl-click: load it as a selection"
                } else {
                    "The document's one channel. Ctrl-click: load it as a selection"
                };
                if r.on_hover_text(hint).clicked() {
                    if ctrl {
                        self.load_colour_channel(c, SelectionOp::Replace);
                    } else if n == 1 {
                    } else if shift && !all {
                        self.channels.edit[c] = !self.channels.edit[c];
                        if !self.channels.edit[..n].contains(&true) {
                            self.channels.reset();
                        }
                        self.channels.visible = self.channels.edit;
                    } else {
                        self.channels.solo(c);
                    }
                }
            });
        }
        if !all && n > 1 && !self.channels.edit[..n].iter().all(|e| *e) {
            let names: Vec<&str> = (0..n).filter(|&c| self.channels.edit[c]).map(|c| list[c].0).collect();
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

    /// Ctrl+2 shows every channel, Ctrl+3, 4, 5 (6 in CMYK) one alone (as in Photoshop).
    pub(crate) fn channel_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        self.sync_channels();
        let pressed = |key| ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, key));
        if pressed(egui::Key::Num2) {
            self.channels.reset();
        }
        let n = self.channels.count();
        for (c, key) in [egui::Key::Num3, egui::Key::Num4, egui::Key::Num5, egui::Key::Num6].into_iter().enumerate() {
            if c < n && n > 1 && pressed(key) {
                self.channels.solo(c);
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
