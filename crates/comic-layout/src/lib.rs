//! Comic and manga pages.
//!
//! - [`page_preset`]: print formats (manga B5, A4 doujinshi, webtoon strip,
//!   Franco-Belgian, US comic book, Bonelli, custom), bleed, inner frame,
//!   crop marks, binding / reading direction, gutters.
//! - [`frame_tool`]: vector panel frames, splitting with gutters (straight or
//!   diagonal), reading order, snapping, frame tool handles.
//! - [`panel_mask`]: panels as layers: a masked folder per panel so art stays
//!   inside, a border layer, kept in sync with the vector shape.
//! - [`templates`]: page layouts as split trees.
//! - [`paper`]: ISO / JIS / US paper sizes, px · mm · in units, print bleed and safe margins.

pub mod frame_tool;
pub mod page_preset;
pub mod panel_mask;
pub mod paper;
pub mod templates;

pub use frame_tool::{BorderStyle, Cut, FrameHandle, FrameMode, Panel, PanelId, SnapTargets};
pub use page_preset::{Binding, Category, GutterMetrics, PageGeometry, PagePreset, WebtoonSpec};
pub use panel_mask::{ComicPage, PanelLayers};
pub use paper::{Orientation, PaperSize, Unit, DPI_PRESETS, PAPER_SIZES};
pub use templates::{Layout, Template};
