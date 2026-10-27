//! Application state and document commands.

use std::collections::HashMap;
use crate::i18n::tr;
use std::path::PathBuf;
use std::time::Instant;

use brush_io::BrushPreset;
use eframe::egui::{self, ecolor::Hsva, Color32, Rect, Vec2};
use painter_core::fill_tool::{
    fill, open_mask_from_pixels, open_mask_from_tiles, reference_pixels, region_to_stroke, restrict_to_selection, ReferenceSource,
};
use painter_core::raster::{commit_stroke, BackgroundRasterizer, RasterizedStroke, StrokeBuffer};
use painter_core::{
    io, project, psd, selection, vector, BrushMode, BrushSettings, Dab, Document, Gradient, History, Layer, LayerId, LayerKind, Mask,
    SelectionOp, StrokeEngine, StrokePoint, TileCoord, TileStore, Tone, VectorStroke, TILE_SIZE,
};
use painter_render::{full_upload, live_planes, plane_uploads, DabStyle, GpuCanvas, Plane, StrokePreview, Upload};

use crate::comic_ui::{ComicWizard, FrameTool};
use crate::dialogs::{ScannerDialog, SvgExportDialog};
use crate::panels::CurveTarget;
use crate::previews::StrokePreviews;
use crate::scene_ui::{SceneDrag, SceneLayer, SceneSnapshot};
use crate::tablet::TabletState;
use crate::tools::{BrushGroup, Tool, ToolDrag, ToolOptions};
use crate::transform_ui::{CropSession, TransformSession};
use crate::view::ViewTransform;
use comic_layout::{ComicPage, PanelId};
use scene_3d::gizmo::Gizmo;
use transform_tools::FloatingSelection;

pub(crate) const UNDO_LIMIT: usize = 100;
/// RAM the undo history of one document may keep of its own before older
/// steps page out to disk, in MiB.
const HISTORY_RAM_MIB: usize = 512;

/// An empty undo history for a document: within a RAM budget, older steps
/// paged out to `history_swap` in the app's data folder. PAINTER_HISTORY_RAM_MB
/// sets the budget (0: no bound); PAINTER_LOW_RAM=1, for machines with 2–4 GB,
/// also keeps fewer steps.
pub(crate) fn new_history() -> History {
    let low_ram = std::env::var("PAINTER_LOW_RAM").is_ok_and(|v| !v.is_empty() && v != "0");
    let (limit, default_mib) = if low_ram { (20, 96) } else { (UNDO_LIMIT, HISTORY_RAM_MIB) };
    let mib = std::env::var("PAINTER_HISTORY_RAM_MB").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default_mib);
    let mut history = History::new(limit);
    history.set_swap_dir(crate::files::app_data_dir().unwrap_or_else(std::env::temp_dir).join("history_swap"));
    history.set_ram_budget((mib > 0).then(|| mib * 1024 * 1024));
    history
}
/// Where a comic page goes in a .painter file (extras/comic.json).
const COMIC_ENTRY: &str = "comic.json";
/// The Clip Studio file kept as the .clip export template, in the app's data folder.
const CLIP_TEMPLATE: &str = "clip-studio-template.clip";
// v2: the Clip Studio style layout with the Brush Size panel.

/// A stroke in progress. Dabs go two ways: to the GPU for the live preview,
/// and to a background CPU rasteriser whose buffer is merged into the edited
/// plane (rebuilt from `before`, see `painter_core::raster`) when the pen lifts.
pub(crate) struct ActiveStroke {
    id: u64,
    pub(crate) plane: Plane,
    engine: StrokeEngine,
    pub(crate) before: TileStore,
    rasterizer: BackgroundRasterizer,
    /// Dabs not yet handed to the GPU preview.
    pending_gpu: Vec<Dab>,
    style: DabStyle,
    alpha_locked: bool,
    respect_selection: bool,
    settings: BrushSettings,
    /// Vector layers: raw input, recorded as a `VectorStroke` on commit.
    vector_points: Option<Vec<StrokePoint>>,
    /// Vector eraser: every dab, to find the strokes it touched.
    vector_erase: Option<Vec<Dab>>,
    /// Ruler snapping and symmetric copies.
    rulers: crate::rulers_ui::StrokeRulers,
    begun: Instant,
    /// The last point fed (after ruler snapping): where a Shift-click line starts next.
    last: StrokePoint,
    /// Stamps and the history brush: colours from a source, merged as the stroke goes.
    source: Option<crate::stamp_tool::SourceStroke>,
}

impl ActiveStroke {
    /// Seconds since the stroke began.
    pub(crate) fn age(&self) -> f32 {
        self.begun.elapsed().as_secs_f32()
    }
}

pub struct PainterApp {
    pub(crate) doc: Document,
    pub(crate) history: History,
    /// Bumped when the document is replaced so the GPU drops stale state.
    pub(crate) epoch: u64,
    pub(crate) brushes: Vec<BrushPreset>,
    /// The Search Layer palette's query.
    pub(crate) layer_search: crate::search_layer::SearchLayer,
    /// Advanced Tool Settings is open, on this category.
    pub(crate) advanced_tool: Option<crate::advanced_tool::Section>,
    /// Colour sets, history and the other colour palettes.
    pub(crate) colors: crate::color_panels::ColorPanels,
    /// The brush list on disk, kept between launches.
    pub(crate) brush_library: crate::brush_library::BrushLibrary,
    /// iPadOS: status bar, edge gestures, safe area.
    pub(crate) ios_window: crate::ios_ui::IosWindow,
    /// On-screen Ctrl / Shift / Alt.
    pub(crate) modifier_keys: crate::modifier_keys::ModifierKeys,
    /// Stroke thumbnails for the Tool Group panel.
    pub(crate) previews: StrokePreviews,
    /// The toolbar's brush group, and the brush last used in each group.
    pub(crate) brush_group: BrushGroup,
    pub(crate) group_brush: HashMap<BrushGroup, usize>,
    /// Fallbacks the last brush import took (shown in the Sub Tool panel).
    pub(crate) import_warnings: Vec<String>,
    pub(crate) brush: usize,
    pub(crate) last_paint_brush: usize,
    pub(crate) last_eraser: usize,
    pub(crate) color: Hsva,
    pub(crate) sub_color: Hsva,
    /// Painting with the transparent colour (Clip Studio's third chip): brushes erase.
    /// Picking any colour ends it (`transparent_from` is the colour it started on).
    pub(crate) transparent: bool,
    pub(crate) transparent_from: Hsva,
    /// The eyedropper's settings and what it's doing.
    pub(crate) eyedropper: crate::eyedropper::Eyedropper,
    /// Layers selected besides the active one (the Layer palette's multi-selection).
    pub(crate) layer_selection: Vec<painter_core::LayerId>,
    pub(crate) tool: Tool,
    pub(crate) tools: ToolOptions,
    pub(crate) view: ViewTransform,
    pub(crate) fit_pending: bool,
    pub(crate) canvas_rect: Rect,
    pub(crate) stroke: Option<ActiveStroke>,
    /// A straight line being placed with Shift (see `crate::lines`).
    pub(crate) line: Option<crate::lines::LineDraft>,
    /// The document (tab id) and point where the last stroke ended.
    pub(crate) last_stroke_end: Option<(u64, StrokePoint)>,
    next_stroke_id: u64,
    pub(crate) drag: Option<ToolDrag>,
    /// Paint into the active layer's mask instead of its pixels.
    pub(crate) edit_mask: bool,
    /// Plane changes waiting to reach the GPU on the next frame.
    pub(crate) uploads: Vec<Upload>,
    pub(crate) recomposite: bool,
    /// What this pass handed the canvas, sent again if egui throws the pass away.
    pub(crate) sent_uploads: (Vec<Upload>, bool),
    pub(crate) tablet: TabletState,
    pub(crate) gpu_ready: bool,
    pub(crate) gpu_stats: Option<(u32, u64)>,
    /// Size being edited in the New Canvas dialog, when open.
    pub(crate) new_canvas: Option<crate::print_ui::NewCanvas>,
    /// The New Canvas settings last used.
    pub(crate) new_canvas_last: Option<crate::print_ui::NewCanvas>,
    /// File → Import → From Scanner, when open.
    pub(crate) scanner: Option<ScannerDialog>,
    /// Edit → Image Size, when open.
    pub(crate) image_size: Option<crate::dialogs::ImageSizeDialog>,
    /// Save As on iPad: the name and format, when open.
    pub(crate) save_as_prompt: Option<crate::dialogs::SaveAsPrompt>,
    /// A file to offer to the system picker once its save finishes (iPad).
    pub(crate) export_after_save: Option<PathBuf>,
    /// File → Quit asked for: the window closes (unsaved documents are asked about first).
    pub(crate) quit_requested: bool,
    /// The resampling last used for Image Size (offered again).
    pub(crate) last_resize_filter: transform_tools::Interpolation,
    /// File → Export → SVG, when open.
    pub(crate) svg_export: Option<SvgExportDialog>,
    /// The open transform (Transform tool), if any.
    pub(crate) transform: Option<TransformSession>,
    /// Don't reopen a transform until the tool is chosen again (after apply, cancel or a refusal).
    pub(crate) transform_blocked: bool,
    pub(crate) next_transform_id: u64,
    /// The open crop box (Crop tool), if any.
    pub(crate) crop: Option<CropSession>,
    /// Pixels from the last Ctrl+C / Ctrl+X.
    pub(crate) clipboard: Option<FloatingSelection>,
    /// Keyboard modifiers during the current canvas drag.
    pub(crate) pointer_mods: egui::Modifiers,
    /// Canvas point under the pointer, if it's over the canvas.
    pub(crate) hover_doc: Option<egui::Pos2>,
    /// The comic page of this document, if it is one (panels and page guides).
    pub(crate) comic: Option<ComicPage>,
    /// Copies of comic parallel to the undo / redo history.
    pub(crate) comic_undo: Vec<Option<ComicPage>>,
    pub(crate) comic_redo: Vec<Option<ComicPage>>,
    pub(crate) comic_wizard: Option<ComicWizard>,
    pub(crate) frame: FrameTool,
    pub(crate) selected_panel: Option<PanelId>,
    /// 3D layers: their scenes by layer, and copies beside the undo history.
    pub(crate) scenes: HashMap<LayerId, SceneLayer>,
    pub(crate) scene_undo: Vec<SceneSnapshot>,
    pub(crate) scene_redo: Vec<SceneSnapshot>,
    pub(crate) scene_revision: u64,
    pub(crate) scene_drag: Option<SceneDrag>,
    pub(crate) gizmo: Gizmo,
    /// The GPU, for reading 3D renders back into layer pixels.
    pub(crate) render_state: Option<eframe::egui_wgpu::RenderState>,
    pub(crate) scene_renderer: Option<painter_render::SceneRenderer>,
    /// Ctrl + Alt resizing of the brush, while held.
    pub(crate) size_drag: Option<crate::brush_cursor::SizeDrag>,
    /// The welcome screen and the About window are open.
    pub(crate) welcome_open: bool,
    pub(crate) about_open: bool,
    /// Help → Reset Settings is open, with what is ticked.
    pub(crate) reset_dialog: Option<crate::settings_reset::ResetChoice>,
    /// Edit → Mode → Bitmap is asking how.
    pub(crate) bitmap_dialog: Option<crate::color_mode_ui::BitmapDialog>,
    /// Edit → Mode → Duotone, Indexed Color and Color Table, and the last choices.
    pub(crate) duotone_dialog: Option<crate::color_mode_ui::DuotoneDialog>,
    /// A Layer Properties slider is being dragged (one undo step for the drag).
    pub(crate) layer_props_drag: bool,
    /// The brush whose size is relative to the canvas, the pixel size it was
    /// given and the canvas side it was for (see `sync_brush_size`).
    pub(crate) brush_px: Option<(usize, f32, f32)>,
    /// The Layers panel's thumbnails.
    pub(crate) layer_thumbs: crate::layer_thumbs::LayerThumbs,
    pub(crate) indexed_dialog: Option<crate::color_mode_ui::IndexedDialog>,
    pub(crate) color_table: Option<crate::color_mode_ui::ColorTableDialog>,
    pub(crate) last_duotone: painter_core::duotone::Duotone,
    pub(crate) last_indexed: painter_core::indexed::IndexedOptions,
    /// Edit → Mode → CMYK Color is asking for the press profile.
    pub(crate) cmyk_dialog: Option<crate::cmyk_ui::CmykDialog>,
    /// File → Print, and its last choices.
    pub(crate) print: Option<crate::print_dialog::PrintDialog>,
    pub(crate) print_settings: crate::print_dialog::PrintSettings,
    /// The main window (Windows: the Print dialog's owner).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) hwnd: isize,
    /// Little CMS conversions for a CMYK document.
    pub(crate) proofer: crate::cmyk_ui::ProoferCache,
    /// "Reset whole brush list" waits for a yes.
    pub(crate) confirm_brush_reset: bool,
    pub(crate) chip_picker: Option<crate::chip_picker::ChipPicker>,
    /// Hold-E eraser: the tool and brush to return to, and when E went down.
    pub(crate) spring_eraser: Option<(Tool, usize, std::time::Instant)>,
    /// H, R or / held: the tool before, the tool held, its key and when.
    pub(crate) spring_tool: Option<(Tool, Tool, egui::Key, std::time::Instant)>,
    /// The name of the command running this frame, for the undo step it makes (History panel).
    pub(crate) next_label: Option<String>,
    /// A guide being pulled out of the ruler bar.
    pub(crate) guide_pull: Option<crate::ruler_bar::GuidePull>,
    /// PAINTER_SCREENSHOT / PAINTER_COMMANDS (a developer aid).
    pub(crate) dev_shot: Option<crate::dev_shot::DevShot>,
    /// The stroke being followed for a smart shape.
    pub(crate) smart: Option<crate::smart_shape::SmartShape>,
    /// File → Set Up File Types is open.
    pub(crate) file_types_open: bool,
    /// Posing: the bone picked, a bone being turned, the bone list's filter.
    pub(crate) pose_bone: Option<usize>,
    pub(crate) pose_drag: Option<crate::pose_ui::PoseDrag>,
    pub(crate) pose_filter: String,
    /// Posing by inverse kinematics (the bones above a dragged joint bend), and how many.
    pub(crate) pose_ik: bool,
    pub(crate) pose_chain: usize,
    /// The Vector Points tool: the picked line and the drag.
    pub(crate) vector_edit: crate::vector_edit::VectorEditState,
    /// The Shape tool's drag or polygon in progress.
    pub(crate) shape: crate::shape_tool::ShapeDraft,
    /// Where the clone stamp copies from.
    pub(crate) clone_source: crate::stamp_tool::CloneSource,
    /// The magnetic lasso in progress.
    pub(crate) magnetic: crate::magnetic::MagneticLasso,
    /// Colour channels shown and edited (the Channels panel).
    pub(crate) channels: crate::channels_ui::ChannelState,
    /// Recorded actions (the Actions panel).
    pub(crate) actions: crate::actions::Actions,
    /// The text tool: the text being edited and new texts' settings.
    pub(crate) text: crate::text_tool::TextState,
    /// Tool families on the strip: last used, open flyout.
    pub(crate) tool_groups: crate::tool_groups::ToolGroups,
    /// The clock and battery chip over the canvas.
    pub(crate) clock: crate::clock_widget::ClockWidget,
    /// The shared drawing session, if any, and the Session panel's fields.
    pub(crate) session: Option<crate::collab_ui::Session>,
    pub(crate) session_form: crate::collab_ui::SessionForm,
    /// Decoded pictures of file object layers' files.
    pub(crate) file_objects: crate::file_object::FileObjectCache,
    /// A layer being renamed in its row (double-click): id, text, field just opened.
    pub(crate) layer_rename: Option<(painter_core::LayerId, String, bool)>,
    /// A brush being renamed in the Tool Group panel: its index, the new name, whether the field has had focus.
    pub(crate) brush_rename: Option<(usize, String, bool)>,
    /// Help → Check for Updates, and the daily check.
    pub(crate) updater: crate::updater::Updater,
    /// The crash report window: open at the start after a crash, and from Help → View Crash Logs.
    pub(crate) crash_reports: crate::crash_report::CrashReports,
    /// Files opened or saved lately (welcome screen, File → Open Recent).
    pub(crate) recent: crate::recent::Recent,
    /// Photoshop's screen modes: 0 standard, 1 full screen with menus, 2 canvas only.
    pub(crate) screen_mode: u8,
    /// For commands that act on the window (full screen).
    pub(crate) egui_ctx: egui::Context,
    /// Android: the "open with" intent was looked at.
    #[cfg(target_os = "android")]
    pub(crate) android_intent_read: bool,
    /// The ensō for them, once loaded.
    pub(crate) logo: Option<egui::TextureHandle>,
    /// Extended Color Selector state.
    pub(crate) gamut: crate::gamut_selector::GamutSelector,
    /// Image tips as textures for the brush cursor.
    pub(crate) tip_textures: crate::brush_cursor::TipTextures,
    /// Painting on a model through a layer over its 3D layer.
    pub(crate) model_paint: Option<crate::texture_paint::ModelPaint>,
    /// Texture documents (by tab) and the models they paint.
    pub(crate) texture_links: crate::texture_paint::TextureLinks,
    /// The document's pixels changed since the last frame.
    pub(crate) doc_touched: bool,
    /// Size of new textures.
    pub(crate) new_texture_size: u32,
    pub(crate) allow_close: bool,
    pub(crate) status: String,
    pub(crate) file_path: Option<PathBuf>,
    pub(crate) dirty: bool,
    title: String,
    /// Workspaces, areas, the command palette and themes.
    pub(crate) ws: crate::workspace_ui::WorkspaceUi,
    /// The other open documents (tabs).
    pub(crate) docs: crate::document_manager::DocManager,
    /// Resolution of the document the clipboard was copied from.
    pub(crate) clipboard_dpi: u32,
    /// Transform with nearest neighbour keeps whole pixels (scale ×n or 1/n,
    /// quarter turns, position on the pixel grid).
    pub(crate) whole_pixels: bool,
    pub(crate) curve_target: CurveTarget,
    /// The ruler selected in the Ruler tool, and what it's dragging.
    pub(crate) selected_ruler: Option<usize>,
    pub(crate) ruler_drag: Option<crate::rulers_ui::RulerDrag>,
    /// A multi-finger touch in progress.
    pub(crate) gesture: Option<crate::canvas::TouchGesture>,
    /// Palm rejection: which touches paint and which navigate (see `crate::palm`).
    pub(crate) palm: crate::palm::PalmArbiter,
    /// Preferences > Input & Tablet is open.
    pub(crate) palm_open: bool,
    /// The open filter dialog (Filter menu).
    pub(crate) filter_dialog: Option<crate::adjust_ui::FilterDialog>,
    pub(crate) started: Instant,
}

impl PainterApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        // Ctrl + / Ctrl − / Ctrl 0 zoom the canvas (as in Clip Studio), not the interface.
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        crate::text_tool::load_fonts();
        crate::text_tool::install_symbol_fallback(&cc.egui_ctx);
        let gpu_ready = match &cc.wgpu_render_state {
            Some(rs) => {
                // PAINTER_LOW_VRAM=1 (or `8bit`, also halving stroke targets) for GPUs with little video memory.
                let profile = match std::env::var("PAINTER_LOW_VRAM").as_deref() {
                    Ok("8bit") => painter_render::RenderProfile { low_precision_strokes: true, ..painter_render::RenderProfile::low_vram() },
                    Ok(v) if !v.is_empty() && v != "0" => painter_render::RenderProfile::low_vram(),
                    _ => painter_render::RenderProfile::default(),
                };
                let canvas = GpuCanvas::with_profile(&rs.device, rs.target_format, profile);
                rs.renderer.write().callback_resources.insert(canvas);
                true
            }
            None => false,
        };
        let ws = crate::workspace_ui::WorkspaceUi::load(cc.storage);
        // The interface language: the one chosen, else the system's.
        crate::i18n::set_language(ws.chrome.language.as_str());
        let (brush_library, saved_brushes) = crate::brush_library::BrushLibrary::load();
        let brushes: Vec<BrushPreset> = saved_brushes.unwrap_or_else(default_brushes);
        let last_eraser = brushes.iter().position(|b| b.settings.mode == BrushMode::Erase).unwrap_or(0);
        let mut app = Self {
            doc: Document::new(2480, 3508),
            history: new_history(),
            epoch: 1,
            brushes,
            brush_library,
            colors: crate::color_panels::ColorPanels::load(),
            layer_search: Default::default(),
            advanced_tool: None,
            ios_window: Default::default(),
            modifier_keys: Default::default(),
            previews: StrokePreviews::default(),
            brush_group: BrushGroup::Pen,
            group_brush: HashMap::new(),
            import_warnings: Vec::new(),
            brush: 0,
            last_paint_brush: 0,
            last_eraser,
            color: Hsva::new(0.0, 0.0, 0.08, 1.0),
            sub_color: Hsva::new(0.0, 0.0, 1.0, 1.0),
            transparent: false,
            transparent_from: Hsva::new(0.0, 0.0, 0.0, 1.0),
            eyedropper: Default::default(),
            layer_selection: Vec::new(),
            tool: Tool::Brush,
            tools: ToolOptions { pencil_only: ws.palm.pencil_only(ToolOptions::default().pencil_only), ..ToolOptions::default() },
            view: ViewTransform::default(),
            fit_pending: true,
            canvas_rect: Rect::NOTHING,
            stroke: None,
            line: None,
            last_stroke_end: None,
            next_stroke_id: 1,
            drag: None,
            edit_mask: false,
            uploads: Vec::new(),
            recomposite: true,
            sent_uploads: (Vec::new(), false),
            tablet: TabletState::default(),
            gpu_ready,
            gpu_stats: None,
            new_canvas: None,
            new_canvas_last: None,
            scanner: None,
            svg_export: None,
            transform: None,
            transform_blocked: false,
            next_transform_id: 0,
            crop: None,
            clipboard: None,
            pointer_mods: egui::Modifiers::NONE,
            hover_doc: None,
            comic: None,
            comic_undo: Vec::new(),
            comic_redo: Vec::new(),
            comic_wizard: None,
            frame: FrameTool::default(),
            selected_panel: None,
            scenes: HashMap::new(),
            scene_undo: Vec::new(),
            scene_redo: Vec::new(),
            scene_revision: 0,
            scene_drag: None,
            gizmo: Gizmo::default(),
            render_state: cc.wgpu_render_state.clone(),
            scene_renderer: None,
            size_drag: None,
            welcome_open: ws.welcome,
            about_open: false,
            reset_dialog: None,
            bitmap_dialog: None,
            duotone_dialog: None,
            layer_props_drag: false,
            brush_px: None,
            layer_thumbs: Default::default(),
            indexed_dialog: None,
            color_table: None,
            last_duotone: Default::default(),
            last_indexed: Default::default(),
            cmyk_dialog: None,
            print: None,
            print_settings: Default::default(),
            hwnd: 0,
            proofer: Default::default(),
            confirm_brush_reset: false,
            chip_picker: None,
            spring_eraser: None,
            spring_tool: None,
            next_label: None,
            guide_pull: None,
            dev_shot: crate::dev_shot::DevShot::new(),
            smart: None,
            file_types_open: false,
            pose_bone: None,
            pose_drag: None,
            pose_filter: String::new(),
            pose_ik: true,
            pose_chain: 2,
            vector_edit: Default::default(),
            shape: Default::default(),
            clone_source: Default::default(),
            magnetic: Default::default(),
            channels: Default::default(),
            actions: crate::actions::Actions::load(),
            text: Default::default(),
            tool_groups: Default::default(),
            clock: Default::default(),
            session: None,
            session_form: Default::default(),
            file_objects: Default::default(),
            layer_rename: None,
            brush_rename: None,
            updater: Default::default(),
            crash_reports: crate::crash_report::CrashReports::at_start(),
            recent: crate::recent::Recent::load(),
            screen_mode: 0,
            egui_ctx: cc.egui_ctx.clone(),
            #[cfg(target_os = "android")]
            android_intent_read: false,
            logo: None,
            gamut: Default::default(),
            tip_textures: Default::default(),
            model_paint: None,
            texture_links: Default::default(),
            doc_touched: false,
            new_texture_size: 1024,
            allow_close: false,
            status: "Ready".into(),
            file_path: None,
            dirty: false,
            title: String::new(),
            ws,
            docs: Default::default(),
            clipboard_dpi: painter_core::DEFAULT_DPI,
            whole_pixels: true,
            image_size: None,
            save_as_prompt: None,
            export_after_save: None,
            quit_requested: false,
            last_resize_filter: transform_tools::Interpolation::Nearest,
            curve_target: CurveTarget::Size,
            selected_ruler: None,
            ruler_drag: None,
            gesture: None,
            palm: Default::default(),
            palm_open: false,
            filter_dialog: None,
            started: Instant::now(),
        };
        app.resync_gpu();
        app.docs.reference.load_saved();
        app
    }

    pub(crate) fn doc_size(&self) -> Vec2 {
        Vec2::new(self.doc.width as f32, self.doc.height as f32)
    }

    pub(crate) fn current_brush(&self) -> &BrushSettings {
        &self.brushes[self.brush].settings
    }

    pub(crate) fn current_brush_mut(&mut self) -> &mut BrushSettings {
        &mut self.brushes[self.brush].settings
    }

    /// The canvas's longer side, which relative brush sizes are fractions of.
    pub(crate) fn canvas_long_side(&self) -> f32 {
        self.doc.width.max(self.doc.height).max(1) as f32
    }

    /// A brush with a size relative to the canvas: its pixel size follows the
    /// document, and a size set in pixels (slider, [ and ], the Brush Size
    /// palette) becomes its new share of the canvas. Every frame.
    pub(crate) fn sync_brush_size(&mut self) {
        let long = self.canvas_long_side();
        let i = self.brush.min(self.brushes.len().saturating_sub(1));
        let Some(settings) = self.brushes.get_mut(i).map(|b| &mut b.settings) else { return };
        if settings.size_relative.is_none() {
            self.brush_px = None;
            return;
        }
        // Changed in pixels on this canvas since it was last set: that's the new size.
        if let Some((bi, px, l)) = self.brush_px {
            if bi == i && l == long && (settings.size - px).abs() > 1e-3 {
                settings.size_relative = Some(settings.size / long);
            }
        }
        settings.size = settings.size_on(long);
        self.brush_px = Some((i, settings.size, long));
    }

    pub(crate) fn select_brush(&mut self, index: usize) {
        self.brush = index;
        self.brush_group = BrushGroup::of(&self.brushes[index].settings);
        self.group_brush.insert(self.brush_group, index);
        if self.brushes[index].settings.mode == BrushMode::Erase {
            self.last_eraser = index;
        } else {
            self.last_paint_brush = index;
        }
        self.tool = Tool::Brush;
    }

    /// Switches the transparent colour on or off.
    pub(crate) fn toggle_transparent(&mut self) {
        self.transparent = !self.transparent;
        self.transparent_from = self.color;
        self.status = if self.transparent { "Transparent colour: brushes erase".into() } else { String::new() };
    }

    pub(crate) fn main_rgb(&self) -> [f32; 3] {
        self.ink_rgb(self.color)
    }

    /// `color` as the document can hold it.
    pub(crate) fn ink_rgb(&self, color: Hsva) -> [f32; 3] {
        // Grey, or black or white, in those colour modes and on Gray or Monochrome layers.
        let rgb = self.layer_expression_color(painter_core::color_mode::mode_color(self.doc.mode, hsva_rgb(color)));
        let bytes = rgb.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        match &self.proofer.proofer {
            // CMYK: the colour the press would print.
            Some(p) if self.doc.mode == painter_core::color_mode::ColorMode::Cmyk => p.proof(bytes).map(|v| v as f32 / 255.0),
            // Indexed: the nearest colour of the table.
            _ if self.doc.mode == painter_core::color_mode::ColorMode::Indexed && !self.doc.palette.is_empty() => {
                let c = self.doc.palette[painter_core::indexed::nearest(&self.doc.palette, bytes)];
                c.map(|v| v as f32 / 255.0)
            }
            _ => rgb,
        }
    }

    // ---- history & GPU sync ------------------------------------------------

    /// Records the current state for undo before an edit.
    /// Puts scenes back from history and redraws (and re-commits) them.
    fn restore_scenes(&mut self, scenes: SceneSnapshot) {
        crate::scene_ui::restore(&mut self.scenes, scenes);
        let ids: Vec<LayerId> = self.scenes.keys().copied().collect();
        for id in ids {
            self.scene_changed(id);
        }
    }

    pub(crate) fn checkpoint(&mut self) {
        // The step keeps no tile the last edit erased whole.
        self.prune_dead_tiles();
        // Named by the command being run, else by what the tool in hand changes.
        match self.next_label.take() {
            Some(label) => self.history.push_labelled(&self.doc, &label),
            None => {
                let hint = match self.tool {
                    Tool::Brush => self.brushes.get(self.brush).map_or("Brush".to_string(), |b| b.settings.name.clone()),
                    tool => tool.name().to_string(),
                };
                self.history.push_hinted(&self.doc, &hint);
            }
        }
        self.push_comic_history(self.comic.clone());
        self.dirty = true;
    }

    /// Drops the tiles edited fully transparent since the last step (an
    /// in-place filter or erase), freeing their RAM and GPU atlas slots.
    pub(crate) fn prune_dead_tiles(&mut self) {
        for plane in live_planes(&self.doc) {
            let Some(tiles) = self.plane_tiles_mut(plane) else { continue };
            let pruned = tiles.prune_unshared_transparent();
            if !pruned.is_empty() {
                self.upload_tiles(plane, pruned);
            }
        }
    }

    /// Opens doc in a new tab (or in place of an untouched blank one).
    pub(crate) fn set_document(&mut self, doc: Document, path: Option<PathBuf>) {
        if self.open_tab_for_new_document() {
            self.set_document_here(doc, path);
        }
    }

    /// Replaces the active document.
    pub(crate) fn set_document_here(&mut self, doc: Document, path: Option<PathBuf>) {
        if let Some(p) = &path {
            self.recent.touch(p, &doc);
        }
        self.doc = doc;
        self.history.clear();
        self.comic = None;
        self.comic_undo.clear();
        self.comic_redo.clear();
        self.scenes.clear();
        self.scene_undo.clear();
        self.scene_redo.clear();
        self.selected_panel = None;
        self.model_paint = None;
        self.selected_ruler = None;
        self.stroke = None;
        self.drag = None;
        self.edit_mask = false;
        self.epoch += 1;
        self.fit_pending = true;
        self.file_path = path;
        self.dirty = false;
        self.resync_gpu();
    }

    /// Queues a full re-upload of every plane (after undo, load, ...).
    pub(crate) fn resync_gpu(&mut self) {
        self.uploads = full_upload(&self.doc);
        self.recomposite = true;
    }

    pub(crate) fn plane_tiles(&self, plane: Plane) -> Option<&TileStore> {
        match plane {
            Plane::Color(id) => self.doc.find(id).and_then(|l| l.tiles()),
            Plane::Mask(id) => self.doc.find(id).and_then(|l| l.mask.as_ref()).map(|m| &m.tiles),
            Plane::Selection => self.doc.selection.as_ref(),
        }
    }

    pub(crate) fn plane_tiles_mut(&mut self, plane: Plane) -> Option<&mut TileStore> {
        match plane {
            Plane::Color(id) => self.doc.find_mut(id).and_then(|l| l.tiles_mut()),
            Plane::Mask(id) => self.doc.find_mut(id).and_then(|l| l.mask.as_mut()).map(|m| &mut m.tiles),
            Plane::Selection => self.doc.selection.as_mut(),
        }
    }

    /// Queues the given tiles of a plane for upload.
    pub(crate) fn upload_tiles(&mut self, plane: Plane, coords: impl IntoIterator<Item = TileCoord>) {
        let Some(tiles) = self.plane_tiles(plane) else {
            return;
        };
        let uploads: Vec<Upload> =
            coords.into_iter().map(|coord| Upload::Tile { plane, coord, tile: tiles.get(coord).cloned() }).collect();
        self.uploads.extend(uploads);
        self.recomposite = true;
    }

    /// Queues a whole plane for re-upload.
    pub(crate) fn upload_plane(&mut self, plane: Plane) {
        match self.plane_tiles(plane) {
            Some(tiles) => {
                let uploads = plane_uploads(plane, tiles);
                self.uploads.extend(uploads);
            }
            None => self.uploads.push(Upload::Clear(plane)),
        }
        self.recomposite = true;
    }

    pub(crate) fn undo(&mut self) {
        // An open transform hasn't touched the document: undo just closes it.
        if self.transform.is_some() {
            self.cancel_transform();
            return;
        }
        let size = self.doc.canvas();
        if self.stroke.is_none() && self.history.undo(&mut self.doc) {
            if let Some(page) = self.comic_undo.pop() {
                self.comic_redo.push(std::mem::replace(&mut self.comic, page));
            }
            if let Some(scenes) = self.scene_undo.pop() {
                self.scene_redo.push(crate::scene_ui::snapshot(&self.scenes));
                self.restore_scenes(scenes);
            }
            if self.doc.canvas() != size {
                self.fit_pending = true;
                self.reset_crop();
            }
            self.dirty = true;
            self.resync_gpu();
        }
    }

    pub(crate) fn redo(&mut self) {
        if self.transform.is_some() {
            return;
        }
        let size = self.doc.canvas();
        if self.stroke.is_none() && self.history.redo(&mut self.doc) {
            if let Some(page) = self.comic_redo.pop() {
                self.comic_undo.push(std::mem::replace(&mut self.comic, page));
            }
            if let Some(scenes) = self.scene_redo.pop() {
                self.scene_undo.push(crate::scene_ui::snapshot(&self.scenes));
                self.restore_scenes(scenes);
            }
            if self.doc.canvas() != size {
                self.fit_pending = true;
                self.reset_crop();
            }
            self.dirty = true;
            self.resync_gpu();
        }
    }

    // ---- layers ----------------------------------------------------------------

    /// Adds layer above the active one, as an undo step.
    pub(crate) fn insert_layer(&mut self, layer: Layer) -> LayerId {
        self.checkpoint();
        let id = self.doc.add_layer(layer);
        self.edit_mask = false;
        self.recomposite = true;
        id
    }

    pub(crate) fn add_raster_layer(&mut self) {
        let id = self.doc.alloc_id();
        let name = format!("Layer {}", self.doc.raster_layer_ids().len() + 1);
        self.insert_layer(Layer::raster(id, name));
    }

    pub(crate) fn add_vector_layer(&mut self) {
        let id = self.doc.alloc_id();
        self.insert_layer(Layer::vector(id, "Vector"));
    }

    pub(crate) fn add_folder(&mut self) {
        let id = self.doc.alloc_id();
        self.insert_layer(Layer::folder(id, "Folder"));
    }

    pub(crate) fn add_gradient_layer(&mut self) {
        let id = self.doc.alloc_id();
        let (w, h) = (self.doc.width as f32, self.doc.height as f32);
        let gradient = Gradient {
            start: [w * 0.5, 0.0],
            end: [w * 0.5, h],
            from: rgba8(self.color),
            to: self.gradient_end_color(),
            shape: self.tools.gradient_shape,
        };
        self.insert_layer(Layer::gradient(id, "Gradient", gradient));
        self.tool = Tool::Gradient;
    }

    /// A screentone layer; with a selection, the tone only covers the selection.
    pub(crate) fn add_tone_layer(&mut self) {
        let id = self.doc.alloc_id();
        let [r, g, b, _] = rgba8(self.color);
        let mut layer = Layer::tone(id, "Tone", Tone { color: [r, g, b], ..Tone::default() });
        if let Some(sel) = &self.doc.selection {
            layer.mask = Some(Mask { tiles: selection::invert(Some(sel), self.doc.canvas()), enabled: true });
        }
        self.insert_layer(layer);
        self.upload_plane(Plane::Mask(id));
    }

    pub(crate) fn delete_active_layer(&mut self) {
        let snapshot_before = self.doc.clone();
        if self.doc.delete_layer(self.doc.active) {
            self.history.push(&snapshot_before);
            self.push_comic_history(self.comic.clone());
            self.dirty = true;
            self.edit_mask = false;
            self.recomposite = true;
        } else {
            self.status = tr("Can't delete the last layer").into();
        }
    }

    pub(crate) fn move_active_layer(&mut self, up: bool) {
        let snapshot_before = self.doc.clone();
        if self.doc.move_layer(self.doc.active, up) {
            self.history.push(&snapshot_before);
            self.push_comic_history(self.comic.clone());
            self.dirty = true;
            self.recomposite = true;
        }
    }

    /// Adds a mask to the active layer. With a selection, unselected pixels start hidden.
    pub(crate) fn add_mask(&mut self) {
        let canvas = self.doc.canvas();
        let tiles = match &self.doc.selection {
            Some(sel) => selection::invert(Some(sel), canvas),
            None => TileStore::new(),
        };
        self.checkpoint();
        let id = self.doc.active;
        if let Some(layer) = self.doc.find_mut(id) {
            layer.mask = Some(Mask { tiles, enabled: true });
            self.edit_mask = true;
        }
        self.upload_plane(Plane::Mask(id));
    }

    pub(crate) fn delete_mask(&mut self) {
        self.checkpoint();
        let id = self.doc.active;
        if let Some(layer) = self.doc.find_mut(id) {
            layer.mask = None;
        }
        self.edit_mask = false;
        self.recomposite = true;
    }

    // ---- strokes -------------------------------------------------------------

    /// The plane a brush or fill on the active layer edits, if editable.
    pub(crate) fn target_plane(&mut self) -> Option<Plane> {
        let layer = self.doc.active_layer()?;
        if self.edit_mask && layer.mask.is_some() {
            if layer.locked {
                self.status = format!("\"{}\" is locked", layer.name);
                return None;
            }
            return Some(Plane::Mask(layer.id));
        }
        if layer.file_object.is_some() {
            self.status = format!("\"{}\" is a file object: edit its file (Open original) or Rasterize it to paint", layer.name);
            return None;
        }
        if !layer.can_paint() {
            self.status = format!("\"{}\" can't be painted on (hidden, locked, or not a pixel layer)", layer.name);
            return None;
        }
        Some(Plane::Color(layer.id))
    }

    fn is_vector_plane(&self, plane: Plane) -> bool {
        matches!(plane, Plane::Color(id) if self.doc.find(id).is_some_and(Layer::is_vector))
    }

    pub(crate) fn begin_stroke(&mut self, p: StrokePoint) {
        let Some(plane) = self.target_plane() else {
            return;
        };
        let is_vector = self.is_vector_plane(plane);
        let rulers = self.stroke_rulers([p.x, p.y]);
        let alpha_locked = matches!(plane, Plane::Color(_)) && !is_vector && self.doc.active_layer().is_some_and(|l| l.alpha_locked);
        let before = self.plane_tiles(plane).cloned().unwrap_or_default();
        let source = match self.tool {
            Tool::Stamp(kind) => {
                if is_vector {
                    self.status = format!("The {} paints on raster layers and masks", self.tool.name());
                    return;
                }
                let Some(source) = self.stamp_source(kind, plane, &before, egui::Pos2::new(p.x, p.y)) else {
                    return;
                };
                Some(source)
            }
            _ => None,
        };
        self.checkpoint();
        let mut settings = self.current_brush().clone();
        if source.is_some() {
            settings.mode = BrushMode::Paint;
        }
        // The transparent colour: any brush erases, with its own tip and dynamics.
        if self.transparent && matches!(plane, Plane::Color(_)) && source.is_none() {
            settings.mode = BrushMode::Erase;
        }
        let color = if matches!(plane, Plane::Mask(_)) {
            [1.0; 3]
        } else {
            // Colour jitter: one variation per stroke, reproducible from its id.
            let seed = self.next_stroke_id.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let r = [0u32, 21, 42].map(|s| ((seed >> s) & 0xF_FFFF) as f32 / 0xF_FFFF as f32);
            settings.color_jitter.apply(self.main_rgb(), r)
        };
        let vector_erase = is_vector && settings.mode == BrushMode::Erase && self.tools.vector_erase != crate::tools::VectorErase::Area;
        if settings.mode != BrushMode::Erase && matches!(plane, Plane::Color(_)) {
            let [r, g, b, _] = self.color.to_srgba_unmultiplied();
            self.colors.used([r, g, b]);
        }
        // Seeded per stroke: random dynamics differ between strokes and replay exactly.
        let mut engine = StrokeEngine::with_seed(&settings, self.next_stroke_id);
        let dabs = engine.add(p);
        let id = self.next_stroke_id;
        self.next_stroke_id += 1;
        self.stroke = Some(ActiveStroke {
            id,
            plane,
            engine,
            before,
            rasterizer: BackgroundRasterizer::spawn(color, settings.clone(), self.doc.canvas()),
            pending_gpu: Vec::new(),
            style: DabStyle::from_settings(color, &settings),
            alpha_locked,
            // Vector strokes are replayed without a selection, so preview them without one too.
            respect_selection: !is_vector,
            vector_points: (is_vector && !vector_erase).then(|| vec![p]),
            vector_erase: vector_erase.then(Vec::new),
            settings,
            rulers,
            begun: Instant::now(),
            last: p,
            source,
        });
        self.queue_dabs(dabs);
    }

    pub(crate) fn continue_stroke(&mut self, mut p: StrokePoint) {
        let zoom = self.view.zoom.max(1e-3);
        if let Some(stroke) = &mut self.stroke {
            // On a ruler: onto its guide once the direction is known.
            if let Some(snap) = &mut stroke.rulers.snap {
                match snap.apply([p.x, p.y], crate::rulers_ui::DECIDE / zoom) {
                    Some([x, y]) => (p.x, p.y) = (x, y),
                    None => return,
                }
            }
            stroke.last = p;
            let screen = self.view.doc_to_screen(self.canvas_rect, Vec2::new(self.doc.width as f32, self.doc.height as f32), egui::Pos2::new(p.x, p.y));
            self.smart_move(p, screen);
            let Some(stroke) = &mut self.stroke else { return };
            if let Some(points) = &mut stroke.vector_points {
                points.push(p);
            }
            let dabs = stroke.engine.add(p);
            self.queue_dabs(dabs);
        }
    }

    fn queue_dabs(&mut self, dabs: Vec<Dab>) {
        let Some(stroke) = self.stroke.as_mut() else {
            return;
        };
        if dabs.is_empty() {
            return;
        }
        // Symmetry rulers: every dab once per copy.
        let dabs = stroke.rulers.replicate(dabs);
        if stroke.source.is_some() {
            let mut stroke = self.stroke.take().expect("stroke in progress");
            self.stamp_from_source(&mut stroke, &dabs);
            self.stroke = Some(stroke);
            return;
        }
        if let Some(all) = &mut stroke.vector_erase {
            all.extend_from_slice(&dabs);
        }
        stroke.pending_gpu.extend_from_slice(&dabs);
        stroke.rasterizer.push(dabs);
    }

    /// Drops the stroke being drawn (a stray finger before a two-finger
    /// gesture, a palm the system or the pen gave away): the tiles go back to
    /// their snapshot and nothing is left to redo.
    pub(crate) fn discard_stroke(&mut self) {
        let Some(stroke) = self.stroke.take() else {
            return;
        };
        // begin_stroke recorded an undo step for it (and the comic page and
        // scenes beside it, which a stroke doesn't change).
        if self.history.rollback(&mut self.doc) {
            self.comic_undo.pop();
            self.scene_undo.pop();
        }
        // Stamps merge into the layer as they go: its tiles go up again. Other
        // strokes only lived in the GPU preview, which goes with the stroke.
        if stroke.source.is_some() {
            self.upload_plane(stroke.plane);
        }
        self.recomposite = true;
    }

    /// Joins the CPU rasteriser and commits its result, which replaces the GPU
    /// preview on the next frame.
    pub(crate) fn end_stroke(&mut self) {
        let Some(mut stroke) = self.stroke.take() else {
            return;
        };
        // Where Shift-click lines start from next (in this document).
        self.last_stroke_end = Some((self.docs.active_id(), stroke.last));
        let tail = stroke.rulers.replicate(stroke.engine.finish());
        if stroke.source.is_some() {
            // Merged as it went: only the last dabs are left.
            self.stamp_from_source(&mut stroke, &tail);
            return;
        }
        if let Some(all) = &mut stroke.vector_erase {
            all.extend_from_slice(&tail);
        }
        if !tail.is_empty() {
            stroke.rasterizer.push(tail);
        }
        let result = stroke.rasterizer.finish();
        let canvas = self.doc.canvas();

        if let (Some(eraser), Plane::Color(id)) = (&stroke.vector_erase, stroke.plane) {
            if let Some(LayerKind::Vector(layer)) = self.doc.find_mut(id).map(|l| &mut l.kind) {
                let erased = match self.tools.vector_erase {
                    crate::tools::VectorErase::ToIntersection => vector::erase_to_intersections(layer, eraser, canvas),
                    _ => vector::erase_touching(layer, eraser, canvas),
                };
                if erased {
                    self.upload_plane(stroke.plane);
                } else {
                    // Nothing hit: just drop the preview.
                    self.recomposite = true;
                }
            }
            return;
        }

        let selection = if stroke.respect_selection { self.doc.selection.clone() } else { None };
        if let (Some(points), Plane::Color(id)) = (stroke.vector_points.take(), stroke.plane) {
            if let Some(LayerKind::Vector(layer)) = self.doc.find_mut(id).map(|l| &mut l.kind) {
                // Symmetric copies are strokes of their own, so they edit like any other.
                for copy in &stroke.rulers.copies {
                    let moved = points
                        .iter()
                        .map(|q| {
                            let [x, y] = copy.apply([q.x, q.y]);
                            StrokePoint { x, y, ..*q }
                        })
                        .collect();
                    layer.strokes.push(VectorStroke { points: moved, settings: stroke.settings.clone(), color: stroke.style.color, seed: stroke.id });
                }
                layer.strokes.push(VectorStroke {
                    points,
                    settings: stroke.settings.clone(),
                    color: stroke.style.color,
                    seed: stroke.id,
                });
            }
        }
        let Some(tiles) = self.plane_tiles_mut(stroke.plane) else {
            return;
        };
        let (opacity, mode, blend) = (stroke.settings.opacity, stroke.settings.mode, stroke.settings.blend);
        commit_stroke(tiles, &stroke.before, &result, opacity, mode, blend, stroke.alpha_locked, selection.as_ref());
        // A shared session: the stroke goes to the others to merge.
        let erase = mode == BrushMode::Erase;
        self.session_stroke(stroke.plane, &result.buffer, &result.dirty, opacity, erase, blend, stroke.alpha_locked, selection.as_ref());
        self.finish_edit(stroke.plane, &stroke.before, result.dirty.iter().copied());
        self.upload_tiles(stroke.plane, result.dirty);
    }

    /// The in-progress stroke's new dabs for this frame's GPU preview.
    pub(crate) fn take_stroke_preview(&mut self) -> Option<StrokePreview> {
        // Source strokes update the layer's tiles directly.
        let stroke = self.stroke.as_mut().filter(|s| s.source.is_none())?;
        Some(StrokePreview {
            id: stroke.id,
            plane: stroke.plane,
            dabs: std::mem::take(&mut stroke.pending_gpu),
            style: stroke.style.clone(),
            opacity: stroke.settings.opacity,
            erase: stroke.settings.mode == BrushMode::Erase,
            blend: stroke.settings.blend.shader_id(),
            alpha_locked: stroke.alpha_locked,
            respect_selection: stroke.respect_selection,
        })
    }

    /// Stamps `dabs` of a source stroke and merges the tiles they touch into its plane.
    fn stamp_from_source(&mut self, stroke: &mut ActiveStroke, dabs: &[Dab]) {
        let Some(src) = stroke.source.as_mut() else { return };
        if dabs.is_empty() {
            return;
        }
        let canvas = self.doc.canvas();
        let mut touched = std::collections::BTreeSet::new();
        for dab in dabs {
            painter_core::raster::stamp_dab(&mut src.stamped.buffer, dab, [1.0; 3], &stroke.settings, canvas, &mut touched);
        }
        src.stamped.dirty.extend(touched.iter().copied());
        let selection = if stroke.respect_selection { self.doc.selection.clone() } else { None };
        let Some(tiles) = self.plane_tiles_mut(stroke.plane) else { return };
        let s = &stroke.settings;
        painter_core::source_stroke::commit_source_stroke(
            tiles,
            &stroke.before,
            &src.stamped.buffer,
            &touched,
            &src.source,
            src.merge,
            s.opacity,
            s.blend,
            stroke.alpha_locked,
            selection.as_ref(),
        );
        self.finish_edit(stroke.plane, &stroke.before, touched.iter().copied());
        self.upload_tiles(stroke.plane, touched);
    }

    // ---- fill, gradient, clear -----------------------------------------------

    /// Commits a prepared stroke buffer onto the edited plane, as a fill would.
    pub(crate) fn commit_region(&mut self, plane: Plane, stroke: &RasterizedStroke, mode: BrushMode) {
        let selection = self.doc.selection.clone();
        let alpha_locked = matches!(plane, Plane::Color(_)) && self.doc.active_layer().is_some_and(|l| l.alpha_locked);
        self.checkpoint();
        let Some(tiles) = self.plane_tiles_mut(plane) else {
            return;
        };
        let before = tiles.clone();
        commit_stroke(tiles, &before, stroke, 1.0, mode, painter_core::BrushBlend::Normal, alpha_locked, selection.as_ref());
        let erase = mode == BrushMode::Erase;
        self.session_stroke(plane, &stroke.buffer, &stroke.dirty, 1.0, erase, painter_core::BrushBlend::Normal, alpha_locked, selection.as_ref());
        self.finish_edit(plane, &before, stroke.dirty.iter().copied());
        self.upload_tiles(plane, stroke.dirty.iter().copied());
    }

    /// Fill bucket at a canvas point.
    pub(crate) fn fill_at(&mut self, x: f32, y: f32) {
        let Some(plane) = self.target_plane() else {
            return;
        };
        if self.is_vector_plane(plane) {
            self.status = tr("Fill works on raster layers and masks, not vector layers").into();
            return;
        }
        let options = self.tools.fill;
        let started = std::time::Instant::now();
        let Some(result) = self.region_at(plane, x, y, &options, true) else {
            return;
        };
        let filled = result.region.count();
        let region = result.region.to_mask();
        if region.is_empty() {
            return;
        }
        let note = match (options.gap, result.gaps_closed) {
            (0, _) => String::new(),
            (gap, true) => format!(", gaps up to {gap} px closed"),
            (_, false) => ", gap closing skipped (the click is inside a gap-sized spot)".into(),
        };
        self.status = format!("Filled {filled} px in {} ms{note}", started.elapsed().as_millis());
        let color = if matches!(plane, Plane::Mask(_)) { [1.0; 3] } else { self.main_rgb() };
        let stroke = region_to_stroke(&region, color);
        self.commit_region(plane, &stroke, BrushMode::Paint);
    }

    /// The area a fill with `options` would cover from a click at `(x, y)`:
    /// bounded by `plane` itself or by the layers the fill refers to, inside
    /// the selection when `in_selection`.
    fn region_at(&mut self, plane: Plane, x: f32, y: f32, options: &painter_core::fill_tool::FillOptions, in_selection: bool) -> Option<painter_core::fill_tool::Fill> {
        let canvas = self.doc.canvas();
        let seed = (x.floor() as i32, y.floor() as i32);
        let reference = self.tools.fill_reference;
        // The boundary: the edited plane itself, or merged layers.
        let mut open = match reference.source {
            ReferenceSource::EditedLayer => {
                let empty = TileStore::new();
                open_mask_from_tiles(self.plane_tiles(plane).unwrap_or(&empty), canvas, seed, options.tolerance)
            }
            ReferenceSource::AllLayers | ReferenceSource::ReferenceLayers => match reference_pixels(&self.doc, &reference) {
                Some(pixels) => open_mask_from_pixels(&pixels, canvas, seed, options.tolerance),
                None => {
                    self.status = tr("No reference layer: tick Reference on the ink layers in the Layers panel").into();
                    return None;
                }
            },
        };
        if in_selection {
            if let Some(selection) = &self.doc.selection {
                restrict_to_selection(&mut open, selection);
            }
        }
        fill(&open, seed, options)
    }

    /// Auto Select: the area a click would fill becomes (or changes) the selection.
    pub(crate) fn magic_select_at(&mut self, at: egui::Pos2, op: SelectionOp) {
        let plane = Plane::Color(self.doc.active);
        let options = self.tools.magic;
        // Adding to or subtracting from the selection looks past it.
        let Some(result) = self.region_at(plane, at.x, at.y, &options, false) else {
            if op == SelectionOp::Replace {
                self.deselect();
            }
            return;
        };
        let count = result.region.count();
        self.combine_selection(result.region.to_mask(), op);
        self.status = format!("Selected {count} px");
    }

    pub(crate) fn gradient_end_color(&self) -> [u8; 4] {
        if self.tools.gradient_to_transparent {
            let [r, g, b, _] = rgba8(self.color);
            [r, g, b, 0]
        } else {
            rgba8(self.sub_color)
        }
    }

    /// Gradient tool: edits a gradient layer, or paints a gradient into the edited plane.
    pub(crate) fn apply_gradient(&mut self, start: [f32; 2], end: [f32; 2]) {
        let shape = self.tools.gradient_shape;
        let active = self.doc.active;
        if !self.edit_mask && matches!(self.doc.find(active).map(|l| &l.kind), Some(LayerKind::Gradient(_))) {
            let (from, to) = (rgba8(self.color), self.gradient_end_color());
            self.checkpoint();
            if let Some(LayerKind::Gradient(g)) = self.doc.find_mut(active).map(|l| &mut l.kind) {
                *g = Gradient { start, end, from, to, shape };
            }
            self.recomposite = true;
            return;
        }
        let Some(plane) = self.target_plane() else {
            return;
        };
        if self.is_vector_plane(plane) {
            self.status = tr("Gradients paint on raster layers and masks").into();
            return;
        }
        let (from, to) = if matches!(plane, Plane::Mask(_)) {
            ([255, 255, 255, 255], [255, 255, 255, 0])
        } else {
            (rgba8(self.color), self.gradient_end_color())
        };
        let gradient = Gradient { start, end, from, to, shape };
        let canvas = self.doc.canvas();
        let mut buffer = StrokeBuffer::new();
        let mut dirty = std::collections::BTreeSet::new();
        for ty in 0..canvas.1.div_ceil(TILE_SIZE) as i32 {
            for tx in 0..canvas.0.div_ceil(TILE_SIZE) as i32 {
                let coord = TileCoord::new(tx, ty);
                let (ox, oy) = coord.origin();
                for (i, px) in buffer.tile_mut(coord).iter_mut().enumerate() {
                    let (lx, ly) = ((i as u32 % TILE_SIZE) as i32, (i as u32 / TILE_SIZE) as i32);
                    *px = gradient.sample((ox + lx) as f32 + 0.5, (oy + ly) as f32 + 0.5);
                }
                dirty.insert(coord);
            }
        }
        self.commit_region(plane, &RasterizedStroke { buffer, dirty }, BrushMode::Paint);
    }

    /// Erases the selected area of the edited plane (Delete key).
    pub(crate) fn clear_selected(&mut self) {
        let Some(sel) = self.doc.selection.clone() else {
            self.status = tr("Nothing selected").into();
            return;
        };
        let Some(plane) = self.target_plane() else {
            return;
        };
        if self.is_vector_plane(plane) {
            self.status = tr("Use the vector eraser on vector layers").into();
            return;
        }
        let stroke = region_to_stroke(&sel, [1.0; 3]);
        self.commit_region(plane, &stroke, BrushMode::Erase);
    }

    // ---- selection -----------------------------------------------------------

    pub(crate) fn set_selection(&mut self, selection: Option<TileStore>) {
        self.checkpoint();
        self.doc.selection = selection;
        self.upload_plane(Plane::Selection);
    }

    pub(crate) fn combine_selection(&mut self, shape: TileStore, op: SelectionOp) {
        let combined = selection::combine(self.doc.selection.as_ref(), shape, op, self.doc.canvas());
        self.set_selection(Some(combined));
    }

    pub(crate) fn select_all(&mut self) {
        let all = selection::select_all(self.doc.canvas());
        self.set_selection(Some(all));
    }

    pub(crate) fn deselect(&mut self) {
        if self.doc.selection.is_some() {
            self.set_selection(None);
        }
    }

    pub(crate) fn invert_selection(&mut self) {
        let inverted = selection::invert(self.doc.selection.as_ref(), self.doc.canvas());
        self.set_selection(Some(inverted));
    }

    // ---- brush presets ---------------------------------------------------------

    /// Adds every preset from a `.sut`, `.abr`, `.rustbrush` or `.todb` file.
    pub(crate) fn import_brushes(&mut self, path: &std::path::Path) {
        let mut library = brush_io::sut::MaterialLibrary::for_current_user();
        if let Some(dir) = crate::files::materials_dir() {
            library.add_copied(&dir);
        }
        match brush_io::cache::import_cached_with(path, &brush_io::cache::default_dir(), &library) {
            Ok(report) if report.presets.is_empty() => {
                self.status = format!("No brushes found in {}", file_name(path));
                self.import_warnings = report.warnings;
            }
            Ok(report) => {
                let first = self.brushes.len();
                let count = report.presets.len();
                let partial = report.presets.iter().filter(|p| !p.unsupported_features().is_empty()).count();
                self.brushes.extend(report.presets);
                self.select_brush(first);
                let mut notes = Vec::new();
                if report.from_cache {
                    notes.push("from cache".to_string());
                }
                if !report.warnings.is_empty() {
                    notes.push(format!("{} fallbacks, see Sub Tool", report.warnings.len()));
                }
                if crate::files::materials_dir().is_some() && report.warnings.iter().any(|w| w.contains("isn't installed")) {
                    notes.push("missing textures: copy Clip Studio's Material folder into Painter/Materials, then import again".into());
                }
                if partial > 0 {
                    notes.push(format!("{partial} use features Painter doesn't paint yet"));
                }
                let notes = if notes.is_empty() { String::new() } else { format!(" ({})", notes.join("; ")) };
                self.status = format!("Imported {count} brushes from {}{notes}", file_name(path));
                self.import_warnings = report.warnings;
            }
            Err(err) => self.status = format!("Couldn't import {}: {err}", file_name(path)),
        }
    }

    pub(crate) fn import_brushes_dialog(&mut self) {
        if let Some(paths) = crate::files::FileDialog::new()
            .add_filter("Brushes", &brush_io::IMPORT_EXTENSIONS)
            .add_filter("Clip Studio sub tool", &["sut"])
            .add_filter("Photoshop brushes", &["abr"])
            .add_filter("Krita presets and bundles", &["kpp", "bundle"])
            .add_filter("MyPaint brushes", &["myb"])
            .add_filter("GIMP brushes", &["gbr", "gih", "vbr"])
            .add_filter("Painter brushes", &[brush_io::native::EXTENSION])
            .purpose(crate::files::Purpose::Brushes)
            .pick_files()
        {
            for path in paths {
                self.import_brushes(&path);
            }
        }
    }

    /// Saves the given presets (all when `None`) as `.rustbrush`.
    pub(crate) fn export_brushes(&mut self, only: Option<usize>) {
        let presets: Vec<BrushPreset> = match only {
            Some(i) => vec![self.brushes[i].clone()],
            None => self.brushes.clone(),
        };
        let name = match only {
            Some(i) => format!("{}.{}", self.brushes[i].name(), brush_io::native::EXTENSION),
            None => format!("brushes.{}", brush_io::native::EXTENSION),
        };
        let Some(path) =
            crate::files::FileDialog::new().add_filter("Painter brushes", &[brush_io::native::EXTENSION]).set_file_name(name).save_file()
        else {
            return;
        };
        self.status = match brush_io::native::save(&presets, &path) {
            Ok(()) => {
                crate::files::offer_export(&path);
                format!("Exported {} brushes to {}", presets.len(), file_name(&path))
            }
            Err(err) => format!("Export failed: {err}"),
        };
    }

    pub(crate) fn delete_brush(&mut self, index: usize) {
        if self.brushes.len() <= 1 {
            return;
        }
        self.brushes.remove(index);
        let fix = |i: usize| {
            if i > index {
                i - 1
            } else {
                i.min(self.brushes.len() - 1)
            }
        };
        self.brush = fix(self.brush);
        self.last_paint_brush = fix(self.last_paint_brush);
        self.last_eraser = fix(self.last_eraser);
        self.group_brush.clear();
        self.select_brush(self.brush);
    }

    /// The built-in brush brush `index` started as (same name), if it's one of them.
    pub(crate) fn default_of(&self, index: usize) -> Option<BrushPreset> {
        let name = &self.brushes.get(index)?.settings.name;
        default_brushes().into_iter().find(|d| d.settings.name == *name)
    }

    /// Brush `index` back to its built-in settings, in the same place.
    pub(crate) fn reset_brush(&mut self, index: usize) {
        let Some(default) = self.default_of(index) else {
            self.status = tr("This brush isn't one of the built-in ones: it has no default to go back to").into();
            return;
        };
        let name = default.settings.name.clone();
        self.brushes[index] = default;
        self.group_brush.clear();
        if index == self.brush {
            self.select_brush(index);
        }
        self.status = format!("“{name}” is back to its default settings");
    }

    /// Every built-in brush back to its default settings, and the missing ones
    /// added back; imported brushes stay as they are.
    pub(crate) fn reset_builtin_brushes(&mut self) {
        let mut reset = 0;
        for i in 0..self.brushes.len() {
            if let Some(default) = self.default_of(i) {
                if self.brushes[i] != default {
                    self.brushes[i] = default;
                    reset += 1;
                }
            }
        }
        let missing: Vec<BrushPreset> =
            default_brushes().into_iter().filter(|d| !self.brushes.iter().any(|b| b.settings.name == d.settings.name)).collect();
        let added = missing.len();
        self.brushes.extend(missing);
        self.group_brush.clear();
        self.select_brush(self.brush.min(self.brushes.len() - 1));
        self.status = format!("Built-in brushes reset ({reset} changed back, {added} added back); imported brushes kept");
    }

    /// The brush list as Painter first had it: built-in brushes only.
    pub(crate) fn reset_brush_list(&mut self) {
        self.brushes = default_brushes();
        self.brush = 0;
        self.last_paint_brush = 0;
        self.last_eraser = self.brushes.iter().position(|b| b.settings.mode == painter_core::BrushMode::Erase).unwrap_or(0);
        self.group_brush.clear();
        self.select_brush(0);
        self.status = tr("The brush list is back to the default brushes").into();
    }

    /// Adds the built-in brushes that are no longer in the list (by name).
    pub(crate) fn restore_default_brushes(&mut self) {
        let missing: Vec<BrushPreset> =
            default_brushes().into_iter().filter(|d| !self.brushes.iter().any(|b| b.settings.name == d.settings.name)).collect();
        self.status = if missing.is_empty() {
            "All the default brushes are already there".into()
        } else {
            format!("Added back {} default brushes", missing.len())
        };
        self.brushes.extend(missing);
        self.group_brush.clear();
    }

    /// Gives brush `index` a new name (trimmed; an empty one is ignored).
    pub(crate) fn rename_brush(&mut self, index: usize, name: &str) {
        let name = name.trim();
        let Some(brush) = self.brushes.get_mut(index).filter(|_| !name.is_empty()) else {
            return;
        };
        if brush.settings.name != name {
            // The group came from the old name: keep it where it is.
            let group = BrushGroup::of(&brush.settings);
            brush.settings.group = Some(crate::toolbar::group_key(group).to_string());
            brush.settings.name = name.to_string();
            self.group_brush.clear();
            if index == self.brush {
                self.select_brush(index);
            }
        }
    }

    /// Copies a brush in place, right after the original, and selects the copy.
    pub(crate) fn duplicate_brush(&mut self, index: usize) {
        let mut copy = self.brushes[index].clone();
        copy.settings.name = copy_name(&copy.settings.name, |n| self.brushes.iter().any(|b| b.settings.name == n));
        self.brushes.insert(index + 1, copy);
        let fix = |i: usize| if i > index { i + 1 } else { i };
        self.last_paint_brush = fix(self.last_paint_brush);
        self.last_eraser = fix(self.last_eraser);
        self.group_brush.clear();
        self.select_brush(index + 1);
    }

    /// Indices of the brushes in a toolbar group, in list order.
    pub(crate) fn brushes_in(&self, group: BrushGroup) -> Vec<usize> {
        (0..self.brushes.len()).filter(|&i| BrushGroup::of(&self.brushes[i].settings) == group).collect()
    }

    /// Toolbar brush group: back to the brush last used in it, else its first.
    pub(crate) fn select_group(&mut self, group: BrushGroup) {
        let remembered = self
            .group_brush
            .get(&group)
            .copied()
            .filter(|&i| i < self.brushes.len() && BrushGroup::of(&self.brushes[i].settings) == group);
        match remembered.or_else(|| self.brushes_in(group).first().copied()) {
            Some(index) => self.select_brush(index),
            None => {
                self.brush_group = group;
                self.tool = Tool::Brush;
            }
        }
    }

    /// Files dropped on the window: brushes are imported, images and projects opened.
    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        // Pictures dropped on the reference board go there.
        let dropped = if dropped.is_empty() { dropped } else { self.drop_on_board(ctx, dropped) };
        // Where on the canvas they were dropped (models stand there).
        let at = ctx
            .input(|i| i.pointer.latest_pos())
            .filter(|p| self.canvas_rect.contains(*p))
            .map(|p| self.view.screen_to_doc(self.canvas_rect, self.doc_size(), p));
        for path in dropped {
            if brush_io::is_brush_file(&path) {
                self.import_brushes(&path);
            } else if crate::files::kind_of(&path) == Some(crate::files::FileKind::Model) {
                self.drop_model(path, at);
            } else {
                self.open_path(path);
            }
        }
        let hovering = ctx.input(|i| i.raw.hovered_files.iter().any(|f| f.path.as_deref().is_some_and(brush_io::is_brush_file)));
        if hovering {
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("brush_drop")));
            let screen = ctx.screen_rect();
            painter.rect_filled(screen, 0.0, Color32::from_black_alpha(150));
            painter.text(
                screen.center(),
                egui::Align2::CENTER_CENTER,
                "Drop to add brushes (.sut, .abr, .rustbrush)",
                egui::FontId::proportional(22.0),
                Color32::WHITE,
            );
        }
    }

    // ---- files ---------------------------------------------------------------

    /// Opens whatever `path` is: a document, a 3D model (new 3D layer) or brushes.
    pub(crate) fn open_any(&mut self, path: PathBuf) {
        match crate::files::kind_of(&path) {
            Some(crate::files::FileKind::Model) => self.new_scene_layer_from(path),
            Some(crate::files::FileKind::Brushes) => self.import_brushes(&path),
            _ => self.open_path(path),
        }
    }

    pub(crate) fn open_dialog(&mut self) {
        let Some(path) = crate::files::FileDialog::new()
            .add_filter("All supported", &[project::EXTENSION, "psd", "kra", "ora", "clip", "png", "jpg"])
            .add_filter("Painter project", &[project::EXTENSION])
            .add_filter("Photoshop", &["psd"])
            .add_filter("Krita", &["kra"])
            .add_filter("Clip Studio", &["clip"])
            .add_filter("OpenRaster", &["ora"])
            .add_filter("PNG image", &["png"])
            .pick_file()
        else {
            return;
        };
        self.open_path(path);
    }

    /// What a project file carries besides the layers: its comic page and 3D scenes.
    pub(crate) fn apply_project_extras(&mut self, path: &std::path::Path, extras: &project::Extras) {
        // A comic page saved with the project: panels, borders, page guides.
        if let Some(bytes) = extras.get(COMIC_ENTRY) {
            match ComicPage::from_json(bytes, &self.doc) {
                Ok(page) => {
                    self.status = format!("Opened {} (comic page, {} panels)", path.display(), page.panels.len());
                    // New panels match the page's existing borders.
                    if let Some(panel) = page.panels.first() {
                        self.frame.border_width = panel.border.width;
                    }
                    self.comic = Some(page);
                }
                Err(err) => self.status = format!("Opened {}, but its comic page couldn't be read: {err}", path.display()),
            }
        }
        // 3D layers, editable again.
        if let Err(err) = self.load_scenes(extras) {
            self.status = format!("Opened {}, but {err}", path.display());
        }
    }
    pub(crate) fn save(&mut self) {
        match self.file_path.clone() {
            Some(path) => self.save_to(path),
            None => self.save_as(),
        }
    }

    pub(crate) fn save_as(&mut self) {
        let name = self
            .file_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or_else(|| format!("painting.{}", project::EXTENSION), |n| n.to_string_lossy().into_owned());
        // iPad: no save panel; ask for the name and format here (see `crate::dialogs::SaveAsPrompt`).
        if crate::MOBILE {
            self.save_as_prompt = Some(crate::dialogs::SaveAsPrompt::new(&name));
            return;
        }
        if let Some(path) = crate::files::FileDialog::new()
            .add_filter("Painter project", &[project::EXTENSION])
            .add_filter("Krita (layers, masks and clipping flattened)", &["kra"])
            .add_filter("OpenRaster (MyPaint, GIMP, Krita)", &["ora"])
            .set_file_name(name)
            .save_file()
        {
            self.save_to(path);
        }
    }

    pub(crate) fn save_to(&mut self, path: PathBuf) {
        // 3D layers: their latest render into the pixels first.
        self.commit_scenes(true);
        let mut extras = project::Extras::new();
        if let Some(comic) = &mut self.comic {
            comic.prune(&self.doc);
            extras.insert(COMIC_ENTRY.into(), comic.to_json());
        }
        self.prune_scenes();
        for (id, layer) in &self.scenes {
            extras.insert(crate::scene_ui::scene_entry(*id), layer.scene.to_bytes());
        }
        self.prune_dead_tiles();
        // On a worker thread, from a copy: painting goes on meanwhile.
        self.recent.touch(&path, &self.doc);
        self.save_in_background(path, extras);

    }

    pub(crate) fn import_png_layer(&mut self) {
        if let Some(path) =
            crate::files::FileDialog::new().add_filter("PNG image", &["png"]).purpose(crate::files::Purpose::ImageLayer).pick_file()
        {
            self.import_png_layer_from(&path);
        }
    }

    /// A PNG as a new layer.
    pub(crate) fn import_png_layer_from(&mut self, path: &std::path::Path) {
        match io::import_png(path) {
            Ok(mut image) => {
                // Nothing is kept past the canvas: what doesn't show is deleted.
                let (w, h) = self.doc.canvas();
                let cut = image.tiles.clip_to_canvas(w, h);
                let id = self.doc.alloc_id();
                self.insert_layer(Layer::raster_with(id, file_stem(path), image.tiles));
                self.upload_plane(Plane::Color(id));
                self.status = format!("Imported {}{}", path.display(), if cut { " (cut to the canvas)" } else { "" });
            }
            Err(err) => self.status = format!("Couldn't import {}: {err}", path.display()),
        }
    }

    pub(crate) fn export_png(&mut self) {
        self.commit_scenes(true);
        let Some(path) = crate::files::FileDialog::new().add_filter("PNG image", &["png"]).set_file_name("painting.png").save_file() else {
            return;
        };
        // Draft layers stay out of exports.
        self.status = match io::export_png(&self.doc.without_drafts(), &path) {
            Ok(()) => {
                crate::files::offer_export(&path);
                format!("Exported {}", path.display())
            }
            Err(err) => format!("Export failed: {err}"),
        };
    }

    pub(crate) fn export_psd(&mut self) {
        self.commit_scenes(true);
        let Some(path) = crate::files::FileDialog::new().add_filter("Photoshop", &["psd"]).set_file_name("painting.psd").save_file() else {
            return;
        };
        self.status = match psd::save(&self.doc.without_drafts(), &path) {
            Ok(()) => {
                crate::files::offer_export(&path);
                format!("Exported {} (procedural and vector layers rasterised)", path.display())
            }
            Err(err) => format!("Export failed: {err}"),
        };
    }

    /// Export → Krita (`"kra"`) or OpenRaster (`"ora"`): the layers without drafts.
    pub(crate) fn export_layered(&mut self, ext: &str) {
        self.commit_scenes(true);
        type Save = fn(&painter_core::Document, &std::path::Path) -> Result<Vec<String>, String>;
        let (label, save): (&str, Save) = match ext {
            "kra" => ("Krita", painter_core::kra::save),
            _ => ("OpenRaster", painter_core::ora::save),
        };
        let name = format!("painting.{ext}");
        let Some(path) = crate::files::FileDialog::new().add_filter(label, &[ext]).set_file_name(name).save_file() else {
            return;
        };
        self.status = match save(&self.doc.without_drafts(), &path) {
            Ok(notes) => {
                crate::files::offer_export(&path);
                if notes.is_empty() {
                    format!("Exported {}", path.display())
                } else {
                    format!("Exported {} — {}", path.display(), crate::document_manager::session_store::summarize(&notes))
                }
            }
            Err(err) => format!("Export failed: {err}"),
        };
    }

    /// Asks for a `.clip` saved by Clip Studio to use as the export template
    /// (only its canvas settings are used) and keeps a copy.
    pub(crate) fn choose_clip_template(&mut self) -> bool {
        let Some(dir) = crate::files::app_data_dir() else { return false };
        let Some(source) = crate::files::FileDialog::new()
            .add_filter("Any Clip Studio file (its layers aren't used)", &[painter_core::clip::EXTENSION])
            .pick_file()
        else {
            return false;
        };
        // Check it now rather than at the next export.
        let check = std::fs::read(&source).map_err(|e| e.to_string()).and_then(|t| {
            painter_core::clip::write(&painter_core::Document::new(8, 8), &t).map(|_| ())
        });
        if let Err(err) = check {
            self.status = format!("Can't use {} as a Clip Studio template: {err}", source.display());
            return false;
        }
        let _ = std::fs::create_dir_all(&dir);
        match std::fs::copy(&source, dir.join(CLIP_TEMPLATE)) {
            Ok(_) => true,
            Err(err) => {
                self.status = format!("Couldn't keep the template: {err}");
                false
            }
        }
    }

    /// Export → Clip Studio: the layers written into the kept template.
    pub(crate) fn export_clip(&mut self) {
        self.commit_scenes(true);
        let Some(template) = crate::files::app_data_dir().map(|d| d.join(CLIP_TEMPLATE)) else { return };
        if !template.is_file() && !self.choose_clip_template() {
            if !self.status.starts_with("Can't use") {
                self.status = tr("Clip Studio export needs one .clip saved by Clip Studio (any, even an empty canvas) as a template").into();
            }
            return;
        }
        let Some(path) = crate::files::FileDialog::new()
            .add_filter("Clip Studio", &[painter_core::clip::EXTENSION])
            .set_file_name("painting.clip")
            .save_file()
        else {
            return;
        };
        self.status = match painter_core::clip::save_with_template(&self.doc.without_drafts(), &template, &path) {
            Ok(notes) => {
                crate::files::offer_export(&path);
                if notes.is_empty() {
                    format!("Exported {}", path.display())
                } else {
                    format!("Exported {} — {}", path.display(), crate::document_manager::session_store::summarize(&notes))
                }
            }
            Err(err) => format!("Clip Studio export failed: {err}"),
        };
    }

    /// Files chosen in the iPadOS document picker, which answers after the
    /// dialog call has returned.
    fn receive_picked_files(&mut self, ctx: &egui::Context) {
        use crate::files::Purpose;
        while let Some((purpose, path)) = crate::files::take_picked() {
            match purpose {
                Purpose::Open => self.open_any(path),
                Purpose::ImageLayer => self.import_png_layer_from(&path),
                Purpose::Brushes => self.import_brushes(&path),
                Purpose::NewModelLayer => self.new_scene_layer_from(path),
                Purpose::AddModel => self.add_model_from(&path),
                Purpose::Theme => self.import_theme_from(&path),
                Purpose::ReferenceImage => self.add_board_image(&path),
                Purpose::ScanPage => self.add_scan_pages(vec![path]),
            }
        }
        if crate::files::picking() {
            // Nothing else wakes egui while the picker is up.
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let name = self.tab_title(self.docs.active);
        let title = format!("{}{name} — Painter", if self.dirty { "• " } else { "" });
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

}

impl eframe::App for PainterApp {
    /// An installed update starts once Painter has closed.
    fn on_exit(&mut self) {
        if let Some(restart) = self.updater.restart.take() {
            restart.run();
        }
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.next_label = None;
        self.sent_uploads.0.clear();
        #[cfg(windows)]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(RawWindowHandle::Win32(h)) = frame.window_handle().map(|w| w.as_raw()) {
                if self.hwnd != h.hwnd.get() {
                    crate::win_alt_space::install(h.hwnd.get());
                }
                self.hwnd = h.hwnd.get();
            }
        }
        self.dev_shot_tick(ctx);
        self.session_tick(ctx);
        self.update_proofer();
        self.sync_brush_size();
        self.file_objects_tick();
        self.updater_tick(ctx);
        self.tablet.poll(frame);
        if let Some(eraser) = self.tablet.take_eraser_switch() {
            // Flipping the pen to its eraser end switches tools, like a real eraser.
            let index = if eraser { self.last_eraser } else { self.last_paint_brush };
            self.select_brush(index);
        }
        self.gpu_stats = frame
            .wgpu_render_state()
            .and_then(|rs| rs.renderer.read().callback_resources.get::<GpuCanvas>().map(GpuCanvas::atlas_usage));
        self.poll_documents(ctx);
        self.document_shortcuts(ctx);
        self.handle_shortcuts(ctx);
        self.handle_dropped_files(ctx);
        if let Some(notice) = crate::files::take_notice() {
            self.status = notice;
        }
        crate::files::picker_window(ctx);
        self.receive_picked_files(ctx);
        // Choosing any colour (wheel, palette, picker…) ends the transparent colour.
        if self.transparent && self.color.to_srgba_unmultiplied() != self.transparent_from.to_srgba_unmultiplied() {
            self.transparent = false;
        }
        self.brush_library.autosave(&self.brushes, false);
        self.colors.autosave();
        if let Some(err) = self.brush_library.error.take() {
            self.status = err;
        }
        self.ws.autosave();
        self.sync_tool_sessions();
        self.prune_scenes();
        self.model_paint_tick();
        self.texture_documents_tick(ctx);
        self.animate_scenes(ctx);
        self.commit_scenes(false);
        self.scene_repaint(ctx);
        self.workspace_frame(ctx);
        // Outermost: the bands iPadOS draws over (rounded corners, camera).
        self.ios_window.safe_area(ctx, crate::theme::backdrop());
        // File → Quit: closing the window asks about unsaved documents first.
        if std::mem::take(&mut self.quit_requested) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let chrome = self.ws.chrome;
        if chrome.show_menu_bar() {
            self.menu_bar(ctx);
        }
        if chrome.show_toolbar() {
            self.command_bar(ctx);
        }
        if chrome.show_workspace_tabs() {
            self.workspace_tabs_bar(ctx);
        }
        if chrome.show_toolbar() {
            self.document_tabs_bar(ctx);
        }
        if chrome.show_status_bar() {
            self.status_bar(ctx);
        }
        if chrome.show_toolbar() {
            self.tool_strip(ctx);
        }
        self.new_canvas_window(ctx);
        self.scanner_window(ctx);
        self.image_size_window(ctx);
        self.reset_settings_window(ctx);
        self.bitmap_window(ctx);
        self.duotone_window(ctx);
        self.indexed_window(ctx);
        self.color_table_window(ctx);
        self.cmyk_window(ctx);
        self.print_window(ctx);
        self.text_editor_window(ctx);
        self.save_as_window(ctx);
        self.svg_export_window(ctx);
        self.comic_wizard_window(ctx);
        self.areas(ctx);
        self.floating_panels(ctx);
        self.advanced_tool_window(ctx);
        self.command_palette_window(ctx);
        self.theme_editor_window(ctx);
        self.filter_window(ctx);
        self.update_title(ctx);
        self.modifier_keys.overlay(ctx, crate::theme::panel(), crate::theme::selected());
        self.welcome_window(ctx);
        self.about_window(ctx);
        self.brush_reset_prompt(ctx);
        self.chip_picker_window(ctx);
        self.file_types_window(ctx);
        self.palm_window(ctx);
        self.updater_window(ctx);
        self.crash_report_window(ctx);
        self.close_prompts(ctx);
        if self.doc.selection.is_some() {
            // Marching ants.
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }
        #[cfg(target_os = "android")]
        {
            crate::android::end_frame(ctx);
            // A file another app opened with Painter: once, after the first frame.
            if !self.android_intent_read {
                self.android_intent_read = true;
                if let Some((name, bytes)) = crate::android::opened_file() {
                    let dir = crate::files::app_data_dir().unwrap_or_else(std::env::temp_dir).join("opened");
                    let path = dir.join(crate::android::name_for(&name, &bytes));
                    match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &bytes)) {
                        Ok(()) => self.open_any(path),
                        Err(e) => self.status = format!("Couldn't open the file: {e}"),
                    }
                }
            }
        }
        // egui lays a new window or grid out twice and throws the first pass
        // away, canvas callback and all: its tile uploads go with the next pass.
        if ctx.will_discard() {
            let (mut again, recomposite) = std::mem::take(&mut self.sent_uploads);
            again.append(&mut self.uploads);
            self.uploads = again;
            self.recomposite |= recomposite;
        }
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // On-screen modifier keys first, so they count for every shortcut.
        self.modifier_keys.inject(raw_input);
        self.intercept_document_keys(raw_input);
        self.intercept_board_keys(raw_input);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        // Screenshot runs (PAINTER_COMMANDS / PAINTER_SCREENSHOT) leave the settings alone.
        if self.dev_shot.is_some() {
            return;
        }
        self.brush_library.autosave(&self.brushes, true);
        self.docs.reference.autosave(true);
        self.ws.save(storage);
    }
}

/// The brushes Painter starts with.
pub(crate) fn default_brushes() -> Vec<BrushPreset> {
    BrushSettings::presets().into_iter().map(BrushPreset::new).collect()
}

pub(crate) fn hsva_rgb(c: Hsva) -> [f32; 3] {
    let [r, g, b, _] = c.to_srgba_unmultiplied();
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0]
}

pub(crate) fn rgba8(c: Hsva) -> [u8; 4] {
    let [r, g, b, _] = c.to_srgba_unmultiplied();
    [r, g, b, 255]
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

pub(crate) fn file_stem(path: &std::path::Path) -> String {
    path.file_stem().map_or_else(|| "Image".into(), |s| s.to_string_lossy().into_owned())
}

/// "G-Pen copy", then "G-Pen copy 2", … : the first name not `taken`.
fn copy_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    let base = format!("{name} copy");
    if !taken(&base) {
        return base;
    }
    (2..).map(|n| format!("{base} {n}")).find(|n| !taken(n)).unwrap_or(base)
}

#[cfg(test)]
mod brush_name_tests {
    use super::*;

    #[test]
    fn copies_get_free_names() {
        assert_eq!(copy_name("G-Pen", |_| false), "G-Pen copy");
        assert_eq!(copy_name("G-Pen", |n| n == "G-Pen copy" || n == "G-Pen copy 2"), "G-Pen copy 3");
    }

    #[test]
    fn a_renamed_brush_stays_in_its_group() {
        let mut settings = BrushSettings::g_pen();
        assert_eq!(BrushGroup::of(&settings), BrushGroup::Pen);
        settings.group = Some(crate::toolbar::group_key(BrushGroup::Pen).into());
        settings.name = "My favourite".into();
        assert_eq!(BrushGroup::of(&settings), BrushGroup::Pen, "the name no longer says pen, the pin does");
    }
}
