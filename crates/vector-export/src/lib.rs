//! SVG export of a document or one layer, tracing raster line art into
//! Bezier paths.
//!
//! The pipeline per layer (see [`export_svg`]):
//!
//! - **Folders** become nested groups, so the layer hierarchy, names,
//!   visibility, opacity and blend modes (as CSS `mix-blend-mode`) survive.
//! - **Vector layers**: each stroke is replayed into its dabs and written as a
//!   filled outline that keeps the pressure profile, or as a centre line
//!   ([`outline`]), in the stroke's colour and opacity.
//! - **Raster layers** (painted line art, cleaned-up scans): thresholded
//!   ([`bitmap`]) and traced ([`trace`]: contours → simplification → corner
//!   detection and curve fitting) into one even-odd path in the layer's
//!   average ink colour.
//! - **Paper** layers become a rectangle; **gradient** layers a rectangle
//!   filled with an SVG linear or radial gradient (exact: same geometry and
//!   straight-colour interpolation); **tone** layers a rotated dot pattern.
//! - **Layer masks** become luminance masks with the hidden areas traced
//!   (hard edges). **Clipping** becomes an alpha mask that reuses the base
//!   layer's content with `<use>` (SVG 2 `mask-type="alpha"`).

pub mod bitmap;
pub mod outline;
pub mod path;
pub mod svg;
pub mod trace;

pub use bitmap::{Bitmap, ThresholdSource};
pub use outline::StrokeMode;
pub use path::{FillRule, Segment, StrokeStyle, VectorPath};
pub use svg::{Paint, Stop, SvgDef, SvgDocument, SvgGroup, SvgNode};
pub use trace::TraceSettings;

use painter_core::{clip_targets, ClipTo, Document, GradientShape, Layer, LayerId, LayerKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportScope {
    /// Every layer, in a group per layer.
    Canvas,
    /// One layer (a folder with its children).
    Layer(LayerId),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExportOptions {
    pub scope: ExportScope,
    pub trace: TraceSettings,
    pub strokes: StrokeMode,
    /// Write paper layers as a background rectangle.
    pub include_paper: bool,
    /// Skip hidden layers instead of writing them with `display="none"`.
    pub skip_hidden: bool,
    /// Coordinate decimals in the SVG.
    pub decimals: usize,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            scope: ExportScope::Canvas,
            trace: TraceSettings::default(),
            strokes: StrokeMode::FilledOutline,
            include_paper: true,
            skip_hidden: false,
            decimals: 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExportReport {
    pub svg: SvgDocument,
    /// Layers or features left out, for the status bar.
    pub warnings: Vec<String>,
    /// Paths written.
    pub paths: usize,
}

impl ExportReport {
    pub fn to_svg(&self) -> String {
        self.svg.to_svg()
    }
}

/// Builds the SVG for `doc` per `options`.
pub fn export_svg(doc: &Document, options: &ExportOptions) -> Result<ExportReport, String> {
    let mut exporter = Exporter { canvas: doc.canvas(), options, defs: Vec::new(), warnings: Vec::new(), paths: 0 };
    let children = match options.scope {
        ExportScope::Canvas => exporter.siblings(&doc.layers),
        ExportScope::Layer(id) => {
            let layer = doc.find(id).ok_or_else(|| format!("no layer with id {}", id.0))?;
            if layer.clip {
                exporter.warnings.push(format!("\"{}\": exported alone, so its clipping is left out", layer.name));
            }
            exporter.layer(layer).into_iter().collect()
        }
    };
    let (width, height) = doc.canvas();
    Ok(ExportReport {
        svg: SvgDocument { width, height, defs: exporter.defs, children, decimals: options.decimals },
        warnings: exporter.warnings,
        paths: exporter.paths,
    })
}

struct Exporter<'a> {
    canvas: (u32, u32),
    options: &'a ExportOptions,
    defs: Vec<SvgDef>,
    warnings: Vec<String>,
    paths: usize,
}

fn stops(from: [u8; 4], to: [u8; 4]) -> Vec<Stop> {
    let stop = |offset: f32, c: [u8; 4]| Stop { offset, color: [c[0], c[1], c[2]].map(|v| v as f32 / 255.0), opacity: c[3] as f32 / 255.0 };
    vec![stop(0.0, from), stop(1.0, to)]
}

impl Exporter<'_> {
    fn canvas_rect(&self, paint: Paint) -> SvgNode {
        let (w, h) = self.canvas;
        SvgNode::Rect { x: 0.0, y: 0.0, width: w as f32, height: h as f32, paint }
    }

    /// One sibling list, bottom to top. Clipped layers get an alpha mask that
    /// reuses their base layer's content (`<use>`), like the compositor's
    /// "multiply by the base's pixel alpha".
    fn siblings(&mut self, layers: &[Layer]) -> Vec<SvgNode> {
        let targets = clip_targets(layers);
        let mut out: Vec<SvgNode> = Vec::new();
        for (layer, target) in layers.iter().zip(targets) {
            let Some(mut node) = self.layer(layer) else { continue };
            match target {
                ClipTo::Nothing => {}
                ClipTo::Skip => {
                    // The base is hidden, so the clipped layer shows nothing either.
                    if let SvgNode::Group(g) = &mut node {
                        g.visible = false;
                    }
                }
                ClipTo::Layer(base) => {
                    let content = format!("content-{}", base.0);
                    let base_id = format!("layer-{}", base.0);
                    let found = out.iter_mut().rev().find_map(|n| match n {
                        SvgNode::Group(g) if g.id == base_id => Some(g),
                        // A clipped wrapper doesn't hold a base.
                        _ => None,
                    });
                    match found {
                        Some(base_group) => {
                            base_group.content_id = Some(content.clone());
                            let mask = format!("clip-{}", layer.id.0);
                            self.defs.push(SvgDef::Mask { id: mask.clone(), alpha: true, children: vec![SvgNode::Use { href: content }] });
                            node = SvgNode::Group(SvgGroup::wrapper(vec![node], Some(mask)));
                        }
                        None => self.warnings.push(format!("\"{}\": clipping base not exported", layer.name)),
                    }
                }
            }
            out.push(node);
        }
        out
    }

    fn layer(&mut self, layer: &Layer) -> Option<SvgNode> {
        if self.options.skip_hidden && !layer.visible {
            return None;
        }
        let children: Vec<SvgNode> = match &layer.kind {
            LayerKind::Folder(folder) => self.siblings(&folder.children),
            LayerKind::Vector(vector) => vector
                .strokes
                .iter()
                .filter_map(|s| outline::stroke_path(s, self.options.strokes, self.options.trace.simplify))
                .map(SvgNode::Path)
                .collect(),
            LayerKind::Raster(tiles) => {
                let (bitmap, color) = bitmap::threshold(tiles, self.canvas, self.options.trace.source, self.options.trace.threshold);
                let segments = trace::trace(&bitmap, &self.options.trace);
                if segments.is_empty() {
                    Vec::new()
                } else {
                    vec![SvgNode::Path(VectorPath::filled(segments, color, 1.0))]
                }
            }
            LayerKind::Fill { color } if self.options.include_paper => {
                vec![self.canvas_rect(Paint::Color(color.map(|c| c as f32 / 255.0)))]
            }
            LayerKind::Fill { .. } => return None,
            // Colour corrections have no SVG equivalent.
            LayerKind::Adjustment(_) => return None,
            LayerKind::Gradient(g) => {
                // Same parameterisation as `Gradient::t`: SVG pads (clamps) beyond the ends too.
                let id = format!("gradient-{}", layer.id.0);
                self.defs.push(match g.shape {
                    GradientShape::Linear => SvgDef::LinearGradient { id: id.clone(), from: g.start, to: g.end, stops: stops(g.from, g.to) },
                    GradientShape::Radial => {
                        let radius = ((g.end[0] - g.start[0]).powi(2) + (g.end[1] - g.start[1]).powi(2)).sqrt();
                        SvgDef::RadialGradient { id: id.clone(), center: g.start, radius, stops: stops(g.from, g.to) }
                    }
                });
                vec![self.canvas_rect(Paint::Ref(id))]
            }
            LayerKind::Tone(tone) => {
                let id = format!("tone-{}", layer.id.0);
                let cell = tone.cell.max(2.0);
                // Same dot area as `Tone::coverage`: density × cell².
                let radius = cell * (tone.density.clamp(0.0, 1.0) / std::f32::consts::PI).sqrt();
                let color = tone.color.map(|c| c as f32 / 255.0);
                self.defs.push(SvgDef::DotPattern { id: id.clone(), cell, angle: tone.angle, radius, color });
                vec![self.canvas_rect(Paint::Ref(id))]
            }
        };
        let mask = layer.mask.as_ref().filter(|m| m.enabled && !m.tiles.is_empty()).map(|m| self.mask(layer, &m.tiles));
        self.paths += children.iter().filter(|c| matches!(c, SvgNode::Path(_))).count();
        Some(SvgNode::Group(SvgGroup {
            id: format!("layer-{}", layer.id.0),
            label: layer.name.clone(),
            opacity: layer.opacity,
            visible: layer.visible,
            blend: layer.blend,
            mask,
            content_id: None,
            children,
        }))
    }

    /// A layer mask as a luminance mask: white canvas, the traced hidden
    /// areas (mask alpha) in black. Soft mask edges become hard ones.
    fn mask(&mut self, layer: &Layer, tiles: &painter_core::TileStore) -> String {
        let id = format!("mask-{}", layer.id.0);
        let (hidden, _) = bitmap::threshold(tiles, self.canvas, ThresholdSource::Alpha, 127);
        let segments = trace::trace(&hidden, &self.options.trace);
        let mut children = vec![self.canvas_rect(Paint::Color([1.0; 3]))];
        if !segments.is_empty() {
            children.push(SvgNode::Path(VectorPath::filled(segments, [0.0; 3], 1.0)));
        }
        self.defs.push(SvgDef::Mask { id: id.clone(), alpha: false, children });
        id
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use painter_core::{BrushSettings, StrokePoint, TileStore, VectorStroke};

    /// Paper, a raster layer with a black square, and a folder holding a vector stroke.
    fn document() -> Document {
        let mut doc = Document::new(64, 64);
        let (w, h) = (64u32, 64u32);
        let rgba: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                if (10..30).contains(&x) && (10..30).contains(&y) { [0, 0, 0, 255] } else { [0; 4] }
            })
            .collect();
        let active = doc.active;
        doc.find_mut(active).unwrap().kind = LayerKind::Raster(TileStore::from_rgba(w, h, &rgba));
        let folder_id = doc.alloc_id();
        let mut folder = Layer::folder(folder_id, "Inks");
        let vector_id = doc.alloc_id();
        let mut vector = Layer::vector(vector_id, "Lines");
        if let LayerKind::Vector(v) = &mut vector.kind {
            v.strokes.push(VectorStroke {
                points: (0..6).map(|i| StrokePoint::new(5.0 + i as f32 * 10.0, 50.0, 0.3 + 0.1 * i as f32)).collect(),
                settings: BrushSettings { size: 6.0, stabilization: 0, ..BrushSettings::g_pen() },
                color: [0.8, 0.1, 0.1],
                seed: 3,
                engine: painter_core::VECTOR_ENGINE,
            });
        }
        if let LayerKind::Folder(f) = &mut folder.kind {
            f.children.push(vector);
        }
        doc.layers.push(folder);
        doc
    }

    #[test]
    fn exports_the_canvas_with_its_hierarchy() {
        let doc = document();
        let report = export_svg(&doc, &ExportOptions::default()).unwrap();
        assert_eq!(report.paths, 2, "traced square + vector stroke");
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        let svg = report.to_svg();
        assert!(svg.contains(r#"inkscape:label="Paper""#) && svg.contains("<rect"));
        assert!(svg.contains(r#"inkscape:label="Layer 1""#));
        // The folder group contains the vector layer group.
        let folder = svg.find(r#"inkscape:label="Inks""#).unwrap();
        let lines = svg.find(r#"inkscape:label="Lines""#).unwrap();
        assert!(lines > folder);
        assert!(svg.contains("#cc1a1a"), "stroke colour");
        assert!(svg.contains("M10 "), "traced square starts on its edge");
    }

    #[test]
    fn exports_gradients_tones_masks_and_clipping() {
        use painter_core::{Gradient, Mask, Tone};
        let mut doc = document();
        // A clipped gradient above the raster square, a tone on top, a mask on the square.
        let gradient_id = doc.alloc_id();
        let mut gradient = Layer::gradient(
            gradient_id,
            "Shade",
            Gradient { start: [0.0, 0.0], end: [64.0, 0.0], from: [255, 0, 0, 255], to: [0, 0, 255, 0], shape: GradientShape::Linear },
        );
        gradient.clip = true;
        doc.layers.insert(2, gradient);
        let tone_id = doc.alloc_id();
        doc.layers.push(Layer::tone(tone_id, "Tone", Tone { cell: 10.0, angle: 30.0, density: 0.25, color: [0, 0, 0] }));
        let hidden: Vec<u8> = (0..64 * 64).flat_map(|i| if i % 64 < 20 { [0, 0, 0, 255] } else { [0; 4] }).collect();
        let active = doc.active;
        doc.find_mut(active).unwrap().mask = Some(Mask { tiles: TileStore::from_rgba(64, 64, &hidden), enabled: true });

        let report = export_svg(&doc, &ExportOptions::default()).unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        let svg = report.to_svg();
        let gid = gradient_id.0;
        assert!(svg.contains(&format!(r#"<linearGradient id="gradient-{gid}""#)));
        assert!(svg.contains(&format!(r#"fill="url(#gradient-{gid})""#)));
        assert!(svg.contains(&format!(r#"<pattern id="tone-{}""#, tone_id.0)));
        // r = cell × √(density / π)
        assert!(svg.contains(r#"r="2.821""#), "{svg}");
        // The square's content is reused by the gradient's clipping mask.
        let base = active.0;
        assert!(svg.contains(&format!(r#"<g id="content-{base}">"#)));
        assert!(svg.contains(&format!(r##"<use href="#content-{base}""##)));
        assert!(svg.contains(&format!(r#"<g mask="url(#clip-{gid})">"#)));
        // The layer mask hides the left 20 px.
        assert!(svg.contains(&format!(r#"mask="url(#mask-{base})""#)));
        assert!(svg.contains(&format!(r#"<mask id="mask-{base}""#)));
    }

    #[test]
    fn exports_one_layer() {
        let doc = document();
        let report = export_svg(&doc, &ExportOptions { scope: ExportScope::Layer(doc.active), ..ExportOptions::default() }).unwrap();
        assert_eq!(report.svg.children.len(), 1);
        assert_eq!(report.paths, 1);
        assert!(!report.to_svg().contains("Paper"));
        assert!(export_svg(&doc, &ExportOptions { scope: ExportScope::Layer(LayerId(999)), ..ExportOptions::default() }).is_err());
    }
}
