//! What the fill looks at: the boundary comes from a reference image, which
//! need not be the layer being filled. Colour goes on the edited layer while
//! the lines are read from the edited layer itself, from every visible layer
//! merged, or only from the layers marked as reference (usually the inks).
//!
//! The result is an "open" mask: pixels whose reference colour is within
//! the tolerance of the clicked pixel's, and inside the selection.

use super::{par_rows, Bitmap};
use crate::document::Document;
use crate::io::{composite_layers, Area};
use crate::layer::{Layer, LayerKind};
use crate::tile::{TileCoord, TileStore, TILE_SIZE};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReferenceSource {
    /// The layer (or mask) being filled.
    EditedLayer,
    /// Every visible layer, merged.
    AllLayers,
    /// Only layers marked as reference layers, merged.
    ReferenceLayers,
}

impl ReferenceSource {
    pub const ALL: [ReferenceSource; 3] = [ReferenceSource::EditedLayer, ReferenceSource::AllLayers, ReferenceSource::ReferenceLayers];

    pub fn label(self) -> &'static str {
        match self {
            ReferenceSource::EditedLayer => "Edited layer",
            ReferenceSource::AllLayers => "All layers",
            ReferenceSource::ReferenceLayers => "Reference layers",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReferenceOptions {
    pub source: ReferenceSource,
    /// Merged sources: also read hidden layers.
    pub include_hidden: bool,
    /// Merged sources: leave locked layers out (e.g. a locked rough sketch).
    pub exclude_locked: bool,
}

impl Default for ReferenceOptions {
    fn default() -> Self {
        Self { source: ReferenceSource::EditedLayer, include_hidden: false, exclude_locked: false }
    }
}

/// The merged reference image for [`ReferenceSource::AllLayers`] or
/// [`ReferenceSource::ReferenceLayers`], premultiplied RGBA8, canvas sized.
/// `None` for reference layers when no layer is marked as one.
pub fn reference_pixels(doc: &Document, options: &ReferenceOptions) -> Option<Vec<u8>> {
    let only_marked = options.source == ReferenceSource::ReferenceLayers;
    let layers = pick_layers(&doc.layers, options, only_marked, false);
    if only_marked && layers.is_empty() {
        return None;
    }
    let mut buf = vec![0u8; (doc.width * doc.height * 4) as usize];
    composite_layers(&layers, &mut buf, Area::full(doc.canvas()));
    Some(buf)
}

/// The layer tree reduced to what the reference reads. Tiles are shared, so
/// this copies no pixels. A folder marked as reference counts for all it holds.
fn pick_layers(layers: &[Layer], options: &ReferenceOptions, only_marked: bool, inside_marked: bool) -> Vec<Layer> {
    let mut out = Vec::new();
    for layer in layers {
        if (!layer.visible && !options.include_hidden) || (options.exclude_locked && layer.locked) {
            continue;
        }
        let marked = inside_marked || layer.reference;
        let mut layer = layer.clone();
        layer.visible = true;
        if let LayerKind::Folder(folder) = &mut layer.kind {
            folder.children = pick_layers(&folder.children, options, only_marked, marked);
            if folder.children.is_empty() {
                continue;
            }
        } else if only_marked && !marked {
            continue;
        }
        out.push(layer);
    }
    out
}

fn matches(p: &[u8; 4], target: [u8; 4], tolerance: u8) -> u8 {
    (p[0].abs_diff(target[0]) <= tolerance
        && p[1].abs_diff(target[1]) <= tolerance
        && p[2].abs_diff(target[2]) <= tolerance
        && p[3].abs_diff(target[3]) <= tolerance) as u8
}

/// Open mask from a merged reference image: pixels within `tolerance` of the
/// colour at `seed` (transparent outside the canvas).
pub fn open_mask_from_pixels(pixels: &[u8], canvas: (u32, u32), seed: (i32, i32), tolerance: u8) -> Bitmap {
    let (w, h) = (canvas.0 as usize, canvas.1 as usize);
    let target = if seed.0 >= 0 && seed.1 >= 0 && (seed.0 as usize) < w && (seed.1 as usize) < h {
        let i = (seed.1 as usize * w + seed.0 as usize) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    } else {
        [0; 4]
    };
    let mut out = Bitmap::new(w, h);
    par_rows(&mut out.data, w, |y0, rows| {
        let src = &pixels[y0 * w * 4..(y0 * w + rows.len()) * 4];
        for (o, p) in rows.iter_mut().zip(src.as_chunks::<4>().0) {
            *o = matches(p, target, tolerance);
        }
    });
    out
}

/// Open mask straight from a layer's tiles (missing tiles are transparent).
pub fn open_mask_from_tiles(tiles: &TileStore, canvas: (u32, u32), seed: (i32, i32), tolerance: u8) -> Bitmap {
    let w = canvas.0 as usize;
    let target = tiles.pixel(seed.0, seed.1);
    let empty = matches(&[0; 4], target, tolerance);
    let ts = TILE_SIZE as usize;
    let mut out = Bitmap::new(w, canvas.1 as usize);
    // Bands of whole tile rows, so each thread reads its own tiles.
    let band_rows = ts;
    let mut bands: Vec<&mut [u8]> = out.data.chunks_mut(band_rows * w).collect();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let per_thread = bands.len().div_ceil(threads).max(1);
    std::thread::scope(|s| {
        let mut ty = 0;
        while !bands.is_empty() {
            let take = per_thread.min(bands.len());
            let group: Vec<&mut [u8]> = bands.drain(..take).collect();
            let first = ty;
            ty += take;
            s.spawn(move || {
                for (k, band) in group.into_iter().enumerate() {
                    let tile_y = first + k;
                    let rows = band.len() / w;
                    for tx in 0..w.div_ceil(ts) {
                        let x0 = tx * ts;
                        let x1 = (x0 + ts).min(w);
                        match tiles.get(TileCoord::new(tx as i32, tile_y as i32)) {
                            None => {
                                for ly in 0..rows {
                                    band[ly * w + x0..ly * w + x1].fill(empty);
                                }
                            }
                            Some(tile) => {
                                let bytes = tile.bytes();
                                for ly in 0..rows {
                                    let src = &bytes[ly * ts * 4..(ly * ts + (x1 - x0)) * 4];
                                    for (o, p) in band[ly * w + x0..ly * w + x1].iter_mut().zip(src.as_chunks::<4>().0) {
                                        *o = matches(p, target, tolerance);
                                    }
                                }
                            }
                        }
                    }
                }
            });
        }
    });
    out
}

/// Closes everything outside the selection (alpha below half), so the fill
/// spreads only inside it.
pub fn restrict_to_selection(open: &mut Bitmap, selection: &TileStore) {
    let mut inside = open_mask_from_tiles(selection, (open.width as u32, open.height as u32), (-1, -1), 127);
    // `open_mask_from_tiles` with a transparent target and tolerance 127 marks
    // alpha <= 127 pixels; inside the selection is the rest.
    inside = inside.inverted();
    open.and(&inside);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::LayerId;
    use crate::tile::TileCoord;

    fn line_layer(id: u64, x: i32, h: i32) -> Layer {
        let mut tiles = TileStore::new();
        for y in 0..h {
            let c = TileCoord::containing(x, y);
            let (ox, oy) = c.origin();
            tiles.get_or_create_mut(c).set_pixel((x - ox) as u32, (y - oy) as u32, [0, 0, 0, 255]);
        }
        Layer::raster_with(LayerId(id), format!("line {x}"), tiles)
    }

    #[test]
    fn tile_and_pixel_masks_agree() {
        let layer = line_layer(1, 70, 100);
        let tiles = layer.tiles().unwrap();
        let canvas = (150, 100);
        let from_tiles = open_mask_from_tiles(tiles, canvas, (5, 5), 16);
        let mut dense = vec![0u8; 150 * 100 * 4];
        for y in 0..100 {
            for x in 0..150 {
                let p = tiles.pixel(x, y);
                dense[((y * 150 + x) * 4) as usize..((y * 150 + x) * 4 + 4) as usize].copy_from_slice(&p);
            }
        }
        assert_eq!(from_tiles, open_mask_from_pixels(&dense, canvas, (5, 5), 16));
        assert!(!from_tiles.get(70, 80) && from_tiles.get(71, 80) && from_tiles.get(149, 99));
    }

    #[test]
    fn reference_layers_and_exclusions() {
        let mut doc = Document::new(100, 40);
        let inks = line_layer(10, 30, 40);
        let mut sketch = line_layer(11, 60, 40);
        sketch.locked = true;
        let mut hidden = line_layer(12, 80, 40);
        hidden.visible = false;
        doc.layers.extend([inks, sketch, hidden]);

        let open = |doc: &Document, options: ReferenceOptions| {
            reference_pixels(doc, &options).map(|px| open_mask_from_pixels(&px, doc.canvas(), (5, 5), 16))
        };
        let all = open(&doc, ReferenceOptions { source: ReferenceSource::AllLayers, ..ReferenceOptions::default() }).unwrap();
        assert!(!all.get(30, 5) && !all.get(60, 5) && all.get(80, 5), "hidden layers are left out");
        let with_hidden =
            open(&doc, ReferenceOptions { source: ReferenceSource::AllLayers, include_hidden: true, ..ReferenceOptions::default() })
                .unwrap();
        assert!(!with_hidden.get(80, 5));
        let unlocked =
            open(&doc, ReferenceOptions { source: ReferenceSource::AllLayers, exclude_locked: true, ..ReferenceOptions::default() })
                .unwrap();
        assert!(!unlocked.get(30, 5) && unlocked.get(60, 5));

        let refs = ReferenceOptions { source: ReferenceSource::ReferenceLayers, ..ReferenceOptions::default() };
        assert!(reference_pixels(&doc, &refs).is_none(), "nothing marked yet");
        doc.find_mut(LayerId(10)).unwrap().reference = true;
        let only_inks = open(&doc, refs).unwrap();
        // The paper isn't a reference: the background reads as transparent.
        assert!(!only_inks.get(30, 5) && only_inks.get(60, 5) && only_inks.get(5, 30));
    }

    #[test]
    fn selection_limits_the_open_area() {
        let mut open = Bitmap::filled(100, 10);
        let mut sel = TileStore::new();
        for x in 0..50 {
            for y in 0..10 {
                sel.get_or_create_mut(TileCoord::containing(x, y)).set_pixel(x as u32, y as u32, [255; 4]);
            }
        }
        restrict_to_selection(&mut open, &sel);
        assert!(open.get(49, 5) && !open.get(50, 5));
    }
}
