//! Shared drawing sessions, as in Drawpile: the messages peers exchange and
//! the document bookkeeping behind them (no networking here).
//!
//! - A stroke (or fill) travels as its stroke buffer, merged by each peer onto
//!   its own pixels: two people painting in one spot both keep their paint.
//! - Any other change travels as the tiles that differ from the last shared
//!   state (`baseline`), and layer changes as the layer tree without pixels.
//! - Whoever joins gets the whole document as a project file.
//!
//! A frame on the wire: `u32` length (big-endian), then the kind byte, a
//! `u32` header length, the JSON header, and a deflated binary part.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use crate::brush::{BrushBlend, BrushMode};
use crate::document::Document;
use crate::layer::{Layer, LayerId, LayerKind};
use crate::raster::merge_stroke;
use crate::tile::{Tile, TileCoord, TileStore, TILE_BYTES, TILE_PIXELS};

/// Largest frame accepted (a whole document can be large).
pub const MAX_FRAME: usize = 1 << 30;
/// Protocol version; peers with another one are turned away.
pub const VERSION: u32 = 1;

/// A plane of the document: a layer's pixels or its mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlaneId {
    pub layer: u64,
    pub mask: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Peer {
    pub id: u32,
    pub name: String,
}

/// What peers say to each other.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    /// A joining peer introduces itself.
    Hello {
        name: String,
        password: String,
        version: u32,
    },
    /// The host lets it in: its id, who's there, the document (a project file).
    Welcome {
        peer: u32,
        peers: Vec<Peer>,
        document: Vec<u8>,
    },
    Refused {
        reason: String,
    },
    /// Who's in the session now.
    Peers {
        peers: Vec<Peer>,
    },
    /// The layer tree changed ([`crate::project::structure`]).
    Structure {
        from: u32,
        json: Vec<u8>,
    },
    /// Tiles of a plane replaced (`None`: cleared).
    Tiles {
        from: u32,
        plane: PlaneId,
        tiles: Vec<(TileCoord, Option<Vec<u8>>)>,
    },
    /// A stroke to merge: its buffer per tile (premultiplied RGBA8) and how it merges.
    Stroke {
        from: u32,
        plane: PlaneId,
        opacity: f32,
        erase: bool,
        blend: BrushBlend,
        alpha_locked: bool,
        tiles: Vec<(TileCoord, Vec<u8>)>,
    },
    Cursor {
        from: u32,
        x: f32,
        y: f32,
    },
    Chat {
        from: u32,
        text: String,
    },
    /// A peer asks the host for the whole document again.
    ResyncRequest {
        from: u32,
    },
    /// The whole document again (a project file).
    Snapshot {
        document: Vec<u8>,
    },
}

#[derive(Serialize, Deserialize)]
struct Header {
    #[serde(default)]
    from: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    version: u32,
    #[serde(default)]
    peer: u32,
    #[serde(default)]
    peers: Vec<Peer>,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    plane: Option<PlaneId>,
    /// Tile coordinates and whether each carries pixels.
    #[serde(default)]
    coords: Vec<(i32, i32, bool)>,
    #[serde(default)]
    opacity: f32,
    #[serde(default)]
    erase: bool,
    #[serde(default)]
    blend: BrushBlend,
    #[serde(default)]
    alpha_locked: bool,
    #[serde(default)]
    x: f32,
    #[serde(default)]
    y: f32,
    #[serde(default)]
    text: String,
}

impl Default for Header {
    fn default() -> Self {
        Self {
            from: 0,
            name: String::new(),
            password: String::new(),
            version: 0,
            peer: 0,
            peers: Vec::new(),
            reason: String::new(),
            plane: None,
            coords: Vec::new(),
            opacity: 1.0,
            erase: false,
            blend: BrushBlend::Normal,
            alpha_locked: false,
            x: 0.0,
            y: 0.0,
            text: String::new(),
        }
    }
}

fn deflate(bytes: &[u8]) -> Vec<u8> {
    let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    let _ = z.write_all(bytes);
    z.finish().unwrap_or_default()
}

fn inflate(bytes: &[u8], limit: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(bytes).take(limit as u64 + 1).read_to_end(&mut out).ok()?;
    (out.len() <= limit).then_some(out)
}

impl Message {
    /// The whole frame, length first.
    pub fn encode(&self) -> Vec<u8> {
        let mut h = Header::default();
        let mut blob = Vec::new();
        let kind: u8 = match self {
            Message::Hello { name, password, version } => {
                (h.name, h.password, h.version) = (name.clone(), password.clone(), *version);
                1
            }
            Message::Welcome { peer, peers, document } => {
                (h.peer, h.peers) = (*peer, peers.clone());
                blob = document.clone();
                2
            }
            Message::Refused { reason } => {
                h.reason = reason.clone();
                3
            }
            Message::Peers { peers } => {
                h.peers = peers.clone();
                4
            }
            Message::Structure { from, json } => {
                h.from = *from;
                blob = json.clone();
                5
            }
            Message::Tiles { from, plane, tiles } => {
                (h.from, h.plane) = (*from, Some(*plane));
                for (c, t) in tiles {
                    h.coords.push((c.x, c.y, t.is_some()));
                    if let Some(t) = t {
                        blob.extend_from_slice(t);
                    }
                }
                6
            }
            Message::Stroke { from, plane, opacity, erase, blend, alpha_locked, tiles } => {
                (h.from, h.plane, h.opacity, h.erase, h.blend, h.alpha_locked) =
                    (*from, Some(*plane), *opacity, *erase, *blend, *alpha_locked);
                for (c, t) in tiles {
                    h.coords.push((c.x, c.y, true));
                    blob.extend_from_slice(t);
                }
                7
            }
            Message::Cursor { from, x, y } => {
                (h.from, h.x, h.y) = (*from, *x, *y);
                8
            }
            Message::Chat { from, text } => {
                (h.from, h.text) = (*from, text.clone());
                9
            }
            Message::ResyncRequest { from } => {
                h.from = *from;
                10
            }
            Message::Snapshot { document } => {
                blob = document.clone();
                11
            }
        };
        let header = serde_json::to_vec(&h).unwrap_or_default();
        let blob = deflate(&blob);
        let mut out = Vec::with_capacity(9 + header.len() + blob.len());
        out.extend_from_slice(&((5 + header.len() + blob.len()) as u32).to_be_bytes());
        out.push(kind);
        out.extend_from_slice(&(header.len() as u32).to_be_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(&blob);
        out
    }

    /// A frame's body (after its length); `None` when it's malformed.
    pub fn decode(body: &[u8]) -> Option<Message> {
        let kind = *body.first()?;
        let hlen = u32::from_be_bytes(body.get(1..5)?.try_into().ok()?) as usize;
        let h: Header = serde_json::from_slice(body.get(5..5 + hlen)?).ok()?;
        let blob = inflate(body.get(5 + hlen..)?, MAX_FRAME)?;
        let tiles_in = |blob: &[u8], all: bool| -> Option<Vec<(TileCoord, Option<Vec<u8>>)>> {
            let mut at = 0;
            let mut out = Vec::with_capacity(h.coords.len());
            for &(x, y, present) in &h.coords {
                if present || all {
                    let t = blob.get(at..at + TILE_BYTES)?.to_vec();
                    at += TILE_BYTES;
                    out.push((TileCoord::new(x, y), Some(t)));
                } else {
                    out.push((TileCoord::new(x, y), None));
                }
            }
            (at == blob.len()).then_some(out)
        };
        Some(match kind {
            1 => Message::Hello { name: h.name, password: h.password, version: h.version },
            2 => Message::Welcome { peer: h.peer, peers: h.peers, document: blob },
            3 => Message::Refused { reason: h.reason },
            4 => Message::Peers { peers: h.peers },
            5 => Message::Structure { from: h.from, json: blob },
            6 => Message::Tiles { from: h.from, plane: h.plane?, tiles: tiles_in(&blob, false)? },
            7 => Message::Stroke {
                from: h.from,
                plane: h.plane?,
                opacity: h.opacity.clamp(0.0, 1.0),
                erase: h.erase,
                blend: h.blend,
                alpha_locked: h.alpha_locked,
                tiles: tiles_in(&blob, true)?.into_iter().map(|(c, t)| (c, t.unwrap_or_default())).collect(),
            },
            8 => Message::Cursor { from: h.from, x: h.x, y: h.y },
            9 => Message::Chat { from: h.from, text: h.text.chars().take(2000).collect() },
            10 => Message::ResyncRequest { from: h.from },
            11 => Message::Snapshot { document: blob },
            _ => return None,
        })
    }

    /// The peer it came from, for messages that say.
    pub fn from(&self) -> Option<u32> {
        match self {
            Message::Structure { from, .. }
            | Message::Tiles { from, .. }
            | Message::Stroke { from, .. }
            | Message::Cursor { from, .. }
            | Message::Chat { from, .. }
            | Message::ResyncRequest { from } => Some(*from),
            _ => None,
        }
    }
}

// ---- document bookkeeping -------------------------------------------------------

fn find_mut(layers: &mut [Layer], id: LayerId) -> Option<&mut Layer> {
    for l in layers {
        if l.id == id {
            return Some(l);
        }
        if let LayerKind::Folder(f) = &mut l.kind {
            if let Some(found) = find_mut(&mut f.children, id) {
                return Some(found);
            }
        }
    }
    None
}

fn find(layers: &[Layer], id: LayerId) -> Option<&Layer> {
    layers.iter().find_map(|l| {
        if l.id == id {
            return Some(l);
        }
        match &l.kind {
            LayerKind::Folder(f) => find(&f.children, id),
            _ => None,
        }
    })
}

/// The pixels of a plane in a layer tree.
pub fn plane_mut(layers: &mut [Layer], plane: PlaneId) -> Option<&mut TileStore> {
    let l = find_mut(layers, LayerId(plane.layer))?;
    if plane.mask {
        l.mask.as_mut().map(|m| &mut m.tiles)
    } else {
        match &mut l.kind {
            LayerKind::Raster(t) => Some(t),
            _ => None,
        }
    }
}

/// Every plane with pixels of its own in a layer tree (raster layers and masks).
fn planes(layers: &[Layer], out: &mut Vec<(PlaneId, TileStore)>) {
    for l in layers {
        if let LayerKind::Raster(t) = &l.kind {
            out.push((PlaneId { layer: l.id.0, mask: false }, t.clone()));
        }
        if let Some(m) = &l.mask {
            out.push((PlaneId { layer: l.id.0, mask: true }, m.tiles.clone()));
        }
        if let LayerKind::Folder(f) = &l.kind {
            planes(&f.children, out);
        }
    }
}

/// Coordinates where two planes differ (a tile only in one counts).
pub fn changed_tiles(old: &TileStore, new: &TileStore) -> Vec<TileCoord> {
    let mut out: Vec<TileCoord> = new
        .iter()
        .filter(|(c, t)| old.get(*c).is_none_or(|o| !std::sync::Arc::ptr_eq(o, t) && o.bytes() != t.bytes()))
        .map(|(c, _)| c)
        .collect();
    out.extend(old.iter().filter(|(c, _)| new.get(*c).is_none()).map(|(c, _)| c));
    out
}

/// What changed in `doc` since `baseline` (the last shared state): whether
/// the layer tree differs, and the tiles of each plane that differ.
pub fn diff(baseline: &Document, doc: &Document) -> (bool, Vec<(PlaneId, Vec<TileCoord>)>) {
    let structure_changed = crate::project::structure(baseline) != crate::project::structure(doc);
    let (mut old, mut new) = (Vec::new(), Vec::new());
    planes(&baseline.layers, &mut old);
    planes(&doc.layers, &mut new);
    let old: std::collections::HashMap<PlaneId, TileStore> = old.into_iter().collect();
    let mut tiles = Vec::new();
    for (id, store) in new {
        let empty = TileStore::new();
        let before = old.get(&id).unwrap_or(&empty);
        let changed = changed_tiles(before, &store);
        if !changed.is_empty() {
            tiles.push((id, changed));
        }
    }
    (structure_changed, tiles)
}

/// The [`Message::Tiles`] for `coords` of a plane of `doc`.
pub fn tiles_message(from: u32, doc: &Document, plane: PlaneId, coords: &[TileCoord]) -> Option<Message> {
    let l = find(&doc.layers, LayerId(plane.layer))?;
    let store = if plane.mask { &l.mask.as_ref()?.tiles } else { l.tiles()? };
    let tiles = coords.iter().map(|&c| (c, store.get(c).map(|t| t.bytes().to_vec()))).collect();
    Some(Message::Tiles { from, plane, tiles })
}

/// Puts received tiles into a layer tree (`None` clears a tile).
pub fn apply_tiles(layers: &mut [Layer], plane: PlaneId, tiles: &[(TileCoord, Option<Vec<u8>>)]) -> bool {
    let Some(store) = plane_mut(layers, plane) else { return false };
    for (c, t) in tiles {
        match t {
            Some(bytes) if bytes.len() == TILE_BYTES => {
                let mut tile = Tile::empty();
                tile.bytes_mut().copy_from_slice(bytes);
                store.insert(*c, tile);
            }
            _ => {
                store.remove(*c);
            }
        }
    }
    true
}

/// A stroke buffer tile (premultiplied f32) as bytes, scaled by `keep` per
/// pixel (the selection, 0..1) when given.
pub fn stroke_tile_bytes(pixels: &[[f32; 4]], keep: Option<&Tile>) -> Vec<u8> {
    pixels
        .iter()
        .enumerate()
        .flat_map(|(i, p)| {
            let k = keep.map_or(1.0, |t| t.bytes()[i * 4 + 3] as f32 / 255.0);
            p.map(|v| (v * k * 255.0).round().clamp(0.0, 255.0) as u8)
        })
        .collect()
}

/// Merges a received stroke into a layer tree, as the painter's own merge did.
#[allow(clippy::too_many_arguments)]
pub fn apply_stroke(
    layers: &mut [Layer],
    plane: PlaneId,
    opacity: f32,
    erase: bool,
    blend: BrushBlend,
    alpha_locked: bool,
    tiles: &[(TileCoord, Vec<u8>)],
) -> bool {
    let Some(store) = plane_mut(layers, plane) else { return false };
    let mode = if erase { BrushMode::Erase } else { BrushMode::Paint };
    for (c, bytes) in tiles {
        if bytes.len() != TILE_PIXELS * 4 {
            continue;
        }
        let pixels: Vec<[f32; 4]> = bytes.as_chunks::<4>().0.iter().map(|p| p.map(|v| v as f32 / 255.0)).collect();
        let merged = merge_stroke(store.get(*c).map(|t| &**t), &pixels, opacity, mode, blend, alpha_locked, None);
        if merged.bytes().iter().all(|&b| b == 0) {
            store.remove(*c);
        } else {
            store.insert(*c, merged);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_survive_the_wire() {
        let tile = vec![7u8; TILE_BYTES];
        let plane = PlaneId { layer: 42, mask: false };
        for m in [
            Message::Hello { name: "Ada".into(), password: "pw".into(), version: VERSION },
            Message::Welcome { peer: 3, peers: vec![Peer { id: 0, name: "Host".into() }], document: vec![1, 2, 3] },
            Message::Tiles { from: 3, plane, tiles: vec![(TileCoord::new(1, -2), Some(tile.clone())), (TileCoord::new(0, 0), None)] },
            Message::Stroke {
                from: 3,
                plane,
                opacity: 0.5,
                erase: false,
                blend: BrushBlend::Normal,
                alpha_locked: true,
                tiles: vec![(TileCoord::new(4, 4), tile.clone())],
            },
            Message::Chat { from: 1, text: "ciao".into() },
            Message::Cursor { from: 2, x: 10.5, y: 20.0 },
        ] {
            let frame = m.encode();
            let len = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
            assert_eq!(len, frame.len() - 4);
            assert_eq!(Message::decode(&frame[4..]), Some(m));
        }
        assert_eq!(Message::decode(&[99, 0, 0, 0, 2, b'{', b'}']), None);
        assert_eq!(Message::decode(&[6, 0, 0, 0, 0]), None);
    }

    #[test]
    fn two_painters_keep_both_strokes() {
        // Two peers paint the same tile at once; each merges the other's stroke.
        let mut a = Document::new(64, 64);
        let id = a.active;
        let mut b = a.clone();
        let plane = PlaneId { layer: id.0, mask: false };
        let stroke = |x: usize, rgb: [f32; 3]| {
            let mut px = vec![[0.0f32; 4]; TILE_PIXELS];
            for y in 0..64 {
                px[y * 64 + x] = [rgb[0], rgb[1], rgb[2], 1.0];
            }
            vec![(TileCoord::new(0, 0), stroke_tile_bytes(&px, None))]
        };
        let (red, blue) = (stroke(10, [1.0, 0.0, 0.0]), stroke(50, [0.0, 0.0, 1.0]));
        for (doc, first, second) in [(&mut a, &red, &blue), (&mut b, &blue, &red)] {
            assert!(apply_stroke(&mut doc.layers, plane, 1.0, false, BrushBlend::Normal, false, first));
            assert!(apply_stroke(&mut doc.layers, plane, 1.0, false, BrushBlend::Normal, false, second));
        }
        let px = |d: &Document, x: i32| d.find(id).unwrap().tiles().unwrap().pixel(x, 5);
        assert_eq!((px(&a, 10), px(&a, 50)), ([255, 0, 0, 255], [0, 0, 255, 255]));
        assert_eq!((px(&b, 10), px(&b, 50)), (px(&a, 10), px(&a, 50)));
    }

    #[test]
    fn diffs_find_tiles_and_layer_changes() {
        let base = Document::new(128, 128);
        let mut doc = base.clone();
        let id = doc.active;
        let plane = PlaneId { layer: id.0, mask: false };
        let msg = {
            let store = plane_mut(&mut doc.layers, plane).unwrap();
            let mut t = Tile::empty();
            t.bytes_mut().fill(200);
            store.insert(TileCoord::new(1, 1), t);
            tiles_message(0, &doc, plane, &[TileCoord::new(1, 1)]).unwrap()
        };
        let (structure, tiles) = diff(&base, &doc);
        assert!(!structure);
        assert_eq!(tiles, vec![(plane, vec![TileCoord::new(1, 1)])]);
        // The other side applies it and matches.
        let mut other = base.clone();
        let Message::Tiles { plane, tiles, .. } = msg else { panic!() };
        apply_tiles(&mut other.layers, plane, &tiles);
        assert!(diff(&other, &doc).1.is_empty());
        // A renamed layer is a structure change; applying it keeps the pixels.
        doc.find_mut(id).unwrap().name = "Inks".into();
        assert!(diff(&other, &doc).0);
        crate::project::apply_structure(&mut other, &crate::project::structure(&doc)).unwrap();
        assert_eq!(other.find(id).unwrap().name, "Inks");
        assert_eq!(other.find(id).unwrap().tiles().unwrap().pixel(70, 70)[0], 200);
    }
}
