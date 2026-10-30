//! Correction layers (Layer → New Correction Layer) with their settings in
//! the layer properties, and the Filter menu: a dialog per filter with a
//! live preview on the canvas, applied to the edited layer (inside the
//! selection) with one undo step.

use std::sync::mpsc::{channel, Receiver};
use crate::i18n::tr;
use std::time::{Duration, Instant};

use eframe::egui;
use painter_core::filters::{self, Filter};
use painter_core::{Adjustment, Layer, LayerId, LayerKind, PressureCurve, TileStore};
use painter_render::Plane;

use crate::app::PainterApp;
use crate::curve::curve_editor;

/// Wait this long after the last change before recomputing the preview.
const PREVIEW_DELAY: Duration = Duration::from_millis(150);

/// Settings of a correction layer. Returns whether anything changed.
pub(crate) fn adjustment_editor(ui: &mut egui::Ui, a: &mut Adjustment) -> bool {
    let mut changed = false;
    let mut slider = |ui: &mut egui::Ui, v: &mut f32, range: std::ops::RangeInclusive<f32>, label: &str| {
        changed |= ui.add(egui::Slider::new(v, range).text(label)).changed();
    };
    ui.label(egui::RichText::new(tr(a.name())).strong());
    match a {
        Adjustment::BrightnessContrast { brightness, contrast } => {
            slider(ui, brightness, -1.0..=1.0, "Brightness");
            slider(ui, contrast, -1.0..=1.0, "Contrast");
        }
        Adjustment::Levels { input_black, input_white, gamma, output_black, output_white } => {
            slider(ui, input_black, 0.0..=1.0, "Input black");
            slider(ui, input_white, 0.0..=1.0, "Input white");
            slider(ui, gamma, 0.1..=4.0, "Gamma");
            slider(ui, output_black, 0.0..=1.0, "Output black");
            slider(ui, output_white, 0.0..=1.0, "Output white");
            if *input_white <= *input_black + 0.01 {
                *input_white = (*input_black + 0.01).min(1.0);
            }
        }
        Adjustment::ToneCurve { curve } => {
            changed |= curve_editor(ui, "tone_curve", curve);
            if ui.small_button(tr("Reset")).clicked() {
                *curve = PressureCurve::linear();
                changed = true;
            }
        }
        Adjustment::HueSaturation { hue, saturation, lightness } => {
            slider(ui, hue, -180.0..=180.0, "Hue");
            slider(ui, saturation, -1.0..=1.0, "Saturation");
            slider(ui, lightness, -1.0..=1.0, "Lightness");
        }
        Adjustment::ColorBalance { shadows, midtones, highlights } => {
            for (label, values) in [("Shadows", shadows), ("Midtones", midtones), ("Highlights", highlights)] {
                ui.label(label);
                for (i, pair) in ["Cyan – Red", "Magenta – Green", "Yellow – Blue"].into_iter().enumerate() {
                    slider(ui, &mut values[i], -1.0..=1.0, pair);
                }
            }
        }
        Adjustment::Posterize { levels } => {
            changed |= ui.add(egui::Slider::new(levels, 2..=32).text(tr("Levels"))).changed();
        }
        Adjustment::Threshold { level } => slider(ui, level, 0.0..=1.0, "Threshold"),
        Adjustment::Invert => {
            ui.small(tr("Inverts the colours below."));
        }
        Adjustment::GradientMap { from, to } => {
            ui.horizontal(|ui| {
                ui.label(tr("Shadows"));
                changed |= ui.color_edit_button_srgb(from).changed();
                ui.label(tr("Highlights"));
                changed |= ui.color_edit_button_srgb(to).changed();
                if ui.small_button(tr("Swap")).clicked() {
                    std::mem::swap(from, to);
                    changed = true;
                }
            });
        }
    }
    ui.small(tr("Changes the colours of the layers below. Clip it to a layer to correct only that one; paint its mask to limit it."));
    changed
}

/// An open filter dialog.
pub(crate) struct FilterDialog {
    filter: Filter,
    layer: LayerId,
    /// Filtering the layer's mask (being edited), not its pixels: a blurred mask feathers its edge.
    mask: bool,
    /// The layer's pixels (or mask) before the filter.
    original: TileStore,
    preview: bool,
    /// When the settings last changed (preview pending).
    changed_at: Option<Instant>,
    /// The settings the layer currently shows (when previewing).
    shown: Option<Filter>,
    /// A preview being computed on a worker thread, for these settings.
    job: Option<(Filter, Receiver<TileStore>)>,
}

/// Default settings for each filter, by name.
pub(crate) fn filter_presets(canvas: (u32, u32)) -> Vec<Filter> {
    vec![
        Filter::GaussianBlur { radius: 6.0 },
        Filter::MotionBlur { angle: 0.0, distance: 20.0 },
        Filter::RadialBlur { center: [canvas.0 as f32 * 0.5, canvas.1 as f32 * 0.5], amount: 0.15 },
        Filter::UnsharpMask { radius: 3.0, amount: 1.0 },
        Filter::Noise { amount: 0.1, monochrome: true, seed: 1 },
    ]
}

impl PainterApp {
    pub(crate) fn add_adjustment_layer(&mut self, adjustment: Adjustment) {
        let id = self.doc.alloc_id();
        let name = adjustment.name();
        self.insert_layer(Layer::adjustment(id, adjustment));
        self.status = format!("New correction layer: {name}");
    }

    /// Opens a filter's dialog on the edited layer.
    pub(crate) fn open_filter(&mut self, filter: Filter) {
        self.close_filter(false);
        let Some(layer) = self.doc.active_layer() else { return };
        let mask = self.edit_mask && layer.mask.is_some();
        let refuse = match &layer.kind {
            _ if layer.locked => Some("the layer is locked"),
            _ if mask => None,
            _ if self.scenes.contains_key(&layer.id) => Some("3D layers are redrawn from their scene (use Flatten to pixels in the 3D panel first)"),
            LayerKind::Raster(_) => None,
            _ => Some("filters work on raster layers and masks"),
        };
        if let Some(reason) = refuse {
            self.status = format!("Can't filter \"{}\": {reason}", layer.name);
            return;
        }
        let original = if mask { layer.mask.as_ref().map(|m| m.tiles.clone()) } else { layer.tiles().cloned() }.unwrap_or_default();
        self.filter_dialog =
            Some(FilterDialog { filter, layer: layer.id, mask, original, preview: true, changed_at: Some(Instant::now()), shown: None, job: None });
    }

    fn set_layer_tiles(&mut self, id: LayerId, mask: bool, tiles: TileStore) {
        if mask {
            if let Some(m) = self.doc.find_mut(id).and_then(|l| l.mask.as_mut()) {
                m.tiles = tiles;
            }
            self.upload_plane(Plane::Mask(id));
        } else {
            if let Some(l) = self.doc.find_mut(id) {
                l.kind = LayerKind::Raster(tiles);
            }
            self.upload_plane(Plane::Color(id));
        }
        self.recomposite = true;
    }

    /// Closes the dialog: `apply` keeps the filter (one undo step), otherwise
    /// the layer goes back to how it was.
    fn close_filter(&mut self, apply: bool) {
        let Some(dialog) = self.filter_dialog.take() else { return };
        if self.doc.find(dialog.layer).is_none() {
            return;
        }
        let result = if apply {
            let mut tiles = match &dialog.shown {
                Some(f) if *f == dialog.filter => {
                    let layer = self.doc.find(dialog.layer);
                    let shown = if dialog.mask { layer.and_then(|l| l.mask.as_ref()).map(|m| &m.tiles) } else { layer.and_then(|l| l.tiles()) };
                    shown.cloned().unwrap_or_default()
                }
                _ => filters::apply(&dialog.original, self.doc.canvas(), self.doc.selection.as_ref(), &dialog.filter),
            };
            if dialog.mask {
                // The amount hidden is the red the filter left (see Mask::settle).
                painter_core::Mask::settle_all(&mut tiles);
            }
            Some(tiles)
        } else {
            None
        };
        // Undo should see the layer as it was.
        self.set_layer_tiles(dialog.layer, dialog.mask, dialog.original);
        if let Some(tiles) = result {
            self.checkpoint();
            self.set_layer_tiles(dialog.layer, dialog.mask, tiles);
            self.status = format!("{} applied", dialog.filter.name());
        }
    }

    pub(crate) fn filter_window(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.filter_dialog else { return };
        // The layer went away or another one is being edited: drop the preview.
        if self.doc.active != dialog.layer || self.doc.find(dialog.layer).is_none() {
            self.close_filter(false);
            return;
        }
        let canvas = (self.doc.width as f32, self.doc.height as f32);
        let mut open = true;
        let (mut ok, mut cancel) = (false, false);
        let mut changed = false;
        egui::Window::new(dialog.filter.name()).open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            let mut slider = |ui: &mut egui::Ui, v: &mut f32, range: std::ops::RangeInclusive<f32>, label: &str| {
                changed |= ui.add(egui::Slider::new(v, range).text(label)).changed();
            };
            match &mut dialog.filter {
                Filter::GaussianBlur { radius } => slider(ui, radius, 0.5..=200.0, "Radius (px)"),
                Filter::MotionBlur { angle, distance } => {
                    slider(ui, angle, -180.0..=180.0, "Angle (°)");
                    slider(ui, distance, 1.0..=300.0, "Distance (px)");
                }
                Filter::RadialBlur { center, amount } => {
                    slider(ui, amount, 0.0..=1.0, "Amount");
                    slider(ui, &mut center[0], 0.0..=canvas.0, "Centre x");
                    slider(ui, &mut center[1], 0.0..=canvas.1, "Centre y");
                }
                Filter::UnsharpMask { radius, amount } => {
                    slider(ui, radius, 0.5..=50.0, "Radius (px)");
                    slider(ui, amount, 0.0..=5.0, "Amount");
                }
                Filter::Noise { amount, monochrome, seed } => {
                    slider(ui, amount, 0.0..=1.0, "Amount");
                    changed |= ui.checkbox(monochrome, tr("Monochrome")).changed();
                    if ui.small_button(tr("New pattern")).clicked() {
                        *seed = seed.wrapping_add(1);
                        changed = true;
                    }
                }
            }
            ui.horizontal(|ui| {
                changed |= ui.checkbox(&mut dialog.preview, tr("Preview")).changed();
                if dialog.job.is_some() {
                    ui.spinner();
                    ui.weak(tr("computing..."));
                }
            });
            ui.small(if dialog.mask {
                tr("Applies to the layer mask being edited, inside the selection if there is one.")
            } else {
                tr("Applies to the edited layer, inside the selection if there is one.")
            });
            ui.horizontal(|ui| {
                ok = ui.button(tr("OK")).clicked();
                cancel = ui.button(tr("Cancel")).clicked();
            });
        });
        if changed {
            dialog.changed_at = Some(Instant::now());
        }
        if ok {
            self.close_filter(true);
            return;
        }
        if cancel || !open {
            self.close_filter(false);
            return;
        }
        // A finished preview: show it if the settings haven't moved on.
        let mut arrived = None;
        if let Some((settings, rx)) = &dialog.job {
            if let Ok(tiles) = rx.try_recv() {
                arrived = Some((settings.clone(), tiles));
            }
        }
        if let Some((settings, tiles)) = arrived {
            dialog.job = None;
            if dialog.preview && settings == dialog.filter {
                dialog.shown = Some(settings);
                let (id, mask) = (dialog.layer, dialog.mask);
                self.set_layer_tiles(id, mask, tiles);
                return;
            }
        }
        // Recompute the preview once the settings settle, off the UI thread.
        let due = dialog.changed_at.is_some_and(|t| t.elapsed() >= PREVIEW_DELAY);
        if dialog.changed_at.is_some() || dialog.job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if due && dialog.job.is_none() {
            dialog.changed_at = None;
            if dialog.preview {
                let (original, selection, canvas, settings) =
                    (dialog.original.clone(), self.doc.selection.clone(), self.doc.canvas(), dialog.filter.clone());
                let (tx, rx) = channel();
                let job = settings.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(filters::apply(&original, canvas, selection.as_ref(), &job));
                });
                dialog.job = Some((settings, rx));
            } else {
                dialog.shown = None;
                let (id, mask, original) = (dialog.layer, dialog.mask, dialog.original.clone());
                self.set_layer_tiles(id, mask, original);
            }
        }
    }
}
