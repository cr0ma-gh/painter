//! Transform tools: scale / rotate, skew, perspective, free distort and mesh
//! warp of a layer or selection, plus canvas crop and the selection clipboard.
//!
//! - [`matrix`]: 3×3 homographies, quad-to-quad mapping, affine parameters.
//! - [`gizmo_transform`]: the on-canvas bounding box, its handles and drags
//!   per mode, and the HUD values.
//! - [`warp`]: the mesh warp surface and its tessellation for rendering.
//! - [`resample`]: nearest / bilinear / bicubic / Lanczos3 resampling through
//!   a transform, the CPU reference for commits; `painter-render`'s
//!   `TransformRenderer` does the same on the GPU for live previews.
//! - [`crop`]: the crop box (aspect presets, thirds, non-destructive canvas
//!   resize) and copy / cut / paste of floating selections.
//!
//! Transforms are non-destructive while the gizmo is open: the source pixels
//! stay untouched and every frame re-renders source → preview through the
//! current matrix; Apply resamples once into the layer.

pub mod commit;
pub mod crop;
pub mod gizmo_transform;
pub mod matrix;
pub mod resample;
pub mod warp;

pub use commit::{commit, render_floating, Placement};
pub use crop::{AspectRatio, BoxHandle, CropBox, CropPlan, FloatingSelection, PasteMode};
pub use gizmo_transform::{GizmoHandle, Hud, Modifiers, TransformAction, TransformGizmo, TransformMode};
pub use matrix::{AffineParams, Mat3, Point};
pub use resample::{resample, Image, Interpolation};
pub use warp::{MeshWarp, WarpMesh};
