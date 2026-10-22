//! What a 3D reference layer holds: objects with transforms, the viewport
//! camera, lights, and the selection. A `ThreeD` layer in the canvas stack
//! renders this into its plane (depth-tested, wgpu) whenever it changes.
//!
//! ## Rendering plan
//!
//! Each object's [`Mesh`] uploads once as a vertex buffer (32-byte
//! [`crate::mesh::Vertex`]: position, normal, uv) and an index buffer; a
//! per-object uniform holds the model matrix. The layer pass renders into a
//! canvas-sized `Rgba8Unorm` colour target with a `Depth32Float` depth
//! target, using `camera.view_projection(aspect)` and the lights below
//! (Lambert + ambient, matcap-like enough for drawing reference), then the
//! result is composited like any layer plane.

use std::sync::Arc;

use crate::blend_loader::BlendObject;
use crate::camera::ViewCamera;
use crate::gizmo::{pick, Pickable, Transform};
use crate::math::{vec3, Vec3};
use crate::mesh::{Aabb, Mesh};

#[derive(Debug, Clone)]
pub struct SceneObject {
    pub name: String,
    pub mesh: Arc<Mesh>,
    pub transform: Transform,
    pub visible: bool,
    /// Base colour (linear-ish 0..1); reference models are usually plain clay.
    pub color: [f32; 3],
    /// Colour painted over the base colour through the UVs (see [`crate::texture`]).
    pub texture: Option<Arc<crate::texture::Texture>>,
    /// A rigged model's skeleton and pose (the mesh shows that pose).
    pub rig: Option<crate::rig::Posed>,
}

/// Clay grey for imported objects.
pub const CLAY: [f32; 3] = [0.78, 0.78, 0.8];

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LightKind {
    /// Parallel light coming from `direction`.
    Sun { direction: Vec3 },
    Point { position: Vec3, range: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Light {
    pub kind: LightKind,
    pub color: [f32; 3],
    pub intensity: f32,
}

#[derive(Debug, Clone)]
pub struct Scene3D {
    pub objects: Vec<SceneObject>,
    pub camera: ViewCamera,
    pub lights: Vec<Light>,
    pub ambient: f32,
    pub selected: Option<usize>,
}

impl Default for Scene3D {
    fn default() -> Self {
        Self {
            objects: Vec::new(),
            camera: ViewCamera::default(),
            lights: vec![Light { kind: LightKind::Sun { direction: vec3(-0.4, 0.6, -0.7).normalize() }, color: [1.0; 3], intensity: 1.0 }],
            ambient: 0.25,
            selected: None,
        }
    }
}

impl Scene3D {
    /// One OBJ model as a scene.
    pub fn from_mesh(mesh: Mesh) -> Self {
        let mut scene = Scene3D::default();
        scene.objects.push(SceneObject { name: mesh.name.clone(), mesh: Arc::new(mesh), transform: Transform::default(), visible: true, color: CLAY, texture: None, rig: None });
        if let Some(bounds) = scene.bounds() {
            scene.camera.frame(&bounds);
        }
        scene
    }

    /// The mesh objects of a `.blend` file, with their transforms.
    pub fn from_blend(objects: Vec<BlendObject>) -> Self {
        let mut scene = Scene3D::default();
        for o in objects {
            if let Some(mesh) = o.mesh {
                scene.objects.push(SceneObject {
                    name: o.name,
                    mesh: Arc::new(mesh),
                    transform: Transform::from_blender(o.location, o.rotation, o.scale),
                    visible: true,
                    color: CLAY,
                    texture: None,
                    rig: None,
                });
            }
        }
        if let Some(bounds) = scene.bounds() {
            scene.camera.frame(&bounds);
        }
        scene
    }

    /// World-space bounds of the visible objects.
    pub fn bounds(&self) -> Option<Aabb> {
        let mut out: Option<Aabb> = None;
        for o in self.objects.iter().filter(|o| o.visible) {
            let m = o.transform.matrix();
            let (lo, hi) = (o.mesh.bounds.min, o.mesh.bounds.max);
            for i in 0..8 {
                let p = m.transform_point(vec3(
                    if i & 1 == 0 { lo.x } else { hi.x },
                    if i & 2 == 0 { lo.y } else { hi.y },
                    if i & 4 == 0 { lo.z } else { hi.z },
                ));
                out = Some(match out {
                    Some(b) => Aabb { min: b.min.min(p), max: b.max.max(p) },
                    None => Aabb { min: p, max: p },
                });
            }
        }
        out
    }

    /// Selects the object under pixel `(x, y)` of a `width` × `height` view.
    pub fn select_at(&mut self, x: f32, y: f32, width: f32, height: f32) -> Option<usize> {
        let ray = self.camera.ray(x, y, width, height);
        let visible: Vec<usize> = (0..self.objects.len()).filter(|&i| self.objects[i].visible).collect();
        let pickables: Vec<Pickable<'_>> =
            visible.iter().map(|&i| Pickable { mesh: &self.objects[i].mesh, transform: &self.objects[i].transform }).collect();
        self.selected = pick(&ray, &pickables).map(|(k, _)| visible[k]);
        self.selected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_and_selects() {
        let mut scene = Scene3D::default();
        for (name, x) in [("left", -3.0), ("right", 3.0)] {
            scene.objects.push(SceneObject {
                name: name.into(),
                mesh: Arc::new(Mesh::cube()),
                transform: Transform { location: vec3(x, 0.0, 0.0), ..Transform::default() },
                visible: true,
                color: CLAY,
                texture: None,
                rig: None,
            });
        }
        let bounds = scene.bounds().unwrap();
        assert_eq!(bounds.min.x, -3.5);
        scene.camera.frame(&bounds);
        let (w, h) = (800.0, 600.0);
        let [x, y] = scene.camera.to_screen(vec3(3.0, 0.0, 0.0), w, h).unwrap();
        assert_eq!(scene.select_at(x, y, w, h), Some(1));
        scene.objects[1].visible = false;
        assert_eq!(scene.select_at(x, y, w, h), None, "hidden objects can't be picked");
    }
}
