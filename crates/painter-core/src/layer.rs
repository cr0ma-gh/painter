//! Layer tree types.

use serde::{Deserialize, Serialize};

use crate::blend::BlendMode;
use crate::brush::BrushSettings;
use crate::procedural::{Gradient, Tone};
use crate::smoothing::StrokePoint;
use crate::tile::{Tile, TileCoord, TileStore};

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
    /// The stroke engine rules it was drawn with: 0 for strokes saved before
    /// 1.0.3 (the field is new), [`VECTOR_ENGINE`] since. Replays keep a
    /// stroke's own rules, so updating Painter doesn't change artwork.
    #[serde(default)]
    pub engine: u32,
}

/// The stroke engine rules new vector strokes record (see [`VectorStroke::engine`]).
/// 1 (1.0.3): tilt curves wait for a pen that reports tilt; a second tip
/// smaller than the dab repeats inside it.
pub const VECTOR_ENGINE: u32 = 1;

impl VectorStroke {
    /// The brush its dabs are stamped with: for a stroke saved before 1.0.3, a
    /// second tip smaller than the dab stays one centred stamp, as it was drawn.
    pub fn stamp_settings(&self) -> std::borrow::Cow<'_, BrushSettings> {
        match &self.settings.dual {
            Some(dual) if self.engine == 0 => {
                let mut settings = self.settings.clone();
                settings.dual = Some(crate::tip::DualTip { centred: true, ..dual.clone() });
                std::borrow::Cow::Owned(settings)
            }
            _ => std::borrow::Cow::Borrowed(&self.settings),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct VectorLayer {
    pub strokes: Vec<VectorStroke>,
    /// Text and balloons, drawn over the strokes.
    pub texts: Vec<crate::text::TextItem>,
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

impl Mask {
    /// How much a straight sRGB colour hides when painted on a mask, as in
    /// Photoshop: black hides, white reveals, greys in between (by luma).
    pub fn hide_amount(rgb: [f32; 3]) -> f32 {
        1.0 - (0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2]).clamp(0.0, 1.0)
    }

    /// Settles freshly edited mask tiles. Brushes, fills and gradients paint a
    /// mask with a grey whose red channel is the amount hidden (painting grey
    /// over grey blends towards it, as in Photoshop); the compositor reads red
    /// too. Settling copies red into the other channels and alpha, so every
    /// reader of the committed mask (CPU compositing, exports, thumbnails) sees
    /// the same amount. Tiles left fully visible are dropped.
    pub fn settle(tiles: &mut TileStore, coords: impl IntoIterator<Item = TileCoord>) {
        for coord in coords {
            let Some(tile) = tiles.get(coord) else { continue };
            if is_settled(tile) {
                continue;
            }
            let tile = tiles.get_or_create_mut(coord);
            settle_tile(tile);
            if tile.is_transparent() {
                tiles.remove(coord);
            }
        }
    }

    /// [`Mask::settle`] on every tile of `tiles`.
    pub fn settle_all(tiles: &mut TileStore) {
        let coords: Vec<TileCoord> = tiles.iter().map(|(c, _)| c).collect();
        Self::settle(tiles, coords);
    }

    /// The mask turned inside out within the canvas: hidden becomes shown.
    pub fn inverted(&self, canvas: (u32, u32)) -> TileStore {
        crate::selection::invert(Some(&self.tiles), canvas)
    }

    /// The pixels the mask shows, as a selection (Ctrl+click on its thumbnail).
    pub fn to_selection(&self, canvas: (u32, u32)) -> TileStore {
        self.inverted(canvas)
    }

    /// Bakes the mask into `pixels` (premultiplied): what it hides is erased,
    /// partly hidden pixels fade. Photoshop's Apply Layer Mask.
    pub fn apply_to(&self, pixels: &mut TileStore) {
        let coords: Vec<TileCoord> = pixels.iter().map(|(c, _)| c).filter(|c| self.tiles.get(*c).is_some()).collect();
        for coord in coords {
            let Some(mask) = self.tiles.get(coord).cloned() else { continue };
            let tile = pixels.get_or_create_mut(coord);
            for (p, m) in tile.bytes_mut().as_chunks_mut::<4>().0.iter_mut().zip(mask.bytes().as_chunks::<4>().0) {
                let keep = 255 - m[3] as u32;
                *p = p.map(|v| ((v as u32 * keep + 127) / 255) as u8);
            }
            if tile.is_transparent() {
                pixels.remove(coord);
            }
        }
    }
}

/// Whether every pixel of a mask tile already has red = green = blue = alpha.
fn is_settled(tile: &Tile) -> bool {
    tile.bytes().as_chunks::<4>().0.iter().all(|p| p[1] == p[0] && p[2] == p[0] && p[3] == p[0])
}

/// One mask tile's red copied into its other channels (see [`Mask::settle`]).
pub fn settle_tile(tile: &mut Tile) {
    for p in tile.bytes_mut().as_chunks_mut::<4>().0 {
        *p = [p[0]; 4];
    }
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
    /// Clip Studio's Layer Property: expression colour, border, tone, extract
    /// line. Drawn from the pixels, which stay as painted (see [`crate::effects`]).
    pub effects: crate::effects::LayerEffects,
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
            effects: Default::default(),
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

    #[test]
    fn grey_mask_paint_settles_on_red() {
        assert_eq!(Mask::hide_amount([0.0; 3]), 1.0);
        assert_eq!(Mask::hide_amount([1.0; 3]), 0.0);
        let mut tiles = TileStore::new();
        let c = TileCoord::new(0, 0);
        // Half-grey paint (red 128) fully covering, and a pixel painted white (red 0).
        tiles.get_or_create_mut(c).set_pixel(0, 0, [128, 128, 128, 255]);
        tiles.get_or_create_mut(c).set_pixel(1, 0, [0, 0, 0, 255]);
        Mask::settle(&mut tiles, [c]);
        assert_eq!(tiles.get(c).unwrap().pixel(0, 0), [128; 4]);
        assert_eq!(tiles.get(c).unwrap().pixel(1, 0), [0; 4]);
        // A tile painted all white settles to nothing hidden: dropped.
        let mut white = TileStore::new();
        white.get_or_create_mut(c).set_pixel(5, 5, [0, 0, 0, 200]);
        Mask::settle(&mut white, [c]);
        assert!(white.is_empty());
    }

    #[test]
    fn applying_a_mask_fades_hidden_pixels() {
        let c = TileCoord::new(0, 0);
        let mut pixels = TileStore::new();
        for x in 0..3 {
            pixels.get_or_create_mut(c).set_pixel(x, 0, [200, 100, 0, 255]);
        }
        let mut mask = Mask { enabled: true, ..Mask::default() };
        mask.tiles.get_or_create_mut(c).set_pixel(0, 0, [255; 4]);
        mask.tiles.get_or_create_mut(c).set_pixel(1, 0, [128; 4]);
        mask.apply_to(&mut pixels);
        let t = pixels.get(c).unwrap();
        assert_eq!(t.pixel(0, 0), [0; 4]);
        assert_eq!(t.pixel(1, 0), [100, 50, 0, 127]);
        assert_eq!(t.pixel(2, 0), [200, 100, 0, 255]);
        // Inverted, the mask hides what it showed; as a selection it selects what it shows.
        let inv = mask.inverted((64, 64));
        assert_eq!(inv.pixel(0, 0)[3], 0);
        assert_eq!(inv.pixel(2, 0)[3], 255);
        assert_eq!(mask.to_selection((64, 64)).pixel(1, 0)[3], 127);
    }
}
