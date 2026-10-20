//! 3D reference layers: models and scenes to draw over.
//!
//! - [`mesh`]: GPU-ready meshes (interleaved vertices, `u32` triangles,
//!   bounds) and an OBJ reader.
//! - [`blend_loader`]: `.blend` reading (Blender 3.4 – 5.x, plain or zstd):
//!   blocks, the SDNA struct catalogue, mesh objects with their transforms.
//! - [`camera`]: the viewport camera with Blender-style navigation.
//! - [`gizmo`]: object transforms, picking, translate / rotate / scale.
//! - [`scene`]: what a 3D layer holds (objects, camera, lights).
//! - [`persist`]: a scene as bytes for project files.
//!
//! Written from the `.blend` format's public description and from how
//! Blender's tools behave, without Blender's (GPL) source, so the crate keeps
//! the project's MIT / Apache licence. Rendering and the `ThreeD` layer in the
//! canvas stack come next.

pub mod blend_loader;
pub mod camera;
pub mod gizmo;
pub mod math;
pub mod mesh;
pub mod persist;
pub mod scene;
