//! Meshes ready for the GPU: an interleaved vertex buffer and a `u32` index
//! buffer of triangles, plus bounds for picking. Polygons (n-gons from
//! Blender faces or OBJ) are fan-triangulated; each face corner becomes a
//! vertex, so per-corner UVs and flat normals stay exact. Also a small OBJ
//! reader; glTF / FBX are planned through dedicated crates.

use crate::math::{vec3, Vec3};

/// One GPU vertex: 32 bytes, `#[repr(C)]` so it can be uploaded as is
/// (`Float32x3` position, `Float32x3` normal, `Float32x2` uv).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

/// Axis-aligned bounds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn radius(&self) -> f32 {
        (self.max - self.min).length() * 0.5
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mesh {
    pub name: String,
    pub vertices: Vec<Vertex>,
    /// Triangles, three indices each.
    pub indices: Vec<u32>,
    pub bounds: Aabb,
}

impl Mesh {
    /// Builds a mesh from polygons given as lists of corner ids; `corner`
    /// turns a corner id into its vertex. Normals are the face normals
    /// (Newell's method, robust for non-planar n-gons).
    pub fn from_polygons(name: &str, faces: &[Vec<usize>], corner: impl Fn(usize) -> Vertex) -> Mesh {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for face in faces.iter().filter(|f| f.len() >= 3) {
            let corners: Vec<Vertex> = face.iter().map(|&c| corner(c)).collect();
            let mut n = Vec3::ZERO;
            for i in 0..corners.len() {
                let (a, b) = (Vec3::from_array(corners[i].position), Vec3::from_array(corners[(i + 1) % corners.len()].position));
                n = n + vec3((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
            }
            let normal = n.normalize().to_array();
            let base = vertices.len() as u32;
            vertices.extend(corners.into_iter().map(|v| Vertex { normal, ..v }));
            for i in 1..face.len() as u32 - 1 {
                indices.extend([base, base + i, base + i + 1]);
            }
        }
        Mesh::from_buffers(name, vertices, indices)
    }

    /// A mesh from ready vertex and triangle-index buffers.
    pub fn from_buffers(name: &str, vertices: Vec<Vertex>, indices: Vec<u32>) -> Mesh {
        let bounds = bounds(&vertices);
        Mesh { name: name.to_string(), vertices, indices, bounds }
    }

    /// The vertex buffer as bytes for `wgpu::util::BufferInitDescriptor`.
    pub fn vertex_bytes(&self) -> &[u8] {
        // SAFETY: Vertex is repr(C), eight f32s, no padding.
        unsafe { std::slice::from_raw_parts(self.vertices.as_ptr().cast(), std::mem::size_of_val(self.vertices.as_slice())) }
    }

    pub fn index_bytes(&self) -> &[u8] {
        // SAFETY: u32 has no padding.
        unsafe { std::slice::from_raw_parts(self.indices.as_ptr().cast(), std::mem::size_of_val(self.indices.as_slice())) }
    }

    pub fn triangles(&self) -> impl Iterator<Item = [Vec3; 3]> + '_ {
        self.indices.as_chunks::<3>().0.iter().map(|t| t.map(|i| Vec3::from_array(self.vertices[i as usize].position)))
    }

    /// Unit cube centred on the origin (default reference model, tests).
    pub fn cube() -> Mesh {
        let p = |x: f32, y: f32, z: f32| [x - 0.5, y - 0.5, z - 0.5];
        let corners = [p(0., 0., 0.), p(1., 0., 0.), p(1., 1., 0.), p(0., 1., 0.), p(0., 0., 1.), p(1., 0., 1.), p(1., 1., 1.), p(0., 1., 1.)];
        let faces: Vec<Vec<usize>> = vec![vec![0, 3, 2, 1], vec![4, 5, 6, 7], vec![0, 1, 5, 4], vec![1, 2, 6, 5], vec![2, 3, 7, 6], vec![3, 0, 4, 7]];
        Mesh::from_polygons("Cube", &faces, |c| Vertex { position: corners[c], ..Vertex::default() })
    }
}

fn bounds(vertices: &[Vertex]) -> Aabb {
    let mut min = vec3(f32::MAX, f32::MAX, f32::MAX);
    let mut max = vec3(f32::MIN, f32::MIN, f32::MIN);
    for v in vertices {
        let p = Vec3::from_array(v.position);
        (min, max) = (min.min(p), max.max(p));
    }
    if vertices.is_empty() {
        (min, max) = (Vec3::ZERO, Vec3::ZERO);
    }
    Aabb { min, max }
}

/// Reads Wavefront OBJ text: `v`, `vt`, `f` (with `v/vt/vn` forms, negative
/// indices). One mesh per file; groups and materials are ignored.
pub fn load_obj(text: &str, name: &str) -> Result<Mesh, String> {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    // Corners as (position, uv) index pairs.
    let mut corners: Vec<(usize, Option<usize>)> = Vec::new();
    let mut faces: Vec<Vec<usize>> = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        let mut parts = line.split_whitespace();
        let bad = || format!("line {}: {line}", line_no + 1);
        let nums = |parts: std::str::SplitWhitespace| parts.map(|s| s.parse::<f32>()).collect::<Result<Vec<f32>, _>>();
        match parts.next() {
            Some("v") => {
                let v = nums(parts).map_err(|_| bad())?;
                positions.push([*v.first().ok_or_else(bad)?, *v.get(1).ok_or_else(bad)?, *v.get(2).ok_or_else(bad)?]);
            }
            Some("vt") => {
                let v = nums(parts).map_err(|_| bad())?;
                uvs.push([*v.first().ok_or_else(bad)?, v.get(1).copied().unwrap_or(0.0)]);
            }
            Some("f") => {
                let resolve = |s: &str, len: usize| -> Option<usize> {
                    let i: i64 = s.parse().ok()?;
                    let idx = if i < 0 { len as i64 + i } else { i - 1 };
                    (0..len as i64).contains(&idx).then_some(idx as usize)
                };
                let mut face = Vec::new();
                for corner in parts {
                    let mut it = corner.split('/');
                    let p = resolve(it.next().unwrap_or(""), positions.len()).ok_or_else(bad)?;
                    let t = it.next().filter(|s| !s.is_empty()).and_then(|s| resolve(s, uvs.len()));
                    face.push(corners.len());
                    corners.push((p, t));
                }
                faces.push(face);
            }
            _ => {}
        }
    }
    if faces.is_empty() {
        return Err("no faces".into());
    }
    Ok(Mesh::from_polygons(name, &faces, |c| {
        let (p, t) = corners[c];
        Vertex { position: positions[p], normal: [0.0; 3], uv: t.map_or([0.0; 2], |t| uvs[t]) }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_is_twelve_outward_triangles() {
        let cube = Mesh::cube();
        assert_eq!(cube.indices.len(), 36);
        assert_eq!(cube.vertices.len(), 24, "per-corner vertices for flat normals");
        assert_eq!(std::mem::size_of::<Vertex>(), 32);
        assert_eq!(cube.vertex_bytes().len(), 24 * 32);
        for v in &cube.vertices {
            let (p, n) = (Vec3::from_array(v.position), Vec3::from_array(v.normal));
            assert!(p.dot(n) > 0.0, "normals point outwards");
        }
        assert_eq!(cube.bounds.center(), Vec3::ZERO);
    }

    #[test]
    fn reads_obj_with_ngons_and_uvs() {
        let obj = "# square + triangle\nv 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\nf 1/1 2/2 3/3 4/4\nf -4 -3 -1\n";
        let mesh = load_obj(obj, "test").unwrap();
        assert_eq!(mesh.indices.len(), 9);
        assert_eq!(mesh.vertices[2].uv, [1.0, 1.0]);
        assert_eq!(mesh.vertices[0].normal, [0.0, 0.0, 1.0]);
        assert!(load_obj("v 0 0 0\nf 1 2 3\n", "bad").is_err());
    }
}
