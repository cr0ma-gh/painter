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

#[allow(dead_code)] // used by the writer
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
}
