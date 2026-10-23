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
    composite_layers(&doc.layers, &mut buf, Area::full(doc.canvas()));
    buf
}

/// A procedural layer's own pixels (no mask, blend or opacity), for formats
/// that only store pixel layers. `None` for layers with tiles or folders.
pub fn render_procedural(layer: &Layer, canvas: (u32, u32)) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; (canvas.0 * canvas.1 * 4) as usize];
    let source = procedural_source(&layer.kind)?;
    let shape = Shape { mask: None, clip: None, opacity: 1.0, mode: BlendMode::Normal };
    blend_source(&mut buf, Area::full(canvas), &source, &shape);
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

/// The part of the canvas being composited: `w` × `h` pixels from (`x0`, `y0`),
/// inside a canvas of `canvas` pixels. The buffer holds just that part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Area {
    pub canvas: (u32, u32),
    pub x0: u32,
    pub y0: u32,
    pub w: u32,
    pub h: u32,
}

impl Area {
    pub fn full(canvas: (u32, u32)) -> Self {
        Self { canvas, x0: 0, y0: 0, w: canvas.0, h: canvas.1 }
    }

    /// Tiles touching the area.
    fn tiles(&self) -> impl Iterator<Item = TileCoord> {
        let (tx0, ty0) = ((self.x0 / TILE_SIZE) as i32, (self.y0 / TILE_SIZE) as i32);
        let tx1 = (self.x0 + self.w).div_ceil(TILE_SIZE) as i32;
        let ty1 = (self.y0 + self.h).div_ceil(TILE_SIZE) as i32;
        (ty0..ty1).flat_map(move |y| (tx0..tx1).map(move |x| TileCoord::new(x, y)))
    }

    fn touches(&self, coord: TileCoord) -> bool {
        let (ox, oy) = coord.origin();
        let s = TILE_SIZE as i64;
        let (ox, oy) = (ox as i64, oy as i64);
        ox + s > self.x0 as i64 && oy + s > self.y0 as i64 && ox < (self.x0 + self.w) as i64 && oy < (self.y0 + self.h) as i64
    }

    /// The buffer index of canvas pixel (`x`, `y`), if it is in the area and the canvas.
    fn index(&self, x: i32, y: i32) -> Option<usize> {
        let in_canvas = x >= 0 && y >= 0 && (x as u32) < self.canvas.0 && (y as u32) < self.canvas.1;
        let (x, y) = (x as i64 - self.x0 as i64, y as i64 - self.y0 as i64);
        (in_canvas && x >= 0 && y >= 0 && x < self.w as i64 && y < self.h as i64).then(|| (y as u32 * self.w + x as u32) as usize)
    }
}

pub(crate) fn composite_layers(layers: &[Layer], buf: &mut [u8], area: Area) {
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
            LayerKind::Folder(folder) if !layer.is_isolated_folder() => composite_layers(&folder.children, buf, area),
            LayerKind::Folder(folder) => {
                let mut group = vec![0u8; buf.len()];
                composite_layers(&folder.children, &mut group, area);
                blend_source(buf, area, &Source::Dense(&group), &shape);
            }
            LayerKind::Raster(tiles) => blend_source(buf, area, &Source::Tiles(tiles), &shape),
            LayerKind::Vector(vector) => blend_source(buf, area, &Source::Tiles(&vector.cache), &shape),
            LayerKind::Adjustment(adjustment) => apply_adjustment(buf, area, &adjustment.op(), &shape),
            kind => {
                let source = procedural_source(kind).expect("remaining kinds are procedural");
                blend_source(buf, area, &source, &shape);
            }
        }
    }
}

/// Blends `source × (1 − mask) × clip × opacity` into `buf`, tile by tile.
/// Mirrors `fs_main` in painter-render's `composite.wgsl`.
fn blend_source(buf: &mut [u8], area: Area, source: &Source<'_>, shape: &Shape<'_>) {
    let coords: Vec<TileCoord> = match source {
        Source::Tiles(tiles) => tiles.iter().map(|(c, _)| c).filter(|c| area.touches(*c)).collect(),
        _ => area.tiles().collect(),
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
            for tx in 0..TILE_SIZE {
                let x = ox + tx as i32;
                let Some(i) = area.index(x, y) else { continue };
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
fn apply_adjustment(buf: &mut [u8], area: Area, op: &crate::adjust::AdjustOp, shape: &Shape<'_>) {
    let pixels = buf.as_chunks_mut::<4>().0;
    for coord in area.tiles() {
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
            for lx in 0..TILE_SIZE {
                let Some(i) = area.index(ox + lx as i32, oy + ly as i32) else { continue };
                let px = &mut pixels[i];
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
                let out = [b[0] + (a[0] * b[3] - b[0]) * k, b[1] + (a[1] * b[3] - b[1]) * k, b[2] + (a[2] * b[3] - b[2]) * k, b[3]];
                *px = to_u8(out);
            }
        }
    }
}

/// The visible colour (premultiplied RGBA8) of the `w` × `h` pixels from
/// (`x0`, `y0`): the whole layer stack composited there only, cheap enough
/// for an eyedropper on a large page. Pixels outside the canvas are 0.
pub fn flatten_region(doc: &Document, x0: u32, y0: u32, w: u32, h: u32) -> Vec<u8> {
    let area = Area { canvas: doc.canvas(), x0, y0, w, h };
    let mut buf = vec![0u8; (w * h * 4) as usize];
    composite_layers(&doc.layers, &mut buf, area);
    buf
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
    // Grey and black-and-white images as grey PNGs with a grey profile; colour with its own.
    let gray = doc.mode != crate::color_mode::ColorMode::Rgb || doc.profile.is_gray();
    let profile = match (gray, doc.profile.is_gray()) {
        (true, false) => crate::color_profile::ColorProfile::GrayGamma22,
        (false, true) => crate::color_profile::ColorProfile::Srgb,
        _ => doc.profile,
    };
    let mut bytes = Vec::new();
    if gray {
        image::DynamicImage::ImageRgba8(image).to_luma_alpha8().write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)?;
    } else {
        image.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)?;
    }
    let tagged = png_with_icc(&png_with_dpi(&bytes, doc.dpi), profile.name(), &profile.icc());
    std::fs::write(path, tagged).map_err(ImageError::IoError)
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


/// A PNG with an `iCCP` chunk (the colour profile, zlib-compressed) right
/// after `IHDR`, before `IDAT` as the spec asks. Anything that isn't a PNG
/// comes back unchanged.
pub fn png_with_icc(png: &[u8], name: &str, icc: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let after_ihdr = 8 + 4 + 4 + 13 + 4;
    if png.len() < after_ihdr || &png[..8] != PNG_SIGNATURE || &png[12..16] != b"IHDR" {
        return png.to_vec();
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    if z.write_all(icc).is_err() {
        return png.to_vec();
    }
    let Ok(compressed) = z.finish() else { return png.to_vec() };
    // Keyword (1–79 Latin-1 characters), a NUL, compression method 0, the profile.
    let keyword: String = name.chars().filter(|c| c.is_ascii_graphic() || *c == ' ').take(79).collect();
    let mut typed = b"iCCP".to_vec();
    typed.extend_from_slice(keyword.as_bytes());
    typed.extend_from_slice(&[0, 0]);
    typed.extend_from_slice(&compressed);
    let mut out = Vec::with_capacity(png.len() + typed.len() + 8);
    out.extend_from_slice(&png[..after_ihdr]);
    out.extend_from_slice(&((typed.len() - 4) as u32).to_be_bytes());
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
    fn png_profiles_round_trip() {
        use crate::color_profile::ColorProfile;
        use image::ImageDecoder;
        for (mode, profile, gray) in [
            (crate::color_mode::ColorMode::Rgb, ColorProfile::DisplayP3, false),
            (crate::color_mode::ColorMode::Grayscale, ColorProfile::GrayGamma22, true),
        ] {
            let mut doc = Document::new(4, 4);
            doc.mode = mode;
            doc.profile = profile;
            let path = std::env::temp_dir().join(format!("painter-icc-{}-{gray}.png", std::process::id()));
            export_png(&doc, &path).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            std::fs::remove_file(&path).ok();
            let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(&bytes)).unwrap();
            assert_eq!(decoder.icc_profile().unwrap(), Some(profile.icc()), "{profile:?}");
            assert_eq!(decoder.color_type() == image::ColorType::La8, gray);
            assert_eq!(png_dpi(&bytes), Some(doc.dpi), "the resolution is still there");
        }
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
    fn region_matches_the_full_flatten() {
        // Paper, a gradient, pixels across tiles in an isolated half-opacity
        // folder, and an inversion over part of it.
        let (w, h) = (300u32, 200u32);
        let mut doc = Document::new(w, h);
        let gradient = crate::procedural::Gradient {
            start: [0.0, 0.0],
            end: [300.0, 200.0],
            from: [200, 20, 40, 255],
            to: [10, 90, 200, 120],
            shape: crate::procedural::GradientShape::Linear,
        };
        let id = doc.alloc_id();
        doc.add_layer(Layer::gradient(id, "Gradient", gradient));
        let mut folder = Layer::folder(doc.alloc_id(), "F");
        folder.opacity = 0.5;
        if let LayerKind::Folder(f) = &mut folder.kind {
            let mut ink = Layer::raster(LayerId(900), "Ink");
            let tiles = ink.tiles_mut().unwrap();
            for (x, y) in [(5u32, 5u32), (130, 70), (260, 190), (128, 64), (127, 63)] {
                let c = TileCoord::new((x / TILE_SIZE) as i32, (y / TILE_SIZE) as i32);
                tiles.get_or_create_mut(c).set_pixel(x % TILE_SIZE, y % TILE_SIZE, [30, 60, 90, 255]);
            }
            f.children.push(ink);
        }
        doc.layers.push(folder);
        let mut invert = Layer::adjustment(doc.alloc_id(), crate::adjust::Adjustment::Invert);
        invert.opacity = 0.7;
        doc.layers.push(invert);

        let full = flatten(&doc);
        for (x0, y0, rw, rh) in [(0, 0, 300, 200), (125, 60, 7, 6), (127, 63, 1, 1), (255, 185, 20, 20), (0, 199, 3, 1)] {
            let region = flatten_region(&doc, x0, y0, rw, rh);
            for y in 0..rh {
                for x in 0..rw {
                    let (cx, cy) = (x0 + x, y0 + y);
                    let got = &region[((y * rw + x) * 4) as usize..][..4];
                    if cx < w && cy < h {
                        let want = &full[((cy * w + cx) * 4) as usize..][..4];
                        assert_eq!(got, want, "pixel ({cx}, {cy}) of region at ({x0}, {y0})");
                    } else {
                        assert_eq!(got, [0; 4], "outside the canvas");
                    }
                }
            }
        }
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
