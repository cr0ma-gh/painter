//! Transform and crop tools on the canvas, and the selection clipboard.
//!
//! **Transform** (Ctrl+T): the active layer's selected pixels (all of them
//! without a selection) are lifted into a floating image. While the gizmo is
//! open the document is untouched: the GPU shows the layer's remaining pixels
//! with the floating image drawn through the current transform over them
//! (`painter_render::TransformPreview`). Apply (Enter) commits on the CPU with
//! `transform_tools::commit`, the same result the preview showed; Cancel (Esc)
//! restores the layer on screen.
//!
//! A layer's mask moves with its pixels (Photoshop's linked mask); while the
//! mask is being edited, the transform moves the mask alone.
//!
//! **Crop** (Shift+C): a box with handles, thirds and aspect presets; Apply moves
//! the canvas window (non-destructive, undoable).
//!
//! **Clipboard**: Ctrl+C / Ctrl+X copy or cut the selected pixels of the
//! active layer; Ctrl+V pastes them on a new layer centred in the view,
//! Ctrl+Shift+V in place.

use std::sync::Arc;
use crate::i18n::tr;

use eframe::egui::{self, Align2, Color32, Pos2, Rect, Stroke, Vec2};
use painter_core::{Document, Layer, LayerId, LayerKind, Mask, SelectionOp, TileCoord, TileStore, TILE_SIZE};
use painter_render::{plane_uploads, Plane, TransformGeometry, TransformPreview};
use transform_tools::crop::{apply_crop as crop_canvas, copy, erase_selected, paste_tiles};
use transform_tools::{
    commit, AspectRatio, BoxHandle, CropBox, FloatingSelection, GizmoHandle, Image, Interpolation, Mat3, Modifiers, PasteMode,
    Placement, TransformGizmo, TransformMode,
};

use crate::app::PainterApp;
use crate::theme;
use crate::tools::{Tool, ToolDrag};

/// Handle grab radius in screen pixels.
const GRAB: f32 = 9.0;
/// Warp mesh subdivisions per grid cell for preview and commit.
const WARP_DETAIL: usize = 8;

pub(crate) struct TransformSession {
    id: u64,
    /// Bumped on every change, so the GPU redraws the preview.
    pub(crate) revision: u64,
    layer: LayerId,
    /// What is transformed: the layer's pixels, or its mask alone (while the
    /// mask is being edited, as Clip Studio does with the mask selected).
    plane: Plane,
    /// The layer without the lifted pixels.
    rest: TileStore,
    floating: FloatingSelection,
    image: Image,
    source: Arc<Vec<u8>>,
    pub(crate) gizmo: TransformGizmo,
    /// The layer's mask, moving with its pixels (Photoshop's linked mask).
    linked_mask: Option<LinkedMask>,
}

/// A mask carried along by a transform of its layer's pixels. The GPU preview
/// shows the pixels live; the mask follows each time a drag ends.
struct LinkedMask {
    rest: TileStore,
    origin: [i32; 2],
    image: Image,
    /// The transform revision the mask on screen was drawn for.
    shown: Option<u64>,
}

impl TransformSession {
    /// Floating-image pixels → canvas pixels.
    fn matrix(&self) -> Mat3 {
        self.gizmo.matrix() * Mat3::translate(self.floating.origin[0] as f64, self.floating.origin[1] as f64)
    }

    fn placement(&self) -> Placement {
        match self.gizmo.mode {
            TransformMode::MeshWarp => {
                let mesh = self.gizmo.warp.tessellate(WARP_DETAIL);
                let [ox, oy] = self.floating.origin.map(|v| v as f64);
                let sources = mesh.uvs.iter().map(|uv| [uv[0] - ox, uv[1] - oy]).collect();
                Placement::Mesh { positions: mesh.positions, sources, indices: mesh.indices }
            }
            _ => Placement::Matrix(self.matrix()),
        }
    }

    /// The linked mask as the transform leaves it, settled; `None` without one.
    /// A mesh warp only bends the pixels: its grid covers them, not the mask.
    fn transformed_mask(&self, canvas: (u32, u32)) -> Option<TileStore> {
        let mask = self.linked_mask.as_ref()?;
        if self.gizmo.mode == TransformMode::MeshWarp {
            return None;
        }
        let placement = Placement::Matrix(self.gizmo.matrix() * Mat3::translate(mask.origin[0] as f64, mask.origin[1] as f64));
        let mut tiles = commit(&mask.rest, &mask.image, &placement, self.gizmo.interpolation, canvas);
        Mask::settle_all(&mut tiles);
        Some(tiles)
    }

    fn geometry(&self) -> TransformGeometry {
        match self.placement() {
            Placement::Matrix(m) => TransformGeometry::Inverse(m.inverse().unwrap_or(Mat3::IDENTITY).to_f32()),
            Placement::Mesh { positions, sources, indices } => TransformGeometry::Mesh {
                positions: positions.iter().map(|p| [p[0] as f32, p[1] as f32]).collect(),
                sources: sources.iter().map(|p| [p[0] as f32, p[1] as f32]).collect(),
                indices,
            },
        }
    }
}

/// A mask lifted for a transform. Its floating pixels carry the amount hidden
/// in their colour and how much of them moves in alpha (the selection's
/// coverage, all of it without one), so once placed they replace the mask
/// under them rather than add to it: a shown part of the mask uncovers what it
/// lands on, as in Photoshop. The result needs [`Mask::settle`]. `None` when
/// nothing would move.
fn lift_mask(tiles: &TileStore, selection: Option<&TileStore>, canvas: (u32, u32)) -> Option<FloatingSelection> {
    let Some(sel) = selection else {
        let mut floating = copy(tiles, None, canvas)?;
        for p in floating.rgba.as_chunks_mut::<4>().0 {
            p[3] = 255;
        }
        return Some(floating);
    };
    let [x0, y0, x1, y1] = painter_core::selection::bounds(sel)?;
    let (x0, y0) = (x0.max(0), y0.max(0));
    let (x1, y1) = (x1.min(canvas.0 as i32 - 1), y1.min(canvas.1 as i32 - 1));
    if x1 < x0 || y1 < y0 {
        return None;
    }
    let (w, h) = ((x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    let size = TILE_SIZE as i32;
    let empty = painter_core::Tile::empty();
    for ty in y0.div_euclid(size)..=y1.div_euclid(size) {
        for tx in x0.div_euclid(size)..=x1.div_euclid(size) {
            let coord = TileCoord::new(tx, ty);
            let Some(cover) = sel.get(coord) else { continue };
            let hidden = tiles.get(coord).map_or(&empty, |t| t);
            let (ox, oy) = coord.origin();
            for ly in 0..TILE_SIZE {
                let y = oy + ly as i32;
                if y < y0 || y > y1 {
                    continue;
                }
                for lx in 0..TILE_SIZE {
                    let x = ox + lx as i32;
                    if x < x0 || x > x1 {
                        continue;
                    }
                    let c = cover.pixel(lx, ly)[3] as u32;
                    if c == 0 {
                        continue;
                    }
                    let v = ((hidden.pixel(lx, ly)[0] as u32 * c + 127) / 255) as u8;
                    let i = (((y - y0) as u32 * w + (x - x0) as u32) * 4) as usize;
                    rgba[i..i + 4].copy_from_slice(&[v, v, v, c as u8]);
                }
            }
        }
    }
    Some(FloatingSelection { origin: [x0, y0], width: w, height: h, rgba })
}

/// What a crop drag is doing.
#[derive(Clone, Copy, PartialEq)]
enum CropDrag {
    Handle(BoxHandle),
    Move,
    /// Drawing a new box from the press point.
    New,
}

pub(crate) struct CropSession {
    pub(crate) crop: CropBox,
    drag: Option<(CropDrag, Pos2, CropBox)>,
}

impl PainterApp {
    fn grab_radius(&self) -> f64 {
        (GRAB / self.view.zoom.max(1e-3)) as f64
    }

    fn mods(&self) -> Modifiers {
        Modifiers { shift: self.pointer_mods.shift, alt: self.pointer_mods.alt }
    }

    // ---- sessions --------------------------------------------------------------

    /// Opens or closes tool sessions when the tool changes. Leaving the
    /// transform tool applies it (like Clip Studio); leaving crop cancels.
    pub(crate) fn sync_tool_sessions(&mut self) {
        if self.tool != Tool::Frame {
            self.frame.cancel();
        }
        if self.tool != Tool::Transform {
            self.transform_blocked = false;
            if self.transform.is_some() {
                self.apply_transform();
            }
        } else if self.transform.is_none() && !self.transform_blocked && self.stroke.is_none() {
            self.begin_transform();
        }
        if self.tool != Tool::Crop {
            self.crop = None;
        } else if self.crop.is_none() {
            self.crop = Some(CropSession { crop: CropBox::full(self.doc.canvas()), drag: None });
        }
    }

    pub(crate) fn begin_transform(&mut self) {
        let canvas = self.doc.canvas();
        let Some(layer) = self.doc.active_layer() else {
            return;
        };
        let id = layer.id;
        // With its mask being edited, the mask alone moves (any kind of layer).
        let mask_only = self.edit_mask && layer.mask.is_some();
        let refuse = match &layer.kind {
            _ if layer.locked => Some("the layer is locked"),
            _ if mask_only => None,
            LayerKind::Raster(_) => None,
            _ => Some("only raster layers can be transformed"),
        };
        if let Some(reason) = refuse {
            self.status = format!("Can't transform: {reason}");
            self.transform_blocked = true;
            return;
        }
        // A file object moves whole (it's drawn again from its file on apply).
        let selection = if layer.file_object.is_some() && !mask_only { None } else { self.doc.selection.as_ref() };
        let (plane, tiles, floating) = if mask_only {
            let tiles = layer.mask.as_ref().map(|m| m.tiles.clone()).unwrap_or_default();
            let floating = lift_mask(&tiles, selection, canvas);
            (Plane::Mask(id), tiles, floating)
        } else {
            let tiles = layer.tiles().cloned().unwrap_or_default();
            let floating = copy(&tiles, selection, canvas);
            (Plane::Color(id), tiles, floating)
        };
        let Some(floating) = floating else {
            self.status = if mask_only { tr("Nothing to transform on this mask") } else { tr("Nothing to transform on this layer") }.into();
            self.transform_blocked = true;
            return;
        };
        let rest = match selection {
            Some(_) => erase_selected(&tiles, selection),
            None => TileStore::new(),
        };
        // Moving the pixels moves their mask too, the same way.
        let linked_mask = match (&layer.mask, mask_only) {
            (Some(mask), false) => lift_mask(&mask.tiles, selection, canvas).map(|f| LinkedMask {
                rest: match selection {
                    Some(_) => erase_selected(&mask.tiles, selection),
                    None => TileStore::new(),
                },
                origin: f.origin,
                image: Image { width: f.width, height: f.height, rgba: f.rgba },
                shown: None,
            }),
            _ => None,
        };
        let source = [floating.origin[0] as f64, floating.origin[1] as f64, floating.width as f64, floating.height as f64];
        let image = Image { width: floating.width, height: floating.height, rgba: floating.rgba.clone() };
        // Show the layer without the lifted pixels; the floating image comes from the preview.
        self.uploads.extend(plane_uploads(plane, &rest));
        self.recomposite = true;
        self.next_transform_id += 1;
        self.transform = Some(TransformSession {
            id: self.next_transform_id,
            revision: 0,
            layer: id,
            plane,
            rest,
            source: Arc::new(floating.rgba.clone()),
            image,
            floating,
            gizmo: TransformGizmo::new(source),
            linked_mask,
        });
        self.status = if mask_only {
            tr("Transform the mask: drag the handles. Enter applies, Esc cancels")
        } else {
            tr("Transform: drag the handles. Enter applies, Esc cancels")
        }
        .into();
    }

    /// Redraws a linked mask where the open transform puts it, once the
    /// handles are let go (the pixels follow the pointer live on the GPU).
    pub(crate) fn sync_transform_mask(&mut self) {
        if self.drag.is_some() {
            return;
        }
        let canvas = self.doc.canvas();
        let Some(session) = &self.transform else { return };
        let Some(mask) = &session.linked_mask else { return };
        if mask.shown == Some(session.revision) {
            return;
        }
        let (layer, revision) = (session.layer, session.revision);
        let moved = session.transformed_mask(canvas);
        let plane = Plane::Mask(layer);
        match moved {
            Some(tiles) => self.uploads.extend(plane_uploads(plane, &tiles)),
            // A mesh warp leaves the mask where it is.
            None => self.upload_plane(plane),
        }
        self.recomposite = true;
        if let Some(mask) = self.transform.as_mut().and_then(|s| s.linked_mask.as_mut()) {
            mask.shown = Some(revision);
        }
    }

    pub(crate) fn apply_transform(&mut self) {
        let Some(session) = self.transform.take() else {
            return;
        };
        let canvas = self.doc.canvas();
        let moved_mask = session.transformed_mask(canvas);
        // File objects: their new place, drawn from the file at full quality.
        if session.plane == Plane::Color(session.layer) && self.doc.find(session.layer).is_some_and(|l| l.file_object.is_some()) {
            let map = match session.placement() {
                Placement::Matrix(_) => Some(session.gizmo.matrix()),
                Placement::Mesh { .. } => None,
            };
            self.checkpoint();
            if self.transform_file_object(session.layer, map) {
                self.set_moved_mask(session.layer, moved_mask);
                self.status = format!("File object placed ({}), drawn from its file", session.gizmo.mode.name());
                if self.tool == Tool::Transform {
                    self.transform_blocked = true;
                }
                return;
            }
            // A mesh warp: the layer became pixels; apply it as pixels below.
        }
        let mut result = commit(&session.rest, &session.image, &session.placement(), session.gizmo.interpolation, canvas);
        self.checkpoint();
        match session.plane {
            Plane::Mask(id) => {
                Mask::settle_all(&mut result);
                if let Some(mask) = self.doc.find_mut(id).and_then(|l| l.mask.as_mut()) {
                    mask.tiles = result;
                }
            }
            _ => {
                if let Some(layer) = self.doc.find_mut(session.layer) {
                    layer.kind = LayerKind::Raster(result);
                }
                self.set_moved_mask(session.layer, moved_mask);
            }
        }
        self.upload_plane(session.plane);
        self.status = format!("Transform applied ({})", session.gizmo.mode.name());
        if self.tool == Tool::Transform {
            // Stay in the tool without reopening on the same pixels at once.
            self.transform_blocked = true;
        }
    }

    /// Puts a linked mask where the transform left it (`None`: it stays).
    fn set_moved_mask(&mut self, id: LayerId, moved: Option<TileStore>) {
        if let Some(tiles) = moved {
            if let Some(mask) = self.doc.find_mut(id).and_then(|l| l.mask.as_mut()) {
                mask.tiles = tiles;
            }
        }
        self.upload_plane(Plane::Mask(id));
    }

    pub(crate) fn cancel_transform(&mut self) {
        let Some(session) = self.transform.take() else {
            return;
        };
        self.upload_plane(session.plane);
        if session.linked_mask.is_some() {
            self.upload_plane(Plane::Mask(session.layer));
        }
        self.status = tr("Transform cancelled").into();
        if self.tool == Tool::Transform {
            self.transform_blocked = true;
        }
    }

    pub(crate) fn reset_transform(&mut self) {
        if let Some(session) = &mut self.transform {
            session.gizmo.reset();
            session.revision += 1;
        }
    }

    /// This frame's GPU preview of the open transform.
    pub(crate) fn transform_preview(&self) -> Option<TransformPreview> {
        let s = self.transform.as_ref()?;
        Some(TransformPreview {
            id: s.id,
            revision: s.revision,
            layer: s.layer,
            mask: matches!(s.plane, Plane::Mask(_)),
            source: s.source.clone(),
            source_size: (s.floating.width, s.floating.height),
            geometry: s.geometry(),
            filter: s.gizmo.interpolation.shader_id(),
        })
    }

    // ---- transform pointer -----------------------------------------------------------

    pub(crate) fn transform_press(&mut self, at: Pos2) {
        if self.transform.is_none() {
            // Clicking again after an apply starts a new transform.
            self.transform_blocked = false;
            self.begin_transform();
        }
        let radius = self.grab_radius();
        let zoom = self.view.zoom as f64;
        let Some(session) = &mut self.transform else {
            return;
        };
        let p = [at.x as f64, at.y as f64];
        if let Some(handle) = session.gizmo.hit_test(p, radius, zoom) {
            session.gizmo.begin_drag(handle, p);
            self.drag = Some(ToolDrag { start: at, current: at, points: Vec::new(), op: SelectionOp::Replace });
        }
    }

    pub(crate) fn transform_move(&mut self, at: Pos2) {
        let mods = self.mods();
        if let Some(session) = &mut self.transform {
            session.gizmo.drag_to([at.x as f64, at.y as f64], mods);
            session.revision += 1;
        }
    }

    pub(crate) fn transform_release(&mut self) {
        if let Some(session) = &mut self.transform {
            session.gizmo.end_drag();
        }
    }

    // ---- crop -----------------------------------------------------------------------

    pub(crate) fn crop_press(&mut self, at: Pos2) {
        let radius = self.grab_radius();
        let Some(session) = &mut self.crop else {
            return;
        };
        let p = [at.x as f64, at.y as f64];
        let r = session.crop.rect;
        let handle = BoxHandle::ALL.into_iter().find(|h| {
            let q = h.position(r);
            (q[0] - p[0]).hypot(q[1] - p[1]) <= radius
        });
        let inside = p[0] >= r[0] && p[0] <= r[0] + r[2] && p[1] >= r[1] && p[1] <= r[1] + r[3];
        let kind = match handle {
            Some(h) => CropDrag::Handle(h),
            None if inside => CropDrag::Move,
            None => CropDrag::New,
        };
        session.drag = Some((kind, at, session.crop));
        self.drag = Some(ToolDrag { start: at, current: at, points: Vec::new(), op: SelectionOp::Replace });
    }

    pub(crate) fn crop_move(&mut self, at: Pos2) {
        let Some(session) = &mut self.crop else {
            return;
        };
        let Some((kind, start, from)) = session.drag else {
            return;
        };
        let p = [at.x as f64, at.y as f64];
        session.crop = from;
        match kind {
            CropDrag::Handle(h) => session.crop.drag(h, p),
            CropDrag::Move => {
                let (dx, dy) = ((at.x - start.x) as f64, (at.y - start.y) as f64);
                session.crop.rect = [from.rect[0] + dx, from.rect[1] + dy, from.rect[2], from.rect[3]];
            }
            CropDrag::New => {
                session.crop.rect = [start.x as f64, start.y as f64, 1.0, 1.0];
                session.crop.drag(BoxHandle::BottomRight, p);
                // Dragging up or left: mirror the box onto the other side of the press.
                let r = &mut session.crop.rect;
                if p[0] < start.x as f64 {
                    r[0] = start.x as f64 - r[2];
                }
                if p[1] < start.y as f64 {
                    r[1] = start.y as f64 - r[3];
                }
            }
        }
    }

    pub(crate) fn crop_release(&mut self) {
        if let Some(session) = &mut self.crop {
            session.drag = None;
        }
    }

    pub(crate) fn apply_crop(&mut self) {
        let Some(session) = self.crop.take() else {
            return;
        };
        let plan = session.crop.plan();
        if plan.offset == [0, 0] && plan.size == self.doc.canvas() {
            self.crop = Some(session);
            return;
        }
        self.checkpoint();
        crop_canvas(&mut self.doc, plan);
        self.resync_gpu();
        self.fit_pending = true;
        self.status = format!("Canvas cropped to {} × {} px (Ctrl+Z restores it)", plan.size.0, plan.size.1);
        if self.tool == Tool::Crop {
            self.crop = Some(CropSession { crop: CropBox::full(self.doc.canvas()), drag: None });
        }
    }

    pub(crate) fn reset_crop(&mut self) {
        let canvas = self.doc.canvas();
        if let Some(session) = &mut self.crop {
            let aspect = session.crop.aspect;
            session.crop = CropBox::full(canvas);
            session.crop.set_aspect(aspect);
        }
    }

    // ---- clipboard --------------------------------------------------------------------

    fn active_raster(&self) -> Option<(LayerId, TileStore)> {
        let layer = self.doc.active_layer()?;
        match &layer.kind {
            LayerKind::Raster(tiles) => Some((layer.id, tiles.clone())),
            _ => None,
        }
    }

    /// Ctrl+C / Ctrl+X. Returns the text to put on the system clipboard (so a
    /// later Ctrl+V reaches the app as a paste event).
    pub(crate) fn copy_selection(&mut self, cut: bool) -> Option<String> {
        let Some((id, tiles)) = self.active_raster() else {
            self.status = tr("Copy works on raster layers").into();
            return None;
        };
        let selection = self.doc.selection.clone();
        let Some(floating) = copy(&tiles, selection.as_ref(), self.doc.canvas()) else {
            self.status = tr("Nothing to copy").into();
            return None;
        };
        let label = format!("Painter selection {} × {} px", floating.width, floating.height);
        if cut {
            self.checkpoint();
            let rest = erase_selected(&tiles, selection.as_ref());
            if let Some(layer) = self.doc.find_mut(id) {
                layer.kind = LayerKind::Raster(rest);
            }
            self.upload_plane(Plane::Color(id));
        }
        self.status = format!("{} {} × {} px", if cut { "Cut" } else { "Copied" }, floating.width, floating.height);
        self.clipboard = Some(floating);
        self.clipboard_dpi = self.doc.dpi;
        Some(label)
    }

    /// File → New from Clipboard (Ctrl+Alt+N): a new document the size of
    /// what's in the clipboard, holding it on its first layer. A picture
    /// copied in another app comes first (72 dpi, like a picture without a
    /// resolution), then a copied image file's path (opened), then Painter's
    /// own copied selection at its document's resolution.
    pub(crate) fn new_from_clipboard(&mut self) {
        let (width, height, pixels, dpi, what) = if let Some(img) = crate::clipboard_image::read() {
            (img.width, img.height, img.premultiplied(), 72, "picture")
        } else if let Some(path) = crate::clipboard_image::copied_image_path() {
            self.open_path(path);
            return;
        } else if let Some(floating) = &self.clipboard {
            (floating.width, floating.height, floating.rgba.clone(), self.clipboard_dpi, "selection")
        } else {
            self.status = tr("The clipboard has no picture: copy one (or a selection) first").into();
            return;
        };
        if width > painter_core::MAX_CANVAS_SIZE || height > painter_core::MAX_CANVAS_SIZE {
            self.status = format!(
                "The copied {what} is {width} × {height} px; canvases go up to {} px a side",
                painter_core::MAX_CANVAS_SIZE
            );
            return;
        }
        let mut doc = Document::new(width, height);
        doc.dpi = dpi;
        if let Some(layer) = doc.active_layer_mut() {
            layer.name = "Pasted".into();
            if let Some(tiles) = layer.tiles_mut() {
                *tiles = TileStore::from_rgba(width, height, &pixels);
            }
        }
        self.set_document(doc, None);
        let active = self.docs.active;
        self.docs.slots[active].name = Some(format!("Clipboard {width}×{height}"));
        self.status = format!("New {width} × {height} px document from the copied {what} ({dpi} dpi)");
    }

    /// Ctrl+V (centred in the view) / Ctrl+Shift+V (in place): a new layer.
    pub(crate) fn paste_selection(&mut self, in_place: bool) {
        let Some(mut floating) = self.clipboard.clone() else {
            self.status = tr("Nothing copied yet").into();
            return;
        };
        // From a document at another resolution: keep the size on paper.
        let factor = crate::document_manager::layer_transfer::print_scale(self.clipboard_dpi, self.doc.dpi, self.tools.paste_match_dpi);
        if (factor - 1.0).abs() > 1e-9 {
            if let Some(scaled) = crate::document_manager::layer_transfer::rescale_floating(&floating, factor) {
                floating = scaled;
            }
        }
        let mode = if in_place {
            PasteMode::InPlace
        } else {
            let c = self.view.screen_to_doc(self.canvas_rect, self.doc_size(), self.canvas_rect.center());
            PasteMode::Centered { view_center: [c.x as f64, c.y as f64] }
        };
        let tiles = paste_tiles(&floating, mode);
        self.checkpoint();
        let id = self.doc.alloc_id();
        self.edit_mask = false;
        let id = self.doc.add_layer(Layer::raster_with(id, "Pasted", tiles));
        self.upload_plane(Plane::Color(id));
        self.recomposite = true;
        self.status = format!("Pasted {} × {} px{}", floating.width, floating.height, if in_place { " in place" } else { "" });
    }

    // ---- overlays -------------------------------------------------------------------

    pub(crate) fn draw_transform_overlay(&self, painter: &egui::Painter, rect: Rect) {
        let Some(session) = &self.transform else {
            return;
        };
        let doc = self.doc_size();
        let to_screen = |p: [f64; 2]| self.view.doc_to_screen(rect, doc, Pos2::new(p[0] as f32, p[1] as f32));
        let gizmo = &session.gizmo;
        let line = |points: Vec<Pos2>, closed: bool| {
            for (width, color) in [(3.0_f32, Color32::from_black_alpha(140)), (1.0, theme::accent())] {
                let shape = if closed {
                    egui::Shape::closed_line(points.clone(), Stroke::new(width, color))
                } else {
                    egui::Shape::line(points.clone(), Stroke::new(width, color))
                };
                painter.add(shape);
            }
        };
        if gizmo.mode == TransformMode::MeshWarp {
            // Grid lines through the warped surface.
            let warp = &gizmo.warp;
            for j in 0..=warp.rows {
                let v = j as f64 / warp.rows as f64;
                line((0..=32).map(|k| to_screen(warp.evaluate(k as f64 / 32.0, v))).collect(), false);
            }
            for i in 0..=warp.cols {
                let u = i as f64 / warp.cols as f64;
                line((0..=32).map(|k| to_screen(warp.evaluate(u, k as f64 / 32.0))).collect(), false);
            }
        } else {
            line(gizmo.outline().into_iter().map(to_screen).collect(), true);
        }
        let handles = gizmo.handles(self.view.zoom as f64);
        let top = handles.iter().find(|(h, _)| *h == GizmoHandle::Scale(BoxHandle::Top)).map(|(_, p)| to_screen(*p));
        for (handle, p) in &handles {
            let p = to_screen(*p);
            match handle {
                GizmoHandle::Rotate => {
                    if let Some(top) = top {
                        painter.line_segment([top, p], Stroke::new(1.0_f32, theme::accent()));
                    }
                    painter.circle_filled(p, 5.0, Color32::WHITE);
                    painter.circle_stroke(p, 5.0, Stroke::new(1.5_f32, theme::accent()));
                }
                GizmoHandle::Pivot => {
                    painter.circle_stroke(p, 6.0, Stroke::new(1.5_f32, Color32::WHITE));
                    painter.line_segment([p - Vec2::new(9.0, 0.0), p + Vec2::new(9.0, 0.0)], Stroke::new(1.0_f32, Color32::WHITE));
                    painter.line_segment([p - Vec2::new(0.0, 9.0), p + Vec2::new(0.0, 9.0)], Stroke::new(1.0_f32, Color32::WHITE));
                }
                GizmoHandle::Corner(_) => {
                    painter.circle_filled(p, 5.5, theme::accent());
                    painter.circle_stroke(p, 5.5, Stroke::new(1.5_f32, Color32::WHITE));
                }
                GizmoHandle::MeshPoint(..) => {
                    painter.circle_filled(p, 4.0, Color32::WHITE);
                    painter.circle_stroke(p, 4.0, Stroke::new(1.0_f32, theme::accent()));
                }
                _ => {
                    let r = Rect::from_center_size(p, Vec2::splat(8.0));
                    painter.rect_filled(r, 1.0, Color32::WHITE);
                    painter.rect_stroke(r, 1.0, Stroke::new(1.0_f32, theme::accent()), egui::StrokeKind::Middle);
                }
            }
        }
    }

    pub(crate) fn draw_crop_overlay(&self, painter: &egui::Painter, rect: Rect) {
        let Some(session) = &self.crop else { return };
        let doc = self.doc_size();
        let to_screen = |x: f64, y: f64| self.view.doc_to_screen(rect, doc, Pos2::new(x as f32, y as f32));
        let [x, y, w, h] = session.crop.rect;
        let (cw, ch) = (doc.x as f64, doc.y as f64);
        // Dim the canvas outside the box (four bands, clipped to the canvas).
        let shade = Color32::from_black_alpha(130);
        let band = |x0: f64, y0: f64, x1: f64, y1: f64| {
            let (x0, y0, x1, y1) = (x0.max(0.0), y0.max(0.0), x1.min(cw), y1.min(ch));
            if x1 > x0 && y1 > y0 {
                let points = vec![to_screen(x0, y0), to_screen(x1, y0), to_screen(x1, y1), to_screen(x0, y1)];
                painter.add(egui::Shape::convex_polygon(points, shade, Stroke::NONE));
            }
        };
        band(0.0, 0.0, cw, y);
        band(0.0, y + h, cw, ch);
        band(0.0, y, x, y + h);
        band(x + w, y, cw, y + h);
        let outline = vec![to_screen(x, y), to_screen(x + w, y), to_screen(x + w, y + h), to_screen(x, y + h)];
        painter.add(egui::Shape::closed_line(outline.clone(), Stroke::new(3.0_f32, Color32::from_black_alpha(150))));
        painter.add(egui::Shape::closed_line(outline, Stroke::new(1.0_f32, Color32::WHITE)));
        for (a, b) in session.crop.thirds() {
            painter.line_segment([to_screen(a[0], a[1]), to_screen(b[0], b[1])], Stroke::new(1.0_f32, Color32::from_white_alpha(110)));
        }
        for handle in BoxHandle::ALL {
            let p = handle.position(session.crop.rect);
            let r = Rect::from_center_size(to_screen(p[0], p[1]), Vec2::splat(8.0));
            painter.rect_filled(r, 1.0, Color32::WHITE);
            painter.rect_stroke(r, 1.0, Stroke::new(1.0_f32, theme::accent()), egui::StrokeKind::Middle);
        }
        let plan = session.crop.plan();
        painter.text(
            to_screen(x, y) + Vec2::new(0.0, -6.0),
            Align2::LEFT_BOTTOM,
            format!("{} × {} px", plan.size.0, plan.size.1),
            egui::FontId::proportional(12.0),
            Color32::WHITE,
        );
    }

    // ---- HUD --------------------------------------------------------------------------

    /// The numeric bar under the canvas while a transform or crop is open.
    pub(crate) fn transform_hud(&mut self, ctx: &egui::Context, rect: Rect) {
        if self.transform.is_none() && self.crop.is_none() {
            return;
        }
        let mut action: Option<&'static str> = None;
        egui::Area::new(egui::Id::new("transform_hud"))
            .order(egui::Order::Foreground)
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(Pos2::new(rect.center().x, rect.bottom() - 10.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(theme::panel().gamma_multiply(0.97))
                    .stroke(Stroke::new(1.0_f32, theme::line()))
                    .corner_radius(6)
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if let Some(session) = &mut self.transform {
                                let mut changed = false;
                                let mut mode = session.gizmo.mode;
                                egui::ComboBox::from_id_salt("transform_mode").selected_text(tr(mode.name())).show_ui(ui, |ui| {
                                    for m in TransformMode::ALL {
                                        ui.selectable_value(&mut mode, m, tr(m.name()));
                                    }
                                });
                                if mode != session.gizmo.mode {
                                    session.gizmo.set_mode(mode);
                                    changed = true;
                                }
                                let mut hud = session.gizmo.hud();
                                let before = hud;
                                ui.separator();
                                ui.label(tr("X"));
                                ui.add(egui::DragValue::new(&mut hud.position[0]).speed(1.0).max_decimals(1));
                                ui.label(tr("Y"));
                                ui.add(egui::DragValue::new(&mut hud.position[1]).speed(1.0).max_decimals(1));
                                let affine = matches!(session.gizmo.mode, TransformMode::ScaleRotate | TransformMode::Skew);
                                ui.add_enabled_ui(affine, |ui| {
                                    ui.label(tr("W"));
                                    ui.add(egui::DragValue::new(&mut hud.scale[0]).speed(0.5).suffix("%").max_decimals(1));
                                    ui.label(tr("H"));
                                    ui.add(egui::DragValue::new(&mut hud.scale[1]).speed(0.5).suffix("%").max_decimals(1));
                                    ui.label(tr("Rotate"));
                                    ui.add(egui::DragValue::new(&mut hud.rotation).speed(0.5).suffix("°").max_decimals(1));
                                });
                                ui.separator();
                                egui::ComboBox::from_id_salt("transform_filter").selected_text(tr(hud.interpolation.name())).show_ui(
                                    ui,
                                    |ui| {
                                        for f in Interpolation::ALL {
                                            ui.selectable_value(&mut hud.interpolation, f, tr(f.name()));
                                        }
                                    },
                                );
                                // Pixel art: whole multiples, quarter turns, the pixel grid.
                                let pixel = hud.interpolation == Interpolation::Nearest && session.gizmo.mode == TransformMode::ScaleRotate;
                                if pixel {
                                    ui.checkbox(&mut self.whole_pixels, tr("Whole pixels")).on_hover_text(
                                        tr("Scale by ×2, ×3… or ½, ⅓…, turn by quarter turns and stay on the pixel grid, so every pixel stays a square block"),
                                    );
                                    for (label, k) in [("½", 0.5), ("×1", 1.0), ("×2", 2.0), ("×3", 3.0), ("×4", 4.0)] {
                                        if ui.small_button(label).on_hover_text(format!("Scale to {}%", k * 100.0)).clicked() {
                                            hud.scale = [k * 100.0; 2];
                                        }
                                    }
                                }
                                if hud != before {
                                    session.gizmo.set_hud(hud);
                                    changed = true;
                                }
                                if pixel && self.whole_pixels {
                                    let (now, snapped) = (session.gizmo.hud(), session.gizmo.whole_pixel_hud());
                                    let off = (now.position[0] - snapped.position[0]).abs()
                                        + (now.position[1] - snapped.position[1]).abs()
                                        + (now.scale[0] - snapped.scale[0]).abs()
                                        + (now.scale[1] - snapped.scale[1]).abs()
                                        + (now.rotation - snapped.rotation).abs();
                                    if off > 1e-6 {
                                        session.gizmo.set_hud(snapped);
                                        changed = true;
                                    }
                                }
                                if changed {
                                    session.revision += 1;
                                }
                            } else if let Some(session) = &mut self.crop {
                                ui.label(tr("Aspect"));
                                for (name, aspect) in AspectRatio::presets() {
                                    if ui.selectable_label(session.crop.aspect == aspect, name).clicked() {
                                        session.crop.set_aspect(aspect);
                                    }
                                }
                                // Custom ratio: the current box's, editable.
                                let mut ratio = match session.crop.aspect {
                                    AspectRatio::Ratio(w, h) => [w, h],
                                    AspectRatio::Free => [session.crop.rect[2].round(), session.crop.rect[3].round()],
                                };
                                ui.label(tr("Custom"));
                                let a = ui.add(egui::DragValue::new(&mut ratio[0]).range(1.0..=10_000.0).speed(0.2).max_decimals(0));
                                ui.label(tr(":"));
                                let b = ui.add(egui::DragValue::new(&mut ratio[1]).range(1.0..=10_000.0).speed(0.2).max_decimals(0));
                                if a.changed() || b.changed() {
                                    session.crop.set_aspect(AspectRatio::Ratio(ratio[0], ratio[1]));
                                }
                                ui.separator();
                                let plan = session.crop.plan();
                                ui.label(format!("{} × {} px", plan.size.0, plan.size.1));
                            }
                            ui.separator();
                            if ui.button(tr("Apply")).on_hover_text(tr("Enter")).clicked() {
                                action = Some("apply");
                            }
                            if ui.button(tr("Cancel")).on_hover_text(tr("Esc")).clicked() {
                                action = Some("cancel");
                            }
                            if ui.button(tr("Reset")).clicked() {
                                action = Some("reset");
                            }
                        });
                    });
            });
        match (action, self.transform.is_some()) {
            (Some("apply"), true) => self.apply_transform(),
            (Some("cancel"), true) => self.cancel_transform(),
            (Some("reset"), true) => self.reset_transform(),
            (Some("apply"), false) => self.apply_crop(),
            (Some("cancel"), false) => {
                self.crop = None;
                self.tool = Tool::Brush;
            }
            (Some("reset"), false) => self.reset_crop(),
            _ => {}
        }
    }

    /// Enter / Esc / Ctrl+T / C and the clipboard events. Runs only when no
    /// text field has the keyboard.
    pub(crate) fn transform_shortcuts(&mut self, ctx: &egui::Context) {
        let (enter, escape, ctrl_t, crop_key, events, shift) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
                i.modifiers.command && i.key_pressed(egui::Key::T),
                // Shift+C (plain C is the transparent colour, as in Clip Studio).
                i.key_pressed(egui::Key::C) && !i.modifiers.command && !i.modifiers.alt && i.modifiers.shift,
                i.events.clone(),
                i.modifiers.shift,
            )
        });
        if ctrl_t {
            self.transform_blocked = false;
            self.tool = Tool::Transform;
        }
        if crop_key {
            self.tool = Tool::Crop;
        }
        if enter {
            if self.transform.is_some() {
                self.apply_transform();
            } else if self.crop.is_some() {
                self.apply_crop();
            }
        }
        if escape {
            if self.transform.is_some() {
                self.cancel_transform();
            } else if self.crop.is_some() {
                self.crop = None;
                self.tool = Tool::Brush;
            }
        }
        for event in events {
            match event {
                egui::Event::Copy => {
                    if let Some(text) = self.copy_selection(false) {
                        ctx.copy_text(text);
                    }
                }
                egui::Event::Cut => {
                    if let Some(text) = self.copy_selection(true) {
                        ctx.copy_text(text);
                    }
                }
                egui::Event::Paste(_) => self.paste_selection(shift),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 128 × 64 mask hiding its left half fully and a grey band at x 70..80.
    fn half_hidden() -> TileStore {
        let (w, h) = (128u32, 64u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let v = if x < 64 { 255 } else if (70..80).contains(&x) { 100 } else { 0 };
                let i = ((y * w + x) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(&[v; 4]);
            }
        }
        TileStore::from_rgba(w, h, &rgba)
    }

    fn moved(tiles: &TileStore, selection: Option<&TileStore>, dx: f64) -> TileStore {
        let canvas = (128, 64);
        let f = lift_mask(tiles, selection, canvas).expect("something to move");
        let rest = match selection {
            Some(_) => erase_selected(tiles, selection),
            None => TileStore::new(),
        };
        let image = Image { width: f.width, height: f.height, rgba: f.rgba };
        let m = Mat3::translate(dx, 0.0) * Mat3::translate(f.origin[0] as f64, f.origin[1] as f64);
        let mut out = commit(&rest, &image, &Placement::Matrix(m), Interpolation::Nearest, canvas);
        Mask::settle_all(&mut out);
        out
    }

    #[test]
    fn a_moved_mask_replaces_what_it_lands_on() {
        let mask = half_hidden();
        // In place, nothing changes.
        let same = moved(&mask, None, 0.0);
        for x in [0, 63, 64, 75, 100] {
            assert_eq!(same.pixel(x, 10), mask.pixel(x, 10), "x {x}");
        }
        // The whole mask 20 px right: the grey band follows, what it left is shown.
        let right = moved(&mask, None, 20.0);
        assert_eq!(right.pixel(10, 5)[3], 0, "uncovered: shown");
        assert_eq!(right.pixel(70, 5)[3], 255);
        assert_eq!(right.pixel(95, 5)[3], 100);
        // A selection of the shown part, moved onto the hidden half, uncovers it there.
        let sel = painter_core::selection::rect_mask([100.0, 0.0], [128.0, 64.0], (128, 64));
        let left = moved(&mask, Some(&sel), -60.0);
        assert_eq!(left.pixel(50, 5)[3], 0, "the shown part landed on hidden pixels");
        assert_eq!(left.pixel(20, 5)[3], 255, "hidden pixels outside it stay hidden");
        assert_eq!(left.pixel(75, 5)[3], 100, "unselected parts stay");
    }
}
