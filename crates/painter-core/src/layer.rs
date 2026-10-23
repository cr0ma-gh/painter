//! Layer tree types.

use serde::{Deserialize, Serialize};

use crate::blend::BlendMode;
use crate::brush::BrushSettings;
use crate::procedural::{Gradient, Tone};
use crate::smoothing::StrokePoint;
use crate::tile::TileStore;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LayerId(pub u64);

#[derive(Clone, Debug)]
pub struct Folder {
    /// Bottom to top.
    pub children: Vec<Layer>,
    /// "Through" blending: children composite straight onto what is below the
    /// folder, and the folder's own blend mode and opacity are ignored. A
    /// folder with an enabled mask is always composited in isolation.
    pub pass_through: bool,
}

/// One recorded vector stroke: replaying `points` through the stroke engine
/// with `settings` reproduces its dabs exactly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VectorStroke {
    pub points: Vec<StrokePoint>,
    pub settings: BrushSettings,
    /// Straight sRGB colour.
    pub color: [f32; 3],
    /// Seed of the stroke's random dynamics, so replays match.
    #[serde(default)]
    pub seed: u64,
}

#[derive(Clone, Debug, Default)]
pub struct VectorLayer {
    pub strokes: Vec<VectorStroke>,
    /// Rasterised strokes; what is composited and exported.
    pub cache: TileStore,
}

#[derive(Clone, Debug)]
pub enum LayerKind {
    Raster(TileStore),
    Vector(VectorLayer),
    Folder(Folder),
    /// Solid opaque colour covering the canvas (the "paper").
    Fill { color: [u8; 3] },
    Gradient(Gradient),
    Tone(Tone),
    /// A correction layer: changes the colours of what is below it.
    Adjustment(crate::adjust::Adjustment),
}

/// A layer mask. Tile alpha is the amount *hidden* (0 = fully visible), so an
/// empty mask hides nothing and costs no memory.
#[derive(Clone, Debug, Default)]
pub struct Mask {
    pub tiles: TileStore,
    pub enabled: bool,
}

#[derive(Clone, Debug)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub kind: LayerKind,
    pub blend: BlendMode,
    pub opacity: f32,
    pub visible: bool,
    pub locked: bool,
    /// Painting only changes colour where pixels are already opaque.
    pub alpha_locked: bool,
    /// Clip to the alpha of the nearest unclipped raster/vector layer below.
    pub clip: bool,
    /// Fill tools may read their boundaries from layers marked like this
    /// (typically the inks) while painting on another layer.
    pub reference: bool,
    /// A draft (Clip Studio's draft layer): shown while working, left out of
    /// exports and prints.
    pub draft: bool,
    /// Clip Studio's layer colour: shown with its dark pixels in this colour
    /// (light ones stay white), on screen only; files and exports keep the
    /// real colours.
    pub layer_color: Option<[u8; 3]>,
    /// A file object (Clip Studio) / linked smart object (Photoshop): the
    /// pixels are an outside file drawn where it's placed, redrawn from the
    /// file when it moves or the file changes.
    pub file_object: Option<FileObject>,
    pub mask: Option<Mask>,
}

/// Where a file object's picture comes from and where it goes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileObject {
    /// The file, as an absolute path.
    pub path: String,
    /// Its picture's pixels to canvas pixels (3 × 3, rows; projective).
    pub placement: [[f64; 3]; 3],
    /// Its picture's size, in pixels.
    pub size: [u32; 2],
    /// The file's modification time when last read (seconds since 1970).
    #[serde(default)]
    pub modified: u64,
}

impl Layer {
    fn with_kind(id: LayerId, name: impl Into<String>, kind: LayerKind) -> Self {
        Self {
            id,
            name: name.into(),
            kind,
            blend: BlendMode::Normal,
            opacity: 1.0,
            visible: true,
            locked: false,
            alpha_locked: false,
            clip: false,
            reference: false,
            draft: false,
            layer_color: None,
            file_object: None,
            mask: None,
        }
    }

    pub fn raster(id: LayerId, name: impl Into<String>) -> Self {
        Self::with_kind(id, name, LayerKind::Raster(TileStore::new()))
    }

    pub fn raster_with(id: LayerId, name: impl Into<String>, tiles: TileStore) -> Self {
        Self::with_kind(id, name, LayerKind::Raster(tiles))
    }

    pub fn vector(id: LayerId, name: impl Into<String>) -> Self {
        Self::with_kind(id, name, LayerKind::Vector(VectorLayer::default()))
    }

    pub fn folder(id: LayerId, name: impl Into<String>) -> Self {
        Self::with_kind(id, name, LayerKind::Folder(Folder { children: Vec::new(), pass_through: true }))
    }

    pub fn fill(id: LayerId, name: impl Into<String>, color: [u8; 3]) -> Self {
        Self::with_kind(id, name, LayerKind::Fill { color })
    }

    pub fn gradient(id: LayerId, name: impl Into<String>, gradient: Gradient) -> Self {
        Self::with_kind(id, name, LayerKind::Gradient(gradient))
    }

    pub fn adjustment(id: LayerId, adjustment: crate::adjust::Adjustment) -> Self {
        let name = adjustment.name();
        Self::with_kind(id, name, LayerKind::Adjustment(adjustment))
    }

    pub fn tone(id: LayerId, name: impl Into<String>, tone: Tone) -> Self {
        Self::with_kind(id, name, LayerKind::Tone(tone))
    }

    /// Pixel tiles: a raster layer's content or a vector layer's cache.
    pub fn tiles(&self) -> Option<&TileStore> {
        match &self.kind {
            LayerKind::Raster(tiles) => Some(tiles),
            LayerKind::Vector(vector) => Some(&vector.cache),
            _ => None,
        }
    }

    pub fn tiles_mut(&mut self) -> Option<&mut TileStore> {
        match &mut self.kind {
            LayerKind::Raster(tiles) => Some(tiles),
            LayerKind::Vector(vector) => Some(&mut vector.cache),
            _ => None,
        }
    }

    pub fn is_folder(&self) -> bool {
        matches!(self.kind, LayerKind::Folder(_))
    }

    pub fn is_vector(&self) -> bool {
        matches!(self.kind, LayerKind::Vector(_))
    }

    pub fn has_pixels(&self) -> bool {
        matches!(self.kind, LayerKind::Raster(_) | LayerKind::Vector(_))
    }

    pub fn can_paint(&self) -> bool {
        self.has_pixels() && self.visible && !self.locked
    }

    /// The mask tiles, if a mask exists and is enabled.
    pub fn active_mask(&self) -> Option<&TileStore> {
        self.mask.as_ref().filter(|m| m.enabled).map(|m| &m.tiles)
    }

    /// Folders composite in isolation unless pass-through without an active mask.
    pub fn is_isolated_folder(&self) -> bool {
        match &self.kind {
            LayerKind::Folder(f) => !f.pass_through || self.active_mask().is_some(),
            _ => false,
        }
    }
}

/// How a layer is clipped, resolved within its sibling list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipTo {
    /// Not clipped, or clipping has no effect (no base, base without pixels).
    Nothing,
    /// Multiply alpha by this layer's pixel alpha.
    Layer(LayerId),
    /// The clipping base is hidden, so the clipped layer is hidden too.
    Skip,
}

/// Resolves clipping for siblings listed bottom to top.
pub fn clip_targets(siblings: &[Layer]) -> Vec<ClipTo> {
    let mut base: Option<&Layer> = None;
    siblings
        .iter()
        .map(|layer| {
            if !layer.clip {
                base = Some(layer);
                return ClipTo::Nothing;
            }
            match base {
                None => ClipTo::Nothing,
                Some(b) if !b.visible => ClipTo::Skip,
                Some(b) if b.has_pixels() => ClipTo::Layer(b.id),
                Some(_) => ClipTo::Nothing,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipping_resolves_to_nearest_unclipped_base() {
        let mut layers = vec![
            Layer::fill(LayerId(1), "Paper", [255; 3]),
            Layer::raster(LayerId(2), "Base"),
            Layer::raster(LayerId(3), "Clip A"),
            Layer::raster(LayerId(4), "Clip B"),
            Layer::raster(LayerId(5), "Free"),
        ];
        layers[2].clip = true;
        layers[3].clip = true;
        let clips = clip_targets(&layers);
        assert_eq!(clips[2], ClipTo::Layer(LayerId(2)));
        assert_eq!(clips[3], ClipTo::Layer(LayerId(2)));
        assert_eq!(clips[4], ClipTo::Nothing);

        layers[1].visible = false;
        assert_eq!(clip_targets(&layers)[3], ClipTo::Skip);
        layers[1].clip = true; // clipped onto the paper: no pixel base, so no effect
        layers[1].visible = true;
        assert_eq!(clip_targets(&layers)[1], ClipTo::Nothing);
    }
}
