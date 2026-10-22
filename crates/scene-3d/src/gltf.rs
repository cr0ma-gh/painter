//! glTF 2.0 models (`.gltf` with its buffers, or a single binary `.glb`), from
//! the Khronos specification: the default scene's node tree (matrix or
//! translation / rotation / scale per node), each node's mesh primitives as
//! triangles (positions, normals, first UV set, indices), and each material's
//! base colour and base colour image (embedded, in the GLB, or a file beside).

use std::path::Path;

use serde_json::Value;

use crate::import::{object, ImportedObject, Y_UP_TO_Z_UP};
use crate::math::{vec3, Mat4, Quat};

/// Reads a `.gltf` or `.glb` file (`path` finds the files a `.gltf` refers to).
pub fn load(path: &Path) -> Result<Vec<ImportedObject>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    read(&bytes, path.parent())
}

/// A glTF document from its bytes; `dir` holds the files it refers to.
pub fn read(bytes: &[u8], dir: Option<&Path>) -> Result<Vec<ImportedObject>, String> {
    let (json, bin) = if bytes.starts_with(b"glTF") { split_glb(bytes)? } else { (bytes.to_vec(), None) };
    let doc: Value = serde_json::from_slice(&json).map_err(|e| format!("glTF: {e}"))?;
    let buffers: Vec<Vec<u8>> = doc["buffers"]
        .as_array()
        .map(|list| {
            list.iter()
                .enumerate()
                .map(|(i, b)| match b["uri"].as_str() {
                    Some(uri) => load_uri(uri, dir).unwrap_or_default(),
                    // The GLB's own binary chunk is buffer 0.
                    None if i == 0 => bin.clone().unwrap_or_default(),
                    None => Vec::new(),
                })
                .collect()
        })
        .unwrap_or_default();
    let reader = Reader { doc: &doc, buffers, dir };
    let scene = doc["scene"].as_u64().unwrap_or(0) as usize;
    let roots: Vec<usize> = match doc["scenes"].get(scene) {
        Some(s) => {
            s["nodes"].as_array().map(|n| n.iter().filter_map(|v| v.as_u64()).map(|v| v as usize).collect()).unwrap_or_default()
        }
        // No scenes: every node that isn't someone's child.
        None => {
            let nodes = doc["nodes"].as_array().map_or(0, |n| n.len());
            let children: Vec<usize> = doc["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|n| n["children"].as_array().into_iter().flatten().filter_map(|c| c.as_u64()).map(|c| c as usize))
                .collect();
            (0..nodes).filter(|i| !children.contains(i)).collect()
        }
    };
    let mut out = Vec::new();
    for root in roots {
        reader.node(root, &Y_UP_TO_Z_UP, &mut out, 0);
    }
    if out.is_empty() {
        return Err("no meshes in the file".into());
    }
    Ok(out)
}

/// The JSON and binary chunks of a GLB.
fn split_glb(bytes: &[u8]) -> Result<(Vec<u8>, Option<Vec<u8>>), String> {
    let u32_at = |i: usize| bytes.get(i..i + 4).map(|b| u32::from_le_bytes(b.try_into().expect("4 bytes")) as usize);
    let total = u32_at(8).ok_or("truncated GLB")?.min(bytes.len());
    let (mut json, mut bin) = (None, None);
    let mut at = 12;
    while at + 8 <= total {
        let (len, kind) = (u32_at(at).ok_or("truncated GLB")?, u32_at(at + 4).ok_or("truncated GLB")?);
        let data = bytes.get(at + 8..at + 8 + len).ok_or("truncated GLB chunk")?.to_vec();
        match kind {
            0x4E4F_534A => json = Some(data),
            0x004E_4942 => bin = Some(data),
            _ => {}
        }
        at += 8 + len;
    }
    Ok((json.ok_or("GLB without JSON")?, bin))
}

/// A buffer or image URI: embedded (`data:…;base64,`) or a file beside the model.
fn load_uri(uri: &str, dir: Option<&Path>) -> Option<Vec<u8>> {
    if let Some(rest) = uri.strip_prefix("data:") {
        let (_, data) = rest.split_once(";base64,")?;
        return base64(data);
    }
    let name = percent_decode(uri);
    std::fs::read(dir?.join(name)).ok()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Standard base64 (padding optional, whitespace ignored).
pub(crate) fn base64(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in text.bytes() {
        if c == b'=' {
            break;
        }
        if c.is_ascii_whitespace() {
            continue;
        }
        acc = (acc << 6) | value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

struct Reader<'a> {
    doc: &'a Value,
    buffers: Vec<Vec<u8>>,
    dir: Option<&'a Path>,
}

impl Reader<'_> {
    fn node(&self, index: usize, parent: &Mat4, out: &mut Vec<ImportedObject>, depth: usize) {
        let Some(node) = self.doc["nodes"].get(index) else { return };
        if depth > 64 {
            return;
        }
        let local = match node["matrix"].as_array() {
            Some(m) if m.len() == 16 => {
                let v: Vec<f32> = m.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect();
                Mat4(std::array::from_fn(|c| std::array::from_fn(|r| v[c * 4 + r])))
            }
            _ => {
                let f = |key: &str, default: &[f32]| -> Vec<f32> {
                    node[key]
                        .as_array()
                        .map(|a| a.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect())
                        .filter(|v: &Vec<f32>| v.len() == default.len())
                        .unwrap_or_else(|| default.to_vec())
                };
                let (t, r, s) = (f("translation", &[0.0; 3]), f("rotation", &[0.0, 0.0, 0.0, 1.0]), f("scale", &[1.0; 3]));
                Mat4::from_trs(vec3(t[0], t[1], t[2]), Quat { x: r[0], y: r[1], z: r[2], w: r[3] }.normalize(), vec3(s[0], s[1], s[2]))
            }
        };
        let world = *parent * local;
        if let Some(mesh) = node["mesh"].as_u64() {
            let name = node["name"].as_str().or_else(|| self.doc["meshes"][mesh as usize]["name"].as_str()).unwrap_or("Mesh");
            self.mesh(mesh as usize, name, &world, out);
        }
        for child in node["children"].as_array().into_iter().flatten().filter_map(|c| c.as_u64()) {
            self.node(child as usize, &world, out, depth + 1);
        }
    }

    fn mesh(&self, index: usize, name: &str, world: &Mat4, out: &mut Vec<ImportedObject>) {
        let primitives = self.doc["meshes"][index]["primitives"].as_array().cloned().unwrap_or_default();
        let several = primitives.len() > 1;
        for (k, p) in primitives.iter().enumerate() {
            // Triangles only (mode 4, the default).
            if p["mode"].as_u64().unwrap_or(4) != 4 {
                continue;
            }
            let attr = &p["attributes"];
            let Some(positions) = attr["POSITION"].as_u64().and_then(|a| self.floats(a as usize, 3)) else { continue };
            let normals = attr["NORMAL"].as_u64().and_then(|a| self.floats(a as usize, 3));
            let uvs = attr["TEXCOORD_0"].as_u64().and_then(|a| self.floats(a as usize, 2));
            let count = positions.len() / 3;
            let indices: Vec<u32> = match p["indices"].as_u64() {
                Some(a) => self.indices(a as usize).unwrap_or_default(),
                None => (0..count as u32).collect(),
            };
            let triangles: Vec<[u32; 3]> = indices.as_chunks::<3>().0.to_vec();
            let pos: Vec<[f32; 3]> = positions.as_chunks::<3>().0.to_vec();
            let nor = normals.map(|n| n.as_chunks::<3>().0.to_vec());
            // glTF's v runs down the image; the scene's runs up.
            let uv = uvs.map(|u| u.as_chunks::<2>().0.iter().map(|t| [t[0], 1.0 - t[1]]).collect());
            let label = if several { format!("{name} {}", k + 1) } else { name.to_string() };
            let Some((mesh, centre)) = object(&label, pos, nor, uv, triangles, world) else { continue };
            let (color, image) = self.material(p["material"].as_u64());
            out.push(ImportedObject { name: label, mesh, location: centre, color, image });
        }
    }

    /// Base colour and image of material `index`.
    fn material(&self, index: Option<u64>) -> ([f32; 3], Option<Vec<u8>>) {
        let Some(m) = index.and_then(|i| self.doc["materials"].get(i as usize)) else { return (crate::scene::CLAY, None) };
        let pbr = &m["pbrMetallicRoughness"];
        let color = pbr["baseColorFactor"]
            .as_array()
            .filter(|a| a.len() >= 3)
            .map(|a| std::array::from_fn(|i| a[i].as_f64().unwrap_or(1.0) as f32))
            .unwrap_or([1.0; 3]);
        let image = pbr["baseColorTexture"]["index"]
            .as_u64()
            .and_then(|t| self.doc["textures"][t as usize]["source"].as_u64())
            .and_then(|i| self.image(i as usize));
        (color, image)
    }

    fn image(&self, index: usize) -> Option<Vec<u8>> {
        let img = self.doc["images"].get(index)?;
        if let Some(uri) = img["uri"].as_str() {
            return load_uri(uri, self.dir);
        }
        self.view(img["bufferView"].as_u64()? as usize).map(|(b, _)| b.to_vec())
    }

    /// A buffer view's bytes and stride.
    fn view(&self, index: usize) -> Option<(&[u8], usize)> {
        let v = self.doc["bufferViews"].get(index)?;
        let buffer = self.buffers.get(v["buffer"].as_u64()? as usize)?;
        let offset = v["byteOffset"].as_u64().unwrap_or(0) as usize;
        let len = v["byteLength"].as_u64()? as usize;
        Some((buffer.get(offset..offset + len)?, v["byteStride"].as_u64().unwrap_or(0) as usize))
    }

    /// An accessor's elements as numbers: (values, components per element).
    fn values(&self, index: usize) -> Option<(Vec<f32>, usize)> {
        let a = self.doc["accessors"].get(index)?;
        let comps = match a["type"].as_str()? {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" => 4,
            _ => return None,
        };
        let count = a["count"].as_u64()? as usize;
        let kind = a["componentType"].as_u64()?;
        let size = match kind {
            5120 | 5121 => 1,
            5122 | 5123 => 2,
            5125 | 5126 => 4,
            _ => return None,
        };
        let normalized = a["normalized"].as_bool().unwrap_or(false);
        let (data, stride) = match a["bufferView"].as_u64() {
            Some(v) => self.view(v as usize)?,
            // No view: all zeros.
            None => return Some((vec![0.0; count * comps], comps)),
        };
        let offset = a["byteOffset"].as_u64().unwrap_or(0) as usize;
        let stride = if stride == 0 { size * comps } else { stride };
        let mut out = Vec::with_capacity(count * comps);
        for e in 0..count {
            for c in 0..comps {
                let at = offset + e * stride + c * size;
                let b = data.get(at..at + size)?;
                let v = match kind {
                    5126 => f32::from_le_bytes(b.try_into().ok()?),
                    5125 => u32::from_le_bytes(b.try_into().ok()?) as f32,
                    5123 => {
                        let v = u16::from_le_bytes(b.try_into().ok()?) as f32;
                        if normalized {
                            v / 65535.0
                        } else {
                            v
                        }
                    }
                    5122 => {
                        let v = i16::from_le_bytes(b.try_into().ok()?) as f32;
                        if normalized {
                            (v / 32767.0).max(-1.0)
                        } else {
                            v
                        }
                    }
                    5121 => {
                        let v = b[0] as f32;
                        if normalized {
                            v / 255.0
                        } else {
                            v
                        }
                    }
                    _ => {
                        let v = b[0] as i8 as f32;
                        if normalized {
                            (v / 127.0).max(-1.0)
                        } else {
                            v
                        }
                    }
                };
                out.push(v);
            }
        }
        Some((out, comps))
    }

    fn floats(&self, index: usize, comps: usize) -> Option<Vec<f32>> {
        let (v, c) = self.values(index)?;
        (c == comps).then_some(v)
    }

    fn indices(&self, index: usize) -> Option<Vec<u32>> {
        // Read as integers directly (u32 indices don't survive a trip through f32).
        let a = self.doc["accessors"].get(index)?;
        let kind = a["componentType"].as_u64()?;
        let count = a["count"].as_u64()? as usize;
        let (data, stride) = self.view(a["bufferView"].as_u64()? as usize)?;
        let size = match kind {
            5121 => 1,
            5123 => 2,
            5125 => 4,
            _ => return None,
        };
        let offset = a["byteOffset"].as_u64().unwrap_or(0) as usize;
        let stride = if stride == 0 { size } else { stride };
        (0..count)
            .map(|e| {
                let at = offset + e * stride;
                let b = data.get(at..at + size)?;
                Some(match size {
                    1 => b[0] as u32,
                    2 => u16::from_le_bytes([b[0], b[1]]) as u32,
                    _ => u32::from_le_bytes(b.try_into().ok()?),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-triangle glTF with an embedded buffer, under a node moved along +Y (up in glTF).
    fn triangle_gltf() -> Vec<u8> {
        let mut bin = Vec::new();
        for v in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            bin.extend(v.to_le_bytes());
        }
        for i in [0u16, 1, 2, 0] {
            bin.extend(i.to_le_bytes()); // padded to 4 bytes
        }
        let b64 = encode(&bin);
        format!(
            r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],
            "nodes":[{{"name":"Tri","mesh":0,"translation":[0,5,0]}}],
            "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},"indices":1,"material":0}}]}}],
            "materials":[{{"pbrMetallicRoughness":{{"baseColorFactor":[1,0.5,0.25,1]}}}}],
            "buffers":[{{"byteLength":{},"uri":"data:application/octet-stream;base64,{b64}"}}],
            "bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":36}},{{"buffer":0,"byteOffset":36,"byteLength":6}}],
            "accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}},{{"bufferView":1,"componentType":5123,"count":3,"type":"SCALAR"}}]}}"#,
            bin.len()
        )
        .into_bytes()
    }

    fn encode(data: &[u8]) -> String {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in data.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for k in 0..4 {
                s.push(if k <= c.len() { T[(n >> (18 - 6 * k) & 63) as usize] as char } else { '=' });
            }
        }
        s
    }

    #[test]
    fn reads_gltf_and_glb() {
        let text = triangle_gltf();
        let objects = read(&text, None).unwrap();
        assert_eq!(objects.len(), 1);
        let o = &objects[0];
        assert_eq!(o.name, "Tri");
        assert_eq!(o.mesh.indices.len(), 3);
        assert_eq!(o.color, [1.0, 0.5, 0.25]);
        // glTF's +Y (the node's offset) is the scene's +Z.
        assert!((o.location.z - 5.0 - 1.0 / 2.0).abs() < 1e-4, "{:?}", o.location);
        assert!(o.location.y.abs() < 1e-4);

        // The same document as a GLB: JSON chunk then binary chunk (here the buffer stays a data URI).
        let mut json = text.clone();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut glb = b"glTF".to_vec();
        glb.extend(2u32.to_le_bytes());
        glb.extend((12 + 8 + json.len() as u32).to_le_bytes());
        glb.extend((json.len() as u32).to_le_bytes());
        glb.extend(0x4E4F_534Au32.to_le_bytes());
        glb.extend(&json);
        assert_eq!(read(&glb, None).unwrap()[0].mesh.indices.len(), 3);
        assert_eq!(base64("aGVsbG8=").unwrap(), b"hello");
    }
}
