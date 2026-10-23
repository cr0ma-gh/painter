//! Tool families on the tool strip, as in Photoshop: neighbouring tools of a
//! family share one button that shows the one in hand (or the last used),
//! with a small triangle in its corner. Click: that tool. Hold, right-click
//! or click the triangle: a flyout with the others. The strip's right-click
//! menu turns grouping off.

use std::collections::HashMap;

use eframe::egui::{self, Pos2, Rect, Sense, Stroke, Vec2};

use crate::app::PainterApp;
use crate::icons::Icon;
use crate::theme;
use crate::toolbar::Item;
use crate::tools::{BrushGroup, SelectShape, Tool};

/// Seconds a press is held before the flyout opens.
const HOLD: f64 = 0.35;

/// The family an item belongs to, if it shares a button.
pub(crate) fn family(item: &Item) -> Option<u8> {
    Some(match item {
        Item::Group(BrushGroup::Pen | BrushGroup::Pencil | BrushGroup::Brush | BrushGroup::Airbrush) => 0,
        Item::Tool(Tool::Stamp(_)) => 1,
        Item::Tool(Tool::Fill | Tool::Gradient) => 2,
        Item::Tool(Tool::Select(SelectShape::Rect | SelectShape::Ellipse)) => 3,
        Item::Tool(Tool::Select(SelectShape::Lasso | SelectShape::Magnetic)) => 4,
        Item::Tool(Tool::Select(SelectShape::Magic | SelectShape::Object)) => 5,
        Item::Tool(Tool::Text(_)) => 6,
        _ => return None,
    })
}

/// The flyout open on the strip: its tools and where it shows.
pub(crate) struct Flyout {
    members: Vec<Item>,
    at: Pos2,
    /// The frame it opened (so the click that opened it doesn't close it).
    opened: u64,
}

#[derive(Default)]
pub(crate) struct ToolGroups {
    /// Last tool used in each family.
    last: HashMap<u8, Item>,
    flyout: Option<Flyout>,
    /// Screenshot runs: open this family.s flyout (`dev.flyout:N`).
    pub dev_open: Option<u8>,
}

impl PainterApp {
    pub(crate) fn item_selected(&self, item: &Item) -> bool {
        match item {
            Item::Group(g) => self.tool == Tool::Brush && self.brush_group == *g,
            Item::Tool(t) => self.tool == *t,
            _ => false,
        }
    }

    pub(crate) fn activate_item(&mut self, item: &Item) {
        match item {
            Item::Group(g) => self.select_group(*g),
            Item::Tool(t) => self.tool = *t,
            Item::Command(id) => {
                let id = id.clone();
                self.run_command(&id);
            }
            Item::Separator => {}
        }
        if let Some(f) = family(item) {
            self.tool_groups.last.insert(f, item.clone());
        }
    }

    fn item_look(item: &Item) -> (Icon, String, String) {
        match item {
            Item::Group(g) => (g.icon(), g.label().to_string(), g.label().to_string()),
            Item::Tool(t) => (t.icon(), t.name().to_string(), t.hint().to_string()),
            _ => (Icon::Options, String::new(), String::new()),
        }
    }

    /// One button for a family's run of tools; returns its response (for the strip's context menu).
    pub(crate) fn group_button(&mut self, ui: &mut egui::Ui, members: &[Item]) -> egui::Response {
        let family = family(&members[0]).unwrap_or(0);
        // The tool in hand, else the last used, else the first.
        let shown = members
            .iter()
            .find(|m| self.item_selected(m))
            .or_else(|| self.tool_groups.last.get(&family).filter(|l| members.contains(l)))
            .unwrap_or(&members[0])
            .clone();
        let (icon, _, hint) = Self::item_look(&shown);
        let selected = self.item_selected(&shown);
        let r = crate::icons::icon_button(ui, icon, 20.0, selected, &format!("{hint}\nHold or right-click for the others"));
        // The corner triangle.
        let rect = r.rect;
        let c = rect.right_bottom() - Vec2::splat(3.0);
        let tri = vec![c, c - Vec2::new(6.0, 0.0), c - Vec2::new(0.0, 6.0)];
        ui.painter().add(egui::Shape::convex_polygon(tri, theme::text_dim(), Stroke::NONE));
        let corner = Rect::from_min_max(rect.right_bottom() - Vec2::splat(11.0), rect.right_bottom());
        let held = r.is_pointer_button_down_on() && ui.input(|i| i.pointer.press_start_time().is_some_and(|t| i.time - t > HOLD));
        let on_corner = r.clicked() && r.interact_pointer_pos().is_some_and(|p| corner.contains(p));
        let dev = self.tool_groups.dev_open == Some(family);
        if dev {
            self.tool_groups.dev_open = None;
        }
        if held || on_corner || r.secondary_clicked() || r.long_touched() || dev {
            if self.tool_groups.flyout.is_none() {
                let frame = ui.ctx().cumulative_pass_nr();
                self.tool_groups.flyout =
                    Some(Flyout { members: members.to_vec(), at: rect.right_top() + Vec2::new(4.0, 0.0), opened: frame });
            }
        } else if r.clicked() {
            self.activate_item(&shown);
        }
        r
    }

    /// The open flyout: every tool of the family with its name and key.
    pub(crate) fn tool_flyout(&mut self, ctx: &egui::Context) {
        let Some(flyout) = &self.tool_groups.flyout else { return };
        let members = flyout.members.clone();
        let (at, opened) = (flyout.at, flyout.opened);
        let mut picked = None;
        let area = egui::Area::new(egui::Id::new("tool_flyout")).order(egui::Order::Foreground).fixed_pos(at).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(190.0);
                for m in &members {
                    let (icon, name, _) = Self::item_look(m);
                    let key = match m {
                        Item::Tool(t) => t.shortcut().unwrap_or(""),
                        _ => "",
                    };
                    let selected = self.item_selected(m);
                    let (rect, r) = ui.allocate_exact_size(Vec2::new(200.0, 26.0), Sense::click());
                    let fill = if selected {
                        ui.visuals().selection.bg_fill
                    } else if r.hovered() {
                        ui.visuals().widgets.hovered.weak_bg_fill
                    } else {
                        egui::Color32::TRANSPARENT
                    };
                    ui.painter().rect_filled(rect, 3.0, fill);
                    icon.paint(ui, Rect::from_min_size(rect.min, Vec2::splat(26.0)), 16.0, theme::text());
                    let font = egui::FontId::proportional(13.0);
                    ui.painter().text(
                        rect.left_center() + Vec2::new(30.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        name,
                        font.clone(),
                        theme::text(),
                    );
                    ui.painter().text(
                        rect.right_center() - Vec2::new(6.0, 0.0),
                        egui::Align2::RIGHT_CENTER,
                        key,
                        font,
                        theme::text_dim(),
                    );
                    if r.clicked() {
                        picked = Some(m.clone());
                    }
                }
            });
        });
        // A click (or Esc) anywhere else closes it; the press that opened it doesn't.
        let outside =
            ctx.input(|i| i.pointer.any_pressed()) && !ctx.pointer_hover_pos().is_some_and(|p| area.response.rect.contains(p));
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if let Some(m) = picked {
            self.activate_item(&m);
            self.tool_groups.flyout = None;
        } else if (outside && ctx.cumulative_pass_nr() > opened + 1) || escape {
            self.tool_groups.flyout = None;
        }
    }
}
