//! `.blend` reader, written from the file format's public description (it
//! doesn't use Blender's code): a header, a flat list of blocks, and an SDNA
//! catalogue ("DNA1") describing every struct, so fields are found by name
//! instead of fixed offsets that change between Blender versions.
//!
//! - **Header**: legacy `BLENDER` + pointer size (`_` 4 / `-` 8 bytes) +
//!   endianness (`v` little / `V` big) + version (`305`), 12 bytes; or the
//!   Blender 5 form `BLENDER17-01v0502`: header size 17, file format 1
//!   (always 64-bit little-endian), version 0502.
//! - **Blocks**: a code (`OB`, `ME`, `DATA`, `DNA1`, `ENDB`, …), the SDNA
//!   struct index, the block's address when it was saved (pointers in other
//!   blocks refer to it), the length and element count. Format 1 uses 32-byte
//!   block headers with 64-bit lengths; legacy files 20 or 24 bytes.
//! - **Compression**: files saved compressed are zstd streams (Blender 3+).
//!
//! Meshes are read from the generic attribute layers (Blender 3.4+ /
//! 4.x / 5.x): the `position` float3 vertex attribute, `.corner_vert` per
//! face corner and `face_offset_indices`; UVs from the float2 corner
//! attribute. Objects give their name, mesh and location / rotation / scale.

use std::collections::HashMap;
use std::fmt;

use crate::mesh::{Mesh, Vertex};

#[derive(Debug)]
pub enum BlendError {
    Io(std::io::Error),
    NotBlend,
    Unsupported(String),
    Corrupt(String),
}

impl fmt::Display for BlendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlendError::Io(e) => write!(f, "{e}"),
            BlendError::NotBlend => f.write_str("not a .blend file"),
            BlendError::Unsupported(m) => write!(f, "unsupported .blend: {m}"),
            BlendError::Corrupt(m) => write!(f, "corrupt .blend: {m}"),
        }
    }
}

impl std::error::Error for BlendError {}

impl From<std::io::Error> for BlendError {
    fn from(e: std::io::Error) -> Self {
        BlendError::Io(e)
    }
}

fn corrupt(msg: impl Into<String>) -> BlendError {
    BlendError::Corrupt(msg.into())
}

/// One block of the file.
#[derive(Debug, Clone)]
pub struct Block {
    pub code: [u8; 4],
    pub sdna: usize,
    /// Address the data had when saved; pointers elsewhere refer to it.
    pub old: u64,
    pub count: usize,
    /// Byte range of the data in [`BlendFile::bytes`].
    pub start: usize,
    pub len: usize,
}

impl Block {
    pub fn code_str(&self) -> String {
        self.code.iter().take_while(|&&c| c != 0).map(|&c| c as char).collect()
    }
}

#[derive(Debug, Clone)]
pub struct Field {
    pub type_index: usize,
    /// As written in DNA: `*next`, `co[3]`, `name[64]`, `(*func)()`.
    pub name: String,
    pub offset: usize,
    pub size: usize,
}

impl Field {
    /// The name without pointer stars and array brackets.
    pub fn base_name(&self) -> &str {
        let n = self.name.trim_start_matches(['*', '(']);
        n.split(['[', ')']).next().unwrap_or(n)
    }

    pub fn is_pointer(&self) -> bool {
        self.name.starts_with('*') || self.name.starts_with("(*")
    }
}

#[derive(Debug, Clone)]
pub struct StructDef {
    pub type_index: usize,
    pub fields: Vec<Field>,
}

/// The SDNA catalogue: type names and sizes, and struct layouts.
#[derive(Debug, Clone, Default)]
pub struct Sdna {
    pub types: Vec<String>,
    pub type_sizes: Vec<usize>,
    pub structs: Vec<StructDef>,
    by_name: HashMap<String, usize>,
}

impl Sdna {
    pub fn struct_by_name(&self, name: &str) -> Option<&StructDef> {
        self.by_name.get(name).map(|&i| &self.structs[i])
    }

    pub fn struct_name(&self, index: usize) -> Option<&str> {
        self.structs.get(index).map(|s| self.types[s.type_index].as_str())
    }

    pub fn field<'a>(&'a self, struct_name: &str, field: &str) -> Option<&'a Field> {
        self.struct_by_name(struct_name)?.fields.iter().find(|f| f.base_name() == field)
    }

    pub fn type_name(&self, field: &Field) -> &str {
        &self.types[field.type_index]
    }
}

/// A decompressed `.blend` file with its blocks and SDNA.
pub struct BlendFile {
    pub bytes: Vec<u8>,
    pub version: u32,
    pub pointer_size: usize,
    pub blocks: Vec<Block>,
    pub sdna: Sdna,
    by_address: HashMap<u64, usize>,
}

impl BlendFile {
    pub fn open(path: &std::path::Path) -> Result<Self, BlendError> {
        Self::parse(std::fs::read(path)?)
    }

    pub fn parse(raw: Vec<u8>) -> Result<Self, BlendError> {
        let bytes = if raw.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
            zstd::decode_all(&raw[..]).map_err(|e| corrupt(format!("zstd: {e}")))?
        } else if raw.starts_with(&[0x1F, 0x8B]) {
            return Err(BlendError::Unsupported("gzip-compressed (pre-3.0) files; re-save from Blender".into()));
        } else {
            raw
        };
        if !bytes.starts_with(b"BLENDER") || bytes.len() < 12 {
            return Err(BlendError::NotBlend);
        }
        // Header forms.
        let (header_len, pointer_size, large, version) = if bytes[7].is_ascii_digit() {
            // BLENDER17-01v0502
            let text = std::str::from_utf8(bytes.get(..17).ok_or_else(|| corrupt("short header"))?).map_err(|_| corrupt("header"))?;
            let header_len: usize = text[7..9].parse().map_err(|_| corrupt("header size"))?;
            let format: u32 = text[10..12].parse().map_err(|_| corrupt("format version"))?;
            if format != 1 || &text[12..13] != "v" {
                return Err(BlendError::Unsupported(format!("file format {format}")));
            }
            let version: u32 = text[13..17].parse().map_err(|_| corrupt("version"))?;
            (header_len, 8, true, version)
        } else {
            let pointer_size = match bytes[7] {
                b'_' => 4,
                b'-' => 8,
                _ => return Err(corrupt("pointer size")),
            };
            if bytes[8] != b'v' {
                return Err(BlendError::Unsupported("big-endian files".into()));
            }
            let version = std::str::from_utf8(&bytes[9..12]).ok().and_then(|v| v.parse().ok()).unwrap_or(0);
            (12, pointer_size, false, version)
        };

        // Blocks.
        let mut blocks = Vec::new();
        let mut at = header_len;
        let u32_at = |b: &[u8], i: usize| b.get(i..i + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap()));
        let u64_at = |b: &[u8], i: usize| b.get(i..i + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap()));
        loop {
            let code: [u8; 4] = bytes.get(at..at + 4).ok_or_else(|| corrupt("truncated block header"))?.try_into().unwrap();
            let (sdna, old, len, count, head) = if large {
                (
                    u32_at(&bytes, at + 4).ok_or_else(|| corrupt("block"))? as usize,
                    u64_at(&bytes, at + 8).ok_or_else(|| corrupt("block"))?,
                    u64_at(&bytes, at + 16).ok_or_else(|| corrupt("block"))? as usize,
                    u64_at(&bytes, at + 24).ok_or_else(|| corrupt("block"))? as usize,
                    32,
                )
            } else {
                let len = u32_at(&bytes, at + 4).ok_or_else(|| corrupt("block"))? as usize;
                let old = if pointer_size == 8 { u64_at(&bytes, at + 8) } else { u32_at(&bytes, at + 8).map(u64::from) }.ok_or_else(|| corrupt("block"))?;
                let o = 8 + pointer_size;
                (
                    u32_at(&bytes, at + o).ok_or_else(|| corrupt("block"))? as usize,
                    old,
                    len,
                    u32_at(&bytes, at + o + 4).ok_or_else(|| corrupt("block"))? as usize,
                    16 + pointer_size,
                )
            };
            let start = at + head;
            if &code == b"ENDB" {
                break;
            }
            if start + len > bytes.len() {
                return Err(corrupt(format!("block {} runs past the end", String::from_utf8_lossy(&code))));
            }
            blocks.push(Block { code, sdna, old, count, start, len });
            at = start + len;
        }
        let dna = blocks.iter().find(|b| &b.code == b"DNA1").ok_or_else(|| corrupt("no DNA1 block"))?;
        let sdna = parse_sdna(&bytes[dna.start..dna.start + dna.len], pointer_size)?;
        let by_address = blocks.iter().enumerate().filter(|(_, b)| b.old != 0).map(|(i, b)| (b.old, i)).collect();
        Ok(Self { bytes, version, pointer_size, blocks, sdna, by_address })
    }

    pub fn data(&self, block: &Block) -> &[u8] {
        &self.bytes[block.start..block.start + block.len]
    }

    /// The block a saved pointer refers to.
    pub fn block_at(&self, address: u64) -> Option<&Block> {
        self.by_address.get(&address).map(|&i| &self.blocks[i])
    }

    /// Blocks holding structs of `struct_name` (e.g. "Object", "Mesh").
    pub fn blocks_of<'a>(&'a self, struct_name: &'a str) -> impl Iterator<Item = &'a Block> + 'a {
        self.blocks.iter().filter(move |b| self.sdna.struct_name(b.sdna) == Some(struct_name) && &b.code != b"DATA")
    }

    /// A struct instance: `struct_name` at byte `base` of `data`.
    pub fn view<'a>(&'a self, struct_name: &'a str, data: &'a [u8]) -> StructView<'a> {
        StructView { file: self, struct_name, data }
    }
}

/// Reads fields of one struct instance by name.
pub struct StructView<'a> {
    file: &'a BlendFile,
    struct_name: &'a str,
    data: &'a [u8],
}

impl<'a> StructView<'a> {
    fn field(&self, name: &str) -> Option<&'a Field> {
        self.file.sdna.field(self.struct_name, name)
    }

    pub fn bytes(&self, name: &str) -> Option<&'a [u8]> {
        let f = self.field(name)?;
        self.data.get(f.offset..f.offset + f.size)
    }

    pub fn i32(&self, name: &str) -> Option<i32> {
        Some(i32::from_le_bytes(self.bytes(name)?.get(..4)?.try_into().ok()?))
    }

    pub fn i8(&self, name: &str) -> Option<i8> {
        Some(*self.bytes(name)?.first()? as i8)
    }

    /// The first of `names` the struct has (fields renamed across versions).
    pub fn first_of<'n>(&self, names: &[&'n str]) -> Option<&'n str> {
        names.iter().copied().find(|n| self.field(n).is_some())
    }

    pub fn f32s<const N: usize>(&self, name: &str) -> Option<[f32; N]> {
        let b = self.bytes(name)?;
        let mut out = [0.0; N];
        for (i, v) in out.iter_mut().enumerate() {
            *v = f32::from_le_bytes(b.get(i * 4..i * 4 + 4)?.try_into().ok()?);
        }
        Some(out)
    }

    pub fn pointer(&self, name: &str) -> Option<u64> {
        let b = self.bytes(name)?;
        Some(if self.file.pointer_size == 8 { u64::from_le_bytes(b.get(..8)?.try_into().ok()?) } else { u32::from_le_bytes(b.get(..4)?.try_into().ok()?) as u64 })
    }

    /// A fixed `char[]` field as text.
    pub fn string(&self, name: &str) -> Option<String> {
        let b = self.bytes(name)?;
        Some(String::from_utf8_lossy(&b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]).into_owned())
    }

    /// A nested struct field.
    pub fn nested(&self, name: &str) -> Option<StructView<'a>> {
        let f = self.field(name)?;
        let ty = self.file.sdna.types[f.type_index].as_str();
        Some(StructView { file: self.file, struct_name: ty, data: self.data.get(f.offset..f.offset + f.size)? })
    }
}

fn parse_sdna(data: &[u8], pointer_size: usize) -> Result<Sdna, BlendError> {
    let mut at = 0;
    let tag = |at: &mut usize, want: &[u8]| -> Result<(), BlendError> {
        *at = (*at + 3) & !3;
        if data.get(*at..*at + 4) != Some(want) {
            return Err(corrupt(format!("SDNA: expected {}", String::from_utf8_lossy(want))));
        }
        *at += 4;
        Ok(())
    };
    let int = |at: &mut usize| -> Result<usize, BlendError> {
        let v = data.get(*at..*at + 4).ok_or_else(|| corrupt("SDNA"))?;
        *at += 4;
        Ok(u32::from_le_bytes(v.try_into().unwrap()) as usize)
    };
    let short = |at: &mut usize| -> Result<usize, BlendError> {
        let v = data.get(*at..*at + 2).ok_or_else(|| corrupt("SDNA"))?;
        *at += 2;
        Ok(u16::from_le_bytes(v.try_into().unwrap()) as usize)
    };
    let strings = |at: &mut usize, n: usize| -> Result<Vec<String>, BlendError> {
        (0..n)
            .map(|_| {
                let end = data[*at..].iter().position(|&c| c == 0).ok_or_else(|| corrupt("SDNA string"))? + *at;
                let s = String::from_utf8_lossy(&data[*at..end]).into_owned();
                *at = end + 1;
                Ok(s)
            })
            .collect()
    };
    if data.get(..4) != Some(b"SDNA") {
        return Err(corrupt("SDNA header"));
    }
    at += 4;
    tag(&mut at, b"NAME")?;
    let n = int(&mut at)?;
    let names = strings(&mut at, n)?;
    tag(&mut at, b"TYPE")?;
    let n = int(&mut at)?;
    let types = strings(&mut at, n)?;
    tag(&mut at, b"TLEN")?;
    let type_sizes: Vec<usize> = (0..types.len()).map(|_| short(&mut at)).collect::<Result<_, _>>()?;
    tag(&mut at, b"STRC")?;
    let n = int(&mut at)?;
    let mut structs = Vec::with_capacity(n);
    let mut by_name = HashMap::new();
    for i in 0..n {
        let type_index = short(&mut at)?;
        let count = short(&mut at)?;
        let mut fields = Vec::with_capacity(count);
        let mut offset = 0;
        for _ in 0..count {
            let field_type = short(&mut at)?;
            let name = names.get(short(&mut at)?).cloned().ok_or_else(|| corrupt("SDNA name index"))?;
            let elements: usize = name.split('[').skip(1).filter_map(|s| s.trim_end_matches(']').parse::<usize>().ok()).product();
            let pointer = name.starts_with('*') || name.starts_with("(*");
            let size = if pointer { pointer_size } else { *type_sizes.get(field_type).unwrap_or(&0) } * elements.max(1);
            fields.push(Field { type_index: field_type, name, offset, size });
            offset += size;
        }
        if let Some(t) = types.get(type_index) {
            by_name.insert(t.clone(), i);
        }
        structs.push(StructDef { type_index, fields });
    }
    Ok(Sdna { types, type_sizes, structs, by_name })
}

/// A 3D object found in the file.
#[derive(Debug, Clone)]
pub struct BlendObject {
    pub name: String,
    pub location: [f32; 3],
    /// Euler angles in radians (XYZ).
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
    pub mesh: Option<Mesh>,
}

/// Every mesh object of the file with its geometry.
pub fn load_objects(file: &BlendFile) -> Result<Vec<BlendObject>, BlendError> {
    let mut out = Vec::new();
    for block in file.blocks_of("Object") {
        let ob = file.view("Object", file.data(block));
        let name = ob.nested("id").and_then(|id| id.string("name")).unwrap_or_default();
        let name = name.get(2..).unwrap_or(&name).to_string(); // drop the "OB" prefix
        let mesh = ob.pointer("data").and_then(|p| file.block_at(p)).filter(|b| file.sdna.struct_name(b.sdna) == Some("Mesh"));
        let mesh = match mesh {
            Some(block) => Some(read_mesh(file, block)?),
            None => continue, // cameras, lights, empties, …
        };
        out.push(BlendObject {
            name,
            location: ob.f32s::<3>("loc").unwrap_or([0.0; 3]),
            rotation: ob.f32s::<3>("rot").unwrap_or([0.0; 3]),
            scale: ob.f32s::<3>("scale").unwrap_or([1.0; 3]),
            mesh,
        });
    }
    Ok(out)
}

/// A named attribute array of one mesh domain.
struct Layer<'a> {
    name: String,
    data: &'a [u8],
}

/// Mesh attribute domains, as stored in Blender 5's `Attribute::domain`.
const DOMAIN_POINT: i8 = 0;
const DOMAIN_CORNER: i8 = 3;

/// The named layers of a `CustomData` (one mesh domain; Blender 3.4–4.x).
fn custom_data_layers<'a>(file: &'a BlendFile, custom_data: Option<StructView<'a>>) -> Vec<Layer<'a>> {
    let Some(cd) = custom_data else { return Vec::new() };
    let (Some(ptr), Some(count)) = (cd.pointer("layers"), cd.i32("totlayer")) else { return Vec::new() };
    let Some(block) = file.block_at(ptr) else { return Vec::new() };
    let Some(def) = file.sdna.struct_by_name("CustomDataLayer") else { return Vec::new() };
    let stride = def.fields.iter().map(|f| f.offset + f.size).max().unwrap_or(0);
    (0..count.max(0) as usize)
        .filter_map(|i| {
            let data = file.data(block).get(i * stride..(i + 1) * stride)?;
            let layer = file.view("CustomDataLayer", data);
            let payload = layer.pointer("data").and_then(|p| file.block_at(p)).map(|b| file.data(b))?;
            Some(Layer { name: layer.string("name").unwrap_or_default(), data: payload })
        })
        .collect()
}

/// Array attributes of `domain` from Blender 5's `AttributeStorage`.
fn storage_layers<'a>(file: &'a BlendFile, storage: Option<StructView<'a>>, domain: i8) -> Vec<Layer<'a>> {
    let Some(storage) = storage else { return Vec::new() };
    let (Some(ptr), Some(count)) = (storage.pointer("dna_attributes"), storage.i32("dna_attributes_num")) else { return Vec::new() };
    let Some(block) = file.block_at(ptr) else { return Vec::new() };
    let Some(def) = file.sdna.struct_by_name("Attribute") else { return Vec::new() };
    let stride = def.fields.iter().map(|f| f.offset + f.size).max().unwrap_or(0);
    (0..count.max(0) as usize)
        .filter_map(|i| {
            let attr = file.view("Attribute", file.data(block).get(i * stride..(i + 1) * stride)?);
            if attr.i8("domain")? != domain {
                return None;
            }
            let name_block = file.block_at(attr.pointer("name")?)?;
            let raw = file.data(name_block);
            let name = String::from_utf8_lossy(&raw[..raw.iter().position(|&c| c == 0).unwrap_or(raw.len())]).into_owned();
            // Only array storage holds per-element data (single values are uniform).
            let holder = file.block_at(attr.pointer("data")?)?;
            if file.sdna.struct_name(holder.sdna) != Some("AttributeArray") {
                return None;
            }
            let array = file.view("AttributeArray", file.data(holder));
            let payload = file.block_at(array.pointer("data")?).map(|b| file.data(b))?;
            Some(Layer { name, data: payload })
        })
        .collect()
}
fn read_mesh(file: &BlendFile, block: &Block) -> Result<Mesh, BlendError> {
    let me = file.view("Mesh", file.data(block));
    let name = me.nested("id").and_then(|id| id.string("name")).unwrap_or_default();
    // Blender 5 keeps generic attributes in AttributeStorage, 3.4–4.x in CustomData;
    // field names differ between versions (the saved DNA keeps the old ones).
    let storage = me.nested("attribute_storage");
    let mut verts = storage_layers(file, me.nested("attribute_storage"), DOMAIN_POINT);
    let mut corners = storage_layers(file, storage, DOMAIN_CORNER);
    if let Some(f) = me.first_of(&["vert_data", "vdata"]) {
        verts.extend(custom_data_layers(file, me.nested(f)));
    }
    if let Some(f) = me.first_of(&["corner_data", "ldata"]) {
        corners.extend(custom_data_layers(file, me.nested(f)));
    }
    let positions = verts.iter().find(|l| l.name == "position").ok_or_else(|| {
        BlendError::Unsupported(format!("mesh {name} has no position attribute (files older than Blender 3.4)"))
    })?;
    let corner_vert = corners.iter().find(|l| l.name == ".corner_vert").ok_or_else(|| corrupt(format!("mesh {name}: no .corner_vert")))?;
    let floats = |b: &[u8]| b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect::<Vec<f32>>();
    let ints = |b: &[u8]| b.as_chunks::<4>().0.iter().map(|c| i32::from_le_bytes(*c)).collect::<Vec<i32>>();
    let positions: Vec<[f32; 3]> = floats(positions.data).as_chunks::<3>().0.to_vec();
    let corner_vert = ints(corner_vert.data);
    // Face corner ranges: offsets[i]..offsets[i + 1]. A mesh of only loose edges has none.
    let faces_num = me.first_of(&["faces_num", "totpoly"]).and_then(|f| me.i32(f)).unwrap_or(0).max(0) as usize;
    let offsets_field = me.first_of(&["face_offset_indices", "poly_offset_indices"]).unwrap_or("face_offset_indices");
    let offsets: Vec<i32> = match me.pointer(offsets_field).and_then(|p| file.block_at(p)) {
        Some(b) => ints(file.data(b)),
        None if faces_num == 0 => vec![0],
        None => return Err(corrupt(format!("mesh {name}: no face offsets"))),
    };
    // The active UV map is the first float2 corner layer not starting with '.'.
    let uvs: Option<Vec<[f32; 2]>> = corners
        .iter()
        .find(|l| !l.name.starts_with('.') && l.data.len() == corner_vert.len() * 8)
        .map(|l| floats(l.data).as_chunks::<2>().0.to_vec());
    let mut faces = Vec::with_capacity(faces_num);
    for f in 0..faces_num.min(offsets.len().saturating_sub(1)) {
        let (a, b) = (offsets[f] as usize, offsets[f + 1] as usize);
        if a < b && b <= corner_vert.len() {
            faces.push((a..b).collect::<Vec<usize>>());
        }
    }
    let vertex = |corner: usize| Vertex {
        position: positions.get(corner_vert[corner] as usize).copied().unwrap_or([0.0; 3]),
        normal: [0.0; 3],
        uv: uvs.as_ref().and_then(|u| u.get(corner)).copied().unwrap_or([0.0; 2]),
    };
    Ok(Mesh::from_polygons(name.get(2..).unwrap_or(&name), &faces, vertex))
}
