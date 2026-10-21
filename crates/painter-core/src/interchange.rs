//! What the layered interchange formats (OpenRaster, Krita) share: a layer's
//! pixels as tiles whatever its kind, placing decoded images into tiles, PNG
//! encoding, blend mode names, and XML escaping.

use crate::blend::{premultiply, unpremultiply, BlendMode};
use crate::layer::{Layer, LayerKind};
use crate::tile::{TileCoord, TileStore, TILE_SIZE};

/// A layer's own premultiplied pixels as tiles, with its mask applied
/// (alpha × (1 − mask)) when `bake_mask`: raster and vector layers as they
/// are, fill / gradient / tone rendered. `None` for folders and correction layers.
pub(crate) fn layer_pixels(layer: &Layer, canvas: (u32, u32), bake_mask: bool) -> Option<TileStore> {
    let mut tiles = match &layer.kind {
        LayerKind::Raster(t) => t.clone(),
        LayerKind::Vector(v) => v.cache.clone(),
        LayerKind::Fill { .. } | LayerKind::Gradient(_) | LayerKind::Tone(_) => {
            let rgba = crate::io::render_procedural(layer, canvas)?;
            TileStore::from_rgba(canvas.0, canvas.1, &rgba)
        }
        LayerKind::Folder(_) | LayerKind::Adjustment(_) => return None,
    };
    if let (true, Some(mask)) = (bake_mask, layer.active_mask()) {
        let coords: Vec<TileCoord> = tiles.iter().map(|(c, _)| c).collect();
        for coord in coords {
            let Some(m) = mask.get(coord).cloned() else { continue };
            let tile = tiles.get_or_create_mut(coord);
            for y in 0..TILE_SIZE {
                for x in 0..TILE_SIZE {
                    let keep = 255 - m.pixel(x, y)[3] as u32;
                    let p = tile.pixel(x, y);
                    tile.set_pixel(x, y, p.map(|v| ((v as u32 * keep + 127) / 255) as u8));
                }
            }
        }
    }
    Some(tiles)
}

/// The smallest rectangle (x, y, w, h) of the canvas holding all of `tiles`'
/// paint, or `None` when they're empty.
pub(crate) fn bounds(tiles: &TileStore, canvas: (u32, u32)) -> Option<(u32, u32, u32, u32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for (coord, tile) in tiles.iter() {
        let (ox, oy) = coord.origin();
        for y in 0..TILE_SIZE {
            for x in 0..TILE_SIZE {
                if tile.pixel(x, y)[3] != 0 {
                    let (px, py) = (ox as i64 + x as i64, oy as i64 + y as i64);
                    x0 = x0.min(px);
                    y0 = y0.min(py);
                    x1 = x1.max(px);
                    y1 = y1.max(py);
                }
            }
        }
    }
    let (x0, y0) = (x0.max(0), y0.max(0));
    let (x1, y1) = (x1.min(canvas.0 as i64 - 1), y1.min(canvas.1 as i64 - 1));
    (x0 <= x1 && y0 <= y1).then(|| (x0 as u32, y0 as u32, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32))
}

/// Unpremultiplied RGBA8 of a rectangle of `tiles`.
pub(crate) fn crop_rgba(tiles: &TileStore, x0: u32, y0: u32, w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let p = unpremultiply(tiles.pixel((x0 + x) as i32, (y0 + y) as i32));
            let i = ((y * w + x) * 4) as usize;
            out[i..i + 4].copy_from_slice(&p);
        }
    }
    out
}

/// Puts an unpremultiplied RGBA8 image at (`x0`, `y0`) into tiles (premultiplied).
pub(crate) fn blit_rgba(store: &mut TileStore, x0: i32, y0: i32, w: u32, h: u32, rgba: &[u8]) {
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let i = ((y as u32 * w + x as u32) * 4) as usize;
            let p = [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]];
            if p[3] == 0 {
                continue;
            }
            let (px, py) = (x0 + x, y0 + y);
            let coord = TileCoord::containing(px, py);
            let (ox, oy) = coord.origin();
            store.get_or_create_mut(coord).set_pixel((px - ox) as u32, (py - oy) as u32, premultiply(p));
        }
    }
}

pub(crate) fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    image::RgbaImage::from_raw(w, h, rgba.to_vec())
        .ok_or("image size mismatch")?
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// Unpremultiplied RGBA8 and size.
pub(crate) fn decode_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).map_err(|e| e.to_string())?.to_rgba8();
    Ok((img.width(), img.height(), img.into_raw()))
}

/// The composite (unpremultiplied RGBA8) and a thumbnail at most `max` pixels on a side.
pub(crate) fn merged_and_thumbnail(doc: &crate::document::Document, max: u32) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut flat = crate::io::flatten(doc);
    for px in flat.as_chunks_mut::<4>().0 {
        *px = unpremultiply(*px);
    }
    let merged = encode_png(doc.width, doc.height, &flat)?;
    let img = image::RgbaImage::from_raw(doc.width, doc.height, flat).ok_or("image size mismatch")?;
    let scale = (max as f32 / doc.width.max(doc.height) as f32).min(1.0);
    let (tw, th) = (((doc.width as f32 * scale).round() as u32).max(1), ((doc.height as f32 * scale).round() as u32).max(1));
    let thumb = image::imageops::resize(&img, tw, th, image::imageops::FilterType::Triangle);
    Ok((merged, encode_png(tw, th, thumb.as_raw())?))
}

/// OpenRaster's `composite-op` for a blend mode, and back.
pub(crate) fn ora_op(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Normal => "svg:src-over",
        BlendMode::Darken => "svg:darken",
        BlendMode::Multiply => "svg:multiply",
        BlendMode::ColorBurn => "svg:color-burn",
        BlendMode::LinearBurn => "krita:linear_burn",
        BlendMode::Lighten => "svg:lighten",
        BlendMode::Screen => "svg:screen",
        BlendMode::ColorDodge => "svg:color-dodge",
        BlendMode::Add => "svg:plus",
        BlendMode::Overlay => "svg:overlay",
        BlendMode::SoftLight => "svg:soft-light",
        BlendMode::HardLight => "svg:hard-light",
        BlendMode::Difference => "svg:difference",
    }
}

pub(crate) fn from_ora_op(op: &str) -> Option<BlendMode> {
    BlendMode::ALL.into_iter().find(|m| ora_op(*m) == op).or(match op {
        "svg:src-atop" => Some(BlendMode::Normal),
        _ => None,
    })
}

/// Krita's `compositeop` for a blend mode, and back.
pub(crate) fn krita_op(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Normal => "normal",
        BlendMode::Darken => "darken",
        BlendMode::Multiply => "multiply",
        BlendMode::ColorBurn => "burn",
        BlendMode::LinearBurn => "linear_burn",
        BlendMode::Lighten => "lighten",
        BlendMode::Screen => "screen",
        BlendMode::ColorDodge => "dodge",
        BlendMode::Add => "add",
        BlendMode::Overlay => "overlay",
        BlendMode::SoftLight => "soft_light_svg",
        BlendMode::HardLight => "hard_light",
        BlendMode::Difference => "diff",
    }
}

pub(crate) fn from_krita_op(op: &str) -> Option<BlendMode> {
    BlendMode::ALL.into_iter().find(|m| krita_op(*m) == op).or(match op {
        "soft_light" | "soft_light_pegtop_delphi" => Some(BlendMode::SoftLight),
        "linear_dodge" => Some(BlendMode::Add),
        _ => None,
    })
}

pub(crate) fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_names_round_trip() {
        for m in BlendMode::ALL {
            assert_eq!(from_ora_op(ora_op(m)), Some(m));
            assert_eq!(from_krita_op(krita_op(m)), Some(m));
        }
        assert_eq!(xml_escape("a<b & \"c\""), "a&lt;b &amp; &quot;c&quot;");
    }

    #[test]
    fn blit_crop_and_bounds() {
        let mut store = TileStore::new();
        let rgba: Vec<u8> = (0..3 * 2).flat_map(|i| [i as u8 * 10, 20, 30, if i == 0 { 0 } else { 255 }]).collect();
        blit_rgba(&mut store, 62, 63, 3, 2, &rgba);
        // The first pixel is transparent but the second row starts at x = 62.
        assert_eq!(bounds(&store, (200, 200)), Some((62, 63, 3, 2)));
        assert_eq!(store.pixel(62, 63)[3], 0);
        assert_eq!(crop_rgba(&store, 62, 63, 3, 2)[4..8], rgba[4..8]);
        assert_eq!(store.iter().count(), 4, "spans four tiles");
    }
}
