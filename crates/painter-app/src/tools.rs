//! Canvas tools other than the brush, their options, overlays and keyboard shortcuts.

use eframe::egui::{self, Color32, Key, KeyboardShortcut, Modifiers, Painter, Pos2, Rect, Stroke};
use painter_core::fill_tool::{FillOptions, ReferenceOptions};
use painter_core::{selection, GradientShape, SelectionOp};

use painter_core::{BrushMode, BrushSettings};

use crate::app::PainterApp;
use crate::icons::Icon;
use crate::stamp_tool::StampKind;
use crate::text_tool::TextKind;

/// Holding a tool key this long makes the switch temporary (released: back to the tool before).
pub(crate) const SPRING_HOLD: std::time::Duration = std::time::Duration::from_millis(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Pen, pencil, airbrush and erasers (the sub tool decides).
    Brush,
    Fill,
    Gradient,
    Select(SelectShape),
    /// Scale, rotate, skew, perspective, free distort and mesh warp of the layer or selection.
    Transform,
    Crop,
    /// Comic panel frames: draw, divide and reshape.
    Frame,
    /// 3D layers: navigate, pick and transform objects.
    Scene3D,
    /// Picks a colour from the canvas or the current layer.
    Eyedropper,
    /// Perspective, symmetry and special rulers, guides: place and edit them.
    Ruler,
    /// Vector layers: pick a line, move it, reshape it by its control points.
    VectorEdit,
    /// Rectangles, ellipses, lines and polygons, outlined with the brush or filled.
    Shape,
    /// The clone and pattern stamps and the history brush: the brush's tip, colours from elsewhere.
    Stamp(crate::stamp_tool::StampKind),
    /// Text and balloons on vector layers.
    Text(crate::text_tool::TextKind),
    Hand,
    /// Zoom: a click zooms in, Alt + click out; a drag right or left zooms in or out (Clip Studio's).
    Zoom,
    /// Rotates the view: drag round the canvas centre.
    Rotate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectShape {
    Rect,
    Ellipse,
    Lasso,
    /// Clip Studio's Auto Select (a magic wand): the area a fill would cover.
    Magic,
    /// Photoshop's magnetic lasso: a lasso that snaps to edges.
    Magnetic,
    /// Photoshop's object selection: what stands out inside a dragged rectangle.
    Object,
}

impl Tool {
    pub const ALL: [Tool; 25] = [
        Tool::Brush,
        Tool::Stamp(StampKind::Clone),
        Tool::Stamp(StampKind::Pattern),
        Tool::Stamp(StampKind::History),
        Tool::Fill,
        Tool::Gradient,
        Tool::Shape,
        Tool::Text(TextKind::Text),
        Tool::Text(TextKind::Balloon),
        Tool::Eyedropper,
        Tool::Select(SelectShape::Rect),
        Tool::Select(SelectShape::Ellipse),
        Tool::Select(SelectShape::Lasso),
        Tool::Select(SelectShape::Magnetic),
        Tool::Select(SelectShape::Magic),
        Tool::Select(SelectShape::Object),
        Tool::Transform,
        Tool::Crop,
        Tool::Frame,
        Tool::Scene3D,
        Tool::Ruler,
        Tool::VectorEdit,
        Tool::Hand,
        Tool::Zoom,
        Tool::Rotate,
    ];

    /// The tools by command id (`tool.N`): only ever appended to, so saved
    /// toolbar buttons and recent commands keep pointing at the same tool.
    pub const COMMANDS: [Tool; 25] = [
        Tool::Brush,
        Tool::Fill,
        Tool::Gradient,
        Tool::Eyedropper,
        Tool::Select(SelectShape::Rect),
        Tool::Select(SelectShape::Ellipse),
        Tool::Select(SelectShape::Lasso),
        Tool::Select(SelectShape::Magic),
        Tool::Transform,
        Tool::Crop,
        Tool::Frame,
        Tool::Scene3D,
        Tool::Ruler,
        Tool::VectorEdit,
        Tool::Hand,
        Tool::Shape,
        Tool::Stamp(StampKind::Clone),
        Tool::Stamp(StampKind::Pattern),
        Tool::Stamp(StampKind::History),
        Tool::Select(SelectShape::Magnetic),
        Tool::Select(SelectShape::Object),
        Tool::Text(TextKind::Text),
        Tool::Text(TextKind::Balloon),
        Tool::Zoom,
        Tool::Rotate,
    ];

    pub fn icon(self) -> Icon {
        match self {
            Tool::Brush => Icon::Paintbrush,
            Tool::Fill => Icon::Bucket,
            Tool::Gradient => Icon::Gradient,
            Tool::Select(SelectShape::Rect) => Icon::RectSelect,
            Tool::Select(SelectShape::Ellipse) => Icon::EllipseSelect,
            Tool::Select(SelectShape::Lasso) => Icon::FreeSelect,
            Tool::Select(SelectShape::Magic) => Icon::MagicSelect,
            Tool::Select(SelectShape::Magnetic) => Icon::MagneticLasso,
            Tool::Select(SelectShape::Object) => Icon::ObjectSelect,
            Tool::Transform => Icon::Transform,
            Tool::Crop => Icon::Crop,
            Tool::Frame => Icon::Grid,
            Tool::Scene3D => Icon::Transform3d,
            Tool::Ruler => Icon::Path,
            Tool::Eyedropper => Icon::ColorPicker,
            Tool::VectorEdit => Icon::Curve,
            Tool::Shape => Icon::Shapes,
            Tool::Text(TextKind::Text) => Icon::Text,
            Tool::Text(TextKind::Balloon) => Icon::Balloon,
            Tool::Stamp(StampKind::Clone) => Icon::Clone,
            Tool::Stamp(StampKind::Pattern) => Icon::PatternStamp,
            Tool::Stamp(StampKind::History) => Icon::HistoryBrush,
            Tool::Hand => Icon::Move,
            Tool::Zoom => Icon::Zoom,
            Tool::Rotate => Icon::RotateRight,
        }
    }

    /// Id used by workspaces to pick which tools their toolbar shows.
    pub fn id(self) -> &'static str {
        match self {
            Tool::Brush => "brush",
            Tool::Fill => "fill",
            Tool::Gradient => "gradient",
            Tool::Select(_) => "select",
            Tool::Transform => "transform",
            Tool::Crop => "crop",
            Tool::Frame => "frame",
            Tool::Scene3D => "scene3d",
            Tool::Ruler => "ruler",
            Tool::Eyedropper => "eyedropper",
            Tool::VectorEdit => "vector_edit",
            Tool::Shape => "shape",
            Tool::Text(TextKind::Text) => "text",
            Tool::Text(TextKind::Balloon) => "balloon",
            Tool::Stamp(StampKind::Clone) => "clone",
            Tool::Stamp(StampKind::Pattern) => "pattern_stamp",
            Tool::Stamp(StampKind::History) => "history_brush",
            Tool::Hand => "hand",
            Tool::Zoom => "zoom",
            Tool::Rotate => "rotate",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Tool::Brush => "Brush",
            Tool::Fill => "Fill Bucket",
            Tool::Gradient => "Gradient",
            Tool::Select(SelectShape::Rect) => "Rectangle Selection",
            Tool::Select(SelectShape::Ellipse) => "Ellipse Selection",
            Tool::Select(SelectShape::Lasso) => "Lasso Selection",
            Tool::Select(SelectShape::Magic) => "Auto Select",
            Tool::Select(SelectShape::Magnetic) => "Magnetic Lasso",
            Tool::Select(SelectShape::Object) => "Object Selection",
            Tool::Transform => "Transform",
            Tool::Crop => "Crop / Resize Canvas",
            Tool::Frame => "Panel Frames",
            Tool::Scene3D => "3D Layer Tool",
            Tool::Ruler => "Rulers and Guides",
            Tool::Eyedropper => "Eyedropper",
            Tool::VectorEdit => "Vector Points",
            Tool::Shape => "Shape",
            Tool::Text(TextKind::Text) => "Text",
            Tool::Text(TextKind::Balloon) => "Balloon",
            Tool::Stamp(StampKind::Clone) => "Clone Stamp",
            Tool::Stamp(StampKind::Pattern) => "Pattern Stamp",
            Tool::Stamp(StampKind::History) => "History Brush",
            Tool::Hand => "Hand (pan)",
            Tool::Zoom => "Zoom",
            Tool::Rotate => "Rotate View",
        }
    }

    pub fn shortcut(self) -> Option<&'static str> {
        Some(match self {
            Tool::Brush => "B",
            Tool::Fill => "G",
            Tool::Gradient => "Shift+G",
            Tool::Select(SelectShape::Rect) => "M",
            Tool::Select(SelectShape::Ellipse) => "Shift+M",
            Tool::Select(SelectShape::Lasso) => "L",
            Tool::Select(SelectShape::Magic) => "W",
            Tool::Select(SelectShape::Magnetic) => "Shift+L",
            Tool::Select(SelectShape::Object) => "Shift+W",
            Tool::Transform => "Ctrl+T",
            Tool::Crop => "Shift+C",
            Tool::Frame => "U",
            Tool::Scene3D | Tool::Ruler => return None,
            Tool::Eyedropper => "I",
            Tool::VectorEdit => "O",
            Tool::Shape => "Shift+U",
            Tool::Text(TextKind::Text) => "T",
            Tool::Text(TextKind::Balloon) => "Shift+T",
            Tool::Stamp(StampKind::Clone) => "S",
            Tool::Stamp(StampKind::Pattern) => "Shift+S",
            Tool::Stamp(StampKind::History) => "Y",
            Tool::Hand => "H",
            Tool::Zoom => "/",
            Tool::Rotate => "R",
        })
    }

    pub fn hint(self) -> &'static str {
        match self {
            Tool::Brush => "Brush / eraser (B, E)",
            Tool::Fill => "Fill bucket (G)",
            Tool::Gradient => "Gradient: drag on the canvas (Shift+G)",
            Tool::Select(SelectShape::Rect) => "Rectangle selection (M). Shift adds, Alt subtracts",
            Tool::Select(SelectShape::Ellipse) => "Ellipse selection (Shift+M)",
            Tool::Select(SelectShape::Lasso) => "Lasso selection (L)",
            Tool::Select(SelectShape::Magic) => "Auto select (W): click an area to select what a fill would cover",
            Tool::Select(SelectShape::Magnetic) => "Magnetic lasso (Shift+L): click, then move along an outline; Enter or a click on the start closes",
            Tool::Select(SelectShape::Object) => "Object selection (Shift+W): drag a rectangle round an object",
            Tool::Transform => "Transform the layer or selection (Ctrl+T). Enter applies, Esc cancels",
            Tool::Crop => "Crop / resize the canvas (Shift+C). Enter applies, Esc cancels",
            Tool::Frame => "Panel frames (U): draw rectangles or polygons, divide panels, reshape them",
            Tool::Scene3D => "3D layer: middle drag orbits, Shift pans, wheel zooms; click picks, G / R / S gizmo",
            Tool::Ruler => "Rulers: perspective, symmetry, special rulers and guides. Drag their handles; Ctrl+1 toggles snapping",
            Tool::Eyedropper => "Eyedropper (I): pick a colour from the canvas or the current layer. Alt with a brush, or hold a finger still on an iPad",
            Tool::VectorEdit => "Vector points (O): pick a vector line, drag its control points to reshape it, drag the line to move it",
            Tool::Shape => "Shapes (Shift+U): rectangles, ellipses, lines and polygons. Shift constrains, Alt draws from the centre",
            Tool::Stamp(StampKind::Clone) => "Clone stamp (S): Alt-click where to copy from, then paint with the current brush's tip",
            Tool::Stamp(StampKind::Pattern) => "Pattern stamp (Shift+S): paint a pattern with the current brush's tip",
            Tool::Stamp(StampKind::History) => "History brush (Y): paint back an earlier state (pick it in the History panel)",
            Tool::Text(TextKind::Text) => "Text (T): click to write, click a text to edit it, drag it to move it",
            Tool::Text(TextKind::Balloon) => "Balloon (Shift+T): balloons, the balloon pen and tails, as Clip Studio's; drag one, then write in it",
            Tool::Hand => "Pan the canvas (H, or hold Space)",
            Tool::Zoom => "Zoom (/): click zooms in, Alt + click zooms out, drag right or left. Hold Ctrl + Space or Alt + Space for a moment",
            Tool::Rotate => "Rotate the view (R): drag round the centre. Shift + Space + drag or Shift + wheel for a moment; - and ^ turn it",
        }
    }
}

/// Toolbar buckets for brush presets, like Clip Studio's tool groups. A
/// preset's group comes from its mode and name (see [`BrushGroup::of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BrushGroup {
    Pen,
    Pencil,
    Brush,
    Airbrush,
    Eraser,
}

impl BrushGroup {
    pub const ALL: [BrushGroup; 5] =
        [BrushGroup::Pen, BrushGroup::Pencil, BrushGroup::Brush, BrushGroup::Airbrush, BrushGroup::Eraser];

    pub fn label(self) -> &'static str {
        match self {
            BrushGroup::Pen => "Pen",
            BrushGroup::Pencil => "Pencil",
            BrushGroup::Brush => "Brush",
            BrushGroup::Airbrush => "Airbrush",
            BrushGroup::Eraser => "Eraser",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            BrushGroup::Pen => Icon::Ink,
            BrushGroup::Pencil => Icon::Pencil,
            BrushGroup::Brush => Icon::Paintbrush,
            BrushGroup::Airbrush => Icon::Airbrush,
            BrushGroup::Eraser => Icon::Eraser,
        }
    }

    /// A pinned group first (see `BrushSettings::group`); then erasers by mode,
    /// the rest by keywords in the name, defaulting to Brush.
    pub fn of(settings: &BrushSettings) -> BrushGroup {
        if let Some(pinned) = BrushGroup::ALL.into_iter().find(|g| settings.group.as_deref() == Some(crate::toolbar::group_key(*g))) {
            return pinned;
        }
        if settings.mode == BrushMode::Erase {
            return BrushGroup::Eraser;
        }
        let name = settings.name.to_lowercase();
        let has = |words: &[&str]| words.iter().any(|w| name.contains(w));
        if has(&["airbrush", "spray", "soft", "glitter", "noise"]) {
            BrushGroup::Airbrush
        } else if has(&["pencil", "crayon", "pastel", "charcoal", "chalk"]) {
            BrushGroup::Pencil
        } else if has(&["pen", "ink", "marker", "liner", "nib"]) {
            BrushGroup::Pen
        } else {
            BrushGroup::Brush
        }
    }
}

pub struct ToolOptions {
    pub fill: FillOptions,
    /// Where the fill bucket reads its boundaries.
    pub fill_reference: ReferenceOptions,
    /// Auto Select: like the fill, without expanding under the lines.
    pub magic: FillOptions,
    pub gradient_shape: GradientShape,
    /// Gradients fade from the main colour to transparent instead of to the sub colour.
    pub gradient_to_transparent: bool,
    pub selection_op: SelectionOp,
    /// Pastes and copies between documents keep their size on paper when the resolutions differ.
    pub paste_match_dpi: bool,
    /// Holding the pen still at the end of a stroke turns it into a clean shape.
    pub smart_shape: bool,
    /// What the eraser takes off vector layers.
    pub vector_erase: VectorErase,
    /// Only a pen draws; fingers move the view (the default on iPad).
    pub pencil_only: bool,
    /// The Shape tool's kind and style.
    pub shapes: crate::shape_tool::ShapeOptions,
    /// The stamps' options and patterns.
    pub stamps: crate::stamp_tool::StampOptions,
    /// The magnetic lasso's and object selection's options.
    pub magnetic: crate::magnetic::MagneticOptions,
}

impl Default for ToolOptions {
    fn default() -> Self {
        Self {
            fill: FillOptions::default(),
            fill_reference: ReferenceOptions::default(),
            magic: FillOptions { expand: 0, ..FillOptions::default() },
            gradient_shape: GradientShape::Linear,
            gradient_to_transparent: true,
            selection_op: SelectionOp::Replace,
            paste_match_dpi: true,
            vector_erase: VectorErase::ToIntersection,
            smart_shape: true,
            pencil_only: cfg!(target_os = "ios"),
            shapes: Default::default(),
            stamps: Default::default(),
            magnetic: Default::default(),
        }
    }
}

/// An in-progress drag of the gradient or a selection tool, in canvas pixels.
pub struct ToolDrag {
    pub start: Pos2,
    pub current: Pos2,
    pub points: Vec<Pos2>,
    pub op: SelectionOp,
}

impl PainterApp {
    fn selection_op_for(&self, modifiers: Modifiers) -> SelectionOp {
        match (modifiers.shift, modifiers.alt) {
            (true, true) => SelectionOp::Intersect,
            (true, false) => SelectionOp::Add,
            (false, true) => SelectionOp::Subtract,
            (false, false) => self.tools.selection_op,
        }
    }

    /// Primary press on the canvas with a tool other than the brush or hand.
    pub(crate) fn tool_press(&mut self, at: Pos2, modifiers: Modifiers) {
        match self.tool {
            Tool::Fill => self.fill_at(at.x, at.y),
            Tool::Select(SelectShape::Magic) => {
                let op = self.selection_op_for(modifiers);
                self.magic_select_at(at, op);
            }
            Tool::Select(SelectShape::Magnetic) => {
                let op = self.selection_op_for(modifiers);
                let double = self.egui_ctx.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary));
                self.magnetic_press(at, op, double);
            }
            Tool::Gradient | Tool::Select(_) => {
                let op = self.selection_op_for(modifiers);
                self.drag = Some(ToolDrag { start: at, current: at, points: vec![at], op });
            }
            Tool::Transform => self.transform_press(at),
            Tool::Crop => self.crop_press(at),
            Tool::Frame => self.frame_press(at),
            Tool::Scene3D => self.scene_press(at),
            Tool::Ruler => self.ruler_press(at),
            Tool::VectorEdit => self.vector_edit_press(at, modifiers.alt),
            Tool::Text(kind) => {
                self.text_press(at, kind);
                if self.text_dragging() {
                    self.drag = Some(ToolDrag { start: at, current: at, points: vec![at], op: SelectionOp::Replace });
                }
            }
            Tool::Shape => {
                let double = self.egui_ctx.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary));
                self.shape_press(at, double);
                if self.shape.start.is_some() {
                    self.drag = Some(ToolDrag { start: at, current: at, points: vec![at], op: SelectionOp::Replace });
                }
            }
            Tool::Eyedropper => {
                self.eyedrop_at(at);
                self.drag = Some(ToolDrag { start: at, current: at, points: vec![at], op: SelectionOp::Replace });
            }
            Tool::Brush | Tool::Stamp(_) | Tool::Hand | Tool::Zoom | Tool::Rotate => {}
        }
    }

    pub(crate) fn tool_move(&mut self, at: Pos2) {
        // Shift with the gradient: its direction in 15° steps.
        let at = match &self.drag {
            Some(drag) if self.tool == Tool::Gradient && self.pointer_mods.shift => crate::lines::snap_angle(drag.start, at, 15.0),
            _ => at,
        };
        match self.tool {
            Tool::Transform => self.transform_move(at),
            Tool::Crop => self.crop_move(at),
            Tool::Frame => self.frame_move(at),
            Tool::Scene3D => self.scene_move(at),
            Tool::Ruler => self.ruler_move(at),
            Tool::VectorEdit => self.vector_edit_move(at),
            Tool::Shape => self.shape_move(at),
            Tool::Text(_) => self.text_move(at),
            Tool::Eyedropper => self.eyedrop_at(at),
            _ => {}
        }
        if let Some(drag) = &mut self.drag {
            drag.current = at;
            if drag.points.last().is_none_or(|p| p.distance(at) >= 1.0) {
                drag.points.push(at);
            }
        }
    }

    pub(crate) fn tool_release(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        match self.tool {
            Tool::Transform => return self.transform_release(),
            Tool::Crop => return self.crop_release(),
            Tool::Frame => return self.frame_release(),
            Tool::Scene3D => return self.scene_release(),
            Tool::Ruler => return self.ruler_release(),
            Tool::VectorEdit => return self.vector_edit_release(),
            Tool::Shape => return self.shape_release(),
            Tool::Text(_) => return self.text_release(),
            _ => {}
        }
        let canvas = self.doc.canvas();
        let moved = drag.start.distance(drag.current) >= 2.0;
        let (a, b) = ([drag.start.x, drag.start.y], [drag.current.x, drag.current.y]);
        match self.tool {
            Tool::Gradient if moved => self.apply_gradient(a, b),
            Tool::Select(shape) => {
                if !moved && shape != SelectShape::Lasso {
                    // A click without dragging clears a replacing selection.
                    if drag.op == SelectionOp::Replace {
                        self.deselect();
                    }
                    return;
                }
                let mask = match shape {
                    SelectShape::Rect => selection::rect_mask(a, b, canvas),
                    SelectShape::Ellipse => selection::ellipse_mask(a, b, canvas),
                    // Auto Select and the magnetic lasso work on presses.
                    SelectShape::Magic | SelectShape::Magnetic => return,
                    SelectShape::Object => return self.select_object(a, b, drag.op),
                    SelectShape::Lasso => {
                        let points: Vec<[f32; 2]> = drag.points.iter().map(|p| [p.x, p.y]).collect();
                        selection::polygon_mask(&points, canvas)
                    }
                };
                self.combine_selection(mask, drag.op);
            }
            _ => {}
        }
    }

    /// Outlines of the in-progress drag, in screen space.
    pub(crate) fn draw_tool_overlay(&self, painter: &Painter, rect: Rect) {
        self.draw_transform_overlay(painter, rect);
        self.draw_crop_overlay(painter, rect);
        self.draw_comic_overlay(painter, rect);
        self.draw_scene_overlay(painter, rect);
        self.draw_uv_overlay(painter, rect);
        let Some(drag) = &self.drag else {
            return;
        };
        let doc = self.doc_size();
        let to_screen = |p: Pos2| self.view.doc_to_screen(rect, doc, p);
        let outline = |points: Vec<Pos2>, closed: bool| {
            let shape = |stroke| {
                if closed {
                    egui::Shape::closed_line(points.clone(), stroke)
                } else {
                    egui::Shape::line(points.clone(), stroke)
                }
            };
            painter.add(shape(Stroke::new(3.0_f32, Color32::from_black_alpha(160))));
            painter.add(shape(Stroke::new(1.0_f32, Color32::WHITE)));
        };
        let (s, c) = (drag.start, drag.current);
        match self.tool {
            Tool::Gradient => {
                outline(vec![to_screen(s), to_screen(c)], false);
                painter.circle_stroke(to_screen(s), 4.0, Stroke::new(1.5_f32, Color32::WHITE));
                painter.circle_filled(to_screen(c), 3.0, Color32::WHITE);
            }
            Tool::Select(SelectShape::Rect | SelectShape::Object) => {
                let corners = [s, Pos2::new(c.x, s.y), c, Pos2::new(s.x, c.y)];
                outline(corners.into_iter().map(to_screen).collect(), true);
            }
            Tool::Select(SelectShape::Ellipse) => {
                let center = Pos2::new((s.x + c.x) * 0.5, (s.y + c.y) * 0.5);
                let (rx, ry) = ((c.x - s.x).abs() * 0.5, (c.y - s.y).abs() * 0.5);
                let points = (0..64)
                    .map(|i| {
                        let t = i as f32 / 64.0 * std::f32::consts::TAU;
                        to_screen(Pos2::new(center.x + rx * t.cos(), center.y + ry * t.sin()))
                    })
                    .collect();
                outline(points, true);
            }
            Tool::Select(SelectShape::Lasso) => {
                outline(drag.points.iter().copied().map(to_screen).collect(), true);
            }
            _ => {}
        }
    }

    pub(crate) fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        self.scene_shortcuts(ctx);
        self.channel_shortcuts(ctx);
        self.ruler_shortcuts(ctx);
        self.transform_shortcuts(ctx);
        self.frame_shortcuts(ctx);
        let shortcut = |mods: Modifiers, key: Key| KeyboardShortcut::new(mods, key);
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        // Shift variants first: shortcut matching ignores an extra Shift.
        let mut quit_requested = false;
        let pressed = ctx.input_mut(|i| {
            let redo = i.consume_shortcut(&shortcut(cmd_shift, Key::Z)) || i.consume_shortcut(&shortcut(Modifiers::COMMAND, Key::Y));
            let save_as = i.consume_shortcut(&shortcut(cmd_shift, Key::S));
            let invert = i.consume_shortcut(&shortcut(cmd_shift, Key::I));
            let quit = i.consume_shortcut(&shortcut(Modifiers::COMMAND, Key::Q));
            if quit {
                quit_requested = true;
            }
            // Before Ctrl+N, which would also match it.
            let new_from_clipboard = i.consume_shortcut(&shortcut(Modifiers::COMMAND | Modifiers::ALT, Key::N));
            [
                redo,
                i.consume_shortcut(&shortcut(Modifiers::COMMAND, Key::Z)),
                save_as,
                i.consume_shortcut(&shortcut(Modifiers::COMMAND, Key::S)),
                i.consume_shortcut(&shortcut(Modifiers::COMMAND, Key::O)),
                i.consume_shortcut(&shortcut(Modifiers::COMMAND, Key::N)),
                i.consume_shortcut(&shortcut(Modifiers::COMMAND, Key::A)),
                i.consume_shortcut(&shortcut(Modifiers::COMMAND, Key::D)),
                invert,
                new_from_clipboard,
            ]
        });
        let [redo, undo, save_as, save, open, new, all, deselect, invert, new_from_clipboard] = pressed;
        if quit_requested && !cfg!(target_os = "ios") {
            self.quit_requested = true;
        }
        // Layers: Ctrl+J duplicates, Ctrl+G groups into a folder (as in Photoshop).
        let (duplicate, group) = ctx.input_mut(|i| {
            (i.consume_shortcut(&shortcut(Modifiers::COMMAND, Key::J)), i.consume_shortcut(&shortcut(Modifiers::COMMAND, Key::G)))
        });
        if duplicate {
            self.duplicate_selected_layers();
        }
        // Ctrl+P: File → Print.
        if ctx.input_mut(|i| i.consume_shortcut(&shortcut(Modifiers::COMMAND, Key::P))) {
            self.open_print();
        }
        // Ctrl+R: the ruler bar (as in Clip Studio).
        if ctx.input_mut(|i| i.consume_shortcut(&shortcut(Modifiers::COMMAND, Key::R))) {
            self.ws.chrome.ruler_bar = !self.ws.chrome.ruler_bar;
        }
        if group {
            self.group_selected_layers();
        }
        if redo {
            self.redo();
        } else if undo {
            self.undo();
        }
        if save_as {
            self.save_as();
        } else if save {
            self.save();
        }
        if open {
            self.open_dialog();
        }
        if new_from_clipboard {
            self.new_from_clipboard();
        } else if new {
            self.open_new_canvas();
        }
        if all {
            self.select_all();
        }
        if deselect {
            self.deselect();
        }
        if invert {
            self.invert_selection();
        }

        let key = |key: Key, shift: bool| {
            ctx.input(|i| i.key_pressed(key) && !i.modifiers.command && !i.modifiers.alt && i.modifiers.shift == shift)
        };
        if key(Key::B, false) {
            self.select_brush(self.last_paint_brush);
        }
        // E: the eraser. Held (longer than a tap), it's only on until released,
        // then the tool before comes back (Clip Studio's temporary tool switch).
        if key(Key::E, false) {
            let on_eraser = self.tool == Tool::Brush && self.brush == self.last_eraser;
            if !on_eraser {
                self.spring_eraser = Some((self.tool, self.brush, std::time::Instant::now()));
            }
            self.select_brush(self.last_eraser);
        }
        if ctx.input(|i| i.key_released(Key::E)) {
            if let Some((tool, brush, since)) = self.spring_eraser.take() {
                if since.elapsed() >= SPRING_HOLD && self.tool == Tool::Brush && self.brush == self.last_eraser {
                    self.select_brush(brush);
                    self.tool = tool;
                }
            }
        }
        if key(Key::G, false) {
            self.tool = Tool::Fill;
        }
        if key(Key::G, true) {
            self.tool = Tool::Gradient;
        }
        if key(Key::M, false) {
            self.tool = Tool::Select(SelectShape::Rect);
        }
        if key(Key::M, true) {
            self.tool = Tool::Select(SelectShape::Ellipse);
        }
        if key(Key::W, false) {
            self.tool = Tool::Select(SelectShape::Magic);
        }
        if key(Key::L, false) {
            self.tool = Tool::Select(SelectShape::Lasso);
        }
        if key(Key::L, true) {
            self.tool = Tool::Select(SelectShape::Magnetic);
        }
        if key(Key::W, true) {
            self.tool = Tool::Select(SelectShape::Object);
        }
        if key(Key::I, false) {
            self.tool = Tool::Eyedropper;
        }
        if key(Key::O, false) {
            self.tool = Tool::VectorEdit;
        }
        // H hand, R rotate, / zoom (Clip Studio's keys). Held, the tool is
        // only on until the key is released, as with E.
        let slash = ctx.input(|i| i.key_pressed(Key::Slash) && !i.modifiers.command && !i.modifiers.alt);
        for (pressed, held, tool) in [(key(Key::H, false), Key::H, Tool::Hand), (key(Key::R, false), Key::R, Tool::Rotate), (slash, Key::Slash, Tool::Zoom)] {
            if pressed && self.tool != tool {
                self.spring_tool = Some((self.tool, tool, held, std::time::Instant::now()));
                self.tool = tool;
            }
        }
        if let Some((before, tool, held, since)) = self.spring_tool {
            if ctx.input(|i| i.key_released(held)) {
                self.spring_tool = None;
                if since.elapsed() >= SPRING_HOLD && self.tool == tool {
                    self.tool = before;
                }
            }
        }
        if key(Key::U, true) {
            self.tool = Tool::Shape;
        }
        if key(Key::T, false) {
            self.tool = Tool::Text(TextKind::Text);
        }
        if key(Key::T, true) {
            self.tool = Tool::Text(TextKind::Balloon);
        }
        if key(Key::S, false) {
            self.tool = Tool::Stamp(StampKind::Clone);
        }
        if key(Key::S, true) {
            self.tool = Tool::Stamp(StampKind::Pattern);
        }
        if key(Key::Y, false) {
            self.tool = Tool::Stamp(StampKind::History);
        }
        if key(Key::X, false) {
            std::mem::swap(&mut self.color, &mut self.sub_color);
        }
        // C: the transparent colour on / off (as in Clip Studio); Shift+C: crop.
        if key(Key::C, false) {
            self.toggle_transparent();
        }
        if key(Key::C, true) {
            self.tool = Tool::Crop;
        }
        // F / Shift+F: the screen modes, as in Photoshop.
        if key(Key::F, false) {
            self.cycle_screen_mode(ctx, true);
        }
        if key(Key::F, true) {
            self.cycle_screen_mode(ctx, false);
        }
        if key(Key::Delete, false) || key(Key::Backspace, false) {
            // A picked vector line (Vector Points tool), else the selected pixels.
            if !self.vector_edit_delete() {
                self.clear_selected();
            }
        }
        if key(Key::OpenBracket, false) {
            let brush = self.current_brush_mut();
            brush.size = (brush.size / 1.15).max(1.0);
        }
        if key(Key::CloseBracket, false) {
            let brush = self.current_brush_mut();
            brush.size = (brush.size * 1.15).min(5000.0);
        }
        // Clip Studio's view keys: - and ^ rotate, Ctrl + NumPad + / − zoom, Ctrl + 0 fits.
        let caret = ctx.input(|i| !i.modifiers.command && i.events.iter().any(|e| matches!(e, egui::Event::Text(t) if t == "^")));
        if key(Key::Minus, false) {
            self.rotate_view_step(-1.0);
        }
        if caret {
            self.rotate_view_step(1.0);
        }
        let (zoom_in, zoom_out, fit) = ctx.input_mut(|i| {
            let zoom_in = i.consume_key(Modifiers::COMMAND, Key::Plus) || i.consume_key(Modifiers::COMMAND, Key::Equals);
            (zoom_in, i.consume_key(Modifiers::COMMAND, Key::Minus), i.consume_key(Modifiers::COMMAND, Key::Num0))
        });
        if zoom_in {
            self.zoom_view_step(true);
        }
        if zoom_out {
            self.zoom_view_step(false);
        }
        if fit {
            self.fit_pending = true;
        }
    }

    /// One step of Clip Studio's rotation keys (5°), `dir` −1 left, 1 right.
    pub(crate) fn rotate_view_step(&mut self, dir: f32) {
        self.view.set_rotation(self.view.rotation + dir * crate::view::ROTATE_STEP.to_radians());
    }

    /// The next zoom level up or down, round the view centre.
    pub(crate) fn zoom_view_step(&mut self, up: bool) {
        self.view.set_zoom(crate::view::zoom_step(self.view.zoom, up));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_a_command() {
        for tool in Tool::ALL {
            assert_eq!(Tool::COMMANDS.iter().filter(|t| **t == tool).count(), 1, "{tool:?}");
        }
        assert_eq!(Tool::ALL.len(), Tool::COMMANDS.len());
    }

    #[test]
    fn brushes_fall_into_toolbar_groups() {
        let group = |name: &str| BrushGroup::of(&BrushSettings { name: name.into(), ..BrushSettings::g_pen() });
        assert_eq!(group("G-pen"), BrushGroup::Pen);
        assert_eq!(group("Mechanical pencil"), BrushGroup::Pencil, "pencil wins over pen");
        assert_eq!(group("Soft airbrush"), BrushGroup::Airbrush);
        assert_eq!(group("Gouache"), BrushGroup::Brush, "anything else is a brush");
        assert_eq!(BrushGroup::of(&BrushSettings::hard_eraser()), BrushGroup::Eraser);
        let presets = BrushSettings::presets();
        let groups: Vec<_> = presets.iter().map(BrushGroup::of).collect();
        assert_eq!(groups, [BrushGroup::Pen, BrushGroup::Pencil, BrushGroup::Airbrush, BrushGroup::Eraser, BrushGroup::Eraser]);
    }
}

/// What the eraser takes off a vector layer (Clip Studio's vector eraser options).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorErase {
    /// Only where it passes, like on pixels (the strokes stay, partly erased).
    Area,
    /// Every stroke it touches, whole.
    WholeStroke,
    /// The touched part of a stroke, up to where other strokes cross it.
    ToIntersection,
}

impl VectorErase {
    pub const ALL: [VectorErase; 3] = [VectorErase::Area, VectorErase::WholeStroke, VectorErase::ToIntersection];

    pub fn name(self) -> &'static str {
        match self {
            VectorErase::Area => "Touched area",
            VectorErase::WholeStroke => "Whole stroke",
            VectorErase::ToIntersection => "Up to intersection",
        }
    }
}
