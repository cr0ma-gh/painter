//! Binary FBX models (versions 7.x, as Blender, Maya, 3ds Max and most
//! asset stores write them), from the format's public description: the node
//! tree, its typed properties (arrays zlib-compressed or not), mesh geometry
//! (polygons of any size, normals, the first UV set), each model's transform
//! and parent, the material's diffuse colour and its image, and the file's
//! up axis and unit.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use crate::import::{object, ImportedObject, Y_UP_TO_Z_UP};
use crate::math::{vec3, Mat4, Quat, Vec3};

const MAGIC: &[u8] = b"Kaydara FBX Binary  \0";

/// Reads a `.fbx` file (`path` finds the textures it refers to).
pub fn load(path: &Path) -> Result<Vec<ImportedObject>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    read(&bytes, path.parent())
}

/// One property of a node.
#[derive(Debug, Clone)]
enum Prop {
    Int(i64),
    Float(f64),
    Str(Vec<u8>),
    Ints(Vec<i64>),
    Floats(Vec<f64>),
}

impl Prop {
    fn int(&self) -> Option<i64> {
        match self {
            Prop::Int(v) => Some(*v),
            Prop::Float(v) => Some(*v as i64),
            _ => None,
        }
    }
    fn float(&self) -> Option<f64> {
        match self {
            Prop::Int(v) => Some(*v as f64),
            Prop::Float(v) => Some(*v),
            _ => None,
        }
    }
    fn text(&self) -> Option<String> {
        match self {
            Prop::Str(b) => Some(String::from_utf8_lossy(b).into_owned()),
            _ => None,
        }
    }
    fn floats(&self) -> Option<Vec<f64>> {
        match self {
            Prop::Floats(v) => Some(v.clone()),
            Prop::Ints(v) => Some(v.iter().map(|&i| i as f64).collect()),
            _ => None,
        }
    }
    fn ints(&self) -> Option<Vec<i64>> {
        match self {
            Prop::Ints(v) => Some(v.clone()),
            Prop::Floats(v) => Some(v.iter().map(|&f| f as i64).collect()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Node {
    name: String,
    props: Vec<Prop>,
    children: Vec<Node>,
}

impl Node {
    fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }
    fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }
    /// The first property of child `name`.
    fn value(&self, name: &str) -> Option<&Prop> {
        self.child(name)?.props.first()
    }
    /// A `Properties70` entry's values (after name, type, label, flags).
    fn p70(&self, key: &str) -> Option<&[Prop]> {
        self.child("Properties70")?
            .children("P")
            .find(|p| p.props.first().and_then(Prop::text).as_deref() == Some(key))
            .map(|p| &p.props[4.min(p.props.len())..])
    }
    fn p70_vec(&self, key: &str) -> Option<Vec3> {
        let v = self.p70(key)?;
        Some(vec3(v.first()?.float()? as f32, v.get(1)?.float()? as f32, v.get(2)?.float()? as f32))
    }
    fn id(&self) -> Option<i64> {
        self.props.first()?.int()
    }
    /// The object's name ("Cube\0\x01Model" → "Cube").
    fn object_name(&self) -> String {
        let raw = self.props.get(1).and_then(Prop::text).unwrap_or_default();
        raw.split('\0').next().unwrap_or("").to_string()
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
    wide: bool,
}

impl Parser<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let s = self.bytes.get(self.at..self.at + n).ok_or("truncated FBX")?;
        self.at += n;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("4 bytes")))
    }
    fn offset(&mut self) -> Result<u64, String> {
        if self.wide {
            Ok(u64::from_le_bytes(self.take(8)?.try_into().expect("8 bytes")))
        } else {
            self.u32().map(u64::from)
        }
    }

    /// A node, or `None` at the empty record that ends a list.
    fn node(&mut self, depth: usize) -> Result<Option<Node>, String> {
        if depth > 64 {
            return Err("FBX nodes nested too deep".into());
        }
        let end = self.offset()? as usize;
        let count = self.offset()? as usize;
        let _list_len = self.offset()?;
        let name_len = self.take(1)?[0] as usize;
        if end == 0 {
            return Ok(None);
        }
        let name = String::from_utf8_lossy(self.take(name_len)?).into_owned();
        let mut props = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            props.push(self.prop()?);
        }
        let mut children = Vec::new();
        while self.at < end {
            match self.node(depth + 1)? {
                Some(n) => children.push(n),
                None => break,
            }
        }
        if end > self.bytes.len() || end < self.at {
            return Err("bad FBX node end".into());
        }
        self.at = end;
        Ok(Some(Node { name, props, children }))
    }

    fn prop(&mut self) -> Result<Prop, String> {
        let kind = self.take(1)?[0];
        let fixed = |p: &mut Self, n: usize| -> Result<[u8; 8], String> {
            let mut b = [0u8; 8];
            b[..n].copy_from_slice(p.take(n)?);
            Ok(b)
        };
        Ok(match kind {
            b'Y' => Prop::Int(i16::from_le_bytes(fixed(self, 2)?[..2].try_into().expect("2")) as i64),
            b'C' => Prop::Int(fixed(self, 1)?[0] as i64),
            b'I' => Prop::Int(i32::from_le_bytes(fixed(self, 4)?[..4].try_into().expect("4")) as i64),
            b'F' => Prop::Float(f32::from_le_bytes(fixed(self, 4)?[..4].try_into().expect("4")) as f64),
            b'D' => Prop::Float(f64::from_le_bytes(fixed(self, 8)?)),
            b'L' => Prop::Int(i64::from_le_bytes(fixed(self, 8)?)),
            b'S' | b'R' => {
                let n = self.u32()? as usize;
                Prop::Str(self.take(n)?.to_vec())
            }
            b'f' | b'd' | b'l' | b'i' | b'b' => {
                let (count, encoding, len) = (self.u32()? as usize, self.u32()?, self.u32()? as usize);
                let raw = self.take(len)?;
                let size = match kind {
                    b'd' | b'l' => 8,
                    b'f' | b'i' => 4,
                    _ => 1,
                };
                let data = if encoding == 1 {
                    let mut out = Vec::with_capacity((count * size).min(1 << 28));
                    flate2::read::ZlibDecoder::new(raw)
                        .take((count * size) as u64)
                        .read_to_end(&mut out)
                        .map_err(|e| format!("FBX array: {e}"))?;
                    out
                } else {
                    raw.to_vec()
                };
                if data.len() < count * size {
                    return Err("short FBX array".into());
                }
                let chunks = data[..count * size].chunks_exact(size);
                match kind {
                    b'd' => Prop::Floats(chunks.map(|c| f64::from_le_bytes(c.try_into().expect("8"))).collect()),
                    b'f' => Prop::Floats(chunks.map(|c| f32::from_le_bytes(c.try_into().expect("4")) as f64).collect()),
                    b'l' => Prop::Ints(chunks.map(|c| i64::from_le_bytes(c.try_into().expect("8"))).collect()),
                    b'i' => Prop::Ints(chunks.map(|c| i32::from_le_bytes(c.try_into().expect("4")) as i64).collect()),
                    _ => Prop::Ints(chunks.map(|c| c[0] as i64).collect()),
                }
            }
            other => return Err(format!("unknown FBX property type {:?}", other as char)),
        })
    }
}

/// An FBX document from its bytes; `dir` holds the textures it refers to.
pub fn read(bytes: &[u8], dir: Option<&Path>) -> Result<Vec<ImportedObject>, String> {
    if !bytes.starts_with(MAGIC) {
        if bytes.starts_with(b"; FBX") || bytes.windows(13).take(4096).any(|w| w == b"FBXHeaderExte") {
            return Err("this is a text (ASCII) FBX; export it as binary FBX".into());
        }
        return Err("not an FBX file".into());
    }
    let version = u32::from_le_bytes(bytes.get(23..27).ok_or("truncated FBX")?.try_into().expect("4 bytes"));
    let mut parser = Parser { bytes, at: 27, wide: version >= 7500 };
    let mut top = Vec::new();
    while parser.at < bytes.len() {
        match parser.node(0) {
            Ok(Some(n)) => top.push(n),
            // The footer follows the last top-level node.
            Ok(None) | Err(_) if !top.is_empty() => break,
            Ok(None) => break,
            Err(e) => return Err(e),
        }
    }
    let root = Node { children: top, ..Default::default() };
    Document::new(&root, dir).objects()
}

struct Document<'a> {
    objects: HashMap<i64, &'a Node>,
    /// Child → parents (object–object links).
    parents: HashMap<i64, Vec<i64>>,
    /// Parent → children, with the property name for object–property links.
    children: HashMap<i64, Vec<(i64, Option<String>)>>,
    settings: Option<&'a Node>,
    dir: Option<&'a Path>,
}

impl<'a> Document<'a> {
    fn new(root: &'a Node, dir: Option<&'a Path>) -> Self {
        let mut objects = HashMap::new();
        for o in root.child("Objects").into_iter().flat_map(|n| n.children.iter()) {
            if let Some(id) = o.id() {
                objects.insert(id, o);
            }
        }
        let (mut parents, mut children) = (HashMap::<i64, Vec<i64>>::new(), HashMap::<i64, Vec<(i64, Option<String>)>>::new());
        for c in root.child("Connections").into_iter().flat_map(|n| n.children("C")) {
            let (Some(child), Some(parent)) = (c.props.get(1).and_then(Prop::int), c.props.get(2).and_then(Prop::int)) else {
                continue;
            };
            parents.entry(child).or_default().push(parent);
            children.entry(parent).or_default().push((child, c.props.get(3).and_then(Prop::text)));
        }
        Document { objects, parents, children, settings: root.child("GlobalSettings"), dir }
    }

    fn kind(&self, id: i64) -> Option<&str> {
        self.objects.get(&id).map(|n| n.name.as_str())
    }

    /// The file's axes and unit as a matrix to the scene's Z-up metres.
    fn global(&self) -> Mat4 {
        let get = |k: &str| self.settings.and_then(|s| s.p70(k)).and_then(|v| v.first()).and_then(Prop::float);
        let up = get("UpAxis").unwrap_or(1.0) as i32;
        let unit = get("UnitScaleFactor").unwrap_or(1.0) as f32 / 100.0;
        let axes = if up == 2 { Mat4::IDENTITY } else { Y_UP_TO_Z_UP };
        axes * Mat4::from_trs(Vec3::ZERO, Quat::IDENTITY, vec3(unit, unit, unit))
    }

    /// A model's own transform (translation, pre-rotation, rotation, scale).
    fn local(&self, model: &Node) -> Mat4 {
        let deg = |v: Vec3| [v.x.to_radians(), v.y.to_radians(), v.z.to_radians()];
        let t = model.p70_vec("Lcl Translation").unwrap_or(Vec3::ZERO);
        let r = model.p70_vec("Lcl Rotation").unwrap_or(Vec3::ZERO);
        let s = model.p70_vec("Lcl Scaling").unwrap_or(vec3(1.0, 1.0, 1.0));
        let pre = model.p70_vec("PreRotation").unwrap_or(Vec3::ZERO);
        let rotation = Quat::from_euler_xyz(deg(pre)) * Quat::from_euler_xyz(deg(r));
        Mat4::from_trs(t, rotation.normalize(), s)
    }

    /// A model's world transform (its parents' included).
    fn world(&self, id: i64, depth: usize) -> Mat4 {
        let Some(model) = self.objects.get(&id) else { return self.global() };
        let parent = self.parents.get(&id).and_then(|p| p.iter().find(|&&p| self.kind(p) == Some("Model"))).copied();
        let base = match parent {
            Some(p) if depth < 64 => self.world(p, depth + 1),
            _ => self.global(),
        };
        base * self.local(model)
    }

    fn objects(&self) -> Result<Vec<ImportedObject>, String> {
        let mut out = Vec::new();
        let mut geometries: Vec<(&i64, &&Node)> = self.objects.iter().filter(|(_, n)| n.name == "Geometry").collect();
        geometries.sort_by_key(|(id, _)| **id);
        for (&gid, geometry) in geometries {
            // Each model using this geometry is an object.
            let models: Vec<i64> =
                self.parents.get(&gid).into_iter().flatten().copied().filter(|&p| self.kind(p) == Some("Model")).collect();
            for model_id in models {
                let model = self.objects[&model_id];
                let geo = Mat4::from_trs(
                    model.p70_vec("GeometricTranslation").unwrap_or(Vec3::ZERO),
                    Quat::from_euler_xyz(
                        model
                            .p70_vec("GeometricRotation")
                            .map_or([0.0; 3], |v| [v.x.to_radians(), v.y.to_radians(), v.z.to_radians()]),
                    ),
                    model.p70_vec("GeometricScaling").unwrap_or(vec3(1.0, 1.0, 1.0)),
                );
                let world = self.world(model_id, 0) * geo;
                let mut name = model.object_name();
                if name.is_empty() {
                    name = geometry.object_name();
                }
                let Some((positions, normals, uvs, triangles)) = mesh_corners(geometry) else { continue };
                let Some((mesh, centre)) = object(&name, positions, normals, uvs, triangles, &world) else { continue };
                let (color, image) = self.material(model_id);
                out.push(ImportedObject { name, mesh, location: centre, color, image });
            }
        }
        if out.is_empty() {
            return Err("no meshes in the file".into());
        }
        Ok(out)
    }

    /// The first material's diffuse colour and image.
    fn material(&self, model: i64) -> ([f32; 3], Option<Vec<u8>>) {
        let Some(&(mat, _)) = self.children.get(&model).and_then(|c| c.iter().find(|(c, _)| self.kind(*c) == Some("Material"))) else {
            return (crate::scene::CLAY, None);
        };
        let node = self.objects[&mat];
        let color = node.p70_vec("DiffuseColor").or_else(|| node.p70_vec("Diffuse")).map_or(crate::scene::CLAY, |v| v.to_array());
        let texture = self.children.get(&mat).and_then(|c| {
            c.iter()
                .filter(|(c, _)| self.kind(*c) == Some("Texture"))
                .find(|(_, p)| {
                    p.as_deref().is_none_or(|p| p.contains("Diffuse") || p.contains("BaseColor") || p.contains("Base Color"))
                })
                .map(|(c, _)| *c)
        });
        let image = texture.and_then(|t| self.texture_image(t));
        // A texture shows its own colours.
        (if image.is_some() { [1.0; 3] } else { color }, image)
    }

    fn texture_image(&self, texture: i64) -> Option<Vec<u8>> {
        // Embedded: a Video object with Content.
        for &(c, _) in self.children.get(&texture).into_iter().flatten() {
            if self.kind(c) == Some("Video") {
                if let Some(Prop::Str(content)) = self.objects[&c].value("Content") {
                    if !content.is_empty() {
                        return Some(content.clone());
                    }
                }
            }
        }
        let node = self.objects[&texture];
        let dir = self.dir?;
        for key in ["RelativeFilename", "FileName"] {
            let Some(path) = node.value(key).and_then(Prop::text) else { continue };
            let path = path.replace('\\', "/");
            let file = Path::new(&path);
            for candidate in [dir.join(file), dir.join(file.file_name()?), dir.join("textures").join(file.file_name()?)] {
                if let Ok(bytes) = std::fs::read(candidate) {
                    return Some(bytes);
                }
            }
        }
        None
    }
}

type Corners = (Vec<[f32; 3]>, Option<Vec<[f32; 3]>>, Option<Vec<[f32; 2]>>, Vec<[u32; 3]>);

/// A geometry's corners (one vertex per polygon corner) and fan triangles.
fn mesh_corners(geometry: &Node) -> Option<Corners> {
    let points = geometry.value("Vertices")?.floats()?;
    let polygon_index = geometry.value("PolygonVertexIndex")?.ints()?;
    let control = |i: usize| -> Option<[f32; 3]> {
        Some([*points.get(i * 3)? as f32, *points.get(i * 3 + 1)? as f32, *points.get(i * 3 + 2)? as f32])
    };
    // Control point and polygon of each corner.
    let mut corner_point = Vec::with_capacity(polygon_index.len());
    let mut corner_polygon = Vec::with_capacity(polygon_index.len());
    let mut triangles = Vec::new();
    let (mut start, mut polygon) = (0usize, 0usize);
    for (k, &raw) in polygon_index.iter().enumerate() {
        let last = raw < 0;
        corner_point.push(if last { !raw } else { raw } as usize);
        corner_polygon.push(polygon);
        if last {
            for i in start + 1..k {
                triangles.push([start as u32, i as u32, i as u32 + 1]);
            }
            start = k + 1;
            polygon += 1;
        }
    }
    let positions: Vec<[f32; 3]> = corner_point.iter().map(|&p| control(p).unwrap_or([0.0; 3])).collect();
    let layer = |element: &str, data: &str, index: &str, comps: usize| -> Option<Vec<Vec<f32>>> {
        let e = geometry.child(element)?;
        let values = e.value(data)?.floats()?;
        let mapping = e.value("MappingInformationType").and_then(Prop::text).unwrap_or_default();
        let reference = e.value("ReferenceInformationType").and_then(Prop::text).unwrap_or_default();
        let indices =
            if reference.starts_with("IndexToDirect") || reference == "Index" { e.value(index).and_then(Prop::ints) } else { None };
        let get = |i: usize| -> Vec<f32> {
            let i = indices.as_ref().map_or(Some(i as i64), |ix| ix.get(i).copied()).filter(|&i| i >= 0).unwrap_or(0) as usize;
            (0..comps).map(|c| values.get(i * comps + c).copied().unwrap_or(0.0) as f32).collect()
        };
        Some(
            (0..corner_point.len())
                .map(|k| match mapping.as_str() {
                    "ByVertice" | "ByVertex" => get(corner_point[k]),
                    "ByPolygon" => get(corner_polygon[k]),
                    "AllSame" => get(0),
                    _ => get(k),
                })
                .collect(),
        )
    };
    let normals =
        layer("LayerElementNormal", "Normals", "NormalsIndex", 3).map(|n| n.into_iter().map(|v| [v[0], v[1], v[2]]).collect());
    // FBX v runs up, as the scene's does.
    let uvs = layer("LayerElementUV", "UV", "UVIndex", 2).map(|u| u.into_iter().map(|v| [v[0], v[1]]).collect());
    Some((positions, normals, uvs, triangles))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Writes test FBX documents (version 7400, 32-bit offsets).
    enum P {
        I(i32),
        L(i64),
        D(f64),
        S(&'static str),
        Dz(Vec<f64>),
        Iz(Vec<i32>),
    }

    fn node(out: &mut Vec<u8>, name: &str, props: &[P], children: &dyn Fn(&mut Vec<u8>)) {
        let start = out.len();
        out.extend([0u8; 12]);
        out.push(name.len() as u8);
        out.extend(name.as_bytes());
        let props_start = out.len();
        for p in props {
            match p {
                P::I(v) => {
                    out.push(b'I');
                    out.extend(v.to_le_bytes());
                }
                P::L(v) => {
                    out.push(b'L');
                    out.extend(v.to_le_bytes());
                }
                P::D(v) => {
                    out.push(b'D');
                    out.extend(v.to_le_bytes());
                }
                P::S(s) => {
                    out.push(b'S');
                    out.extend((s.len() as u32).to_le_bytes());
                    out.extend(s.as_bytes());
                }
                // Doubles compressed, ints plain: both encodings get tested.
                P::Dz(v) => {
                    let raw: Vec<u8> = v.iter().flat_map(|d| d.to_le_bytes()).collect();
                    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                    z.write_all(&raw).unwrap();
                    let z = z.finish().unwrap();
                    out.push(b'd');
                    out.extend((v.len() as u32).to_le_bytes());
                    out.extend(1u32.to_le_bytes());
                    out.extend((z.len() as u32).to_le_bytes());
                    out.extend(z);
                }
                P::Iz(v) => {
                    out.push(b'i');
                    out.extend((v.len() as u32).to_le_bytes());
                    out.extend(0u32.to_le_bytes());
                    out.extend((v.len() as u32 * 4).to_le_bytes());
                    out.extend(v.iter().flat_map(|i| i.to_le_bytes()));
                }
            }
        }
        let props_len = (out.len() - props_start) as u32;
        let before = out.len();
        children(out);
        if out.len() > before {
            out.extend([0u8; 13]);
        }
        let end = out.len() as u32;
        out[start..start + 4].copy_from_slice(&end.to_le_bytes());
        out[start + 4..start + 8].copy_from_slice(&(props.len() as u32).to_le_bytes());
        out[start + 8..start + 12].copy_from_slice(&props_len.to_le_bytes());
    }

    fn p70(out: &mut Vec<u8>, name: &'static str, values: [f64; 3]) {
        node(out, "P", &[P::S(name), P::S("Lcl"), P::S(""), P::S("A"), P::D(values[0]), P::D(values[1]), P::D(values[2])], &|_| {});
    }

    /// A unit quad (one 4-sided polygon) in a model moved 2 units up (+Y), unit cm.
    fn quad_fbx() -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend([0x1A, 0x00]);
        out.extend(7400u32.to_le_bytes());
        node(&mut out, "GlobalSettings", &[], &|o| {
            node(o, "Properties70", &[], &|o| {
                node(o, "P", &[P::S("UpAxis"), P::S("int"), P::S("Integer"), P::S(""), P::I(1)], &|_| {});
                node(o, "P", &[P::S("UnitScaleFactor"), P::S("double"), P::S("Number"), P::S(""), P::D(100.0)], &|_| {});
            })
        });
        node(&mut out, "Objects", &[], &|o| {
            node(o, "Geometry", &[P::L(10), P::S("Quad\0\x01Geometry"), P::S("Mesh")], &|o| {
                node(o, "Vertices", &[P::Dz(vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0])], &|_| {});
                node(o, "PolygonVertexIndex", &[P::Iz(vec![0, 1, 2, !3])], &|_| {});
            });
            node(o, "Model", &[P::L(20), P::S("Floor\0\x01Model"), P::S("Mesh")], &|o| {
                node(o, "Properties70", &[], &|o| p70(o, "Lcl Translation", [0.0, 2.0, 0.0]));
            });
            node(o, "Material", &[P::L(30), P::S("Red\0\x01Material"), P::S("")], &|o| {
                node(o, "Properties70", &[], &|o| p70(o, "DiffuseColor", [1.0, 0.0, 0.0]));
            });
        });
        node(&mut out, "Connections", &[], &|o| {
            node(o, "C", &[P::S("OO"), P::L(10), P::L(20)], &|_| {});
            node(o, "C", &[P::S("OO"), P::L(20), P::L(0)], &|_| {});
            node(o, "C", &[P::S("OO"), P::L(30), P::L(20)], &|_| {});
        });
        out.extend([0u8; 13]);
        out
    }

    #[test]
    fn reads_binary_fbx() {
        let objects = read(&quad_fbx(), None).unwrap();
        assert_eq!(objects.len(), 1);
        let o = &objects[0];
        assert_eq!(o.name, "Floor");
        // One quad: two triangles.
        assert_eq!(o.mesh.indices.len(), 6);
        assert_eq!(o.color, [1.0, 0.0, 0.0]);
        // Y-up +2 is Z-up +2; the quad (x 0..1, file z 0..1 → scene y 0..−1) centres at (0.5, −0.5, 2).
        let l = o.location;
        assert!((l.x - 0.5).abs() < 1e-5 && (l.y + 0.5).abs() < 1e-5 && (l.z - 2.0).abs() < 1e-5, "{l:?}");
        assert!(read(b"; FBX 7.4.0 project file", None).unwrap_err().contains("ASCII"));
    }
}
