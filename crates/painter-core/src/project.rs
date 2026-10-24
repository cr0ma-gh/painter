//! Native project format (`.painter`).
//!
//! A zip container:
//! - `document.json`: canvas size, layer tree and every layer property;
//! - `planes/*.tiles`: one entry per tiled plane (layer pixels, masks, the
//!   selection), zstd-compressed by the container. Each holds a tile count
//!   (`u32` LE) then per tile `x: i32, y: i32` (LE) and 64×64 premultiplied
//!   RGBA8 pixels.
//!
//! - `extras/<name>`: opaque data other crates attach to a project (the comic
//!   page of `comic-layout`, as JSON). Files without them load as before;
//!   older builds ignore them.
//!
//! Vector layers store their strokes; the raster cache is rebuilt on load.
//! Uniform tiles (e.g. the hidden outside of a panel mask) are shared again on
//! load, so they don't cost a tile of memory each.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{self, Cursor, Read, Seek, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::blend::BlendMode;
use crate::document::Document;
use crate::layer::{Folder, Layer, LayerId, LayerKind, Mask, VectorLayer, VectorStroke};
use crate::procedural::{Gradient, Tone};
use crate::rulers::RulerSet;
use crate::tile::{Tile, TileCoord, TileStore, TILE_BYTES};
use crate::vector;

pub const EXTENSION: &str = "painter";
const FORMAT_VERSION: u32 = 1;
const DOCUMENT_ENTRY: &str = "document.json";
/// The picture file managers show for the file.
pub const THUMBNAIL_ENTRY: &str = "thumbnail.png";
pub const THUMBNAIL_SIZE: u32 = 256;

/// A PNG thumbnail of `doc`, at most `max` pixels on a side (drafts left out).
pub fn thumbnail(doc: &Document, max: u32) -> Result<Vec<u8>, String> {
    crate::interchange::thumbnail_png(doc, max)
}

/// The thumbnail (PNG bytes) a Painter project, Krita or OpenRaster file
/// carries: what file managers show for it.
pub fn embedded_thumbnail(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut zip = ZipArchive::new(Cursor::new(bytes)).ok()?;
    for name in [THUMBNAIL_ENTRY, "Thumbnails/thumbnail.png", "preview.png", "mergedimage.png"] {
        if let Ok(mut entry) = zip.by_name(name) {
            let mut out = Vec::new();
            if entry.read_to_end(&mut out).is_ok() && !out.is_empty() {
                return Some(out);
            }
        }
    }
    None
}
const EXTRAS_PREFIX: &str = "extras/";

/// Extra named payloads saved alongside the document (see the module docs).
pub type Extras = BTreeMap<String, Vec<u8>>;

#[derive(Debug)]
pub enum ProjectError {
    Io(io::Error),
    Zip(zip::result::ZipError),
    Json(serde_json::Error),
    Format(String),
}

impl fmt::Display for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProjectError::Io(e) => write!(f, "{e}"),
            ProjectError::Zip(e) => write!(f, "{e}"),
            ProjectError::Json(e) => write!(f, "invalid document.json: {e}"),
            ProjectError::Format(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for ProjectError {}

impl From<io::Error> for ProjectError {
    fn from(e: io::Error) -> Self {
        ProjectError::Io(e)
    }
}

impl From<zip::result::ZipError> for ProjectError {
    fn from(e: zip::result::ZipError) -> Self {
        ProjectError::Zip(e)
    }
}

impl From<serde_json::Error> for ProjectError {
    fn from(e: serde_json::Error) -> Self {
        ProjectError::Json(e)
    }
}

#[derive(Serialize, Deserialize)]
struct DocumentFile {
    format: u32,
    width: u32,
    height: u32,
    /// Absent in files from before resolutions were saved.
    #[serde(default = "default_dpi")]
    dpi: u32,
    #[serde(default)]
    rulers: RulerSet,
    active: u64,
    next_id: u64,
    layers: Vec<LayerFile>,
    selection: Option<String>,
    /// Saved selections: name and plane entry. Absent in older files.
    #[serde(default)]
    channels: Vec<(String, String)>,
    /// Colour mode; RGB in older files.
    #[serde(default)]
    mode: crate::color_mode::ColorMode,
    /// Colour profile; sRGB in older files.
    #[serde(default)]
    profile: crate::color_profile::ColorProfile,
    /// A CMYK document's press profile (its ICC in `CMYK_PROFILE_ENTRY`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cmyk: Option<CmykFile>,
    /// A Duotone document's inks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duotone: Option<crate::duotone::Duotone>,
    /// An Indexed Color document's colour table.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    palette: Vec<[u8; 3]>,
}

#[derive(Serialize, Deserialize)]
struct CmykFile {
    name: String,
    intent: crate::cmyk::CmykIntent,
    black_point: bool,
}

const CMYK_PROFILE_ENTRY: &str = "profiles/cmyk.icc";

fn default_dpi() -> u32 {
    crate::document::DEFAULT_DPI
}

#[derive(Serialize, Deserialize)]
struct LayerFile {
    id: u64,
    name: String,
    blend: BlendMode,
    opacity: f32,
    visible: bool,
    locked: bool,
    alpha_locked: bool,
    clip: bool,
    #[serde(default)]
    reference: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    layer_color: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_object: Option<crate::layer::FileObject>,
    mask: Option<MaskFile>,
    content: ContentFile,
}

#[derive(Serialize, Deserialize)]
struct MaskFile {
    enabled: bool,
    tiles: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentFile {
    Raster { tiles: String },
    Vector {
        strokes: Vec<VectorStroke>,
        /// Absent in files from before text layers.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        texts: Vec<crate::text::TextItem>,
    },
    Folder { pass_through: bool, children: Vec<LayerFile> },
    Fill { color: [u8; 3] },
    Gradient { gradient: Gradient },
    Tone { tone: Tone },
    Adjustment { adjustment: crate::adjust::Adjustment },
}

/// Saves via a temporary file, so a failed save never corrupts the previous one.
pub fn save(doc: &Document, path: &Path) -> Result<(), ProjectError> {
    save_with(doc, &Extras::new(), path)
}

/// `save` with extra payloads (names are plain file names, e.g. `comic.json`).
pub fn save_with(doc: &Document, extras: &Extras, path: &Path) -> Result<(), ProjectError> {
    let tmp = path.with_extension(format!("{EXTENSION}.tmp"));
    {
        let mut file = io::BufWriter::new(File::create(&tmp)?);
        write_to(doc, extras, &mut file)?;
        file.flush()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn load(path: &Path) -> Result<Document, ProjectError> {
    load_with(path).map(|(doc, _)| doc)
}

/// `load` plus the extra payloads the file carries.
pub fn load_with(path: &Path) -> Result<(Document, Extras), ProjectError> {
    read_from(io::BufReader::new(File::open(path)?))
}

pub fn to_bytes(doc: &Document) -> Result<Vec<u8>, ProjectError> {
    to_bytes_with(doc, &Extras::new())
}

pub fn to_bytes_with(doc: &Document, extras: &Extras) -> Result<Vec<u8>, ProjectError> {
    let mut out = Cursor::new(Vec::new());
    write_to(doc, extras, &mut out)?;
    Ok(out.into_inner())
}

pub fn from_bytes(bytes: &[u8]) -> Result<Document, ProjectError> {
    from_bytes_with(bytes).map(|(doc, _)| doc)
}

pub fn from_bytes_with(bytes: &[u8]) -> Result<(Document, Extras), ProjectError> {
    read_from(Cursor::new(bytes))
}

fn write_to<W: Write + Seek>(doc: &Document, extras: &Extras, writer: W) -> Result<(), ProjectError> {
    let mut zip = ZipWriter::new(writer);
    // First and uncompressed: file-manager thumbnailers (Windows, Linux, macOS
    // Quick Look) read it straight from the start of the file.
    if let Ok(thumb) = crate::interchange::thumbnail_png(doc, THUMBNAIL_SIZE) {
        zip.start_file(THUMBNAIL_ENTRY, SimpleFileOptions::default().compression_method(CompressionMethod::Stored))?;
        zip.write_all(&thumb)?;
    }
    let packed = SimpleFileOptions::default().compression_method(CompressionMethod::Zstd);
    let mut planes: Vec<(String, &TileStore)> = Vec::new();

    let layers = doc.layers.iter().map(|l| layer_file(l, &mut planes)).collect();
    let selection = doc.selection.as_ref().map(|s| {
        let name = "planes/selection.tiles".to_string();
        planes.push((name.clone(), s));
        name
    });
    let channels = doc
        .channels
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let name = format!("planes/channel-{i}.tiles");
            planes.push((name.clone(), &c.mask));
            (c.name.clone(), name)
        })
        .collect();
    let file = DocumentFile {
        format: FORMAT_VERSION,
        width: doc.width,
        height: doc.height,
        dpi: doc.dpi,
        rulers: doc.rulers.clone(),
        active: doc.active.0,
        next_id: doc.next_id(),
        layers,
        selection,
        channels,
        mode: doc.mode,
        profile: doc.profile,
        cmyk: doc.cmyk.as_ref().map(|c| CmykFile { name: c.name.clone(), intent: c.intent, black_point: c.black_point }),
        duotone: doc.duotone.clone(),
        palette: doc.palette.clone(),
    };
    zip.start_file(DOCUMENT_ENTRY, packed)?;
    serde_json::to_writer_pretty(&mut zip, &file)?;
    for (name, tiles) in planes {
        zip.start_file(name, packed)?;
        zip.write_all(&encode_tiles(tiles))?;
    }
    if let Some(c) = &doc.cmyk {
        zip.start_file(CMYK_PROFILE_ENTRY, packed)?;
        zip.write_all(&c.icc)?;
    }
    for (name, bytes) in extras {
        if name.is_empty() || name.contains(['/', '\\']) {
            return Err(ProjectError::Format(format!("invalid extra name {name:?}")));
        }
        zip.start_file(format!("{EXTRAS_PREFIX}{name}"), packed)?;
        zip.write_all(bytes)?;
    }
    zip.finish()?;
    Ok(())
}

fn layer_file<'a>(layer: &'a Layer, planes: &mut Vec<(String, &'a TileStore)>) -> LayerFile {
    let id = layer.id.0;
    let content = match &layer.kind {
        LayerKind::Raster(tiles) => {
            let name = format!("planes/layer-{id}.tiles");
            planes.push((name.clone(), tiles));
            ContentFile::Raster { tiles: name }
        }
        LayerKind::Vector(v) => ContentFile::Vector { strokes: v.strokes.clone(), texts: v.texts.clone() },
        LayerKind::Folder(f) => ContentFile::Folder {
            pass_through: f.pass_through,
            children: f.children.iter().map(|c| layer_file(c, planes)).collect(),
        },
        LayerKind::Fill { color } => ContentFile::Fill { color: *color },
        LayerKind::Gradient(g) => ContentFile::Gradient { gradient: *g },
        LayerKind::Tone(t) => ContentFile::Tone { tone: *t },
        LayerKind::Adjustment(a) => ContentFile::Adjustment { adjustment: a.clone() },
    };
    let mask = layer.mask.as_ref().map(|m| {
        let name = format!("planes/mask-{id}.tiles");
        planes.push((name.clone(), &m.tiles));
        MaskFile { enabled: m.enabled, tiles: name }
    });
    LayerFile {
        id,
        name: layer.name.clone(),
        blend: layer.blend,
        opacity: layer.opacity,
        visible: layer.visible,
        locked: layer.locked,
        alpha_locked: layer.alpha_locked,
        clip: layer.clip,
        reference: layer.reference,
        draft: layer.draft,
        layer_color: layer.layer_color,
        file_object: layer.file_object.clone(),
        mask,
        content,
    }
}

/// A document's layer tree and settings without its pixels: what a
/// collaboration session sends when layers change.
#[derive(Serialize, Deserialize)]
struct StructureFile {
    width: u32,
    height: u32,
    dpi: u32,
    #[serde(default)]
    mode: crate::color_mode::ColorMode,
    #[serde(default)]
    profile: crate::color_profile::ColorProfile,
    layers: Vec<LayerFile>,
}

/// The layer tree and settings of `doc`, as JSON (no pixels).
pub fn structure(doc: &Document) -> Vec<u8> {
    let mut planes = Vec::new();
    let file = StructureFile {
        width: doc.width,
        height: doc.height,
        dpi: doc.dpi,
        mode: doc.mode,
        profile: doc.profile,
        layers: doc.layers.iter().map(|l| layer_file(l, &mut planes)).collect(),
    };
    serde_json::to_vec(&file).unwrap_or_default()
}

/// Rebuilds `layers` from a [`structure`]: layers keep the pixels they have
/// here (by id), new ones start empty. Returns the canvas size, resolution,
/// mode and profile it carries.
pub fn apply_structure_to(
    layers: &mut Vec<Layer>,
    json: &[u8],
) -> Result<((u32, u32), u32, crate::color_mode::ColorMode, crate::color_profile::ColorProfile), ProjectError> {
    let file: StructureFile = serde_json::from_slice(json)?;
    if file.width == 0 || file.height == 0 || file.width > crate::MAX_CANVAS_SIZE || file.height > crate::MAX_CANVAS_SIZE {
        return Err(ProjectError::Format("structure: bad canvas size".into()));
    }
    // The planes here, by the entry names layer_file gives them.
    let mut here: std::collections::HashMap<String, TileStore> = std::collections::HashMap::new();
    fn collect(layers: &[Layer], out: &mut std::collections::HashMap<String, TileStore>) {
        for l in layers {
            if let LayerKind::Raster(t) = &l.kind {
                out.insert(format!("planes/layer-{}.tiles", l.id.0), t.clone());
            }
            if let Some(m) = &l.mask {
                out.insert(format!("planes/mask-{}.tiles", l.id.0), m.tiles.clone());
            }
            if let LayerKind::Folder(f) = &l.kind {
                collect(&f.children, out);
            }
        }
    }
    collect(layers, &mut here);
    let canvas = (file.width, file.height);
    let mut plane = |name: &str| Ok(here.remove(name).unwrap_or_default());
    let rebuilt = file.layers.into_iter().map(|l| read_layer(l, &mut plane, canvas)).collect::<Result<Vec<_>, _>>()?;
    *layers = rebuilt;
    Ok((canvas, file.dpi.clamp(1, 100_000), file.mode, file.profile))
}

/// [`apply_structure_to`] on a document: its layers, size, resolution,
/// mode and profile; the active layer stays when it still exists.
pub fn apply_structure(doc: &mut Document, json: &[u8]) -> Result<(), ProjectError> {
    let (canvas, dpi, mode, profile) = apply_structure_to(&mut doc.layers, json)?;
    (doc.width, doc.height) = canvas;
    doc.dpi = dpi;
    doc.mode = mode;
    doc.profile = profile;
    doc.reserve_ids();
    if doc.find(doc.active).is_none() {
        doc.active = doc.raster_layer_ids().last().copied().or_else(|| doc.layers.last().map(|l| l.id)).unwrap_or(doc.active);
    }
    Ok(())
}

fn read_from<R: Read + Seek>(reader: R) -> Result<(Document, Extras), ProjectError> {
    let mut zip = ZipArchive::new(reader)?;
    let file: DocumentFile = {
        let entry = zip.by_name(DOCUMENT_ENTRY)?;
        serde_json::from_reader(entry)?
    };
    if file.format > FORMAT_VERSION {
        return Err(ProjectError::Format(format!(
            "made by a newer version (format {}, this build reads up to {FORMAT_VERSION})",
            file.format
        )));
    }
    let canvas = (file.width, file.height);
    let mut layers = Vec::with_capacity(file.layers.len());
    for layer in file.layers {
        layers.push(read_layer(layer, &mut |name| read_plane(&mut zip, name), canvas)?);
    }
    let mut doc = Document::from_parts(file.width, file.height, layers, LayerId(file.active), file.next_id);
    doc.dpi = file.dpi.clamp(1, 100_000);
    doc.rulers = file.rulers;
    if let Some(name) = file.selection {
        doc.selection = Some(read_plane(&mut zip, &name)?);
    }
    doc.mode = file.mode;
    doc.profile = file.profile;
    doc.duotone = file.duotone;
    doc.palette = file.palette;
    if let Some(c) = file.cmyk {
        let mut icc = Vec::new();
        zip.by_name(CMYK_PROFILE_ENTRY)?.read_to_end(&mut icc)?;
        doc.cmyk = Some(crate::cmyk::CmykSetup { name: c.name, icc: std::sync::Arc::new(icc), intent: c.intent, black_point: c.black_point });
    }
    for (name, entry) in file.channels {
        doc.channels.push(crate::document::SavedChannel { name, mask: read_plane(&mut zip, &entry)? });
    }
    if doc.find(doc.active).is_none() {
        doc.active = doc.raster_layer_ids().last().copied().unwrap_or(doc.layers[doc.layers.len() - 1].id);
    }
    let mut extras = Extras::new();
    let names: Vec<String> = zip.file_names().filter(|n| n.starts_with(EXTRAS_PREFIX)).map(String::from).collect();
    for name in names {
        let mut bytes = Vec::new();
        zip.by_name(&name)?.read_to_end(&mut bytes)?;
        extras.insert(name[EXTRAS_PREFIX.len()..].to_string(), bytes);
    }
    Ok((doc, extras))
}

/// A layer from its description; `plane` gives the pixels of a plane by its entry name.
fn read_layer(file: LayerFile, plane: &mut dyn FnMut(&str) -> Result<TileStore, ProjectError>, canvas: (u32, u32)) -> Result<Layer, ProjectError> {
    let kind = match file.content {
        ContentFile::Raster { tiles } => LayerKind::Raster(plane(&tiles)?),
        ContentFile::Vector { strokes, texts } => {
            let mut layer = VectorLayer { strokes, texts, cache: TileStore::new() };
            vector::rasterize(&mut layer, canvas);
            LayerKind::Vector(layer)
        }
        ContentFile::Folder { pass_through, children } => {
            let children = children.into_iter().map(|c| read_layer(c, plane, canvas)).collect::<Result<_, _>>()?;
            LayerKind::Folder(Folder { children, pass_through })
        }
        ContentFile::Fill { color } => LayerKind::Fill { color },
        ContentFile::Gradient { gradient } => LayerKind::Gradient(gradient),
        ContentFile::Tone { tone } => LayerKind::Tone(tone),
        ContentFile::Adjustment { adjustment } => LayerKind::Adjustment(adjustment),
    };
    let mask = match file.mask {
        Some(m) => Some(Mask { enabled: m.enabled, tiles: plane(&m.tiles)? }),
        None => None,
    };
    Ok(Layer {
        id: LayerId(file.id),
        name: file.name,
        kind,
        blend: file.blend,
        opacity: file.opacity.clamp(0.0, 1.0),
        visible: file.visible,
        locked: file.locked,
        alpha_locked: file.alpha_locked,
        clip: file.clip,
        reference: file.reference,
        draft: file.draft,
        layer_color: file.layer_color,
        file_object: file.file_object,
        mask,
    })
}

fn read_plane<R: Read + Seek>(zip: &mut ZipArchive<R>, name: &str) -> Result<TileStore, ProjectError> {
    let mut bytes = Vec::new();
    zip.by_name(name)?.read_to_end(&mut bytes)?;
    decode_tiles(&bytes).ok_or_else(|| ProjectError::Format(format!("corrupt tile plane {name}")))
}

fn encode_tiles(tiles: &TileStore) -> Vec<u8> {
    let mut entries: Vec<_> = tiles.iter().collect();
    entries.sort_by_key(|(c, _)| *c);
    let mut out = Vec::with_capacity(4 + entries.len() * (8 + TILE_BYTES));
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (coord, tile) in entries {
        out.extend_from_slice(&coord.x.to_le_bytes());
        out.extend_from_slice(&coord.y.to_le_bytes());
        out.extend_from_slice(tile.bytes());
    }
    out
}

fn decode_tiles(bytes: &[u8]) -> Option<TileStore> {
    let count = u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?) as usize;
    let record = 8 + TILE_BYTES;
    if bytes.len() != 4 + count * record {
        return None;
    }
    let mut store = TileStore::new();
    // Uniform tiles share one allocation per value.
    let mut uniform: std::collections::HashMap<[u8; 4], std::sync::Arc<Tile>> = std::collections::HashMap::new();
    for chunk in bytes[4..].chunks_exact(record) {
        let x = i32::from_le_bytes(chunk[0..4].try_into().ok()?);
        let y = i32::from_le_bytes(chunk[4..8].try_into().ok()?);
        let pixels = &chunk[8..];
        let first: [u8; 4] = pixels[..4].try_into().ok()?;
        let coord = TileCoord::new(x, y);
        if pixels.as_chunks::<4>().0.iter().all(|p| *p == first) {
            let shared = uniform.entry(first).or_insert_with(|| {
                let mut tile = Tile::empty();
                tile.bytes_mut().copy_from_slice(pixels);
                std::sync::Arc::new(tile)
            });
            store.insert_shared(coord, shared.clone());
            continue;
        }
        let mut tile = Tile::empty();
        tile.bytes_mut().copy_from_slice(pixels);
        store.insert(coord, tile);
    }
    Some(store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::BrushSettings;
    use crate::io::flatten;
    use crate::procedural::GradientShape;
    use crate::smoothing::StrokePoint;

    fn rich_document() -> Document {
        let mut doc = Document::new(130, 70);
        let base = doc.active;
        let layer = doc.find_mut(base).unwrap();
        layer.tiles_mut().unwrap().get_or_create_mut(TileCoord::new(1, 0)).set_pixel(3, 4, [10, 20, 30, 40]);
        layer.blend = BlendMode::Multiply;
        layer.alpha_locked = true;
        let mut mask = Mask { enabled: false, ..Mask::default() };
        mask.tiles.get_or_create_mut(TileCoord::new(0, 0)).set_pixel(1, 1, [99; 4]);
        layer.mask = Some(mask);

        let folder = doc.alloc_id();
        doc.add_layer(Layer::folder(folder, "Folder ✓"));
        let v = doc.alloc_id();
        let mut vector_layer = Layer::vector(v, "Ink");
        if let LayerKind::Vector(vl) = &mut vector_layer.kind {
            vl.strokes.push(VectorStroke {
                points: vec![StrokePoint::new(5.0, 5.0, 1.0), StrokePoint::new(60.0, 30.0, 0.4)],
                settings: BrushSettings::pencil(),
                color: [0.1, 0.2, 0.3],
                seed: 42,
            });
            vector::rasterize(vl, doc.canvas());
        }
        vector_layer.clip = true;
        doc.add_layer(vector_layer);
        let g = doc.alloc_id();
        doc.add_layer(Layer::gradient(
            g,
            "Gradient",
            Gradient { start: [0.0, 0.0], end: [100.0, 60.0], from: [1, 2, 3, 4], to: [5, 6, 7, 8], shape: GradientShape::Radial },
        ));
        let t = doc.alloc_id();
        let mut tone = Layer::tone(t, "Tone", Tone::default());
        tone.opacity = 0.25;
        tone.visible = false;
        doc.add_layer(tone);
        doc.selection = Some(crate::selection::rect_mask([2.0, 2.0], [20.0, 9.0], doc.canvas()));
        doc.channels.push(crate::document::SavedChannel { name: "Eyes".into(), mask: crate::selection::rect_mask([25.0, 25.0], [40.0, 40.0], doc.canvas()) });
        doc.mode = crate::color_mode::ColorMode::Grayscale;
        doc.duotone = Some(crate::duotone::Duotone::default());
        doc.palette = vec![[1, 2, 3], [250, 0, 9]];
        doc.cmyk = Some(crate::cmyk::CmykSetup { name: "Press".into(), icc: std::sync::Arc::new(vec![1, 2, 3]), intent: crate::cmyk::CmykIntent::Saturation, black_point: false });
        doc.active = v;
        doc
    }

    #[test]
    fn round_trip_preserves_everything() {
        let mut doc = rich_document();
        doc.dpi = 350;
        let guide = crate::rulers::RulerKind::Guide { vertical: false, position: 12.0 };
        doc.rulers.rulers.push(crate::rulers::Ruler { scope: crate::rulers::RulerScope::SameFolder, ..crate::rulers::Ruler::on_layer(guide, 2) });
        let bytes = to_bytes(&doc).unwrap();
        let back = from_bytes(&bytes).unwrap();
        assert_eq!((back.width, back.height, back.active, back.dpi), (doc.width, doc.height, doc.active, 350));
        assert_eq!(back.rows_top_down(), doc.rows_top_down());
        assert_eq!(back.rulers, doc.rulers, "rulers keep their layer and scope");
        assert_eq!(flatten(&back), flatten(&doc));
        let base = back.find(LayerId(2)).unwrap();
        assert_eq!(base.blend, BlendMode::Multiply);
        assert!(base.alpha_locked);
        let mask = base.mask.as_ref().unwrap();
        assert!(!mask.enabled);
        assert_eq!(mask.tiles.pixel(1, 1), [99; 4]);
        assert_eq!(back.find(LayerId(3)).unwrap().name, "Folder ✓");
        assert!(back.find(doc.active).unwrap().clip);
        assert_eq!(back.selection.as_ref().unwrap().pixel(5, 5)[3], 255);
        assert_eq!(back.channels.len(), 1);
        assert_eq!(back.channels[0].name, "Eyes");
        assert_eq!(back.mode, crate::color_mode::ColorMode::Grayscale);
        assert_eq!(back.cmyk, doc.cmyk, "the press profile and its settings");
        assert_eq!((&back.duotone, &back.palette), (&doc.duotone, &doc.palette), "Duotone inks and the colour table");
        assert_eq!(back.channels[0].mask.pixel(30, 30)[3], 255);
        // New layers never reuse saved ids.
        let mut back = back;
        assert!(back.alloc_id().0 >= doc.next_id());
    }

    #[test]
    fn extras_round_trip_and_bad_names_are_refused() {
        let doc = rich_document();
        let mut extras = Extras::new();
        extras.insert("comic.json".into(), br#"{"panels":[]}"#.to_vec());
        let bytes = to_bytes_with(&doc, &extras).unwrap();
        let (back, read) = from_bytes_with(&bytes).unwrap();
        assert_eq!(read, extras);
        assert_eq!(flatten(&back), flatten(&doc));
        // Plain loading ignores them; files without extras give none.
        assert!(from_bytes(&bytes).is_ok());
        assert!(from_bytes_with(&to_bytes(&doc).unwrap()).unwrap().1.is_empty());
        extras.insert("../evil".into(), Vec::new());
        assert!(to_bytes_with(&doc, &extras).is_err());
    }

    #[test]
    fn uniform_tiles_are_shared_on_load() {
        let mut doc = Document::new(256, 256);
        let full = crate::selection::invert(Some(&TileStore::new()), (256, 256));
        let id = doc.active;
        doc.find_mut(id).unwrap().mask = Some(Mask { tiles: full, enabled: true });
        let back = from_bytes(&to_bytes(&doc).unwrap()).unwrap();
        let tiles = &back.find(id).unwrap().mask.as_ref().unwrap().tiles;
        let arcs: Vec<_> = tiles.iter().map(|(_, t)| std::sync::Arc::as_ptr(t)).collect();
        assert_eq!(arcs.len(), 16);
        assert!(arcs.iter().all(|p| *p == arcs[0]), "one shared tile");
        assert_eq!(tiles.pixel(100, 100), [255; 4]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(from_bytes(b"not a zip").is_err());
    }
}

#[cfg(test)]
mod thumbnail_tests {
    use super::*;

    #[test]
    fn projects_start_with_an_uncompressed_thumbnail() {
        let doc = Document::new(800, 400);
        let bytes = to_bytes(&doc).unwrap();
        // The first local file header: stored (method 0), named thumbnail.png.
        assert_eq!(&bytes[0..4], b"PK\x03\x04");
        assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), 0, "stored");
        let name_len = u16::from_le_bytes([bytes[26], bytes[27]]) as usize;
        assert_eq!(&bytes[30..30 + name_len], THUMBNAIL_ENTRY.as_bytes());
        let png = embedded_thumbnail(&bytes).expect("a thumbnail");
        let (w, h, _) = crate::interchange::decode_png(&png).unwrap();
        assert_eq!((w, h), (256, 128));
        // And the project still opens.
        assert_eq!(from_bytes(&bytes).unwrap().canvas(), (800, 400));
    }
}
