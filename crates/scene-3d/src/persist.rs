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
/// 2: textures (after the meshes, raw RGBA). 3: rigs (bones in the header,
/// per-vertex skin data after the textures) and each object's pose.
const FORMAT: u32 = 3;

#[derive(Serialize, Deserialize)]
struct Header {
    camera: CameraFile,
    lights: Vec<LightFile>,
    ambient: f32,
    selected: Option<usize>,
    meshes: Vec<MeshFile>,
    objects: Vec<ObjectFile>,
    #[serde(default)]
    textures: Vec<TextureFile>,
    #[serde(default)]
    rigs: Vec<RigFile>,
}

#[derive(Serialize, Deserialize)]
struct BoneFile {
    name: String,
    parent: Option<usize>,
    translation: [f32; 3],
    /// w, x, y, z.
    rotation: [f32; 4],
    scale: [f32; 3],
    /// Column-major.
    inverse_bind: [f32; 16],
}

/// A rig: its bones here, its per-vertex data (position, normal: 6 × f32;
/// joints: 4 × u16; weights: 4 × f32 — 48 bytes) after the textures.
#[derive(Serialize, Deserialize)]
struct RigFile {
    bones: Vec<BoneFile>,
    base: [f32; 16],
    centre: [f32; 3],
    vertices: usize,
}

const RIG_VERTEX_BYTES: usize = 48;

fn flat(m: &crate::math::Mat4) -> [f32; 16] {
    std::array::from_fn(|i| m.0[i / 4][i % 4])
}

fn unflat(v: [f32; 16]) -> crate::math::Mat4 {
    crate::math::Mat4(std::array::from_fn(|c| std::array::from_fn(|r| v[c * 4 + r])))
}

#[derive(Serialize, Deserialize)]
struct TextureFile {
    width: u32,
    height: u32,
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
    /// Lens shift (NDC); files from before it existed have none.
    #[serde(default)]
    shift: [f32; 2],
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
    #[serde(default)]
    texture: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rig: Option<usize>,
    /// Per bone, w x y z (empty: the rest pose).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pose: Vec<[f32; 4]>,
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
        // Distinct textures, the same way.
        let mut tex_index: HashMap<usize, usize> = HashMap::new();
        let mut textures: Vec<&Arc<crate::texture::Texture>> = Vec::new();
        for t in self.objects.iter().filter_map(|o| o.texture.as_ref()) {
            tex_index.entry(Arc::as_ptr(t) as usize).or_insert_with(|| {
                textures.push(t);
                textures.len() - 1
            });
        }
        // Distinct rigs, the same way.
        let mut rig_index: HashMap<usize, usize> = HashMap::new();
        let mut rigs: Vec<&Arc<crate::rig::Rig>> = Vec::new();
        for p in self.objects.iter().filter_map(|o| o.rig.as_ref()) {
            rig_index.entry(Arc::as_ptr(&p.rig) as usize).or_insert_with(|| {
                rigs.push(&p.rig);
                rigs.len() - 1
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
                shift: cam.shift,
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
            textures: textures.iter().map(|t| TextureFile { width: t.width, height: t.height }).collect(),
            rigs: rigs
                .iter()
                .map(|r| RigFile {
                    bones: r
                        .bones
                        .iter()
                        .map(|b| BoneFile {
                            name: b.name.clone(),
                            parent: b.parent,
                            translation: b.translation.to_array(),
                            rotation: [b.rotation.w, b.rotation.x, b.rotation.y, b.rotation.z],
                            scale: b.scale.to_array(),
                            inverse_bind: flat(&b.inverse_bind),
                        })
                        .collect(),
                    base: flat(&r.base),
                    centre: r.centre.to_array(),
                    vertices: r.positions.len(),
                })
                .collect(),
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
                        texture: o.texture.as_ref().map(|t| tex_index[&(Arc::as_ptr(t) as usize)]),
                        rig: o.rig.as_ref().map(|p| rig_index[&(Arc::as_ptr(&p.rig) as usize)]),
                        pose: o.rig.as_ref().filter(|p| !p.is_rest()).map_or_else(Vec::new, |p| p.pose.iter().map(|q| [q.w, q.x, q.y, q.z]).collect()),
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
        for t in textures {
            out.extend_from_slice(&t.rgba);
        }
        for r in rigs {
            for k in 0..r.positions.len() {
                for f in r.positions[k].iter().chain(&r.normals[k]) {
                    out.extend_from_slice(&f.to_le_bytes());
                }
                for j in r.joints[k] {
                    out.extend_from_slice(&j.to_le_bytes());
                }
                for w in r.weights[k] {
                    out.extend_from_slice(&w.to_le_bytes());
                }
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
        let mut textures = Vec::with_capacity(header.textures.len());
        for t in &header.textures {
            let len = (t.width as usize).checked_mul(t.height as usize).and_then(|n| n.checked_mul(4)).ok_or("texture too big")?;
            let data = bytes.get(at..at + len).ok_or("truncated texture data")?;
            at += len;
            textures.push(Arc::new(crate::texture::Texture::from_rgba(t.width, t.height, data.to_vec()).ok_or("bad texture size")?));
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
            shift: c.shift,
            ..ViewCamera::default()
        };
        let mut rigs = Vec::with_capacity(header.rigs.len());
        for r in &header.rigs {
            let len = r.vertices.checked_mul(RIG_VERTEX_BYTES).ok_or("rig too big")?;
            let data = bytes.get(at..at + len).ok_or("truncated rig data")?;
            at += len;
            let f = |b: &[u8]| f32::from_le_bytes(b.try_into().expect("4 bytes"));
            let (mut positions, mut normals, mut joints, mut weights) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            for v in data.as_chunks::<RIG_VERTEX_BYTES>().0 {
                positions.push([f(&v[0..4]), f(&v[4..8]), f(&v[8..12])]);
                normals.push([f(&v[12..16]), f(&v[16..20]), f(&v[20..24])]);
                joints.push(std::array::from_fn(|i| u16::from_le_bytes([v[24 + i * 2], v[25 + i * 2]])));
                weights.push(std::array::from_fn(|i| f(&v[32 + i * 4..36 + i * 4])));
            }
            let bones: Vec<crate::rig::Bone> = r
                .bones
                .iter()
                .enumerate()
                .map(|(i, b)| crate::rig::Bone {
                    name: b.name.clone(),
                    // Parents come first; anything else is a root.
                    parent: b.parent.filter(|&p| p < i),
                    translation: Vec3::from_array(b.translation),
                    rotation: quat(b.rotation),
                    scale: Vec3::from_array(b.scale),
                    inverse_bind: unflat(b.inverse_bind),
                })
                .collect();
            if joints.iter().flatten().any(|&j| j as usize >= bones.len().max(1)) {
                return Err("rig: a vertex follows a missing bone".into());
            }
            rigs.push(Arc::new(crate::rig::Rig {
                bones,
                base: unflat(r.base),
                centre: Vec3::from_array(r.centre),
                positions,
                normals,
                joints,
                weights,
            }));
        }
        let mut objects = Vec::with_capacity(header.objects.len());
        for o in header.objects {
            let mesh = meshes.get(o.mesh).ok_or_else(|| format!("object {} refers to a missing mesh", o.name))?.clone();
            objects.push(SceneObject {
                name: o.name,
                mesh,
                transform: Transform { location: Vec3::from_array(o.location), rotation: quat(o.rotation), scale: Vec3::from_array(o.scale) },
                visible: o.visible,
                color: o.color,
                texture: o.texture.and_then(|i| textures.get(i).cloned()),
                rig: o.rig.and_then(|i| rigs.get(i).cloned()).map(|rig| {
                    let mut posed = crate::rig::Posed::new(rig);
                    if o.pose.len() == posed.pose.len() {
                        posed.pose = o.pose.iter().map(|&q| quat(q)).collect();
                    }
                    posed
                }),
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
                texture: None,
                rig: None,
            });
        }
        scene.selected = Some(1);
        scene.camera.set_view(ViewPreset::Top);
        scene.camera.toggle_projection();
        scene.lights.push(Light { kind: LightKind::Point { position: vec3(1.0, 2.0, 3.0), range: 9.0 }, color: [1.0, 0.5, 0.2], intensity: 2.0 });
        scene.ambient = 0.4;
        let tex = crate::texture::Texture::from_rgba(2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        scene.objects[1].texture = Some(Arc::new(tex.clone()));

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
        assert_eq!(back.objects[1].texture.as_deref(), Some(&tex));
        assert!(back.objects[0].texture.is_none());
        // One copy of the cube: header + 24 vertices × 32 + 36 indices × 4.
        assert!(bytes.len() < 2 * (24 * 32 + 36 * 4) + 2000);

        assert!(Scene3D::from_bytes(b"nope").is_err());
        assert!(Scene3D::from_bytes(&bytes[..bytes.len() - 10]).is_err(), "truncated");
    }
}

#[cfg(test)]
mod rig_tests {
    use super::*;
    use crate::math::{vec3, Mat4};
    use crate::rig::{Bone, Posed, Rig};

    #[test]
    fn rigs_and_poses_round_trip() {
        let bone = |name: &str, parent| Bone {
            name: name.into(),
            parent,
            translation: vec3(0.0, 0.0, 1.0),
            rotation: Quat::from_axis_angle(Vec3::X, 0.3),
            scale: vec3(1.0, 1.0, 1.0),
            inverse_bind: Mat4::from_trs(vec3(0.0, 0.0, -1.0), Quat::IDENTITY, vec3(1.0, 1.0, 1.0)),
        };
        let rig = Arc::new(Rig {
            bones: vec![bone("root", None), bone("tip", Some(0))],
            base: Mat4::IDENTITY,
            centre: vec3(0.0, 0.0, 0.5),
            positions: vec![[0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 1.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            joints: vec![[0, 0, 0, 0], [0, 1, 0, 0], [1, 0, 0, 0]],
            weights: vec![[1.0, 0.0, 0.0, 0.0], [0.5, 0.5, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]],
        });
        let mesh = Mesh::from_buffers("m", vec![Vertex { position: [0.0; 3], normal: [0.0, 0.0, 1.0], uv: [0.0; 2] }; 3], vec![0, 1, 2]);
        let mut posed = Posed::new(rig.clone());
        posed.pose[1] = Quat::from_axis_angle(Vec3::Y, 0.8);
        let mut scene = Scene3D::default();
        scene.objects.push(SceneObject {
            name: "arm".into(),
            mesh: Arc::new(rig.deform(&mesh, &posed.pose)),
            transform: Transform::default(),
            visible: true,
            color: [1.0; 3],
            texture: None,
            rig: Some(posed.clone()),
        });
        let back = Scene3D::from_bytes(&scene.to_bytes()).unwrap();
        let o = &back.objects[0];
        let r = o.rig.as_ref().expect("rig kept");
        assert_eq!(*r.rig, *rig);
        assert_eq!(r.pose, posed.pose);
        assert_eq!(o.mesh.vertices, scene.objects[0].mesh.vertices);
    }
}
