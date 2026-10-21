//! On-screen Ctrl, Shift and Alt (View → On-Screen Modifier Keys), for
//! tablets without a keyboard. A tap latches a key and lights it; another tap
//! releases it. While latched it is added to every input event before egui
//! sees it, so shortcuts, Shift-clicks, Ctrl-drags and the tools' own
//! modifier handling work exactly as with a real keyboard.

use eframe::egui::{self, Modifiers, RawInput, Sense, Vec2};

pub(crate) struct ModifierKeys {
    pub(crate) shown: bool,
    ctrl: bool,
    shift: bool,
    alt: bool,
}

impl Default for ModifierKeys {
    /// Shown on iPadOS (no keyboard to count on), hidden elsewhere.
    fn default() -> Self {
        Self { shown: cfg!(target_os = "ios"), ctrl: false, shift: false, alt: false }
    }
}

impl ModifierKeys {
    /// The latched keys (`command` is Ctrl outside macOS, as egui sees it).
    pub(crate) fn latched(&self) -> Modifiers {
        if !self.shown {
            return Modifiers::NONE;
        }
        let mut m = Modifiers::NONE;
        if self.ctrl {
            m |= Modifiers::CTRL;
            m.command = !cfg!(target_os = "macos");
        }
        if self.shift {
            m |= Modifiers::SHIFT;
        }
        if self.alt {
            m |= Modifiers::ALT;
        }
        m
    }

    /// Adds the latched keys to this frame's input (before egui reads it).
    pub(crate) fn inject(&self, raw: &mut RawInput) {
        let add = self.latched();
        if add.is_none() {
            return;
        }
        raw.modifiers |= add;
        for event in &mut raw.events {
            match event {
                egui::Event::Key { modifiers, .. }
                | egui::Event::PointerButton { modifiers, .. }
                | egui::Event::MouseWheel { modifiers, .. } => *modifiers |= add,
                _ => {}
            }
        }
    }

    /// The three keys, floating at the bottom left of the window; drag the
    /// strip by its grip to move it.
    pub(crate) fn overlay(&mut self, ctx: &egui::Context, fill: egui::Color32, accent: egui::Color32) {
        if !self.shown {
            return;
        }
        let size = if cfg!(target_os = "ios") { Vec2::new(64.0, 44.0) } else { Vec2::new(52.0, 30.0) };
        let screen = ctx.screen_rect();
        egui::Area::new(egui::Id::new("modifier_keys"))
            .order(egui::Order::Foreground)
            .movable(true)
            .constrain(true)
            .pivot(egui::Align2::LEFT_BOTTOM)
            .default_pos(egui::pos2(screen.left() + 64.0, screen.bottom() - 40.0))
            .show(ctx, |ui| {
                egui::Frame::new().fill(fill.gamma_multiply(0.9)).corner_radius(8).inner_margin(4).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        // The grip: the area moves when dragged anywhere but on a key.
                        let (grip, response) = ui.allocate_exact_size(Vec2::new(10.0, size.y), Sense::hover());
                        let dot = ui.visuals().weak_text_color();
                        for row in -1..=1 {
                            for col in [-1.0, 1.0] {
                                let c = grip.center() + Vec2::new(col * 2.5, row as f32 * 6.0);
                                ui.painter().circle_filled(c, 1.5, dot);
                            }
                        }
                        response.on_hover_text("Drag to move");
                        for (label, on) in [("Ctrl", &mut self.ctrl), ("Shift", &mut self.shift), ("Alt", &mut self.alt)] {
                            let text = egui::RichText::new(label).strong();
                            let button = egui::Button::new(text).min_size(size).corner_radius(6).selected(*on);
                            let button = if *on { button.fill(accent) } else { button };
                            if ui.add(button).on_hover_text(format!("Hold {label} until tapped again")).clicked() {
                                *on = !*on;
                            }
                        }
                    });
                });
            });
    }

    /// Releases every latched key.
    pub(crate) fn release(&mut self) {
        (self.ctrl, self.shift, self.alt) = (false, false, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latched_keys_reach_every_event() {
        let mut keys = ModifierKeys { shown: true, ..ModifierKeys::default() };
        keys.ctrl = true;
        keys.shift = true;
        let mut raw = RawInput {
            events: vec![
                egui::Event::Key { key: egui::Key::Z, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE },
                egui::Event::PointerButton {
                    pos: egui::pos2(1.0, 1.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::ALT,
                },
            ],
            ..RawInput::default()
        };
        keys.inject(&mut raw);
        assert!(raw.modifiers.ctrl && raw.modifiers.shift && !raw.modifiers.alt);
        // Ctrl+Shift+Z (redo) is what the app's shortcut sees.
        let egui::Event::Key { modifiers, .. } = raw.events[0] else { unreachable!() };
        assert!(modifiers.matches_logically(Modifiers::COMMAND | Modifiers::SHIFT));
        let egui::Event::PointerButton { modifiers, .. } = raw.events[1] else { unreachable!() };
        assert!(modifiers.alt && modifiers.ctrl, "a real key held too is kept");
        // Hidden keys don't act.
        keys.shown = false;
        let mut raw = RawInput { modifiers: Modifiers::NONE, ..RawInput::default() };
        keys.inject(&mut raw);
        assert!(raw.modifiers.is_none());
        keys.release();
    }
}
