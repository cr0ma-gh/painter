//! Image import/export and CPU flattening.

use std::io::Cursor;
use std::path::Path;

use image::{ImageError, ImageFormat, RgbaImage};

use crate::blend::{composite, premultiply, to_f32, to_u8, unpremultiply, BlendMode, Rgba};
use crate::document::Document;
use crate::layer::{clip_targets, ClipTo, Layer, LayerKind};
use crate::tile::{TileCoord, TileStore, TILE_SIZE};

pub struct ImportedImage {
    pub width: u32,
    pub height: u32,
    pub tiles: TileStore,
    /// Resolution stored in the file, if any.
    pub dpi: Option<u32>,
}

/// Composites the visible layer tree into premultiplied RGBA8, matching the GPU compositor.
pub fn flatten(doc: &Document) -> Vec<u8> {
    let mut buf = vec![0u8; (doc.width * doc.height * 4) as usize];
    composite_layers(&doc.layers, &mut buf, doc.canvas());
    buf
}

/// A procedural layer's own pixels (no mask, blend or opacity), for formats
/// that only store pixel layers. `None` for layers with tiles or folders.
pub fn render_procedural(layer: &Layer, canvas: (u32, u32)) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; (canvas.0 * canvas.1 * 4) as usize];
    let source = procedural_source(&layer.kind)?;
    let shape = Shape { mask: None, clip: None, opacity: 1.0, mode: BlendMode::Normal };
    blend_source(&mut buf, canvas, &source, &shape);
    Some(buf)
}

/// Where a layer's premultiplied colour comes from.
enum Source<'a> {
    Tiles(&'a TileStore),
    Dense(&'a [u8]),
    Procedural(Box<dyn Fn(f32, f32) -> Rgba + 'a>),
}

/// Everything that shapes a source on its way into the backdrop.
struct Shape<'a> {
    mask: Option<&'a TileStore>,
    clip: Option<&'a TileStore>,
    opacity: f32,
    mode: BlendMode,
}

fn procedural_source(kind: &LayerKind) -> Option<Source<'_>> {
    match kind {
        LayerKind::Fill { color } => {
            let s = [color[0] as f32 / 255.0, color[1] as f32 / 255.0, color[2] as f32 / 255.0, 1.0];
            Some(Source::Procedural(Box::new(move |_, _| s)))
        }
        LayerKind::Gradient(g) => Some(Source::Procedural(Box::new(move |x, y| g.sample(x, y)))),
        LayerKind::Tone(t) => Some(Source::Procedural(Box::new(move |x, y| t.sample(x, y)))),
        _ => None,
    }
}

pub(crate) fn composite_layers(layers: &[Layer], buf: &mut [u8], canvas: (u32, u32)) {
    for (layer, clip) in layers.iter().zip(clip_targets(layers)) {
        if !layer.visible || layer.opacity <= 0.0 || clip == ClipTo::Skip {
            continue;
        }
        let clip = match clip {
            ClipTo::Layer(id) => layers.iter().find(|l| l.id == id).and_then(Layer::tiles),
            _ => None,
        };
        let shape = Shape { mask: layer.active_mask(), clip, opacity: layer.opacity, mode: layer.blend };
        match &layer.kind {
            LayerKind::Folder(folder) if !layer.is_isolated_folder() => composite_layers(&folder.children, buf, canvas),
            LayerKind::Folder(folder) => {
                let mut group = vec![0u8; buf.len()];
                composite_layers(&folder.children, &mut group, canvas);
                blend_source(buf, canvas, &Source::Dense(&group), &shape);
            }
            LayerKind::Raster(tiles) => blend_source(buf, canvas, &Source::Tiles(tiles), &shape),
            LayerKind::Vector(vector) => blend_source(buf, canvas, &Source::Tiles(&vector.cache), &shape),
            LayerKind::Adjustment(adjustment) => apply_adjustment(buf, canvas, &adjustment.op(), &shape),
            kind => {
                let source = procedural_source(kind).expect("remaining kinds are procedural");
                blend_source(buf, canvas, &source, &shape);
            }
        }
    }
}

/// Blends `source × (1 − mask) × clip × opacity` into `buf`, tile by tile.
/// Mirrors `fs_main` in painter-render's `composite.wgsl`.
fn blend_source(buf: &mut [u8], canvas: (u32, u32), source: &Source<'_>, shape: &Shape<'_>) {
    let (w, h) = canvas;
    let coords: Vec<TileCoord> = match source {
        Source::Tiles(tiles) => tiles.iter().map(|(c, _)| c).collect(),
        _ => (0..h.div_ceil(TILE_SIZE) as i32)
            .flat_map(|y| (0..w.div_ceil(TILE_SIZE) as i32).map(move |x| TileCoord::new(x, y)))
            .collect(),
    };
    let pixels = buf.as_chunks_mut::<4>().0;
    for coord in coords {
        let clip_tile = match shape.clip {
            Some(clip) => match clip.get(coord) {
                Some(t) => Some(t),
                None => continue, // nothing to clip to here
            },
            None => None,
        };
        let mask_tile = shape.mask.and_then(|m| m.get(coord));
        let source_tile = match source {
            Source::Tiles(tiles) => tiles.get(coord),
            _ => None,
        };
        let (ox, oy) = coord.origin();
        for ty in 0..TILE_SIZE {
            let y = oy + ty as i32;
            if y < 0 || y >= h as i32 {
                continue;
            }
            for tx in 0..TILE_SIZE {
                let x = ox + tx as i32;
                if x < 0 || x >= w as i32 {
                    continue;
                }
                let i = (y as u32 * w + x as u32) as usize;
                let mut s = match source {
                    Source::Tiles(_) => to_f32(source_tile.map_or([0; 4], |t| t.pixel(tx, ty))),
                    Source::Dense(group) => to_f32([group[i * 4], group[i * 4 + 1], group[i * 4 + 2], group[i * 4 + 3]]),
                    Source::Procedural(f) => f(x as f32 + 0.5, y as f32 + 0.5),
                };
                if s[3] <= 0.0 {
                    continue;
                }
                let mut k = shape.opacity;
                if let Some(m) = mask_tile {
                    k *= 1.0 - m.pixel(tx, ty)[3] as f32 / 255.0;
                }
                if let Some(c) = clip_tile {
                    k *= c.pixel(tx, ty)[3] as f32 / 255.0;
                }
                if k <= 0.0 {
                    continue;
                }
                s = s.map(|v| v * k);
                blend_px(&mut pixels[i], s, shape.mode);
            }
        }
    }
}

/// A correction layer: the backdrop's colour run through `op`, mixed in by
/// opacity × (1 − mask) × clip. Alpha is untouched. Mirrors the adjustment
/// branch of `fs_main` in `composite.wgsl`.
fn apply_adjustment(buf: &mut [u8], canvas: (u32, u32), op: &crate::adjust::AdjustOp, shape: &Shape<'_>) {
    let (w, h) = canvas;
    let pixels = buf.as_chunks_mut::<4>().0;
    for ty in 0..h.div_ceil(TILE_SIZE) as i32 {
        for tx in 0..w.div_ceil(TILE_SIZE) as i32 {
            let coord = TileCoord::new(tx, ty);
            let clip_tile = match shape.clip {
                Some(clip) => match clip.get(coord) {
                    Some(t) => Some(t),
                    None => continue,
                },
                None => None,
            };
            let mask_tile = shape.mask.and_then(|m| m.get(coord));
            let (ox, oy) = coord.origin();
            for ly in 0..TILE_SIZE {
                let y = oy as u32 + ly;
                if y >= h {
                    break;
                }
                for lx in 0..TILE_SIZE {
                    let x = ox as u32 + lx;
                    if x >= w {
                        break;
                    }
                    let px = &mut pixels[(y * w + x) as usize];
                    let b = to_f32(*px);
                    if b[3] <= 0.0 {
                        continue;
                    }
                    let mut k = shape.opacity;
                    if let Some(m) = mask_tile {
                        k *= 1.0 - m.pixel(lx, ly)[3] as f32 / 255.0;
                    }
                    if let Some(c) = clip_tile {
                        k *= c.pixel(lx, ly)[3] as f32 / 255.0;
                    }
                    if k <= 0.0 {
                        continue;
                    }
                    let a = op.apply_pixel(*px);
                    let out = [
                        b[0] + (a[0] * b[3] - b[0]) * k,
                        b[1] + (a[1] * b[3] - b[1]) * k,
                        b[2] + (a[2] * b[3] - b[2]) * k,
                        b[3],
                    ];
                    *px = to_u8(out);
                }
            }
        }
    }
}

fn blend_px(dst: &mut [u8; 4], s: Rgba, mode: BlendMode) {
    *dst = to_u8(composite(to_f32(*dst), s, mode));
}
/// Writes the flattened canvas as PNG, with its resolution (`pHYs`).
pub fn export_png(doc: &Document, path: &Path) -> Result<(), ImageError> {
    let mut pixels = flatten(doc);
    for px in pixels.as_chunks_mut::<4>().0 {
        *px = unpremultiply(*px);
    }
    let image = RgbaImage::from_raw(doc.width, doc.height, pixels).expect("buffer matches canvas size");
    let mut bytes = Vec::new();
    image.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)?;
    std::fs::write(path, png_with_dpi(&bytes, doc.dpi)).map_err(ImageError::IoError)
}

/// Reads a PNG (or any format `image` knows); `dpi` comes from a PNG's `pHYs`.
pub fn import_png(path: &Path) -> Result<ImportedImage, ImageError> {
    let bytes = std::fs::read(path).map_err(ImageError::IoError)?;
    let image = image::load_from_memory(&bytes)?.to_rgba8();
    let (width, height) = image.dimensions();
    let mut raw = image.into_raw();
    for px in raw.as_chunks_mut::<4>().0 {
        *px = premultiply(*px);
    }
    Ok(ImportedImage { width, height, tiles: TileStore::from_rgba(width, height, &raw), dpi: png_dpi(&bytes) })
}

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const METERS_PER_INCH: f64 = 0.0254;

/// A PNG with a `pHYs` chunk (pixels per metre) right after `IHDR`, as the
/// spec asks (before `IDAT`). Anything that isn't a PNG comes back unchanged.
pub fn png_with_dpi(png: &[u8], dpi: u32) -> Vec<u8> {
    // Signature, then IHDR: length, type, 13 bytes of data, CRC.
    let after_ihdr = 8 + 4 + 4 + 13 + 4;
    if png.len() < after_ihdr || &png[..8] != PNG_SIGNATURE || &png[12..16] != b"IHDR" || png_dpi(png).is_some() {
        return png.to_vec();
    }
    let ppm = (dpi as f64 / METERS_PER_INCH).round() as u32;
    let mut data = Vec::with_capacity(9);
    data.extend_from_slice(&ppm.to_be_bytes());
    data.extend_from_slice(&ppm.to_be_bytes());
    data.push(1); // unit: metre
    let mut out = Vec::with_capacity(png.len() + 21);
    out.extend_from_slice(&png[..after_ihdr]);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let typed: Vec<u8> = b"pHYs".iter().chain(&data).copied().collect();
    out.extend_from_slice(&typed);
    out.extend_from_slice(&crc32(&typed).to_be_bytes());
    out.extend_from_slice(&png[after_ihdr..]);
    out
}

/// A PNG's resolution from its `pHYs` chunk, when it's given in metres.
pub fn png_dpi(png: &[u8]) -> Option<u32> {
    if png.get(..8)? != PNG_SIGNATURE {
        return None;
    }
    let mut at = 8;
    while let Some(header) = png.get(at..at + 8) {
        let len = u32::from_be_bytes(header[..4].try_into().ok()?) as usize;
        let data = png.get(at + 8..at + 8 + len)?;
        match &header[4..8] {
            b"pHYs" if len == 9 && data[8] == 1 => {
                let ppm = u32::from_be_bytes(data[..4].try_into().ok()?);
                let dpi = (ppm as f64 * METERS_PER_INCH).round() as u32;
                return (dpi > 0).then_some(dpi);
            }
            b"IDAT" | b"IEND" => return None,
            _ => at += 8 + len + 4,
        }
    }
    None
}

/// CRC-32 as PNG uses it (ISO-HDLC, reflected 0xEDB88320).
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::{Layer, LayerId};
    use crate::tile::TileCoord;

    #[test]
    fn png_resolution_round_trips() {
        // A known value: the CRC of an empty IEND chunk.
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        let mut doc = Document::new(3, 2);
        doc.dpi = 600;
        let path = std::env::temp_dir().join(format!("painter-dpi-{}.png", std::process::id()));
        export_png(&doc, &path).unwrap();
        let back = import_png(&path).unwrap();
        // The image crate still reads it, so the inserted chunk is well formed.
        assert_eq!((back.width, back.height, back.dpi), (3, 2, Some(600)));
        std::fs::remove_file(&path).ok();
        let plain = png_with_dpi(b"not a png", 300);
        assert_eq!(plain, b"not a png");
        assert_eq!(png_dpi(&plain), None);
    }

    #[test]
    fn flatten_applies_blend_modes() {

        let mut doc = Document::new(4, 4);
        let layer = doc.active_layer_mut().unwrap();
        layer.blend = BlendMode::Multiply;
        layer.tiles_mut().unwrap().get_or_create_mut(TileCoord::new(0, 0)).set_pixel(1, 1, [255, 0, 0, 255]);
        let px = flatten(&doc);
        let at = |x: usize, y: usize| &px[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
        assert_eq!(at(1, 1), [255, 0, 0, 255]);
        assert_eq!(at(0, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn isolated_folder_opacity() {
        let mut doc = Document::new(2, 1);
        let folder_id = doc.alloc_id();
        let mut folder = Layer::folder(folder_id, "F");
        folder.opacity = 0.5;
        if let LayerKind::Folder(f) = &mut folder.kind {
            f.pass_through = false;
            let mut black = Layer::raster(LayerId(99), "Black");
            black.tiles_mut().unwrap().get_or_create_mut(TileCoord::new(0, 0)).set_pixel(0, 0, [0, 0, 0, 255]);
            f.children.push(black);
        }
        doc.layers.push(folder);
        let px = flatten(&doc);
        assert_eq!(&px[0..4], &[128, 128, 128, 255]);
    }

    fn red_pixels(doc: &mut Document, id: LayerId, xs: std::ops::Range<u32>) {
        let tiles = doc.find_mut(id).unwrap().tiles_mut().unwrap();
        for x in xs {
            tiles.get_or_create_mut(TileCoord::new(0, 0)).set_pixel(x, 0, [255, 0, 0, 255]);
        }
    }

    #[test]
    fn masks_hide_and_clipping_follows_base_alpha() {
        let mut doc = Document::new(4, 1);
        let base = doc.active;
        red_pixels(&mut doc, base, 0..2);
        // Mask hides pixel 1 of the base.
        let mut mask = crate::layer::Mask { enabled: true, ..Default::default() };
        mask.tiles.get_or_create_mut(TileCoord::new(0, 0)).set_pixel(1, 0, [255; 4]);
        doc.find_mut(base).unwrap().mask = Some(mask);

        // Blue layer clipped to the base, covering every pixel.
        let clipped = doc.alloc_id();
        let mut layer = Layer::raster(clipped, "Clipped");
        layer.clip = true;
        for x in 0..4 {
            layer.tiles_mut().unwrap().get_or_create_mut(TileCoord::new(0, 0)).set_pixel(x, 0, [0, 0, 255, 255]);
        }
        doc.add_layer(layer);

        let px = flatten(&doc);
        assert_eq!(&px[0..4], &[0, 0, 255, 255], "clipped over visible base");
        // Clipping uses the base's pixel alpha, not its mask.
        assert_eq!(&px[4..8], &[0, 0, 255, 255]);
        assert_eq!(&px[8..12], &[255, 255, 255, 255], "no base pixels: clipped layer hidden");
    }

    #[test]
    fn procedural_layers_and_masked_pass_through_folder() {
        let mut doc = Document::new(10, 1);
        let gradient = crate::procedural::Gradient {
            start: [0.0, 0.0],
            end: [10.0, 0.0],
            from: [0, 0, 0, 255],
            to: [0, 0, 0, 0],
            shape: crate::procedural::GradientShape::Linear,
        };
        let id = doc.alloc_id();
        doc.add_layer(Layer::gradient(id, "Gradient", gradient));
        let px = flatten(&doc);
        assert!(px[0] < 20 && px[9 * 4] > 235, "black fading to paper");

        // A mask on a pass-through folder forces isolation and hides its content.
        let folder_id = doc.alloc_id();
        let mut folder = Layer::folder(folder_id, "F");
        let mut mask = crate::layer::Mask { enabled: true, ..Default::default() };
        for x in 0..10 {
            mask.tiles.get_or_create_mut(TileCoord::new(0, 0)).set_pixel(x, 0, [255; 4]);
        }
        folder.mask = Some(mask);
        if let LayerKind::Folder(f) = &mut folder.kind {
            let mut black = Layer::raster(LayerId(500), "Black");
            black.tiles_mut().unwrap().get_or_create_mut(TileCoord::new(0, 0)).set_pixel(9, 0, [0, 0, 0, 255]);
            f.children.push(black);
        }
        doc.layers.push(folder);
        assert_eq!(flatten(&doc)[9 * 4..9 * 4 + 4], px[9 * 4..9 * 4 + 4]);
    }
}
