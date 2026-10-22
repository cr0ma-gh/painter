//! Krita documents (.kra): a ZIP with a stored `mimetype`
//! (`application/x-krita`), `maindoc.xml` describing the image and its layer
//! tree (first child = topmost), and each paint layer's pixels at
//! `<image name>/layers/<filename>` in Krita's tiled format:
//!
//! ```text
//! VERSION 2
//! TILEWIDTH 64
//! TILEHEIGHT 64
//! PIXELSIZE 4
//! DATA <tile count>
//! <x>,<y>,LZF,<byte count>\n<byte count bytes>   (per tile)
//! ```
//!
//! A tile's bytes start with a flag: 1 = LZF-compressed planar channels
//! (all of the first channel, then the second…), 0 = the raw interleaved
//! pixels. 8-bit RGBA pixels are B, G, R, A, not premultiplied. Plus
//! `mergedimage.png` (the composite) and `preview.png`.
//!
//! Read: paint and group layers (pass-through, opacity, blend, visibility,
//! lock, offsets), RGBA and grey layers at 8 or 16 bits, transparency masks.
//! Other layer kinds are listed as warnings and the composite is added as a
//! hidden layer so nothing is lost. Written: paint and group layers, with
//! masks and clipping baked into the pixels (Krita has no clipping like
//! Clip Studio's) and correction layers left out.

use crate::blend::BlendMode;
use crate::document::Document;
use crate::interchange::{self as ix, xml_escape};
use crate::layer::{clip_targets, ClipTo, Folder, Layer, LayerId, LayerKind, Mask};
use crate::ora::{clip_to_canvas, flat_pixels};
use crate::tile::{TileCoord, TileStore, TILE_SIZE};
use crate::zipfile::{ZipReader, ZipWriter};

pub const MIMETYPE: &str = "application/x-krita";
/// The colour profile Painter's documents are in, as Krita names its sRGB profile.
const PROFILE: &str = "sRGB-elle-V2-srgbtrc.icc";
const T: usize = TILE_SIZE as usize;

/// LZF decompression (liblzf's format, which Krita uses).
pub fn lzf_decompress(input: &[u8], out_len: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(out_len);
    let mut i = 0;
    while i < input.len() {
        let ctrl = input[i] as usize;
        i += 1;
        if ctrl < 32 {
            let n = ctrl + 1;
            out.extend_from_slice(input.get(i..i + n)?);
            i += n;
        } else {
            let mut len = ctrl >> 5;
            if len == 7 {
                len += *input.get(i)? as usize;
                i += 1;
            }
            let back = ((ctrl & 0x1f) << 8) + *input.get(i)? as usize + 1;
            i += 1;
            let start = out.len().checked_sub(back)?;
            for k in 0..len + 2 {
                let b = out[start + k];
                out.push(b);
            }
        }
        if out.len() > out_len {
            return None;
        }
    }
    (out.len() == out_len).then_some(out)
}

/// LZF compression (greedy, with a hash of the next three bytes). `None`
/// when it wouldn't be smaller.
pub fn lzf_compress(input: &[u8]) -> Option<Vec<u8>> {
    const HLOG: usize = 14;
    let mut table = vec![usize::MAX; 1 << HLOG];
    let hash = |i: usize| -> usize {
        let v = (input[i] as u32) << 16 | (input[i + 1] as u32) << 8 | input[i + 2] as u32;
        ((v.wrapping_mul(2654435761)) >> (32 - HLOG)) as usize
    };
    let mut out = Vec::with_capacity(input.len());
    let mut lit: Vec<u8> = Vec::with_capacity(32);
    let flush = |out: &mut Vec<u8>, lit: &mut Vec<u8>| {
        if !lit.is_empty() {
            out.push(lit.len() as u8 - 1);
            out.append(lit);
        }
    };
    let mut i = 0;
    while i < input.len() {
        let mut matched = 0;
        let mut back = 0;
        if i + 2 < input.len() {
            let h = hash(i);
            let r = table[h];
            table[h] = i;
            if r != usize::MAX && i - r <= 8192 && input[r..r + 3] == input[i..i + 3] {
                back = i - r;
                let max = (input.len() - i).min(264);
                matched = 3;
                while matched < max && input[r + matched] == input[i + matched] {
                    matched += 1;
                }
            }
        }
        if matched >= 3 {
            flush(&mut out, &mut lit);
            let (len, off) = (matched - 2, back - 1);
            if len < 7 {
                out.push(((len << 5) | (off >> 8)) as u8);
            } else {
                out.push(((7 << 5) | (off >> 8)) as u8);
                out.push((len - 7) as u8);
            }
            out.push(off as u8);
            for k in i + 1..(i + matched).min(input.len().saturating_sub(2)) {
                table[hash(k)] = k;
            }
            i += matched;
        } else {
            lit.push(input[i]);
            if lit.len() == 32 {
                flush(&mut out, &mut lit);
            }
            i += 1;
        }
        if out.len() >= input.len() {
            return None;
        }
    }
    flush(&mut out, &mut lit);
    (out.len() < input.len()).then_some(out)
}

/// Interleaved pixels → one plane per channel (Krita's "linearized" order), and back.
fn linearize(data: &[u8], pixel: usize) -> Vec<u8> {
    let n = data.len() / pixel;
    let mut out = vec![0; data.len()];
    for p in 0..n {
        for c in 0..pixel {
            out[c * n + p] = data[p * pixel + c];
        }
    }
    out
}

fn delinearize(data: &[u8], pixel: usize) -> Vec<u8> {
    let n = data.len() / pixel;
    let mut out = vec![0; data.len()];
    for p in 0..n {
        for c in 0..pixel {
            out[p * pixel + c] = data[c * n + p];
        }
    }
    out
}

/// Krita's colour spaces we read: how to turn one pixel into straight RGBA8.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Space {
    Rgba8,
    Rgba16,
    Gray8,
    Gray16,
    /// Masks and selections: one byte, 255 = selected.
    Alpha8,
}

impl Space {
    fn of(name: &str) -> Option<Space> {
        Some(match name {
            "RGBA" => Space::Rgba8,
            "RGBA16" => Space::Rgba16,
            "GRAYA" => Space::Gray8,
            "GRAYA16" => Space::Gray16,
            "ALPHA" | "Alpha" | "GRAYU8" => Space::Alpha8,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            Space::Rgba8 => 4,
            Space::Rgba16 => 8,
            Space::Gray8 => 2,
            Space::Gray16 => 4,
            Space::Alpha8 => 1,
        }
    }

    fn rgba(self, p: &[u8]) -> [u8; 4] {
        let w = |i: usize| (u16::from_le_bytes([p[i], p[i + 1]]) >> 8) as u8;
        match self {
            Space::Rgba8 => [p[2], p[1], p[0], p[3]],
            Space::Rgba16 => [w(4), w(2), w(0), w(6)],
            Space::Gray8 => [p[0], p[0], p[0], p[1]],
            Space::Gray16 => [w(0), w(0), w(0), w(2)],
            Space::Alpha8 => [p[0]; 4],
        }
    }
}

/// Decodes a tiled layer file into (x, y, straight RGBA8 64 × 64) tiles.
fn read_tiles(data: &[u8], space: Space) -> Result<Vec<(i32, i32, Vec<u8>)>, String> {
    fn next_line<'a>(data: &'a [u8], at: &mut usize) -> Option<&'a str> {
        let end = data.get(*at..)?.iter().position(|b| *b == b'\n')? + *at;
        let s = std::str::from_utf8(&data[*at..end]).ok();
        *at = end + 1;
        s
    }
    let mut at = 0;
    let mut pixel = space.size();
    let mut count = None;
    let (mut tw, mut th) = (T, T);
    while count.is_none() {
        let l = next_line(data, &mut at).ok_or("truncated layer header")?.trim().to_string();
        let mut parts = l.split_whitespace();
        let (key, value) = (parts.next().unwrap_or(""), parts.next().and_then(|v| v.parse::<usize>().ok()));
        match (key, value) {
            ("VERSION", Some(v)) if v != 2 => return Err(format!("layer data version {v} isn't supported")),
            ("TILEWIDTH", Some(v)) => tw = v,
            ("TILEHEIGHT", Some(v)) => th = v,
            ("PIXELSIZE", Some(v)) => pixel = v,
            ("DATA", Some(v)) => count = Some(v),
            _ => {}
        }
    }
    if pixel != space.size() || tw != T || th != T {
        return Err(format!("unexpected layer data ({pixel} bytes a pixel, {tw}×{th} tiles)"));
    }
    let bytes = T * T * pixel;
    let mut tiles = Vec::new();
    for _ in 0..count.unwrap_or(0) {
        let l = next_line(data, &mut at).ok_or("truncated layer data")?.to_string();
        let f: Vec<&str> = l.trim().split(',').collect();
        let [x, y, codec, size] = f[..] else { return Err(format!("bad tile header {l:?}")) };
        let (x, y, size): (i32, i32, usize) =
            (x.parse().map_err(|_| "bad tile x")?, y.parse().map_err(|_| "bad tile y")?, size.parse().map_err(|_| "bad tile size")?);
        if codec != "LZF" {
            return Err(format!("tile compression {codec} isn't supported"));
        }
        let body = data.get(at..at + size).ok_or("truncated tile")?;
        at += size;
        let raw = match body.first() {
            Some(1) => delinearize(&lzf_decompress(&body[1..], bytes).ok_or("damaged tile")?, pixel),
            Some(0) => body.get(1..1 + bytes).ok_or("short tile")?.to_vec(),
            _ => return Err("damaged tile".into()),
        };
        let rgba = raw.chunks_exact(pixel).flat_map(|p| space.rgba(p)).collect();
        tiles.push((x, y, rgba));
    }
    Ok(tiles)
}

/// Encodes straight BGRA8 64 × 64 tiles, LZF-compressed when that's smaller.
fn write_tiles(tiles: &[(i32, i32, Vec<u8>)]) -> Vec<u8> {
    let mut out = format!("VERSION 2\nTILEWIDTH {T}\nTILEHEIGHT {T}\nPIXELSIZE 4\nDATA {}\n", tiles.len()).into_bytes();
    for (x, y, bgra) in tiles {
        let body = match lzf_compress(&linearize(bgra, 4)) {
            Some(c) => [&[1u8][..], &c].concat(),
            None => [&[0u8][..], bgra].concat(),
        };
        out.extend(format!("{x},{y},LZF,{}\n", body.len()).as_bytes());
        out.extend(body);
    }
    out
}

/// A layer's tiles as straight BGRA8, empty ones left out.
fn bgra_tiles(tiles: &TileStore) -> Vec<(i32, i32, Vec<u8>)> {
    let mut out: Vec<(i32, i32, Vec<u8>)> = tiles
        .iter()
        .filter(|(_, t)| (0..TILE_SIZE).any(|y| (0..TILE_SIZE).any(|x| t.pixel(x, y)[3] != 0)))
        .map(|(c, t)| {
            let (ox, oy) = c.origin();
            let mut bgra = Vec::with_capacity(T * T * 4);
            for y in 0..TILE_SIZE {
                for x in 0..TILE_SIZE {
                    let [r, g, b, a] = crate::blend::unpremultiply(t.pixel(x, y));
                    bgra.extend([b, g, r, a]);
                }
            }
            (ox, oy, bgra)
        })
        .collect();
    out.sort_by_key(|(x, y, _)| (*y, *x));
    out
}

fn uuid(seed: u64) -> String {
    let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
    let mut next = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let (a, b) = (next(), next());
    format!("{{{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}}}", a >> 32, (a >> 16) & 0xffff, a & 0xfff, b >> 52, b & 0xffff_ffff_ffff)
}

struct Writer<'a> {
    doc: &'a Document,
    dir: String,
    zip: ZipWriter,
    xml: String,
    count: usize,
    warnings: Vec<String>,
}

impl Writer<'_> {
    fn list(&mut self, layers: &[Layer], depth: usize) {
        let pad = " ".repeat(depth);
        self.xml += &format!("{pad}<layers>\n");
        let clips = clip_targets(layers);
        for (i, layer) in layers.iter().enumerate().rev() {
            let base = match clips[i] {
                ClipTo::Layer(id) => layers.iter().find(|l| l.id == id),
                _ => None,
            };
            self.layer(layer, base, matches!(clips[i], ClipTo::Skip), depth + 1);
        }
        self.xml += &format!("{pad}</layers>\n");
    }

    fn layer(&mut self, layer: &Layer, clip_base: Option<&Layer>, hidden: bool, depth: usize) {
        let pad = " ".repeat(depth);
        self.count += 1;
        let filename = format!("layer{}", self.count);
        let common = format!(
            r#"name="{}" filename="{filename}" x="0" y="0" opacity="{}" visible="{}" locked="{}" colorspacename="RGBA" uuid="{}" collapsed="0" intimeline="0" colorlabel="0"{}"#,
            xml_escape(&layer.name),
            (layer.opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
            (layer.visible && !hidden) as u8,
            layer.locked as u8,
            uuid(layer.id.0),
            if layer.id == self.doc.active { r#" selected="true""# } else { "" },
        );
        match &layer.kind {
            LayerKind::Folder(f) => {
                if layer.active_mask().is_some() {
                    self.warnings.push(format!("“{}”: folder masks were dropped", layer.name));
                }
                let op = if f.pass_through { "normal" } else { ix::krita_op(layer.blend) };
                self.xml += &format!(
                    "{pad}<layer nodetype=\"grouplayer\" {common} compositeop=\"{op}\" passthrough=\"{}\">\n",
                    f.pass_through as u8
                );
                self.list(&f.children, depth + 1);
                self.xml += &format!("{pad}</layer>\n");
            }
            LayerKind::Adjustment(_) => {
                self.count -= 1;
                self.warnings.push(format!("“{}”: correction layers aren't carried over to Krita and were left out", layer.name));
            }
            _ => {
                let tiles = flat_pixels(layer, clip_base, self.doc.canvas()).unwrap_or_default();
                if layer.clip && clip_base.is_some() {
                    self.warnings.push(format!("“{}”: clipping was baked into the pixels", layer.name));
                }
                let path = format!("{}/layers/{filename}", self.dir);
                self.zip.add(&path, &write_tiles(&bgra_tiles(&tiles)), true);
                self.zip.add(&format!("{path}.defaultpixel"), &[0; 4], false);
                // Krita reads each paint layer's colour profile from beside it.
                self.zip.add(&format!("{path}.icc"), &ix::srgb_icc(PROFILE), true);
                self.xml += &format!(
                    "{pad}<layer nodetype=\"paintlayer\" {common} compositeop=\"{}\" channelflags=\"\" channellockflags=\"{}\"/>\n",
                    ix::krita_op(layer.blend),
                    if layer.alpha_locked { "1110" } else { "" },
                );
            }
        }
    }
}

/// The document as a Krita file, and what couldn't be kept as is.
pub fn write(doc: &Document, title: &str) -> Result<(Vec<u8>, Vec<String>), String> {
    let mut zip = ZipWriter::new();
    zip.add("mimetype", MIMETYPE.as_bytes(), false);
    let dir: String = title.chars().filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_')).collect();
    let dir = if dir.trim().is_empty() { "Unnamed".to_string() } else { dir.trim().to_string() };
    let mut w = Writer { doc, dir: dir.clone(), zip, xml: String::new(), count: 0, warnings: Vec::new() };
    w.list(&doc.layers, 2);
    let Writer { mut zip, xml: layers, mut warnings, .. } = w;
    let main = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE DOC PUBLIC '-//KDE//DTD krita 2.0//EN' 'http://www.calligra.org/DTD/krita-2.0.dtd'>\n\
<DOC xmlns=\"http://www.calligra.org/DTD/krita\" syntaxVersion=\"2.0\" kritaVersion=\"5.2.0\" editor=\"Painter\">\n\
 <IMAGE name=\"{}\" mime=\"application/x-kra\" width=\"{}\" height=\"{}\" colorspacename=\"RGBA\" profile=\"{PROFILE}\" x-res=\"{}\" y-res=\"{}\" description=\"\">\n\
{layers} </IMAGE>\n</DOC>\n",
        xml_escape(&dir),
        doc.width,
        doc.height,
        doc.dpi,
        doc.dpi
    );
    zip.add("maindoc.xml", main.as_bytes(), true);
    // The image's own profile, as Krita writes it.
    zip.add(&format!("{dir}/annotations/icc"), &ix::srgb_icc(PROFILE), true);
    let info = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE document-info PUBLIC '-//KDE//DTD document-info 1.1//EN' 'http://www.calligra.org/DTD/document-info-1.1.dtd'>\n\
<document-info xmlns=\"http://www.calligra.org/DTD/document-info\">\n <about>\n  <title>{}</title>\n  <editing-cycles>1</editing-cycles>\n </about>\n <author/>\n</document-info>\n",
        xml_escape(title)
    );
    zip.add("documentinfo.xml", info.as_bytes(), true);
    let (merged, preview) = ix::merged_and_thumbnail(doc, 256)?;
    zip.add("mergedimage.png", &merged, false);
    zip.add("preview.png", &preview, false);
    warnings.dedup();
    Ok((zip.finish(), warnings))
}

pub fn save(doc: &Document, path: &std::path::Path) -> Result<Vec<String>, String> {
    let title = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let (bytes, warnings) = write(doc, &title)?;
    std::fs::write(path, bytes).map_err(|e| e.to_string())?;
    Ok(warnings)
}

struct Reader<'a> {
    zip: &'a ZipReader,
    dir: String,
    canvas: (u32, u32),
    next: u64,
    active: Option<LayerId>,
    warnings: Vec<String>,
    lost: bool,
}

fn num<T: std::str::FromStr>(n: roxmltree::Node, name: &str) -> Option<T> {
    n.attribute(name).and_then(|v| v.trim().parse().ok())
}

impl Reader<'_> {
    fn id(&mut self) -> LayerId {
        self.next += 1;
        LayerId(self.next)
    }

    fn data(&self, filename: &str) -> Option<Vec<u8>> {
        let path = format!("{}/layers/{filename}", self.dir);
        self.zip.read(&path).or_else(|| {
            // The directory is the image's name; be lenient about it.
            let suffix = format!("/layers/{filename}");
            let name = self.zip.names().find(|n| n.ends_with(&suffix))?.to_string();
            self.zip.read(&name)
        })
    }

    /// A tiled file at a layer's offset, as straight-RGBA tiles in the canvas.
    fn pixels(&mut self, n: roxmltree::Node, file: &str, space: Space) -> Result<TileStore, String> {
        let data = self.data(file).ok_or_else(|| format!("missing {file}"))?;
        let (ox, oy) = (num::<i32>(n, "x").unwrap_or(0), num::<i32>(n, "y").unwrap_or(0));
        let tiles = read_tiles(&data, space)?;
        let mut store = TileStore::new();
        // Outside its tiles a layer has its `.defaultpixel` (usually transparent).
        let default = self.data(&format!("{file}.defaultpixel")).filter(|d| d.len() == space.size()).map(|d| space.rgba(&d));
        if let Some(fill) = default.filter(|p| p[3] != 0) {
            let (w, h) = (self.canvas.0 as i32, self.canvas.1 as i32);
            let mut canvas = fill.repeat((w * h) as usize);
            for (x, y, rgba) in &tiles {
                for (i, p) in rgba.as_chunks::<4>().0.iter().enumerate() {
                    let (px, py) = (x + ox + (i % T) as i32, y + oy + (i / T) as i32);
                    if px >= 0 && py >= 0 && px < w && py < h {
                        let at = ((py * w + px) * 4) as usize;
                        canvas[at..at + 4].copy_from_slice(p);
                    }
                }
            }
            ix::blit_rgba(&mut store, 0, 0, w as u32, h as u32, &canvas);
            return Ok(store);
        }
        for (x, y, rgba) in tiles {
            ix::blit_rgba(&mut store, x + ox, y + oy, TILE_SIZE, TILE_SIZE, &rgba);
        }
        clip_to_canvas(&mut store, self.canvas);
        Ok(store)
    }

    /// A transparency mask's selection (255 = shown) as our mask tiles (alpha
    /// = amount hidden). Where the file has no tile its default pixel applies:
    /// the `.defaultpixel` file, else transparent (hidden), as Krita reads it.
    fn mask(&self, n: roxmltree::Node, file: &str) -> Result<TileStore, String> {
        let data = self.data(file).ok_or_else(|| format!("missing {file}"))?;
        let default_shown = self.data(&format!("{file}.defaultpixel")).and_then(|d| d.first().copied()).unwrap_or(0);
        let (ox, oy) = (num::<i32>(n, "x").unwrap_or(0), num::<i32>(n, "y").unwrap_or(0));
        let (w, h) = (self.canvas.0 as i32, self.canvas.1 as i32);
        let mut hide = TileStore::new();
        let set = |hide: &mut TileStore, px: i32, py: i32, v: u8| {
            if v != 0 && px >= 0 && py >= 0 && px < w && py < h {
                let c = TileCoord::containing(px, py);
                let (tx, ty) = c.origin();
                hide.get_or_create_mut(c).set_pixel((px - tx) as u32, (py - ty) as u32, [v; 4]);
            }
        };
        let tiles = read_tiles(&data, Space::Alpha8)?;
        if default_shown != 255 {
            // Everything starts (partly) hidden; the file's tiles overwrite it below.
            for py in 0..h {
                for px in 0..w {
                    set(&mut hide, px, py, 255 - default_shown);
                }
            }
        }
        for (x, y, rgba) in tiles {
            for (i, p) in rgba.as_chunks::<4>().0.iter().enumerate() {
                let (px, py) = (x + ox + (i % T) as i32, y + oy + (i / T) as i32);
                if px >= 0 && py >= 0 && px < w && py < h {
                    let c = TileCoord::containing(px, py);
                    let (tx, ty) = c.origin();
                    hide.get_or_create_mut(c).set_pixel((px - tx) as u32, (py - ty) as u32, [255 - p[3]; 4]);
                }
            }
        }
        Ok(hide)
    }

    /// The `<layers>` child's layers, bottom to top.
    fn layers(&mut self, parent: roxmltree::Node) -> Vec<Layer> {
        let Some(list) = parent.children().find(|c| c.has_tag_name("layers")) else { return Vec::new() };
        let mut out: Vec<Layer> = list.children().filter(|c| c.has_tag_name("layer")).filter_map(|c| self.node(c)).collect();
        out.reverse();
        out
    }

    fn node(&mut self, n: roxmltree::Node) -> Option<Layer> {
        let name = n.attribute("name").unwrap_or("Layer").to_string();
        let kind = n.attribute("nodetype").unwrap_or("");
        let op = n.attribute("compositeop").unwrap_or("normal");
        let blend = ix::from_krita_op(op).unwrap_or_else(|| {
            self.warnings.push(format!("“{name}”: blend mode {op} isn't supported, using Normal"));
            BlendMode::Normal
        });
        let opacity = num::<f32>(n, "opacity").unwrap_or(255.0).clamp(0.0, 255.0) / 255.0;
        let id = self.id();
        let mut layer = match kind {
            "grouplayer" => {
                let children = self.layers(n);
                let mut folder = Layer::folder(id, name.clone());
                folder.kind = LayerKind::Folder(Folder { children, pass_through: n.attribute("passthrough") == Some("1") });
                folder
            }
            "paintlayer" => {
                let space = n.attribute("colorspacename").unwrap_or("RGBA");
                let tiles = match Space::of(space).filter(|s| *s != Space::Alpha8) {
                    Some(space) => {
                        let file = n.attribute("filename").unwrap_or("");
                        self.pixels(n, file, space).unwrap_or_else(|e| {
                            self.warnings.push(format!("“{name}”: {e}"));
                            self.lost = true;
                            TileStore::new()
                        })
                    }
                    None => {
                        self.warnings.push(format!("“{name}”: colour space {space} isn't supported"));
                        self.lost = true;
                        TileStore::new()
                    }
                };
                let mut l = Layer::raster_with(id, name.clone(), tiles);
                // "Inherit alpha" (alpha left out of channelflags): the nearest Painter
                // equivalent is clipping to the layer below.
                if n.attribute("channelflags").is_some_and(|f| f.len() == 4 && f.ends_with('0')) {
                    l.clip = true;
                    self.warnings.push(format!("“{name}”: Krita's inherit alpha became clipping to the layer below"));
                }
                l.alpha_locked = n.attribute("channellockflags").is_some_and(|f| f.ends_with('0') && f.len() == 4);
                l
            }
            other => {
                let what = match other {
                    "adjustmentlayer" => "filter layers",
                    "generatorlayer" => "fill layers",
                    "shapelayer" => "vector layers",
                    "filelayer" => "file layers",
                    "clonelayer" => "clone layers",
                    _ => "this kind of layer",
                };
                self.warnings.push(format!("“{name}”: {what} aren't supported; see the hidden “Krita composite” layer"));
                self.lost = true;
                return None;
            }
        };
        layer.blend = blend;
        layer.opacity = opacity;
        layer.visible = n.attribute("visible") != Some("0");
        layer.locked = n.attribute("locked") == Some("1");
        if n.attribute("selected") == Some("true") && !layer.is_folder() {
            self.active = Some(layer.id);
        }
        self.masks(n, &mut layer);
        Some(layer)
    }

    /// The first transparency mask, as our mask (amount hidden = 255 − selected).
    fn masks(&mut self, n: roxmltree::Node, layer: &mut Layer) {
        let Some(masks) = n.children().find(|c| c.has_tag_name("masks")) else { return };
        let mut used = false;
        for m in masks.children().filter(|c| c.has_tag_name("mask")) {
            let kind = m.attribute("nodetype").unwrap_or("");
            if kind != "transparencymask" || used {
                if kind != "selectionmask" {
                    self.warnings.push(format!("“{}”: a {kind} was left out", layer.name));
                }
                continue;
            }
            let file = format!("{}.pixelselection", m.attribute("filename").unwrap_or(""));
            match self.mask(m, &file) {
                Ok(hide) => {
                    layer.mask = Some(Mask { tiles: hide, enabled: m.attribute("visible") != Some("0") });
                    used = true;
                }
                Err(e) => self.warnings.push(format!("“{}”: mask left out ({e})", layer.name)),
            }
        }
    }
}

/// The first paint layer (top-down), for when nothing is marked selected.
fn first_raster(layers: &[Layer]) -> Option<LayerId> {
    layers.iter().rev().find_map(|l| match &l.kind {
        LayerKind::Folder(f) => first_raster(&f.children),
        LayerKind::Raster(_) => Some(l.id),
        _ => None,
    })
}

/// Reads a Krita file; also returns what couldn't be read as is.
pub fn read(data: Vec<u8>) -> Result<(Document, Vec<String>), String> {
    let zip = ZipReader::new(data)?;
    let xml = zip.read("maindoc.xml").ok_or("not a Krita file (no maindoc.xml)")?;
    let xml = String::from_utf8(xml).map_err(|_| "maindoc.xml isn't UTF-8")?;
    let opts = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let tree = roxmltree::Document::parse_with_options(&xml, opts).map_err(|e| format!("maindoc.xml: {e}"))?;
    let image = tree.descendants().find(|n| n.has_tag_name("IMAGE")).ok_or("maindoc.xml: no IMAGE")?;
    let (w, h) = (num::<u32>(image, "width").ok_or("no width")?, num::<u32>(image, "height").ok_or("no height")?);
    let mut warnings = Vec::new();
    if let Some(space) = image.attribute("colorspacename").filter(|s| Space::of(s).is_none()) {
        warnings.push(format!("the image is in {space}; layers in other colour spaces can't be read"));
    }
    let mut r = Reader {
        zip: &zip,
        dir: image.attribute("name").unwrap_or("Unnamed").to_string(),
        canvas: (w, h),
        next: 0,
        active: None,
        warnings,
        lost: false,
    };
    let mut layers = r.layers(image);
    if r.lost {
        if let Some(Ok((mw, mh, rgba))) = zip.read("mergedimage.png").map(|b| ix::decode_png(&b)) {
            let mut tiles = TileStore::new();
            ix::blit_rgba(&mut tiles, 0, 0, mw, mh, &rgba);
            clip_to_canvas(&mut tiles, (w, h));
            let mut l = Layer::raster_with(r.id(), "Krita composite", tiles);
            l.visible = false;
            layers.push(l);
        }
    }
    let active = r.active.or_else(|| first_raster(&layers)).unwrap_or(LayerId(0));
    let mut doc = Document::from_parts(w, h, layers, active, r.next + 1);
    if doc.layers.is_empty() {
        let id = doc.alloc_id();
        doc.layers.push(Layer::raster(id, "Layer 1"));
        doc.active = id;
    }
    if let Some(dpi) = num::<f32>(image, "x-res").filter(|d| *d >= 1.0) {
        doc.dpi = dpi.round() as u32;
    }
    let mut warnings = r.warnings;
    warnings.dedup();
    Ok((doc, warnings))
}

pub fn load(path: &std::path::Path) -> Result<(Document, Vec<String>), String> {
    read(std::fs::read(path).map_err(|e| e.to_string())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::flatten;

    #[test]
    fn lzf_round_trips() {
        let mut data: Vec<u8> = (0..20_000u32).map(|i| ((i / 37) % 5) as u8).collect();
        data.extend((0..3000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8));
        let c = lzf_compress(&data).unwrap();
        assert!(c.len() < data.len() / 2);
        assert_eq!(lzf_decompress(&c, data.len()).unwrap(), data);
        // A hand-made stream: literal "abc", then copy 5 from 3 back.
        assert_eq!(lzf_decompress(&[2, b'a', b'b', b'c', 3 << 5, 2], 8).unwrap(), b"abcabcab");
        assert!(lzf_decompress(&[2, b'a'], 3).is_none());
        let noise: Vec<u8> = (0..500u32).map(|i| (i.wrapping_mul(2654435761) >> 7) as u8).collect();
        assert!(lzf_compress(&noise).is_none());
    }

    #[test]
    fn tiles_round_trip_both_ways() {
        let mut bgra = vec![0u8; T * T * 4];
        for (i, p) in bgra.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            *p = [(i % 256) as u8, 10, 200, if i % 3 == 0 { 255 } else { 0 }];
        }
        let noise: Vec<u8> = (0..T * T * 4).map(|i| ((i as u32).wrapping_mul(2654435761) >> 11) as u8).collect();
        let file = write_tiles(&[(0, 0, bgra.clone()), (64, -64, noise.clone())]);
        let tiles = read_tiles(&file, Space::Rgba8).unwrap();
        assert_eq!(tiles.len(), 2);
        assert_eq!((tiles[1].0, tiles[1].1), (64, -64));
        let swap = |b: &[u8]| -> Vec<u8> { b.as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0], p[3]]).collect() };
        assert_eq!(tiles[0].2, swap(&bgra));
        assert_eq!(tiles[1].2, swap(&noise), "stored raw");
    }

    #[test]
    fn round_trips_a_document() {
        let doc = crate::ora::tests::sample();
        let (bytes, warnings) = write(&doc, "Test <1>").unwrap();
        assert_eq!(&bytes[38..38 + MIMETYPE.len()], MIMETYPE.as_bytes());
        assert!(warnings.iter().any(|w| w.contains("clipping")));
        let zip = ZipReader::new(bytes.clone()).unwrap();
        assert!(zip.names().any(|n| n == "Test 1/layers/layer2"), "{:?}", zip.names().collect::<Vec<_>>());
        let (back, warnings) = read(bytes).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!((back.width, back.height, back.dpi), (160, 100, 144));
        let names: Vec<String> =
            back.rows_top_down().iter().map(|(id, d)| format!("{}{}", "-".repeat(*d), back.find(*id).unwrap().name)).collect();
        assert_eq!(names, ["Folder", "-Blue", "Shade", "Red & <ink>", "Paper"]);
        assert!(back.find(back.rows_top_down()[0].0).unwrap().is_isolated_folder());
        assert_eq!(back.find(back.active).unwrap().name, "Red & <ink>");
        let (a, b) = (flatten(&back), flatten(&doc));
        assert!(a.iter().zip(&b).all(|(x, y)| x.abs_diff(*y) <= 3), "same picture");
    }

    #[test]
    fn reads_masks_and_keeps_unsupported_layers_as_the_composite() {
        // A hand-written Krita file: a filter layer over a half-masked paint layer.
        let mut zip = ZipWriter::new();
        zip.add("mimetype", MIMETYPE.as_bytes(), false);
        let main = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE DOC PUBLIC '-//KDE//DTD krita 2.0//EN' 'http://www.calligra.org/DTD/krita-2.0.dtd'>
<DOC xmlns="http://www.calligra.org/DTD/krita" syntaxVersion="2.0">
 <IMAGE name="img" width="64" height="64" colorspacename="RGBA" x-res="72">
  <layers>
   <layer nodetype="adjustmentlayer" name="Levels" filename="layer3"/>
   <layer nodetype="paintlayer" name="Ink" filename="layer2" x="10" y="0" opacity="128" visible="1" compositeop="multiply" colorspacename="RGBA">
    <masks><mask nodetype="transparencymask" name="m" filename="mask1" x="0" y="0" visible="1"/></masks>
   </layer>
  </layers>
 </IMAGE>
</DOC>"#;
        zip.add("maindoc.xml", main.as_bytes(), true);
        zip.add("img/layers/layer2", &write_tiles(&[(0, 0, [10u8, 20, 30, 255].repeat(T * T))]), true);
        let mut sel = format!("VERSION 2\nTILEWIDTH 64\nTILEHEIGHT 64\nPIXELSIZE 1\nDATA 1\n0,0,LZF,{}\n", T * T + 1).into_bytes();
        sel.push(0);
        sel.extend((0..T * T).map(|i| if i % T < 32 { 255 } else { 0 }));
        zip.add("img/layers/mask1.pixelselection", &sel, true);
        zip.add("mergedimage.png", &ix::encode_png(1, 1, &[1, 2, 3, 255]).unwrap(), false);
        let (doc, warnings) = read(zip.finish()).unwrap();
        assert!(warnings.iter().any(|w| w.contains("Levels")), "{warnings:?}");
        let names: Vec<&str> = doc.rows_top_down().iter().map(|(id, _)| doc.find(*id).unwrap().name.as_str()).collect();
        assert_eq!(names, ["Krita composite", "Ink"]);
        let ink = doc.find(doc.rows_top_down()[1].0).unwrap();
        assert_eq!((ink.blend, (ink.opacity * 255.0).round()), (BlendMode::Multiply, 128.0));
        let t = ink.tiles().unwrap();
        assert_eq!(t.pixel(9, 5)[3], 0, "offset by 10");
        assert_eq!(crate::blend::unpremultiply(t.pixel(10, 5)), [30, 20, 10, 255]);
        let mask = ink.active_mask().unwrap();
        assert_eq!(mask.pixel(5, 5)[3], 0, "shown");
        assert_eq!(mask.pixel(40, 5)[3], 255, "hidden");
        assert_eq!(doc.dpi, 72);
    }

    #[test]
    fn default_pixels_and_inherit_alpha() {
        let mut zip = ZipWriter::new();
        zip.add("mimetype", MIMETYPE.as_bytes(), false);
        let main = r#"<?xml version="1.0" encoding="UTF-8"?>
<DOC xmlns="http://www.calligra.org/DTD/krita" syntaxVersion="2.0">
 <IMAGE name="img" width="128" height="64" colorspacename="RGBA">
  <layers>
   <layer nodetype="paintlayer" name="Shade" filename="layer3" colorspacename="RGBA" channelflags="1110"/>
   <layer nodetype="paintlayer" name="Red" filename="layer2" colorspacename="RGBA">
    <masks><mask nodetype="transparencymask" name="m" filename="mask1" visible="1"/></masks>
   </layer>
  </layers>
 </IMAGE>
</DOC>"#;
        zip.add("maindoc.xml", main.as_bytes(), true);
        // Red everywhere by default; the right tile is cleared.
        zip.add("img/layers/layer2", &write_tiles(&[(64, 0, vec![0; T * T * 4])]), true);
        zip.add("img/layers/layer2.defaultpixel", &[0, 0, 255, 255], false);
        zip.add("img/layers/layer3", &write_tiles(&[]), true);
        // A mask with no tiles and no default pixel hides everything.
        zip.add("img/layers/mask1.pixelselection", b"VERSION 2\nTILEWIDTH 64\nTILEHEIGHT 64\nPIXELSIZE 1\nDATA 0\n", true);
        let (doc, warnings) = read(zip.finish()).unwrap();
        assert!(warnings.iter().any(|w| w.contains("inherit alpha")), "{warnings:?}");
        let rows = doc.rows_top_down();
        let shade = doc.find(rows[0].0).unwrap();
        assert!(shade.clip);
        let red = doc.find(rows[1].0).unwrap();
        let t = red.tiles().unwrap();
        assert_eq!(crate::blend::unpremultiply(t.pixel(10, 10)), [255, 0, 0, 255]);
        assert_eq!(t.pixel(100, 10)[3], 0);
        assert_eq!(red.active_mask().unwrap().pixel(10, 10)[3], 255, "hidden");
    }
}
