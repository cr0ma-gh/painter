//! What the model readers (glTF / GLB, FBX) produce, and their shared steps:
//! Y-up files turned to the scene's Z-up (as Blender's importers do: x, −z, y),
//! each object's world transform baked into its vertices, then the object
//! centred on its own origin, placed at `location`.

use crate::math::{vec3, Mat4, Vec3};
use crate::mesh::{Mesh, Vertex};

/// One object read from a model file.
#[derive(Debug, Clone)]
pub struct ImportedObject {
    pub name: String,
    pub mesh: Mesh,
    /// Where the object's centre is in the scene.
    pub location: Vec3,
    /// Base colour (linear-ish 0..1).
    pub color: [f32; 3],
    /// The base colour image, as the file's bytes (PNG or JPEG), when there is one.
    pub image: Option<Vec<u8>>,
}

/// Y-up (glTF, most FBX) to Z-up: x stays, y becomes z, z becomes −y.
pub(crate) const Y_UP_TO_Z_UP: Mat4 = Mat4([[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, -1.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]]);

/// Triangles with their corners (in the file's axes) as an object: `world`
/// (the axis change included) baked into the positions and normals, then centred.
pub(crate) fn object(
    name: &str,
    positions: Vec<[f32; 3]>,
    normals: Option<Vec<[f32; 3]>>,
    uvs: Option<Vec<[f32; 2]>>,
    triangles: Vec<[u32; 3]>,
    world: &Mat4,
) -> Option<(Mesh, Vec3)> {
    if positions.is_empty() || triangles.is_empty() {
        return None;
    }
    let n = positions.len() as u32;
    let triangles: Vec<[u32; 3]> = triangles.into_iter().filter(|t| t.iter().all(|&i| i < n)).collect();
    let moved: Vec<Vec3> = positions.iter().map(|p| world.transform_point(Vec3::from_array(*p))).collect();
    // Centre on the bounds' middle.
    let (mut lo, mut hi) = (vec3(f32::MAX, f32::MAX, f32::MAX), vec3(f32::MIN, f32::MIN, f32::MIN));
    for p in &moved {
        (lo, hi) = (lo.min(*p), hi.max(*p));
    }
    let centre = (lo + hi) * 0.5;
    let uv = |i: usize| uvs.as_ref().and_then(|u| u.get(i)).copied().unwrap_or([0.0, 0.0]);
    let mesh = match normals.filter(|n| n.len() == positions.len()) {
        Some(normals) => {
            let vertices: Vec<Vertex> = (0..positions.len())
                .map(|i| Vertex {
                    position: (moved[i] - centre).to_array(),
                    normal: world.transform_vector(Vec3::from_array(normals[i])).normalize().to_array(),
                    uv: uv(i),
                })
                .collect();
            Mesh::from_buffers(name, vertices, triangles.into_iter().flatten().collect())
        }
        // No normals in the file: flat ones from each triangle.
        None => {
            let faces: Vec<Vec<usize>> = triangles.iter().map(|t| t.iter().map(|&i| i as usize).collect()).collect();
            Mesh::from_polygons(name, &faces, |c| Vertex { position: (moved[c] - centre).to_array(), normal: [0.0; 3], uv: uv(c) })
        }
    };
    Some((mesh, centre))
}
