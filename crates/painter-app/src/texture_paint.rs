//! Painting 3D models, like Blender's texture painting, with every tool.
//!
//! An object of a 3D layer can have a colour texture (new, loaded from an
//! image file, or a material's image next to an OBJ). Two ways to paint it:
//!
//! - **Paint on model**: a "Paint on model" layer goes over the 3D layer and
//!   becomes the active layer, so brushes, fills, shapes and gradients all
//!   work on it as usual. When a stroke ends, its paint is carried onto the
//!   texture through the camera (`scene_3d::texture::project`: only what's in
//!   view, fading on surfaces seen edge-on) and the layer is emptied again.
//!   The middle button and the wheel still turn the model meanwhile. Undo
//!   takes back the stroke and the texture together.
//! - **Edit texture (UV)**: the texture opens as a document of its own, with
//!   the mesh's UV layout drawn over it and a live 3D preview; what's painted
//!   there goes back to the model as you work (and when you switch tabs).

use std::collections::HashMap;
use crate::i18n::tr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Pos2, Rect, Stroke};
use painter_core::{io, Document, Layer, LayerId, LayerKind, TileStore};
use painter_render::{Plane, SceneDraw, SceneLight, ScenePreview, SceneRenderer};
use scene_3d::camera::ViewCamera;
use scene_3d::mesh::Mesh;
use scene_3d::texture::{project, uv_edges, Texture};

use crate::app::PainterApp;
use crate::document_manager::session_store::Slot;
use crate::scene_ui::SceneLayer;
use crate::tools::Tool;

/// Texture sizes offered for a new texture.
const SIZES: [u32; 4] = [512, 1024, 2048, 4096];
/// How long after the last change a texture document is copied to its model.
const PUSH_DELAY: Duration = Duration::from_millis(250);
/// Side of the live 3D preview of a texture document.
const PREVIEW_SIDE: u32 = 320;

/// Painting on a model through a layer over the 3D layer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ModelPaint {
    pub(crate) scene_layer: LayerId,
    pub(crate) object: usize,
    /// The layer the tools paint on; emptied onto the texture after each stroke.
    pub(crate) overlay: LayerId,
}

/// A document showing an object's texture, linked back to the model.
pub(crate) struct TextureLink {
    /// Tab holding the 3D layer.
    source_slot: u64,
    layer: LayerId,
    object: usize,
    /// The object's mesh when the texture was opened: the link holds while it's the same.
    mesh: Arc<Mesh>,
    color: [f32; 3],
    camera: ViewCamera,
    preview: Option<egui::TextureHandle>,
    /// Copy to the model (and redraw the preview) at this time.
    push_at: Option<Instant>,
}

/// What the texture section of the 3D panel asked for.
enum Action {
    New,
    Load,
    Save,
    Remove,
    PaintOnModel,
    StopPainting,
    EditUv,
}

/// A texture as a plain image layer's picture: premultiplied tiles.
fn texture_tiles(t: &Texture) -> TileStore {
    let mut rgba = t.rgba.clone();
    for p in rgba.as_chunks_mut::<4>().0 {
        *p = painter_core::blend::premultiply(*p);
    }
    TileStore::from_rgba(t.width, t.height, &rgba)
}

/// A document's composite as a texture (straight alpha).
fn document_texture(doc: &Document) -> Option<Texture> {
    let mut rgba = io::flatten(doc);
    for p in rgba.as_chunks_mut::<4>().0 {
        *p = painter_core::blend::unpremultiply(*p);
    }
    Texture::from_rgba(doc.width, doc.height, rgba)
}

impl PainterApp {
    /// The selected object's texture controls, under the 3D panel.
    pub(crate) fn texture_panel(&mut self, ui: &mut egui::Ui, id: LayerId) {
        let Some(scene) = self.scenes.get(&id).map(|l| &l.scene) else { return };
        let Some(sel) = scene.selected.filter(|&i| i < scene.objects.len()) else {
            if self.model_paint.is_some() {
                ui.small(tr("Painting on a model: select its object to see the texture."));
            }
            return;
        };
        let object = &scene.objects[sel];
        let texture = object.texture.as_ref().map(|t| (t.width, t.height));
        let has_uvs = object.mesh.vertices.iter().any(|v| v.uv != [0.0, 0.0]);
        let painting = self.model_paint.is_some_and(|p| p.scene_layer == id && p.object == sel);
        let mut action = None;
        egui::CollapsingHeader::new(tr("Texture painting")).id_salt("scene_texture").default_open(true).show(ui, |ui| {
            match texture {
                Some((w, h)) => ui.label(format!("Texture: {w} × {h}")),
                None => ui.label(tr("No texture yet: the object shows its colour.")),
            };
            if !has_uvs {
                ui.colored_label(
                    crate::theme::text_dim(),
                    "This mesh has no UVs, so paint can't be placed on it: unwrap it in Blender and export it again.",
                );
            }
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("texture_size")
                    .selected_text(format!("{0} × {0}", self.new_texture_size))
                    .width(96.0)
                    .show_ui(ui, |ui| {
                        for s in SIZES {
                            ui.selectable_value(&mut self.new_texture_size, s, format!("{s} × {s}"));
                        }
                    });
                if ui.button(tr("New")).on_hover_text(tr("A new transparent texture over the colour (replaces the current one)")).clicked() {
                    action = Some(Action::New);
                }
                if ui.button(tr("Load…")).on_hover_text(tr("An image file as the texture")).clicked() {
                    action = Some(Action::Load);
                }
            });
            ui.horizontal(|ui| {
                if ui.add_enabled(texture.is_some(), egui::Button::new(tr("Save…"))).on_hover_text(tr("The texture as a PNG")).clicked() {
                    action = Some(Action::Save);
                }
                if ui.add_enabled(texture.is_some(), egui::Button::new(tr("Remove"))).clicked() {
                    action = Some(Action::Remove);
                }
            });
            ui.horizontal(|ui| {
                if painting {
                    if ui.button(tr("Stop painting")).clicked() {
                        action = Some(Action::StopPainting);
                    }
                } else if ui
                    .add_enabled(has_uvs, egui::Button::new(tr("Paint on model")))
                    .on_hover_text(tr("Paint over the model with any tool; each stroke goes onto the texture"))
                    .clicked()
                {
                    action = Some(Action::PaintOnModel);
                }
                if ui
                    .add_enabled(has_uvs, egui::Button::new(tr("Edit texture (UV)")))
                    .on_hover_text(tr("Open the texture as a document, with the UV layout and a live 3D preview"))
                    .clicked()
                {
                    action = Some(Action::EditUv);
                }
            });
            if painting {
                ui.small(tr("Paint on the “Paint on model” layer; middle drag turns the model, the wheel zooms."));
            }
        });
        match action {
            Some(Action::New) => {
                self.checkpoint();
                let size = self.new_texture_size;
                self.set_texture(id, sel, Some(Texture::new(size, size)));
            }
            Some(Action::Load) => self.load_texture_dialog(id, sel),
            Some(Action::Save) => self.save_texture_dialog(id, sel),
            Some(Action::Remove) => {
                self.checkpoint();
                self.set_texture(id, sel, None);
            }
            Some(Action::PaintOnModel) => self.start_model_paint(id, sel),
            Some(Action::StopPainting) => self.stop_model_paint(),
            Some(Action::EditUv) => self.open_texture_document(id, sel),
            None => {}
        }
    }

    fn set_texture(&mut self, id: LayerId, object: usize, texture: Option<Texture>) {
        if let Some(o) = self.scenes.get_mut(&id).and_then(|l| l.scene.objects.get_mut(object)) {
            o.texture = texture.map(Arc::new);
        }
        self.scene_changed(id);
    }

    /// The object's texture, made first when it has none.
    fn ensure_texture(&mut self, id: LayerId, object: usize) {
        let missing = self.scenes.get(&id).and_then(|l| l.scene.objects.get(object)).is_some_and(|o| o.texture.is_none());
        if missing {
            let size = self.new_texture_size;
            self.set_texture(id, object, Some(Texture::new(size, size)));
        }
    }

    fn load_texture_dialog(&mut self, id: LayerId, object: usize) {
        let Some(path) =
            crate::files::FileDialog::new().add_filter("Image", &["png", "jpg", "jpeg", "bmp", "tga", "webp"]).pick_file()
        else {
            return;
        };
        match load_texture(&path) {
            Ok(t) => {
                self.checkpoint();
                self.status = format!("Texture {} × {} from {}", t.width, t.height, path.display());
                self.set_texture(id, object, Some(t));
            }
            Err(err) => self.status = format!("Couldn't load {}: {err}", path.display()),
        }
    }

    fn save_texture_dialog(&mut self, id: LayerId, object: usize) {
        let Some(o) = self.scenes.get(&id).and_then(|l| l.scene.objects.get(object)) else { return };
        let Some(t) = o.texture.clone() else { return };
        let name = format!("{} texture.png", o.name);
        let Some(path) = crate::files::FileDialog::new().add_filter("PNG image", &["png"]).set_file_name(name).save_file() else {
            return;
        };
        let saved = image::RgbaImage::from_raw(t.width, t.height, t.rgba.clone())
            .ok_or_else(|| "texture size mismatch".to_string())
            .and_then(|img| img.save_with_format(&path, image::ImageFormat::Png).map_err(|e| e.to_string()));
        self.status = match saved {
            Ok(()) => {
                crate::files::offer_export(&path);
                format!("Saved the texture as {}", path.display())
            }
            Err(err) => format!("Couldn't save the texture: {err}"),
        };
    }

    // ---- painting on the model -------------------------------------------------------

    pub(crate) fn start_model_paint(&mut self, id: LayerId, object: usize) {
        self.stop_model_paint();
        self.checkpoint();
        self.ensure_texture(id, object);
        self.doc.active = id;
        let overlay = self.doc.alloc_id();
        self.doc.add_layer(Layer::raster(overlay, "Paint on model"));
        self.model_paint = Some(ModelPaint { scene_layer: id, object, overlay });
        if self.tool == Tool::Scene3D {
            self.tool = Tool::Brush;
        }
        self.resync_gpu();
        self.status = tr("Painting on the model: each stroke goes onto its texture. Middle drag turns it, the wheel zooms").into();
    }

    /// Ends painting on the model; the paint layer goes away.
    pub(crate) fn stop_model_paint(&mut self) {
        self.model_paint_tick();
        let Some(paint) = self.model_paint.take() else { return };
        if self.doc.find(paint.overlay).is_some() {
            self.checkpoint();
            self.doc.delete_layer(paint.overlay);
            if self.doc.find(paint.scene_layer).is_some() {
                self.doc.active = paint.scene_layer;
            }
            self.resync_gpu();
        }
    }

    /// After each stroke on the paint layer: its paint onto the texture.
    pub(crate) fn model_paint_tick(&mut self) {
        let Some(paint) = self.model_paint else { return };
        let (Some(overlay), true) = (self.doc.find(paint.overlay), self.scenes.contains_key(&paint.scene_layer)) else {
            // The layer was deleted (or the 3D layer): painting ends.
            self.model_paint = None;
            return;
        };
        if self.stroke.is_some() || self.drag.is_some() || self.transform.is_some() {
            return;
        }
        if overlay.tiles().is_none_or(TileStore::is_empty) {
            return;
        }
        let size = self.doc.canvas();
        let mut alone = overlay.clone();
        alone.visible = true;
        let flat = io::flatten(&Document::from_parts(size.0, size.1, vec![alone], paint.overlay, 1));
        let landed = self.scenes.get_mut(&paint.scene_layer).and_then(|layer| {
            let scene = &layer.scene;
            let mut texture = Texture::clone(scene.objects.get(paint.object)?.texture.as_deref()?);
            project(scene, paint.object, &flat, size, &mut texture)?;
            layer.scene.objects[paint.object].texture = Some(Arc::new(texture));
            Some(())
        });
        if landed.is_some() {
            self.scene_changed(paint.scene_layer);
        } else {
            self.status = tr("That paint didn't land on the model (off it, or behind something)").into();
        }
        if let Some(l) = self.doc.find_mut(paint.overlay) {
            l.kind = LayerKind::Raster(TileStore::new());
        }
        self.upload_plane(Plane::Color(paint.overlay));
    }

    // ---- the texture as a document ---------------------------------------------------

    /// Opens object `object`'s texture in a new tab, linked to the model.
    pub(crate) fn open_texture_document(&mut self, id: LayerId, object: usize) {
        self.ensure_texture(id, object);
        let Some(o) = self.scenes.get(&id).and_then(|l| l.scene.objects.get(object)).cloned() else { return };
        let Some(texture) = o.texture.clone() else { return };
        let source_slot = self.docs.slots[self.docs.active].id;
        let mut doc = Document::from_parts(texture.width, texture.height, Vec::new(), LayerId(0), 1);
        let layer = doc.alloc_id();
        doc.layers.push(Layer::raster_with(layer, "Texture", texture_tiles(&texture)));
        doc.active = layer;
        let mut camera = ViewCamera::default();
        camera.frame(&o.mesh.bounds);
        let link = TextureLink {
            source_slot,
            layer: id,
            object,
            mesh: o.mesh.clone(),
            color: o.color,
            camera,
            preview: None,
            push_at: Some(Instant::now()),
        };
        self.set_document(doc, None);
        let slot = &mut self.docs.slots[self.docs.active];
        if slot.id == source_slot {
            return; // no new tab
        }
        slot.name = Some(format!("{} texture", o.name));
        self.texture_links.insert(slot.id, link);
        if self.tool == Tool::Scene3D {
            self.tool = Tool::Brush;
        }
        self.status = tr("Texture opened: paint it here, the model follows. The UV layout is drawn over it").into();
    }

    /// Each frame: texture documents send their picture to their models.
    pub(crate) fn texture_documents_tick(&mut self, ctx: &egui::Context) {
        if self.texture_links.is_empty() {
            return;
        }
        // Links whose tab was closed go.
        let open: std::collections::HashSet<u64> = self.docs.slots.iter().map(|s| s.id).collect();
        self.texture_links.retain(|slot, link| open.contains(slot) && open.contains(&link.source_slot));
        let active = self.docs.slots[self.docs.active].id;
        if std::mem::take(&mut self.doc_touched) {
            if let Some(link) = self.texture_links.get_mut(&active) {
                link.push_at.get_or_insert(Instant::now() + PUSH_DELAY);
            }
        }
        let now = Instant::now();
        let due: Vec<u64> = self.texture_links.iter().filter(|(_, l)| l.push_at.is_some_and(|t| t <= now)).map(|(s, _)| *s).collect();
        for slot in due {
            // A suspended document's pixels are on disk: push once they're back.
            if let Some(i) = self.docs.index_of(slot).filter(|&i| self.docs.is_suspended(i)) {
                self.wake_document(i);
                continue;
            }
            let texture = if slot == active {
                document_texture(&self.doc)
            } else {
                self.docs.slots.iter().find(|s| s.id == slot).and_then(|s| s.parked.as_ref()).and_then(|p| document_texture(&p.doc))
            };
            let Some(texture) = texture.map(Arc::new) else { continue };
            let Some(link) = self.texture_links.get_mut(&slot) else { continue };
            link.push_at = None;
            let (source_slot, layer, object, mesh) = (link.source_slot, link.layer, link.object, link.mesh.clone());
            let pushed = push_texture(&mut self.docs.slots, active, &mut self.scenes, source_slot, layer, object, &mesh, &texture);
            if pushed == Pushed::Active {
                // The model's tab is the one shown: its scenes are the app's.
                self.scene_changed(layer);
                self.dirty = true;
            }
            if slot == active {
                self.render_texture_preview(ctx, slot, texture);
            }
        }
        if self.texture_links.values().any(|l| l.push_at.is_some()) {
            ctx.request_repaint_after(PUSH_DELAY);
        }
        if self.texture_links.contains_key(&active) {
            self.texture_preview_window(ctx, active);
        }
    }

    fn render_texture_preview(&mut self, ctx: &egui::Context, slot: u64, texture: Arc<Texture>) {
        let Some(rs) = self.render_state.clone() else { return };
        let Some(link) = self.texture_links.get_mut(&slot) else { return };
        let side = PREVIEW_SIDE;
        let preview = ScenePreview {
            layer: LayerId(u64::MAX),
            revision: 0,
            view_proj: link.camera.view_projection(1.0).0,
            draws: vec![SceneDraw {
                mesh: link.mesh.clone(),
                model: scene_3d::math::Mat4::IDENTITY.0,
                color: link.color,
                selected: false,
                texture: Some(texture),
            }],
            lights: vec![SceneLight::Sun { direction: link.camera.forward().to_array(), color: [0.85; 3] }],
            ambient: 0.3,
        };
        let renderer = self.scene_renderer.get_or_insert_with(|| SceneRenderer::new(&rs.device));
        let rgba = renderer.render_to_rgba(&rs.device, &rs.queue, &preview, (side, side));
        let image = egui::ColorImage::from_rgba_premultiplied([side as usize, side as usize], &rgba);
        match &mut link.preview {
            Some(handle) => handle.set(image, egui::TextureOptions::LINEAR),
            None => link.preview = Some(ctx.load_texture("texture-preview", image, egui::TextureOptions::LINEAR)),
        }
    }

    /// The live 3D preview of a texture document; dragging it turns the model.
    fn texture_preview_window(&mut self, ctx: &egui::Context, slot: u64) {
        let Some(link) = self.texture_links.get_mut(&slot) else { return };
        let mut turned = false;
        egui::Window::new(tr("3D Preview"))
            .id(egui::Id::new("texture_preview"))
            .default_pos(egui::pos2(60.0, 120.0))
            .resizable(false)
            .show(ctx, |ui| {
                let size = egui::vec2(PREVIEW_SIDE as f32, PREVIEW_SIDE as f32);
                let (rect, response) = ui.allocate_exact_size(size, egui::Sense::drag());
                ui.painter().rect_filled(rect, 4.0, crate::theme::well());
                if let Some(tex) = &link.preview {
                    ui.painter().image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                }
                if response.dragged() {
                    let d = response.drag_delta();
                    link.camera.orbit(d.x, d.y);
                    turned = true;
                }
                if response.hovered() {
                    let scroll = ui.input(|i| i.raw_scroll_delta.y);
                    if scroll != 0.0 {
                        link.camera.zoom((-scroll * 0.0015).exp());
                        turned = true;
                    }
                }
                ui.small(tr("Drag to turn, wheel to zoom. Painting here updates the model."));
            });
        if turned {
            link.push_at = Some(Instant::now());
        }
    }

    /// The UV layout over a texture document.
    pub(crate) fn draw_uv_overlay(&self, painter: &egui::Painter, rect: Rect) {
        let Some(slot) = self.docs.slots.get(self.docs.active).map(|s| s.id) else { return };
        let Some(link) = self.texture_links.get(&slot) else { return };
        let doc = self.doc_size();
        let (w, h) = self.doc.canvas();
        let stroke = Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(80, 200, 255, 150));
        for [a, b] in uv_edges(&link.mesh, w, h) {
            let p = self.view.doc_to_screen(rect, doc, Pos2::new(a[0], a[1]));
            let q = self.view.doc_to_screen(rect, doc, Pos2::new(b[0], b[1]));
            painter.line_segment([p, q], stroke);
        }
    }
}

/// An image file as a texture.
pub(crate) fn load_texture(path: &std::path::Path) -> Result<Texture, String> {
    let img = image::open(path).map_err(|e| e.to_string())?.to_rgba8();
    Texture::from_rgba(img.width(), img.height(), img.into_raw()).ok_or_else(|| "empty image".into())
}

/// The diffuse image an OBJ's material names (`mtllib` → `map_Kd`), if it's there.
pub(crate) fn obj_texture(obj_path: &std::path::Path, obj_text: &str) -> Option<Texture> {
    let dir = obj_path.parent()?;
    let lib = obj_text.lines().find_map(|l| l.trim().strip_prefix("mtllib ").map(|s| s.trim().to_string()))?;
    let mtl = std::fs::read_to_string(dir.join(lib)).ok()?;
    let map = mtl.lines().find_map(|l| l.trim().strip_prefix("map_Kd ").map(|s| s.trim().to_string()))?;
    // Options like "-bm 1 file.png": the file name comes last.
    let file = map.split_whitespace().last()?;
    load_texture(&dir.join(file)).ok()
}

/// Where [`push_texture`] put a texture document's picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pushed {
    /// Onto the model in the active document (`PainterApp::scenes`): the caller redraws it.
    Active,
    /// Onto the model in a parked tab, marked to redraw once shown.
    Parked,
    /// Nowhere: the tab, layer or object is gone, or the object's mesh changed.
    Nowhere,
}

/// Sets `texture` on object `object` of 3D layer `layer` in tab `source_slot`.
/// The active tab (`active`) keeps its scenes in the app (`active_scenes`),
/// not in its slot, which is parked only while another tab is shown.
#[allow(clippy::too_many_arguments)]
fn push_texture(
    slots: &mut [Slot],
    active: u64,
    active_scenes: &mut HashMap<LayerId, SceneLayer>,
    source_slot: u64,
    layer: LayerId,
    object: usize,
    mesh: &Arc<Mesh>,
    texture: &Arc<Texture>,
) -> Pushed {
    let set = |scenes: &mut HashMap<LayerId, SceneLayer>| {
        let o = scenes.get_mut(&layer)?.scene.objects.get_mut(object).filter(|o| Arc::ptr_eq(&o.mesh, mesh))?;
        o.texture = Some(texture.clone());
        Some(())
    };
    if source_slot == active {
        return if set(active_scenes).is_some() { Pushed::Active } else { Pushed::Nowhere };
    }
    let Some(source) = slots.iter_mut().find(|s| s.id == source_slot) else { return Pushed::Nowhere };
    let Some(parked) = source.parked.as_mut() else { return Pushed::Nowhere };
    if set(&mut parked.scenes).is_none() {
        return Pushed::Nowhere;
    }
    if let Some(scene_layer) = parked.scenes.get_mut(&layer) {
        scene_layer.touch();
    }
    parked.dirty = true;
    source.revision += 1;
    Pushed::Parked
}

/// Map of texture documents by tab.
pub(crate) type TextureLinks = HashMap<u64, TextureLink>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obj_materials_bring_their_image() {
        let dir = std::env::temp_dir().join(format!("painter-obj-tex-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::from_raw(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 128]).unwrap().save(dir.join("skin.png")).unwrap();
        std::fs::write(dir.join("m.mtl"), "newmtl a\nKd 1 1 1\nmap_Kd -bm 1 skin.png\n").unwrap();
        let obj = "mtllib m.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 0 1\nf 1/1 2/2 3/3\n";
        let t = obj_texture(&dir.join("m.obj"), obj).expect("texture found");
        assert_eq!((t.width, t.height), (2, 1));
        assert_eq!(&t.rgba[4..], &[0, 0, 255, 128], "straight alpha kept");
        assert!(obj_texture(&dir.join("m.obj"), "v 0 0 0\n").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Scenes with one 3D layer holding a cube, and that cube's mesh.
    fn cube_scenes(layer: LayerId) -> (HashMap<LayerId, SceneLayer>, Arc<Mesh>) {
        let scene = scene_3d::scene::Scene3D::from_mesh(Mesh::cube());
        let mesh = scene.objects[0].mesh.clone();
        let mut scenes = HashMap::new();
        crate::scene_ui::restore(&mut scenes, HashMap::from([(layer, scene)]));
        (scenes, mesh)
    }

    fn parked(scenes: HashMap<LayerId, SceneLayer>) -> crate::document_manager::session_store::DocSession {
        crate::document_manager::session_store::DocSession {
            doc: Document::from_parts(4, 4, Vec::new(), LayerId(0), 1),
            history: crate::app::new_history(),
            view: crate::view::ViewTransform::default(),
            comic: None,
            comic_undo: Vec::new(),
            comic_redo: Vec::new(),
            selected_panel: None,
            scenes,
            scene_undo: Vec::new(),
            scene_redo: Vec::new(),
            file_path: None,
            dirty: false,
            edit_mask: false,
            mask_view_mode: painter_render::MaskViewMode::Off,
            proof_colors: false,
            gamut_warning: false,
            parked_at: Instant::now(),
            residency: crate::document_manager::session_store::Residency::Resident,
            thumbnail: None,
        }
    }

    fn texture_of(scenes: &HashMap<LayerId, SceneLayer>, layer: LayerId) -> Option<Arc<Texture>> {
        scenes[&layer].scene.objects[0].texture.clone()
    }

    #[test]
    fn texture_links_reach_parked_and_active_models() {
        let layer = LayerId(7);
        let texture = Arc::new(Texture::from_rgba(1, 1, vec![9, 8, 7, 255]).unwrap());
        let (model_scenes, mesh) = cube_scenes(layer);

        // The model's tab (1) is parked while the texture's tab (2) is shown.
        let mut slots = vec![
            Slot { id: 1, parked: Some(parked(model_scenes.clone())), untitled: 1, name: None, revision: 0 },
            Slot { id: 2, parked: None, untitled: 2, name: None, revision: 0 },
        ];
        let mut app_scenes = HashMap::new();
        assert_eq!(push_texture(&mut slots, 2, &mut app_scenes, 1, layer, 0, &mesh, &texture), Pushed::Parked);
        let session = slots[0].parked.as_ref().unwrap();
        assert!(texture_of(&session.scenes, layer).is_some_and(|t| Arc::ptr_eq(&t, &texture)));
        assert!(session.dirty);
        assert_eq!(slots[0].revision, 1);

        // The model's tab is the active one: its scenes are the app's, its slot holds nothing.
        let mut slots = vec![
            Slot { id: 1, parked: None, untitled: 1, name: None, revision: 0 },
            Slot { id: 2, parked: Some(parked(HashMap::new())), untitled: 2, name: None, revision: 0 },
        ];
        let mut app_scenes = model_scenes;
        assert_eq!(push_texture(&mut slots, 1, &mut app_scenes, 1, layer, 0, &mesh, &texture), Pushed::Active);
        assert!(texture_of(&app_scenes, layer).is_some_and(|t| Arc::ptr_eq(&t, &texture)));
        assert_eq!(slots[0].revision, 0);

        // A model whose mesh was replaced since the link was made isn't painted.
        let other = Arc::new(Mesh::cube());
        let (mut app_scenes, _) = cube_scenes(layer);
        assert_eq!(push_texture(&mut slots, 1, &mut app_scenes, 1, layer, 0, &other, &texture), Pushed::Nowhere);
        assert!(texture_of(&app_scenes, layer).is_none());
    }

    #[test]
    fn texture_documents_round_trip() {
        let t = Texture::from_rgba(2, 2, vec![255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 0, 0, 10, 20, 30, 255]).unwrap();
        let mut doc = Document::from_parts(2, 2, Vec::new(), LayerId(0), 1);
        let id = doc.alloc_id();
        doc.layers.push(Layer::raster_with(id, "Texture", texture_tiles(&t)));
        let back = document_texture(&doc).unwrap();
        for (a, b) in t.rgba.iter().zip(&back.rgba) {
            assert!(a.abs_diff(*b) <= 2, "{:?} vs {:?}", t.rgba, back.rgba);
        }
    }
}
