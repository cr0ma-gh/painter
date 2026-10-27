//! 3D reference layers: models and scenes to draw over.
//!
//! - [`mesh`]: GPU-ready meshes (interleaved vertices, `u32` triangles,
//!   bounds) and an OBJ reader.
//! - [`blend_loader`]: `.blend` reading (Blender 2.80 – 5.x, plain, gzip or zstd):
//!   blocks, the SDNA struct catalogue, mesh objects with their transforms.
//! - [`gltf`], [`fbx`]: glTF / GLB and binary FBX models, through [`import`]
//!   (Y-up to Z-up, transforms baked, objects centred).
//! - [`camera`]: the viewport camera with Blender-style navigation, and the
//!   camera a perspective drawing's vanishing points imply.
//! - [`gizmo`]: object transforms, picking, translate / rotate / scale.
//! - [`scene`]: what a 3D layer holds (objects, camera, lights).
//! - [`texture`]: painted textures and projection painting onto them.
//! - [`persist`]: a scene as bytes for project files.
//!
//! Written from the `.blend` format's public description and from how
//! Blender's tools behave, without Blender's (GPL) source, so the crate keeps
//! the project's MIT / Apache licence. Rendering and the `ThreeD` layer in the
//! canvas stack come next.

pub mod blend_loader;
pub mod camera;
pub mod fbx;
pub mod gizmo;
pub mod gltf;
pub mod import;
pub mod math;
pub mod mesh;
pub mod persist;
pub mod rig;
pub mod scene;
pub mod texture;
