//! The text tool (T) and the balloon tool (Shift+T), as Clip Studio's and
//! Photoshop's type: click to write, or drag a balloon and write in it.
//! Texts live on vector layers (a new "Text" layer when the current one
//! isn't vector) and stay editable: click one to change its words, font,
//! size, colour, alignment, direction, outline or balloon; drag it to move
//! it. Esc or a click elsewhere ends the editing; an empty text without a
//! balloon goes away.
//!
//! The balloon tool has Clip Studio's sub tools (rounded, rough, ellipse,
//! rectangle, polygon, thought and flash balloons, the balloon pen and the
//! two tails), each with its own settings. A selected balloon has handles to
//! size and turn it, and each tail has handles for its base, bend and tip.

use eframe::egui::{self, Color32, Painter, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use crate::i18n::tr;
use painter_core::text::{self, Antialias, Balloon, BalloonShape, Tail, TextAlign, TextItem};
use painter_core::{Layer, LayerId, LayerKind};
use painter_render::Plane;

use crate::app::PainterApp;
use crate::tools::Tool;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextKind {
    Text,
    Balloon,
}

/// The balloon tool's sub tools, as Clip Studio's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BalloonTool {
    Rounded,
    Rough,
    Ellipse,
    Rectangle,
    Polygon,
    Thought,
    Flash,
    Pen,
    Tail,
    ThoughtTail,
}

impl BalloonTool {
    pub const ALL: [BalloonTool; 10] = [
        BalloonTool::Rounded,
        BalloonTool::Rough,
        BalloonTool::Ellipse,
        BalloonTool::Rectangle,
        BalloonTool::Polygon,
        BalloonTool::Thought,
        BalloonTool::Flash,
        BalloonTool::Pen,
        BalloonTool::Tail,
        BalloonTool::ThoughtTail,
    ];

    pub fn name(self) -> &'static str {
        match self {
            BalloonTool::Rounded => "Rounded balloon",
            BalloonTool::Rough => "Rough rounded balloon",
            BalloonTool::Ellipse => "Ellipse balloon",
            BalloonTool::Rectangle => "Rectangle balloon",
            BalloonTool::Polygon => "Polygon balloon",
            BalloonTool::Thought => "Thought balloon",
            BalloonTool::Flash => "Flash balloon",
            BalloonTool::Pen => "Balloon pen",
            BalloonTool::Tail => "Balloon tail",
            BalloonTool::ThoughtTail => "Thought balloon tail",
        }
    }

    fn is_tail(self) -> bool {
        matches!(self, BalloonTool::Tail | BalloonTool::ThoughtTail)
    }

    fn balloon(self) -> Balloon {
        let b = Balloon::default();
        match self {
            BalloonTool::Rounded | BalloonTool::Tail | BalloonTool::ThoughtTail => {
                Balloon { shape: BalloonShape::Rectangle, corner: 95.0, ..b }
            }
            BalloonTool::Rough => Balloon { shape: BalloonShape::Rectangle, corner: 95.0, rough: 60.0, ..b },
            BalloonTool::Ellipse => b,
            BalloonTool::Rectangle => Balloon { shape: BalloonShape::Rectangle, ..b },
            BalloonTool::Polygon => Balloon { shape: BalloonShape::Polygon, sides: 6, ..b },
            BalloonTool::Thought => Balloon { shape: BalloonShape::Cloud, ..b },
            BalloonTool::Flash => Balloon { shape: BalloonShape::Burst, ..b },
            BalloonTool::Pen => Balloon { shape: BalloonShape::Free, ..b },
        }
    }
}

/// Where a balloon's line or fill colour comes from, as Clip Studio's three buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ink {
    Main,
    Sub,
    Custom,
}

/// One sub tool's settings.
#[derive(Clone, Debug)]
pub(crate) struct BalloonPreset {
    pub balloon: Balloon,
    pub line: Ink,
    pub fill: Ink,
    /// Tails: width at the base, % of the balloon's shorter half side.
    pub tail_width: f32,
}

#[derive(Clone, Debug)]
enum TextDrag {
    /// The text and its balloon, from where it was grabbed.
    Move {
        grab: Pos2,
        orig: TextItem,
    },
    /// A tail's base (0), bend (1) or tip (2).
    TailPoint {
        tail: usize,
        which: u8,
    },
    /// A side or corner handle: `side` is -1, 0 or 1 across and down, in the balloon's own turn.
    Resize {
        side: [f32; 2],
        orig: TextItem,
    },
    Rotate {
        grab: Pos2,
        orig: TextItem,
    },
    /// A new balloon from this corner.
    NewBalloon {
        start: Pos2,
    },
    /// A balloon drawn by hand (the points are in `TextState::pen`).
    Pen,
    /// A new tail from this point in the selected balloon.
    NewTail {
        base: Pos2,
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
    pub balloon_tool: BalloonTool,
    /// Settings of each sub tool, in `BalloonTool::ALL`'s order.
    pub presets: Vec<BalloonPreset>,
    /// Balloons go on a new layer each (else on the selected vector layer).
    pub new_layer: bool,
    pen: Vec<Pos2>,
}

impl Default for TextState {
    fn default() -> Self {
        let template = TextItem { size: 60.0, ..Default::default() };
        let presets = BalloonTool::ALL
            .iter()
            .map(|t| BalloonPreset { balloon: t.balloon(), line: Ink::Main, fill: Ink::Sub, tail_width: 45.0 })
            .collect();
        Self {
            editing: None,
            drag: None,
            pointer: Pos2::ZERO,
            template,
            font_filter: String::new(),
            fonts: Vec::new(),
            typed: None,
            balloon_tool: BalloonTool::Rounded,
            presets,
            new_layer: false,
            pen: Vec::new(),
        }
    }
}

impl TextState {
    fn preset(&self) -> &BalloonPreset {
        let i = BalloonTool::ALL.iter().position(|t| *t == self.balloon_tool).unwrap_or(0);
        &self.presets[i]
    }

    fn preset_mut(&mut self) -> &mut BalloonPreset {
        let i = BalloonTool::ALL.iter().position(|t| *t == self.balloon_tool).unwrap_or(0);
        &mut self.presets[i]
    }
}

/// Built-in font for texts whose font isn't here, and the system's fonts found in the background.
pub(crate) fn load_fonts() {
    if let Some(data) = egui::FontDefinitions::default().font_data.get("Ubuntu-Light") {
        text::set_fallback_font(data.font.to_vec());
    }
    std::thread::Builder::new().name("font-scan".into()).spawn(|| text::scan_font_dirs(&text::system_font_dirs())).ok();
}

fn pos(p: [f32; 2]) -> Pos2 {
    Pos2::new(p[0], p[1])
}

/// `v` turned by `angle` radians.
fn turn(v: Vec2, angle: f32) -> Vec2 {
    let (s, c) = angle.sin_cos();
    Vec2::new(v.x * c - v.y * s, v.x * s + v.y * c)
}

/// The balloon's handles: eight round its box (with their sides) and the turning one above it.
fn balloon_handles(b: &Balloon, centre: [f32; 2], zoom: f32) -> (Vec<([f32; 2], Pos2)>, Pos2) {
    let c = pos(centre);
    let mut out = Vec::new();
    for sy in [-1.0, 0.0, 1.0] {
        for sx in [-1.0, 0.0, 1.0] {
            if sx != 0.0 || sy != 0.0 {
                out.push(([sx, sy], c + turn(Vec2::new(sx * b.radii[0], sy * b.radii[1]), b.rotation)));
            }
        }
    }
    let rotate = c + turn(Vec2::new(0.0, -b.radii[1] - 28.0 / zoom.max(1e-3)), b.rotation);
    (out, rotate)
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

    /// Edits this text; an older file's balloon becomes today's kind (it looks the same).
    fn select_text(&mut self, id: LayerId, i: usize) {
        self.text.editing = Some((id, i));
        self.doc.active = id;
        if let Some(t) = self.text_item_mut() {
            let centre = t.pos;
            if let Some(b) = &mut t.balloon {
                b.upgrade(centre);
            }
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

    /// A vector layer to put a new text on: the current one, or a new layer above it.
    fn text_layer(&mut self, new_layer: bool, name: &str) -> Option<LayerId> {
        if let Some(l) = self.doc.active_layer() {
            if l.is_vector() && !l.locked && l.visible && !new_layer {
                return Some(l.id);
            }
        }
        let id = self.doc.alloc_id();
        self.doc.add_layer(Layer::vector(id, name));
        self.doc.active = id;
        Some(id)
    }

    fn new_text(&mut self, item: TextItem) {
        let balloon = item.balloon.is_some();
        self.next_label = Some(if balloon { "New Balloon" } else { "New Text" }.into());
        self.checkpoint();
        let new_layer = balloon && self.text.new_layer;
        let Some(id) = self.text_layer(new_layer, if balloon { "Balloon" } else { "Text" }) else { return };
        let mut i = 0;
        if let Some(LayerKind::Vector(v)) = self.doc.find_mut(id).map(|l| &mut l.kind) {
            v.texts.push(item);
            i = v.texts.len() - 1;
        }
        self.text.editing = Some((id, i));
        self.redraw_text_layer(id);
    }

    /// A colour for a balloon's line or fill.
    fn ink(&self, ink: Ink, custom: [u8; 3]) -> [u8; 3] {
        let of = |c| self.ink_rgb(c).map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        match ink {
            Ink::Main => of(self.color),
            Ink::Sub => of(self.sub_color),
            Ink::Custom => custom,
        }
    }

    /// The sub tool's balloon, in its colours.
    fn preset_balloon(&self) -> Balloon {
        let p = self.text.preset();
        let mut b = p.balloon.clone();
        b.line = self.ink(p.line, b.line);
        if p.fill != Ink::Custom {
            let [r, g, bl] = self.ink(p.fill, [0; 3]);
            b.fill = [r, g, bl, 255];
        }
        b
    }

    /// Grabs a handle of the selected balloon (tail points, sides and corners, turning).
    fn grab_handle(&mut self, at: Pos2) -> Option<TextDrag> {
        let reach = 9.0 / self.view.zoom.max(1e-3);
        let t = self.text_item()?;
        let b = t.balloon.as_ref()?;
        for (i, tail) in b.tails.iter().enumerate() {
            for (which, p) in [(2u8, tail.tip), (1, tail.middle()), (0, tail.base)] {
                if pos(p).distance(at) <= reach {
                    return Some(TextDrag::TailPoint { tail: i, which });
                }
            }
        }
        let (handles, rotate) = balloon_handles(b, t.pos, self.view.zoom);
        if rotate.distance(at) <= reach {
            return Some(TextDrag::Rotate { grab: at, orig: t.clone() });
        }
        handles.iter().find(|(_, p)| p.distance(at) <= reach).map(|(side, _)| TextDrag::Resize { side: *side, orig: t.clone() })
    }

    pub(crate) fn text_press(&mut self, at: Pos2, kind: TextKind) {
        self.text.typed = None;
        self.text.pointer = at;
        if let Some(drag) = self.grab_handle(at) {
            self.next_label = Some(
                match drag {
                    TextDrag::TailPoint { .. } => "Edit Balloon Tail",
                    TextDrag::Rotate { .. } => "Rotate Balloon",
                    _ => "Resize Balloon",
                }
                .into(),
            );
            self.checkpoint();
            self.text.drag = Some(drag);
            return;
        }
        let tool = self.text.balloon_tool;
        if kind == TextKind::Balloon && tool.is_tail() {
            // A tail starts inside a balloon and goes where it's dragged.
            let hit = self.text_at(at).filter(|&(id, i)| {
                matches!(self.doc.find(id).map(|l| &l.kind), Some(LayerKind::Vector(v)) if v.texts.get(i).is_some_and(|t| t.balloon.is_some()))
            });
            match hit {
                Some((id, i)) => {
                    if self.text.editing != Some((id, i)) {
                        self.finish_text();
                    }
                    if let Some((id, i)) = self.text_at(at) {
                        self.select_text(id, i);
                        self.text.drag = Some(TextDrag::NewTail { base: at });
                    }
                }
                None => {
                    self.finish_text();
                    self.status = tr("Drag from inside a balloon to draw its tail").into();
                }
            }
            return;
        }
        if let Some((id, i)) = self.text_at(at) {
            if self.text.editing != Some((id, i)) {
                self.finish_text();
            }
            // Finishing may have removed an empty text below this one.
            let Some((id, i)) = self.text_at(at) else { return };
            self.select_text(id, i);
            if let Some(t) = self.text_item() {
                let orig = t.clone();
                self.next_label = Some("Move Text".into());
                self.checkpoint();
                self.text.drag = Some(TextDrag::Move { grab: at, orig });
            }
            return;
        }
        let was_editing = self.text.editing.is_some();
        self.finish_text();
        match kind {
            TextKind::Balloon if tool == BalloonTool::Pen => {
                self.text.pen = vec![at];
                self.text.drag = Some(TextDrag::Pen);
            }
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
        let Some(drag) = self.text.drag.clone() else { return };
        if matches!(drag, TextDrag::Pen) {
            if self.text.pen.last().is_none_or(|p| p.distance(at) >= 1.5 / self.view.zoom.max(1e-3)) {
                self.text.pen.push(at);
            }
            return;
        }
        let Some((id, _)) = self.text.editing else { return };
        let snap = self.pointer_mods.shift;
        let Some(t) = self.text_item_mut() else { return };
        match drag {
            TextDrag::Move { grab, orig } => {
                let d = at - grab;
                *t = orig;
                t.transform(|p| [p[0] + d.x, p[1] + d.y], 1.0);
            }
            TextDrag::TailPoint { tail, which } => {
                if let Some(tail) = t.balloon.as_mut().and_then(|b| b.tails.get_mut(tail)) {
                    match which {
                        0 => tail.base = [at.x, at.y],
                        1 => tail.bend = Some([at.x, at.y]),
                        _ => tail.tip = [at.x, at.y],
                    }
                }
            }
            TextDrag::Resize { side, orig } => {
                let Some(ob) = &orig.balloon else { return };
                // In the balloon's own turn: the opposite side stays where it was.
                let local = turn(at - pos(orig.pos), -ob.rotation);
                let mut centre = Vec2::ZERO;
                let mut radii = ob.radii;
                for k in 0..2 {
                    if side[k] != 0.0 {
                        let anchor = -side[k] * ob.radii[k];
                        let p = if k == 0 { local.x } else { local.y };
                        radii[k] = ((p - anchor).abs() / 2.0).max(4.0);
                        centre[k] = anchor + side[k] * radii[k];
                    }
                }
                let moved = turn(centre, ob.rotation);
                *t = orig.clone();
                t.pos = [orig.pos[0] + moved.x, orig.pos[1] + moved.y];
                if let Some(b) = &mut t.balloon {
                    b.radii = radii;
                    // Tails keep their tips and follow with their bases.
                    for tail in &mut b.tails {
                        tail.base = [tail.base[0] + moved.x, tail.base[1] + moved.y];
                    }
                }
            }
            TextDrag::Rotate { grab, orig } => {
                let c = pos(orig.pos);
                let mut angle = (at - c).angle() - (grab - c).angle();
                let start = orig.balloon.as_ref().map_or(0.0, |b| b.rotation);
                if snap {
                    let step = 15f32.to_radians();
                    angle = ((start + angle) / step).round() * step - start;
                }
                *t = orig;
                if let Some(b) = &mut t.balloon {
                    b.rotation = start + angle;
                }
            }
            TextDrag::NewBalloon { .. } | TextDrag::NewTail { .. } | TextDrag::Pen => return,
        }
        self.redraw_text_layer(id);
    }

    pub(crate) fn text_release(&mut self) {
        let Some(drag) = self.text.drag.take() else { return };
        let end = self.text.pointer;
        let zoom = self.view.zoom.max(1e-3);
        match drag {
            TextDrag::NewBalloon { start } => {
                let (mut c, mut r) = (start + (end - start) / 2.0, ((end - start) / 2.0).abs());
                if r.x < 20.0 || r.y < 15.0 {
                    // A click: a balloon of a usual size round the click.
                    c = start;
                    r = Vec2::new(self.text.template.size * 3.5, self.text.template.size * 2.2);
                }
                let bal = Balloon { radii: [r.x, r.y], ..self.preset_balloon() };
                let item = TextItem { pos: [c.x, c.y], color: self.main_rgb8(), balloon: Some(bal), ..self.text.template.clone() };
                self.new_text(item);
            }
            TextDrag::Pen => {
                let pen = std::mem::take(&mut self.text.pen);
                let r = Rect::from_points(&pen);
                // A scribble that encloses next to nothing isn't a balloon.
                let n = pen.len();
                let area = (0..n).map(|i| pen[i].x * pen[(i + 1) % n].y - pen[(i + 1) % n].x * pen[i].y).sum::<f32>().abs() / 2.0;
                if n < 6 || r.width() < 12.0 / zoom || r.height() < 12.0 / zoom || area < r.area() * 0.15 {
                    self.status = tr("Draw the balloon's outline round the words").into();
                    return;
                }
                let (c, half) = (r.center(), r.size() / 2.0);
                // A little smoothing, so the hand's jitter doesn't show.
                let n = pen.len();
                let points = (0..n)
                    .map(|i| {
                        let p = (pen[(i + n - 1) % n].to_vec2() + pen[i].to_vec2() * 2.0 + pen[(i + 1) % n].to_vec2()) / 4.0;
                        [(p.x - c.x) / half.x, (p.y - c.y) / half.y]
                    })
                    .collect();
                let bal = Balloon { shape: BalloonShape::Free, radii: [half.x, half.y], points, ..self.preset_balloon() };
                let item = TextItem { pos: [c.x, c.y], color: self.main_rgb8(), balloon: Some(bal), ..self.text.template.clone() };
                self.new_text(item);
            }
            TextDrag::NewTail { base } => {
                if base.distance(end) < 12.0 / zoom {
                    return;
                }
                let thought = self.text.balloon_tool == BalloonTool::ThoughtTail;
                let percent = self.text.preset().tail_width;
                let Some((id, _)) = self.text.editing else { return };
                self.next_label = Some("Add Balloon Tail".into());
                self.checkpoint();
                if let Some(b) = self.text_item_mut().and_then(|t| t.balloon.as_mut()) {
                    let width = b.tail_width(percent);
                    b.tails.push(Tail { base: [base.x, base.y], bend: None, tip: [end.x, end.y], width, thought });
                }
                self.redraw_text_layer(id);
            }
            _ => {}
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

    /// The edited text's box, its balloon's handles and its tails' handles.
    pub(crate) fn draw_text_overlay(&self, painter: &Painter, rect: Rect) {
        let doc = self.doc_size();
        let screen = |p: Pos2| self.view.doc_to_screen(rect, doc, p);
        let accent = crate::theme::accent();
        let thin = Stroke::new(1.0_f32, accent);
        match &self.text.drag {
            Some(TextDrag::NewBalloon { start }) => {
                // The sub tool's shape in the dragged box.
                let (c, r) = (*start + (self.text.pointer - *start) / 2.0, ((self.text.pointer - *start) / 2.0).abs());
                let b = Balloon { radii: [r.x.max(1.0), r.y.max(1.0)], ..self.text.preset().balloon.clone() };
                let outline = text::balloon_outline(&b, [c.x, c.y]).into_iter().map(|p| screen(pos(p))).collect();
                painter.add(Shape::closed_line(outline, thin));
            }
            Some(TextDrag::Pen) => {
                let line: Vec<Pos2> = self.text.pen.iter().map(|p| screen(*p)).collect();
                painter.add(Shape::line(line, Stroke::new(1.5_f32, accent)));
            }
            Some(TextDrag::NewTail { base }) => {
                painter.line_segment([screen(*base), screen(self.text.pointer)], Stroke::new(1.5_f32, accent));
            }
            _ => {}
        }
        let Some(t) = self.text_item() else { return };
        let b = text::bounds(&TextItem { balloon: None, ..t.clone() });
        let words = Rect::from_two_pos(screen(Pos2::new(b[0], b[1])), screen(Pos2::new(b[2], b[3])));
        let Some(bal) = &t.balloon else {
            painter.rect_stroke(words, 0.0, Stroke::new(3.0_f32, Color32::from_black_alpha(90)), egui::StrokeKind::Middle);
            painter.rect_stroke(words, 0.0, thin, egui::StrokeKind::Middle);
            return;
        };
        // The words' box, faint, inside the balloon's box with its handles (as Clip Studio).
        painter.rect_stroke(words, 0.0, Stroke::new(1.0_f32, accent.gamma_multiply(0.5)), egui::StrokeKind::Middle);
        let corners: Vec<Pos2> = text::balloon_box(bal, t.pos).iter().map(|p| screen(pos(*p))).collect();
        painter.add(Shape::closed_line(corners.clone(), Stroke::new(3.0_f32, Color32::from_black_alpha(70))));
        painter.add(Shape::closed_line(corners, Stroke::new(1.5_f32, accent)));
        let (handles, rotate) = balloon_handles(bal, t.pos, self.view.zoom);
        let top = screen(handles[1].1);
        let spin = screen(rotate);
        painter.line_segment([top, spin], thin);
        painter.circle_filled(spin, 4.5, Color32::WHITE);
        painter.circle_stroke(spin, 4.5, thin);
        for (_, p) in handles {
            painter.circle_filled(screen(p), 4.5, accent);
            painter.circle_stroke(screen(p), 4.5, Stroke::new(1.0_f32, Color32::WHITE));
        }
        for tail in &bal.tails {
            let (base, mid, tip) = (screen(pos(tail.base)), screen(pos(tail.middle())), screen(pos(tail.tip)));
            painter.add(Shape::dashed_line(&[base, mid, tip], Stroke::new(1.0_f32, accent), 4.0, 3.0));
            painter.circle_filled(tip, 5.0, Color32::WHITE);
            painter.circle_stroke(tip, 5.0, Stroke::new(1.5_f32, accent));
            let d = Vec2::splat(3.5);
            painter.rect_filled(Rect::from_center_size(mid, d * 2.0), 0.0, if tail.bend.is_some() { accent } else { Color32::WHITE });
            painter.rect_stroke(Rect::from_center_size(mid, d * 2.0), 0.0, thin, egui::StrokeKind::Middle);
            painter.circle_filled(base, 3.0, accent);
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
        let tail_width = self.text.presets[BalloonTool::ALL.iter().position(|t| *t == BalloonTool::Tail).unwrap_or(0)].tail_width;
        egui::Window::new(tr("Text"))
            .id(egui::Id::new("text_editor"))
            .open(&mut open)
            .resizable(true)
            .default_width(320.0)
            // Beside the canvas's right edge, clear of the tool settings.
            .default_pos(self.canvas_rect.right_top() + Vec2::new(-350.0, 12.0))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(ui.ctx().screen_rect().height() * 0.7).show(ui, |ui| {
                    let r = ui.add(
                        egui::TextEdit::multiline(&mut item.text).desired_rows(3).desired_width(f32::INFINITY).hint_text(tr("Type here")),
                    );
                    if self.text.drag.is_none() && !r.has_focus() && before.text.is_empty() {
                        r.request_focus();
                    }
                    ui.separator();
                    text_props_ui(ui, &mut item, &fonts, &mut self.text.font_filter, ink);
                    ui.separator();
                    balloon_edit_ui(ui, &mut item, tail_width);
                });
                ui.separator();
                ui.horizontal(|ui| {
                    done = ui.button(tr("Done")).clicked();
                    delete = ui.button(tr("Delete")).clicked();
                });
            });
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
            self.text.template = TextItem { text: String::new(), pos: [0.0, 0.0], balloon: None, ..item.clone() };
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
        if kind == TextKind::Balloon {
            self.balloon_tools_ui(ui);
            ui.separator();
            self.balloon_preset_ui(ui);
            ui.separator();
        }
        let mut template = self.text.template.clone();
        let mut show = |ui: &mut egui::Ui| {
            text_props_ui(ui, &mut template, &fonts, &mut self.text.font_filter, ink);
        };
        match kind {
            TextKind::Text => show(ui),
            TextKind::Balloon => {
                egui::CollapsingHeader::new(tr("Text")).id_salt("balloon_text_settings").show(ui, show);
            }
        }
        self.text.template = template;
        ui.small(match kind {
            TextKind::Text => "Click to write; click a text to edit it, drag it to move it. Esc ends.",
            _ if self.text.balloon_tool.is_tail() => "Drag from inside a balloon to where it points. Drag the square to bend it.",
            _ if self.text.balloon_tool == BalloonTool::Pen => "Draw the balloon's outline round the words, then write in it.",
            TextKind::Balloon => "Drag a balloon (or click), then write in it. Handles size and turn it.",
        });
        ui.small(tr("New texts take the main colour."));
    }

    /// The sub tool list, as Clip Studio's: a little drawing of each and its name.
    fn balloon_tools_ui(&mut self, ui: &mut egui::Ui) {
        for tool in BalloonTool::ALL {
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::click());
            let chosen = self.text.balloon_tool == tool;
            if chosen {
                ui.painter().rect_filled(rect, 3.0, crate::theme::selected());
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, 3.0, crate::theme::hover());
            }
            let ink = if chosen { crate::theme::strong() } else { crate::theme::text() };
            let icon = Rect::from_min_size(rect.min + Vec2::new(6.0, 4.0), Vec2::new(58.0, 18.0));
            draw_tool_icon(
                ui.painter(),
                icon,
                tool,
                &self.text.presets[BalloonTool::ALL.iter().position(|t| *t == tool).unwrap()],
                ink,
            );
            ui.painter().text(
                rect.right_center() - Vec2::new(8.0, 0.0),
                egui::Align2::RIGHT_CENTER,
                tool.name(),
                egui::FontId::proportional(13.0),
                ink,
            );
            if resp.clicked() {
                self.text.balloon_tool = tool;
            }
        }
    }

    /// The chosen sub tool's settings, in Clip Studio's order.
    fn balloon_preset_ui(&mut self, ui: &mut egui::Ui) {
        let tool = self.text.balloon_tool;
        let main = self.ink(Ink::Main, [0; 3]);
        let sub = self.ink(Ink::Sub, [0; 3]);
        let mut new_layer = self.text.new_layer;
        let p = self.text.preset_mut();
        if tool.is_tail() {
            ui.add(egui::Slider::new(&mut p.tail_width, 5.0..=150.0).text(tr("Width")).suffix(" %"))
                .on_hover_text(tr("At the base, as a share of the balloon's shorter half side"));
            ui.small(tr("Tails take the balloon's colours."));
            return;
        }
        egui::Grid::new("balloon_preset").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label(tr("Line color"));
            ink_row(ui, "line", &mut p.line, main, sub, |ui| {
                ui.color_edit_button_srgb(&mut p.balloon.line);
            });
            ui.end_row();
            ui.label(tr("Fill color"));
            ink_row(ui, "fill", &mut p.fill, main, sub, |ui| {
                ui.color_edit_button_srgba_unmultiplied(&mut p.balloon.fill);
            });
            ui.end_row();
            ui.label(tr("How to add"));
            egui::ComboBox::from_id_salt("balloon_add")
                .selected_text(if new_layer { "Add to new layer" } else { "Add to selected layer" })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut new_layer, true, tr("Add to new layer"));
                    ui.selectable_value(&mut new_layer, false, tr("Add to selected layer"));
                });
            ui.end_row();
            balloon_look_ui(ui, &mut p.balloon, tool != BalloonTool::Pen);
        });
        self.text.new_layer = new_layer;
    }
}

/// Main / Sub / own colour buttons for a balloon's line or fill.
fn ink_row(ui: &mut egui::Ui, salt: &str, ink: &mut Ink, main: [u8; 3], sub: [u8; 3], custom: impl FnOnce(&mut egui::Ui)) {
    ui.push_id(salt, |ui| {
        ui.horizontal(|ui| {
            for (which, rgb, tip) in [(Ink::Main, main, "Main colour"), (Ink::Sub, sub, "Sub colour")] {
                let (rect, r) = ui.allocate_exact_size(Vec2::new(26.0, 20.0), Sense::click());
                let chosen = *ink == which;
                ui.painter().rect_filled(rect.shrink(3.0), 2.0, Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
                let edge = if chosen { Stroke::new(2.0_f32, crate::theme::accent()) } else { Stroke::new(1.0_f32, crate::theme::line()) };
                ui.painter().rect_stroke(rect.shrink(1.0), 3.0, edge, egui::StrokeKind::Middle);
                if r.on_hover_text(tip).clicked() {
                    *ink = which;
                }
            }
            let r = ui.add(egui::Button::new(tr("Own")).selected(*ink == Ink::Custom)).on_hover_text(tr("A colour of its own"));
            if r.clicked() {
                *ink = Ink::Custom;
            }
            if *ink == Ink::Custom {
                custom(ui);
            }
        });
    });
}

/// Figure, corners, sides, line width, anti-aliasing and brush shape (grid rows).
fn balloon_look_ui(ui: &mut egui::Ui, b: &mut Balloon, figure: bool) {
    if figure {
        ui.label(tr("Figure"));
        ui.horizontal_wrapped(|ui| {
            for s in BalloonShape::ALL {
                ui.selectable_value(&mut b.shape, s, tr(s.name()));
            }
        });
        ui.end_row();
    }
    match b.shape {
        BalloonShape::Rectangle => {
            ui.label(tr("Roundness of corner"));
            ui.add(egui::Slider::new(&mut b.corner, 0.0..=100.0));
            ui.end_row();
        }
        BalloonShape::Polygon => {
            ui.label(tr("Sides"));
            ui.add(egui::Slider::new(&mut b.sides, 3..=16));
            ui.end_row();
        }
        _ => {}
    }
    ui.label(tr("Brush size"));
    ui.add(egui::Slider::new(&mut b.width, 0.0..=60.0).suffix(" px"));
    ui.end_row();
    ui.label(tr("Anti-aliasing"));
    ui.horizontal(|ui| {
        for a in Antialias::ALL {
            ui.selectable_value(&mut b.antialias, a, tr(a.name()));
        }
    });
    ui.end_row();
    ui.label(tr("Brush shape"));
    ui.horizontal(|ui| {
        let mut rough = b.rough > 0.0;
        egui::ComboBox::from_id_salt("balloon_brush").selected_text(if rough { "Rough" } else { "Smooth" }).show_ui(ui, |ui| {
            ui.selectable_value(&mut rough, false, tr("Smooth"));
            ui.selectable_value(&mut rough, true, tr("Rough"));
        });
        if rough != (b.rough > 0.0) {
            b.rough = if rough { 60.0 } else { 0.0 };
        }
        if rough {
            ui.add(egui::Slider::new(&mut b.rough, 1.0..=100.0).show_value(false)).on_hover_text(tr("How rough"));
        }
    });
    ui.end_row();
}

/// A little drawing of a sub tool, for its row in the list.
fn draw_tool_icon(painter: &Painter, r: Rect, tool: BalloonTool, preset: &BalloonPreset, ink: Color32) {
    let stroke = Stroke::new(1.3_f32, ink);
    let c = r.center();
    let at = |p: [f32; 2]| Pos2::new(p[0], p[1]);
    match tool {
        BalloonTool::Tail | BalloonTool::ThoughtTail => {
            let b = Balloon { radii: [16.0, 7.0], ..BalloonTool::Rounded.balloon() };
            let centre = [c.x - 10.0, c.y - 2.0];
            painter.add(Shape::closed_line(text::balloon_outline(&b, centre).into_iter().map(at).collect(), stroke));
            if tool == BalloonTool::Tail {
                painter.add(Shape::line(vec![c + Vec2::new(8.0, 0.0), c + Vec2::new(26.0, 8.0), c + Vec2::new(4.0, 4.0)], stroke));
            } else {
                for (k, rad) in [(0.0, 3.0), (1.0, 2.0), (2.0, 1.3)] {
                    painter.circle_stroke(c + Vec2::new(12.0 + k * 6.0, 4.0 + k * 2.0), rad, stroke);
                }
            }
        }
        BalloonTool::Pen => {
            let wave: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let t = i as f32 / 24.0;
                    Pos2::new(r.left() + 4.0 + t * (r.width() - 8.0), c.y + (t * 9.0).sin() * 3.0 - (t - 0.5) * 6.0)
                })
                .collect();
            painter.add(Shape::line(wave, Stroke::new(2.0_f32, ink)));
        }
        _ => {
            let b = Balloon { radii: [r.width() / 2.0 - 3.0, r.height() / 2.0 - 2.0], ..preset.balloon.clone() };
            let (scale, ry) = if b.shape == BalloonShape::Burst { (0.78, 0.75) } else { (1.0, 1.0) };
            let b = Balloon { radii: [b.radii[0] * scale, b.radii[1] * ry], ..b };
            let outline: Vec<Pos2> = text::balloon_outline(&b, [c.x, c.y]).into_iter().map(at).collect();
            painter.add(Shape::closed_line(outline, Stroke::new(if b.rough > 0.0 { 2.0_f32 } else { 1.3 }, ink)));
        }
    }
}

/// Font, size, colour, alignment, direction and outline of `item`.
fn text_props_ui(ui: &mut egui::Ui, item: &mut TextItem, fonts: &[String], filter: &mut String, ink: [u8; 3]) {
    let shown = if item.font.is_empty() { "Built-in".to_string() } else { item.font.clone() };
    let missing = !text::has_font(&item.font);
    ui.horizontal(|ui| {
        ui.label(tr("Font"));
        egui::ComboBox::from_id_salt("text_font")
            .selected_text(if missing { format!("{shown} (missing)") } else { shown })
            .width(200.0)
            .show_ui(ui, |ui| {
                ui.add(egui::TextEdit::singleline(filter).hint_text(tr("Search fonts")));
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
        ui.small(tr("Looking for the fonts on this computer…"));
    }
    ui.horizontal(|ui| {
        ui.add(egui::DragValue::new(&mut item.size).range(4.0..=2000.0).speed(0.5).max_decimals(1).suffix(" px"));
        ui.color_edit_button_srgb(&mut item.color);
        if ui.small_button(tr("Main colour")).clicked() {
            item.color = ink;
        }
    });
    ui.horizontal(|ui| {
        ui.selectable_value(&mut item.align, TextAlign::Left, tr("Left"));
        ui.selectable_value(&mut item.align, TextAlign::Center, tr("Centre"));
        ui.selectable_value(&mut item.align, TextAlign::Right, tr("Right"));
        ui.checkbox(&mut item.vertical, tr("Vertical"));
    });
    ui.add(egui::Slider::new(&mut item.line_spacing, 0.6..=3.0).text(tr("Line spacing")));
    ui.add(egui::Slider::new(&mut item.tracking, -20.0..=80.0).text(tr("Letter spacing")).suffix(" px"));
    ui.horizontal(|ui| {
        ui.add(egui::Slider::new(&mut item.outline, 0.0..=30.0).text(tr("Outline")).suffix(" px"));
        ui.color_edit_button_srgb(&mut item.outline_color);
    });
}

/// The edited text's balloon: its look, size, turn and tails.
fn balloon_edit_ui(ui: &mut egui::Ui, item: &mut TextItem, tail_width: f32) {
    let mut has_balloon = item.balloon.is_some();
    if ui.checkbox(&mut has_balloon, tr("Balloon")).changed() {
        item.balloon = has_balloon.then(|| {
            let b = text::bounds(&TextItem { balloon: None, ..item.clone() });
            let r = [((b[2] - b[0]) * 0.75).max(item.size * 2.0), ((b[3] - b[1]) * 0.85).max(item.size * 1.4)];
            Balloon { radii: r, ..BalloonTool::Rounded.balloon() }
        });
    }
    // The words alone, for Fit to Text.
    let words = text::bounds(&TextItem { balloon: None, ..item.clone() });
    let centre = item.pos;
    let Some(b) = &mut item.balloon else { return };
    egui::Grid::new("balloon_edit").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
        ui.label(tr("Line color"));
        ui.color_edit_button_srgb(&mut b.line);
        ui.end_row();
        ui.label(tr("Fill color"));
        ui.color_edit_button_srgba_unmultiplied(&mut b.fill);
        ui.end_row();
        balloon_look_ui(ui, b, b.shape != BalloonShape::Free);
        ui.label(tr("Width"));
        ui.add(egui::Slider::new(&mut b.radii[0], 10.0..=3000.0).logarithmic(true).suffix(" px"));
        ui.end_row();
        ui.label(tr("Height"));
        ui.add(egui::Slider::new(&mut b.radii[1], 10.0..=3000.0).logarithmic(true).suffix(" px"));
        ui.end_row();
        ui.label(tr("Rotation"));
        let mut deg = b.rotation.to_degrees();
        if ui.add(egui::Slider::new(&mut deg, -180.0..=180.0).suffix("°").max_decimals(1)).changed() {
            b.rotation = deg.to_radians();
        }
        ui.end_row();
    });
    if ui.button(tr("Fit to Text")).on_hover_text(tr("Size the balloon round the words")).clicked() {
        let t = words;
        b.radii = [((t[2] - t[0]) * 0.72).max(20.0), ((t[3] - t[1]) * 0.8).max(15.0)];
    }
    ui.label(tr("Tails"));
    let mut remove = None;
    for (i, tail) in b.tails.iter_mut().enumerate() {
        ui.push_id(i, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut tail.width).range(1.0..=2000.0).speed(0.5).max_decimals(0).suffix(" px"));
                ui.checkbox(&mut tail.thought, tr("Thought"));
                if tail.bend.is_some() && ui.small_button(tr("Straighten")).clicked() {
                    tail.bend = None;
                }
                if ui.small_button(tr("Remove")).clicked() {
                    remove = Some(i);
                }
            });
        });
    }
    if let Some(i) = remove {
        b.tails.remove(i);
    }
    if ui.button(tr("Add Tail")).on_hover_text(tr("Or drag one out of the balloon with the Balloon tail tool")).clicked() {
        let width = b.tail_width(tail_width);
        let r = b.radii;
        b.tails.push(Tail {
            base: [centre[0], centre[1] + r[1] * 0.5],
            bend: None,
            tip: [centre[0] - r[0] * 0.3, centre[1] + r[1] * 1.6],
            width,
            thought: false,
        });
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
    /// For screenshots (`dev.text_demo`): a caption, a rough speech balloon with
    /// a curved tail, a thought balloon with its bubbles, a flash and a vertical
    /// line of text, the speech balloon left selected with its handles.
    pub(crate) fn text_demo(&mut self) {
        let (w, h) = self.doc.canvas();
        let (w, h) = (w as f32, h as f32);
        let font = text::font_names()
            .into_iter()
            .find(|n| n == "Comic Sans MS" || n == "Arial" || n.starts_with("DejaVu Sans"))
            .unwrap_or_default();
        // Sized by the short side, so it fits portrait pages and landscape pictures alike.
        let u = w.min(h);
        let line = u / 300.0;
        let base = TextItem { font, size: u / 22.0, ..Default::default() };
        self.new_text(TextItem {
            text: "Meanwhile, in the city…".into(),
            pos: [w * 0.5, h * 0.07],
            outline: u / 250.0,
            ..base.clone()
        });
        self.finish_text();
        self.new_text(TextItem {
            text: "Shout!".into(),
            pos: [w * 0.78, h * 0.72],
            size: u / 14.0,
            balloon: Some(Balloon { radii: [u * 0.2, u * 0.13], width: line, ..BalloonTool::Flash.balloon() }),
            ..base.clone()
        });
        self.finish_text();
        let (tx, ty) = (w * 0.76, h * 0.36);
        self.new_text(TextItem {
            text: "Hmm…".into(),
            pos: [tx, ty],
            balloon: Some(Balloon {
                radii: [u * 0.17, u * 0.1],
                width: line,
                tails: vec![Tail { base: [tx, ty], bend: None, tip: [w * 0.6, h * 0.52], width: u * 0.06, thought: true }],
                ..BalloonTool::Thought.balloon()
            }),
            ..base.clone()
        });
        self.finish_text();
        self.new_text(TextItem { text: "縦書き\nTEST".into(), vertical: true, pos: [w * 0.08, h * 0.62], ..base.clone() });
        self.finish_text();
        let (cx, cy) = (w * 0.33, h * 0.3);
        self.new_text(TextItem {
            text: "Hello!\nCan you hear me?".into(),
            pos: [cx, cy],
            balloon: Some(Balloon {
                radii: [u * 0.24, u * 0.14],
                width: line * 1.3,
                tails: vec![Tail {
                    base: [cx, cy + u * 0.06],
                    bend: Some([w * 0.36, h * 0.47]),
                    tip: [w * 0.26, h * 0.56],
                    width: u * 0.07,
                    thought: false,
                }],
                ..BalloonTool::Rough.balloon()
            }),
            ..base
        });
        self.tool = Tool::Text(TextKind::Balloon);
        self.text.balloon_tool = BalloonTool::Rough;
    }
}
