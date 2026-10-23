//! The text tool (T) and the balloon tool (Shift+T), as Clip Studio's and
//! Photoshop's type: click to write, or drag a balloon and write in it.
//! Texts live on vector layers (a new "Text" layer when the current one
//! isn't vector) and stay editable: click one to change its words, font,
//! size, colour, alignment, direction, outline or balloon; drag it to move
//! it, drag the balloon's tail tip to aim it. Esc or a click elsewhere ends
//! the editing; an empty text without a balloon goes away.

use eframe::egui::{self, Color32, Painter, Pos2, Rect, Stroke, Vec2};
use painter_core::text::{self, Balloon, BalloonShape, TextAlign, TextItem};
use painter_core::{Layer, LayerId, LayerKind};
use painter_render::Plane;

use crate::app::PainterApp;
use crate::tools::Tool;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextKind {
    Text,
    Balloon,
}

#[derive(Clone, Copy, Debug)]
enum TextDrag {
    /// The text and its balloon, by the offset from where it was grabbed.
    Move {
        grab: Vec2,
        tail: Option<Vec2>,
    },
    Tail,
    /// A new balloon from this corner.
    NewBalloon {
        start: Pos2,
    },
}

pub(crate) struct TextState {
    /// The text being edited: its layer and index.
    pub editing: Option<(LayerId, usize)>,
    drag: Option<TextDrag>,
    pointer: Pos2,
    /// Settings new texts start with (the last ones used).
    pub template: TextItem,
    font_filter: String,
    /// Fonts found so far (the scan runs in the background).
    fonts: Vec<String>,
    /// The text last typed into (one undo step for a run of typing).
    typed: Option<(LayerId, usize)>,
}

impl Default for TextState {
    fn default() -> Self {
        let template = TextItem { size: 60.0, ..Default::default() };
        Self { editing: None, drag: None, pointer: Pos2::ZERO, template, font_filter: String::new(), fonts: Vec::new(), typed: None }
    }
}

/// Built-in font for texts whose font isn't here, and the system's fonts found in the background.
pub(crate) fn load_fonts() {
    if let Some(data) = egui::FontDefinitions::default().font_data.get("Ubuntu-Light") {
        text::set_fallback_font(data.font.to_vec());
    }
    std::thread::Builder::new().name("font-scan".into()).spawn(|| text::scan_font_dirs(&text::system_font_dirs())).ok();
}

impl PainterApp {
    fn text_item(&self) -> Option<&TextItem> {
        let (id, i) = self.text.editing?;
        match &self.doc.find(id)?.kind {
            LayerKind::Vector(v) => v.texts.get(i),
            _ => None,
        }
    }

    fn text_item_mut(&mut self) -> Option<&mut TextItem> {
        let (id, i) = self.text.editing?;
        match &mut self.doc.find_mut(id)?.kind {
            LayerKind::Vector(v) => v.texts.get_mut(i),
            _ => None,
        }
    }

    /// Draws the text layer `id` again after a change.
    fn redraw_text_layer(&mut self, id: LayerId) {
        let canvas = self.doc.canvas();
        if let Some(LayerKind::Vector(v)) = self.doc.find_mut(id).map(|l| &mut l.kind) {
            painter_core::vector::rasterize(v, canvas);
        }
        self.upload_plane(Plane::Color(id));
        self.dirty = true;
    }

    /// The text (or balloon) under `at`, on visible vector layers, topmost first.
    fn text_at(&self, at: Pos2) -> Option<(LayerId, usize)> {
        fn walk(layers: &[Layer], at: [f32; 2]) -> Option<(LayerId, usize)> {
            layers.iter().rev().filter(|l| l.visible).find_map(|l| match &l.kind {
                LayerKind::Vector(v) => text::hit(&v.texts, at).map(|i| (l.id, i)),
                LayerKind::Folder(f) => walk(&f.children, at),
                _ => None,
            })
        }
        walk(&self.doc.layers, [at.x, at.y])
    }

    /// Ends the editing; an empty text without a balloon is removed.
    pub(crate) fn finish_text(&mut self) {
        self.text.typed = None;
        let Some((id, i)) = self.text.editing.take() else { return };
        let empty = matches!(self.doc.find(id).map(|l| &l.kind), Some(LayerKind::Vector(v)) if v.texts.get(i).is_some_and(|t| t.text.trim().is_empty() && t.balloon.is_none()));
        if empty {
            if let Some(LayerKind::Vector(v)) = self.doc.find_mut(id).map(|l| &mut l.kind) {
                v.texts.remove(i);
            }
            self.redraw_text_layer(id);
        }
    }

    /// A vector layer to put a new text on: the current one, or a new "Text" layer above it.
    fn text_layer(&mut self) -> Option<LayerId> {
        if let Some(l) = self.doc.active_layer() {
            if l.is_vector() && !l.locked && l.visible {
                return Some(l.id);
            }
        }
        let id = self.doc.alloc_id();
        self.doc.add_layer(Layer::vector(id, "Text"));
        self.doc.active = id;
        Some(id)
    }

    fn new_text(&mut self, item: TextItem) {
        self.next_label = Some(if item.balloon.is_some() { "New Balloon" } else { "New Text" }.into());
        self.checkpoint();
        let Some(id) = self.text_layer() else { return };
        let mut i = 0;
        if let Some(LayerKind::Vector(v)) = self.doc.find_mut(id).map(|l| &mut l.kind) {
            v.texts.push(item);
            i = v.texts.len() - 1;
        }
        self.text.editing = Some((id, i));
        self.redraw_text_layer(id);
    }

    pub(crate) fn text_press(&mut self, at: Pos2, kind: TextKind) {
        self.text.typed = None;
        self.text.pointer = at;
        // The balloon's tail tip of the text being edited.
        let reach = 10.0 / self.view.zoom.max(1e-3);
        if let Some(tip) = self.text_item().and_then(|t| t.balloon.as_ref()).and_then(|b| b.tail) {
            if Pos2::new(tip[0], tip[1]).distance(at) <= reach {
                self.next_label = Some("Move Balloon Tail".into());
                self.checkpoint();
                self.text.drag = Some(TextDrag::Tail);
                return;
            }
        }
        if let Some((id, i)) = self.text_at(at) {
            if self.text.editing != Some((id, i)) {
                self.finish_text();
            }
            // Finishing may have removed an empty text below this one.
            let Some((id, i)) = self.text_at(at) else { return };
            self.text.editing = Some((id, i));
            self.doc.active = id;
            if let Some(t) = self.text_item() {
                let grab = at - Pos2::new(t.pos[0], t.pos[1]);
                let tail = t.balloon.as_ref().and_then(|b| b.tail).map(|p| Pos2::new(p[0], p[1]) - Pos2::new(t.pos[0], t.pos[1]));
                self.next_label = Some("Move Text".into());
                self.checkpoint();
                self.text.drag = Some(TextDrag::Move { grab, tail });
            }
            return;
        }
        let was_editing = self.text.editing.is_some();
        self.finish_text();
        match kind {
            TextKind::Balloon => self.text.drag = Some(TextDrag::NewBalloon { start: at }),
            // A click away from the text being edited only ends the editing.
            TextKind::Text if was_editing => {}
            TextKind::Text => {
                let item = TextItem {
                    text: String::new(),
                    pos: [at.x, at.y],
                    color: self.main_rgb8(),
                    balloon: None,
                    ..self.text.template.clone()
                };
                self.new_text(item);
            }
        }
    }

    fn main_rgb8(&self) -> [u8; 3] {
        self.main_rgb().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
    }

    /// Whether the press started a drag (so moves and the release come back).
    pub(crate) fn text_dragging(&self) -> bool {
        self.text.drag.is_some()
    }

    pub(crate) fn text_move(&mut self, at: Pos2) {
        self.text.pointer = at;
        let Some(drag) = self.text.drag else { return };
        let id = match self.text.editing {
            Some((id, _)) => id,
            None => return,
        };
        let changed = match (drag, self.text_item_mut()) {
            (TextDrag::Move { grab, tail }, Some(t)) => {
                let p = at - grab;
                t.pos = [p.x, p.y];
                if let (Some(b), Some(off)) = (&mut t.balloon, tail) {
                    b.tail = Some([p.x + off.x, p.y + off.y]);
                }
                true
            }
            (TextDrag::Tail, Some(t)) => {
                if let Some(b) = &mut t.balloon {
                    b.tail = Some([at.x, at.y]);
                }
                true
            }
            _ => false,
        };
        if changed {
            self.redraw_text_layer(id);
        }
    }

    pub(crate) fn text_release(&mut self) {
        let Some(drag) = self.text.drag.take() else { return };
        if let TextDrag::NewBalloon { start } = drag {
            let end = self.text.pointer;
            let (mut c, mut r) = (start + (end - start) / 2.0, ((end - start) / 2.0).abs());
            if r.x < 20.0 || r.y < 15.0 {
                // A click: a balloon of a usual size round the click.
                c = start;
                r = Vec2::new(self.text.template.size * 3.5, self.text.template.size * 2.2);
            }
            let bal = Balloon {
                radii: [r.x, r.y],
                tail: Some([c.x - r.x * 0.3, c.y + r.y * 1.6]),
                ..self.text.template.balloon.clone().unwrap_or_default()
            };
            let item = TextItem { pos: [c.x, c.y], color: self.main_rgb8(), balloon: Some(bal), ..self.text.template.clone() };
            self.new_text(item);
        }
    }

    pub(crate) fn text_keys(&mut self, ctx: &egui::Context) {
        if self.text.editing.is_some() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.finish_text();
        }
        if self.text.editing.is_some() && !matches!(self.tool, Tool::Text(_)) {
            self.finish_text();
        }
    }

    /// The edited text's box and its balloon's tail handle.
    pub(crate) fn draw_text_overlay(&self, painter: &Painter, rect: Rect) {
        if let Some(TextDrag::NewBalloon { start }) = self.text.drag {
            let doc = self.doc_size();
            let a = self.view.doc_to_screen(rect, doc, start);
            let b = self.view.doc_to_screen(rect, doc, self.text.pointer);
            painter.rect_stroke(Rect::from_two_pos(a, b), 0.0, Stroke::new(1.0_f32, crate::theme::accent()), egui::StrokeKind::Middle);
        }
        let Some(t) = self.text_item() else { return };
        let doc = self.doc_size();
        let screen = |x: f32, y: f32| self.view.doc_to_screen(rect, doc, Pos2::new(x, y));
        let b = text::bounds(t);
        let r = Rect::from_two_pos(screen(b[0], b[1]), screen(b[2], b[3]));
        painter.rect_stroke(r, 0.0, Stroke::new(3.0_f32, Color32::from_black_alpha(90)), egui::StrokeKind::Middle);
        painter.rect_stroke(r, 0.0, Stroke::new(1.0_f32, crate::theme::accent()), egui::StrokeKind::Middle);
        if let Some(tip) = t.balloon.as_ref().and_then(|b| b.tail) {
            let c = screen(tip[0], tip[1]);
            painter.circle_filled(c, 5.0, Color32::WHITE);
            painter.circle_stroke(c, 5.0, Stroke::new(1.5_f32, crate::theme::accent()));
        }
    }

    /// The text being edited: its words and look, in a window by the canvas.
    pub(crate) fn text_editor_window(&mut self, ctx: &egui::Context) {
        let Some((id, _)) = self.text.editing else { return };
        let Some(mut item) = self.text_item().cloned() else {
            self.text.editing = None;
            return;
        };
        let before = item.clone();
        let mut open = true;
        let mut delete = false;
        let mut done = false;
        let fonts = self.font_list();
        let ink = self.main_rgb8();
        egui::Window::new("Text").id(egui::Id::new("text_editor")).open(&mut open).resizable(true).default_width(320.0).show(
            ctx,
            |ui| {
                let r = ui.add(
                    egui::TextEdit::multiline(&mut item.text).desired_rows(3).desired_width(f32::INFINITY).hint_text("Type here"),
                );
                if self.text.drag.is_none() && !r.has_focus() && before.text.is_empty() {
                    r.request_focus();
                }
                ui.separator();
                text_props_ui(ui, &mut item, &fonts, &mut self.text.font_filter, ink);
                ui.separator();
                ui.horizontal(|ui| {
                    done = ui.button("Done").clicked();
                    delete = ui.button("Delete").clicked();
                });
            },
        );
        if delete {
            self.next_label = Some("Delete Text".into());
            self.checkpoint();
            if let Some((id, i)) = self.text.editing.take() {
                if let Some(LayerKind::Vector(v)) = self.doc.find_mut(id).map(|l| &mut l.kind) {
                    v.texts.remove(i);
                }
                self.redraw_text_layer(id);
            }
            return;
        }
        if item != before {
            // One undo step for a run of typing; style changes each get one.
            let typing = item.text != before.text && TextItem { text: before.text.clone(), ..item.clone() } == before;
            if !typing || self.text.typed.is_none_or(|t| t != (id, self.text.editing.map_or(0, |e| e.1))) {
                self.next_label = Some(if typing { "Type Text" } else { "Text Style" }.into());
                self.checkpoint();
            }
            self.text.typed = typing.then(|| (id, self.text.editing.map_or(0, |e| e.1)));
            // New texts start like this one.
            self.text.template = TextItem { text: String::new(), pos: [0.0, 0.0], ..item.clone() };
            if let Some(t) = self.text_item_mut() {
                *t = item;
            }
            self.redraw_text_layer(id);
        }
        if done || !open {
            self.finish_text();
        }
    }

    fn font_list(&mut self) -> Vec<String> {
        // The background scan adds fonts for a moment after startup.
        let names = text::font_names();
        if names.len() != self.text.fonts.len() {
            self.text.fonts = names;
        }
        self.text.fonts.clone()
    }

    /// Tool Settings for the text and balloon tools: what new texts look like.
    pub(crate) fn text_panel(&mut self, ui: &mut egui::Ui, kind: TextKind) {
        let fonts = self.font_list();
        let ink = self.main_rgb8();
        let mut template = self.text.template.clone();
        if kind == TextKind::Balloon && template.balloon.is_none() {
            template.balloon = Some(Balloon::default());
        }
        text_props_ui(ui, &mut template, &fonts, &mut self.text.font_filter, ink);
        self.text.template = template;
        ui.small(match kind {
            TextKind::Text => "Click to write; click a text to edit it, drag it to move it. Esc ends.",
            TextKind::Balloon => "Drag a balloon (or click), then write in it. Drag the tail's tip to aim it.",
        });
        ui.small("New texts take the main colour.");
    }
}

/// Font, size, colour, alignment, direction, outline and balloon of `item`.
fn text_props_ui(ui: &mut egui::Ui, item: &mut TextItem, fonts: &[String], filter: &mut String, ink: [u8; 3]) {
    let shown = if item.font.is_empty() { "Built-in".to_string() } else { item.font.clone() };
    let missing = !text::has_font(&item.font);
    ui.horizontal(|ui| {
        ui.label("Font");
        egui::ComboBox::from_id_salt("text_font")
            .selected_text(if missing { format!("{shown} (missing)") } else { shown })
            .width(200.0)
            .show_ui(ui, |ui| {
                ui.add(egui::TextEdit::singleline(filter).hint_text("Search fonts"));
                egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                    ui.selectable_value(&mut item.font, String::new(), "Built-in");
                    let f = filter.to_lowercase();
                    for name in fonts.iter().filter(|n| f.is_empty() || n.to_lowercase().contains(&f)) {
                        ui.selectable_value(&mut item.font, name.clone(), name);
                    }
                });
            });
    });
    if fonts.is_empty() {
        ui.small("Looking for the fonts on this computer…");
    }
    ui.horizontal(|ui| {
        ui.add(egui::DragValue::new(&mut item.size).range(4.0..=2000.0).speed(0.5).max_decimals(1).suffix(" px"));
        ui.color_edit_button_srgb(&mut item.color);
        if ui.small_button("Main colour").clicked() {
            item.color = ink;
        }
    });
    ui.horizontal(|ui| {
        ui.selectable_value(&mut item.align, TextAlign::Left, "Left");
        ui.selectable_value(&mut item.align, TextAlign::Center, "Centre");
        ui.selectable_value(&mut item.align, TextAlign::Right, "Right");
        ui.checkbox(&mut item.vertical, "Vertical");
    });
    ui.add(egui::Slider::new(&mut item.line_spacing, 0.6..=3.0).text("Line spacing"));
    ui.add(egui::Slider::new(&mut item.tracking, -20.0..=80.0).text("Letter spacing").suffix(" px"));
    ui.horizontal(|ui| {
        ui.add(egui::Slider::new(&mut item.outline, 0.0..=30.0).text("Outline").suffix(" px"));
        ui.color_edit_button_srgb(&mut item.outline_color);
    });
    let mut has_balloon = item.balloon.is_some();
    if ui.checkbox(&mut has_balloon, "Balloon").changed() {
        item.balloon = has_balloon.then(|| {
            let b = text::bounds(&TextItem { balloon: None, ..item.clone() });
            let r = [((b[2] - b[0]) * 0.75).max(item.size * 2.0), ((b[3] - b[1]) * 0.85).max(item.size * 1.4)];
            Balloon { radii: r, tail: Some([item.pos[0] - r[0] * 0.3, item.pos[1] + r[1] * 1.6]), ..Default::default() }
        });
    }
    // The words alone, for Fit to Text.
    let words = text::bounds(&TextItem { balloon: None, ..item.clone() });
    if let Some(b) = &mut item.balloon {
        ui.horizontal_wrapped(|ui| {
            for s in BalloonShape::ALL {
                ui.selectable_value(&mut b.shape, s, s.name());
            }
        });
        ui.add(egui::Slider::new(&mut b.radii[0], 10.0..=3000.0).logarithmic(true).text("Width").suffix(" px"));
        ui.add(egui::Slider::new(&mut b.radii[1], 10.0..=3000.0).logarithmic(true).text("Height").suffix(" px"));
        ui.horizontal(|ui| {
            ui.label("Fill");
            ui.color_edit_button_srgba_unmultiplied(&mut b.fill);
            ui.label("Line");
            ui.color_edit_button_srgb(&mut b.line);
            ui.add(egui::DragValue::new(&mut b.width).range(0.0..=60.0).speed(0.2).suffix(" px"));
        });
        let mut tail = b.tail.is_some();
        if ui.checkbox(&mut tail, "Tail").changed() {
            b.tail = tail.then(|| [item.pos[0] - b.radii[0] * 0.3, item.pos[1] + b.radii[1] * 1.6]);
        }
        if ui.button("Fit to Text").on_hover_text("Size the balloon round the words").clicked() {
            let t = words;
            b.radii = [((t[2] - t[0]) * 0.72).max(20.0), ((t[3] - t[1]) * 0.8).max(15.0)];
        }
    }
}

/// A system font with arrows and other symbols the built-in interface font
/// lacks (→, ✓…), added after it so menus and hints show them.
pub(crate) fn install_symbol_fallback(ctx: &egui::Context) {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:/Windows".into());
    let candidates = [
        format!("{windir}/Fonts/seguisym.ttf"),
        "/System/Library/Fonts/Apple Symbols.ttf".into(),
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf".into(),
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf".into(),
        "/usr/share/fonts/TTF/DejaVuSans.ttf".into(),
        "/usr/share/fonts/dejavu/DejaVuSans.ttf".into(),
        "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf".into(),
        "/system/fonts/Roboto-Regular.ttf".into(),
    ];
    let Some(bytes) = candidates.iter().find_map(|p| std::fs::read(p).ok()) else { return };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("symbols".into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("symbols".into());
    }
    ctx.set_fonts(fonts);
}

impl PainterApp {
    /// For screenshots (`dev.text_demo`): a caption, a speech balloon with a
    /// tail, a shout and a vertical line of text, the balloon left open for editing.
    pub(crate) fn text_demo(&mut self) {
        let (w, h) = self.doc.canvas();
        let (w, h) = (w as f32, h as f32);
        let font = text::font_names()
            .into_iter()
            .find(|n| n == "Comic Sans MS" || n == "Arial" || n.starts_with("DejaVu Sans"))
            .unwrap_or_default();
        let base = TextItem { font, size: w / 30.0, ..Default::default() };
        self.new_text(TextItem {
            text: "Meanwhile, in the city…".into(),
            pos: [w * 0.5, h * 0.08],
            outline: w / 300.0,
            ..base.clone()
        });
        self.finish_text();
        self.new_text(TextItem {
            text: "Shout!".into(),
            pos: [w * 0.72, h * 0.62],
            size: w / 20.0,
            balloon: Some(Balloon { shape: BalloonShape::Burst, radii: [w * 0.2, w * 0.13], width: w / 400.0, ..Default::default() }),
            ..base.clone()
        });
        self.finish_text();
        self.new_text(TextItem { text: "縦書き\nTEST".into(), vertical: true, pos: [w * 0.15, h * 0.55], ..base.clone() });
        self.finish_text();
        self.new_text(TextItem {
            text: "Hello!\nCan you hear me?".into(),
            pos: [w * 0.45, h * 0.3],
            balloon: Some(Balloon {
                radii: [w * 0.22, w * 0.12],
                width: w / 400.0,
                tail: Some([w * 0.3, h * 0.45]),
                ..Default::default()
            }),
            ..base
        });
        self.tool = Tool::Text(TextKind::Balloon);
    }
}
