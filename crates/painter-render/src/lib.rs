//! GPU side of Painter: keeps painted tiles of every layer, mask and the
//! selection resident in a shared tile atlas, previews strokes with instanced
//! dabs, composites the layer tree with blend-mode shaders, and draws the
//! result with the pan/zoom/rotate view transform.

mod assets;
mod atlas;
mod canvas;
mod gpu;
mod plan;
mod scene;
mod stroke;
mod sync;
mod transform;

pub use atlas::Plane;
pub use sync::{full_upload, live_planes, plane_uploads};
pub use canvas::{CanvasFrame, GpuCanvas, Upload, ViewUniform};
pub use plan::{build_plan, isolation_depth, split_at_layer, LayerPath, NodeKind, PathLevel, PlanNode};
pub use stroke::{DabRenderer, DabStyle, StrokePreview};
pub use scene::{SceneDraw, SceneLight, ScenePreview, SceneRenderer, MAX_LIGHTS};
pub use transform::{TransformGeometry, TransformPreview, TransformRenderer};
