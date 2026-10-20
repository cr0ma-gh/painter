//! A scene as bytes, for project files: `P3DS`, a format version, a JSON
//! header (objects, transforms, camera, lights) and then the meshes' raw
//! vertex (32 bytes each) and `u32` index data, little-endian. Objects that
//! share a mesh share it in the file too.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::camera::{OrbitStyle, Projection, ViewCamera};
use crate::gizmo::Transform;
use crate::math::{Quat, Vec3};
use crate::mesh::{Mesh, Vertex};
use crate::scene::{Light, LightKind, Scene3D, SceneObject};

const MAGIC: &[u8; 4] = b"P3DS";
const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Header {
    camera: CameraFile,
    lights: Vec<LightFile>,
    ambient: f32,
    selected: Option<usize>,
    meshes: Vec<MeshFile>,
    objects: Vec<ObjectFile>,
}

#[derive(Serialize, Deserialize)]
struct CameraFile {
    pivot: [f32; 3],
    distance: f32,
    /// w, x, y, z.
    rotation: [f32; 4],
    /// Vertical FOV in radians; `None` = orthographic.
    fov_y: Option<f32>,
    trackball: bool,
    near: f32,
    far: f32,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LightKindFile {
    Sun { direction: [f32; 3] },
    Point { position: [f32; 3], range: f32 },
}

#[derive(Serialize, Deserialize)]
struct LightFile {
    #[serde(flatten)]
    kind: LightKindFile,
    color: [f32; 3],
    intensity: f32,
}

#[derive(Serialize, Deserialize)]
struct MeshFile {
    name: String,
    vertices: usize,
    indices: usize,
}

#[derive(Serialize, Deserialize)]
struct ObjectFile {
    name: String,
    mesh: usize,
    location: [f32; 3],
    rotation: [f32; 4],
    scale: [f32; 3],
    visible: bool,
    color: [f32; 3],
}

impl Scene3D {
    pub fn to_bytes(&self) -> Vec<u8> {
        // Distinct meshes, in first-use order.
        let mut index: HashMap<usize, usize> = HashMap::new();
        let mut meshes: Vec<&Arc<Mesh>> = Vec::new();
        for o in &self.objects {
            index.entry(Arc::as_ptr(&o.mesh) as usize).or_insert_with(|| {
                meshes.push(&o.mesh);
                meshes.len() - 1
            });
        }
        let cam = &self.camera;
        let q = cam.rotation;
        let header = Header {
            camera: CameraFile {
                pivot: cam.pivot.to_array(),
                distance: cam.distance,
                rotation: [q.w, q.x, q.y, q.z],
                fov_y: match cam.projection {
                    Projection::Perspective { fov_y } => Some(fov_y),
                    Projection::Orthographic => None,
                },
                trackball: cam.orbit_style == OrbitStyle::Trackball,
                near: cam.near,
                far: cam.far,
            },
            lights: self
                .lights
                .iter()
                .map(|l| LightFile {
                    kind: match l.kind {
                        LightKind::Sun { direction } => LightKindFile::Sun { direction: direction.to_array() },
                        LightKind::Point { position, range } => LightKindFile::Point { position: position.to_array(), range },
                    },
                    color: l.color,
                    intensity: l.intensity,
                })
                .collect(),
            ambient: self.ambient,
            selected: self.selected,
            meshes: meshes.iter().map(|m| MeshFile { name: m.name.clone(), vertices: m.vertices.len(), indices: m.indices.len() }).collect(),
            objects: self
                .objects
                .iter()
                .map(|o| {
                    let r = o.transform.rotation;
                    ObjectFile {
                        name: o.name.clone(),
                        mesh: index[&(Arc::as_ptr(&o.mesh) as usize)],
                        location: o.transform.location.to_array(),
                        rotation: [r.w, r.x, r.y, r.z],
                        scale: o.transform.scale.to_array(),
                        visible: o.visible,
                        color: o.color,
                    }
                })
                .collect(),
        };
        let json = serde_json::to_vec(&header).unwrap_or_default();
        let mut out = Vec::with_capacity(12 + json.len() + meshes.iter().map(|m| m.vertex_bytes().len() + m.index_bytes().len()).sum::<usize>());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&FORMAT.to_le_bytes());
        out.extend_from_slice(&(json.len() as u32).to_le_bytes());
        out.extend_from_slice(&json);
        for m in meshes {
            for v in &m.vertices {
                for f in v.position.iter().chain(&v.normal).chain(&v.uv) {
                    out.extend_from_slice(&f.to_le_bytes());
                }
            }
            for i in &m.indices {
                out.extend_from_slice(&i.to_le_bytes());
            }
        }
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Scene3D, String> {
        if bytes.get(..4) != Some(MAGIC) {
            return Err("not a saved 3D scene".into());
        }
        let u32_at = |i: usize| bytes.get(i..i + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        let format = u32_at(4).ok_or("truncated")?;
        if format > FORMAT {
            return Err(format!("3D scene made by a newer version (format {format})"));
        }
        let len = u32_at(8).ok_or("truncated")? as usize;
        let header: Header = serde_json::from_slice(bytes.get(12..12 + len).ok_or("truncated header")?).map_err(|e| format!("3D scene: {e}"))?;
        let mut at = 12 + len;
        let mut meshes = Vec::with_capacity(header.meshes.len());
        for m in &header.meshes {
            let vbytes = m.vertices.checked_mul(32).ok_or("mesh too big")?;
            let ibytes = m.indices.checked_mul(4).ok_or("mesh too big")?;
            let data = bytes.get(at..at + vbytes + ibytes).ok_or("truncated mesh data")?;
            at += vbytes + ibytes;
            let floats: Vec<f32> = data[..vbytes].as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
            let vertices: Vec<Vertex> = floats
                .as_chunks::<8>()
                .0
                .iter()
                .map(|f| Vertex { position: [f[0], f[1], f[2]], normal: [f[3], f[4], f[5]], uv: [f[6], f[7]] })
                .collect();
            let indices: Vec<u32> = data[vbytes..].as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b)).collect();
            if !indices.len().is_multiple_of(3) || indices.iter().any(|&i| i as usize >= vertices.len()) {
                return Err(format!("mesh {}: bad indices", m.name));
            }
            meshes.push(Arc::new(Mesh::from_buffers(&m.name, vertices, indices)));
        }
        // Renormalised only when off, so saved values come back bit-exact.
        let quat = |r: [f32; 4]| {
            let q = Quat { w: r[0], x: r[1], y: r[2], z: r[3] };
            if ((q.w * q.w + q.x * q.x + q.y * q.y + q.z * q.z) - 1.0).abs() > 1e-4 { q.normalize() } else { q }
        };
        let c = &header.camera;
        let camera = ViewCamera {
            pivot: Vec3::from_array(c.pivot),
            distance: c.distance.max(1e-3),
            rotation: quat(c.rotation),
            projection: match c.fov_y {
                Some(fov_y) => Projection::Perspective { fov_y },
                None => Projection::Orthographic,
            },
            orbit_style: if c.trackball { OrbitStyle::Trackball } else { OrbitStyle::Turntable },
            near: c.near,
            far: c.far,
            ..ViewCamera::default()
        };
        let mut objects = Vec::with_capacity(header.objects.len());
        for o in header.objects {
            let mesh = meshes.get(o.mesh).ok_or_else(|| format!("object {} refers to a missing mesh", o.name))?.clone();
            objects.push(SceneObject {
                name: o.name,
                mesh,
                transform: Transform { location: Vec3::from_array(o.location), rotation: quat(o.rotation), scale: Vec3::from_array(o.scale) },
                visible: o.visible,
                color: o.color,
            });
        }
        let lights = header
            .lights
            .into_iter()
            .map(|l| Light {
                kind: match l.kind {
                    LightKindFile::Sun { direction } => LightKind::Sun {
                        direction: {
                            let d = Vec3::from_array(direction);
                            if (d.length() - 1.0).abs() > 1e-4 { d.normalize() } else { d }
                        },
                    },
                    LightKindFile::Point { position, range } => LightKind::Point { position: Vec3::from_array(position), range },
                },
                color: l.color,
                intensity: l.intensity,
            })
            .collect();
        let selected = header.selected.filter(|&i| i < objects.len());
        Ok(Scene3D { objects, camera, lights, ambient: header.ambient, selected })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::ViewPreset;
    use crate::math::vec3;
    use crate::scene::CLAY;

    #[test]
    fn round_trips_everything() {
        let cube = Arc::new(Mesh::cube());
        let mut scene = Scene3D::default();
        for (name, x) in [("a", -2.0), ("b", 2.0)] {
            scene.objects.push(SceneObject {
                name: name.into(),
                mesh: cube.clone(),
                transform: Transform { location: vec3(x, 1.0, 0.5), rotation: Quat::from_euler_xyz([0.3, 0.0, 1.0]), scale: vec3(1.0, 2.0, 1.0) },
                visible: name == "a",
                color: CLAY,
            });
        }
        scene.selected = Some(1);
        scene.camera.set_view(ViewPreset::Top);
        scene.camera.toggle_projection();
        scene.lights.push(Light { kind: LightKind::Point { position: vec3(1.0, 2.0, 3.0), range: 9.0 }, color: [1.0, 0.5, 0.2], intensity: 2.0 });
        scene.ambient = 0.4;

        let bytes = scene.to_bytes();
        let back = Scene3D::from_bytes(&bytes).unwrap();
        assert_eq!(back.objects.len(), 2);
        assert!(Arc::ptr_eq(&back.objects[0].mesh, &back.objects[1].mesh), "a shared mesh stays shared");
        assert_eq!(*back.objects[0].mesh, *cube);
        assert_eq!(back.objects[1].transform.location, scene.objects[1].transform.location);
        assert_eq!(back.objects[1].transform.scale, scene.objects[1].transform.scale);
        assert!((back.objects[0].transform.rotation.w - scene.objects[0].transform.rotation.w).abs() < 1e-6);
        assert!(!back.objects[1].visible);
        assert_eq!(back.selected, Some(1));
        assert_eq!(back.camera.projection, Projection::Orthographic);
        assert!((back.camera.forward() - scene.camera.forward()).length() < 1e-5);
        assert_eq!(back.lights, scene.lights);
        assert_eq!(back.ambient, 0.4);
        // One copy of the cube: header + 24 vertices × 32 + 36 indices × 4.
        assert!(bytes.len() < 2 * (24 * 32 + 36 * 4) + 2000);

        assert!(Scene3D::from_bytes(b"nope").is_err());
        assert!(Scene3D::from_bytes(&bytes[..bytes.len() - 10]).is_err(), "truncated");
    }
}
