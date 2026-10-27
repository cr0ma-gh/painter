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
//! - **Compression**: files saved compressed are zstd streams (Blender 3+)
//!   or gzip streams (older versions).
//!
//! Meshes are read from the generic attribute layers (Blender 3.4+ /
//! 4.x / 5.x): the `position` float3 vertex attribute, `.corner_vert` per
//! face corner and `face_offset_indices`; UVs from the float2 corner
//! attribute. Older files (Blender 2.80–3.3) keep struct arrays instead:
//! `MVert::co`, `MLoop::v`, `MPoly::loopstart` / `totloop` and `MLoopUV::uv`;
//! each part falls back to them when its generic attribute is missing.
//! Objects give their name, mesh and location / rotation / scale.

use std::collections::HashMap;
use std::fmt;

use crate::camera::quat_from_axes;
use crate::math::{vec3, Mat4, Quat, Vec3};
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
            // Compressed files before Blender 3.0.
            let mut out = Vec::new();
            std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&raw[..]), &mut out).map_err(|e| corrupt(format!("gzip: {e}")))?;
            out
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
        let mesh = ob.pointer("data").and_then(|p| file.block_at(p)).filter(|b| file.sdna.struct_name(b.sdna) == Some("Mesh"));
        let mesh = match mesh {
            Some(block) => Some(read_mesh(file, block)?),
            None => continue, // cameras, lights, empties, …
        };
        // Where the object really is (parents included), as location / rotation / scale.
        let (location, rotation, scale) = decompose(&world_matrix(file, block, 0));
        out.push(BlendObject {
            name: object_name(&ob),
            location: location.to_array(),
            rotation: rotation.to_euler_xyz(),
            scale: scale.to_array(),
            mesh,
        });
    }
    Ok(out)
}

fn object_name(ob: &StructView<'_>) -> String {
    let name = ob.nested("id").and_then(|id| id.string("name")).unwrap_or_default();
    name.get(2..).unwrap_or(&name).to_string() // drop the "OB" prefix
}

/// A `float [4][4]` field (column-major, like [`Mat4`]).
fn matrix_field(view: &StructView<'_>, name: &str) -> Option<Mat4> {
    let m = view.f32s::<16>(name)?;
    let m = Mat4(std::array::from_fn(|c| std::array::from_fn(|r| m[c * 4 + r])));
    m.0.iter().flatten().all(|v| v.is_finite()).then_some(m)
}

/// An object's own rotation, by its rotation mode: quaternion (0), Euler
/// angles in one of six orders (1 = XYZ … 6 = ZYX), or axis-angle (−1).
fn object_rotation(ob: &StructView<'_>) -> Quat {
    let mode = ob.bytes("rotmode").and_then(|b| b.get(..2)).map_or(1, |b| i16::from_le_bytes([b[0], b[1]]));
    match mode {
        0 => {
            let q = ob.f32s::<4>("quat").unwrap_or([1.0, 0.0, 0.0, 0.0]);
            Quat { w: q[0], x: q[1], y: q[2], z: q[3] }.normalize()
        }
        -1 => {
            let axis = Vec3::from_array(ob.f32s::<3>("rotAxis").unwrap_or([0.0, 1.0, 0.0]));
            let angle = ob.f32s::<1>("rotAngle").map_or(0.0, |a| a[0]);
            if axis.length() < 1e-9 { Quat::IDENTITY } else { Quat::from_axis_angle(axis, angle) }
        }
        _ => {
            let e = ob.f32s::<3>("rot").unwrap_or([0.0; 3]);
            let q = |axis: usize| Quat::from_axis_angle([Vec3::X, Vec3::Y, Vec3::Z][axis], e[axis]);
            // The order the axes apply in (the first is applied first).
            let [a, b, c] = match mode {
                2 => [0, 2, 1], // XZY
                3 => [1, 0, 2], // YXZ
                4 => [1, 2, 0], // YZX
                5 => [2, 0, 1], // ZXY
                6 => [2, 1, 0], // ZYX
                _ => [0, 1, 2], // XYZ
            };
            (q(c) * q(b) * q(a)).normalize()
        }
    }
}

/// An object's world matrix: the one Blender saved (`object_to_world`, or
/// `obmat` before 4.0), else rebuilt from location, rotation and scale
/// through its parents (Blender 4.1+ no longer saves it).
pub fn world_matrix(file: &BlendFile, block: &Block, depth: usize) -> Mat4 {
    let ob = file.view("Object", file.data(block));
    if let Some(m) = ob.first_of(&["object_to_world", "obmat"]).and_then(|f| matrix_field(&ob, f)) {
        return m;
    }
    let local = Mat4::from_trs(
        Vec3::from_array(ob.f32s::<3>("loc").unwrap_or([0.0; 3])),
        object_rotation(&ob),
        // Saved DNA keeps the old name `size`.
        Vec3::from_array(ob.first_of(&["scale", "size"]).and_then(|n| ob.f32s::<3>(n)).unwrap_or([1.0; 3])),
    );
    match ob.pointer("parent").filter(|&p| p != 0 && depth < 32).and_then(|p| file.block_at(p)) {
        Some(parent) => {
            let inverse = matrix_field(&ob, "parentinv").unwrap_or(Mat4::IDENTITY);
            world_matrix(file, parent, depth + 1) * inverse * local
        }
        None => local,
    }
}

/// Location, rotation and scale of a matrix without shear.
pub fn decompose(m: &Mat4) -> (Vec3, Quat, Vec3) {
    let axis = |c: usize| vec3(m.0[c][0], m.0[c][1], m.0[c][2]);
    let mut scale = vec3(axis(0).length(), axis(1).length(), axis(2).length());
    let unit = |v: Vec3, s: f32| if s > 1e-12 { v * (1.0 / s) } else { v };
    let (x, y, mut z) = (unit(axis(0), scale.x), unit(axis(1), scale.y), unit(axis(2), scale.z));
    // A mirrored matrix: put the flip in the scale.
    if x.cross(y).dot(z) < 0.0 {
        z = -z;
        scale.z = -scale.z;
    }
    (vec3(m.0[3][0], m.0[3][1], m.0[3][2]), quat_from_axes(x, y, z), scale)
}

/// The scene camera of a `.blend` file and what it sees.
#[derive(Debug, Clone, PartialEq)]
pub struct BlendCamera {
    pub name: String,
    /// The camera's world location.
    pub location: Vec3,
    /// Camera axes (−Z forward, +Y up) to world, like [`crate::camera::ViewCamera::rotation`].
    pub rotation: Quat,
    pub orthographic: bool,
    /// Focal length and sensor size in millimetres.
    pub lens: f32,
    pub sensor_width: f32,
    pub sensor_height: f32,
    /// Which sensor size spans the frame: automatic (the larger side), horizontal or vertical.
    pub sensor_fit: SensorFit,
    /// Orthographic: the frame's larger side in world units.
    pub ortho_scale: f32,
    /// Lens shift, in units of the frame's larger side.
    pub shift: [f32; 2],
    pub clip_start: f32,
    pub clip_end: f32,
    /// The render size in pixels (resolution percentage applied) and pixel aspect.
    pub resolution: [u32; 2],
    pub pixel_aspect: [f32; 2],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensorFit {
    Auto,
    Horizontal,
    Vertical,
}

impl BlendCamera {
    /// The render frame's width / height.
    pub fn frame_aspect(&self) -> f32 {
        let w = self.resolution[0].max(1) as f32 * self.pixel_aspect[0].max(1e-3);
        let h = self.resolution[1].max(1) as f32 * self.pixel_aspect[1].max(1e-3);
        w / h
    }

    /// Half the frame's width and height: at unit distance for a
    /// perspective camera, in world units for an orthographic one.
    pub fn frame_half_size(&self) -> (f32, f32) {
        let aspect = self.frame_aspect();
        let horizontal = match self.sensor_fit {
            SensorFit::Auto => aspect >= 1.0,
            SensorFit::Horizontal => true,
            SensorFit::Vertical => false,
        };
        // Auto uses the sensor width for whichever side is larger.
        let sensor = if self.sensor_fit == SensorFit::Vertical { self.sensor_height } else { self.sensor_width };
        let half = if self.orthographic { self.ortho_scale * 0.5 } else { sensor * 0.5 / self.lens.max(1e-3) };
        if horizontal { (half, half / aspect) } else { (half * aspect, half) }
    }
}

/// The camera the file renders with: the first scene's camera, else the first camera object.
pub fn load_camera(file: &BlendFile) -> Option<BlendCamera> {
    let is_camera = |b: &&Block| {
        let ob = file.view("Object", file.data(b));
        ob.pointer("data").and_then(|p| file.block_at(p)).is_some_and(|d| file.sdna.struct_name(d.sdna) == Some("Camera"))
    };
    let scenes: Vec<&Block> = file.blocks_of("Scene").collect();
    let from_scene = scenes.iter().find_map(|s| {
        let scene = file.view("Scene", file.data(s));
        let block = scene.pointer("camera").filter(|&p| p != 0).and_then(|p| file.block_at(p)).filter(is_camera)?;
        Some((block, Some(scene)))
    });
    let (block, scene) = from_scene.or_else(|| file.blocks_of("Object").find(is_camera).map(|b| (b, scenes.first().map(|s| file.view("Scene", file.data(s))))))?;
    let ob = file.view("Object", file.data(block));
    let cam = file.view("Camera", file.block_at(ob.pointer("data")?).map(|b| file.data(b))?);
    // Positive sizes; a missing or nonsensical value falls back to Blender's default.
    let f = |name: &str, default: f32| cam.f32s::<1>(name).map(|v| v[0]).filter(|v| v.is_finite() && *v > 0.0).unwrap_or(default);
    let float = |name: &str, default: f32| cam.f32s::<1>(name).map_or(default, |v| v[0]);
    let (location, rotation, _) = decompose(&world_matrix(file, block, 0));
    // Render size: RenderData's resolution, scaled by its percentage.
    let render = scene.as_ref().and_then(|s| s.nested("r"));
    let int = |name: &str| render.as_ref().and_then(|r| r.i32(name));
    let short = |name: &str| render.as_ref().and_then(|r| r.bytes(name)).and_then(|b| b.get(..2)).map(|b| i16::from_le_bytes([b[0], b[1]]));
    let percent = short("size").filter(|&p| p > 0).unwrap_or(100) as f32 / 100.0;
    let res = |name: &str, default: i32| ((int(name).filter(|&v| v > 0).unwrap_or(default)) as f32 * percent).round().max(1.0) as u32;
    let aspect = |name: &str| render.as_ref().and_then(|r| r.f32s::<1>(name)).map_or(1.0, |v| v[0]).max(1e-3);
    Some(BlendCamera {
        name: object_name(&ob),
        location,
        rotation,
        // Camera::type: 0 perspective, 1 orthographic, 2 panoramic (drawn as perspective).
        orthographic: cam.i8("type") == Some(1),
        lens: f("lens", 50.0),
        sensor_width: f("sensor_x", 36.0),
        sensor_height: f("sensor_y", 24.0),
        sensor_fit: match cam.i8("sensor_fit") {
            Some(1) => SensorFit::Horizontal,
            Some(2) => SensorFit::Vertical,
            _ => SensorFit::Auto,
        },
        ortho_scale: f("ortho_scale", 6.0),
        shift: [float("shiftx", 0.0), float("shifty", 0.0)],
        clip_start: cam.first_of(&["clip_start", "clipsta"]).map_or(0.1, |n| f(n, 0.1)),
        clip_end: cam.first_of(&["clip_end", "clipend"]).map_or(100.0, |n| f(n, 100.0)),
        resolution: [res("xsch", 1920), res("ysch", 1080)],
        pixel_aspect: [aspect("xasp"), aspect("yasp")],
    })
}

/// A named attribute array of one mesh domain.
struct Layer<'a> {
    name: String,
    /// `CustomDataLayer::type` (e.g. [`CD_MVERT`]); -1 for Blender 5 attributes.
    kind: i32,
    data: &'a [u8],
}

/// Mesh attribute domains, as stored in Blender 5's `Attribute::domain`.
const DOMAIN_POINT: i8 = 0;
const DOMAIN_CORNER: i8 = 3;

/// `CustomDataLayer::type` of the struct arrays meshes kept before the
/// generic attributes (Blender 2.80–3.3; some lingered until 3.5).
const CD_MVERT: i32 = 0;
const CD_MLOOPUV: i32 = 16;
const CD_MPOLY: i32 = 25;
const CD_MLOOP: i32 = 26;

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
            Some(Layer { name: layer.string("name").unwrap_or_default(), kind: layer.i32("type").unwrap_or(-1), data: payload })
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
            Some(Layer { name, kind: -1, data: payload })
        })
        .collect()
}

/// A pre-3.4 struct array of a mesh: the `CustomData` layer of `kind`, else
/// the `Mesh` pointer field naming the same data.
fn legacy_array<'a>(file: &'a BlendFile, me: &StructView<'a>, layers: &[Layer<'a>], kind: i32, pointer: &str) -> Option<&'a [u8]> {
    layers.iter().find(|l| l.kind == kind).map(|l| l.data).or_else(|| me.pointer(pointer).and_then(|p| file.block_at(p)).map(|b| file.data(b)))
}

/// The elements of a saved array of `struct_name` structs.
fn struct_array<'a>(file: &'a BlendFile, struct_name: &'a str, data: &'a [u8]) -> impl Iterator<Item = StructView<'a>> + 'a {
    let size = file.sdna.struct_by_name(struct_name).and_then(|s| file.sdna.type_sizes.get(s.type_index)).copied().unwrap_or(0);
    let chunks = if size == 0 { data[..0].chunks_exact(1) } else { data.chunks_exact(size) };
    chunks.map(move |c| file.view(struct_name, c))
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
    let count = |names: &[&str]| me.first_of(names).and_then(|f| me.i32(f)).unwrap_or(0).max(0) as usize;
    let floats = |b: &[u8]| b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect::<Vec<f32>>();
    let ints = |b: &[u8]| b.as_chunks::<4>().0.iter().map(|c| i32::from_le_bytes(*c)).collect::<Vec<i32>>();
    // Each part from its generic attribute, else from the pre-3.4 struct array.
    let positions: Vec<[f32; 3]> = if let Some(l) = verts.iter().find(|l| l.name == "position") {
        floats(l.data).as_chunks::<3>().0.to_vec()
    } else if let Some(data) = legacy_array(file, &me, &verts, CD_MVERT, "mvert") {
        struct_array(file, "MVert", data).map(|v| v.f32s("co").unwrap_or([0.0; 3])).collect()
    } else if count(&["verts_num", "totvert"]) == 0 {
        Vec::new()
    } else {
        return Err(corrupt(format!("mesh {name}: no vertex positions")));
    };
    let corner_vert: Vec<i32> = if let Some(l) = corners.iter().find(|l| l.name == ".corner_vert") {
        ints(l.data)
    } else if let Some(data) = legacy_array(file, &me, &corners, CD_MLOOP, "mloop") {
        struct_array(file, "MLoop", data).map(|l| l.i32("v").unwrap_or(-1)).collect()
    } else if count(&["corners_num", "totloop"]) == 0 {
        Vec::new()
    } else {
        return Err(corrupt(format!("mesh {name}: no .corner_vert")));
    };
    // Face corner ranges. A mesh of only loose edges has none.
    let faces_num = count(&["faces_num", "totpoly"]);
    let offsets_field = me.first_of(&["face_offset_indices", "poly_offset_indices"]).unwrap_or("face_offset_indices");
    let polys = me.first_of(&["face_data", "pdata"]).map(|f| custom_data_layers(file, me.nested(f))).unwrap_or_default();
    let ranges: Vec<(usize, usize)> = if let Some(b) = me.pointer(offsets_field).and_then(|p| file.block_at(p)) {
        // offsets[i]..offsets[i + 1]
        ints(file.data(b)).windows(2).map(|w| (w[0].max(0) as usize, w[1].max(0) as usize)).collect()
    } else if let Some(data) = legacy_array(file, &me, &polys, CD_MPOLY, "mpoly") {
        struct_array(file, "MPoly", data)
            .map(|p| {
                let start = p.i32("loopstart").unwrap_or(0).max(0) as usize;
                (start, start.saturating_add(p.i32("totloop").unwrap_or(0).max(0) as usize))
            })
            .collect()
    } else if faces_num == 0 {
        Vec::new()
    } else {
        return Err(corrupt(format!("mesh {name}: no face offsets")));
    };
    // The active UV map: the first MLoopUV layer of older files, else the
    // first float2 corner layer not starting with '.' (MLoop arrays are 8 bytes too).
    let uvs: Option<Vec<[f32; 2]>> = legacy_array(file, &me, &corners, CD_MLOOPUV, "mloopuv")
        .map(|data| struct_array(file, "MLoopUV", data).map(|uv| uv.f32s("uv").unwrap_or([0.0; 2])).collect())
        .or_else(|| {
            corners
                .iter()
                .find(|l| l.kind != CD_MLOOP && !l.name.starts_with('.') && l.data.len() == corner_vert.len() * 8)
                .map(|l| floats(l.data).as_chunks::<2>().0.to_vec())
        });
    let mut faces = Vec::with_capacity(faces_num);
    for &(a, b) in ranges.iter().take(faces_num) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Struct layouts as Blender 2.80–3.3 saved them (trimmed to what the reader needs).
    const STRUCTS: &[(&str, &[(&str, &str)])] = &[
        ("ID", &[("char", "name[66]"), ("char", "_pad[6]")]),
        ("MVert", &[("float", "co[3]"), ("short", "no[3]"), ("char", "flag"), ("char", "bweight")]),
        ("MLoop", &[("int", "v"), ("int", "e")]),
        ("MPoly", &[("int", "loopstart"), ("int", "totloop"), ("short", "mat_nr"), ("char", "flag"), ("char", "_pad")]),
        ("MLoopUV", &[("float", "uv[2]"), ("int", "flag")]),
        ("CustomDataLayer", &[("int", "type"), ("int", "offset"), ("int", "flag"), ("int", "active"), ("char", "name[64]"), ("char", "*data")]),
        ("CustomData", &[("CustomDataLayer", "*layers"), ("int", "totlayer"), ("int", "maxlayer")]),
        (
            "Mesh",
            &[
                ("ID", "id"),
                ("MVert", "*mvert"),
                ("MLoop", "*mloop"),
                ("MPoly", "*mpoly"),
                ("MLoopUV", "*mloopuv"),
                ("CustomData", "vdata"),
                ("CustomData", "ldata"),
                ("CustomData", "pdata"),
                ("int", "totvert"),
                ("int", "totloop"),
                ("int", "totpoly"),
                ("int", "_pad"),
            ],
        ),
        ("Object", &[("ID", "id"), ("char", "*data"), ("float", "loc[3]"), ("int", "_pad")]),
    ];
    const TYPES: &[(&str, u16)] = &[
        ("char", 1),
        ("short", 2),
        ("int", 4),
        ("float", 4),
        ("ID", 72),
        ("MVert", 20),
        ("MLoop", 8),
        ("MPoly", 12),
        ("MLoopUV", 12),
        ("CustomDataLayer", 88),
        ("CustomData", 16),
        ("Mesh", 168),
        ("Object", 96),
    ];

    /// The DNA1 block for [`STRUCTS`].
    fn dna() -> Vec<u8> {
        let type_index = |t: &str| TYPES.iter().position(|(n, _)| *n == t).unwrap() as u16;
        let mut names: Vec<&str> = Vec::new();
        for (_, fields) in STRUCTS {
            for (_, n) in fields.iter() {
                if !names.contains(n) {
                    names.push(n);
                }
            }
        }
        let align = |b: &mut Vec<u8>| b.resize(b.len().next_multiple_of(4), 0);
        let mut b = b"SDNANAME".to_vec();
        b.extend((names.len() as u32).to_le_bytes());
        names.iter().for_each(|n| b.extend(n.bytes().chain([0])));
        align(&mut b);
        b.extend(b"TYPE");
        b.extend((TYPES.len() as u32).to_le_bytes());
        TYPES.iter().for_each(|(n, _)| b.extend(n.bytes().chain([0])));
        align(&mut b);
        b.extend(b"TLEN");
        TYPES.iter().for_each(|(_, s)| b.extend(s.to_le_bytes()));
        align(&mut b);
        b.extend(b"STRC");
        b.extend((STRUCTS.len() as u32).to_le_bytes());
        for (name, fields) in STRUCTS {
            b.extend(type_index(name).to_le_bytes());
            b.extend((fields.len() as u16).to_le_bytes());
            for (ty, n) in fields.iter() {
                b.extend(type_index(ty).to_le_bytes());
                b.extend((names.iter().position(|x| x == n).unwrap() as u16).to_le_bytes());
            }
        }
        b
    }

    /// One struct instance with `fields` (nested ones as `outer.inner`) set.
    fn fill(sdna: &Sdna, name: &str, fields: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut out = vec![0; sdna.type_sizes[sdna.struct_by_name(name).unwrap().type_index]];
        for (path, bytes) in fields {
            let (mut ty, mut at) = (name, 0);
            for part in path.split('.') {
                let f = sdna.field(ty, part).unwrap_or_else(|| panic!("{ty}.{part}"));
                at += f.offset;
                ty = &sdna.types[f.type_index];
            }
            out[at..at + bytes.len()].copy_from_slice(bytes);
        }
        out
    }

    fn floats(values: &[f32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    /// A 2.93-style file: a quad and a triangle sharing an edge, as MVert /
    /// MLoop / MPoly / MLoopUV arrays reached through `CustomData` layers
    /// (`through_layers`) or only through the `Mesh` pointers.
    fn legacy_file(through_layers: bool) -> Vec<u8> {
        let sdna = parse_sdna(&dna(), 8).unwrap();
        let int = |v: i32| v.to_le_bytes().to_vec();
        let ptr = |a: u64| a.to_le_bytes().to_vec();
        let positions = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 2.0]];
        let loops = [0, 1, 2, 3, 0, 1, 4];
        let polys = [(0, 4), (4, 3)];
        let mvert: Vec<u8> = positions.iter().flat_map(|co| fill(&sdna, "MVert", &[("co", floats(co))])).collect();
        let mloop: Vec<u8> = loops.iter().flat_map(|&v| fill(&sdna, "MLoop", &[("v", int(v))])).collect();
        let mpoly: Vec<u8> = polys.iter().flat_map(|&(s, n)| fill(&sdna, "MPoly", &[("loopstart", int(s)), ("totloop", int(n))])).collect();
        let mloopuv: Vec<u8> = (0..loops.len()).flat_map(|c| fill(&sdna, "MLoopUV", &[("uv", floats(&[c as f32 * 0.1, 0.5]))])).collect();
        let (mvert_at, mloop_at, mpoly_at, uv_at) = (0x100, 0x200, 0x300, 0x400);
        let layer = |kind: i32, data: u64| fill(&sdna, "CustomDataLayer", &[("type", int(kind)), ("data", ptr(data))]);

        let mut mesh = vec![
            ("id.name", b"MELegacy\0".to_vec()),
            ("totvert", int(positions.len() as i32)),
            ("totloop", int(loops.len() as i32)),
            ("totpoly", int(polys.len() as i32)),
        ];
        let mut blocks: Vec<(&[u8; 4], &str, u64, Vec<u8>)> = vec![
            (b"DATA", "MVert", mvert_at, mvert),
            (b"DATA", "MLoop", mloop_at, mloop),
            (b"DATA", "MPoly", mpoly_at, mpoly),
            (b"DATA", "MLoopUV", uv_at, mloopuv),
        ];
        if through_layers {
            for (layers_field, count_field, at, layers) in [
                ("vdata.layers", "vdata.totlayer", 0x500, vec![layer(CD_MVERT, mvert_at)]),
                // MLoop is 8 bytes per corner like a float2 UV map; it must not be taken for one.
                ("ldata.layers", "ldata.totlayer", 0x600, vec![layer(CD_MLOOP, mloop_at), layer(CD_MLOOPUV, uv_at)]),
                ("pdata.layers", "pdata.totlayer", 0x700, vec![layer(CD_MPOLY, mpoly_at)]),
            ] {
                mesh.push((layers_field, ptr(at)));
                mesh.push((count_field, int(layers.len() as i32)));
                blocks.push((b"DATA", "CustomDataLayer", at, layers.concat()));
            }
        } else {
            mesh.extend([("mvert", ptr(mvert_at)), ("mloop", ptr(mloop_at)), ("mpoly", ptr(mpoly_at)), ("mloopuv", ptr(uv_at))]);
        }
        blocks.push((b"ME\0\0", "Mesh", 0x800, fill(&sdna, "Mesh", &mesh)));
        blocks.push((b"OB\0\0", "Object", 0x900, fill(&sdna, "Object", &[("id.name", b"OBLegacy\0".to_vec()), ("data", ptr(0x800))])));

        // 64-bit little-endian, 24-byte block headers.
        let mut out = b"BLENDER-v293".to_vec();
        let mut block = |code: &[u8; 4], sdna_index: usize, old: u64, data: &[u8], count: usize| {
            out.extend(code);
            out.extend((data.len() as u32).to_le_bytes());
            out.extend(old.to_le_bytes());
            out.extend((sdna_index as u32).to_le_bytes());
            out.extend((count as u32).to_le_bytes());
            out.extend(data);
        };
        for (code, ty, old, data) in &blocks {
            let index = STRUCTS.iter().position(|(n, _)| n == ty).unwrap();
            block(code, index, *old, data, data.len() / sdna.type_sizes[sdna.structs[index].type_index]);
        }
        block(b"DNA1", 0, 0, &dna(), 1);
        block(b"ENDB", 0, 0, &[], 0);
        out
    }

    fn check(mesh: &Mesh) {
        assert_eq!(mesh.name, "Legacy");
        // Each face corner becomes a vertex; a quad and a triangle give three triangles.
        assert_eq!(mesh.vertices.len(), 7);
        assert_eq!(mesh.indices, vec![0, 1, 2, 0, 2, 3, 4, 5, 6]);
        assert_eq!(mesh.bounds.min.to_array(), [0.0, 0.0, 0.0]);
        assert_eq!(mesh.bounds.max.to_array(), [1.0, 1.0, 2.0]);
        assert_eq!(mesh.vertices[2].position, [1.0, 1.0, 0.0]);
        assert_eq!(mesh.vertices[6].position, [0.0, 0.0, 2.0]);
        assert_eq!(mesh.vertices[0].normal, [0.0, 0.0, 1.0]);
        assert_eq!(mesh.vertices[4].normal, [0.0, -1.0, 0.0]);
        assert_eq!(mesh.vertices[3].uv, [3.0 * 0.1, 0.5]);
        assert_eq!(mesh.vertices[6].uv, [6.0 * 0.1, 0.5]);
    }

    #[test]
    fn legacy_mesh_through_custom_data_layers() {
        let objects = load_objects(&BlendFile::parse(legacy_file(true)).unwrap()).unwrap();
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].name, "Legacy");
        check(objects[0].mesh.as_ref().unwrap());
    }

    #[test]
    fn legacy_mesh_through_mesh_pointers() {
        let objects = load_objects(&BlendFile::parse(legacy_file(false)).unwrap()).unwrap();
        check(objects[0].mesh.as_ref().unwrap());
    }

    #[test]
    fn gzip_compressed_file() {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, &legacy_file(true)).unwrap();
        let file = BlendFile::parse(gz.finish().unwrap()).unwrap();
        assert_eq!(file.version, 293);
        check(load_objects(&file).unwrap()[0].mesh.as_ref().unwrap());
    }
}
