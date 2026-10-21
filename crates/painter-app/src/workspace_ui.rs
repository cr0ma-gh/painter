//! The window around the canvas, after Blender: workspace tabs along the
//! top, the rest of the window split into areas (each with an editor-type
//! menu, tabs and split / close commands, borders dragged to resize), the
//! command palette (F3 or Ctrl+Space), zen mode (Tab or F11) and the theme
//! editor. The layout model lives in the `ui_workspace` crate; this module
//! draws it with egui and maps editor types to the app's panels.

use eframe::egui::{
    self, text::LayoutJob, Align, Align2, Color32, CursorIcon, Key, Layout, Modifiers, Rect, Sense, TextFormat, UiBuilder, Vec2,
};
use painter_core::fill_tool::GapPreset;
use ui_workspace::command_palette::{Command, CommandCategory, CommandRegistry, PaletteState};
use ui_workspace::tabs::{Chrome, WorkspaceSet};
use ui_workspace::themes::{Rgba, ThemeLibrary};
use ui_workspace::tiles::{self, AreaId, EditorType, SplitKind};

use crate::app::PainterApp;
use crate::theme;
use crate::tools::{BrushGroup, Tool};

const WORKSPACES_KEY: &str = "workspaces_v1";
const CHROME_KEY: &str = "chrome_v1";
const THEMES_KEY: &str = "themes_v1";
/// Height of an area's header strip.
const HEADER: f32 = 22.0;
/// Rows shown in the command palette.
const PALETTE_ROWS: usize = 14;

pub(crate) struct WorkspaceUi {
    pub(crate) set: WorkspaceSet,
    pub(crate) chrome: Chrome,
    pub(crate) themes: ThemeLibrary,
    pub(crate) commands: CommandRegistry,
    pub(crate) palette: PaletteState,
    pub(crate) theme_editor: bool,
    /// Theme revision last applied to egui.
    applied_theme: Option<u64>,
    /// Workspace tab being renamed, with the text so far.
    renaming: Option<(usize, String)>,
    /// Where settings live when eframe has no storage (iPadOS), and what
    /// was last written there.
    settings_file: Option<std::path::PathBuf>,
    written: String,
    checked: Option<std::time::Instant>,
}

impl WorkspaceUi {
    pub(crate) fn load(storage: Option<&dyn eframe::Storage>) -> Self {
        // Without eframe storage (iPadOS), a JSON file of the same keys.
        let settings_file = if storage.is_none() { crate::files::settings_file() } else { None };
        let written = settings_file.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        let file: std::collections::HashMap<String, String> = serde_json::from_str(&written).unwrap_or_default();
        let get = |key: &str| storage.and_then(|s| s.get_string(key)).or_else(|| file.get(key).cloned());
        Self {
            set: get(WORKSPACES_KEY).map(|j| WorkspaceSet::from_json(&j)).unwrap_or_default(),
            chrome: get(CHROME_KEY).and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default(),
            themes: get(THEMES_KEY).map(|j| ThemeLibrary::from_json(&j)).unwrap_or_default(),
            commands: CommandRegistry::new(),
            palette: PaletteState::default(),
            theme_editor: false,
            applied_theme: None,
            renaming: None,
            settings_file,
            written,
            checked: None,
        }
    }

    fn entries(&self) -> [(&'static str, String); 3] {
        [
            (WORKSPACES_KEY, self.set.to_json()),
            (CHROME_KEY, serde_json::to_string(&self.chrome).unwrap_or_default()),
            (THEMES_KEY, self.themes.to_json()),
        ]
    }

    /// Writes the settings file (when there is one) if anything changed;
    /// checks at most every two seconds.
    pub(crate) fn autosave(&mut self) {
        let Some(path) = &self.settings_file else { return };
        if self.checked.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(2)) {
            return;
        }
        self.checked = Some(std::time::Instant::now());
        let map: std::collections::BTreeMap<&str, String> = self.entries().into_iter().collect();
        let json = serde_json::to_string(&map).unwrap_or_default();
        if json != self.written {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            // Through a temporary file, so a crash mid-write keeps the old settings.
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, &json).is_ok() && std::fs::rename(&tmp, path).is_ok() {
                self.written = json;
            }
        }
    }

    pub(crate) fn save(&self, storage: &mut dyn eframe::Storage) {
        for (key, value) in self.entries() {
            storage.set_string(key, value);
        }
    }
}

/// A built-in command: id, label, category, shortcut, keywords, enabled.
type FixedCommand = (&'static str, &'static str, CommandCategory, &'static str, &'static [&'static str], bool);

/// What a click in an area header asks for.
enum AreaAction {
    SetEditor(EditorType),
    ShowTab(EditorType),
    AddTab(EditorType),
    CloseTab(EditorType),
    Split(SplitKind),
    Close,
}

fn to_egui(r: tiles::Rect) -> Rect {
    Rect::from_min_size(egui::pos2(r.x, r.y), Vec2::new(r.w, r.h))
}

fn from_egui(r: Rect) -> tiles::Rect {
    tiles::Rect::new(r.min.x, r.min.y, r.width(), r.height())
}

fn color32(c: Rgba) -> Color32 {
    let [r, g, b, a] = c.0;
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

impl PainterApp {
    /// Per-frame bookkeeping: the theme and the workspace shortcuts.
    pub(crate) fn workspace_frame(&mut self, ctx: &egui::Context) {
        if self.ws.applied_theme != Some(self.ws.themes.revision) {
            theme::apply(ctx, self.ws.themes.current());
            self.ws.applied_theme = Some(self.ws.themes.revision);
        }
        if ctx.wants_keyboard_input() || self.ws.palette.open {
            return;
        }
        let (palette, zen) = ctx.input_mut(|i| {
            let palette = i.consume_key(Modifiers::NONE, Key::F3) || i.consume_key(Modifiers::COMMAND, Key::Space);
            let zen = i.consume_key(Modifiers::NONE, Key::F11) || i.consume_key(Modifiers::NONE, Key::Tab);
            (palette, zen)
        });
        if palette {
            self.open_palette();
        }
        if zen {
            self.toggle_zen();
        }
    }

    /// Loads a theme JSON file into the library and switches to it.
    pub(crate) fn import_theme_from(&mut self, path: &std::path::Path) {
        let lib = &mut self.ws.themes;
        self.status = match std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|j| lib.import(&j)) {
            Ok(_) => format!("Theme loaded from {}", path.display()),
            Err(err) => format!("Couldn't load the theme: {err}"),
        };
    }

    pub(crate) fn toggle_zen(&mut self) {
        self.ws.chrome.toggle_zen();
        self.status = if self.ws.chrome.zen {
            "Zen mode: the corner button, Tab, F11 or a four-finger tap brings the panels back".into()
        } else {
            String::new()
        };
    }

    /// In zen mode, a faint button in the top-right corner of the canvas
    /// brings the panels back, for touch screens without a keyboard.
    fn zen_exit_button(&mut self, ctx: &egui::Context, canvas: Rect) {
        let size = if cfg!(target_os = "ios") { 44.0 } else { 28.0 };
        let rect = Rect::from_min_size(egui::pos2(canvas.right() - size - 8.0, canvas.top() + 8.0), Vec2::splat(size));
        let response = egui::Area::new(egui::Id::new("zen_exit"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .show(ctx, |ui| {
                let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
                let strong = response.hovered() || response.is_pointer_button_down_on();
                let fill = theme::backdrop().gamma_multiply(if strong { 0.95 } else { 0.55 });
                let ink = ui.visuals().text_color().gamma_multiply(if strong { 1.0 } else { 0.6 });
                ui.painter().rect_filled(rect, size * 0.25, fill);
                // Four corner brackets: "leave full screen".
                let (c, a, b) = (rect.center(), size * 0.16, size * 0.3);
                let stroke = egui::Stroke::new(2.0_f32, ink);
                for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                    let corner = c + Vec2::new(sx * a, sy * a);
                    ui.painter().line_segment([corner, corner + Vec2::new(sx * (b - a), 0.0)], stroke);
                    ui.painter().line_segment([corner, corner + Vec2::new(0.0, sy * (b - a))], stroke);
                }
                response.on_hover_text("Show the panels again (Tab, F11, four-finger tap)")
            })
            .inner;
        if response.clicked() {
            self.toggle_zen();
        }
    }

    // ---- workspace tabs -----------------------------------------------------------------

    pub(crate) fn workspace_tabs_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("workspace_tabs")
            .frame(egui::Frame::new().fill(theme::backdrop()).inner_margin(egui::Margin::symmetric(6, 2)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let mut activate = None;
                    let mut menu: Option<(usize, &str)> = None;
                    for (i, w) in self.ws.set.workspaces.iter().enumerate() {
                        if let Some((k, text)) = &mut self.ws.renaming {
                            if *k == i {
                                let edit = ui.add(egui::TextEdit::singleline(text).desired_width(110.0));
                                edit.request_focus();
                                if edit.lost_focus() {
                                    menu = Some((i, "rename_done"));
                                }
                                continue;
                            }
                        }
                        let tab = ui.selectable_label(i == self.ws.set.active, &w.name);
                        if tab.clicked() {
                            activate = Some(i);
                        }
                        if tab.double_clicked() {
                            menu = Some((i, "rename"));
                        }
                        tab.context_menu(|ui| {
                            for (label, what) in [
                                ("Rename", "rename"),
                                ("Duplicate", "duplicate"),
                                ("Reset Layout", "reset"),
                                ("Move Left", "left"),
                                ("Move Right", "right"),
                                ("Delete", "delete"),
                            ] {
                                if ui.button(label).clicked() {
                                    menu = Some((i, what));
                                    ui.close_menu();
                                }
                            }
                        });
                    }
                    if ui.small_button("+").on_hover_text("New workspace: a copy of this one").clicked() {
                        let name = format!("{} copy", self.ws.set.current().name);
                        self.ws.set.duplicate(&name);
                    }
                    if let Some(i) = activate {
                        self.ws.set.activate(i);
                    }
                    if let Some((i, what)) = menu {
                        self.workspace_menu(i, what);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Zen").on_hover_text("Hide everything but the canvas (Tab or F11)").clicked() {
                            self.toggle_zen();
                        }
                        if ui.small_button("Search").on_hover_text("Command palette (F3 or Ctrl+Space)").clicked() {
                            self.open_palette();
                        }
                    });
                });
            });
    }

    fn workspace_menu(&mut self, i: usize, what: &str) {
        let set = &mut self.ws.set;
        match what {
            "rename" => self.ws.renaming = Some((i, set.workspaces[i].name.clone())),
            "rename_done" => {
                if let Some((k, name)) = self.ws.renaming.take() {
                    set.rename(k, &name);
                }
            }
            "duplicate" => {
                set.activate(i);
                let name = format!("{} copy", set.current().name);
                set.duplicate(&name);
            }
            "reset" => set.workspaces[i].reset(),
            "left" if i > 0 => set.reorder(i, i - 1),
            "right" => set.reorder(i, i + 1),
            "delete" => {
                let last = !set.remove(i);
                if last {
                    self.status = "The last workspace can't be deleted".into();
                }
            }
            _ => {}
        }
    }

    /// Whether the toolbar shows `tool` in this workspace.
    pub(crate) fn tool_in_workspace(&self, tool: Tool) -> bool {
        self.ws.set.current().shows_tool(tool.id())
    }

    pub(crate) fn group_in_workspace(&self, group: BrushGroup) -> bool {
        self.ws.set.current().shows_tool(if group == BrushGroup::Eraser { "eraser" } else { "brush" })
    }

    // ---- areas ----------------------------------------------------------------------------

    /// The workspace's areas fill the rest of the window.
    pub(crate) fn areas(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::backdrop())).show(ctx, |ui| {
            let full = ui.max_rect();
            let tree = self.ws.set.current().tiles.clone();
            if self.ws.chrome.zen {
                // Only the canvas, edge to edge.
                let mut child = ui.new_child(UiBuilder::new().max_rect(full).id_salt("zen_canvas"));
                self.canvas_ui(&mut child);
                self.zen_exit_button(ui.ctx(), full);
                return;
            }
            let (areas, borders) = tree.layout(from_egui(full));
            let mut canvas_shown = false;
            let mut actions = Vec::new();
            for a in &areas {
                if let Some(action) = self.area_ui(ui, &tree, a.id, to_egui(a.rect), &mut canvas_shown) {
                    actions.push((a.id, action));
                }
            }
            // Borders last, so they win the pointer over the areas' edges.
            for border in &borders {
                let rect = to_egui(border.rect).expand2(match border.kind {
                    SplitKind::SideBySide => Vec2::new(2.0, 0.0),
                    SplitKind::Stacked => Vec2::new(0.0, 2.0),
                });
                let response = ui.interact(rect, ui.id().with(("border", border.split.0)), Sense::drag());
                let cursor = match border.kind {
                    SplitKind::SideBySide => CursorIcon::ResizeHorizontal,
                    SplitKind::Stacked => CursorIcon::ResizeVertical,
                };
                if response.hovered() || response.dragged() {
                    ui.ctx().set_cursor_icon(cursor);
                    let color = if response.dragged() { theme::accent() } else { theme::hover() };
                    ui.painter().rect_filled(to_egui(border.rect), 0.0, color);
                }
                if response.dragged() {
                    if let Some(p) = response.interact_pointer_pos() {
                        let at = if border.kind == SplitKind::SideBySide { p.x } else { p.y };
                        self.ws.set.current_mut().tiles.drag_border(border, at);
                    }
                }
            }
            for (id, action) in actions {
                self.apply_area_action(id, action);
            }
        });
    }

    fn area_ui(
        &mut self,
        ui: &mut egui::Ui,
        tree: &tiles::TileTree,
        id: AreaId,
        rect: Rect,
        canvas_shown: &mut bool,
    ) -> Option<AreaAction> {
        let area = tree.area(id)?.clone();
        let editor = area.editor();
        let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), HEADER.min(rect.height())));
        let body = Rect::from_min_max(egui::pos2(rect.min.x, header.max.y), rect.max);
        ui.painter().rect_filled(header, 0.0, theme::title());
        let many_areas = tree.areas().len() > 1;
        let mut action = None;

        let mut head = ui.new_child(
            UiBuilder::new()
                .max_rect(header.shrink2(Vec2::new(4.0, 1.0)))
                .layout(Layout::left_to_right(Align::Center))
                .id_salt(("area_head", id.0)),
        );
        head.set_clip_rect(header.intersect(ui.clip_rect()));
        head.spacing_mut().item_spacing.x = 2.0;
        head.spacing_mut().interact_size.y = HEADER - 4.0;
        egui::ComboBox::from_id_salt("editor_type").selected_text(egui::RichText::new(editor.title()).small()).width(104.0).show_ui(
            &mut head,
            |ui| {
                for e in EditorType::ALL {
                    if ui.selectable_label(e == editor, e.title()).clicked() {
                        action = Some(AreaAction::SetEditor(e));
                    }
                }
            },
        );
        if area.editors.len() > 1 {
            for (i, e) in area.editors.iter().enumerate() {
                let label = egui::RichText::new(e.title()).small();
                if head.selectable_label(i == area.active, label).clicked() {
                    action = Some(AreaAction::ShowTab(*e));
                }
            }
        }
        head.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.menu_button("···", |ui| {
                if ui.button("Split Right").clicked() {
                    action = Some(AreaAction::Split(SplitKind::SideBySide));
                    ui.close_menu();
                }
                if ui.button("Split Down").clicked() {
                    action = Some(AreaAction::Split(SplitKind::Stacked));
                    ui.close_menu();
                }
                ui.menu_button("Add Tab", |ui| {
                    for e in EditorType::ALL.into_iter().filter(|e| !area.editors.contains(e)) {
                        if ui.button(e.title()).clicked() {
                            action = Some(AreaAction::AddTab(e));
                            ui.close_menu();
                        }
                    }
                });
                ui.separator();
                let holds_canvas = area.editors.contains(&EditorType::Canvas);
                if ui.add_enabled(area.editors.len() > 1 && editor != EditorType::Canvas, egui::Button::new("Close Tab")).clicked() {
                    action = Some(AreaAction::CloseTab(editor));
                    ui.close_menu();
                }
                let close = ui.add_enabled(many_areas && !holds_canvas, egui::Button::new("Close Area"));
                if close.on_disabled_hover_text("The area holding the canvas stays").clicked() {
                    action = Some(AreaAction::Close);
                    ui.close_menu();
                }
            });
        });

        if body.height() < 4.0 {
            return action;
        }
        ui.painter().rect_filled(body, 0.0, if editor.is_viewport() { theme::backdrop() } else { theme::panel() });
        let mut content =
            ui.new_child(UiBuilder::new().max_rect(body).layout(Layout::top_down(Align::Min)).id_salt(("area_body", id.0)));
        content.set_clip_rect(body.intersect(ui.clip_rect()));
        if editor == EditorType::Reference {
            self.reference_view_ui(&mut content);
            return action;
        }
        if editor == EditorType::Canvas {
            if *canvas_shown {
                content.centered_and_justified(|ui| ui.label("The canvas is shown in another area"));
            } else {
                *canvas_shown = true;
                self.canvas_ui(&mut content);
            }
            return action;
        }
        let mut inner = content.new_child(UiBuilder::new().max_rect(body.shrink(6.0)).layout(Layout::top_down(Align::Min)));
        egui::ScrollArea::vertical()
            .id_salt("area_scroll")
            .auto_shrink([false, false])
            .show(&mut inner, |ui| self.editor_ui(ui, editor));
        action
    }

    /// An editor's contents.
    fn editor_ui(&mut self, ui: &mut egui::Ui, editor: EditorType) {
        match editor {
            EditorType::Canvas | EditorType::Reference => {}
            EditorType::Layers => self.layers_panel(ui),
            EditorType::ToolSettings => self.tool_property_panel(ui),
            EditorType::ToolGroup => self.subtool_panel(ui),
            EditorType::BrushSize => self.brush_size_panel(ui),
            EditorType::ColorWheel => self.color_panel(ui),
            EditorType::Navigator => self.navigator_panel(ui),
            EditorType::Viewport3D => {
                if self.active_scene().is_some() {
                    self.scene_panel(ui);
                } else {
                    ui.label("The active layer isn't a 3D layer.");
                    ui.small("3D layers render straight onto the canvas; their objects, camera and lights are edited here.");
                    if ui.button("New 3D layer from a model...").clicked() {
                        self.new_scene_layer_dialog();
                    }
                }
            }
            EditorType::ComicPanels => {
                if self.comic.as_ref().is_some_and(|c| !c.panels.is_empty()) {
                    self.panels_section(ui);
                } else {
                    ui.label("No comic panels on this page.");
                    if ui.button("New comic page...").clicked() {
                        self.comic_wizard = Some(crate::comic_ui::ComicWizard::open());
                    }
                    ui.small("Or draw frames with the Frame tool (U).");
                }
            }
            EditorType::Timeline | EditorType::NodeGraph => {
                ui.label(format!("{}: not available yet.", editor.title()));
                ui.small("The area is kept in the layout; switch it to another editor from the menu at its top left.");
            }
        }
    }

    fn apply_area_action(&mut self, id: AreaId, action: AreaAction) {
        let tiles = &mut self.ws.set.current_mut().tiles;
        match action {
            AreaAction::SetEditor(e) => {
                tiles.set_editor(id, e);
            }
            AreaAction::ShowTab(e) | AreaAction::AddTab(e) => {
                tiles.add_tab(id, e);
            }
            AreaAction::CloseTab(e) => {
                tiles.close_tab(id, e);
            }
            AreaAction::Split(kind) => {
                // The new area starts as the same editor type, like Blender.
                let editor = tiles.area(id).map_or(EditorType::Layers, |a| a.editor());
                let editor = if editor == EditorType::Canvas { EditorType::Layers } else { editor };
                tiles.split(id, kind, 0.5, editor, false);
            }
            AreaAction::Close => {
                tiles.close(id);
            }
        }
        // The canvas can't go missing: if a retype removed it, it comes back as a tab.
        if tiles.find(EditorType::Canvas).is_none() {
            tiles.add_tab(id, EditorType::Canvas);
            tiles.set_editor(id, EditorType::Canvas);
            self.status = "The canvas needs an area; it stays here".into();
        }
    }

    /// Shows `editor`: brings its tab forward, or adds it beside the panels.
    pub(crate) fn show_editor(&mut self, editor: EditorType) {
        let tiles = &mut self.ws.set.current_mut().tiles;
        if let Some(area) = tiles.find(editor) {
            tiles.add_tab(area, editor);
            return;
        }
        let panel = tiles.areas().into_iter().find(|a| tiles.area(*a).is_some_and(|x| !x.editor().is_viewport()));
        match panel {
            Some(area) => {
                tiles.add_tab(area, editor);
            }
            None => {
                let canvas = tiles.areas()[0];
                tiles.split(canvas, SplitKind::SideBySide, 0.78, editor, false);
            }
        }
    }

    pub(crate) fn editor_open(&self, editor: EditorType) -> bool {
        self.ws.set.current().tiles.find(editor).is_some()
    }

    pub(crate) fn toggle_editor(&mut self, editor: EditorType) {
        let tiles = &mut self.ws.set.current_mut().tiles;
        match tiles.find(editor) {
            Some(area) if editor != EditorType::Canvas => {
                tiles.close_tab(area, editor);
            }
            Some(_) => {}
            None => self.show_editor(editor),
        }
    }

    // ---- command palette --------------------------------------------------------------

    pub(crate) fn open_palette(&mut self) {
        self.refresh_commands();
        self.ws.palette.open();
    }

    /// Rebuilds the command list from the app's current state (brushes,
    /// workspaces, themes and what's enabled change as it runs).
    fn refresh_commands(&mut self) {
        use CommandCategory as C;
        let r = &mut self.ws.commands;
        r.unregister_prefix("");
        let has_sel = self.doc.selection.is_some();
        let has_mask = self.doc.active_layer().is_some_and(|l| l.mask.is_some());
        let fixed: [FixedCommand; 37] = [
            ("file.new", "New Canvas...", C::File, "Ctrl+N", &["document", "create"], true),
            ("file.comic", "New Comic Page / Storyboard...", C::File, "", &["manga", "panels", "template"], true),
            ("file.open", "Open...", C::File, "Ctrl+O", &["load"], true),
            ("file.save", "Save", C::File, "Ctrl+S", &[], true),
            ("file.save_as", "Save As...", C::File, "Ctrl+Shift+S", &[], true),
            ("file.import_png", "Import PNG as Layer...", C::File, "", &["image", "picture"], true),
            ("file.scan", "Import from Scanner...", C::File, "", &["scan", "wia"], true),
            ("file.import_brushes", "Import Brushes...", C::Brush, "", &["sut", "abr", "clip studio", "photoshop"], true),
            ("file.export_png", "Export PNG...", C::File, "", &["save image"], true),
            ("file.export_psd", "Export PSD...", C::File, "", &["photoshop"], true),
            ("file.export_svg", "Export SVG...", C::File, "", &["vector"], true),
            ("file.export_brushes", "Export All Brushes...", C::Brush, "", &[], true),
            ("edit.undo", "Undo", C::Edit, "Ctrl+Z", &[], self.history.can_undo()),
            ("edit.redo", "Redo", C::Edit, "Ctrl+Shift+Z", &[], self.history.can_redo()),
            ("edit.clear", "Clear Selected", C::Edit, "Del", &["delete", "erase"], has_sel),
            ("select.all", "Select All", C::Select, "Ctrl+A", &[], true),
            ("select.none", "Deselect", C::Select, "Ctrl+D", &["clear selection"], has_sel),
            ("select.invert", "Invert Selection", C::Select, "Ctrl+Shift+I", &[], true),
            ("layer.raster", "New Raster Layer", C::Layer, "", &["add layer"], true),
            ("layer.vector", "New Vector Layer", C::Layer, "", &[], true),
            ("layer.folder", "New Folder", C::Layer, "", &["group"], true),
            ("layer.gradient", "New Gradient Layer", C::Layer, "", &[], true),
            ("layer.scene3d", "New 3D Layer from Model...", C::Layer, "", &["blend", "obj", "blender"], true),
            ("layer.tone", "New Tone Layer (from selection)", C::Layer, "", &["screentone", "halftone"], has_sel),
            ("layer.add_mask", "Add Layer Mask", C::Layer, "", &[], !has_mask),
            ("layer.delete_mask", "Delete Layer Mask", C::Layer, "", &[], has_mask),
            ("layer.delete", "Delete Layer", C::Layer, "", &["remove layer"], true),
            ("view.fit", "Fit to Window", C::View, "0", &["zoom"], true),
            ("view.actual", "Actual Pixels (100%)", C::View, "", &["zoom 100"], true),
            ("view.reset_rotation", "Reset Rotation", C::View, "", &[], true),
            ("view.flip", "Flip View Horizontally", C::View, "", &["mirror", "check the drawing"], true),
            ("view.grid", "Show Grid", C::View, "Ctrl+'", &["guides"], true),
            ("rulers.snap", "Snap to Rulers", C::View, "Ctrl+1", &["perspective", "symmetry", "ruler"], true),
            ("view.zen", "Toggle Zen Mode", C::View, "Tab / F11", &["fullscreen", "hide panels", "distraction free"], true),
            ("view.theme_editor", "Theme Editor...", C::View, "", &["colors", "colours", "dark", "light"], true),
            ("workspace.reset", "Reset Workspace Layout", C::Workspace, "", &["default layout"], true),
            ("workspace.duplicate", "New Workspace (copy of this one)", C::Workspace, "", &["custom layout"], true),
        ];
        for (id, label, cat, keys, words, enabled) in fixed {
            let mut c = Command::new(id, label, cat).keywords(words);
            if !keys.is_empty() {
                c = c.shortcut(keys);
            }
            c.enabled = enabled;
            r.register(c);
        }
        for (i, tool) in Tool::ALL.iter().enumerate() {
            let mut c = Command::new(&format!("tool.{i}"), tool.name(), C::Tool).keywords(&["tool"]);
            if let Some(keys) = tool.shortcut() {
                c = c.shortcut(keys);
            }
            r.register(c);
        }
        for (i, b) in self.brushes.iter().enumerate() {
            let group = BrushGroup::of(&b.settings).label();
            r.register(Command::new(&format!("brush.{i}"), &b.settings.name, C::Brush).keywords(&[group, "brush preset"]));
        }
        for (i, w) in self.ws.set.workspaces.iter().enumerate() {
            r.register(Command::new(&format!("workspace.{i}"), &format!("Workspace: {}", w.name), C::Workspace));
        }
        for (i, t) in self.ws.themes.themes.iter().enumerate() {
            r.register(Command::new(&format!("theme.{i}"), &format!("Theme: {}", t.name), C::View).keywords(&["colour scheme"]));
        }
        for (i, e) in EditorType::ALL.iter().enumerate() {
            r.register(
                Command::new(&format!("editor.{i}"), &format!("Show {}", e.title()), C::Workspace).keywords(&["panel", "window"]),
            );
        }
        for (i, a) in painter_core::Adjustment::defaults().iter().enumerate() {
            r.register(Command::new(&format!("adjust.{i}"), &format!("New Correction Layer: {}", a.name()), C::Layer).keywords(&["adjustment", "colour correction", "color"]));
        }
        for (i, f) in crate::adjust_ui::filter_presets(self.doc.canvas()).iter().enumerate() {
            r.register(Command::new(&format!("filter.{i}"), &format!("Filter: {}...", f.name()), C::Filter).keywords(&["effect", "blur", "sharpen"]));
        }
        for preset in GapPreset::ALL {
            let label = format!("Fill: Close Gaps {}", preset.label());
            r.register(Command::new(&format!("fill.gap.{}", preset.pixels()), &label, C::Tool).keywords(&["bucket", "gap closing"]));
        }
    }

    /// Runs a palette command by id.
    pub(crate) fn run_command(&mut self, id: &str) {
        if let Some(c) = self.ws.commands.get(id) {
            if !c.enabled {
                self.status = format!("{} isn't available right now", c.label);
                return;
            }
        }
        self.ws.commands.note_used(id);
        let index = |prefix: &str| id.strip_prefix(prefix).and_then(|n| n.parse::<usize>().ok());
        match id {
            "file.new" => self.open_new_canvas(),
            "file.comic" => self.comic_wizard = Some(crate::comic_ui::ComicWizard::open()),
            "file.open" => self.open_dialog(),
            "file.save" => self.save(),
            "file.save_as" => self.save_as(),
            "file.import_png" => self.import_png_layer(),
            "file.scan" => self.scanner = Some(crate::dialogs::ScannerDialog::open()),
            "file.import_brushes" => self.import_brushes_dialog(),
            "file.export_png" => self.export_png(),
            "file.export_psd" => self.export_psd(),
            "file.export_svg" => self.svg_export = Some(crate::dialogs::SvgExportDialog::default()),
            "file.export_brushes" => self.export_brushes(None),
            "edit.undo" => self.undo(),
            "edit.redo" => self.redo(),
            "edit.clear" => self.clear_selected(),
            "select.all" => self.select_all(),
            "select.none" => self.deselect(),
            "select.invert" => self.invert_selection(),
            "layer.raster" => self.add_raster_layer(),
            "layer.vector" => self.add_vector_layer(),
            "layer.folder" => self.add_folder(),
            "layer.gradient" => self.add_gradient_layer(),
            "layer.scene3d" => self.new_scene_layer_dialog(),
            "layer.tone" => self.add_tone_layer(),
            "layer.add_mask" => self.add_mask(),
            "layer.delete_mask" => self.delete_mask(),
            "layer.delete" => self.delete_active_layer(),
            "view.fit" => self.fit_pending = true,
            "view.actual" => self.view.set_zoom(1.0),
            "view.reset_rotation" => self.view.set_rotation(0.0),
            "view.flip" => self.view.flip = !self.view.flip,
            "view.grid" => self.doc.rulers.grid.visible = !self.doc.rulers.grid.visible,
            "rulers.snap" => self.toggle_ruler_snapping(),
            "view.zen" => self.toggle_zen(),
            "view.theme_editor" => self.ws.theme_editor = true,
            "workspace.reset" => self.ws.set.current_mut().reset(),
            "workspace.duplicate" => {
                let name = format!("{} copy", self.ws.set.current().name);
                self.ws.set.duplicate(&name);
            }
            _ => {
                if let Some(i) = index("tool.") {
                    match Tool::ALL.get(i) {
                        Some(Tool::Brush) => self.select_brush(self.last_paint_brush),
                        Some(tool) => self.tool = *tool,
                        None => {}
                    }
                } else if let Some(i) = index("brush.") {
                    if i < self.brushes.len() {
                        self.select_brush(i);
                    }
                } else if let Some(i) = index("workspace.") {
                    self.ws.set.activate(i);
                } else if let Some(i) = index("theme.") {
                    self.ws.themes.select(i);
                } else if let Some(i) = index("editor.") {
                    if let Some(e) = EditorType::ALL.get(i) {
                        self.show_editor(*e);
                    }
                } else if let Some(i) = index("adjust.") {
                    if let Some(a) = painter_core::Adjustment::defaults().into_iter().nth(i) {
                        self.add_adjustment_layer(a);
                    }
                } else if let Some(i) = index("filter.") {
                    if let Some(f) = crate::adjust_ui::filter_presets(self.doc.canvas()).into_iter().nth(i) {
                        self.open_filter(f);
                    }
                } else if let Some(px) = index("fill.gap.") {
                    self.tools.fill.gap = px as u32;
                    self.tool = Tool::Fill;
                }
            }
        }
    }

    pub(crate) fn command_palette_window(&mut self, ctx: &egui::Context) {
        if !self.ws.palette.open {
            return;
        }
        let matches = self.ws.commands.search(&self.ws.palette.query, PALETTE_ROWS);
        let (up, down, enter, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if up {
            self.ws.palette.move_selection(-1, matches.len());
        }
        if down {
            self.ws.palette.move_selection(1, matches.len());
        }
        let mut run: Option<String> = None;
        if enter {
            run = matches.get(self.ws.palette.selected).map(|m| self.ws.commands.commands()[m.index].id.clone());
        }
        let accent = theme::accent();
        let response = egui::Window::new("Command palette")
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .anchor(Align2::CENTER_TOP, [0.0, 70.0])
            .fixed_size([480.0, 0.0])
            .show(ctx, |ui| {
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.ws.palette.query)
                        .hint_text("Search commands, tools, brushes, workspaces...")
                        .desired_width(f32::INFINITY),
                );
                edit.request_focus();
                if edit.changed() {
                    self.ws.palette.query_changed();
                }
                ui.separator();
                if matches.is_empty() {
                    ui.weak("No command matches");
                }
                for (k, m) in matches.iter().enumerate() {
                    let c = &self.ws.commands.commands()[m.index];
                    let (rect, row) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::click());
                    let picked = k == self.ws.palette.selected;
                    if picked || row.hovered() {
                        ui.painter().rect_filled(rect, 3.0, if picked { theme::selected() } else { theme::hover() });
                    }
                    let text_color = if c.enabled { theme::text() } else { theme::text_dim() };
                    let mut job = LayoutJob::default();
                    for (offset, ch) in c.label.char_indices() {
                        let hit = m.highlights.contains(&offset);
                        let format = TextFormat {
                            font_id: egui::FontId::proportional(13.0),
                            color: if hit { accent } else { text_color },
                            ..Default::default()
                        };
                        job.append(&ch.to_string(), 0.0, format);
                    }
                    let galley = ui.fonts(|f| f.layout_job(job));
                    ui.painter().galley(rect.left_center() + Vec2::new(8.0, -galley.size().y * 0.5), galley, text_color);
                    let right = match &c.shortcut {
                        Some(keys) => format!("{}   {}", c.category.label(), keys),
                        None => c.category.label().to_string(),
                    };
                    ui.painter().text(
                        rect.right_center() - Vec2::new(8.0, 0.0),
                        Align2::RIGHT_CENTER,
                        right,
                        egui::FontId::proportional(11.0),
                        theme::text_dim(),
                    );
                    if row.clicked() {
                        run = Some(c.id.clone());
                    }
                }
                ui.add_space(2.0);
                ui.small("Up / Down to pick, Enter to run, Esc to close");
            });
        let clicked_outside = response.is_some_and(|r| r.response.clicked_elsewhere());
        if escape || clicked_outside || run.is_some() {
            self.ws.palette.close();
        }
        if let Some(id) = run {
            self.run_command(&id);
        }
    }

    // ---- theme editor -----------------------------------------------------------------

    pub(crate) fn theme_editor_window(&mut self, ctx: &egui::Context) {
        if !self.ws.theme_editor {
            return;
        }
        let mut open = true;
        egui::Window::new("Theme").open(&mut open).resizable(false).collapsible(false).show(ctx, |ui| {
            let lib = &mut self.ws.themes;
            let mut pick = None;
            egui::ComboBox::from_label("Theme").selected_text(&lib.current().name).show_ui(ui, |ui| {
                for (i, t) in lib.themes.iter().enumerate() {
                    if ui.selectable_label(i == lib.active, &t.name).clicked() {
                        pick = Some(i);
                    }
                }
            });
            if let Some(i) = pick {
                lib.select(i);
            }
            let custom = lib.active >= 3;
            if custom {
                let mut name = lib.current().name.clone();
                if ui
                    .horizontal(|ui| {
                        ui.label("Name");
                        ui.text_edit_singleline(&mut name).changed()
                    })
                    .inner
                {
                    lib.edit().name = name;
                }
            } else {
                ui.small("Editing a built-in theme makes a copy of it.");
            }
            ui.separator();
            let mut colors = lib.current().colors;
            let mut changed: Option<(usize, Rgba)> = None;
            egui::Grid::new("theme_colors").num_columns(4).spacing([10.0, 4.0]).show(ui, |ui| {
                for (k, (label, value)) in colors.fields_mut().into_iter().enumerate() {
                    let mut c = color32(*value);
                    if ui.color_edit_button_srgba(&mut c).changed() {
                        let [r, g, b, a] = c.to_srgba_unmultiplied();
                        changed = Some((k, Rgba([r, g, b, a])));
                    }
                    ui.label(label);
                    if k % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
            if let Some((k, value)) = changed {
                *lib.edit().colors.fields_mut()[k].1 = value;
            }
            let (mut size, mut rounding) = (lib.current().font_size, lib.current().rounding as f32);
            if ui.add(egui::Slider::new(&mut size, 10.0..=18.0).step_by(0.5).text("Text size")).changed() {
                lib.edit().font_size = size;
            }
            if ui.add(egui::Slider::new(&mut rounding, 0.0..=8.0).step_by(1.0).text("Rounding")).changed() {
                lib.edit().rounding = rounding as u8;
            }
            let warnings = lib.current().contrast_warnings();
            if !warnings.is_empty() {
                ui.separator();
                for w in warnings {
                    ui.colored_label(color32(lib.current().colors.warning), w);
                }
            }
            ui.separator();
            let mut import = false;
            let mut export = false;
            let mut delete = false;
            ui.horizontal(|ui| {
                import = ui.button("Import...").on_hover_text("A theme JSON file; missing colours come from its base").clicked();
                export = ui.button("Export...").clicked();
                delete = ui.add_enabled(custom, egui::Button::new("Delete")).clicked();
            });
            if import {
                if let Some(path) =
                    crate::files::FileDialog::new().add_filter("Theme", &["json"]).purpose(crate::files::Purpose::Theme).pick_file()
                {
                    self.import_theme_from(&path);
                }
            }
            let lib = &mut self.ws.themes;
            if export {
                let name = format!("{}.json", lib.current().name);
                if let Some(path) = crate::files::FileDialog::new().add_filter("Theme", &["json"]).set_file_name(name).save_file() {
                    self.status = match std::fs::write(&path, lib.current().to_json()) {
                        Ok(()) => {
                            crate::files::offer_export(&path);
                            format!("Theme saved to {}", path.display())
                        }
                        Err(err) => format!("Couldn't save the theme: {err}"),
                    };
                }
            }
            let lib = &mut self.ws.themes;
            if delete && lib.active >= 3 {
                lib.themes.remove(lib.active);
                lib.active = 0;
                lib.revision += 1;
            }
        });
        if !open {
            self.ws.theme_editor = false;
        }
    }
}
