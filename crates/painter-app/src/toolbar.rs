//! The tool strip on the left, customisable like Clip Studio's Tools
//! palette and command bar: items can be dragged into another order,
//! removed, and commands (anything in the command palette) added as quick
//! buttons, with separators. Kept between launches as a list of keys.

use eframe::egui::{self, Color32, Sense, Stroke, Vec2};

use crate::app::PainterApp;
use crate::icons::{icon_button, Icon};
use crate::theme;
use crate::tools::{BrushGroup, SelectShape, Tool};

/// One button (or gap) of the tool strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Item {
    Group(BrushGroup),
    Tool(Tool),
    /// A command palette entry, by id (e.g. `file.save`).
    Command(String),
    Separator,
}

fn group_key(g: BrushGroup) -> &'static str {
    match g {
        BrushGroup::Pen => "pen",
        BrushGroup::Pencil => "pencil",
        BrushGroup::Brush => "brush",
        BrushGroup::Airbrush => "airbrush",
        BrushGroup::Eraser => "eraser",
    }
}

fn tool_key(t: Tool) -> &'static str {
    match t {
        Tool::Select(SelectShape::Rect) => "select_rect",
        Tool::Select(SelectShape::Ellipse) => "select_ellipse",
        Tool::Select(SelectShape::Lasso) => "select_lasso",
        other => other.id(),
    }
}

impl Item {
    pub(crate) fn key(&self) -> String {
        match self {
            Item::Group(g) => format!("group:{}", group_key(*g)),
            Item::Tool(t) => format!("tool:{}", tool_key(*t)),
            Item::Command(id) => format!("cmd:{id}"),
            Item::Separator => "sep".into(),
        }
    }

    pub(crate) fn parse(key: &str) -> Option<Item> {
        if key == "sep" {
            return Some(Item::Separator);
        }
        let (kind, name) = key.split_once(':')?;
        match kind {
            "group" => BrushGroup::ALL.into_iter().find(|g| group_key(*g) == name).map(Item::Group),
            "tool" => Tool::ALL.into_iter().find(|t| tool_key(*t) == name).map(Item::Tool),
            "cmd" if !name.is_empty() => Some(Item::Command(name.to_string())),
            _ => None,
        }
    }
}

/// The strip as it comes: brush groups, then the tools in families.
pub(crate) fn default_items() -> Vec<Item> {
    let mut items: Vec<Item> = BrushGroup::ALL.into_iter().map(Item::Group).collect();
    items.push(Item::Separator);
    for tool in Tool::ALL.into_iter().filter(|t| *t != Tool::Brush) {
        if matches!(tool, Tool::Select(SelectShape::Rect) | Tool::Transform | Tool::Hand) {
            items.push(Item::Separator);
        }
        items.push(Item::Tool(tool));
    }
    items
}

/// The strip from saved keys; unknown keys are dropped, tools added since come back at the end.
pub(crate) fn from_keys(keys: &[String]) -> Vec<Item> {
    let mut items: Vec<Item> = keys.iter().filter_map(|k| Item::parse(k)).collect();
    for item in default_items().into_iter().filter(|i| !matches!(i, Item::Separator)) {
        if !items.contains(&item) {
            items.push(item);
        }
    }
    items
}

/// Moves item `from` to sit at `to` (an index in the list before the move).
pub(crate) fn move_item(items: &mut Vec<Item>, from: usize, to: usize) {
    if from >= items.len() || from == to {
        return;
    }
    let item = items.remove(from);
    let to = if to > from { to - 1 } else { to }.min(items.len());
    items.insert(to, item);
}

/// An icon for well-known commands; others show their initials.
fn command_icon(id: &str) -> Option<Icon> {
    Some(match id {
        "file.new" | "file.new_clipboard" => Icon::New,
        "file.open" => Icon::Open,
        "file.save" => Icon::Save,
        "file.save_as" | "file.export_png" | "file.export_psd" | "file.export_kra" | "file.export_ora" | "file.export_svg" => Icon::SaveAs,
        "file.import_brushes" | "file.export_brushes" => Icon::Brushes,
        "edit.undo" => Icon::Undo,
        "edit.redo" => Icon::Redo,
        "edit.clear" | "layer.delete" => Icon::Delete,
        "select.all" | "select.none" | "select.invert" => Icon::RectSelect,
        "layer.raster" => Icon::Layer,
        "layer.vector" => Icon::Path,
        "layer.folder" => Icon::GroupLayer,
        "layer.gradient" => Icon::GradientLinear,
        "layer.scene3d" => Icon::Transform3d,
        "layer.add_mask" | "layer.delete_mask" => Icon::LayerMask,
        "view.fit" => Icon::ZoomFit,
        "view.actual" => Icon::ZoomOriginal,
        "view.reset_rotation" => Icon::Reset,
        "view.grid" => Icon::Grid,
        "rulers.snap" => Icon::Path,
        _ => return None,
    })
}

/// "Import PNG as Layer..." → "IP".
fn initials(label: &str) -> String {
    label
        .split_whitespace()
        .filter(|w| w.chars().next().is_some_and(|c| c.is_alphanumeric()))
        .take(2)
        .filter_map(|w| w.chars().next())
        .collect::<String>()
        .to_uppercase()
}

impl PainterApp {
    fn item_shown(&self, item: &Item) -> bool {
        match item {
            Item::Group(g) => self.group_in_workspace(*g),
            Item::Tool(t) => self.tool_in_workspace(*t),
            Item::Command(_) | Item::Separator => true,
        }
    }

    /// The tool strip: its items, then the main / sub colour chips.
    pub(crate) fn tool_strip(&mut self, ctx: &egui::Context) {
        let editing = self.ws.toolbar_editing;
        egui::SidePanel::left("tools")
            .resizable(false)
            .exact_width(42.0)
            .frame(egui::Frame::new().fill(theme::panel()).inner_margin(egui::Margin::symmetric(3, 6)))
            .show(ctx, |ui| {
                let strip = ui.max_rect();
                // Right-click on the strip's background: customise. Registered first, so the
                // buttons on top of it still get their clicks.
                let background = ui.interact(strip, ui.id().with("tool_strip_bg"), Sense::click());
                background.context_menu(|ui| {
                    if ui.button("Customize Toolbar...").clicked() {
                        self.ws.toolbar_editing = true;
                        ui.close_menu();
                    }
                });
                // No drag-to-scroll: on a touch screen a pen tap that moves a hair would scroll instead of pressing.
                egui::ScrollArea::vertical().id_salt("tool_strip_scroll").drag_to_scroll(false).show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        if editing {
                            self.tool_strip_editing(ui);
                        } else {
                            self.tool_strip_buttons(ui);
                        }
                        ui.separator();
                        ui.add_space(4.0);
                        crate::widgets::color_chips(ui, &mut self.color, &mut self.sub_color, 24.0);
                        if crate::widgets::transparent_chip(ui, self.transparent, 20.0).clicked() {
                            self.toggle_transparent();
                        }
                    });
                });
            });
        if editing {
            self.toolbar_editor_window(ctx);
        }
    }

    fn tool_strip_buttons(&mut self, ui: &mut egui::Ui) {
        let items = self.ws.toolbar.clone();
        let mut last_was_separator = true;
        for (i, item) in items.iter().enumerate() {
            if !self.item_shown(item) {
                continue;
            }
            let response = match item {
                Item::Separator => {
                    if !last_was_separator {
                        ui.separator();
                    }
                    last_was_separator = true;
                    continue;
                }
                Item::Group(group) => {
                    let selected = self.tool == Tool::Brush && self.brush_group == *group;
                    let r = icon_button(ui, group.icon(), 20.0, selected, group.label());
                    if r.clicked() {
                        self.select_group(*group);
                    }
                    r
                }
                Item::Tool(tool) => {
                    let r = icon_button(ui, tool.icon(), 20.0, self.tool == *tool, tool.hint());
                    if r.clicked() {
                        self.tool = *tool;
                    }
                    r
                }
                Item::Command(id) => {
                    let r = self.command_button(ui, id);
                    if r.clicked() {
                        let id = id.clone();
                        self.run_command(&id);
                    }
                    r
                }
            };
            last_was_separator = false;
            response.context_menu(|ui| {
                if ui.button("Remove from Toolbar").clicked() {
                    self.ws.toolbar.remove(i);
                    ui.close_menu();
                }
                if ui.button("Customize Toolbar...").clicked() {
                    self.ws.toolbar_editing = true;
                    ui.close_menu();
                }
            });
        }
    }

    /// A quick command: its icon, or its initials.
    fn command_button(&mut self, ui: &mut egui::Ui, id: &str) -> egui::Response {
        if self.ws.commands.get(id).is_none() {
            self.refresh_commands();
        }
        let label = self.ws.commands.get(id).map_or_else(|| id.to_string(), |c| c.label.clone());
        let hint = match self.ws.commands.get(id).and_then(|c| c.shortcut.clone()) {
            Some(keys) => format!("{label} ({keys})"),
            None => label.clone(),
        };
        match command_icon(id) {
            Some(icon) => icon_button(ui, icon, 20.0, false, &hint),
            None => {
                let side = 30.0 * crate::icons::panel_factor(ui);
                let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
                if response.hovered() {
                    ui.painter().rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill);
                }
                ui.painter().rect_stroke(rect.shrink(4.0), 3.0, Stroke::new(1.0_f32, theme::text_dim()), egui::StrokeKind::Inside);
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    initials(&label),
                    egui::FontId::proportional(11.0),
                    theme::text(),
                );
                response.on_hover_text(hint)
            }
        }
    }

    /// Edit mode: every item can be dragged up or down; right-click removes.
    fn tool_strip_editing(&mut self, ui: &mut egui::Ui) {
        let items = self.ws.toolbar.clone();
        let mut moved = None;
        let side = 30.0 * crate::icons::panel_factor(ui);
        for (i, item) in items.iter().enumerate() {
            let height = if *item == Item::Separator { 10.0 } else { side };
            let (rect, response) = ui.allocate_exact_size(Vec2::new(side, height), Sense::click_and_drag());
            let hovered_drop = response.dnd_hover_payload::<usize>().is_some();
            let fill = if hovered_drop {
                theme::accent()
            } else if response.hovered() {
                theme::hover()
            } else {
                theme::well()
            };
            ui.painter().rect_filled(rect, 3.0, fill);
            match item {
                Item::Separator => {
                    ui.painter().hline(rect.x_range().shrink(4.0), rect.center().y, Stroke::new(1.0_f32, theme::text_dim()));
                }
                Item::Group(g) => g.icon().paint(ui, rect, 18.0, theme::text()),
                Item::Tool(t) => t.icon().paint(ui, rect, 18.0, theme::text()),
                Item::Command(id) => match command_icon(id) {
                    Some(icon) => icon.paint(ui, rect, 18.0, theme::text()),
                    None => {
                        let label = self.ws.commands.get(id).map_or_else(|| id.clone(), |c| c.label.clone());
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            initials(&label),
                            egui::FontId::proportional(11.0),
                            theme::text(),
                        );
                    }
                },
            }
            if !self.item_shown(item) {
                // Hidden in this workspace: shown faded while editing.
                ui.painter().rect_filled(rect, 3.0, Color32::from_black_alpha(120));
            }
            response.dnd_set_drag_payload(i);
            if let Some(from) = response.dnd_release_payload::<usize>() {
                moved = Some((*from, i));
            }
            let title = match item {
                Item::Group(g) => g.label().to_string(),
                Item::Tool(t) => t.name().to_string(),
                Item::Command(id) => self.ws.commands.get(id).map_or_else(|| id.clone(), |c| c.label.clone()),
                Item::Separator => "Separator".into(),
            };
            response.on_hover_text(format!("{title}: drag to move, right-click to remove")).context_menu(|ui| {
                if ui.button("Remove").clicked() {
                    self.ws.toolbar.remove(i);
                    ui.close_menu();
                }
            });
        }
        if let Some((from, to)) = moved {
            move_item(&mut self.ws.toolbar, from, to);
        }
    }

    /// The window beside the strip while customising: add tools, commands and separators.
    fn toolbar_editor_window(&mut self, ctx: &egui::Context) {
        if self.ws.commands.commands().is_empty() {
            self.refresh_commands();
        }
        let mut open = true;
        egui::Window::new("Customize Toolbar").open(&mut open).default_pos(egui::pos2(60.0, 120.0)).default_width(300.0).show(
            ctx,
            |ui| {
                ui.small("Drag the toolbar's buttons to reorder them; right-click one to remove it.");
                ui.horizontal(|ui| {
                    if ui.button("+ Separator").clicked() {
                        self.ws.toolbar.push(Item::Separator);
                    }
                    if ui.button("Reset").on_hover_text("Back to the standard toolbar").clicked() {
                        self.ws.toolbar = default_items();
                    }
                    if ui.button("Done").clicked() {
                        self.ws.toolbar_editing = false;
                    }
                });
                let missing: Vec<Item> =
                    default_items().into_iter().filter(|i| *i != Item::Separator && !self.ws.toolbar.contains(i)).collect();
                if !missing.is_empty() {
                    ui.label("Tools not on the toolbar");
                    ui.horizontal_wrapped(|ui| {
                        for item in missing {
                            let (icon, name) = match &item {
                                Item::Group(g) => (g.icon(), g.label()),
                                Item::Tool(t) => (t.icon(), t.name()),
                                _ => continue,
                            };
                            if icon_button(ui, icon, 16.0, false, &format!("Add {name}")).clicked() {
                                self.ws.toolbar.push(item);
                            }
                        }
                    });
                }
                ui.separator();
                ui.label("Add a command");
                let search = &mut self.ws.toolbar_search;
                ui.add(egui::TextEdit::singleline(search).hint_text("Search commands").desired_width(f32::INFINITY));
                let query = search.to_lowercase();
                let commands: Vec<(String, String)> = self
                    .ws
                    .commands
                    .commands()
                    .iter()
                    .filter(|c| query.is_empty() || c.label.to_lowercase().contains(&query) || c.id.contains(&query))
                    .map(|c| (c.id.clone(), c.label.clone()))
                    .collect();
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    for (id, label) in commands {
                        let on = self.ws.toolbar.contains(&Item::Command(id.clone()));
                        if ui.add_enabled(!on, egui::Button::new(&label)).on_hover_text(&id).clicked() {
                            self.ws.toolbar.push(Item::Command(id));
                        }
                    }
                });
            },
        );
        if !open {
            self.ws.toolbar_editing = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_and_unknown_ones_drop() {
        let items = default_items();
        let keys: Vec<String> = items.iter().map(Item::key).collect();
        assert_eq!(from_keys(&keys), items);
        let saved = vec!["cmd:file.save".to_string(), "tool:hand".into(), "sep".into(), "tool:teleport".into(), "group:pen".into()];
        let loaded = from_keys(&saved);
        assert_eq!(
            loaded[..4],
            [Item::Command("file.save".into()), Item::Tool(Tool::Hand), Item::Separator, Item::Group(BrushGroup::Pen)]
        );
        // Tools missing from an old list come back at the end.
        assert!(loaded.contains(&Item::Tool(Tool::Ruler)) && loaded.contains(&Item::Group(BrushGroup::Eraser)));
        assert_eq!(loaded.iter().filter(|i| **i == Item::Tool(Tool::Hand)).count(), 1);
    }

    #[test]
    fn moving_items() {
        let mut v = vec![Item::Separator, Item::Tool(Tool::Fill), Item::Tool(Tool::Hand), Item::Tool(Tool::Crop)];
        move_item(&mut v, 3, 1);
        assert_eq!(v[1], Item::Tool(Tool::Crop));
        move_item(&mut v, 1, 4);
        assert_eq!(v[3], Item::Tool(Tool::Crop));
        move_item(&mut v, 9, 0);
        assert_eq!(v.len(), 4);
        assert_eq!(initials("Import PNG as Layer..."), "IP");
    }
}
