//! Painter's GPU-independent core: the document model, sparse tile storage,
//! the brush/stroke engine and file IO.
//!
//! The CPU tile store is the source of truth for pixels. Renderers mirror it
//! (see `painter-render`), which keeps undo, export and tests deterministic.

pub mod adjust;
pub mod blend;
pub mod brush;
pub mod channels;
pub mod clip;
pub mod cmyk;
pub mod collab;
pub mod color_mode;
pub mod color_profile;
pub mod document;
pub mod duotone;
pub mod effects;
pub mod fill_tool;
pub mod gif;
pub mod filters;
pub mod history;
pub(crate) mod interchange;
pub mod indexed;
pub mod io;
pub mod kra;
pub mod lab;
pub mod layer;
pub mod layer_ops;
pub mod ora;
pub mod print;
pub mod procedural;
pub mod project;
pub mod psd;
pub mod raster;
pub mod rulers;
pub mod selection;
pub mod smart_select;
pub mod smoothing;
pub mod source_stroke;
pub mod stroke;
pub mod text;
pub mod tile;
pub mod tip;
pub mod vector;
pub mod zipfile;

pub use adjust::{AdjustOp, Adjustment};
pub use blend::BlendMode;
pub use brush::{AntiAlias, BrushBlend, BrushMode, BrushSettings, ColorJitter, Dab, PressureCurve, Scatter, SmoothingMethod};
pub use document::{Document, SavedChannel, DEFAULT_DPI, MAX_CANVAS_SIZE};
pub use history::History;
pub use layer::{clip_targets, ClipTo, FileObject, Folder, Layer, LayerId, LayerKind, Mask, VectorLayer, VectorStroke};
pub use procedural::{Gradient, GradientShape, Tone};
pub use rulers::{Grid, Ruler, RulerKind, RulerScope, RulerSet};
pub use selection::SelectionOp;
pub use smoothing::{Smoother, StrokePoint};
pub use stroke::StrokeEngine;
pub use tile::{Tile, TileCoord, TileStore, TILE_BYTES, TILE_SIZE};
pub use tip::{DabTexture, DabTip, DualTip, TipBitmap, TipKind, TipShape};
