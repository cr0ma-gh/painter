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
use ui_workspace::tabs::{Chrome, WorkspaceSet, UI_SCALE_RANGE};
use ui_workspace::themes::{Rgba, ThemeLibrary};
use ui_workspace::tiles::{self, AreaId, EditorType, SplitKind};

use crate::app::PainterApp;
use crate::theme;
use crate::advanced_tool::BrushFormat;
use crate::tools::{BrushGroup, Tool};

const WORKSPACES_KEY: &str = "workspaces_v1";
const CHROME_KEY: &str = "chrome_v1";
const THEMES_KEY: &str = "themes_v1";
const TOOLBAR_KEY: &str = "toolbar_v1";
const COMMAND_BAR_KEY: &str = "command_bar_v1";
const BARS_KEY: &str = "bars_v1";
const WELCOME_KEY: &str = "welcome_v1";
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
    /// The egui zoom last set from the interface scale (to notice Ctrl +/- changing it).
    applied_zoom: Option<f32>,
    /// Where each docked area was last drawn (to place a panel that floats off it).
    pub(crate) area_rects: Vec<(AreaId, Rect)>,
    /// The tool strip's buttons, in order (see `toolbar.rs`).
    pub(crate) toolbar: Vec<crate::toolbar::Item>,
    /// Customize Toolbar is open, and its command search.
    pub(crate) toolbar_editing: bool,
    pub(crate) toolbar_search: String,
    /// The command bar's buttons under the menu (commands and separators).
    pub(crate) command_bar: Vec<crate::toolbar::Item>,
    pub(crate) command_bar_editing: bool,
    pub(crate) command_bar_search: String,
    /// Which of the tool strip and the command bar are shown.
    pub(crate) bars: crate::toolbar::Bars,
    /// Show the welcome screen at startup.
    pub(crate) welcome: bool,
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
            applied_zoom: None,
            area_rects: Vec::new(),
            toolbar: get(TOOLBAR_KEY)
                .and_then(|j| serde_json::from_str::<Vec<String>>(&j).ok())
                .map_or_else(crate::toolbar::default_items, |keys| crate::toolbar::from_keys(&keys)),
            toolbar_editing: false,
            toolbar_search: String::new(),
            command_bar: get(COMMAND_BAR_KEY)
                .and_then(|j| serde_json::from_str::<Vec<String>>(&j).ok())
                .map_or_else(crate::toolbar::default_command_bar, |keys| crate::toolbar::command_bar_from_keys(&keys)),
            command_bar_editing: false,
            command_bar_search: String::new(),
            bars: get(BARS_KEY).and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default(),
            welcome: get(WELCOME_KEY).and_then(|j| serde_json::from_str(&j).ok()).unwrap_or(true),
            renaming: None,
            settings_file,
            written,
            checked: None,
        }
    }

    /// Everything back as Painter first starts: layout, panels, toolbars,
    /// theme, interface size, welcome screen. Saved like any change.
    pub(crate) fn reset(&mut self) {
        self.set = WorkspaceSet::default();
        self.chrome = Chrome::default();
        self.themes = ThemeLibrary::default();
        self.toolbar = crate::toolbar::default_items();
        self.command_bar = crate::toolbar::default_command_bar();
        self.bars = Default::default();
        self.welcome = true;
        self.applied_theme = None;
        self.applied_zoom = None;
        self.area_rects.clear();
        self.renaming = None;
        self.toolbar_editing = false;
        self.command_bar_editing = false;
        self.theme_editor = false;
    }

    fn entries(&self) -> [(&'static str, String); 7] {
        [
            (WORKSPACES_KEY, self.set.to_json()),
            (CHROME_KEY, serde_json::to_string(&self.chrome).unwrap_or_default()),
            (THEMES_KEY, self.themes.to_json()),
            (TOOLBAR_KEY, serde_json::to_string(&self.toolbar.iter().map(crate::toolbar::Item::key).collect::<Vec<_>>()).unwrap_or_default()),
            (COMMAND_BAR_KEY, serde_json::to_string(&self.command_bar.iter().map(crate::toolbar::Item::key).collect::<Vec<_>>()).unwrap_or_default()),
            (BARS_KEY, serde_json::to_string(&self.bars).unwrap_or_default()),
            (WELCOME_KEY, serde_json::to_string(&self.welcome).unwrap_or_default()),
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
    /// Fold (true) or unfold the area.
    Fold(bool),
    /// Detach this editor into a floating window.
    Float(EditorType),
    /// The area's own scale.
    Scale(f32),
}

/// `style` with text, spacing and widget sizes multiplied by `scale`: one
/// panel's own zoom (icons follow through [`crate::icons::panel_factor`]).
pub(crate) fn scaled_style(style: &egui::Style, scale: f32) -> egui::Style {
    let mut s = style.clone();
    for font in s.text_styles.values_mut() {
        font.size *= scale;
    }
    let sp = &mut s.spacing;
    sp.item_spacing *= scale;
    sp.button_padding *= scale;
    sp.interact_size *= scale;
    sp.indent *= scale;
    sp.slider_width *= scale;
    sp.slider_rail_height *= scale;
    sp.combo_width *= scale;
    sp.text_edit_width *= scale;
    sp.icon_width *= scale;
    sp.icon_width_inner *= scale;
    sp.icon_spacing *= scale;
    sp.combo_height *= scale;
    sp.scroll.bar_width *= scale;
    s
}

/// Ctrl + Alt + wheel over `body`: the panel's new scale, if it changed.
fn panel_scale_wheel(ui: &egui::Ui, body: Rect, scale: f32) -> Option<f32> {
    if !ui.rect_contains_pointer(body) {
        return None;
    }
    let (wheel, held) = ui.input(|i| (i.raw_scroll_delta.y, i.modifiers.ctrl && i.modifiers.alt));
    if !held || wheel.abs() < 0.5 {
        return None;
    }
    let step = if wheel > 0.0 { 1.1 } else { 1.0 / 1.1 };
    let next = tiles::clamp_panel_scale((scale * step * 20.0).round() / 20.0);
    (next != scale).then_some(next)
}

/// A folded area: a header strip (icon, name) or, beside a side-by-side
/// neighbour, a narrow column (icon only). A click unfolds it.
fn folded_area_ui(ui: &mut egui::Ui, tree: &tiles::TileTree, id: AreaId, rect: Rect) -> Option<AreaAction> {
    let editor = tree.area(id)?.editor();
    let response = ui.interact(rect, ui.id().with(("folded", id.0)), Sense::click());
    let fill = if response.hovered() { theme::hover() } else { theme::title() };
    ui.painter().rect_filled(rect, 0.0, fill);
    let text = theme::text();
    let size = HEADER - 8.0;
    let icon = crate::icons::editor_icon(editor);
    if rect.width() >= rect.height() {
        let chevron = Rect::from_min_size(rect.min + Vec2::new(4.0, 0.0), Vec2::new(size, rect.height().min(HEADER)));
        crate::icons::paint_glyph(ui.painter(), chevron, size - 4.0, crate::icons::Glyph::Unfold, text);
        let icon_rect = chevron.translate(Vec2::new(size + 4.0, 0.0));
        icon.paint(ui, icon_rect, size, text);
        if rect.width() > 3.0 * HEADER {
            let at = egui::pos2(icon_rect.max.x + 6.0, chevron.center().y);
            ui.painter().text(at, egui::Align2::LEFT_CENTER, editor.title(), egui::FontId::proportional(11.0), text);
        }
    } else {
        let icon_rect = Rect::from_min_size(rect.min + Vec2::new(0.0, 2.0), Vec2::new(rect.width(), HEADER));
        icon.paint(ui, icon_rect, size, text);
        let chevron = icon_rect.translate(Vec2::new(0.0, HEADER));
        crate::icons::paint_glyph(ui.painter(), chevron, size - 6.0, crate::icons::Glyph::UnfoldSide, text);
    }
    response.on_hover_text(format!("{}: click to unfold", editor.title())).clicked().then_some(AreaAction::Fold(false))
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
        self.apply_ui_scale(ctx);
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

    /// The interface size: everything (text, icons, panels, lines) scales
    /// together, like Blender's Resolution Scale. iPadOS starts a step
    /// larger for fingers. egui's own Ctrl + / Ctrl − / Ctrl 0 change it too.
    fn apply_ui_scale(&mut self, ctx: &egui::Context) {
        let base = if crate::MOBILE { 1.25 } else { 1.0 };
        let (lo, hi) = UI_SCALE_RANGE;
        let zoom = ctx.zoom_factor();
        if let Some(applied) = self.ws.applied_zoom {
            if (zoom - applied).abs() > 1e-3 {
                self.ws.chrome.ui_scale = zoom / base;
            }
        }
        let scale = self.ws.chrome.ui_scale;
        self.ws.chrome.ui_scale = if scale.is_finite() { scale.clamp(lo, hi) } else { 1.0 };
        let target = base * self.ws.chrome.ui_scale;
        if (zoom - target).abs() > 1e-3 {
            ctx.set_zoom_factor(target);
        }
        self.ws.applied_zoom = Some(target);
    }

    /// Steps the interface size up (`+1`), down (`-1`) or back to 100% (`0`).
    pub(crate) fn step_ui_scale(&mut self, step: i32) {
        let (lo, hi) = UI_SCALE_RANGE;
        let s = &mut self.ws.chrome.ui_scale;
        *s = match step {
            0 => 1.0,
            _ => ((*s * 10.0).round() + step.signum() as f32) / 10.0,
        }
        .clamp(lo, hi);
        self.status = format!("Interface size {:.0}%", *s * 100.0);
    }

    /// The interface size slider with presets (the tab bar's popup and the Window menu).
    pub(crate) fn ui_scale_controls(&mut self, ui: &mut egui::Ui) {
        let (lo, hi) = UI_SCALE_RANGE;
        let mut percent = self.ws.chrome.ui_scale * 100.0;
        // Applied when the drag ends, so the slider doesn't move under the pointer.
        let slider = ui.add(egui::Slider::new(&mut percent, lo * 100.0..=hi * 100.0).step_by(5.0).suffix("%").text("Interface"));
        if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
            self.ws.chrome.ui_scale = percent / 100.0;
        }
        ui.horizontal_wrapped(|ui| {
            for p in [75, 100, 125, 150, 200] {
                if ui.selectable_label((self.ws.chrome.ui_scale * 100.0).round() as i32 == p, format!("{p}%")).clicked() {
                    self.ws.chrome.ui_scale = p as f32 / 100.0;
                }
            }
        });
        ui.small("Ctrl + and Ctrl − change it, Ctrl 0 resets it.");
    }

    /// Loads a theme JSON file into the library and switches to it.
    pub(crate) fn import_theme_from(&mut self, path: &std::path::Path) {
        let lib = &mut self.ws.themes;
        self.status = match std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|j| lib.import(&j)) {
            Ok(_) => format!("Theme loaded from {}", path.display()),
            Err(err) => format!("Couldn't load the theme: {err}"),
        };
    }

    /// Photoshop's screen modes (F cycles them, Shift+F back): 0 standard,
    /// 1 full screen with menus and panels, 2 full screen with the canvas only.
    pub(crate) fn set_screen_mode(&mut self, ctx: &egui::Context, mode: u8) {
        let mode = mode % 3;
        if !crate::MOBILE {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(mode > 0));
        }
        let want_zen = mode == 2;
        if self.ws.chrome.zen != want_zen {
            self.ws.chrome.toggle_zen();
        }
        self.screen_mode = mode;
        self.status = match mode {
            0 => "Standard screen mode".into(),
            1 => "Full screen with menus (F: canvas only, Shift+F: back)".into(),
            _ => "Full screen, canvas only (F: standard again; Tab shows the panels)".into(),
        };
    }

    pub(crate) fn cycle_screen_mode(&mut self, ctx: &egui::Context, forward: bool) {
        let next = if forward { self.screen_mode + 1 } else { self.screen_mode + 2 };
        self.set_screen_mode(ctx, next);
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
        let size = if crate::MOBILE { 44.0 } else { 28.0 };
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
                        use crate::icons::{glyph_button, Glyph};
                        let size = 14.0;
                        if glyph_button(ui, Glyph::FullScreen, size, "Zen mode: only the canvas (Tab or F11)").clicked() {
                            self.toggle_zen();
                        }
                        if glyph_button(ui, Glyph::Search, size, "Command palette (F3 or Ctrl+Space)").clicked() {
                            self.open_palette();
                        }
                        let scale = glyph_button(ui, Glyph::Scale, size, "Interface size (Ctrl + / Ctrl − / Ctrl 0)");
                        let popup = ui.make_persistent_id("ui_scale_popup");
                        if scale.clicked() {
                            ui.memory_mut(|m| m.toggle_popup(popup));
                        }
                        egui::popup_below_widget(ui, popup, &scale, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                            ui.set_min_width(220.0);
                            self.ui_scale_controls(ui);
                        });
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
            let (areas, borders) = tree.layout_with(from_egui(full), HEADER);
            self.ws.area_rects = areas.iter().map(|a| (a.id, to_egui(a.rect))).collect();
            let mut canvas_shown = false;
            let mut actions = Vec::new();
            for a in &areas {
                let action = if a.collapsed {
                    folded_area_ui(ui, &tree, a.id, to_egui(a.rect))
                } else {
                    self.area_ui(ui, &tree, a.id, to_egui(a.rect), &mut canvas_shown)
                };
                if let Some(action) = action {
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
        // Narrow areas show icons only.
        let roomy = rect.width() >= 200.0;
        let icon_size = HEADER - 8.0;
        let text = theme::text();
        // The editor-type menu: the editor's icon, and its name when there's room.
        let type_menu = |ui: &mut egui::Ui, action: &mut Option<AreaAction>| {
            let image = crate::icons::editor_icon(editor).image(icon_size, text);
            let title = if roomy && area.editors.len() == 1 { editor.title() } else { "" };
            ui.menu_image_text_button(image, egui::RichText::new(title).small(), |ui| {
                for e in EditorType::ALL {
                    let item = egui::Button::image_and_text(crate::icons::editor_icon(e).image(14.0, text), e.title()).selected(e == editor);
                    if ui.add(item).clicked() {
                        *action = Some(AreaAction::SetEditor(e));
                        ui.close_menu();
                    }
                }
            })
            .response
            .on_hover_text(format!("{}: change what this area shows", editor.title()));
        };
        if area.editors.len() > 1 {
            // Stacked tabs, as Photoshop's panel groups: every tab by name when
            // they fit (the shown one framed), icons only when they don't.
            let font = egui::TextStyle::Small.resolve(head.style());
            let names: f32 = area
                .editors
                .iter()
                .map(|e| head.fonts(|f| f.layout_no_wrap(e.title().to_string(), font.clone(), text).size().x) + 16.0)
                .sum();
            let fits = names + 36.0 <= header.width();
            head.spacing_mut().button_padding.x = 4.0;
            for (i, e) in area.editors.iter().enumerate() {
                let shown = i == area.active;
                if i > 0 && fits {
                    head.add_space(2.0);
                    let x = head.cursor().left();
                    let y = header.center().y;
                    head.painter().vline(x, (y - 5.0)..=(y + 5.0), egui::Stroke::new(1.0_f32, theme::line()));
                    head.add_space(3.0);
                }
                let tab = if fits {
                    let label = egui::RichText::new(e.title()).small().color(if shown { theme::strong() } else { theme::text_dim() });
                    head.add(egui::Button::new(label).selected(shown).frame(shown))
                } else {
                    let image = crate::icons::editor_icon(*e).image(icon_size, text);
                    let label = if shown && roomy { e.title() } else { "" };
                    head.add(egui::Button::image_and_text(image, egui::RichText::new(label).small()).selected(shown).frame(shown))
                };
                if tab.on_hover_text(e.title()).clicked() {
                    action = Some(AreaAction::ShowTab(*e));
                }
            }
        } else {
            type_menu(&mut head, &mut action);
        }
        // Viewports (the canvas, a reference) don't fold.
        let can_fold = many_areas && !area.editors.iter().any(|e| e.is_viewport());
        head.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if can_fold && crate::icons::glyph_button(ui, crate::icons::Glyph::Fold, icon_size - 4.0, "Fold this area to its header").clicked() {
                action = Some(AreaAction::Fold(true));
            }
            ui.menu_button(egui::RichText::new("···").small(), |ui| {
                // With tabs the header has no type menu: change the shown panel from here.
                if area.editors.len() > 1 {
                    ui.menu_button("Show Instead", |ui| {
                        for e in EditorType::ALL {
                            let item = egui::Button::image_and_text(crate::icons::editor_icon(e).image(14.0, text), e.title()).selected(e == editor);
                            if ui.add(item).clicked() {
                                action = Some(AreaAction::SetEditor(e));
                                ui.close_menu();
                            }
                        }
                    });
                }
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
                let float = ui.add_enabled(editor != EditorType::Canvas, egui::Button::new("Float"));
                if float.on_hover_text("Detach this panel into its own window").clicked() {
                    action = Some(AreaAction::Float(editor));
                    ui.close_menu();
                }
                if !editor.is_viewport() {
                    ui.separator();
                    let mut percent = area.scale * 100.0;
                    let (lo, hi) = tiles::PANEL_SCALE;
                    let r = ui.add(egui::Slider::new(&mut percent, lo * 100.0..=hi * 100.0).step_by(5.0).suffix("%").text("Panel scale"));
                    if r.drag_stopped() || (r.changed() && !r.dragged()) {
                        action = Some(AreaAction::Scale(percent / 100.0));
                    }
                    if ui.button("Panel Scale 100%").on_hover_text("Ctrl + Alt + wheel over a panel changes its scale").clicked() {
                        action = Some(AreaAction::Scale(1.0));
                        ui.close_menu();
                    }
                }
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
        if !editor.is_viewport() {
            // The panel's own scale, and Ctrl + Alt + wheel to change it (Blender zooms a region with Ctrl + middle drag).
            if (area.scale - 1.0).abs() > 1e-3 {
                content.set_style(scaled_style(content.style(), area.scale));
            }
            if let Some(scale) = panel_scale_wheel(ui, body, area.scale) {
                action = Some(AreaAction::Scale(scale));
            }
        }
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

    /// Panels detached into their own windows (the area menu's Float, or
    /// Window → Panels). Each keeps its place, size and own scale; the title
    /// bar folds it, the dock icon puts it back into the layout, × hides it.
    /// They stay in zen mode, which only hides the docked layout and the bars.
    pub(crate) fn floating_panels(&mut self, ctx: &egui::Context) {
        let panels = self.ws.set.current().floating.clone();
        let mut dock = None;
        let mut close = None;
        let mut moved = Vec::new();
        for panel in &panels {
            let mut open = true;
            let shown = egui::Window::new(egui::RichText::new(panel.editor.title()).small())
                .id(egui::Id::new(("floating_panel", self.ws.set.active, panel.id)))
                .default_pos(egui::pos2(panel.pos[0], panel.pos[1]))
                .default_size(Vec2::new(panel.size[0], panel.size[1]))
                .resizable(true)
                .collapsible(true)
                .open(&mut open)
                .frame(egui::Frame::window(&ctx.style()).fill(theme::panel()).inner_margin(4))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        let (at, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                        crate::icons::editor_icon(panel.editor).paint(ui, at, 14.0, theme::text_dim());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if crate::icons::glyph_button(ui, crate::icons::Glyph::Dock, 12.0, "Dock: put the panel back into the layout").clicked() {
                                dock = Some(panel.id);
                            }
                            let mut percent = panel.scale * 100.0;
                            let (lo, hi) = tiles::PANEL_SCALE;
                            let r = ui.add(egui::DragValue::new(&mut percent).range(lo * 100.0..=hi * 100.0).speed(1.0).suffix("%"));
                            if r.on_hover_text("Panel scale (Ctrl + Alt + wheel over the panel)").changed() {
                                moved.push((panel.id, None, Some(percent / 100.0)));
                            }
                        });
                    });
                    let body = ui.available_rect_before_wrap();
                    if let Some(scale) = panel_scale_wheel(ui, body, panel.scale) {
                        moved.push((panel.id, None, Some(scale)));
                    }
                    if (panel.scale - 1.0).abs() > 1e-3 {
                        ui.set_style(scaled_style(ui.style(), panel.scale));
                    }
                    if panel.editor == EditorType::Reference {
                        ui.set_min_size(Vec2::new(200.0, 160.0));
                        self.reference_view_ui(ui);
                    } else {
                        egui::ScrollArea::vertical().id_salt(("floating_scroll", panel.id)).auto_shrink([false, false]).show(ui, |ui| {
                            self.editor_ui(ui, panel.editor);
                        });
                    }
                });
            if let Some(shown) = shown {
                let r = shown.response.rect;
                if (r.min.x - panel.pos[0]).abs() > 0.5 || (r.min.y - panel.pos[1]).abs() > 0.5 || (r.width() - panel.size[0]).abs() > 0.5 {
                    moved.push((panel.id, Some(r), None));
                }
            }
            if !open {
                close = Some(panel.id);
            }
        }
        let workspace = self.ws.set.current_mut();
        for (id, rect, scale) in moved {
            if let Some(p) = workspace.floating.iter_mut().find(|p| p.id == id) {
                if let Some(r) = rect {
                    p.pos = [r.min.x, r.min.y];
                    p.size = [r.width(), r.height()];
                }
                if let Some(s) = scale {
                    p.scale = tiles::clamp_panel_scale(s);
                }
            }
        }
        if let Some(id) = dock {
            workspace.dock(id);
        }
        if let Some(id) = close {
            workspace.floating.retain(|p| p.id != id);
        }
    }

    /// Shows `editor`: brings its docked tab forward, or opens it floating.
    pub(crate) fn show_panel(&mut self, editor: EditorType, ctx: &egui::Context) {
        let workspace = self.ws.set.current_mut();
        if let Some(area) = workspace.tiles.find(editor) {
            workspace.tiles.add_tab(area, editor);
            workspace.tiles.set_collapsed(area, false);
        } else if !workspace.floating.iter().any(|p| p.editor == editor) {
            let c = ctx.screen_rect().center();
            let k = (workspace.floating.len() % 8) as f32 * 32.0;
            workspace.add_floating(editor, [c.x - 260.0 + k, c.y - 260.0 + k], [280.0, 360.0], 1.0);
        }
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
                    if ui.button("New 3D layer from the perspective ruler").clicked() {
                        self.new_scene_layer_from_ruler();
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
            EditorType::ColorSlider => self.color_slider_panel(ui),
            EditorType::GamutSelector => self.gamut_selector_panel(ui),
            EditorType::ColorSet => self.color_set_panel(ui),
            EditorType::ColorHistory => self.color_history_panel(ui),
            EditorType::IntermediateColor => self.intermediate_color_panel(ui),
            EditorType::ApproximateColor => self.approximate_color_panel(ui),
            EditorType::ColorMixing => self.color_mixing_panel(ui),
            EditorType::SearchLayer => self.search_layer_panel(ui),
            EditorType::History => self.history_panel(ui),
            EditorType::Channels => self.channels_panel(ui),
            EditorType::Actions => self.actions_panel(ui),
            EditorType::Session => self.session_panel(ui),
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
            AreaAction::Fold(fold) => {
                tiles.set_collapsed(id, fold);
            }
            AreaAction::Scale(scale) => {
                tiles.set_scale(id, scale);
            }
            AreaAction::Float(editor) => {
                let at = self.ws.area_rects.iter().find(|(a, _)| *a == id).map(|(_, r)| *r);
                let (pos, size) = at.map_or(([200.0, 150.0], [280.0, 360.0]), |r| ([r.min.x + 24.0, r.min.y + 24.0], [r.width().max(220.0), r.height().max(240.0)]));
                let workspace = self.ws.set.current_mut();
                if workspace.float(id, editor, pos, size).is_none() {
                    self.status = "The last panel stays docked".into();
                }
                return;
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

    /// Docked (as any tab) or floating.
    pub(crate) fn editor_open(&self, editor: EditorType) -> bool {
        self.ws.set.current().shows(editor)
    }

    /// Window menu: hides a shown panel; a hidden one opens in a floating
    /// window (Float / Dock in its menus move it in and out of the layout).
    pub(crate) fn toggle_editor(&mut self, editor: EditorType, ctx: &egui::Context) {
        let workspace = self.ws.set.current_mut();
        if workspace.floating.iter().any(|p| p.editor == editor) {
            workspace.floating.retain(|p| p.editor != editor);
            return;
        }
        match workspace.tiles.find(editor) {
            Some(area) if editor != EditorType::Canvas => {
                workspace.tiles.close_tab(area, editor);
            }
            Some(_) => {}
            None => self.show_panel(editor, ctx),
        }
    }

    // ---- command palette --------------------------------------------------------------

    pub(crate) fn open_palette(&mut self) {
        self.refresh_commands();
        self.ws.palette.open();
    }

    /// Rebuilds the command list from the app's current state (brushes,
    /// workspaces, themes and what's enabled change as it runs).
    pub(crate) fn refresh_commands(&mut self) {
        use CommandCategory as C;
        let r = &mut self.ws.commands;
        r.unregister_prefix("");
        let has_sel = self.doc.selection.is_some();
        let has_mask = self.doc.active_layer().is_some_and(|l| l.mask.is_some());
        let fixed: [FixedCommand; 80] = [
            ("file.new", "New Canvas...", C::File, "Ctrl+N", &["document", "create"], true),
            ("file.new_clipboard", "New from Clipboard", C::File, "Ctrl+Alt+N", &["paste", "clipboard", "screenshot", "document", "create"], true),
            ("file.comic", "New Comic Page / Storyboard...", C::File, "", &["manga", "panels", "template"], true),
            ("file.open", "Open...", C::File, "Ctrl+O", &["load"], true),
            ("file.session", "Shared Session...", C::File, "", &["collaborate", "drawpile", "together", "multiplayer", "network", "host", "join"], true),
            ("file.save", "Save", C::File, "Ctrl+S", &[], true),
            ("file.save_as", "Save As...", C::File, "Ctrl+Shift+S", &[], true),
            ("file.types", "Set Up File Types...", C::File, "", &["file association", "thumbnails", "explorer", "open with", "default app"], true),
            ("file.import_png", "Import PNG as Layer...", C::File, "", &["image", "picture"], true),
            ("file.scan", "Import from Scanner or Camera...", C::File, "", &["scan", "wia", "camera", "photo", "document"], true),
            ("file.import_brushes", "Import Brushes...", C::Brush, "", &["sut", "abr", "clip studio", "photoshop", "krita", "kpp", "bundle", "mypaint", "myb", "gimp", "gbr"], true),
            ("file.export_png", "Export PNG...", C::File, "", &["save image"], true),
            ("file.export_psd", "Export PSD...", C::File, "", &["photoshop"], true),
            ("file.export_kra", "Export Krita (.kra)...", C::File, "", &["krita", "kra"], true),
            ("file.export_ora", "Export OpenRaster (.ora)...", C::File, "", &["openraster", "mypaint", "gimp", "ora"], true),
            ("file.export_clip", "Export Clip Studio (.clip)...", C::File, "", &["clip studio", "csp", "clip"], true),
            ("file.export_svg", "Export SVG...", C::File, "", &["vector"], true),
            ("file.export_brushes", "Export All Brushes...", C::Brush, "", &[], true),
            ("file.quit", "Quit", C::File, "Ctrl+Q", &["exit", "close"], !cfg!(target_os = "ios")),
            ("help.welcome", "Welcome Screen", C::View, "", &["start", "splash", "features"], true),
            ("help.about", "About Painter", C::View, "", &["version", "about", "help"], true),
            ("help.reset_settings", "Reset Settings...", C::View, "", &["preferences", "defaults", "restore", "factory", "layout"], true),
            ("help.updates", "Check for Updates...", C::View, "", &["update", "upgrade", "new version", "release"], true),
            ("edit.undo", "Undo", C::Edit, "Ctrl+Z", &[], self.history.can_undo()),
            ("edit.redo", "Redo", C::Edit, "Ctrl+Shift+Z", &[], self.history.can_redo()),
            ("edit.image_size", "Image Size...", C::Edit, "", &["resize", "scale", "resample", "pixel art", "nearest", "resolution"], true),
            ("edit.clear", "Clear Selected", C::Edit, "Del", &["delete", "erase"], has_sel),
            ("edit.define_pattern", "Define Pattern", C::Edit, "", &["pattern", "stamp", "texture", "tile"], true),
            ("image.mode.0", "Mode: RGB Color", C::Edit, "", &["colour mode", "color mode", "rgb"], true),
            ("image.mode.1", "Mode: Grayscale", C::Edit, "", &["colour mode", "greyscale", "black and white", "desaturate"], true),
            ("image.mode.2", "Mode: Bitmap...", C::Edit, "", &["colour mode", "1 bit", "dither", "halftone", "monochrome"], true),
            ("select.all", "Select All", C::Select, "Ctrl+A", &[], true),
            ("select.none", "Deselect", C::Select, "Ctrl+D", &["clear selection"], has_sel),
            ("select.invert", "Invert Selection", C::Select, "Ctrl+Shift+I", &[], true),
            ("select.save_channel", "Save Selection as Channel", C::Select, "", &["alpha channel", "store", "keep selection"], has_sel),
            ("layer.raster", "New Raster Layer", C::Layer, "", &["add layer"], true),
            ("layer.vector", "New Vector Layer", C::Layer, "", &[], true),
            ("layer.folder", "New Folder", C::Layer, "", &["group"], true),
            ("layer.gradient", "New Gradient Layer", C::Layer, "", &[], true),
            ("layer.scene3d", "New 3D Layer from Model...", C::Layer, "", &["blend", "obj", "blender", "fbx", "gltf", "glb"], true),
            ("layer.scene3d_ruler", "New 3D Layer from Perspective Ruler", C::Layer, "", &["3d space", "vanishing point", "camera"], true),
            ("layer.file_object", "New File Object Layer...", C::Layer, "", &["smart object", "linked", "place", "embed", "reference file"], true),
            ("layer.tone", "New Tone Layer (from selection)", C::Layer, "", &["screentone", "halftone"], has_sel),
            ("layer.add_mask", "Add Layer Mask", C::Layer, "", &[], !has_mask),
            ("layer.delete_mask", "Delete Layer Mask", C::Layer, "", &[], has_mask),
            ("layer.delete", "Delete Layer", C::Layer, "", &["remove layer"], true),
            ("view.fit", "Fit to Window", C::View, "0", &["zoom"], true),
            ("view.actual", "Actual Pixels (100%)", C::View, "", &["zoom 100"], true),
            ("view.rotate_left", "Rotate View Left", C::View, ",", &["turn"], true),
            ("view.rotate_right", "Rotate View Right", C::View, ".", &["turn"], true),
            ("view.reset_rotation", "Reset Rotation", C::View, "", &[], true),
            ("view.tool_strip", "Show Tool Strip", C::View, "", &["toolbar", "tools", "hide", "left"], true),
            ("view.command_bar", "Show Command Bar", C::View, "", &["toolbar", "icons", "hide", "top"], true),
            ("view.flip", "Flip View Horizontally", C::View, "", &["mirror", "check the drawing"], true),
            ("view.grid", "Show Grid", C::View, "Ctrl+'", &["guides"], true),
            ("rulers.snap", "Snap to Rulers", C::View, "Ctrl+1", &["perspective", "symmetry", "ruler"], true),
            ("view.zen", "Toggle Zen Mode", C::View, "Tab / F11", &["fullscreen", "hide panels", "distraction free"], true),
            ("view.screen.0", "Screen Mode: Standard", C::View, "", &["window", "screen mode"], true),
            ("view.screen.1", "Screen Mode: Full Screen with Menus", C::View, "F", &["fullscreen", "screen mode"], true),
            ("view.screen.2", "Screen Mode: Full Screen", C::View, "", &["fullscreen", "canvas only", "presentation", "screen mode"], true),
            ("view.ruler_bar", "Ruler Bar", C::View, "Ctrl+R", &["rulers", "measure", "guides", "units"], true),
            ("view.brush_crosshair", "Brush Crosshair On / Off", C::View, "", &["cursor", "pointer", "cross"], true),
            ("view.clock", "Clock and Battery On / Off", C::View, "", &["time", "hour", "battery", "charge", "widget"], true),
            ("view.modifier_keys", "On-Screen Modifier Keys", C::View, "", &["ctrl", "shift", "alt", "touch", "keyboard"], true),
            ("layer.duplicate", "Duplicate Layers", C::Layer, "Ctrl+J", &["copy", "clone"], true),
            ("layer.group", "Group Layers into Folder", C::Layer, "Ctrl+G", &["folder", "group"], true),
            ("layer.draft", "Toggle Draft Layer", C::Layer, "", &["draft", "sketch", "export", "hide in export"], true),
            ("color.transparent", "Transparent Colour", C::Brush, "", &["alpha", "erase", "clear", "transparency"], true),
            ("brush.advanced", "Advanced Tool Settings...", C::Brush, "", &["brush", "export", "abr", "sut", "photoshop", "clip studio", "spray", "jitter"], true),
            ("brush.export_abr", "Export Brush for Photoshop (.abr)...", C::Brush, "", &["photoshop", "abr"], true),
            ("brush.export_sut", "Export Brush for Clip Studio (.sut)...", C::Brush, "", &["clip studio", "sut", "csp"], true),
            ("brush.export_krita", "Export Brush for Krita (.bundle)...", C::Brush, "", &["krita", "kpp", "bundle"], true),
            ("brush.export_mypaint", "Export Brush for MyPaint (.myb)...", C::Brush, "", &["mypaint", "myb", "drawpile"], true),
            ("brush.export_gimp", "Export Brush for GIMP (.gbr / .vbr)...", C::Brush, "", &["gimp", "gbr", "vbr"], true),
            ("view.ui_bigger", "Bigger Interface", C::View, "Ctrl +", &["scale", "zoom", "size", "resolution"], true),
            ("view.ui_smaller", "Smaller Interface", C::View, "Ctrl -", &["scale", "zoom", "size", "resolution"], true),
            ("view.ui_reset", "Interface Size 100%", C::View, "Ctrl 0", &["scale", "zoom", "size", "resolution", "reset"], true),
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
        for (i, tool) in Tool::COMMANDS.iter().enumerate() {
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
        for (i, a) in self.actions.list.iter().enumerate() {
            r.register(Command::new(&format!("action.{i}"), &format!("Play Action: {}", a.name), C::Edit).keywords(&["action", "macro", "batch", "repeat"]));
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
        self.actions.record(id);
        self.next_label = self.ws.commands.get(id).map(|c| c.label.trim_end_matches("...").trim_end_matches('…').to_string());
        let index = |prefix: &str| id.strip_prefix(prefix).and_then(|n| n.parse::<usize>().ok());
        match id {
            "file.new" => self.open_new_canvas(),
            "file.new_clipboard" => self.new_from_clipboard(),
            "file.comic" => self.comic_wizard = Some(crate::comic_ui::ComicWizard::open()),
            "file.open" => self.open_dialog(),
            "file.session" => self.show_editor(EditorType::Session),
            "file.save" => self.save(),
            "file.save_as" => self.save_as(),
            "file.types" => self.file_types_open = true,
            "file.import_png" => self.import_png_layer(),
            "file.scan" => self.scanner = Some(crate::dialogs::ScannerDialog::open()),
            "file.import_brushes" => self.import_brushes_dialog(),
            "file.export_png" => self.export_png(),
            "file.export_psd" => self.export_psd(),
            "file.export_kra" => self.export_layered("kra"),
            "file.export_ora" => self.export_layered("ora"),
            "file.export_clip" => self.export_clip(),
            "file.export_svg" => self.svg_export = Some(crate::dialogs::SvgExportDialog::default()),
            "file.export_brushes" => self.export_brushes(None),
            "edit.undo" => self.undo(),
            "edit.image_size" => self.image_size = Some(crate::dialogs::ImageSizeDialog::open(self.doc.canvas(), self.last_resize_filter)),
            "edit.redo" => self.redo(),
            "edit.clear" => self.clear_selected(),
            "edit.define_pattern" => self.define_pattern(),
            "image.mode.0" => self.set_color_mode(painter_core::color_mode::ColorMode::Rgb),
            "image.mode.1" => self.set_color_mode(painter_core::color_mode::ColorMode::Grayscale),
            "image.mode.2" => self.set_color_mode(painter_core::color_mode::ColorMode::Bitmap),
            "select.all" => self.select_all(),
            "select.none" => self.deselect(),
            "select.invert" => self.invert_selection(),
            "select.save_channel" => self.save_selection_channel(),
            "layer.raster" => self.add_raster_layer(),
            "layer.vector" => self.add_vector_layer(),
            "layer.folder" => self.add_folder(),
            "layer.gradient" => self.add_gradient_layer(),
            "layer.scene3d" => self.new_scene_layer_dialog(),
            "layer.scene3d_ruler" => self.new_scene_layer_from_ruler(),
            "layer.file_object" => self.new_file_object_dialog(),
            "layer.tone" => self.add_tone_layer(),
            "layer.add_mask" => self.add_mask(),
            "layer.delete_mask" => self.delete_mask(),
            "layer.delete" => self.delete_active_layer(),
            "view.fit" => self.fit_pending = true,
            "view.actual" => self.view.set_zoom(1.0),
            "view.rotate_left" => self.view.set_rotation(self.view.rotation - 15f32.to_radians()),
            "view.rotate_right" => self.view.set_rotation(self.view.rotation + 15f32.to_radians()),
            "view.tool_strip" => self.ws.bars.tool_strip = !self.ws.bars.tool_strip,
            "view.command_bar" => self.ws.bars.command_bar = !self.ws.bars.command_bar,
            "file.quit" => self.quit_requested = true,
            "help.welcome" => self.welcome_open = true,
            "help.about" => self.about_open = true,
            "help.updates" => self.check_for_updates(),
            "help.reset_settings" => self.reset_dialog = Some(Default::default()),
            "view.reset_rotation" => self.view.set_rotation(0.0),
            "view.flip" => self.view.flip = !self.view.flip,
            "view.grid" => self.doc.rulers.grid.visible = !self.doc.rulers.grid.visible,
            "rulers.snap" => self.toggle_ruler_snapping(),
            "view.zen" => self.toggle_zen(),
            "view.screen.0" => { let ctx = self.egui_ctx.clone(); self.set_screen_mode(&ctx, 0) }
            "view.screen.1" => { let ctx = self.egui_ctx.clone(); self.set_screen_mode(&ctx, 1) }
            "view.screen.2" => { let ctx = self.egui_ctx.clone(); self.set_screen_mode(&ctx, 2) }
            "view.ruler_bar" => self.ws.chrome.ruler_bar = !self.ws.chrome.ruler_bar,
            "view.brush_crosshair" => self.ws.chrome.brush_crosshair = !self.ws.chrome.brush_crosshair,
            "view.clock" => self.ws.chrome.clock = !self.ws.chrome.clock,
            "view.ui_bigger" => self.step_ui_scale(1),
            "color.transparent" => self.toggle_transparent(),
            "layer.duplicate" => self.duplicate_selected_layers(),
            "layer.group" => self.group_selected_layers(),
            "layer.draft" => {
                let on = !self.doc.find(self.doc.active).is_some_and(|l| l.draft);
                self.set_on_selected_layers(|l| l.draft = on);
            }
            "brush.advanced" => self.open_advanced_tool_settings(),
            "brush.export_abr" => self.export_abr(self.brush),
            "brush.export_sut" => self.export_sut(self.brush),
            "brush.export_krita" => self.export_brush_as(self.brush, BrushFormat::Krita),
            "brush.export_mypaint" => self.export_brush_as(self.brush, BrushFormat::MyPaint),
            "brush.export_gimp" => self.export_brush_as(self.brush, BrushFormat::Gimp),
            "view.ui_smaller" => self.step_ui_scale(-1),
            "view.ui_reset" => self.step_ui_scale(0),
            "view.modifier_keys" => {
                self.modifier_keys.shown = !self.modifier_keys.shown;
                if !self.modifier_keys.shown {
                    self.modifier_keys.release();
                }
            }
            "view.theme_editor" => self.ws.theme_editor = true,
            "workspace.reset" => self.ws.set.current_mut().reset(),
            "workspace.duplicate" => {
                let name = format!("{} copy", self.ws.set.current().name);
                self.ws.set.duplicate(&name);
            }
            _ => {
                if let Some(i) = index("tool.") {
                    match Tool::COMMANDS.get(i) {
                        Some(Tool::Brush) => self.select_brush(self.last_paint_brush),
                        Some(tool) => self.tool = *tool,
                        None => {}
                    }
                } else if let Some(i) = index("brush.") {
                    if i < self.brushes.len() {
                        self.select_brush(i);
                    }
                } else if let Some(i) = index("action.") {
                    self.play_action(i);
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
