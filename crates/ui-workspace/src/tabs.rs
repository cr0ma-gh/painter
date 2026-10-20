//! Workspaces: tabs along the top of the window, each with its own area
//! layout and tool set, like Blender's Layout / Sculpting / Shading tabs.
//! Built-in workspaces can be rearranged and reset; custom ones are copies
//! the user names. The whole set saves as JSON.

use serde::{Deserialize, Serialize};

use crate::tiles::{EditorType, SplitKind, TileTree};

/// Which built-in layout a workspace started from (for Reset).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceKind {
    Painting,
    Inking,
    Scene3D,
    Comic,
    Compositing,
}

impl WorkspaceKind {
    pub const ALL: [WorkspaceKind; 5] =
        [WorkspaceKind::Painting, WorkspaceKind::Inking, WorkspaceKind::Scene3D, WorkspaceKind::Comic, WorkspaceKind::Compositing];

    pub fn title(self) -> &'static str {
        match self {
            WorkspaceKind::Painting => "Painting",
            WorkspaceKind::Inking => "Inking",
            WorkspaceKind::Scene3D => "3D Layout",
            WorkspaceKind::Comic => "Comic",
            WorkspaceKind::Compositing => "Compositing",
        }
    }

    /// The built-in arrangement.
    pub fn layout(self) -> TileTree {
        use EditorType::*;
        use SplitKind::*;
        let mut t = TileTree::new(Canvas);
        let canvas = t.root();
        match self {
            // The current default: tools left, canvas, colour and layers right.
            WorkspaceKind::Painting => {
                let right = t.split(canvas, SideBySide, 0.8, ColorWheel, false).unwrap();
                t.add_tab(right, Navigator);
                t.set_editor(right, ColorWheel);
                t.split(right, Stacked, 0.36, Layers, false);
                let left = t.split(canvas, SideBySide, 0.25, BrushSize, true).unwrap();
                let tools = t.split(left, SideBySide, 0.3, ToolGroup, false).unwrap();
                t.split(tools, Stacked, 0.5, ToolSettings, false);
            }
            // As much canvas as possible: one narrow column of tool settings
            // and layers, the navigator to check the whole page.
            WorkspaceKind::Inking => {
                let right = t.split(canvas, SideBySide, 0.82, ToolSettings, false).unwrap();
                let layers = t.split(right, Stacked, 0.45, Layers, false).unwrap();
                t.split(layers, Stacked, 0.6, Navigator, false);
            }
            // The 3D viewport beside the canvas it renders into.
            WorkspaceKind::Scene3D => {
                t.split(canvas, SideBySide, 0.5, Viewport3D, true);
                let right = t.split(canvas, SideBySide, 0.7, ToolSettings, false).unwrap();
                t.split(right, Stacked, 0.55, Layers, false);
            }
            // Panels and layers on the right, the page large.
            WorkspaceKind::Comic => {
                let right = t.split(canvas, SideBySide, 0.75, ComicPanels, false).unwrap();
                t.split(right, Stacked, 0.4, Layers, false);
                let left = t.split(canvas, SideBySide, 0.2, ToolSettings, true).unwrap();
                t.split(left, Stacked, 0.6, ToolGroup, true);
            }
            // Node graph under the canvas, layers on the side.
            WorkspaceKind::Compositing => {
                t.split(canvas, Stacked, 0.6, NodeGraph, false);
                t.split(canvas, SideBySide, 0.75, Layers, false);
            }
        }
        t
    }

    /// Tools shown in the toolbar (`None`: all of them).
    pub fn tools(self) -> Option<Vec<String>> {
        let list: &[&str] = match self {
            WorkspaceKind::Painting | WorkspaceKind::Compositing => return None,
            WorkspaceKind::Inking => &["brush", "eraser", "fill", "select", "transform", "ruler", "hand"],
            WorkspaceKind::Scene3D => &["scene3d", "transform", "hand", "select"],
            WorkspaceKind::Comic => &["frame", "brush", "eraser", "fill", "select", "transform", "ruler", "hand", "crop"],
        };
        Some(list.iter().map(|s| s.to_string()).collect())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub name: String,
    /// The built-in layout it came from; `Reset` goes back to it.
    pub kind: WorkspaceKind,
    pub tiles: TileTree,
    /// Tool ids shown in the toolbar; `None` = every tool.
    pub tools: Option<Vec<String>>,
}

impl Workspace {
    pub fn builtin(kind: WorkspaceKind) -> Self {
        Self { name: kind.title().into(), kind, tiles: kind.layout(), tools: kind.tools() }
    }

    pub fn shows_tool(&self, tool: &str) -> bool {
        self.tools.as_ref().is_none_or(|t| t.iter().any(|x| x == tool))
    }

    pub fn reset(&mut self) {
        self.tiles = self.kind.layout();
        self.tools = self.kind.tools();
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceSet {
    pub workspaces: Vec<Workspace>,
    pub active: usize,
}

impl Default for WorkspaceSet {
    fn default() -> Self {
        Self { workspaces: WorkspaceKind::ALL.into_iter().map(Workspace::builtin).collect(), active: 0 }
    }
}

impl WorkspaceSet {
    pub fn current(&self) -> &Workspace {
        &self.workspaces[self.active.min(self.workspaces.len() - 1)]
    }

    pub fn current_mut(&mut self) -> &mut Workspace {
        let i = self.active.min(self.workspaces.len() - 1);
        &mut self.workspaces[i]
    }

    pub fn activate(&mut self, index: usize) -> bool {
        let ok = index < self.workspaces.len();
        if ok {
            self.active = index;
        }
        ok
    }

    /// A copy of the current workspace under a new name, made current.
    pub fn duplicate(&mut self, name: &str) -> usize {
        let mut copy = self.current().clone();
        copy.name = self.unique_name(name);
        self.workspaces.insert(self.active + 1, copy);
        self.active += 1;
        self.active
    }

    fn unique_name(&self, name: &str) -> String {
        let taken = |n: &str| self.workspaces.iter().any(|w| w.name == n);
        let base = if name.trim().is_empty() { "Workspace" } else { name.trim() };
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|k| format!("{base} {k}")).find(|n| !taken(n)).expect("some name is free")
    }

    pub fn rename(&mut self, index: usize, name: &str) {
        if index < self.workspaces.len() && !name.trim().is_empty() {
            let name = self.unique_name(name);
            self.workspaces[index].name = name;
        }
    }

    /// Removes a workspace; the last one stays.
    pub fn remove(&mut self, index: usize) -> bool {
        if self.workspaces.len() <= 1 || index >= self.workspaces.len() {
            return false;
        }
        self.workspaces.remove(index);
        if self.active >= index && self.active > 0 {
            self.active -= 1;
        }
        true
    }

    /// Moves a tab to a new position, keeping the same workspace current.
    pub fn reorder(&mut self, from: usize, to: usize) {
        if from >= self.workspaces.len() || to >= self.workspaces.len() || from == to {
            return;
        }
        let current = self.workspaces[self.active].name.clone();
        let w = self.workspaces.remove(from);
        self.workspaces.insert(to, w);
        self.active = self.workspaces.iter().position(|w| w.name == current).unwrap_or(0);
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Reads a saved set, repairing what it can; unreadable input gives the defaults.
    pub fn from_json(json: &str) -> Self {
        let mut set: WorkspaceSet = serde_json::from_str(json).unwrap_or_default();
        if set.workspaces.is_empty() {
            set = WorkspaceSet::default();
        }
        for w in &mut set.workspaces {
            w.tiles.sanitize();
        }
        set.active = set.active.min(set.workspaces.len() - 1);
        set
    }
}

/// What's on screen besides the areas. Zen mode (Tab / F11) hides all of it
/// and every area but the canvas; toggling back restores the layout as it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chrome {
    pub menu_bar: bool,
    pub toolbar: bool,
    pub workspace_tabs: bool,
    pub status_bar: bool,
    pub zen: bool,
}

impl Default for Chrome {
    fn default() -> Self {
        Self { menu_bar: true, toolbar: true, workspace_tabs: true, status_bar: true, zen: false }
    }
}

impl Chrome {
    pub fn toggle_zen(&mut self) {
        self.zen = !self.zen;
    }

    pub fn show_menu_bar(&self) -> bool {
        self.menu_bar && !self.zen
    }

    pub fn show_toolbar(&self) -> bool {
        self.toolbar && !self.zen
    }

    pub fn show_workspace_tabs(&self) -> bool {
        self.workspace_tabs && !self.zen
    }

    pub fn show_status_bar(&self) -> bool {
        self.status_bar && !self.zen
    }

    /// In zen mode only the viewport areas are drawn, each filling the window
    /// in turn (the canvas wins if both are open).
    pub fn shows_area(&self, editor: EditorType) -> bool {
        !self.zen || editor == EditorType::Canvas
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_layouts_have_what_they_promise() {
        for kind in WorkspaceKind::ALL {
            let w = Workspace::builtin(kind);
            assert!(w.tiles.find(EditorType::Canvas).is_some(), "{kind:?} has a canvas");
            let mut copy = w.tiles.clone();
            copy.sanitize();
            assert_eq!(copy, w.tiles, "{kind:?} is already clean");
        }
        let painting = Workspace::builtin(WorkspaceKind::Painting);
        assert_eq!(painting.tiles.areas().len(), 6);
        assert!(Workspace::builtin(WorkspaceKind::Scene3D).tiles.find(EditorType::Viewport3D).is_some());
        assert!(Workspace::builtin(WorkspaceKind::Comic).tiles.find(EditorType::ComicPanels).is_some());
        assert!(painting.shows_tool("anything"));
        assert!(!Workspace::builtin(WorkspaceKind::Inking).shows_tool("gradient"));
    }

    #[test]
    fn custom_workspaces_save_and_reset() {
        let mut set = WorkspaceSet::default();
        let i = set.duplicate("Painting");
        assert_eq!(set.current().name, "Painting 2");
        let canvas = set.current().tiles.find(EditorType::Canvas).unwrap();
        set.current_mut().tiles.split(canvas, SplitKind::Stacked, 0.7, EditorType::Timeline, false);
        assert_eq!(set.current().tiles.areas().len(), 7);

        let back = WorkspaceSet::from_json(&set.to_json());
        assert_eq!(back, set);
        assert_eq!(back.active, i);

        set.current_mut().reset();
        assert_eq!(set.current().tiles, WorkspaceKind::Painting.layout());

        set.reorder(i, 0);
        assert_eq!(set.active, 0);
        assert!(set.remove(0));
        assert_eq!(set.workspaces.len(), 5);
        assert_eq!(WorkspaceSet::from_json("not json"), WorkspaceSet::default());
    }

    #[test]
    fn zen_hides_everything_but_the_canvas() {
        let mut chrome = Chrome::default();
        assert!(chrome.show_toolbar() && chrome.shows_area(EditorType::Layers));
        chrome.toggle_zen();
        assert!(!chrome.show_toolbar() && !chrome.show_menu_bar() && !chrome.show_status_bar());
        assert!(chrome.shows_area(EditorType::Canvas) && !chrome.shows_area(EditorType::Layers));
        chrome.toggle_zen();
        assert_eq!(chrome, Chrome::default());
    }
}
