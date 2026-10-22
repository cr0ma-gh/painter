//! A flattened description of what to composite, rebuilt from the document each frame.

use painter_core::{clip_targets, AdjustOp, BlendMode, ClipTo, Document, Gradient, Layer, LayerId, LayerKind, Tone};

#[derive(Clone, Debug)]
pub struct PlanNode {
    pub id: LayerId,
    pub blend: BlendMode,
    pub opacity: f32,
    /// The layer has an enabled mask (its `Plane::Mask` tiles).
    pub mask: bool,
    /// Alpha comes from this layer's pixels as well.
    pub clip: Option<LayerId>,
    /// Layer colour (straight sRGB 0..1): dark pixels shown in it, light ones white.
    pub tint: Option<[f32; 3]>,
    pub kind: NodeKind,
}

#[derive(Clone, Debug)]
pub enum NodeKind {
    /// Raster or vector tiles (`Plane::Color`).
    Pixels,
    /// Premultiplied colour covering the canvas.
    Solid([f32; 4]),
    Gradient(Gradient),
    Tone(Tone),
    /// A correction layer: recolours the backdrop.
    Adjust(AdjustOp),
    Group { isolated: bool, children: Vec<PlanNode> },
}

pub fn build_plan(doc: &Document) -> Vec<PlanNode> {
    build(&doc.layers)
}

fn build(layers: &[Layer]) -> Vec<PlanNode> {
    layers
        .iter()
        .zip(clip_targets(layers))
        .filter(|(l, clip)| l.visible && l.opacity > 0.0 && *clip != ClipTo::Skip)
        .map(|(l, clip)| {
            let clip = match clip {
                ClipTo::Layer(id) => Some(id),
                _ => None,
            };
            let kind = match &l.kind {
                LayerKind::Raster(_) | LayerKind::Vector(_) => NodeKind::Pixels,
                LayerKind::Fill { color } => {
                    NodeKind::Solid([color[0] as f32 / 255.0, color[1] as f32 / 255.0, color[2] as f32 / 255.0, 1.0])
                }
                LayerKind::Gradient(g) => NodeKind::Gradient(*g),
                LayerKind::Tone(t) => NodeKind::Tone(*t),
                LayerKind::Adjustment(a) => NodeKind::Adjust(a.op()),
                // A clipped folder needs its own result to clip.
                LayerKind::Folder(f) => {
                    NodeKind::Group { isolated: l.is_isolated_folder() || clip.is_some(), children: build(&f.children) }
                }
            };
            let tint = match (&kind, l.layer_color) {
                (NodeKind::Pixels, Some(c)) => Some(c.map(|v| v as f32 / 255.0)),
                _ => None,
            };
            PlanNode { id: l.id, blend: l.blend, opacity: l.opacity, mask: l.active_mask().is_some(), clip, tint, kind }
        })
        .collect()
}

/// How many nested isolated groups need their own render targets.
pub fn isolation_depth(nodes: &[PlanNode]) -> usize {
    nodes
        .iter()
        .map(|n| match &n.kind {
            NodeKind::Group { isolated, children } => isolation_depth(children) + usize::from(*isolated),
            _ => 0,
        })
        .max()
        .unwrap_or(0)
}

/// The composite split around one layer, for caching while it is painted.
///
/// After inlining pass-through folders, the path to the layer only crosses
/// isolated groups. Each level is `below ⊕ (next level or the layer) ⊕ above`;
/// `levels[0]` is the root.
pub struct LayerPath<'a> {
    pub levels: Vec<PathLevel<'a>>,
    /// The painted layer's node.
    pub node: &'a PlanNode,
}

pub struct PathLevel<'a> {
    pub below: Vec<&'a PlanNode>,
    pub above: Vec<&'a PlanNode>,
    /// The isolated group leading to the next level (`None` on the last level).
    pub enter: Option<&'a PlanNode>,
}

impl PathLevel<'_> {
    /// Source-over is associative, so an all-Normal `above` can be
    /// pre-composited into one layer, unless something there clips to the
    /// painted layer (its alpha changes with every dab). Other blend modes
    /// depend on the backdrop.
    pub fn above_is_flattenable(&self, painted: LayerId) -> bool {
        // Corrections above recolour whatever is below them, the painted layer included.
        self.above.iter().all(|n| n.blend == BlendMode::Normal && n.clip != Some(painted) && !matches!(n.kind, NodeKind::Adjust(_)))
    }
}

/// Nodes in compositing order with pass-through groups replaced by their children.
fn inline_pass_through(nodes: &[PlanNode]) -> Vec<&PlanNode> {
    let mut out = Vec::new();
    for node in nodes {
        match &node.kind {
            NodeKind::Group { isolated: false, children } => out.extend(inline_pass_through(children)),
            _ => out.push(node),
        }
    }
    out
}

/// Splits the plan around pixel layer `id`; `None` if it isn't composited (hidden, zero opacity).
pub fn split_at_layer(nodes: &[PlanNode], id: LayerId) -> Option<LayerPath<'_>> {
    let flat = inline_pass_through(nodes);
    for (i, &node) in flat.iter().enumerate() {
        let level = |enter| PathLevel { below: flat[..i].to_vec(), above: flat[i + 1..].to_vec(), enter };
        match &node.kind {
            NodeKind::Pixels if node.id == id => return Some(LayerPath { levels: vec![level(None)], node }),
            NodeKind::Group { isolated: true, children } => {
                if let Some(mut inner) = split_at_layer(children, id) {
                    inner.levels.insert(0, level(Some(node)));
                    return Some(inner);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels(id: u64, blend: BlendMode) -> PlanNode {
        PlanNode { id: LayerId(id), blend, opacity: 1.0, mask: false, clip: None, tint: None, kind: NodeKind::Pixels }
    }

    fn group(id: u64, isolated: bool, children: Vec<PlanNode>) -> PlanNode {
        PlanNode {
            id: LayerId(id),
            blend: BlendMode::Normal,
            opacity: 0.5,
            mask: false,
            clip: None,
            tint: None,
            kind: NodeKind::Group { isolated, children },
        }
    }

    #[test]
    fn splits_flat_stack() {
        let plan = vec![pixels(1, BlendMode::Normal), pixels(2, BlendMode::Normal), pixels(3, BlendMode::Normal)];
        let path = split_at_layer(&plan, LayerId(2)).unwrap();
        assert_eq!(path.levels.len(), 1);
        assert_eq!((path.levels[0].below.len(), path.levels[0].above.len()), (1, 1));
        assert!(path.levels[0].above_is_flattenable(LayerId(2)));
    }

    #[test]
    fn layers_clipped_to_the_painted_layer_are_not_flattened() {
        let mut clipped = pixels(3, BlendMode::Normal);
        clipped.clip = Some(LayerId(2));
        let plan = vec![pixels(1, BlendMode::Normal), pixels(2, BlendMode::Normal), clipped];
        let path = split_at_layer(&plan, LayerId(2)).unwrap();
        assert!(!path.levels[0].above_is_flattenable(LayerId(2)));
    }

    #[test]
    fn pass_through_is_inlined_and_isolated_adds_level() {
        let plan = vec![
            pixels(1, BlendMode::Normal),
            group(10, false, vec![pixels(2, BlendMode::Multiply), group(11, true, vec![pixels(3, BlendMode::Normal), pixels(4, BlendMode::Normal)])]),
            pixels(5, BlendMode::Screen),
        ];
        let path = split_at_layer(&plan, LayerId(4)).unwrap();
        assert_eq!(path.levels.len(), 2);
        let root = &path.levels[0];
        assert_eq!((root.below.len(), root.above.len()), (2, 1));
        assert_eq!(root.enter.map(|n| n.id), Some(LayerId(11)));
        assert!(!root.above_is_flattenable(LayerId(4)));
        assert_eq!((path.levels[1].below.len(), path.levels[1].above.len()), (1, 0));
        assert!(split_at_layer(&plan, LayerId(9)).is_none());
    }
}
