//! Blender-style areas: the window is a binary tree of splits whose leaves
//! are areas. Each area shows one editor type at a time (with a stack of
//! tabs) and can be split again, joined with its sibling, resized by
//! dragging the border between, or switched to another editor type.
//!
//! Nodes live in a map keyed by id, so ids handed out to the UI (to drag a
//! border, to change an area's editor) stay valid while the tree changes
//! elsewhere.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// What an area shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EditorType {
    Canvas,
    /// Another open document, view only (zoom, pan, flip of its own).
    Reference,
    Viewport3D,
    Layers,
    ToolSettings,
    ToolGroup,
    BrushSize,
    ColorWheel,
    Navigator,
    ComicPanels,
    Timeline,
    NodeGraph,
    ColorSlider,
    ColorSet,
    ColorHistory,
    IntermediateColor,
    ApproximateColor,
    ColorMixing,
    SearchLayer,
    /// Clip Studio's History: every undo step by name; click one to go back to it.
    History,
    /// Photoshop's Channels: view and edit red, green and blue apart, saved selections.
    Channels,
    /// Photoshop's Actions: record a series of commands, play it back.
    Actions,
    /// Krita's extended gamut selector: shapes and colour models.
    GamutSelector,
}

impl EditorType {
    pub const ALL: [EditorType; 23] = [
        EditorType::Canvas,
        EditorType::Reference,
        EditorType::Viewport3D,
        EditorType::Layers,
        EditorType::ToolSettings,
        EditorType::ToolGroup,
        EditorType::BrushSize,
        EditorType::ColorWheel,
        EditorType::Navigator,
        EditorType::ComicPanels,
        EditorType::Timeline,
        EditorType::NodeGraph,
        EditorType::ColorSlider,
        EditorType::ColorSet,
        EditorType::ColorHistory,
        EditorType::IntermediateColor,
        EditorType::ApproximateColor,
        EditorType::ColorMixing,
        EditorType::SearchLayer,
        EditorType::GamutSelector,
        EditorType::History,
        EditorType::Channels,
        EditorType::Actions,
    ];

    pub fn title(self) -> &'static str {
        match self {
            EditorType::Canvas => "Canvas",
            EditorType::Reference => "Reference View",
            EditorType::Viewport3D => "3D Scene",
            EditorType::Layers => "Layers",
            EditorType::ToolSettings => "Tool Settings",
            EditorType::ToolGroup => "Tool Group",
            EditorType::BrushSize => "Brush Size",
            EditorType::ColorWheel => "Color Wheel",
            EditorType::Navigator => "Navigator",
            EditorType::ComicPanels => "Comic Panels",
            EditorType::Timeline => "Timeline",
            EditorType::NodeGraph => "Node Graph",
            EditorType::ColorSlider => "Color Slider",
            EditorType::ColorSet => "Color Set",
            EditorType::ColorHistory => "Color History",
            EditorType::IntermediateColor => "Intermediate Color",
            EditorType::ApproximateColor => "Approximate Color",
            EditorType::ColorMixing => "Color Mixing",
            EditorType::SearchLayer => "Search Layer",
            EditorType::GamutSelector => "Extended Color Selector",
            EditorType::History => "History",
            EditorType::Channels => "Channels",
            EditorType::Actions => "Actions",
        }
    }

    /// Editors that show the document view and want the whole area (no
    /// scrolling, no padding).
    pub fn is_viewport(self) -> bool {
        matches!(self, EditorType::Canvas | EditorType::Reference)
    }
}

/// How a split divides its space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitKind {
    /// First child on the left, second on the right (a vertical border).
    SideBySide,
    /// First child on top, second below (a horizontal border).
    Stacked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AreaId(pub u32);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Area {
    /// Editors stacked in this area as tabs; never empty.
    pub editors: Vec<EditorType>,
    pub active: usize,
    /// Folded down to a strip (its header, or a narrow column beside a
    /// side-by-side neighbour), like a collapsed Clip Studio palette.
    #[serde(default)]
    pub collapsed: bool,
    /// This area's own interface scale (1 = the window's), like zooming a
    /// Blender region.
    #[serde(default = "unit")]
    pub scale: f32,
}

fn unit() -> f32 {
    1.0
}

impl Area {
    pub fn editor(&self) -> EditorType {
        self.editors[self.active.min(self.editors.len() - 1)]
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Split {
    pub kind: SplitKind,
    /// Share of the space given to `first`, 0..1.
    pub ratio: f32,
    pub first: AreaId,
    pub second: AreaId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Node {
    Area(Area),
    Split(Split),
}

/// A screen rectangle in points.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }
}

/// An area's place on screen after [`TileTree::layout`].
#[derive(Clone, Debug, PartialEq)]
pub struct AreaRect {
    pub id: AreaId,
    pub rect: Rect,
    /// Shown folded: a header strip when wider than tall, a narrow column otherwise.
    pub collapsed: bool,
}

/// A draggable border: the split it belongs to and its hit rectangle.
#[derive(Clone, Debug, PartialEq)]
pub struct Border {
    pub split: AreaId,
    pub kind: SplitKind,
    pub rect: Rect,
    /// The space the split divides, for turning drags into ratios.
    pub parent: Rect,
}

/// Areas never get smaller than this while dragging (points).
pub const MIN_AREA: f32 = 60.0;
/// Width of the gap (and the grab zone) between areas.
pub const BORDER: f32 = 4.0;
/// Smallest and largest scale of a single panel.
pub const PANEL_SCALE: (f32, f32) = (0.5, 3.0);

/// `scale` kept within [`PANEL_SCALE`] (1 when it isn't a number).
pub fn clamp_panel_scale(scale: f32) -> f32 {
    if scale.is_finite() { scale.clamp(PANEL_SCALE.0, PANEL_SCALE.1) } else { 1.0 }
}

/// Thickness of a collapsed area (its header) unless the caller says otherwise.
pub const COLLAPSED: f32 = 22.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TileTree {
    nodes: BTreeMap<AreaId, Node>,
    root: AreaId,
    next: u32,
}

impl TileTree {
    /// One area showing `editor`.
    pub fn new(editor: EditorType) -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(AreaId(0), Node::Area(Area { editors: vec![editor], active: 0, collapsed: false, scale: 1.0 }));
        Self { nodes, root: AreaId(0), next: 1 }
    }

    pub fn root(&self) -> AreaId {
        self.root
    }

    pub fn node(&self, id: AreaId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    pub fn area(&self, id: AreaId) -> Option<&Area> {
        match self.nodes.get(&id) {
            Some(Node::Area(a)) => Some(a),
            _ => None,
        }
    }

    fn area_mut(&mut self, id: AreaId) -> Option<&mut Area> {
        match self.nodes.get_mut(&id) {
            Some(Node::Area(a)) => Some(a),
            _ => None,
        }
    }

    /// Leaf areas, in depth-first (reading) order.
    pub fn areas(&self) -> Vec<AreaId> {
        let mut out = Vec::new();
        self.walk(self.root, &mut |id, node| {
            if matches!(node, Node::Area(_)) {
                out.push(id);
            }
        });
        out
    }

    fn walk(&self, id: AreaId, f: &mut impl FnMut(AreaId, &Node)) {
        let Some(node) = self.nodes.get(&id) else { return };
        f(id, node);
        if let Node::Split(s) = node {
            let (a, b) = (s.first, s.second);
            self.walk(a, f);
            self.walk(b, f);
        }
    }

    fn parent(&self, child: AreaId) -> Option<AreaId> {
        self.nodes.iter().find_map(|(id, n)| match n {
            Node::Split(s) if s.first == child || s.second == child => Some(*id),
            _ => None,
        })
    }

    fn alloc(&mut self) -> AreaId {
        let id = AreaId(self.next);
        self.next += 1;
        id
    }

    /// Splits `area` in two. The existing editors keep the first part
    /// (`ratio` of the space) unless `new_first`; the new area shows
    /// `editor`. Returns the new area; `area` keeps its id.
    pub fn split(&mut self, area: AreaId, kind: SplitKind, ratio: f32, editor: EditorType, new_first: bool) -> Option<AreaId> {
        if !matches!(self.nodes.get(&area), Some(Node::Area(_))) {
            return None;
        }
        let split = self.alloc();
        let added = self.alloc();
        self.relink(area, split);
        self.nodes.insert(added, Node::Area(Area { editors: vec![editor], active: 0, collapsed: false, scale: 1.0 }));
        let (first, second) = if new_first { (added, area) } else { (area, added) };
        self.nodes.insert(split, Node::Split(Split { kind, ratio: ratio.clamp(0.05, 0.95), first, second }));
        Some(added)
    }

    /// Points whatever referred to `old` (its parent split, or the root) at `new`.
    fn relink(&mut self, old: AreaId, new: AreaId) {
        match self.parent(old) {
            Some(parent) => {
                if let Some(Node::Split(s)) = self.nodes.get_mut(&parent) {
                    if s.first == old {
                        s.first = new;
                    } else {
                        s.second = new;
                    }
                }
            }
            None => self.root = new,
        }
    }

    /// Removes `area`; its sibling takes the parent's place (Blender's
    /// "join"). The last area can't be closed.
    pub fn close(&mut self, area: AreaId) -> bool {
        if !matches!(self.nodes.get(&area), Some(Node::Area(_))) {
            return false;
        }
        let Some(parent) = self.parent(area) else { return false };
        let Some(Node::Split(s)) = self.nodes.get(&parent) else { return false };
        let sibling = if s.first == area { s.second } else { s.first };
        self.relink(parent, sibling);
        self.nodes.remove(&parent);
        self.nodes.remove(&area);
        true
    }
    /// Shows `editor` in `area` (Blender's editor-type menu). If the area
    /// already has it as a tab, that tab comes forward; otherwise the
    /// current tab changes type.
    pub fn set_editor(&mut self, area: AreaId, editor: EditorType) -> bool {
        let Some(a) = self.area_mut(area) else { return false };
        match a.editors.iter().position(|e| *e == editor) {
            Some(i) => a.active = i,
            None => {
                let i = a.active.min(a.editors.len() - 1);
                a.editors[i] = editor;
            }
        }
        true
    }

    /// Adds `editor` as another tab in `area` and shows it.
    pub fn add_tab(&mut self, area: AreaId, editor: EditorType) -> bool {
        let Some(a) = self.area_mut(area) else { return false };
        if let Some(i) = a.editors.iter().position(|e| *e == editor) {
            a.active = i;
        } else {
            a.editors.push(editor);
            a.active = a.editors.len() - 1;
        }
        true
    }

    /// Closes one tab; closing the last tab closes the area.
    pub fn close_tab(&mut self, area: AreaId, editor: EditorType) -> bool {
        let Some(a) = self.area_mut(area) else { return false };
        let Some(i) = a.editors.iter().position(|e| *e == editor) else { return false };
        if a.editors.len() == 1 {
            return self.close(area);
        }
        a.editors.remove(i);
        a.active = a.active.min(a.editors.len() - 1);
        true
    }

    /// Swaps what two areas show (Blender's area swap, Ctrl-drag a corner).
    pub fn swap(&mut self, a: AreaId, b: AreaId) -> bool {
        let (Some(x), Some(y)) = (self.area(a).cloned(), self.area(b).cloned()) else { return false };
        self.nodes.insert(a, Node::Area(y));
        self.nodes.insert(b, Node::Area(x));
        true
    }

    /// The area showing `editor` (as its current tab or a hidden one).
    pub fn find(&self, editor: EditorType) -> Option<AreaId> {
        self.areas().into_iter().find(|id| self.area(*id).is_some_and(|a| a.editors.contains(&editor)))
    }

    pub fn set_ratio(&mut self, split: AreaId, ratio: f32) -> bool {
        match self.nodes.get_mut(&split) {
            Some(Node::Split(s)) => {
                s.ratio = ratio.clamp(0.0, 1.0);
                true
            }
            _ => false,
        }
    }

    /// Moves a border to pointer position `at` (x for side-by-side splits,
    /// y for stacked ones), keeping both sides at least [`MIN_AREA`] wide
    /// when the space allows.
    pub fn drag_border(&mut self, border: &Border, at: f32) -> bool {
        let (start, len) = match border.kind {
            SplitKind::SideBySide => (border.parent.x, border.parent.w),
            SplitKind::Stacked => (border.parent.y, border.parent.h),
        };
        if len <= BORDER {
            return false;
        }
        let min = (MIN_AREA / len).min(0.5);
        let ratio = ((at - start) / len).clamp(min, 1.0 - min);
        self.set_ratio(border.split, ratio)
    }

    /// Sets an area's own interface scale, within [`PANEL_SCALE`].
    pub fn set_scale(&mut self, area: AreaId, scale: f32) -> bool {
        let Some(a) = self.area_mut(area) else { return false };
        a.scale = clamp_panel_scale(scale);
        true
    }

    /// Folds `area` down to a strip, or opens it again. The last area, and
    /// whatever would leave nothing open, can't fold.
    pub fn set_collapsed(&mut self, area: AreaId, collapsed: bool) -> bool {
        if collapsed && self.parent(area).is_none() {
            return false;
        }
        let Some(a) = self.area_mut(area) else { return false };
        a.collapsed = collapsed;
        if collapsed && self.areas().iter().all(|id| self.area(*id).is_some_and(|a| a.collapsed)) {
            if let Some(a) = self.area_mut(area) {
                a.collapsed = false;
            }
            return false;
        }
        true
    }

    /// The space a fully folded subtree needs along `axis` (`None` if any
    /// area in it is open): a strip per folded area, stacked or side by side.
    fn folded_size(&self, id: AreaId, axis: SplitKind, strip: f32) -> Option<f32> {
        match self.nodes.get(&id)? {
            Node::Area(a) => a.collapsed.then_some(strip),
            Node::Split(s) => {
                let (a, b) = (self.folded_size(s.first, axis, strip)?, self.folded_size(s.second, axis, strip)?);
                Some(if s.kind == axis { a + BORDER + b } else { a.max(b) })
            }
        }
    }

    /// Places every area in `rect`, with [`BORDER`]-wide gaps, and returns
    /// the areas and the draggable borders. Collapsed areas take a
    /// [`COLLAPSED`]-thick strip and their neighbours the rest.
    pub fn layout(&self, rect: Rect) -> (Vec<AreaRect>, Vec<Border>) {
        self.layout_with(rect, COLLAPSED)
    }

    /// [`Self::layout`] with collapsed areas `strip` points thick.
    pub fn layout_with(&self, rect: Rect, strip: f32) -> (Vec<AreaRect>, Vec<Border>) {
        let mut areas = Vec::new();
        let mut borders = Vec::new();
        self.place(self.root, rect, strip, &mut areas, &mut borders);
        (areas, borders)
    }

    fn place(&self, id: AreaId, r: Rect, strip: f32, areas: &mut Vec<AreaRect>, borders: &mut Vec<Border>) {
        match self.nodes.get(&id) {
            // The root alone can't fold.
            Some(Node::Area(a)) => areas.push(AreaRect { id, rect: r, collapsed: a.collapsed && id != self.root }),
            Some(Node::Split(s)) => {
                let length = match s.kind {
                    SplitKind::SideBySide => r.w,
                    SplitKind::Stacked => r.h,
                };
                let room = (length - BORDER).max(0.0);
                // A folded side keeps its strip; the other side gets the rest.
                let (fa, fb) = (self.folded_size(s.first, s.kind, strip), self.folded_size(s.second, s.kind, strip));
                let first = match (fa, fb) {
                    (Some(a), _) => a.min(room),
                    (None, Some(b)) => (room - b).max(0.0),
                    (None, None) => room * s.ratio,
                };
                let (a, gap, b) = match s.kind {
                    SplitKind::SideBySide => (
                        Rect::new(r.x, r.y, first, r.h),
                        Rect::new(r.x + first, r.y, BORDER, r.h),
                        Rect::new(r.x + first + BORDER, r.y, (room - first).max(0.0), r.h),
                    ),
                    SplitKind::Stacked => (
                        Rect::new(r.x, r.y, r.w, first),
                        Rect::new(r.x, r.y + first, r.w, BORDER),
                        Rect::new(r.x, r.y + first + BORDER, r.w, (room - first).max(0.0)),
                    ),
                };
                // Folded sides have a fixed size: nothing to drag.
                if fa.is_none() && fb.is_none() {
                    borders.push(Border { split: id, kind: s.kind, rect: gap, parent: r });
                }
                self.place(s.first, a, strip, areas, borders);
                self.place(s.second, b, strip, areas, borders);
            }
            None => {}
        }
    }

    /// Repairs a tree loaded from disk: drops unreachable nodes, empty areas
    /// and dangling splits, and makes sure an area shows the canvas.
    pub fn sanitize(&mut self) {
        let mut reachable = Vec::new();
        self.walk(self.root, &mut |id, _| reachable.push(id));
        self.nodes.retain(|id, _| reachable.contains(id));
        let broken: Vec<AreaId> = self
            .nodes
            .iter()
            .filter_map(|(id, n)| match n {
                Node::Split(s) if !self.nodes.contains_key(&s.first) || !self.nodes.contains_key(&s.second) => Some(*id),
                _ => None,
            })
            .collect();
        if !broken.is_empty() || self.nodes.is_empty() {
            *self = TileTree::new(EditorType::Canvas);
            return;
        }
        for node in self.nodes.values_mut() {
            match node {
                Node::Area(a) => {
                    if a.editors.is_empty() {
                        a.editors.push(EditorType::Canvas);
                    }
                    a.active = a.active.min(a.editors.len() - 1);
                }
                Node::Split(s) => s.ratio = if s.ratio.is_finite() { s.ratio.clamp(0.05, 0.95) } else { 0.5 },
            }
        }
        self.next = self.next.max(self.nodes.keys().map(|id| id.0 + 1).max().unwrap_or(0));
        if self.find(EditorType::Canvas).is_none() {
            let first = self.areas()[0];
            self.add_tab(first, EditorType::Canvas);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_join_and_retype() {
        let mut t = TileTree::new(EditorType::Canvas);
        let canvas = t.root();
        let right = t.split(canvas, SplitKind::SideBySide, 0.75, EditorType::Layers, false).unwrap();
        let color = t.split(right, SplitKind::Stacked, 0.4, EditorType::ColorWheel, true).unwrap();
        assert_eq!(t.areas().len(), 3);
        // Reading order: canvas, colour (on top), layers.
        let names: Vec<_> = t.areas().iter().map(|id| t.area(*id).unwrap().editor()).collect();
        assert_eq!(names, [EditorType::Canvas, EditorType::ColorWheel, EditorType::Layers]);

        // Ids handed out stay valid: retype the colour area, then close it.
        assert!(t.set_editor(color, EditorType::Navigator));
        assert_eq!(t.area(color).unwrap().editor(), EditorType::Navigator);
        assert!(t.close(color));
        assert_eq!(t.areas().len(), 2);
        assert!(t.find(EditorType::Layers).is_some());
        let last = t.areas()[0];
        t.close(t.areas()[1]);
        assert!(!t.close(last), "the last area stays");
    }

    #[test]
    fn layout_and_border_drag() {
        let mut t = TileTree::new(EditorType::Canvas);
        t.split(t.root(), SplitKind::SideBySide, 0.5, EditorType::Layers, false);
        let (areas, borders) = t.layout(Rect::new(0.0, 0.0, 1004.0, 600.0));
        assert_eq!(areas[0].rect, Rect::new(0.0, 0.0, 500.0, 600.0));
        assert_eq!(areas[1].rect, Rect::new(504.0, 0.0, 500.0, 600.0));
        assert_eq!(borders.len(), 1);
        assert!(borders[0].rect.contains(502.0, 300.0));

        assert!(t.drag_border(&borders[0], 800.0));
        let (areas, _) = t.layout(Rect::new(0.0, 0.0, 1004.0, 600.0));
        assert!((areas[0].rect.w - 0.7968 * 1000.0).abs() < 1.0);
        // Dragging past the edge leaves the minimum size.
        t.drag_border(&borders[0], 2000.0);
        let (areas, _) = t.layout(Rect::new(0.0, 0.0, 1004.0, 600.0));
        assert!(areas[1].rect.w >= MIN_AREA - 1.0);
    }

    #[test]
    fn collapsed_areas_fold_to_strips() {
        // Canvas | (colour over layers over navigator)
        let mut t = TileTree::new(EditorType::Canvas);
        let side = t.split(t.root(), SplitKind::SideBySide, 0.7, EditorType::Layers, false).unwrap();
        let color = t.split(side, SplitKind::Stacked, 0.4, EditorType::ColorWheel, true).unwrap();
        let nav = t.split(side, SplitKind::Stacked, 0.5, EditorType::Navigator, false).unwrap();
        let full = Rect::new(0.0, 0.0, 1004.0, 800.0);
        let rect_of = |t: &TileTree, id| t.layout(full).0.into_iter().find(|a| a.id == id).unwrap();

        // Folding the colour wheel leaves its header; the rest of the column grows.
        let before = rect_of(&t, side).rect.h;
        assert!(t.set_collapsed(color, true));
        let folded = rect_of(&t, color);
        assert!(folded.collapsed && folded.rect.h == COLLAPSED);
        let layers = rect_of(&t, side).rect;
        assert!(layers.h > before, "{} > {before}", layers.h);
        assert_eq!(layers.y, COLLAPSED + BORDER);
        // The colour wheel's border with the rest no longer drags.
        let (_, borders) = t.layout(full);
        assert_eq!(borders.len(), 2, "canvas | column, layers / navigator");

        // Folding the whole column narrows it to a strip beside the canvas.
        assert!(t.set_collapsed(side, true));
        assert!(t.set_collapsed(nav, true));
        let canvas = rect_of(&t, t.find(EditorType::Canvas).unwrap()).rect;
        assert_eq!(canvas.w, 1004.0 - BORDER - COLLAPSED);
        assert!(t.layout(full).0.iter().filter(|a| a.collapsed).all(|a| a.rect.w == COLLAPSED));

        // Opening one area gives the column its width back.
        assert!(t.set_collapsed(side, false));
        assert!(rect_of(&t, side).rect.w > 200.0);
        // Not everything can fold, and the state survives saving.
        let lone = TileTree::new(EditorType::Canvas);
        let mut lone2 = lone.clone();
        assert!(!lone2.set_collapsed(lone.root(), true));
        let back: TileTree = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert!(back.area(color).unwrap().collapsed);
        let old = serde_json::to_string(&TileTree::new(EditorType::Canvas)).unwrap().replace(",\"collapsed\":false", "");
        assert!(serde_json::from_str::<TileTree>(&old).is_ok(), "files from before folding still load");
    }

    #[test]
    fn tabs_swap_and_sanitize() {
        let mut t = TileTree::new(EditorType::Canvas);
        let side = t.split(t.root(), SplitKind::SideBySide, 0.8, EditorType::Layers, false).unwrap();
        t.add_tab(side, EditorType::Navigator);
        assert_eq!(t.area(side).unwrap().editor(), EditorType::Navigator);
        assert!(t.close_tab(side, EditorType::Navigator));
        assert_eq!(t.area(side).unwrap().editor(), EditorType::Layers);
        let canvas = t.find(EditorType::Canvas).unwrap();
        assert!(t.swap(canvas, side));
        assert_eq!(t.area(side).unwrap().editor(), EditorType::Canvas);

        // A saved tree without a canvas gets one back.
        let json = serde_json::to_string(&t).unwrap().replace("Canvas", "Timeline");
        let mut loaded: TileTree = serde_json::from_str(&json).unwrap();
        loaded.sanitize();
        assert!(loaded.find(EditorType::Canvas).is_some());
        assert_eq!(loaded.areas().len(), 2);
    }
}
