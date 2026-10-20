//! Photoshop PSD import/export (8-bit RGB).
//!
//! Written from the published file format: header, empty colour-mode and
//! image-resource sections, the layer & mask section (layer records, channel
//! data, group dividers via `lsct`, Unicode names via `luni`) and a flattened
//! composite. Channel data is PackBits (RLE) compressed.
//!
//! Mapping:
//! - raster and vector layers → pixel layers (vectors are rasterised);
//! - fill, gradient and tone layers → pixel layers of their rendered content;
//! - folders → groups (`pass` blend key for pass-through);
//! - masks → user masks (PSD stores visibility, we store the hidden amount);
//! - opacity, visibility, clipping and the 13 blend modes map directly.
//!
//! Reading accepts 8-bit RGB files with raw or RLE channels; other depths,
//! colour modes and ZIP-compressed channels are rejected with an error.
//! Adjustment and other special layers are skipped.

use std::fmt;
use std::path::Path;

use crate::blend::{premultiply, unpremultiply, BlendMode};
use crate::document::{Document, MAX_CANVAS_SIZE};
use crate::io::{flatten, render_procedural};
use crate::layer::{Folder, Layer, LayerKind, Mask};
use crate::tile::{Tile, TileCoord, TileStore, TILE_SIZE};

#[derive(Debug)]
pub enum PsdError {
    Io(std::io::Error),
    Unsupported(String),
    Corrupt(&'static str),
}

impl fmt::Display for PsdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PsdError::Io(e) => write!(f, "{e}"),
            PsdError::Unsupported(what) => write!(f, "unsupported PSD: {what}"),
            PsdError::Corrupt(what) => write!(f, "corrupt PSD: {what}"),
        }
    }
}

impl std::error::Error for PsdError {}

impl From<std::io::Error> for PsdError {
    fn from(e: std::io::Error) -> Self {
        PsdError::Io(e)
    }
}

fn blend_key(mode: BlendMode) -> &'static [u8; 4] {
    match mode {
        BlendMode::Normal => b"norm",
        BlendMode::Darken => b"dark",
        BlendMode::Multiply => b"mul ",
        BlendMode::ColorBurn => b"idiv",
        BlendMode::LinearBurn => b"lbrn",
        BlendMode::Lighten => b"lite",
        BlendMode::Screen => b"scrn",
        BlendMode::ColorDodge => b"div ",
        BlendMode::Add => b"lddg",
        BlendMode::Overlay => b"over",
        BlendMode::SoftLight => b"sLit",
        BlendMode::HardLight => b"hLit",
        BlendMode::Difference => b"diff",
    }
}

fn blend_from_key(key: &[u8]) -> BlendMode {
    BlendMode::ALL.into_iter().find(|m| blend_key(*m) == key).unwrap_or_default()
}

// ---- PackBits ----------------------------------------------------------------

pub fn packbits_encode(row: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < row.len() {
        // Run of identical bytes?
        let mut run = 1;
        while i + run < row.len() && run < 128 && row[i + run] == row[i] {
            run += 1;
        }
        if run >= 3 {
            out.push((257 - run) as u8);
            out.push(row[i]);
            i += run;
            continue;
        }
        // Literal span until the next run of 3+.
        let start = i;
        while i < row.len() && i - start < 128 {
            if i + 2 < row.len() && row[i] == row[i + 1] && row[i] == row[i + 2] {
                break;
            }
            i += 1;
        }
        out.push((i - start - 1) as u8);
        out.extend_from_slice(&row[start..i]);
    }
}

pub fn packbits_decode(data: &[u8], expected: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(expected);
    let mut i = 0;
    while i < data.len() && out.len() < expected {
        let n = data[i] as i8;
        i += 1;
        if n >= 0 {
            let len = n as usize + 1;
            out.extend_from_slice(data.get(i..i + len)?);
            i += len;
        } else if n != -128 {
            let len = (1 - n as isize) as usize;
            let byte = *data.get(i)?;
            out.extend(std::iter::repeat_n(byte, len));
            i += 1;
        }
    }
    (out.len() == expected).then_some(out)
}

/// Image resource holding the print resolution.
const RESOLUTION_INFO: u16 = 0x03ED;

/// Horizontal resolution in pixels per inch from the image resources, if
/// present (stored as 16.16 fixed point, per inch or per centimetre).
fn resolution(mut resources: &[u8]) -> Option<u32> {
    while resources.len() >= 12 && &resources[..4] == b"8BIM" {
        let id = u16::from_be_bytes([resources[4], resources[5]]);
        let name_len = resources[6] as usize;
        let name_total = (1 + name_len).next_multiple_of(2);
        let at = 6 + name_total;
        let size = u32::from_be_bytes(resources.get(at..at + 4)?.try_into().ok()?) as usize;
        let data = resources.get(at + 4..at + 4 + size)?;
        if id == RESOLUTION_INFO && size >= 16 {
            let fixed = u32::from_be_bytes(data[..4].try_into().ok()?);
            let unit = u16::from_be_bytes([data[4], data[5]]);
            let per_unit = fixed as f64 / 65536.0;
            // Unit 2: pixels per centimetre.
            let dpi = if unit == 2 { per_unit * 2.54 } else { per_unit }.round() as u32;
            return (dpi > 0).then_some(dpi);
        }
        resources = resources.get(at + 4 + size.next_multiple_of(2)..)?;
    }
    None
}

// ---- writing -----------------------------------------------------------------

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn i16(&mut self, v: i16) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
    /// Writes a `u32` length placeholder; returns its position.
    fn begin_len(&mut self) -> usize {
        self.0.extend_from_slice(&[0; 4]);
        self.0.len() - 4
    }
    fn end_len(&mut self, at: usize) {
        let len = (self.0.len() - at - 4) as u32;
        self.0[at..at + 4].copy_from_slice(&len.to_be_bytes());
    }
    fn pad_to(&mut self, start: usize, multiple: usize) {
        while !(self.0.len() - start).is_multiple_of(multiple) {
            self.0.push(0);
        }
    }
}

/// One channel's planar pixels within a rectangle.
struct Channel {
    id: i16,
    data: Vec<u8>,
}

struct LayerRecord {
    name: String,
    rect: [i32; 4], // top, left, bottom, right
    channels: Vec<Channel>,
    blend: [u8; 4],
    opacity: u8,
    clipping: bool,
    visible: bool,
    /// (rect, default colour, planar data)
    mask: Option<([i32; 4], u8, Vec<u8>)>,
    /// `lsct` section type: 1 open folder, 3 bounding divider.
    section: Option<u32>,
}

/// Packed RGBA8 premultiplied canvas → straight-alpha planar channels in `rect`.
fn channels_from_rgba(pixels: &[u8], canvas: (u32, u32), rect: [i32; 4]) -> Vec<Channel> {
    let [top, left, bottom, right] = rect;
    let (w, h) = ((right - left) as usize, (bottom - top) as usize);
    let mut planes = vec![vec![0u8; w * h]; 4];
    for y in 0..h {
        for x in 0..w {
            let i = ((top as usize + y) * canvas.0 as usize + left as usize + x) * 4;
            let px = unpremultiply([pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]);
            for c in 0..4 {
                planes[c][y * w + x] = px[c];
            }
        }
    }
    let [r, g, b, a] = <[Vec<u8>; 4]>::try_from(planes).expect("four planes");
    vec![Channel { id: -1, data: a }, Channel { id: 0, data: r }, Channel { id: 1, data: g }, Channel { id: 2, data: b }]
}

/// Tight pixel bounds `[top, left, bottom, right)` of non-transparent pixels, clipped to the canvas.
fn tile_bounds(tiles: &TileStore, canvas: (u32, u32)) -> Option<[i32; 4]> {
    let mut b: Option<[i32; 4]> = None;
    for (coord, tile) in tiles.iter() {
        let (ox, oy) = coord.origin();
        for (i, px) in tile.bytes().as_chunks::<4>().0.iter().enumerate() {
            if px[3] == 0 {
                continue;
            }
            let (x, y) = (ox + (i as u32 % TILE_SIZE) as i32, oy + (i as u32 / TILE_SIZE) as i32);
            if x < 0 || y < 0 || x >= canvas.0 as i32 || y >= canvas.1 as i32 {
                continue;
            }
            b = Some(b.map_or([y, x, y + 1, x + 1], |b| [b[0].min(y), b[1].min(x), b[2].max(y + 1), b[3].max(x + 1)]));
        }
    }
    b
}

fn tiles_to_rgba(tiles: &TileStore, canvas: (u32, u32)) -> Vec<u8> {
    let mut out = vec![0u8; (canvas.0 * canvas.1 * 4) as usize];
    for (coord, tile) in tiles.iter() {
        let (ox, oy) = coord.origin();
        for ty in 0..TILE_SIZE as i32 {
            let y = oy + ty;
            if y < 0 || y >= canvas.1 as i32 {
                continue;
            }
            for tx in 0..TILE_SIZE as i32 {
                let x = ox + tx;
                if x < 0 || x >= canvas.0 as i32 {
                    continue;
                }
                let i = ((y as u32 * canvas.0 + x as u32) * 4) as usize;
                out[i..i + 4].copy_from_slice(&tile.pixel(tx as u32, ty as u32));
            }
        }
    }
    out
}

fn mask_record(mask: &Mask, canvas: (u32, u32)) -> Option<([i32; 4], u8, Vec<u8>)> {
    let rect = tile_bounds(&mask.tiles, canvas)?;
    let [top, left, bottom, right] = rect;
    let w = (right - left) as usize;
    let mut data = vec![255u8; w * (bottom - top) as usize];
    for y in top..bottom {
        for x in left..right {
            data[(y - top) as usize * w + (x - left) as usize] = 255 - mask.tiles.pixel(x, y)[3];
        }
    }
    Some((rect, 255, data))
}

fn collect_records(layers: &[Layer], canvas: (u32, u32), out: &mut Vec<LayerRecord>) {
    for layer in layers {
        let mut record = LayerRecord {
            name: layer.name.clone(),
            rect: [0; 4],
            channels: Vec::new(),
            blend: *blend_key(layer.blend),
            opacity: (layer.opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
            clipping: layer.clip,
            visible: layer.visible,
            mask: layer.mask.as_ref().and_then(|m| mask_record(m, canvas)),
            section: None,
        };
        match &layer.kind {
            LayerKind::Folder(folder) => {
                // Bottom-up: divider, children, then the folder header.
                out.push(LayerRecord {
                    name: "</Layer group>".into(),
                    rect: [0; 4],
                    channels: empty_channels(),
                    blend: *b"norm",
                    opacity: 255,
                    clipping: false,
                    visible: true,
                    mask: None,
                    section: Some(3),
                });
                collect_records(&folder.children, canvas, out);
                record.channels = empty_channels();
                record.section = Some(1);
                if folder.pass_through {
                    record.blend = *b"pass";
                }
                out.push(record);
            }
            // Corrections have no simple PSD form: left out of the layers (the merged image shows them).
            LayerKind::Adjustment(_) => {}
            kind => {
                let pixels = match kind {
                    LayerKind::Raster(tiles) => tiles_to_rgba(tiles, canvas),
                    LayerKind::Vector(v) => tiles_to_rgba(&v.cache, canvas),
                    _ => render_procedural(layer, canvas).expect("procedural layer"),
                };
                let bounds = match kind {
                    LayerKind::Raster(tiles) => tile_bounds(tiles, canvas),
                    LayerKind::Vector(v) => tile_bounds(&v.cache, canvas),
                    _ => Some([0, 0, canvas.1 as i32, canvas.0 as i32]),
                };
                match bounds {
                    Some(rect) => {
                        record.rect = rect;
                        record.channels = channels_from_rgba(&pixels, canvas, rect);
                    }
                    None => record.channels = empty_channels(),
                }
                out.push(record);
            }
        }
    }
}

fn empty_channels() -> Vec<Channel> {
    [-1, 0, 1, 2].into_iter().map(|id| Channel { id, data: Vec::new() }).collect()
}

/// RLE-compresses planar rows: compression tag, row byte counts, data.
fn rle_channel(data: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut counts = Vec::with_capacity(height * 2);
    let mut body = Vec::new();
    for row in data.chunks(width.max(1)).take(height) {
        let before = body.len();
        packbits_encode(row, &mut body);
        counts.extend_from_slice(&((body.len() - before) as u16).to_be_bytes());
    }
    let mut out = Vec::with_capacity(2 + counts.len() + body.len());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&counts);
    out.extend_from_slice(&body);
    out
}

fn pascal_name(name: &str) -> Vec<u8> {
    let ascii: Vec<u8> = name.chars().map(|c| if c.is_ascii() && !c.is_ascii_control() { c as u8 } else { b'_' }).take(255).collect();
    let mut out = vec![ascii.len() as u8];
    out.extend_from_slice(&ascii);
    while out.len() % 4 != 0 {
        out.push(0);
    }
    out
}

pub fn write(doc: &Document) -> Vec<u8> {
    let canvas = doc.canvas();
    let mut w = Writer(Vec::new());
    // Header
    w.bytes(b"8BPS");
    w.u16(1);
    w.bytes(&[0; 6]);
    w.u16(4); // R, G, B + merged alpha
    w.u32(canvas.1);
    w.u32(canvas.0);
    w.u16(8);
    w.u16(3); // RGB
    w.u32(0); // colour mode data
    // Image resources: just the resolution.
    let resources = w.begin_len();
    w.bytes(b"8BIM");
    w.u16(RESOLUTION_INFO);
    w.u16(0); // empty name, padded to even
    w.u32(16);
    for _ in 0..2 {
        w.u32(doc.dpi.min(0x7fff) << 16); // 16.16 fixed pixels per inch
        w.u16(1); // shown as pixels per inch
        w.u16(1); // size shown in inches
    }
    w.end_len(resources);

    // Layer and mask information
    let mut records = Vec::new();
    collect_records(&doc.layers, canvas, &mut records);
    let section = w.begin_len();
    let info = w.begin_len();
    // Negative count: the first alpha channel holds the merged transparency.
    w.i16(-(records.len() as i16));
    let mut channel_blobs: Vec<Vec<u8>> = Vec::new();
    for record in &records {
        let [top, left, bottom, right] = record.rect;
        let (rw, rh) = ((right - left) as usize, (bottom - top) as usize);
        w.i32(top);
        w.i32(left);
        w.i32(bottom);
        w.i32(right);
        let channel_count = record.channels.len() + usize::from(record.mask.is_some());
        w.u16(channel_count as u16);
        let mut blobs = Vec::new();
        for ch in &record.channels {
            let blob = if rw * rh == 0 { 0u16.to_be_bytes().to_vec() } else { rle_channel(&ch.data, rw, rh) };
            w.i16(ch.id);
            w.u32(blob.len() as u32);
            blobs.push(blob);
        }
        if let Some((mrect, _, data)) = &record.mask {
            let blob = rle_channel(data, (mrect[3] - mrect[1]) as usize, (mrect[2] - mrect[0]) as usize);
            w.i16(-2);
            w.u32(blob.len() as u32);
            blobs.push(blob);
        }
        channel_blobs.extend(blobs);
        w.bytes(b"8BIM");
        w.bytes(&record.blend);
        w.u8(record.opacity);
        w.u8(u8::from(record.clipping));
        w.u8(if record.visible { 0 } else { 0x02 });
        w.u8(0);
        let extra = w.begin_len();
        // Layer mask data
        match &record.mask {
            Some((mrect, default, _)) => {
                w.u32(20);
                for v in mrect {
                    w.i32(*v);
                }
                w.u8(*default);
                w.u8(0);
                w.u16(0);
            }
            None => w.u32(0),
        }
        w.u32(0); // blending ranges
        w.bytes(&pascal_name(&record.name));
        // Unicode name
        w.bytes(b"8BIMluni");
        let luni = w.begin_len();
        let utf16: Vec<u16> = record.name.encode_utf16().collect();
        w.u32(utf16.len() as u32);
        for c in utf16 {
            w.u16(c);
        }
        w.pad_to(luni + 4, 4);
        w.end_len(luni);
        if let Some(kind) = record.section {
            w.bytes(b"8BIMlsct");
            w.u32(12);
            w.u32(kind);
            w.bytes(b"8BIM");
            w.bytes(&record.blend);
        }
        w.end_len(extra);
    }
    for blob in channel_blobs {
        w.bytes(&blob);
    }
    w.pad_to(info + 4, 2);
    w.end_len(info);
    w.u32(0); // global mask info
    w.end_len(section);

    // Composite image data (R, G, B, A planes), RLE.
    let flat = flatten(doc);
    let (cw, ch) = (canvas.0 as usize, canvas.1 as usize);
    let planes: Vec<Vec<u8>> = (0..4)
        .map(|c| {
            flat.as_chunks::<4>().0.iter().map(|px| unpremultiply(*px)[c]).collect()
        })
        .collect();
    w.u16(1);
    let mut bodies = Vec::new();
    for plane in &planes {
        for row in plane.chunks(cw) {
            let mut body = Vec::new();
            packbits_encode(row, &mut body);
            w.u16(body.len() as u16);
            bodies.push(body);
        }
    }
    debug_assert_eq!(bodies.len(), ch * 4);
    for body in bodies {
        w.bytes(&body);
    }
    w.0
}

pub fn save(doc: &Document, path: &Path) -> Result<(), PsdError> {
    std::fs::write(path, write(doc))?;
    Ok(())
}

// ---- reading -----------------------------------------------------------------

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], PsdError> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len()).ok_or(PsdError::Corrupt("unexpected end of file"))?;
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }
    fn u8(&mut self) -> Result<u8, PsdError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, PsdError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn i16(&mut self) -> Result<i16, PsdError> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, PsdError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, PsdError> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
}

struct ReadRecord {
    name: String,
    rect: [i32; 4],
    channels: Vec<(i16, u32)>,
    blend: [u8; 4],
    opacity: u8,
    clipping: bool,
    visible: bool,
    mask: Option<([i32; 4], u8)>,
    section: Option<(u32, Option<[u8; 4]>)>,
}

/// Decodes one channel of `width × height`; returns `None` for an empty rect.
fn read_channel(r: &mut Reader<'_>, len: u32, width: usize, height: usize) -> Result<Option<Vec<u8>>, PsdError> {
    let blob = r.take(len as usize)?;
    if width * height == 0 || blob.len() < 2 {
        return Ok(None);
    }
    let compression = u16::from_be_bytes([blob[0], blob[1]]);
    let body = &blob[2..];
    match compression {
        0 => body.get(..width * height).map(|b| Some(b.to_vec())).ok_or(PsdError::Corrupt("short raw channel")),
        1 => {
            let counts_len = height * 2;
            let counts = body.get(..counts_len).ok_or(PsdError::Corrupt("missing RLE row counts"))?;
            let mut pos = counts_len;
            let mut out = Vec::with_capacity(width * height);
            for row in counts.as_chunks::<2>().0 {
                let n = u16::from_be_bytes([row[0], row[1]]) as usize;
                let packed = body.get(pos..pos + n).ok_or(PsdError::Corrupt("short RLE row"))?;
                out.extend(packbits_decode(packed, width).ok_or(PsdError::Corrupt("bad RLE row"))?);
                pos += n;
            }
            Ok(Some(out))
        }
        other => Err(PsdError::Unsupported(format!("channel compression {other} (only raw and RLE)"))),
    }
}

pub fn read(data: &[u8]) -> Result<Document, PsdError> {
    let mut r = Reader { data, pos: 0 };
    if r.take(4)? != b"8BPS" {
        return Err(PsdError::Corrupt("missing 8BPS signature"));
    }
    let version = r.u16()?;
    if version != 1 {
        return Err(PsdError::Unsupported("large document format (PSB)".into()));
    }
    r.take(6)?;
    let channel_count = r.u16()?;
    let height = r.u32()?;
    let width = r.u32()?;
    let depth = r.u16()?;
    let mode = r.u16()?;
    if depth != 8 || mode != 3 {
        return Err(PsdError::Unsupported(format!("{depth}-bit colour mode {mode} (only 8-bit RGB)")));
    }
    if width == 0 || height == 0 || width > MAX_CANVAS_SIZE || height > MAX_CANVAS_SIZE {
        return Err(PsdError::Unsupported(format!("{width}×{height} canvas (max {MAX_CANVAS_SIZE})")));
    }
    let canvas = (width, height);
    let skip = r.u32()? as usize;
    r.take(skip)?; // colour mode data
    let skip = r.u32()? as usize;
    let dpi = resolution(r.take(skip)?);

    let section_len = r.u32()? as usize;
    let section_end = r.pos + section_len;
    let mut doc = Document::new(width, height);
    doc.layers.clear();
    if let Some(dpi) = dpi {
        doc.dpi = dpi;
    }
    let mut layers: Vec<Layer> = Vec::new();
    if section_len > 0 {
        let info_len = r.u32()? as usize;
        if info_len > 0 {
            layers = read_layers(&mut r, canvas, &mut doc)?;
        }
    }
    r.pos = section_end;

    if layers.is_empty() {
        // Flat file: read the composite into one layer.
        let compression = r.u16()?;
        let (w, h) = (width as usize, height as usize);
        let channels = channel_count.min(4) as usize;
        let mut planes = Vec::new();
        match compression {
            0 => {
                for _ in 0..channel_count {
                    planes.push(r.take(w * h)?.to_vec());
                }
            }
            1 => {
                let counts: Vec<usize> =
                    (0..channel_count as usize * h).map(|_| r.u16().map(|v| v as usize)).collect::<Result<_, _>>()?;
                for c in 0..channel_count as usize {
                    let mut plane = Vec::with_capacity(w * h);
                    for row in 0..h {
                        let packed = r.take(counts[c * h + row])?;
                        plane.extend(packbits_decode(packed, w).ok_or(PsdError::Corrupt("bad RLE row"))?);
                    }
                    planes.push(plane);
                }
            }
            other => return Err(PsdError::Unsupported(format!("composite compression {other}"))),
        }
        let mut rgba = vec![0u8; w * h * 4];
        for i in 0..w * h {
            let a = if channels >= 4 { planes[3][i] } else { 255 };
            let px = premultiply([planes[0][i], planes[1][i], planes[2][i], a]);
            rgba[i * 4..i * 4 + 4].copy_from_slice(&px);
        }
        let id = doc.alloc_id();
        layers.push(Layer::raster_with(id, "Background", TileStore::from_rgba(width, height, &rgba)));
    }

    let active = layers.iter().rev().find(|l| l.has_pixels()).map_or(layers[0].id, |l| l.id);
    doc.layers = layers;
    doc.active = active;
    Ok(doc)
}

fn read_layers(r: &mut Reader<'_>, canvas: (u32, u32), doc: &mut Document) -> Result<Vec<Layer>, PsdError> {
    let count = r.i16()?.unsigned_abs() as usize;
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let rect = [r.i32()?, r.i32()?, r.i32()?, r.i32()?];
        let n = r.u16()? as usize;
        let channels = (0..n).map(|_| Ok((r.i16()?, r.u32()?))).collect::<Result<Vec<_>, PsdError>>()?;
        if r.take(4)? != b"8BIM" {
            return Err(PsdError::Corrupt("bad blend mode signature"));
        }
        let blend: [u8; 4] = r.take(4)?.try_into().unwrap();
        let opacity = r.u8()?;
        let clipping = r.u8()? != 0;
        let flags = r.u8()?;
        r.u8()?;
        let extra_len = r.u32()? as usize;
        let extra_end = r.pos + extra_len;
        let mask_len = r.u32()? as usize;
        let mask = if mask_len >= 18 {
            let mrect = [r.i32()?, r.i32()?, r.i32()?, r.i32()?];
            let default = r.u8()?;
            r.take(mask_len - 17)?;
            Some((mrect, default))
        } else {
            r.take(mask_len)?;
            None
        };
        let ranges = r.u32()? as usize;
        r.take(ranges)?;
        let name_len = r.u8()? as usize;
        let mut name = String::from_utf8_lossy(r.take(name_len)?).into_owned();
        let padded = (1 + name_len).div_ceil(4) * 4;
        r.take(padded - 1 - name_len)?;
        let mut section = None;
        while r.pos + 12 <= extra_end {
            let sig = r.take(4)?;
            if sig != b"8BIM" && sig != b"8B64" {
                break;
            }
            let key: [u8; 4] = r.take(4)?.try_into().unwrap();
            let len = r.u32()? as usize;
            let start = r.pos;
            match &key {
                b"luni" => {
                    let chars = r.u32()? as usize;
                    let units = (0..chars).map(|_| r.u16()).collect::<Result<Vec<_>, _>>()?;
                    name = String::from_utf16_lossy(&units);
                }
                b"lsct" | b"lsdk" => {
                    let kind = r.u32()?;
                    let blend = if len >= 12 {
                        r.take(4)?;
                        Some(r.take(4)?.try_into().unwrap())
                    } else {
                        None
                    };
                    section = Some((kind, blend));
                }
                _ => {}
            }
            r.pos = start + len;
        }
        r.pos = extra_end;
        records.push(ReadRecord {
            name,
            rect,
            channels,
            blend,
            opacity,
            clipping,
            visible: flags & 0x02 == 0,
            mask,
            section,
        });
    }

    // Channel image data follows the records, in record order.
    let mut stack: Vec<Vec<Layer>> = vec![Vec::new()];
    for record in records {
        let [top, left, bottom, right] = record.rect;
        let (w, h) = ((right - left).max(0) as usize, (bottom - top).max(0) as usize);
        let mut planes: [Option<Vec<u8>>; 4] = [None, None, None, None];
        let mut mask_plane = None;
        for &(id, len) in &record.channels {
            match id {
                -1..=2 => planes[(id + 1) as usize] = read_channel(r, len, w, h)?,
                -2 => {
                    let (mrect, _) = record.mask.unwrap_or(([0; 4], 255));
                    let (mw, mh) = ((mrect[3] - mrect[1]).max(0) as usize, (mrect[2] - mrect[0]).max(0) as usize);
                    mask_plane = read_channel(r, len, mw, mh)?;
                }
                _ => {
                    r.take(len as usize)?;
                }
            }
        }

        let id = doc.alloc_id();
        match record.section {
            Some((3, _)) => {
                stack.push(Vec::new());
                continue;
            }
            Some((1 | 2, blend)) => {
                let children = if stack.len() > 1 { stack.pop().unwrap() } else { Vec::new() };
                let key = blend.unwrap_or(record.blend);
                let pass_through = &key == b"pass";
                let mut layer = Layer::folder(id, record.name);
                if let LayerKind::Folder(f) = &mut layer.kind {
                    *f = Folder { children, pass_through };
                }
                apply_common(&mut layer, &record.blend, record.opacity, record.clipping, record.visible);
                if pass_through {
                    layer.blend = BlendMode::Normal;
                }
                layer.mask = build_mask(record.mask, mask_plane, canvas);
                stack.last_mut().expect("root list").push(layer);
                continue;
            }
            _ => {}
        }
        let mut tiles = TileStore::new();
        if w * h > 0 {
            let get = |c: usize, i: usize| planes[c].as_ref().map_or(if c == 0 { 255 } else { 0 }, |p| p[i]);
            for y in 0..h {
                for x in 0..w {
                    let (cx, cy) = (left + x as i32, top + y as i32);
                    if cx < 0 || cy < 0 || cx >= canvas.0 as i32 || cy >= canvas.1 as i32 {
                        continue;
                    }
                    let i = y * w + x;
                    let px = premultiply([get(1, i), get(2, i), get(3, i), get(0, i)]);
                    if px[3] == 0 {
                        continue;
                    }
                    let coord = TileCoord::containing(cx, cy);
                    let (ox, oy) = coord.origin();
                    tiles.get_or_create_mut(coord).set_pixel((cx - ox) as u32, (cy - oy) as u32, px);
                }
            }
        }
        let mut layer = Layer::raster_with(id, record.name, tiles);
        apply_common(&mut layer, &record.blend, record.opacity, record.clipping, record.visible);
        layer.mask = build_mask(record.mask, mask_plane, canvas);
        stack.last_mut().expect("root list").push(layer);
    }
    // Unbalanced groups: fold leftovers back into the root.
    while stack.len() > 1 {
        let inner = stack.pop().unwrap();
        stack.last_mut().unwrap().extend(inner);
    }
    Ok(stack.pop().unwrap())
}

fn apply_common(layer: &mut Layer, blend: &[u8; 4], opacity: u8, clipping: bool, visible: bool) {
    layer.blend = blend_from_key(blend);
    layer.opacity = opacity as f32 / 255.0;
    layer.clip = clipping;
    layer.visible = visible;
}

/// PSD masks store visibility; ours store the hidden amount.
fn build_mask(info: Option<([i32; 4], u8)>, plane: Option<Vec<u8>>, canvas: (u32, u32)) -> Option<Mask> {
    let (rect, default) = info?;
    let mut tiles = TileStore::new();
    let hidden_outside = 255 - default;
    if hidden_outside > 0 {
        for ty in 0..canvas.1.div_ceil(TILE_SIZE) {
            for tx in 0..canvas.0.div_ceil(TILE_SIZE) {
                let mut tile = Tile::empty();
                for px in tile.bytes_mut().as_chunks_mut::<4>().0 {
                    *px = [hidden_outside; 4];
                }
                tiles.insert(TileCoord::new(tx as i32, ty as i32), tile);
            }
        }
    }
    if let Some(plane) = plane {
        let [top, left, bottom, right] = rect;
        let w = (right - left) as usize;
        for y in top..bottom {
            for x in left..right {
                if x < 0 || y < 0 || x >= canvas.0 as i32 || y >= canvas.1 as i32 {
                    continue;
                }
                let hide = 255 - plane[(y - top) as usize * w + (x - left) as usize];
                let coord = TileCoord::containing(x, y);
                let (ox, oy) = coord.origin();
                tiles.get_or_create_mut(coord).set_pixel((x - ox) as u32, (y - oy) as u32, [hide; 4]);
            }
        }
    }
    Some(Mask { tiles, enabled: true })
}

pub fn load(path: &Path) -> Result<Document, PsdError> {
    read(&std::fs::read(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::BrushSettings;
    use crate::layer::LayerId;
    use crate::raster::paint_stroke;

    fn layer_names(doc: &Document) -> Vec<String> {
        doc.rows_top_down().into_iter().filter_map(|(id, _)| doc.find(id).map(|l| l.name.clone())).collect()
    }

    #[test]
    fn packbits_round_trip() {
        let rows: Vec<Vec<u8>> = vec![
            vec![],
            vec![7],
            vec![1, 1, 1, 1, 2, 3, 4, 4, 4, 5],
            (0..=255).collect(),
            vec![9; 300],
            (0..500).map(|i| (i / 3 % 7) as u8).collect(),
        ];
        for row in rows {
            let mut packed = Vec::new();
            packbits_encode(&row, &mut packed);
            assert_eq!(packbits_decode(&packed, row.len()).unwrap(), row);
        }
    }

    #[test]
    fn round_trip_keeps_structure_and_pixels() {
        let mut doc = Document::new(90, 60);
        let canvas = doc.canvas();
        let base = doc.active;
        let dabs: Vec<_> = (0..30).map(|i| crate::brush::Dab { x: 5.0 + i as f32 * 2.5, y: 30.0, radius: 6.0, alpha: 1.0, angle: 0.0, thickness: 1.0, mirror: false }).collect();
        paint_stroke(doc.find_mut(base).unwrap().tiles_mut().unwrap(), &dabs, [0.8, 0.3, 0.1], &BrushSettings::g_pen(), canvas, false, None);
        let mut mask = Mask { enabled: true, ..Mask::default() };
        paint_stroke(&mut mask.tiles, &dabs[..10], [1.0; 3], &BrushSettings::g_pen(), canvas, false, None);
        doc.find_mut(base).unwrap().mask = Some(mask);

        let folder = doc.alloc_id();
        let mut group = Layer::folder(folder, "Grüppe");
        group.opacity = 0.5;
        if let LayerKind::Folder(f) = &mut group.kind {
            f.pass_through = false;
            let mut inner = Layer::raster(LayerId(77), "Inner");
            inner.blend = BlendMode::Screen;
            inner.clip = true;
            inner.visible = false;
            paint_stroke(inner.tiles_mut().unwrap(), &dabs, [0.1, 0.9, 0.2], &BrushSettings::airbrush(), canvas, false, None);
            f.children.push(inner);
        }
        doc.layers.push(group);

        doc.dpi = 600;
        let adjust = doc.alloc_id();
        doc.layers.push(Layer::adjustment(adjust, crate::adjust::Adjustment::Invert));
        let back = read(&write(&doc)).unwrap();
        doc.layers.retain(|l| l.id != adjust);
        assert_eq!(back.dpi, 600, "resolution resource");
        assert_eq!(layer_names(&back), layer_names(&doc));
        let inner = back.find(back.rows_top_down()[1].0).unwrap();
        assert_eq!((inner.blend, inner.clip, inner.visible), (BlendMode::Screen, true, false));
        let group = back.find(back.rows_top_down()[0].0).unwrap();
        assert!(matches!(&group.kind, LayerKind::Folder(f) if !f.pass_through));
        assert!((group.opacity - 0.5).abs() < 0.01);
        // Pixels survive up to premultiply rounding.
        let (a, b) = (flatten(&doc), flatten(&back));
        assert!(a.iter().zip(&b).all(|(x, y)| x.abs_diff(*y) <= 2));
    }

    #[test]
    fn flat_psd_reads_composite() {
        let mut doc = Document::new(8, 4);
        doc.layers.truncate(1); // paper only
        let mut bytes = write(&doc);
        // Drop the layer section: rewrite as a file with an empty layer section.
        // Header, colour mode data (empty), then the image resources.
        let resources = u32::from_be_bytes(bytes[30..34].try_into().unwrap()) as usize;
        let header = 26 + 4 + 4 + resources;
        let section_len = u32::from_be_bytes(bytes[header..header + 4].try_into().unwrap()) as usize;
        bytes.splice(header..header + 4 + section_len, 0u32.to_be_bytes());
        let flat = read(&bytes).unwrap();
        assert_eq!(flat.layers.len(), 1);
        assert_eq!(flat.layers[0].tiles().unwrap().pixel(3, 2), [255, 255, 255, 255]);
    }
}
