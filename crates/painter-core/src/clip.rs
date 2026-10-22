//! Clip Studio Paint documents (`.clip`): reading, and writing into a
//! document saved by Clip Studio used as a template.
//!
//! Worked out from the files themselves (and the public notes of open-source
//! converters), for interoperability; Clip Studio itself was not examined.
//!
//! A `.clip` is a chunk file: `CSFCHUNK`, the file size and the first chunk's
//! offset (big-endian u64s), then chunks `CHNK<type>` + u64 size: `Head`
//! (the database chunk's offset), `Exta` (one per pixel buffer: an id
//! `extrnlid…` and its blocks), `SQLi` (an SQLite database) and `Foot`.
//!
//! The database describes the canvas (`Canvas`: size, resolution, root
//! folder) and the layers (`Layer`: name, type, opacity 0–256, blend,
//! visibility, lock, clipping, offsets; each folder's first child, then each
//! layer's next sibling, bottom to top). A layer's pixels: `Layer.LayerRender
//! Mipmap` → `Mipmap.BaseMipmapInfo` → `MipmapInfo` (100 %) → `Offscreen`
//! (its size in an attribute blob, and the `Exta` id). An `Exta` holds
//! 256 × 256 blocks, each a "BlockDataBeginChunk" record (UTF-16 name, block
//! index, raw size, block size, whether it has data, then a zlib stream);
//! colour blocks are the alpha plane then 4 bytes a pixel (B, G, R, x), masks
//! one byte a pixel.
//!
//! Every layer kind has its picture in its render buffer, so vector, text,
//! fill and 3D layers come in as their pixels; the paper becomes a fill.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use crate::blend::{premultiply, BlendMode};
use crate::document::Document;
use crate::layer::{Folder, Layer, LayerId, LayerKind, Mask};
use crate::tile::{TileCoord, TileStore};

pub const EXTENSION: &str = "clip";
const BLOCK: u32 = 256;

fn be32(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(i..i + 4)?.try_into().ok()?))
}

fn be64(d: &[u8], i: usize) -> Option<u64> {
    Some(u64::from_be_bytes(d.get(i..i + 8)?.try_into().ok()?))
}

fn utf16be(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|c| c.to_be_bytes()).collect()
}

/// A chunk: its type and where its body is.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Chunk {
    pub(crate) kind: [u8; 4],
    /// Offset of the `CHNK` tag.
    pub(crate) at: usize,
    pub(crate) body: std::ops::Range<usize>,
}

/// The chunks of a `.clip` file.
pub(crate) fn chunks(data: &[u8]) -> Result<Vec<Chunk>, String> {
    if !data.starts_with(b"CSFCHUNK") {
        return Err("not a Clip Studio file".into());
    }
    let mut at = be64(data, 16).ok_or("truncated file")? as usize;
    let mut out = Vec::new();
    while at + 16 <= data.len() {
        if &data[at..at + 4] != b"CHNK" {
            return Err(format!("damaged chunk at {at}"));
        }
        let kind: [u8; 4] = data[at + 4..at + 8].try_into().expect("4 bytes");
        let size = be64(data, at + 8).ok_or("truncated chunk")? as usize;
        let body = at + 16..(at + 16).checked_add(size).filter(|e| *e <= data.len()).ok_or("truncated chunk")?;
        out.push(Chunk { kind, at, body: body.clone() });
        if &kind == b"Foot" {
            break;
        }
        at = body.end;
    }
    Ok(out)
}

/// An `Exta` chunk's id and its block data.
fn exta(body: &[u8]) -> Option<(String, &[u8])> {
    let idlen = be64(body, 0)? as usize;
    let id = String::from_utf8_lossy(body.get(8..8 + idlen)?).into_owned();
    let size = be64(body, 8 + idlen)? as usize;
    Some((id, body.get(16 + idlen..16 + idlen + size)?))
}

/// The blocks with data in a pixel buffer: (index, decompressed bytes).
pub(crate) fn read_blocks(data: &[u8]) -> Vec<(u32, Vec<u8>)> {
    let marker = utf16be("BlockDataBeginChunk");
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(p) = data.get(i..).and_then(|rest| rest.windows(marker.len()).position(|w| w == marker.as_slice())) {
        let at = i + p + marker.len();
        i = at + 20;
        let (Some(index), Some(has)) = (be32(data, at), be32(data, at + 16)) else { break };
        if has == 0 {
            continue;
        }
        let (Some(len), Some(zlen)) =
            (be32(data, at + 20), data.get(at + 24..at + 28).map(|b| u32::from_le_bytes(b.try_into().expect("4 bytes"))))
        else {
            break;
        };
        let Some(z) = data.get(at + 28..at + 28 + zlen as usize) else { break };
        let mut raw = Vec::new();
        if flate2::read::ZlibDecoder::new(z).read_to_end(&mut raw).is_ok() {
            out.push((index, raw));
        }
        i = at + 24 + len as usize;
    }
    out
}

/// An offscreen's size from its attribute blob: (width, height, blocks across, down).
pub(crate) fn offscreen_size(attribute: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let marker = utf16be("Parameter");
    let p = attribute.windows(marker.len()).position(|w| w == marker.as_slice())? + marker.len();
    Some((be32(attribute, p)?, be32(attribute, p + 4)?, be32(attribute, p + 8)?, be32(attribute, p + 12)?))
}

/// Clip Studio's blend mode numbers.
pub(crate) fn blend_of(n: i64) -> Option<BlendMode> {
    Some(match n {
        0 => BlendMode::Normal,
        1 => BlendMode::Darken,
        2 => BlendMode::Multiply,
        3 => BlendMode::ColorBurn,
        4 => BlendMode::LinearBurn,
        7 => BlendMode::Lighten,
        8 => BlendMode::Screen,
        9 => BlendMode::ColorDodge,
        11 => BlendMode::Add,
        14 => BlendMode::Overlay,
        15 => BlendMode::SoftLight,
        16 => BlendMode::HardLight,
        21 => BlendMode::Difference,
        _ => return None,
    })
}

pub(crate) fn blend_number(mode: BlendMode) -> i64 {
    (0..32).find(|n| blend_of(*n) == Some(mode)).unwrap_or(0)
}

/// One `Layer` row, the columns used.
#[derive(Debug, Clone, Default)]
struct Row {
    main: i64,
    name: String,
    kind: i64,
    folder: bool,
    opacity: i64,
    blend: i64,
    visible: bool,
    locked: bool,
    clip: bool,
    draft: bool,
    first_child: i64,
    next: i64,
    offset: (i64, i64),
    render_offset: (i64, i64),
    mask_offset: (i64, i64),
    render_mipmap: i64,
    mask_mipmap: i64,
    selected: bool,
    paper: Option<[u8; 3]>,
    /// A vector layer (its strokes listed in `VectorObjectList`).
    vector: bool,
}

struct Reader<'a> {
    db: rusqlite::Connection,
    buffers: HashMap<String, &'a [u8]>,
    canvas: (u32, u32),
    warnings: Vec<String>,
    next_id: u64,
    active: Option<LayerId>,
    /// Some layer's picture couldn't be read: add Clip Studio's preview.
    lost: bool,
}

fn int(row: &rusqlite::Row, name: &str) -> i64 {
    row.get::<_, Option<i64>>(name).ok().flatten().unwrap_or(0)
}

impl Reader<'_> {
    fn id(&mut self) -> LayerId {
        self.next_id += 1;
        LayerId(self.next_id)
    }

    fn rows(&self) -> Result<HashMap<i64, Row>, String> {
        let has = |col: &str| -> bool { self.db.prepare(&format!("SELECT {col} FROM Layer LIMIT 0")).is_ok() };
        let draft = if has("DraftLayer") { "DraftLayer" } else { "0" };
        let vector = if has("VectorNormalStrokeIndex") { "VectorNormalStrokeIndex" } else { "0" };
        let sql = format!(
            "SELECT MainId, LayerName, LayerType, LayerFolder, LayerOpacity, LayerComposite, LayerVisibility, LayerLock, LayerClip, \
             {draft} AS Draft, {vector} AS Vector, LayerFirstChildIndex, LayerNextIndex, LayerOffsetX, LayerOffsetY, LayerRenderOffscrOffsetX, \
             LayerRenderOffscrOffsetY, LayerMaskOffscrOffsetX, LayerMaskOffscrOffsetY, LayerMaskOffsetX, LayerMaskOffsetY, \
             LayerRenderMipmap, LayerLayerMaskMipmap, LayerSelect, DrawColorEnable, DrawColorMainRed, DrawColorMainGreen, \
             DrawColorMainBlue, SpecialRenderType FROM Layer"
        );
        let mut stmt = self.db.prepare(&sql).map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        let mut out = HashMap::new();
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let channel = |c: &str| (int(r, c) as u64 >> 24) as u8;
            let paper = (int(r, "LayerType") == 1584 || int(r, "SpecialRenderType") == 20) && int(r, "DrawColorEnable") != 0;
            let row = Row {
                main: int(r, "MainId"),
                name: r.get::<_, Option<String>>("LayerName").ok().flatten().unwrap_or_default(),
                kind: int(r, "LayerType"),
                folder: int(r, "LayerFolder") & 1 != 0,
                opacity: int(r, "LayerOpacity"),
                blend: int(r, "LayerComposite"),
                visible: int(r, "LayerVisibility") & 1 != 0,
                locked: int(r, "LayerLock") & 1 != 0,
                clip: int(r, "LayerClip") != 0,
                draft: int(r, "Draft") != 0,
                first_child: int(r, "LayerFirstChildIndex"),
                next: int(r, "LayerNextIndex"),
                offset: (int(r, "LayerOffsetX"), int(r, "LayerOffsetY")),
                render_offset: (int(r, "LayerRenderOffscrOffsetX"), int(r, "LayerRenderOffscrOffsetY")),
                mask_offset: (
                    int(r, "LayerMaskOffsetX") + int(r, "LayerMaskOffscrOffsetX"),
                    int(r, "LayerMaskOffsetY") + int(r, "LayerMaskOffscrOffsetY"),
                ),
                render_mipmap: int(r, "LayerRenderMipmap"),
                mask_mipmap: int(r, "LayerLayerMaskMipmap"),
                selected: int(r, "LayerSelect") & 1 != 0,
                paper: paper.then(|| [channel("DrawColorMainRed"), channel("DrawColorMainGreen"), channel("DrawColorMainBlue")]),
                vector: int(r, "Vector") != 0,
            };
            out.insert(row.main, row);
        }
        Ok(out)
    }

    /// A mipmap's full-size offscreen: its attribute and pixel buffer id.
    fn offscreen(&self, mipmap: i64) -> Option<(Vec<u8>, String)> {
        let info: i64 = self.db.query_row("SELECT BaseMipmapInfo FROM Mipmap WHERE MainId = ?1", [mipmap], |r| r.get(0)).ok()?;
        let off: i64 = self.db.query_row("SELECT Offscreen FROM MipmapInfo WHERE MainId = ?1", [info], |r| r.get(0)).ok()?;
        self.db
            .query_row("SELECT Attribute, BlockData FROM Offscreen WHERE MainId = ?1", [off], |r| {
                let attribute: Vec<u8> = r.get(0)?;
                let id: Vec<u8> = r.get(1)?;
                Ok((attribute, String::from_utf8_lossy(&id).into_owned()))
            })
            .ok()
    }

    /// A pixel buffer placed at `origin` on the canvas: colour (premultiplied
    /// tiles) or, for one byte a pixel, coverage in the alpha.
    fn pixels(&mut self, mipmap: i64, origin: (i64, i64)) -> Option<TileStore> {
        let (attribute, id) = self.offscreen(mipmap)?;
        let (w, h, across, _) = offscreen_size(&attribute)?;
        let data = self.buffers.get(&id)?;
        let mut store = TileStore::new();
        let (cw, ch) = (self.canvas.0 as i64, self.canvas.1 as i64);
        let n = (BLOCK * BLOCK) as usize;
        for (index, raw) in read_blocks(data) {
            let (bx, by) = ((index % across.max(1)) * BLOCK, (index / across.max(1)) * BLOCK);
            let per_pixel = raw.len() / n;
            for y in 0..BLOCK {
                for x in 0..BLOCK {
                    let (ox, oy) = (bx + x, by + y);
                    if ox >= w || oy >= h {
                        continue;
                    }
                    let (px, py) = (origin.0 + ox as i64, origin.1 + oy as i64);
                    if px < 0 || py < 0 || px >= cw || py >= ch {
                        continue;
                    }
                    let i = (y * BLOCK + x) as usize;
                    let rgba = match per_pixel {
                        // The alpha plane, then B, G, R, x.
                        5 => {
                            let c = &raw[n + i * 4..n + i * 4 + 4];
                            [c[2], c[1], c[0], raw[i]]
                        }
                        // Grey and alpha planes.
                        2 => [raw[n + i], raw[n + i], raw[n + i], raw[i]],
                        // Coverage only (masks, monochrome).
                        1 => [raw[i]; 4],
                        _ => continue,
                    };
                    if rgba[3] == 0 {
                        continue;
                    }
                    let coord = TileCoord::containing(px as i32, py as i32);
                    let (tx, ty) = coord.origin();
                    store.get_or_create_mut(coord).set_pixel((px as i32 - tx) as u32, (py as i32 - ty) as u32, premultiply(rgba));
                }
            }
        }
        Some(store)
    }

    /// The children of folder `first`'s chain, bottom to top.
    fn chain(&mut self, rows: &HashMap<i64, Row>, mut at: i64, depth: usize) -> Vec<Layer> {
        let mut out = Vec::new();
        let mut seen = 0;
        while at != 0 && seen < 10_000 {
            seen += 1;
            let Some(row) = rows.get(&at) else { break };
            if let Some(layer) = self.layer(rows, row, depth) {
                out.push(layer);
            }
            at = row.next;
        }
        out
    }

    fn layer(&mut self, rows: &HashMap<i64, Row>, row: &Row, depth: usize) -> Option<Layer> {
        let id = self.id();
        let name = if row.name.is_empty() { "Layer".to_string() } else { row.name.clone() };
        let mut layer = if row.folder {
            let children = if depth < 64 { self.chain(rows, row.first_child, depth + 1) } else { Vec::new() };
            let mut f = Layer::folder(id, name.clone());
            f.kind = LayerKind::Folder(Folder { children, pass_through: false });
            f
        } else if let Some(color) = row.paper {
            Layer::fill(id, name.clone(), color)
        } else {
            let origin = (row.offset.0 + row.render_offset.0, row.offset.1 + row.render_offset.1);
            let tiles = if row.render_mipmap != 0 { self.pixels(row.render_mipmap, origin) } else { None };
            if tiles.is_none() && row.render_mipmap != 0 {
                self.lost = true;
                if row.vector {
                    // Clip Studio redraws vector layers from their strokes; it keeps no pixels for them.
                    self.warnings.push(format!("“{name}”: a vector layer, whose strokes aren't read yet; see the hidden “Clip Studio preview” layer (or rasterize it in Clip Studio first)"));
                } else {
                    self.warnings.push(format!("“{name}”: its picture isn't stored in the file (an image, video or 3D layer); see the hidden “Clip Studio preview” layer"));
                }
            }
            if tiles.is_some() && row.kind != 1 && row.kind != 0 {
                // Text, gradient, fill, 3D… layers: their rendered picture.
                self.warnings.push(format!("“{name}”: came in as pixels"));
            }
            Layer::raster_with(id, name.clone(), tiles.unwrap_or_default())
        };
        layer.opacity = (row.opacity as f32 / 256.0).clamp(0.0, 1.0);
        layer.visible = row.visible;
        layer.locked = row.locked;
        layer.clip = row.clip;
        layer.draft = row.draft;
        layer.blend = blend_of(row.blend).unwrap_or_else(|| {
            self.warnings.push(format!("“{name}”: blend mode {} isn't supported, using Normal", row.blend));
            BlendMode::Normal
        });
        if row.mask_mipmap != 0 {
            match self.pixels(row.mask_mipmap, row.mask_offset) {
                // Clip Studio's mask: 255 = shown; ours: alpha = hidden.
                Some(shown) => layer.mask = Some(Mask { tiles: invert_coverage(&shown, self.canvas), enabled: true }),
                None => self.warnings.push(format!("“{name}”: its mask couldn't be read")),
            }
        }
        if row.selected && !layer.is_folder() {
            self.active = Some(layer.id);
        }
        Some(layer)
    }
}

/// Coverage tiles (alpha = shown) as a mask (alpha = hidden) over the canvas.
fn invert_coverage(shown: &TileStore, canvas: (u32, u32)) -> TileStore {
    let mut out = TileStore::new();
    for ty in 0..canvas.1.div_ceil(crate::tile::TILE_SIZE) as i32 {
        for tx in 0..canvas.0.div_ceil(crate::tile::TILE_SIZE) as i32 {
            let coord = TileCoord::new(tx, ty);
            let src = shown.get(coord);
            let tile = out.get_or_create_mut(coord);
            for y in 0..crate::tile::TILE_SIZE {
                for x in 0..crate::tile::TILE_SIZE {
                    let a = src.map_or(0, |t| t.pixel(x, y)[3]);
                    tile.set_pixel(x, y, [255 - a; 4]);
                }
            }
        }
    }
    // Fully shown tiles cost nothing.
    let empty: Vec<TileCoord> = out.iter().filter(|(_, t)| t.is_transparent()).map(|(c, _)| c).collect();
    for c in empty {
        out.remove(c);
    }
    out
}

/// The SQLite database inside a `.clip`, opened from a temporary copy.
pub(crate) fn open_database(data: &[u8], chunks: &[Chunk]) -> Result<(rusqlite::Connection, std::path::PathBuf), String> {
    let sql = chunks.iter().find(|c| &c.kind == b"SQLi").ok_or("no database in this Clip Studio file")?;
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("painter-clip-{}-{n}.sqlite", std::process::id()));
    std::fs::write(&tmp, &data[sql.body.clone()]).map_err(|e| e.to_string())?;
    let db = rusqlite::Connection::open(&tmp).map_err(|e| e.to_string())?;
    Ok((db, tmp))
}

/// Reads a `.clip`; also returns what couldn't be read as is.
pub fn read(data: &[u8]) -> Result<(Document, Vec<String>), String> {
    let chunks = chunks(data)?;
    let mut buffers = HashMap::new();
    for c in chunks.iter().filter(|c| &c.kind == b"Exta") {
        if let Some((id, blocks)) = exta(&data[c.body.clone()]) {
            buffers.insert(id, blocks);
        }
    }
    let (db, tmp) = open_database(data, &chunks)?;
    let result = (|| {
        let (w, h, dpi, root): (f64, f64, f64, i64) = db
            .query_row("SELECT CanvasWidth, CanvasHeight, CanvasResolution, CanvasRootFolder FROM Canvas LIMIT 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get::<_, Option<f64>>(2)?.unwrap_or(300.0), r.get(3)?))
            })
            .map_err(|e| format!("no canvas: {e}"))?;
        let canvas = (w.round().max(1.0) as u32, h.round().max(1.0) as u32);
        let mut reader = Reader { db, buffers, canvas, warnings: Vec::new(), next_id: 0, active: None, lost: false };
        let rows = reader.rows()?;
        let first = rows.get(&root).map(|r| r.first_child).ok_or("no root folder")?;
        // The size in pixels is the root folder's picture's: the canvas may be in mm or cm.
        if let Some((w, h, _, _)) = rows
            .get(&root)
            .filter(|r| r.render_mipmap != 0)
            .and_then(|r| reader.offscreen(r.render_mipmap))
            .and_then(|(attribute, _)| offscreen_size(&attribute))
        {
            reader.canvas = (w.max(1), h.max(1));
        }
        let canvas = reader.canvas;
        let mut layers = reader.chain(&rows, first, 0);
        if reader.lost {
            // Clip Studio's own picture of the whole document, hidden, so nothing is lost.
            let image = reader.db.query_row("SELECT ImageData FROM CanvasPreview LIMIT 1", [], |r| r.get::<_, Vec<u8>>(0)).ok();
            if let Some(img) = image.and_then(|b| image::load_from_memory(&b).ok()) {
                let img = image::imageops::resize(&img.to_rgba8(), canvas.0, canvas.1, image::imageops::FilterType::Triangle);
                let mut tiles = TileStore::new();
                crate::interchange::blit_rgba(&mut tiles, 0, 0, canvas.0, canvas.1, img.as_raw());
                let id = reader.id();
                let mut l = Layer::raster_with(id, "Clip Studio preview", tiles);
                l.visible = false;
                layers.push(l);
            }
        }
        let active = reader.active.or_else(|| first_raster(&layers)).unwrap_or(LayerId(0));
        let mut doc = Document::from_parts(canvas.0, canvas.1, layers, active, reader.next_id + 1);
        doc.dpi = dpi.round().max(1.0) as u32;
        if doc.layers.is_empty() {
            let id = doc.alloc_id();
            doc.layers.push(Layer::raster(id, "Layer 1"));
            doc.active = id;
        }
        let mut warnings = reader.warnings;
        warnings.dedup();
        Ok((doc, warnings))
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}

fn first_raster(layers: &[Layer]) -> Option<LayerId> {
    layers.iter().rev().find_map(|l| match &l.kind {
        LayerKind::Folder(f) => first_raster(&f.children),
        LayerKind::Raster(_) => Some(l.id),
        _ => None,
    })
}

pub fn load(path: &Path) -> Result<(Document, Vec<String>), String> {
    read(&std::fs::read(path).map_err(|e| e.to_string())?)
}

/// Clip Studio's own preview of the document (a PNG or JPEG in the database), if any.
pub fn preview(data: &[u8]) -> Option<Vec<u8>> {
    let chunks = chunks(data).ok()?;
    let (db, tmp) = open_database(data, &chunks).ok()?;
    let image = db.query_row("SELECT ImageData FROM CanvasPreview LIMIT 1", [], |r| r.get::<_, Vec<u8>>(0)).ok();
    drop(db);
    let _ = std::fs::remove_file(tmp);
    image
}

// ---- Writing ----

/// Mipmap levels go down by halves while the bigger side stays this long.
const MIN_MIPMAP_SIDE: u32 = 64;

/// A premultiplied RGBA picture.
struct Picture {
    w: u32,
    h: u32,
    px: Vec<[u8; 4]>,
}

impl Picture {
    fn from_tiles(tiles: &TileStore, w: u32, h: u32) -> Self {
        let mut px = vec![[0u8; 4]; (w * h) as usize];
        for (coord, tile) in tiles.iter() {
            let (ox, oy) = coord.origin();
            for y in 0..crate::tile::TILE_SIZE {
                for x in 0..crate::tile::TILE_SIZE {
                    let (cx, cy) = (ox + x as i32, oy + y as i32);
                    if cx >= 0 && cy >= 0 && (cx as u32) < w && (cy as u32) < h {
                        px[(cy as u32 * w + cx as u32) as usize] = tile.pixel(x, y);
                    }
                }
            }
        }
        Self { w, h, px }
    }

    fn from_flat(w: u32, h: u32, rgba: &[u8]) -> Self {
        Self { w, h, px: rgba.as_chunks::<4>().0.to_vec() }
    }

    /// Half the size (rounded down), each pixel the average of four.
    fn half(&self) -> Self {
        let (w, h) = ((self.w / 2).max(1), (self.h / 2).max(1));
        let mut px = vec![[0u8; 4]; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let mut sum = [0u32; 4];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (sx, sy) = ((x * 2 + dx).min(self.w - 1), (y * 2 + dy).min(self.h - 1));
                    let p = self.px[(sy * self.w + sx) as usize];
                    for (s, v) in sum.iter_mut().zip(p) {
                        *s += v as u32;
                    }
                }
                px[(y * w + x) as usize] = sum.map(|v| ((v + 2) / 4) as u8);
            }
        }
        Self { w, h, px }
    }
}

/// How many mipmap levels Clip Studio keeps for a picture this size.
fn mipmap_levels(w: u32, h: u32) -> usize {
    let (mut w, mut h, mut n) = (w, h, 1);
    while (w / 2).max(h / 2) >= MIN_MIPMAP_SIDE {
        (w, h, n) = (w / 2, h / 2, n + 1);
    }
    n
}

/// One "BlockDataBeginChunk" … "BlockDataEndChunk" record.
fn block_record(index: u32, zlib: Option<&[u8]>) -> Vec<u8> {
    let size = 104 + zlib.map_or(0, |z| 8 + z.len());
    let mut r = Vec::with_capacity(size);
    r.extend((size as u32).to_be_bytes());
    r.extend(19u32.to_be_bytes());
    r.extend(utf16be("BlockDataBeginChunk"));
    for v in [index, BLOCK * BLOCK * 5, BLOCK, BLOCK, zlib.is_some() as u32] {
        r.extend(v.to_be_bytes());
    }
    if let Some(z) = zlib {
        r.extend((z.len() as u32 + 4).to_be_bytes());
        r.extend((z.len() as u32).to_le_bytes());
        r.extend(z);
    }
    r.extend(17u32.to_be_bytes());
    r.extend(utf16be("BlockDataEndChunk"));
    r
}

/// A picture as block records, and each record's size.
fn encode_blocks(p: &Picture) -> (Vec<u8>, Vec<u32>) {
    let (across, down) = (p.w.div_ceil(BLOCK), p.h.div_ceil(BLOCK));
    let n = (BLOCK * BLOCK) as usize;
    let (mut out, mut sizes, mut sums) = (Vec::new(), Vec::new(), Vec::new());
    for by in 0..down {
        for bx in 0..across {
            let mut raw = vec![0u8; n * 5];
            let mut any = false;
            for y in 0..BLOCK {
                for x in 0..BLOCK {
                    let (sx, sy) = (bx * BLOCK + x, by * BLOCK + y);
                    if sx >= p.w || sy >= p.h {
                        continue;
                    }
                    let [r, g, b, a] = crate::blend::unpremultiply(p.px[(sy * p.w + sx) as usize]);
                    if a == 0 {
                        continue;
                    }
                    any = true;
                    let i = (y * BLOCK + x) as usize;
                    raw[i] = a;
                    raw[n + i * 4..n + i * 4 + 4].copy_from_slice(&[b, g, r, 0]);
                }
            }
            let record = if any {
                let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
                std::io::Write::write_all(&mut z, &raw).expect("in memory");
                let z = z.finish().expect("in memory");
                // Adler-32 of what follows the block's length: the stream length (LE) and the stream.
                let mut summed = (z.len() as u32).to_le_bytes().to_vec();
                summed.extend(&z);
                sums.push(adler32(&summed));
                block_record(by * across + bx, Some(&z))
            } else {
                sums.push(0);
                block_record(by * across + bx, None)
            };
            sizes.push(record.len() as u32);
            out.extend(record);
        }
    }
    // Then every block's status (1, as Clip Studio writes on saving) and checksum.
    out.extend(block_table("BlockStatus", &vec![1; sums.len()]));
    out.extend(block_table("BlockCheckSum", &sums));
    (out, sizes)
}

/// A per-block table after the blocks: UTF-16 name, then 12, the count, 4 and the values.
fn block_table(name: &str, values: &[u32]) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend((name.encode_utf16().count() as u32).to_be_bytes());
    t.extend(utf16be(name));
    for v in [12, values.len() as u32, 4] {
        t.extend(v.to_be_bytes());
    }
    for v in values {
        t.extend(v.to_be_bytes());
    }
    t
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

/// An offscreen's attribute blob for a picture: the template's parameter and
/// initial-colour sections with this size, and the block sizes.
fn attribute(template: &[u8], w: u32, h: u32, sizes: &[u32]) -> Result<Vec<u8>, String> {
    let bad = || "the template's pixel buffers aren't in a known layout".to_string();
    let field = |i: usize| be32(template, i).map(|v| v as usize).ok_or_else(bad);
    let (head, plen, ilen) = (field(0)?, field(4)?, field(8)?);
    let mut param = template.get(head..head + plen).ok_or_else(bad)?.to_vec();
    let init = template.get(head + plen..head + plen + ilen).ok_or_else(bad)?;
    let name = utf16be("Parameter");
    if head != 16 || param.len() < 4 + name.len() + 16 || param[4..4 + name.len()] != name[..] {
        return Err(bad());
    }
    let at = 4 + name.len();
    for (i, v) in [w, h, w.div_ceil(BLOCK), h.div_ceil(BLOCK)].into_iter().enumerate() {
        param[at + i * 4..at + i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    let mut blocks = Vec::new();
    blocks.extend(9u32.to_be_bytes());
    blocks.extend(utf16be("BlockSize"));
    for v in [12, sizes.len() as u32, 4] {
        blocks.extend(v.to_be_bytes());
    }
    for s in sizes {
        blocks.extend(s.to_be_bytes());
    }
    let mut out = Vec::new();
    for v in [16, param.len() as u32, init.len() as u32, blocks.len() as u32] {
        out.extend(v.to_be_bytes());
    }
    out.extend(param);
    out.extend(init);
    out.extend(blocks);
    Ok(out)
}

/// Random hex digits (ids only need to be unique).
fn random_hex(n: usize) -> String {
    use std::hash::{BuildHasher, Hasher};
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut s = String::new();
    while s.len() < n {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        s += &format!("{:016X}", h.finish());
    }
    s.truncate(n);
    s
}

/// A layer UUID the way Clip Studio writes them (10-4-4-4-10 hex digits).
fn layer_uuid() -> String {
    let h = random_hex(32).to_lowercase();
    format!("{}-{}-{}-{}-{}", &h[..10], &h[10..14], &h[14..18], &h[18..22], &h[22..])
}

type Value = rusqlite::types::Value;

struct Writer<'a> {
    db: &'a rusqlite::Connection,
    doc: &'a Document,
    /// (id, block records) for each pixel buffer.
    buffers: Vec<(String, Vec<u8>)>,
    warnings: Vec<String>,
    next_layer: i64,
    next_mipmap: i64,
    next_info: i64,
    next_offscreen: i64,
    next_thumbnail: i64,
    /// A colour offscreen's attribute blob from the template.
    color_attribute: Vec<u8>,
    thumbnails: bool,
    active: i64,
}

fn sql(e: rusqlite::Error) -> String {
    format!("database: {e}")
}

/// The columns of `table` but `_PW_ID`.
fn columns(db: &rusqlite::Connection, table: &str) -> Result<Vec<String>, String> {
    let mut s = db.prepare(&format!("PRAGMA table_info(\"{table}\")")).map_err(sql)?;
    let names = s.query_map([], |r| r.get::<_, String>(1)).map_err(sql)?.collect::<Result<Vec<_>, _>>().map_err(sql)?;
    Ok(names.into_iter().filter(|c| c != "_PW_ID").collect())
}

impl Writer<'_> {
    /// A copy of the template row kept in `temp.proto_<table>`, with `set` changed.
    fn insert(&self, table: &str, set: &[(&str, Value)]) -> Result<(), String> {
        let have = columns(self.db, table)?;
        let cols = have.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(", ");
        self.db.execute(&format!("INSERT INTO \"{table}\" ({cols}) SELECT {cols} FROM temp.\"proto_{table}\""), []).map_err(sql)?;
        let row = self.db.last_insert_rowid();
        let set: Vec<&(&str, Value)> = set.iter().filter(|(c, _)| have.iter().any(|h| h == c)).collect();
        if !set.is_empty() {
            let assign = set.iter().enumerate().map(|(i, (c, _))| format!("\"{c}\" = ?{}", i + 1)).collect::<Vec<_>>().join(", ");
            let values: Vec<&dyn rusqlite::ToSql> = set.iter().map(|(_, v)| v as &dyn rusqlite::ToSql).collect();
            self.db.execute(&format!("UPDATE \"{table}\" SET {assign} WHERE _PW_ID = {row}"), values.as_slice()).map_err(sql)?;
        }
        Ok(())
    }

    /// A new pixel buffer and its `Offscreen` row; the row's id.
    fn offscreen(&mut self, layer: i64, picture: &Picture) -> Result<i64, String> {
        let (records, sizes) = encode_blocks(picture);
        let id = format!("extrnlid{}", random_hex(32));
        let off = self.next_offscreen;
        self.next_offscreen += 1;
        let attribute = attribute(&self.color_attribute, picture.w, picture.h, &sizes)?;
        self.insert(
            "Offscreen",
            &[
                ("MainId", Value::Integer(off)),
                ("LayerId", Value::Integer(layer)),
                ("Attribute", Value::Blob(attribute)),
                ("BlockData", Value::Blob(id.clone().into_bytes())),
            ],
        )?;
        self.buffers.push((id, records));
        Ok(off)
    }

    /// A picture as a mipmap (every level a pixel buffer) for layer `layer`; its id.
    fn mipmap(&mut self, layer: i64, picture: Picture) -> Result<i64, String> {
        let levels = mipmap_levels(picture.w, picture.h);
        let first = self.next_info;
        let mut level = picture;
        for i in 0..levels {
            let off = self.offscreen(layer, &level)?;
            let info = self.next_info;
            self.next_info += 1;
            let next = if i + 1 < levels { info + 1 } else { 0 };
            self.insert(
                "MipmapInfo",
                &[
                    ("MainId", Value::Integer(info)),
                    ("LayerId", Value::Integer(layer)),
                    ("ThisScale", Value::Real(100.0 / (1u64 << i) as f64)),
                    ("Offscreen", Value::Integer(off)),
                    ("NextIndex", Value::Integer(next)),
                ],
            )?;
            if i + 1 < levels {
                level = level.half();
            }
        }
        let mipmap = self.next_mipmap;
        self.next_mipmap += 1;
        self.insert(
            "Mipmap",
            &[
                ("MainId", Value::Integer(mipmap)),
                ("LayerId", Value::Integer(layer)),
                ("MipmapCount", Value::Integer(levels as i64)),
                ("BaseMipmapInfo", Value::Integer(first)),
            ],
        )?;
        Ok(mipmap)
    }

    /// An empty thumbnail for Clip Studio to redraw; its id, or 0 when the template has none.
    fn thumbnail(&mut self, layer: i64) -> Result<i64, String> {
        if !self.thumbnails {
            return Ok(0);
        }
        let side = 512;
        let off = self.offscreen(layer, &Picture { w: side, h: side, px: vec![[0; 4]; (side * side) as usize] })?;
        let thumb = self.next_thumbnail;
        self.next_thumbnail += 1;
        let (w, h) = self.doc.canvas();
        self.insert(
            "LayerThumbnail",
            &[
                ("MainId", Value::Integer(thumb)),
                ("LayerId", Value::Integer(layer)),
                ("ThumbnailOffscreen", Value::Integer(off)),
                ("ThumbnailCanvasWidth", Value::Integer(w as i64)),
                ("ThumbnailCanvasHeight", Value::Integer(h as i64)),
            ],
        )?;
        Ok(thumb)
    }

    /// Writes `layers` (bottom to top) as siblings; the first one's id, or 0.
    fn list(&mut self, layers: &[Layer]) -> Result<i64, String> {
        let mut ids = Vec::new();
        for layer in layers {
            if let Some(id) = self.layer(layer)? {
                ids.push(id);
            }
        }
        for pair in ids.windows(2) {
            self.db.execute("UPDATE Layer SET LayerNextIndex = ?1 WHERE MainId = ?2", [pair[1], pair[0]]).map_err(sql)?;
        }
        Ok(ids.first().copied().unwrap_or(0))
    }

    fn layer(&mut self, layer: &Layer) -> Result<Option<i64>, String> {
        let canvas = self.doc.canvas();
        match &layer.kind {
            LayerKind::Adjustment(_) => {
                self.warnings
                    .push(format!("“{}”: correction layers aren't written to Clip Studio files and were left out", layer.name));
                Ok(None)
            }
            LayerKind::Folder(f) => {
                if layer.active_mask().is_some() {
                    self.warnings.push(format!("“{}”: folder masks aren't written to Clip Studio files and were dropped", layer.name));
                }
                let id = self.next_layer;
                self.next_layer += 1;
                let first = self.list(&f.children)?;
                // The folder's own picture: its children composited.
                let inner = Document::from_parts(canvas.0, canvas.1, f.children.clone(), LayerId(0), 1);
                let flat = crate::io::flatten(&inner);
                self.row(layer, id, Picture::from_flat(canvas.0, canvas.1, &flat), first, true).map(Some)
            }
            kind => {
                if layer.active_mask().is_some() {
                    self.warnings.push(format!("“{}”: its mask was applied to the pixels", layer.name));
                }
                if !matches!(kind, LayerKind::Raster(_)) {
                    self.warnings.push(format!("“{}”: saved as pixels", layer.name));
                }
                let tiles = crate::interchange::layer_pixels(layer, canvas, true).unwrap_or_default();
                let id = self.next_layer;
                self.next_layer += 1;
                self.row(layer, id, Picture::from_tiles(&tiles, canvas.0, canvas.1), 0, false).map(Some)
            }
        }
    }

    fn row(&mut self, layer: &Layer, id: i64, picture: Picture, first_child: i64, folder: bool) -> Result<i64, String> {
        let mipmap = self.mipmap(id, picture)?;
        let thumbnail = self.thumbnail(id)?;
        let active = layer.id == self.doc.active;
        if active {
            self.active = id;
        }
        let flag = |b: bool| Value::Integer(b as i64);
        let zero = || Value::Integer(0);
        self.insert(
            "Layer",
            &[
                ("MainId", Value::Integer(id)),
                ("LayerName", Value::Text(layer.name.clone())),
                ("LayerType", Value::Integer(if folder { 0 } else { 1 })),
                ("LayerFolder", flag(folder)),
                ("LayerOpacity", Value::Integer((layer.opacity.clamp(0.0, 1.0) * 256.0).round() as i64)),
                ("LayerComposite", Value::Integer(blend_number(layer.blend))),
                ("LayerVisibility", flag(layer.visible)),
                ("LayerLock", flag(layer.locked)),
                ("LayerClip", flag(layer.clip)),
                ("LayerSelect", flag(active)),
                ("LayerFirstChildIndex", Value::Integer(first_child)),
                ("LayerNextIndex", zero()),
                ("LayerOffsetX", zero()),
                ("LayerOffsetY", zero()),
                ("LayerRenderOffscrOffsetX", zero()),
                ("LayerRenderOffscrOffsetY", zero()),
                ("LayerMaskOffsetX", zero()),
                ("LayerMaskOffsetY", zero()),
                ("LayerMaskOffscrOffsetX", zero()),
                ("LayerMaskOffscrOffsetY", zero()),
                ("LayerRenderMipmap", Value::Integer(mipmap)),
                ("LayerLayerMaskMipmap", zero()),
                ("LayerRenderThumbnail", Value::Integer(thumbnail)),
                ("LayerLayerMaskThumbnail", zero()),
                ("LayerUuid", Value::Text(layer_uuid())),
            ],
        )?;
        Ok(id)
    }
}

/// The tables with a `LayerId` column (rows that belong to layers).
fn layer_tables(db: &rusqlite::Connection) -> Result<Vec<String>, String> {
    let mut s = db.prepare("SELECT name FROM sqlite_master WHERE type = 'table'").map_err(sql)?;
    let tables = s.query_map([], |r| r.get::<_, String>(0)).map_err(sql)?.collect::<Result<Vec<_>, _>>().map_err(sql)?;
    let mut out = Vec::new();
    for t in tables {
        if columns(db, &t)?.iter().any(|c| c == "LayerId") {
            out.push(t);
        }
    }
    Ok(out)
}

/// The template's database rebuilt for `doc`; returns the pixel buffers to write.
type Filled = (Vec<(String, Vec<u8>)>, Vec<String>);

fn fill_database(db: &rusqlite::Connection, doc: &Document) -> Result<Filled, String> {
    let root: i64 =
        db.query_row("SELECT CanvasRootFolder FROM Canvas LIMIT 1", [], |r| r.get(0)).map_err(|_| "the template has no canvas")?;
    let has_vector = db.prepare("SELECT VectorNormalStrokeIndex FROM Layer LIMIT 0").is_ok();
    let raster: i64 = db
        .query_row(
            &format!(
                "SELECT MainId FROM Layer WHERE LayerType = 1 AND COALESCE(LayerFolder, 0) = 0 AND MainId != ?1 {} ORDER BY _PW_ID LIMIT 1",
                if has_vector { "AND COALESCE(VectorNormalStrokeIndex, 0) = 0" } else { "" }
            ),
            [root],
            |r| r.get(0),
        )
        .map_err(|_| "the template needs a raster layer: save a new canvas from Clip Studio and choose that")?;
    let one = |q: &str, id: i64| -> Result<i64, String> { db.query_row(q, [id], |r| r.get(0)).map_err(sql) };
    let mipmap = one("SELECT LayerRenderMipmap FROM Layer WHERE MainId = ?1", raster)?;
    let info = one("SELECT BaseMipmapInfo FROM Mipmap WHERE MainId = ?1", mipmap)?;
    let off = one("SELECT Offscreen FROM MipmapInfo WHERE MainId = ?1", info)?;
    let color_attribute: Vec<u8> =
        db.query_row("SELECT Attribute FROM Offscreen WHERE MainId = ?1", [off], |r| r.get(0)).map_err(sql)?;
    for (table, filter) in [
        ("Layer", format!("MainId = {raster}")),
        ("Mipmap", format!("MainId = {mipmap}")),
        ("MipmapInfo", format!("MainId = {info}")),
        ("Offscreen", format!("MainId = {off}")),
    ] {
        db.execute(&format!("CREATE TEMP TABLE \"proto_{table}\" AS SELECT * FROM \"{table}\" WHERE {filter} LIMIT 1"), [])
            .map_err(sql)?;
    }
    // Thumbnails are optional: Clip Studio redraws them.
    let thumbnails: i64 = db
        .execute(
            &format!("CREATE TEMP TABLE proto_LayerThumbnail AS SELECT * FROM LayerThumbnail WHERE LayerId = {raster} LIMIT 1"),
            [],
        )
        .and_then(|_| db.query_row("SELECT COUNT(*) FROM temp.proto_LayerThumbnail", [], |r| r.get(0)))
        .unwrap_or(0);

    // Out with the template's layers and everything hanging off them.
    db.execute("DELETE FROM Layer WHERE MainId != ?1", [root]).map_err(sql)?;
    for t in layer_tables(db)? {
        db.execute(&format!("DELETE FROM \"{t}\""), []).map_err(sql)?;
    }
    for t in ["ExternalChunk", "RemovedExternal", "LayerComp"] {
        let _ = db.execute(&format!("DELETE FROM \"{t}\""), []);
    }
    let _ =
        db.execute("UPDATE LayerCompManager SET FirstLayerCompIndex = 0, LastStateLayerCompIndex = 0, AppliedLayerCompIndex = -1", []);

    let mut w = Writer {
        db,
        doc,
        buffers: Vec::new(),
        warnings: Vec::new(),
        next_layer: root + 1,
        next_mipmap: 1,
        next_info: 1,
        next_offscreen: 1,
        next_thumbnail: 1,
        color_attribute,
        thumbnails: thumbnails > 0,
        active: 0,
    };
    let (cw, ch) = doc.canvas();
    let root_mipmap = w.mipmap(root, Picture::from_flat(cw, ch, &crate::io::flatten(doc)))?;
    let root_thumbnail = w.thumbnail(root)?;
    let first = w.list(&doc.layers)?;
    db.execute(
        "UPDATE Layer SET LayerFirstChildIndex = ?1, LayerRenderMipmap = ?2, LayerRenderThumbnail = ?3, LayerLayerMaskMipmap = 0 WHERE MainId = ?4",
        [first, root_mipmap, root_thumbnail, root],
    )
    .map_err(sql)?;
    let active = if w.active != 0 { w.active } else { first };
    db.execute(
        "UPDATE Canvas SET CanvasWidth = ?1, CanvasHeight = ?2, CanvasResolution = ?3, CanvasUnit = 0, CanvasCurrentLayer = ?4",
        rusqlite::params![cw as f64, ch as f64, doc.dpi as f64, active],
    )
    .map_err(sql)?;

    // Clip Studio's preview of the whole picture.
    let (merged, _) = crate::interchange::merged_and_thumbnail(doc, 1)?;
    let _ = db.execute(
        "UPDATE CanvasPreview SET ImageType = 1, ImageWidth = ?1, ImageHeight = ?2, ImageData = ?3",
        rusqlite::params![cw as i64, ch as i64, merged],
    );

    // The highest ids in use, as Clip Studio keeps them.
    for (table, max) in [
        ("Layer", w.next_layer - 1),
        ("Mipmap", w.next_mipmap - 1),
        ("MipmapInfo", w.next_info - 1),
        ("Offscreen", w.next_offscreen - 1),
        ("LayerThumbnail", w.next_thumbnail - 1),
    ] {
        let _ = db.execute("UPDATE ElemScheme SET MaxIndex = ?1 WHERE TableName = ?2", rusqlite::params![max, table]);
    }
    Ok((w.buffers, w.warnings))
}

/// The document written into `template`, a `.clip` saved by Clip Studio
/// (its canvas settings are kept, its layers replaced); also returns what
/// couldn't be kept as is.
pub fn write(doc: &Document, template: &[u8]) -> Result<(Vec<u8>, Vec<String>), String> {
    let chunks = chunks(template)?;
    let head = chunks.iter().find(|c| &c.kind == b"Head").ok_or("the template has no header chunk")?;
    let mut head = template[head.body.clone()].to_vec();
    if head.len() < 24 {
        return Err("the template's header chunk is too short".into());
    }
    let (db, tmp) = open_database(template, &chunks)?;
    let filled = fill_database(&db, doc).and_then(|(buffers, warnings)| {
        // The buffers come first in the file; the database says where each one starts.
        let mut at = 24 + 16 + head.len() as u64;
        for (id, records) in &buffers {
            db.execute("INSERT INTO ExternalChunk (ExternalID, Offset) VALUES (?1, ?2)", rusqlite::params![id.as_bytes(), at as i64])
                .map_err(sql)?;
            at += 16 + 8 + id.len() as u64 + 8 + records.len() as u64;
        }
        Ok((buffers, warnings, at))
    });
    let closed = db.close().map_err(|(_, e)| sql(e));
    let result = filled.and_then(|(buffers, warnings, database_at)| {
        closed?;
        let database = std::fs::read(&tmp).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        out.extend(b"CSFCHUNK");
        out.extend([0u8; 8]); // the file size, set below
        out.extend(24u64.to_be_bytes());
        head[8..16].copy_from_slice(&database_at.to_be_bytes());
        let chunk = |out: &mut Vec<u8>, kind: &[u8; 4], body: &[&[u8]]| {
            out.extend(b"CHNK");
            out.extend(kind);
            out.extend((body.iter().map(|b| b.len()).sum::<usize>() as u64).to_be_bytes());
            for b in body {
                out.extend(*b);
            }
        };
        chunk(&mut out, b"Head", &[&head]);
        for (id, records) in &buffers {
            chunk(
                &mut out,
                b"Exta",
                &[&(id.len() as u64).to_be_bytes(), id.as_bytes(), &(records.len() as u64).to_be_bytes(), records],
            );
        }
        debug_assert_eq!(out.len() as u64, database_at);
        chunk(&mut out, b"SQLi", &[&database]);
        chunk(&mut out, b"Foot", &[]);
        let size = out.len() as u64;
        out[8..16].copy_from_slice(&size.to_be_bytes());
        Ok((out, warnings))
    });
    let _ = std::fs::remove_file(&tmp);
    result
}

/// Writes the document into a copy of the template file at `path`.
pub fn save_with_template(doc: &Document, template: &Path, path: &Path) -> Result<Vec<String>, String> {
    let template = std::fs::read(template).map_err(|e| format!("the Clip Studio template: {e}"))?;
    let (bytes, warnings) = write(doc, &template)?;
    std::fs::write(path, bytes).map_err(|e| e.to_string())?;
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_and_attributes_parse() {
        // A block record as Clip Studio writes it, with one block of data.
        let mut raw = vec![0u8; (BLOCK * BLOCK * 5) as usize];
        raw[0] = 255; // alpha of pixel 0
        raw[(BLOCK * BLOCK) as usize..(BLOCK * BLOCK) as usize + 4].copy_from_slice(&[30, 20, 10, 0]);
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut z, &raw).unwrap();
        let z = z.finish().unwrap();
        let mut rec = Vec::new();
        rec.extend(0u32.to_be_bytes());
        rec.extend(19u32.to_be_bytes());
        rec.extend(utf16be("BlockDataBeginChunk"));
        for v in [7u32, raw.len() as u32, BLOCK, BLOCK, 1, z.len() as u32 + 4] {
            rec.extend(v.to_be_bytes());
        }
        rec.extend((z.len() as u32).to_le_bytes());
        rec.extend(&z);
        let blocks = read_blocks(&rec);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].0, 7);
        assert_eq!(&blocks[0].1[..1], &[255]);

        let mut attr = vec![0, 0, 0, 16];
        attr.extend(9u32.to_be_bytes());
        attr.extend(utf16be("Parameter"));
        for v in [2000u32, 1000, 8, 4] {
            attr.extend(v.to_be_bytes());
        }
        assert_eq!(offscreen_size(&attr), Some((2000, 1000, 8, 4)));
        assert_eq!(blend_of(blend_number(BlendMode::Multiply)), Some(BlendMode::Multiply));
        assert!(read(b"nope").is_err());
    }

    /// A tiny stand-in for a file saved by Clip Studio: a root folder and one
    /// empty raster layer, with the tables and columns the reader and writer use.
    fn template() -> Vec<u8> {
        let path = std::env::temp_dir().join(format!("painter-clip-template-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let db = rusqlite::Connection::open(&path).unwrap();
        let attribute = {
            let empty = Picture { w: 8, h: 8, px: vec![[0; 4]; 64] };
            let (_, sizes) = encode_blocks(&empty);
            let mut param = Vec::new();
            param.extend(9u32.to_be_bytes());
            param.extend(utf16be("Parameter"));
            param.extend([0u8; 16 + 64]);
            let mut init = Vec::new();
            init.extend(9u32.to_be_bytes());
            init.extend(utf16be("InitColor"));
            init.extend([0u8; 20]);
            let mut a = Vec::new();
            for v in [16, param.len() as u32, init.len() as u32, 0] {
                a.extend(v.to_be_bytes());
            }
            a.extend(param);
            a.extend(init);
            attribute(&a, 8, 8, &sizes).unwrap()
        };
        db.execute_batch(
            "CREATE TABLE Canvas(_PW_ID INTEGER PRIMARY KEY AUTOINCREMENT, MainId INTEGER, CanvasUnit INTEGER, CanvasWidth REAL, CanvasHeight REAL, CanvasResolution REAL, CanvasRootFolder INTEGER, CanvasCurrentLayer INTEGER);
             CREATE TABLE Layer(_PW_ID INTEGER PRIMARY KEY AUTOINCREMENT, MainId INTEGER, CanvasId INTEGER, LayerName TEXT, LayerType INTEGER, LayerLock INTEGER, LayerClip INTEGER, LayerOffsetX INTEGER, LayerOffsetY INTEGER, LayerRenderOffscrOffsetX INTEGER, LayerRenderOffscrOffsetY INTEGER, LayerMaskOffsetX INTEGER, LayerMaskOffsetY INTEGER, LayerMaskOffscrOffsetX INTEGER, LayerMaskOffscrOffsetY INTEGER, LayerOpacity INTEGER, LayerComposite INTEGER, LayerFolder INTEGER, LayerVisibility INTEGER, LayerSelect INTEGER, LayerNextIndex INTEGER, LayerFirstChildIndex INTEGER, LayerUuid TEXT, LayerRenderMipmap INTEGER, LayerLayerMaskMipmap INTEGER, LayerRenderThumbnail INTEGER, LayerLayerMaskThumbnail INTEGER, DrawColorEnable INTEGER, DrawColorMainRed INTEGER, DrawColorMainGreen INTEGER, DrawColorMainBlue INTEGER, SpecialRenderType INTEGER);
             CREATE TABLE Mipmap(_PW_ID INTEGER PRIMARY KEY AUTOINCREMENT, MainId INTEGER, CanvasId INTEGER, LayerId INTEGER, MipmapCount INTEGER, BaseMipmapInfo INTEGER);
             CREATE TABLE MipmapInfo(_PW_ID INTEGER PRIMARY KEY AUTOINCREMENT, MainId INTEGER, CanvasId INTEGER, LayerId INTEGER, ThisScale REAL, Offscreen INTEGER, NextIndex INTEGER);
             CREATE TABLE Offscreen(_PW_ID INTEGER PRIMARY KEY AUTOINCREMENT, MainId INTEGER, CanvasId INTEGER, LayerId INTEGER, Attribute BLOB, BlockData BLOB);
             CREATE TABLE ExternalChunk(ExternalID BLOB, Offset INTEGER);
             CREATE TABLE ElemScheme(_PW_ID INTEGER PRIMARY KEY AUTOINCREMENT, TableName TEXT, ElemType INTEGER, MaxIndex INTEGER);
             CREATE TABLE CanvasPreview(_PW_ID INTEGER PRIMARY KEY AUTOINCREMENT, MainId INTEGER, CanvasId INTEGER, ImageType INTEGER, ImageWidth INTEGER, ImageHeight INTEGER, ImageData BLOB);
             INSERT INTO Canvas VALUES (1, 1, 0, 8, 8, 72, 2, 3);
             INSERT INTO Layer (MainId, CanvasId, LayerName, LayerType, LayerFolder, LayerOpacity, LayerVisibility, LayerFirstChildIndex) VALUES (2, 1, '', 256, 1, 256, 1, 3);
             INSERT INTO Layer (MainId, CanvasId, LayerName, LayerType, LayerFolder, LayerOpacity, LayerVisibility, LayerRenderMipmap) VALUES (3, 1, 'Layer 1', 1, 0, 256, 1, 1);
             INSERT INTO Mipmap (MainId, CanvasId, LayerId, MipmapCount, BaseMipmapInfo) VALUES (1, 1, 3, 1, 1);
             INSERT INTO MipmapInfo (MainId, CanvasId, LayerId, ThisScale, Offscreen, NextIndex) VALUES (1, 1, 3, 100, 1, 0);
             INSERT INTO ElemScheme (TableName, MaxIndex) VALUES ('Layer', 3);
             INSERT INTO CanvasPreview (MainId, CanvasId) VALUES (1, 1);",
        )
        .unwrap();
        db.execute("INSERT INTO Offscreen (MainId, CanvasId, LayerId, Attribute, BlockData) VALUES (1, 1, 3, ?1, X'00')", [attribute])
            .unwrap();
        drop(db);
        let database = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let mut out = b"CSFCHUNK".to_vec();
        out.extend([0u8; 8]);
        out.extend(24u64.to_be_bytes());
        for (kind, body) in [(b"Head", [0u8; 40].to_vec()), (b"SQLi", database), (b"Foot", Vec::new())] {
            out.extend(b"CHNK");
            out.extend(kind);
            out.extend((body.len() as u64).to_be_bytes());
            out.extend(body);
        }
        out
    }

    #[test]
    fn written_files_read_back() {
        use crate::layer::Folder;
        let (w, h) = (300, 200);
        let rect = |c: [u8; 4], x0: usize, x1: usize| {
            let mut buf = vec![0u8; w * h * 4];
            for y in 20..180 {
                for x in x0..x1 {
                    buf[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&c);
                }
            }
            TileStore::from_rgba(w as u32, h as u32, &buf)
        };
        let mut doc = Document::new(w as u32, h as u32);
        doc.dpi = 350;
        let id = doc.alloc_id();
        doc.layers.push(Layer::raster_with(id, "Red", rect([200, 0, 0, 255], 10, 150)));
        let id = doc.alloc_id();
        let mut blue = Layer::raster_with(id, "Blue", rect([0, 0, 100, 100], 100, 290));
        blue.blend = BlendMode::Multiply;
        blue.opacity = 0.5;
        blue.clip = true;
        let id = doc.alloc_id();
        let mut folder = Layer::folder(id, "Folder");
        folder.kind = LayerKind::Folder(Folder { children: vec![blue], pass_through: false });
        doc.layers.push(folder);

        let (bytes, notes) = write(&doc, &template()).unwrap();
        assert_eq!(notes, vec!["“Paper”: saved as pixels".to_string()]);
        assert_eq!(be64(&bytes, 8), Some(bytes.len() as u64), "file size in the header");
        // Every buffer ends with its blocks' statuses and checksums, as Clip Studio checks them.
        let mut checked = 0;
        for c in chunks(&bytes).unwrap().iter().filter(|c| &c.kind == b"Exta") {
            let (_, blocks) = exta(&bytes[c.body.clone()]).unwrap();
            let marker = utf16be("BlockCheckSum");
            let at = blocks.windows(marker.len()).position(|w| w == marker.as_slice()).expect("checksum table") + marker.len();
            assert!(blocks.windows(22).any(|w| w == utf16be("BlockStatus").as_slice()), "status table");
            let count = be32(blocks, at + 4).unwrap() as usize;
            let begin = utf16be("BlockDataBeginChunk");
            let mut i = 0;
            while let Some(p) = blocks[i..].windows(begin.len()).position(|w| w == begin.as_slice()) {
                let a = i + p + begin.len();
                let index = be32(blocks, a).unwrap() as usize;
                assert!(index < count);
                let stored = be32(blocks, at + 12 + 4 * index).unwrap();
                i = a + 20;
                if be32(blocks, a + 16).unwrap() == 0 {
                    assert_eq!(stored, 0);
                    continue;
                }
                let len = be32(blocks, a + 20).unwrap() as usize;
                assert_eq!(stored, adler32(&blocks[a + 24..a + 24 + len]), "block {index}");
                checked += 1;
                i = a + 24 + len;
            }
        }
        assert!(checked > 0, "some blocks have data");
        let (back, notes) = read(&bytes).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!((back.width, back.height, back.dpi), (w as u32, h as u32, 350));
        let names: Vec<&str> = back.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Paper", "Layer 1", "Red", "Folder"]);
        let LayerKind::Folder(f) = &back.layers[3].kind else { panic!("not a folder") };
        let b = &f.children[0];
        assert_eq!((b.name.as_str(), b.blend, b.clip), ("Blue", BlendMode::Multiply, true));
        assert!((b.opacity - 0.5).abs() < 0.01);
        let (a, z) = (crate::io::flatten(&doc), crate::io::flatten(&back));
        assert!(a.iter().zip(&z).all(|(p, q)| p.abs_diff(*q) <= 1), "same picture");
    }

    #[test]
    fn mipmap_levels_match_clip_studio() {
        // Sizes and level counts seen in files saved by Clip Studio.
        assert_eq!(mipmap_levels(1600, 1200), 5);
        assert_eq!(mipmap_levels(2000, 1000), 5);
        assert_eq!(mipmap_levels(1272, 1076), 5);
        assert_eq!(mipmap_levels(22208, 8598), 9);
    }
}
