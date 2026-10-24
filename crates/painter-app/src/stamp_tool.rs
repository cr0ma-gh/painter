//! The clone stamp (S), the pattern stamp (Shift+S) and the history brush (Y),
//! as in Photoshop. They paint with the current brush's tip and dynamics, the
//! colour coming from elsewhere:
//! - clone: another spot of the layer or of the whole image (Alt-click picks
//!   it; "Aligned" keeps the same offset from one stroke to the next);
//! - pattern: a repeated pattern (built in, or Edit → Define Pattern);
//! - history: the same spot at an earlier state (the History panel picks it;
//!   the oldest kept state otherwise), transparency included.

use std::path::PathBuf;
use crate::i18n::tr;

use eframe::egui::{self, Color32, Painter, Pos2, Rect, Stroke, Vec2};
use painter_core::raster::RasterizedStroke;
use painter_core::source_stroke::{self, Pattern, SourceMerge, StrokeSource};
use painter_core::{Layer, LayerKind, TileStore};
use painter_render::Plane;

use crate::app::PainterApp;
use crate::tools::Tool;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StampKind {
    Clone,
    Pattern,
    History,
}

pub(crate) struct StampOptions {
    /// Clone: the offset stays from one stroke to the next.
    pub aligned: bool,
    /// Clone: sample the visible image, not only the current layer.
    pub all_layers: bool,
    pub patterns: Vec<Pattern>,
    pub pattern: usize,
    pub pattern_scale: f32,
    /// Pattern: its shapes in the main colour instead of its own colours.
    pub tint: bool,
}

impl Default for StampOptions {
    fn default() -> Self {
        let mut patterns = Pattern::builtin(16, [0, 0, 0]);
        patterns.extend(load_patterns());
        Self { aligned: true, all_layers: false, patterns, pattern: 0, pattern_scale: 1.0, tint: true }
    }
}

/// Where the clone stamp copies from.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CloneSource {
    /// The spot Alt-clicked last.
    pub point: Option<Pos2>,
    /// Source minus destination, once a stroke started (kept while aligned).
    pub offset: Option<Vec2>,
}

/// A stroke painting from a source: stamped on the UI thread, merged as it goes.
pub(crate) struct SourceStroke {
    pub source: StrokeSource,
    pub merge: SourceMerge,
    pub stamped: RasterizedStroke,
}

fn patterns_dir() -> Option<PathBuf> {
    crate::files::app_data_dir().map(|d| d.join("patterns"))
}

/// Patterns saved with Define Pattern.
fn load_patterns() -> Vec<Pattern> {
    let Some(dir) = patterns_dir() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "png")).collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|p| {
            let img = image::open(&p).ok()?.to_rgba8();
            let (w, h) = img.dimensions();
            let rgba = img.into_raw().as_chunks::<4>().0.iter().flat_map(|&c| painter_core::blend::premultiply(c)).collect();
            Some(Pattern::new(&p.file_stem()?.to_string_lossy(), w, h, rgba))
        })
        .collect()
}

/// The layer with `id` anywhere in `layers`.
fn find_layer(layers: &[Layer], id: painter_core::LayerId) -> Option<&Layer> {
    layers.iter().find_map(|l| {
        if l.id == id {
            return Some(l);
        }
        match &l.kind {
            LayerKind::Folder(f) => find_layer(&f.children, id),
            _ => None,
        }
    })
}

impl Tool {
    /// Tools that paint strokes with the brush tip.
    pub fn paints(self) -> bool {
        matches!(self, Tool::Brush | Tool::Stamp(_))
    }
}

impl PainterApp {
    /// Alt-click with the clone stamp: copy from here.
    pub(crate) fn set_clone_source(&mut self, at: Pos2) {
        self.clone_source = CloneSource { point: Some(at), offset: None };
        self.status = format!("Clone source at {:.0}, {:.0}", at.x, at.y);
    }

    /// Where a stamp stroke starting at `at` on `plane` takes its colours
    /// (`before`: the plane as the stroke starts). `None`: nothing to paint from.
    pub(crate) fn stamp_source(&mut self, kind: StampKind, plane: Plane, before: &TileStore, at: Pos2) -> Option<SourceStroke> {
        let (source, merge) = match kind {
            StampKind::Clone => {
                let Some(point) = self.clone_source.point else {
                    self.status = tr("Clone stamp: Alt-click where to copy from first").into();
                    return None;
                };
                let offset = match self.clone_source.offset {
                    Some(o) if self.tools.stamps.aligned => o,
                    _ => point - at,
                };
                self.clone_source.offset = Some(offset);
                let image = if self.tools.stamps.all_layers {
                    let (w, h) = self.doc.canvas();
                    TileStore::from_rgba(w, h, &painter_core::io::flatten(&self.doc))
                } else {
                    before.clone()
                };
                (StrokeSource::Image { image, offset: [offset.x.round() as i32, offset.y.round() as i32] }, SourceMerge::Over)
            }
            StampKind::Pattern => {
                let o = &self.tools.stamps;
                let mut pattern = o.patterns.get(o.pattern).or(o.patterns.first())?.clone();
                if o.tint || matches!(plane, Plane::Mask(_)) {
                    let ink = if matches!(plane, Plane::Mask(_)) { [1.0; 3] } else { self.main_rgb() };
                    let rgba: Vec<u8> = pattern
                        .rgba
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .flat_map(|c| {
                            let a = c[3] as f32;
                            [(ink[0] * a + 0.5) as u8, (ink[1] * a + 0.5) as u8, (ink[2] * a + 0.5) as u8, c[3]]
                        })
                        .collect();
                    pattern = Pattern::new(&pattern.name, pattern.width, pattern.height, rgba);
                }
                (StrokeSource::Pattern { pattern, scale: o.pattern_scale }, SourceMerge::Over)
            }
            StampKind::History => {
                let Some((_, layers)) = self.history.source() else {
                    self.status = tr("History brush: nothing to go back to yet").into();
                    return None;
                };
                let image = match plane {
                    Plane::Color(id) => find_layer(layers, id).and_then(Layer::tiles).cloned(),
                    Plane::Mask(id) => find_layer(layers, id).and_then(|l| l.mask.as_ref()).map(|m| m.tiles.clone()),
                    _ => None,
                };
                (StrokeSource::Image { image: image.unwrap_or_default(), offset: [0, 0] }, SourceMerge::Replace)
            }
        };
        Some(SourceStroke { source, merge, stamped: source_stroke::new_stroke() })
    }

    /// The clone source's marker, and the source spot following the pointer.
    pub(crate) fn draw_stamp_overlay(&self, painter: &Painter, rect: Rect) {
        if self.tool != Tool::Stamp(StampKind::Clone) {
            return;
        }
        let doc = self.doc_size();
        let screen = |p: Pos2| self.view.doc_to_screen(rect, doc, p);
        let hover = painter.ctx().pointer_hover_pos().filter(|p| rect.contains(*p)).map(|p| self.view.screen_to_doc(rect, doc, p));
        let spot = match (self.clone_source.offset, hover) {
            (Some(offset), Some(h)) if self.stroke.is_some() || self.tools.stamps.aligned => Some(h + offset),
            _ => self.clone_source.point,
        };
        let Some(spot) = spot else { return };
        let c = screen(spot);
        let r = (self.current_brush().size * 0.5 * self.view.zoom).max(4.0);
        for (w, colour) in [(3.0_f32, Color32::from_black_alpha(140)), (1.0, Color32::WHITE)] {
            let s = Stroke::new(w, colour);
            painter.circle_stroke(c, r, s);
            painter.line_segment([c - Vec2::X * 6.0, c + Vec2::X * 6.0], s);
            painter.line_segment([c - Vec2::Y * 6.0, c + Vec2::Y * 6.0], s);
        }
    }

    pub(crate) fn stamp_panel(&mut self, ui: &mut egui::Ui, kind: StampKind) {
        match kind {
            StampKind::Clone => {
                let o = &mut self.tools.stamps;
                ui.checkbox(&mut o.aligned, tr("Aligned")).on_hover_text(tr("Keep the same offset from one stroke to the next"));
                ui.horizontal(|ui| {
                    ui.label(tr("Sample"));
                    ui.selectable_value(&mut o.all_layers, false, tr("Current layer"));
                    ui.selectable_value(&mut o.all_layers, true, tr("All layers"));
                });
                ui.small(match self.clone_source.point {
                    Some(p) => format!("Copying from {:.0}, {:.0}. Alt-click to pick another spot.", p.x, p.y),
                    None => "Alt-click where to copy from, then paint.".into(),
                });
            }
            StampKind::Pattern => {
                let o = &mut self.tools.stamps;
                let current = o.patterns.get(o.pattern).map_or("None", |p| p.name.as_str()).to_string();
                egui::ComboBox::from_label(tr("Pattern")).selected_text(current).show_ui(ui, |ui| {
                    for (i, p) in o.patterns.iter().enumerate() {
                        ui.selectable_value(&mut o.pattern, i, &p.name);
                    }
                });
                ui.add(egui::Slider::new(&mut o.pattern_scale, 0.25..=8.0).logarithmic(true).text(tr("Scale")));
                ui.checkbox(&mut o.tint, tr("Main colour")).on_hover_text(tr("Paint the pattern's shapes in the main colour"));
                if ui.button(tr("Define Pattern from Selection")).clicked() {
                    self.define_pattern();
                }
            }
            StampKind::History => {
                let from = self.history.source().map_or("nothing yet".to_string(), |(l, _)| format!("“{l}”"));
                ui.label(format!("Paints from {from}"));
                ui.small(tr("Pick another state with the brush button in the History panel."));
            }
        }
        ui.small(tr("The tip and dynamics are the current brush's: pick one in the Tool Group."));
    }

    /// Edit → Define Pattern: the selected part of the layer (all of it without
    /// a selection) becomes a pattern, saved for next time.
    pub(crate) fn define_pattern(&mut self) {
        let Some(layer) = self.doc.active_layer() else { return };
        let Some(tiles) = layer.tiles() else {
            self.status = tr("Define Pattern works on pixel layers").into();
            return;
        };
        let area = match &self.doc.selection {
            Some(sel) => painter_core::selection::bounds(sel),
            None => painter_core::selection::bounds(tiles),
        };
        let Some([x0, y0, x1, y1]) = area else {
            self.status = tr("Define Pattern: nothing there").into();
            return;
        };
        let (w, h) = ((x1 - x0 + 1).min(2048) as u32, (y1 - y0 + 1).min(2048) as u32);
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                rgba.extend_from_slice(&tiles.pixel(x0 + x, y0 + y));
            }
        }
        let n = self.tools.stamps.patterns.len() + 1;
        let pattern = Pattern::new(&format!("Pattern {n}"), w, h, rgba);
        if let Some(dir) = patterns_dir() {
            let straight: Vec<u8> =
                pattern.rgba.as_chunks::<4>().0.iter().flat_map(|&c| painter_core::blend::unpremultiply(c)).collect();
            let saved = std::fs::create_dir_all(&dir).is_ok()
                && image::RgbaImage::from_raw(w, h, straight)
                    .is_some_and(|img| img.save(dir.join(format!("{}.png", pattern.name))).is_ok());
            if !saved {
                self.status = tr("Couldn't save the pattern; it's there until Painter closes").into();
            }
        }
        self.status = format!("Defined “{}” ({w} × {h} px)", pattern.name);
        self.tools.stamps.patterns.push(pattern);
        self.tools.stamps.pattern = self.tools.stamps.patterns.len() - 1;
        self.tool = Tool::Stamp(StampKind::Pattern);
    }
}

impl PainterApp {
    /// For screenshots (`dev.stamp_demo`): a disc, cloned to the right, a pattern
    /// stroke, and the history brush wiping part of the disc back to blank.
    pub(crate) fn stamp_demo(&mut self) {
        let (w, h) = self.doc.canvas();
        let (w, h) = (w as f32, h as f32);
        self.tools.shapes.style = crate::shape_tool::ShapeStyle::Fill;
        self.tools.shapes.kind = crate::shape_tool::ShapeKind::Ellipse;
        let disc = crate::shape_tool::dragged_shape(
            crate::shape_tool::ShapeKind::Ellipse,
            Pos2::new(w * 0.1, h * 0.1),
            Pos2::new(w * 0.4, h * 0.3),
            false,
            false,
            0.0,
        );
        if let Some(disc) = disc {
            self.draw_shape(&disc);
        }
        self.current_brush_mut().size = w / 12.0;
        let line = |app: &mut PainterApp, y: f32, x0: f32, x1: f32| {
            let pts: Vec<_> = (0..=40).map(|i| x0 + (x1 - x0) * i as f32 / 40.0).collect();
            app.begin_stroke(painter_core::StrokePoint::new(pts[0], y, 1.0));
            for x in &pts[1..] {
                app.continue_stroke(painter_core::StrokePoint::new(*x, y, 1.0));
            }
            app.end_stroke();
        };
        // Clone: from the disc's middle to the right half.
        self.tool = Tool::Stamp(StampKind::Clone);
        self.set_clone_source(Pos2::new(w * 0.25, h * 0.2));
        line(self, h * 0.2, w * 0.6, w * 0.8);
        // Pattern: a band below.
        self.tool = Tool::Stamp(StampKind::Pattern);
        self.tools.stamps.pattern = 0;
        self.tools.stamps.pattern_scale = 3.0;
        line(self, h * 0.5, w * 0.1, w * 0.9);
        // History: back to the blank page across the disc.
        self.tool = Tool::Stamp(StampKind::History);
        line(self, h * 0.2, w * 0.05, w * 0.3);
    }
}
