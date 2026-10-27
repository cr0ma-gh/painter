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

    /// The run of canvas row `y`, within the tile whose left column is `ox`, that
    /// is in the area and the canvas: tile-local columns `tx0..tx1` and the buffer
    /// index of `tx0`. `None` when the row misses.
    fn row_span(&self, ox: i32, y: i32) -> Option<(u32, u32, usize)> {
        if y < 0 || y as u32 >= self.canvas.1 || (y as u32) < self.y0 || y as u32 >= self.y0 + self.h {
            return None;
        }
        let lo = (ox as i64).max(self.x0 as i64).max(0);
        let hi = (ox as i64 + TILE_SIZE as i64).min((self.x0 + self.w).min(self.canvas.0) as i64);
        if lo >= hi {
            return None;
        }
        let i = ((y as u32 - self.y0) * self.w) as usize + (lo - self.x0 as i64) as usize;
        Some(((lo - ox as i64) as u32, (hi - ox as i64) as u32, i))
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
            // Layer property effects show in exports, as on the canvas.
            LayerKind::Raster(_) | LayerKind::Vector(_) if layer.effects.is_active() => {
                let shown = crate::effects::render(layer.tiles().expect("pixel layer"), &layer.effects, area.canvas);
                blend_source(buf, area, &Source::Tiles(&shown), &shape);
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
///
/// Normal blending of byte sources at full strength (the bulk of most
/// artwork) takes an exact integer path; see [`blend_normal_row`].
fn blend_source(buf: &mut [u8], area: Area, source: &Source<'_>, shape: &Shape<'_>) {
    let coords: Vec<TileCoord> = match source {
        Source::Tiles(tiles) => tiles.iter().map(|(c, _)| c).filter(|c| area.touches(*c)).collect(),
        _ => area.tiles().collect(),
    };
    let integer = shape.mode == BlendMode::Normal && !matches!(source, Source::Procedural(_));
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
        let unshaped = shape.opacity == 1.0 && mask_tile.is_none() && clip_tile.is_none();
        let (ox, oy) = coord.origin();
        for ty in 0..TILE_SIZE {
            let y = oy + ty as i32;
            let Some((tx0, tx1, i0)) = area.row_span(ox, y) else { continue };
            let len = (tx1 - tx0) as usize;
            if integer && unshaped {
                let src = match source {
                    Source::Tiles(_) => match source_tile {
                        Some(t) => &t.bytes().as_chunks::<4>().0[(ty * TILE_SIZE + tx0) as usize..][..len],
                        None => continue,
                    },
                    Source::Dense(group) => &group.as_chunks::<4>().0[i0..i0 + len],
                    Source::Procedural(_) => unreachable!("procedural sources blend in float"),
                };
                blend_normal_row(&mut pixels[i0..i0 + len], src);
                continue;
            }
            for (i, tx) in (i0..).zip(tx0..tx1) {
                let x = ox + tx as i32;
                let s8 = match source {
                    Source::Tiles(_) => source_tile.map_or([0; 4], |t| t.pixel(tx, ty)),
                    Source::Dense(group) => [group[i * 4], group[i * 4 + 1], group[i * 4 + 2], group[i * 4 + 3]],
                    Source::Procedural(_) => [0; 4],
                };
                let mut s = match source {
                    Source::Procedural(f) => f(x as f32 + 0.5, y as f32 + 0.5),
                    _ => to_f32(s8),
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
                if integer && k == 1.0 {
                    blend_normal_px(&mut pixels[i], s8);
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

/// `round(x / 255)` for `x <= 255 * 255`, without a division.
#[inline(always)]
fn div255(x: u16) -> u16 {
    let x = x + 128;
    (x + (x >> 8)) >> 8
}

/// Normal-mode "source over" of premultiplied `s` onto `d` in integers:
/// `round((s·255 + d·(255 − sa)) / 255)` per channel. For a valid premultiplied
/// source (`rgb <= a`) this is exactly what [`blend_px`] produces, as the true
/// result never lies within float error of a rounding boundary; the exhaustive
/// `normal_integer_path_matches_float` test checks that.
#[inline(always)]
fn normal_over(d: &mut [u8; 4], s: [u8; 4]) {
    let inv = 255 - s[3] as u16;
    let c = |i: usize| div255(s[i] as u16 * 255 + d[i] as u16 * inv) as u8;
    let a = c(3);
    *d = [c(0).min(a), c(1).min(a), c(2).min(a), a];
}

fn is_premultiplied(s: [u8; 4]) -> bool {
    s[0] <= s[3] && s[1] <= s[3] && s[2] <= s[3]
}

/// One pixel of [`blend_normal_row`]: transparent sources leave `dst` alone,
/// opaque ones replace it, the rest blend in integers (or in float for a
/// source breaking the premultiplied invariant, to keep its clamping).
#[inline(always)]
fn blend_normal_px(dst: &mut [u8; 4], s: [u8; 4]) {
    match s[3] {
        0 => {}
        255 => *dst = s,
        _ if is_premultiplied(s) => normal_over(dst, s),
        _ => blend_px(dst, to_f32(s), BlendMode::Normal),
    }
}

/// Normal blending of a full-strength row of byte pixels, identical to
/// [`blend_px`] per pixel. Fully transparent rows are skipped, fully opaque
/// ones copied, and valid premultiplied rows run a branch-free 16-bit integer
/// loop that auto-vectorises (SSE2, NEON).
fn blend_normal_row(dst: &mut [[u8; 4]], src: &[[u8; 4]]) {
    let (mut lo, mut hi, mut valid) = (255u8, 0u8, true);
    for &s in src {
        lo = lo.min(s[3]);
        hi = hi.max(s[3]);
        valid &= is_premultiplied(s);
    }
    if hi == 0 {
        return;
    }
    if lo == 255 {
        dst.copy_from_slice(src);
    } else if valid {
        // Alpha 0 (so colour 0) leaves `d` as is and alpha 255 replaces it, so
        // no branches are needed; `dst` is premultiplied, being our own output.
        for (d, &s) in dst.iter_mut().zip(src) {
            normal_over(d, s);
        }
    } else {
        for (d, &s) in dst.iter_mut().zip(src) {
            blend_normal_px(d, s);
        }
    }
}
/// Writes the flattened canvas as PNG, with its resolution (`pHYs`).
pub fn export_png(doc: &Document, path: &Path) -> Result<(), ImageError> {
    let mut pixels = flatten(doc);
    for px in pixels.as_chunks_mut::<4>().0 {
        *px = unpremultiply(*px);
    }
    let image = RgbaImage::from_raw(doc.width, doc.height, pixels).expect("buffer matches canvas size");
    // Grey and black-and-white images as grey PNGs with a grey profile; colour with its own.
    let gray = matches!(doc.mode, crate::color_mode::ColorMode::Grayscale | crate::color_mode::ColorMode::Bitmap) || doc.profile.is_gray();
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

    #[test]
    fn normal_integer_path_matches_float() {
        // Every channel value against every source alpha and backdrop value,
        // on opaque and on matching-alpha backdrops.
        for sa in 0..=255u8 {
            for sc in 0..=sa {
                let s = [sc, sa - sc, sa / 2, sa];
                for b in 0..=255u8 {
                    for d in [[b, b / 3, 255 - b, 255], [b, b / 2, 0, b]] {
                        let (mut fast, mut float) = (d, d);
                        blend_normal_px(&mut fast, s);
                        blend_px(&mut float, to_f32(s), BlendMode::Normal);
                        assert_eq!(fast, float, "{s:?} over {d:?}");
                        normal_over(&mut fast, s);
                        normal_over(&mut float, s);
                        assert_eq!(fast, float);
                    }
                }
            }
        }
    }

    #[test]
    fn normal_rows_match_float() {
        let mut seed = 0x2545_f491_u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let mut pixel = |valid: bool| {
            let r = next().to_le_bytes();
            let a = match r[3] % 4 {
                0 => 0,
                1 => 255,
                _ => r[3],
            };
            if valid {
                premultiply([r[0], r[1], r[2], a])
            } else {
                r
            }
        };
        for round in 0..400 {
            let valid = round % 4 != 0;
            let src: Vec<[u8; 4]> = (0..64).map(|_| pixel(valid)).collect();
            let dst: Vec<[u8; 4]> = (0..64).map(|_| pixel(true)).collect();
            let mut fast = dst.clone();
            blend_normal_row(&mut fast, &src);
            let mut float = dst;
            for (d, &s) in float.iter_mut().zip(&src) {
                if s[3] > 0 {
                    blend_px(d, to_f32(s), BlendMode::Normal);
                }
            }
            assert_eq!(fast, float, "round {round}");
        }
        // Opaque and transparent rows take the copy and skip shortcuts.
        let mut row = vec![[9, 8, 7, 200]; 5];
        blend_normal_row(&mut row, &[[0; 4]; 5]);
        assert_eq!(row, vec![[9, 8, 7, 200]; 5]);
        blend_normal_row(&mut row, &[[1, 2, 3, 255]; 5]);
        assert_eq!(row, vec![[1, 2, 3, 255]; 5]);
    }

    #[test]
    fn row_spans_cover_the_area() {
        let area = Area { canvas: (100, 90), x0: 30, y0: 10, w: 80, h: 20 };
        for coord in area.tiles() {
            let (ox, oy) = coord.origin();
            for ty in 0..TILE_SIZE as i32 {
                let span = area.row_span(ox, oy + ty);
                let index = |tx: u32| area.index(ox + tx as i32, oy + ty).map(|i| (tx, i));
                let expect: Vec<(u32, usize)> = (0..TILE_SIZE).filter_map(index).collect();
                let got: Vec<(u32, usize)> = span.map_or(vec![], |(tx0, tx1, i0)| (tx0..tx1).zip(i0..).collect());
                assert_eq!(got, expect, "tile {coord:?} row {ty}");
            }
        }
    }
}
