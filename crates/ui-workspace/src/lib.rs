//! The window layout model, independent of the GUI toolkit.
//!
//! - [`tiles`]: the screen as a tree of areas. Any area splits side by side
//!   or stacked, borders drag, areas join back, and each area shows an
//!   editor type (canvas, 3D view, layers, …) that can be switched.
//! - [`tabs`]: workspaces (Painting, Inking, 3D, Comic, Compositing), each
//!   keeping its own tile layout and tool set; custom ones can be made,
//!   saved and reset. Also the zen-mode switch that hides everything but
//!   the canvas.
//! - [`command_palette`]: every command, tool, filter and brush preset in a
//!   registry, found by fuzzy search (F3).
//! - [`themes`]: colour themes as JSON, built-in dark and light presets,
//!   contrast checks for the theme editor.
//!
//! The app draws these with egui and maps editor types to its panels; this
//! crate holds the state and the rules, so they can be saved and tested.

pub mod command_palette;
pub mod tabs;
pub mod themes;
pub mod tiles;

pub use command_palette::{Command, CommandCategory, CommandRegistry, Match, PaletteState};
pub use tabs::{Chrome, LayerThumbMode, PerformanceProfile, Workspace, WorkspaceKind, WorkspaceSet};
pub use themes::{Rgba, Theme, ThemeColors, ThemeLibrary};
pub use tiles::{AreaId, EditorType, Rect, SplitKind, TileTree};
