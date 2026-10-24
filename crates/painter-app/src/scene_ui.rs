//! 3D reference layers in the app.
//!
//! A 3D layer is a raster layer whose pixels come from a `scene_3d::Scene3D`.
//! While the app runs, the GPU renders the scene straight into the layer's
//! plane (`painter_render::ScenePreview`, depth-buffered) whenever it
//! changes; a moment after the last change the render is read back into the
//! layer's tiles, so saving, exporting and CPU compositing see it too.
//!
//! With the 3D tool on a 3D layer, navigation works like Blender: middle drag
//! orbits, Shift + middle drag pans, Ctrl + middle drag or the wheel zooms;
//! 1 / 3 / 7 (Ctrl for the opposite side) set front / right / top views,
//! 5 toggles perspective, Home or . frames everything. Left click picks an
//! object; G / R / S switch the gizmo; dragging its arrows, rings or handles
//! moves, rotates or scales the object (Ctrl snaps).

use std::collections::HashMap;
use crate::i18n::tr;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Key, Modifiers, Pos2, Rect, Stroke, Vec2};
use painter_core::rulers::{Ruler, RulerKind, RulerScope};
use painter_core::{project, Layer, LayerId, LayerKind, SelectionOp, TileStore};
use painter_render::{Plane, SceneDraw, SceneLight, ScenePreview, SceneRenderer, MAX_LIGHTS};
use scene_3d::blend_loader::{load_camera, load_objects, BlendCamera, BlendFile};
use scene_3d::camera::{OrbitStyle, Projection, ViewCamera, ViewPreset};
use scene_3d::gizmo::{Constraint, Gizmo, GizmoMode, Transform, AXIS_COLORS};
use scene_3d::math::{vec3, Vec3};
use scene_3d::mesh::load_obj;
use scene_3d::scene::{Light, LightKind, Scene3D, SceneObject};

use crate::app::PainterApp;
use crate::tools::{Tool, ToolDrag};

/// Read the render back into the layer this long after the last change.
const COMMIT_DELAY: Duration = Duration::from_millis(400);

#[derive(Clone)]
pub(crate) struct SceneLayer {
    pub(crate) scene: Scene3D,
    revision: u64,
    /// When to copy the render into the layer's pixels.
    commit_at: Option<Instant>,
    /// The camera of the .blend file the layer came from (not saved in projects).
    blend_camera: Option<BlendCamera>,
}

impl SceneLayer {
    /// Marks the scene changed while its document is parked: drawn again and
    /// copied into the layer's pixels once the document is shown.
    pub(crate) fn touch(&mut self) {
        self.commit_at = Some(Instant::now());
    }
}

/// What a canvas drag on a 3D layer does.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum SceneDrag {
    Orbit,
    Pan,
    Zoom,
    Gizmo,
}

/// A 3D layer's scene in a .painter file: `extras/scene3d-<layer id>.bin`.
const SCENE_PREFIX: &str = "scene3d-";
const SCENE_SUFFIX: &str = ".bin";

pub(crate) fn scene_entry(id: LayerId) -> String {
    format!("{SCENE_PREFIX}{}{SCENE_SUFFIX}", id.0)
}

/// Loads a model file as a scene, with the .blend file's camera if it has one.
pub(crate) fn load_model(path: &std::path::Path) -> Result<(Scene3D, Option<BlendCamera>), String> {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    match ext.as_str() {
        "obj" => {
            let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
            let name = path.file_stem().map_or("model".into(), |s| s.to_string_lossy().into_owned());
            let mut scene = Scene3D::from_mesh(load_obj(&text, &name)?);
            // The material's image, when the OBJ comes with one.
            if let (Some(o), Some(t)) = (scene.objects.first_mut(), crate::texture_paint::obj_texture(path, &text)) {
                o.texture = Some(std::sync::Arc::new(t));
            }
            Ok((scene, None))
        }
        "blend" => {
            let file = BlendFile::open(path).map_err(|e| e.to_string())?;
            let scene = Scene3D::from_blend(load_objects(&file).map_err(|e| e.to_string())?);
            if scene.objects.is_empty() {
                return Err("no mesh objects in the file".into());
            }
            Ok((scene, load_camera(&file)))
        }
        "gltf" | "glb" => Ok((scene_from_imported(scene_3d::gltf::load(path)?), None)),
        "fbx" => Ok((scene_from_imported(scene_3d::fbx::load(path)?), None)),
        _ => Err("unknown model format".into()),
    }
}

/// The model files a 3D layer opens.
pub(crate) const MODEL_EXTENSIONS: [&str; 5] = ["blend", "obj", "gltf", "glb", "fbx"];

/// Eye height for a camera made from a perspective ruler: a standing person's,
/// in metres, so models at their real size look right in an eye-level drawing.
const EYE_HEIGHT: f32 = 1.6;

/// glTF / FBX objects as a scene, their images as textures.
fn scene_from_imported(objects: Vec<scene_3d::import::ImportedObject>) -> Scene3D {
    let mut scene = Scene3D::default();
    for o in objects {
        let texture = o.image.as_deref().and_then(|bytes| image::load_from_memory(bytes).ok()).and_then(|i| {
            let i = i.to_rgba8();
            scene_3d::texture::Texture::from_rgba(i.width(), i.height(), i.into_raw())
        });
        scene.objects.push(SceneObject {
            name: o.name,
            mesh: std::sync::Arc::new(o.mesh),
            transform: Transform { location: o.location, ..Transform::default() },
            visible: true,
            color: o.color,
            texture: texture.map(std::sync::Arc::new),
            rig: o.rig.map(|r| scene_3d::rig::Posed::new(std::sync::Arc::new(r))),
        });
    }
    if let Some(bounds) = scene.bounds() {
        scene.camera.frame(&bounds);
    }
    scene
}

impl PainterApp {
    /// The active layer's scene, when the active layer is a 3D layer.
    pub(crate) fn active_scene(&self) -> Option<&SceneLayer> {
        self.scenes.get(&self.scene_target())
    }

    /// The 3D layer being worked on: the active layer, or while painting on a
    /// model (the paint layer active), the model's layer.
    pub(crate) fn scene_target(&self) -> LayerId {
        match &self.model_paint {
            Some(p) if p.overlay == self.doc.active => p.scene_layer,
            _ => self.doc.active,
        }
    }

    /// The 3D tool is on and the active layer is 3D: the canvas navigates the scene.
    pub(crate) fn scene_navigation_active(&self) -> bool {
        (self.tool == Tool::Scene3D || self.scene_target() != self.doc.active) && self.active_scene().is_some()
    }

    /// Marks a scene changed: redraw now, copy into the pixels shortly.
    pub(crate) fn scene_changed(&mut self, id: LayerId) {
        self.scene_revision += 1;
        let revision = self.scene_revision;
        if let Some(layer) = self.scenes.get_mut(&id) {
            layer.revision = revision;
            layer.commit_at = Some(Instant::now() + COMMIT_DELAY);
        }
    }

    /// This frame's GPU work for every 3D layer.
    pub(crate) fn scene_previews(&self) -> Vec<ScenePreview> {
        let (w, h) = self.doc.canvas();
        self.scenes.iter().filter(|(id, _)| self.doc.find(**id).is_some()).map(|(id, layer)| preview(*id, layer, (w, h))).collect()
    }

    /// New 3D layer from a model file chosen by the user.
    pub(crate) fn new_scene_layer_dialog(&mut self) {
        if let Some(path) = crate::files::FileDialog::new()
            .add_filter("3D model", &MODEL_EXTENSIONS)
            .purpose(crate::files::Purpose::NewModelLayer)
            .pick_file()
        {
            self.new_scene_layer_from(path);
        }
    }

    /// New 3D layer from a model file.
    pub(crate) fn new_scene_layer_from(&mut self, path: std::path::PathBuf) {
        match load_model(&path) {
            Ok((mut scene, blend_camera)) => {
                self.checkpoint();
                let id = self.doc.alloc_id();
                let name = path.file_stem().map_or("3D".into(), |s| format!("3D {}", s.to_string_lossy()));
                self.doc.add_layer(Layer::raster(id, name));
                self.doc.active = id;
                let objects = scene.objects.len();
                // A rigged model (its bones show, ready to pose) or a lone object starts selected.
                scene.selected = scene.objects.iter().position(|o| o.rig.is_some()).or((objects == 1).then_some(0));
                // Seen through the file's camera, like Blender's camera view.
                if let Some(cam) = &blend_camera {
                    scene.camera = self.blend_view(&scene, cam);
                }
                self.scenes.insert(id, SceneLayer { scene, revision: 0, commit_at: None, blend_camera: blend_camera.clone() });
                self.scene_changed(id);
                self.tool = Tool::Scene3D;
                self.recomposite = true;
                let mut note = String::new();
                if let Some(cam) = &blend_camera {
                    note = format!(", through camera {}", cam.name);
                    note += &match self.set_view_ruler(id) {
                        Some(n) => format!(" with a {n}-point perspective ruler"),
                        None => " (orthographic: no perspective ruler)".into(),
                    };
                }
                self.status = format!(
                    "3D layer from {} ({objects} objects{note}). Middle drag orbits, Shift pans, wheel zooms",
                    path.display()
                );
            }
            Err(err) => self.status = format!("Couldn't load {}: {err}", path.display()),
        }
    }

    /// The view through a Blender camera on this document's canvas, orbiting the scene's centre.
    fn blend_view(&self, scene: &Scene3D, cam: &BlendCamera) -> ViewCamera {
        let (w, h) = self.doc.canvas();
        ViewCamera::from_blend(cam, w as f32, h as f32, scene.bounds().map(|b| b.center()))
    }

    /// Gives 3D layer `id` a perspective ruler matching its current view: the
    /// vanishing points of the scene's X, Y and Z axes (those that converge),
    /// for drawing on any layer. Replaces the layer's previous perspective
    /// ruler. Returns how many vanishing points it has (`None`: no ruler,
    /// e.g. an orthographic view).
    pub(crate) fn set_view_ruler(&mut self, id: LayerId) -> Option<usize> {
        let (w, h) = self.doc.canvas();
        let vanishing = self.scenes.get(&id)?.scene.camera.axis_vanishing_points(w as f32, h as f32);
        let rulers = &mut self.doc.rulers.rulers;
        let existing = rulers.iter().position(|r| r.layer == Some(id.0) && matches!(r.kind, RulerKind::Perspective { .. }));
        if vanishing.is_empty() {
            if let Some(i) = existing {
                rulers.remove(i);
            }
            return None;
        }
        let count = vanishing.len();
        // The grid the ruler had stays.
        let grid = existing.and_then(|i| match &rulers[i].kind {
            RulerKind::Perspective { grid, .. } => Some(*grid),
            _ => None,
        });
        let kind = RulerKind::Perspective { vanishing, grid: grid.unwrap_or_default() };
        match existing {
            Some(i) => rulers[i].kind = kind,
            None => rulers.push(Ruler { scope: RulerScope::AllLayers, ..Ruler::on_layer(kind, id.0) }),
        }
        self.dirty = true;
        Some(count)
    }

    /// Adds a model's objects to the active 3D layer.
    fn add_model_dialog(&mut self) {
        if !self.scenes.contains_key(&self.doc.active) {
            return;
        }
        if let Some(path) =
            crate::files::FileDialog::new().add_filter("3D model", &MODEL_EXTENSIONS).purpose(crate::files::Purpose::AddModel).pick_file()
        {
            self.add_model_from(&path);
        }
    }

    /// Adds a model file's objects to the active 3D layer.
    pub(crate) fn add_model_from(&mut self, path: &std::path::Path) {
        let id = self.doc.active;
        if !self.scenes.contains_key(&id) {
            self.status = tr("Select a 3D layer to add the model to").into();
            return;
        }
        match load_model(path) {
            Ok((added, _)) => {
                self.checkpoint();
                if let Some(layer) = self.scenes.get_mut(&id) {
                    layer.scene.objects.extend(added.objects);
                    layer.scene.selected = Some(layer.scene.objects.len() - 1);
                }
                self.scene_changed(id);
            }
            Err(err) => self.status = format!("Couldn't load {}: {err}", path.display()),
        }
    }

    /// The vanishing points of the perspective ruler for the active layer
    /// (its own, or one for all layers), else of any perspective ruler.
    pub(crate) fn perspective_ruler(&self) -> Option<Vec<[f32; 2]>> {
        let active = self.doc.active.0;
        let rulers = &self.doc.rulers.rulers;
        let perspective = |r: &&Ruler| matches!(r.kind, RulerKind::Perspective { .. });
        let applies = |r: &&Ruler| r.visible && (r.layer.is_none() || r.layer == Some(active) || matches!(r.scope, RulerScope::AllLayers));
        let ruler = rulers.iter().filter(perspective).find(applies).or_else(|| rulers.iter().find(perspective))?;
        match &ruler.kind {
            RulerKind::Perspective { vanishing, .. } => Some(vanishing.clone()),
            _ => None,
        }
    }

    /// An empty 3D layer seen through the camera the perspective ruler
    /// implies (no undo step: callers make it).
    fn ruler_scene_layer(&mut self) -> Result<LayerId, String> {
        let points = self.perspective_ruler().ok_or("add a perspective ruler first")?;
        let (w, h) = self.doc.canvas();
        let camera = ViewCamera::from_vanishing_points(&points, w as f32, h as f32, EYE_HEIGHT)
            .ok_or("these vanishing points can't belong to one camera (two of them should sit on opposite sides of the picture)")?;
        let id = self.doc.alloc_id();
        self.doc.add_layer(Layer::raster(id, "3D Perspective".to_string()));
        self.doc.active = id;
        let scene = Scene3D { camera, ..Scene3D::default() };
        self.scenes.insert(id, SceneLayer { scene, revision: 0, commit_at: None, blend_camera: None });
        self.scene_changed(id);
        self.recomposite = true;
        Ok(id)
    }

    /// Layer → New 3D Layer from Perspective Ruler: the drawing's perspective
    /// as a 3D space; models dropped on the canvas stand on its ground.
    pub(crate) fn new_scene_layer_from_ruler(&mut self) {
        self.checkpoint();
        match self.ruler_scene_layer() {
            Ok(_) => {
                let n = self.perspective_ruler().map_or(0, |p| p.len());
                self.status = format!(
                    "3D layer in the {n}-point perspective (eye {EYE_HEIGHT} m above the ground). Drop .fbx, .glb, .obj or .blend models on the canvas to place them"
                );
            }
            Err(err) => self.status = format!("No 3D layer: {err}"),
        }
    }

    /// A model file dropped at canvas point `at`: added to the active 3D
    /// layer, or to a new one in the perspective ruler's space, standing on
    /// the ground under the pointer. With neither, it opens as a new 3D layer.
    pub(crate) fn drop_model(&mut self, path: std::path::PathBuf, at: Option<Pos2>) {
        let into_active = self.scenes.contains_key(&self.doc.active);
        if !into_active && self.perspective_ruler().is_none() {
            self.new_scene_layer_from(path);
            return;
        }
        let mut added = match load_model(&path) {
            Ok((scene, _)) => scene,
            Err(err) => {
                self.status = format!("Couldn't load {}: {err}", path.display());
                return;
            }
        };
        let Some(bounds) = added.bounds() else { return };
        self.checkpoint();
        let id = if into_active {
            self.doc.active
        } else {
            match self.ruler_scene_layer() {
                Ok(id) => id,
                Err(err) => {
                    self.status = format!("Couldn't place the model: {err}");
                    return;
                }
            }
        };
        let (w, h) = self.doc.canvas();
        let (w, h) = (w as f32, h as f32);
        let Some(layer) = self.scenes.get_mut(&id) else { return };
        let at = at.unwrap_or(Pos2::new(w * 0.5, h * 0.5));
        let (hit, base, scale) = ground_placement(&layer.scene.camera, &bounds, [at.x, at.y], w, h);
        for o in &mut added.objects {
            o.transform.location = hit + (o.transform.location - base) * scale;
            o.transform.scale = o.transform.scale * scale;
        }
        let count = added.objects.len();
        layer.scene.objects.extend(added.objects);
        layer.scene.selected = Some(layer.scene.objects.len() - 1);
        self.scene_changed(id);
        self.tool = Tool::Scene3D;
        let resized = if scale == 1.0 { String::new() } else { format!(", scaled ×{scale:.3} to fit") };
        self.status = format!("Placed {} ({count} objects{resized}). Drag the gizmo to move it", path.display());
    }

    /// Copies due renders into their layers' pixels. `force` does all pending ones now.
    pub(crate) fn commit_scenes(&mut self, force: bool) {
        let busy = self.scene_drag.is_some();
        let now = Instant::now();
        let due: Vec<LayerId> =
            self.scenes.iter().filter(|(_, l)| l.commit_at.is_some_and(|t| force || (!busy && t <= now))).map(|(id, _)| *id).collect();
        let Some(rs) = self.render_state.clone() else {
            return;
        };
        let size = self.doc.canvas();
        for id in due {
            let Some(layer) = self.scenes.get_mut(&id) else {
                continue;
            };
            layer.commit_at = None;
            // New pixels and a changed scene, both saved with the project.
            self.dirty = true;
            let preview = preview(id, layer, size);
            let renderer = self.scene_renderer.get_or_insert_with(|| SceneRenderer::new(&rs.device));
            let rgba = renderer.render_to_rgba(&rs.device, &rs.queue, &preview, size);
            if let Some(l) = self.doc.find_mut(id) {
                l.kind = LayerKind::Raster(TileStore::from_rgba(size.0, size.1, &rgba));
            }
            // The GPU already shows the scene; the tiles are for files and the CPU.
            self.upload_plane(Plane::Color(id));
        }
    }

    /// Asks for frames while a commit is pending.
    pub(crate) fn scene_repaint(&self, ctx: &egui::Context) {
        if self.scenes.values().any(|l| l.commit_at.is_some()) {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
    }

    /// Scenes saved in a project's extras, for layers the document has. Their
    /// pixels were saved too, so they only need drawing, not committing.
    pub(crate) fn load_scenes(&mut self, extras: &project::Extras) -> Result<(), String> {
        let mut failed = Vec::new();
        for (name, bytes) in extras {
            let Some(id) = name.strip_prefix(SCENE_PREFIX).and_then(|s| s.strip_suffix(SCENE_SUFFIX)).and_then(|s| s.parse().ok())
            else {
                continue;
            };
            let id = LayerId(id);
            if self.doc.find(id).is_none() {
                continue;
            }
            match Scene3D::from_bytes(bytes) {
                Ok(scene) => {
                    self.scene_revision += 1;
                    self.scenes.insert(id, SceneLayer { scene, revision: self.scene_revision, commit_at: None, blend_camera: None });
                }
                Err(err) => failed.push(format!("layer {}: {err}", id.0)),
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(format!("some 3D scenes couldn't be read ({})", failed.join("; ")))
        }
    }

    /// Redraws every 3D layer on the GPU (after switching documents, whose layer ids may repeat).
    pub(crate) fn refresh_scene_previews(&mut self) {
        for layer in self.scenes.values_mut() {
            self.scene_revision += 1;
            layer.revision = self.scene_revision;
        }
    }

    /// Drops scenes whose layers are gone.
    pub(crate) fn prune_scenes(&mut self) {
        let doc = &self.doc;
        self.scenes.retain(|id, _| doc.find(*id).is_some());
    }

    // ---- navigation --------------------------------------------------------------------

    /// Middle-button navigation and the wheel. Called instead of canvas
    /// panning / zooming while [`Self::scene_navigation_active`].
    pub(crate) fn scene_navigate(&mut self, ui: &egui::Ui, rect: Rect, response: &egui::Response) {
        let id = self.scene_target();
        let (delta, mods, scroll, middle) =
            ui.input(|i| (i.pointer.delta(), i.modifiers, i.raw_scroll_delta.y, i.pointer.middle_down()));
        let canvas_h = self.doc.canvas().1 as f32;
        // Pointer motion in canvas pixels (the canvas view may be zoomed).
        let per_px = 1.0 / self.view.zoom.max(1e-3);
        let Some(layer) = self.scenes.get_mut(&id) else {
            return;
        };
        let cam = &mut layer.scene.camera;
        let mut changed = false;
        if middle && response.dragged_by(egui::PointerButton::Middle) {
            let kind = if mods.shift && mods.ctrl {
                SceneDrag::Zoom
            } else if mods.shift {
                SceneDrag::Pan
            } else if mods.ctrl {
                SceneDrag::Zoom
            } else {
                SceneDrag::Orbit
            };
            match kind {
                SceneDrag::Orbit => cam.orbit(delta.x, delta.y),
                SceneDrag::Pan => cam.pan(delta.x * per_px, delta.y * per_px, canvas_h),
                _ => cam.zoom((delta.y * 0.01).exp()),
            }
            self.scene_drag = Some(kind);
            changed = delta != Vec2::ZERO;
        } else if self.scene_drag.is_some_and(|d| d != SceneDrag::Gizmo) {
            self.scene_drag = None;
        }
        // Plain wheel zooms the scene; Ctrl + wheel still zooms the canvas.
        if scroll != 0.0 && !mods.ctrl && response.hovered() && rect.is_positive() {
            cam.zoom((-scroll * 0.0015).exp());
            changed = true;
        }
        if changed {
            self.scene_changed(id);
        }
    }

    /// Keys for the 3D tool; consumed so they don't trigger other tools.
    pub(crate) fn scene_shortcuts(&mut self, ctx: &egui::Context) {
        if !self.scene_navigation_active() || self.tool != Tool::Scene3D {
            return;
        }
        let id = self.doc.active;
        let pressed = |key: Key, mods: Modifiers| ctx.input_mut(|i| i.consume_key(mods, key));
        let mut mode = None;
        for (key, m) in [(Key::G, GizmoMode::Translate), (Key::R, GizmoMode::Rotate), (Key::S, GizmoMode::Scale)] {
            if pressed(key, Modifiers::NONE) {
                mode = Some(m);
            }
        }
        if let Some(m) = mode {
            self.gizmo.mode = m;
        }
        let mut view = None;
        for (key, front, back) in [
            (Key::Num1, ViewPreset::Front, ViewPreset::Back),
            (Key::Num3, ViewPreset::Right, ViewPreset::Left),
            (Key::Num7, ViewPreset::Top, ViewPreset::Bottom),
        ] {
            if pressed(key, Modifiers::CTRL) {
                view = Some(back);
            } else if pressed(key, Modifiers::NONE) {
                view = Some(front);
            }
        }
        let toggle = pressed(Key::Num5, Modifiers::NONE);
        let frame = pressed(Key::Home, Modifiers::NONE) || pressed(Key::Period, Modifiers::NONE);
        let Some(layer) = self.scenes.get_mut(&id) else {
            return;
        };
        let mut changed = false;
        if let Some(v) = view {
            layer.scene.camera.set_view(v);
            changed = true;
        }
        if toggle {
            layer.scene.camera.toggle_projection();
            changed = true;
        }
        if frame {
            if let Some(b) = layer.scene.bounds() {
                layer.scene.camera.frame(&b);
                changed = true;
            }
        }
        if changed {
            self.scene_changed(id);
        }
    }

    // ---- picking and the gizmo ---------------------------------------------------------

    /// World units per *screen* pixel at the selected object (constant-size gizmo).
    fn gizmo_scale(&self, layer: &SceneLayer, at: Vec3) -> f32 {
        Gizmo::world_per_pixel(&layer.scene.camera, at, self.doc.canvas().1 as f32) / self.view.zoom.max(1e-3)
    }

    pub(crate) fn scene_press(&mut self, at: Pos2) {
        // A joint of the selected rigged object: pose that bone.
        if self.pose_press(at) {
            return;
        }
        let id = self.doc.active;
        let (w, h) = self.doc.canvas();
        let (w, h) = (w as f32, h as f32);
        let Some(layer) = self.scenes.get(&id) else {
            return;
        };
        let ray = layer.scene.camera.ray(at.x, at.y, w, h);
        // The gizmo of the selected object first.
        if let Some(sel) = layer.scene.selected.and_then(|i| layer.scene.objects.get(i)) {
            let scale = self.gizmo_scale(layer, sel.transform.location);
            if let Some(constraint) = self.gizmo.hit_test(&ray, &sel.transform, scale) {
                let (t, camera) = (sel.transform, layer.scene.camera.clone());
                if self.gizmo.begin(constraint, &ray, &t, &camera, [at.x, at.y]) {
                    self.checkpoint();
                    self.scene_drag = Some(SceneDrag::Gizmo);
                    self.drag = Some(ToolDrag { start: at, current: at, points: Vec::new(), op: SelectionOp::Replace });
                    return;
                }
            }
        }
        if let Some(layer) = self.scenes.get_mut(&id) {
            let before = layer.scene.selected;
            layer.scene.select_at(at.x, at.y, w, h);
            if layer.scene.selected != before {
                self.scene_changed(id);
            }
        }
    }

    pub(crate) fn scene_move(&mut self, at: Pos2) {
        if self.pose_move(at) {
            return;
        }
        if self.scene_drag != Some(SceneDrag::Gizmo) {
            return;
        }
        let id = self.doc.active;
        let (w, h) = self.doc.canvas();
        let snap = self.pointer_mods.ctrl;
        let Some(layer) = self.scenes.get_mut(&id) else {
            return;
        };
        let ray = layer.scene.camera.ray(at.x, at.y, w as f32, h as f32);
        let Some(sel) = layer.scene.selected else {
            return;
        };
        if let Some(t) = self.gizmo.update(&ray, &layer.scene.camera, [at.x, at.y], snap) {
            layer.scene.objects[sel].transform = t;
            self.scene_changed(id);
        }
    }

    pub(crate) fn scene_release(&mut self) {
        self.pose_release();
        self.gizmo.end();
        if self.scene_drag == Some(SceneDrag::Gizmo) {
            self.scene_drag = None;
        }
    }

    pub(crate) fn draw_scene_overlay(&self, painter: &egui::Painter, rect: Rect) {
        if !self.scene_navigation_active() || self.tool != Tool::Scene3D {
            return;
        }
        let Some(layer) = self.active_scene() else {
            return;
        };
        let Some(obj) = layer.scene.selected.and_then(|i| layer.scene.objects.get(i)) else {
            return;
        };
        let cam = &layer.scene.camera;
        let (w, h) = self.doc.canvas();
        let doc = self.doc_size();
        let screen = |p: Vec3| cam.to_screen(p, w as f32, h as f32).map(|[x, y]| self.view.doc_to_screen(rect, doc, Pos2::new(x, y)));
        // Selection bounds.
        let m = obj.transform.matrix();
        let (lo, hi) = (obj.mesh.bounds.min, obj.mesh.bounds.max);
        let corner = |i: usize| {
            m.transform_point(vec3(
                if i & 1 == 0 { lo.x } else { hi.x },
                if i & 2 == 0 { lo.y } else { hi.y },
                if i & 4 == 0 { lo.z } else { hi.z },
            ))
        };
        for (a, b) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)] {
            if let (Some(p), Some(q)) = (screen(corner(a)), screen(corner(b))) {
                painter.line_segment([p, q], Stroke::new(1.0_f32, Color32::from_rgb(255, 160, 40)));
            }
        }
        // The gizmo.
        let c = obj.transform.location;
        let len = self.gizmo.size_px * self.gizmo_scale(layer, c);
        let axes = self.gizmo.axes(&obj.transform);
        let active = self.gizmo.dragging();
        let color = |i: usize| {
            let [r, g, b] = AXIS_COLORS[i];
            if active == Some(Constraint::Axis(i)) {
                Color32::from_rgb(255, 230, 120)
            } else {
                Color32::from_rgb(r, g, b)
            }
        };
        match self.gizmo.mode {
            GizmoMode::Rotate => {
                for (i, axis) in axes.iter().enumerate() {
                    let u = if axis.z.abs() < 0.9 { axis.cross(Vec3::Z).normalize() } else { axis.cross(Vec3::X).normalize() };
                    let v = axis.cross(u);
                    let ring: Vec<Pos2> = (0..=64)
                        .filter_map(|k| {
                            let a = k as f32 / 64.0 * std::f32::consts::TAU;
                            screen(c + (u * a.cos() + v * a.sin()) * len)
                        })
                        .collect();
                    painter.add(egui::Shape::line(ring, Stroke::new(2.0_f32, color(i))));
                }
            }
            GizmoMode::Translate | GizmoMode::Scale => {
                for (i, axis) in axes.iter().enumerate() {
                    if let (Some(a), Some(b)) = (screen(c + *axis * (len * 0.2)), screen(c + *axis * len)) {
                        painter.line_segment([a, b], Stroke::new(2.5_f32, color(i)));
                        if self.gizmo.mode == GizmoMode::Translate {
                            painter.circle_filled(b, 5.0, color(i));
                        } else {
                            painter.rect_filled(Rect::from_center_size(b, Vec2::splat(9.0)), 1.0, color(i));
                        }
                    }
                }
            }
        }
        if let Some(p) = screen(c) {
            painter.circle_stroke(p, 6.0, Stroke::new(1.5_f32, Color32::WHITE));
        }
        // A rigged model's bones, over everything.
        self.draw_bones(painter, rect);
    }

    // ---- panel --------------------------------------------------------------------------

    pub(crate) fn scene_panel(&mut self, ui: &mut egui::Ui) {
        let id = self.scene_target();
        if !self.scenes.contains_key(&id) {
            ui.label(tr("The active layer isn't a 3D layer."));
            if ui.button(tr("New 3D layer from a model…")).on_hover_text(tr(".blend (Blender 3.4+), .obj, .fbx, .gltf or .glb")).clicked() {
                self.new_scene_layer_dialog();
            }
            if ui.button(tr("New 3D layer from the perspective ruler")).on_hover_text(tr("Turns the drawing's vanishing points into a 3D space; drop models on the canvas to place them")).clicked() {
                self.new_scene_layer_from_ruler();
            }
            return;
        }
        // The state before this frame's edits, for the undo step they make.
        let before = self.scenes.get(&id).map(|l| l.scene.clone());
        let mut changed = false;
        let mut checkpoint = false;
        let mut add_model = false;
        let mut flatten = false;
        let mut blend_view = false;
        let mut view_ruler = false;
        let blend_camera_name = self.scenes.get(&id).and_then(|l| l.blend_camera.as_ref()).map(|c| c.name.clone());
        // Records an edit; a drag counts once for undo.
        let edit = |r: &egui::Response, changed: &mut bool, checkpoint: &mut bool| {
            if r.changed() {
                *changed = true;
            }
            if r.drag_started() || (r.changed() && !r.dragged()) {
                *checkpoint = true;
            }
        };
        ui.horizontal(|ui| {
            use crate::icons::{icon_button, Icon};
            add_model = icon_button(ui, Icon::Add, 16.0, false, "Add a model to this layer (.blend or .obj)").clicked();
            if icon_button(ui, Icon::ZoomFit, 16.0, false, "Frame everything (Home or .)").clicked() {
                if let Some(layer) = self.scenes.get_mut(&id) {
                    if let Some(b) = layer.scene.bounds() {
                        layer.scene.camera.frame(&b);
                        changed = true;
                    }
                }
            }
            flatten = icon_button(ui, Icon::MergeDown, 16.0, false, "Flatten to pixels: keep the current render as a plain raster layer").clicked();
            ui.separator();
            for (mode, icon, hint) in [
                (GizmoMode::Translate, Icon::Move, "Move (G)"),
                (GizmoMode::Rotate, Icon::RotateRight, "Rotate (R)"),
                (GizmoMode::Scale, Icon::Transform, "Scale (S)"),
            ] {
                if icon_button(ui, icon, 16.0, self.gizmo.mode == mode, hint).clicked() {
                    self.gizmo.mode = mode;
                }
            }
            ui.checkbox(&mut self.gizmo.local, tr("Local")).on_hover_text(tr("Gizmo along the object's own axes"));
        });
        let Some(layer) = self.scenes.get_mut(&id) else {
            return;
        };
        let scene = &mut layer.scene;

        egui::CollapsingHeader::new(tr("Objects")).id_salt("scene_objects").default_open(true).show(ui, |ui| {
            let mut remove = None;
            for i in 0..scene.objects.len() {
                ui.horizontal(|ui| {
                    let o = &mut scene.objects[i];
                    if ui.checkbox(&mut o.visible, tr("")).changed() {
                        changed = true;
                        checkpoint = true;
                    }
                    let tris = o.mesh.indices.len() / 3;
                    if ui.selectable_label(scene.selected == Some(i), format!("{}  ({tris} tris)", o.name)).clicked() {
                        scene.selected = Some(i);
                        changed = true;
                    }
                    if crate::icons::icon_button(ui, crate::icons::Icon::Delete, 12.0, false, "Remove").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                scene.objects.remove(i);
                scene.selected = None;
                changed = true;
                checkpoint = true;
            }
        });

        if let Some(sel) = scene.selected.filter(|&i| i < scene.objects.len()) {
            egui::CollapsingHeader::new(tr("Transform")).id_salt("scene_transform").default_open(true).show(ui, |ui| {
                let o = &mut scene.objects[sel];
                let t = &mut o.transform;
                let mut row = |ui: &mut egui::Ui, label: &str, values: &mut [f32; 3], speed: f32, suffix: &str| {
                    ui.horizontal(|ui| {
                        ui.label(label);
                        for v in values.iter_mut() {
                            let r = ui.add(egui::DragValue::new(v).speed(speed).max_decimals(3).suffix(suffix));
                            edit(&r, &mut changed, &mut checkpoint);
                        }
                    });
                };
                let mut loc = t.location.to_array();
                row(ui, "Location", &mut loc, 0.01, "");
                t.location = Vec3::from_array(loc);
                let mut rot = t.euler_degrees();
                let before = rot;
                row(ui, "Rotation", &mut rot, 0.5, "°");
                if rot != before {
                    t.set_euler_degrees(rot);
                }
                let mut scale = t.scale.to_array();
                row(ui, "Scale", &mut scale, 0.01, "");
                t.scale = Vec3::from_array(scale);
                ui.horizontal(|ui| {
                    ui.label(tr("Colour"));
                    let r = ui.color_edit_button_rgb(&mut o.color);
                    edit(&r, &mut changed, &mut checkpoint);
                });
            });
            // Rigged models: their pose.
            if scene.objects[sel].rig.is_some() {
                let (c, k) = crate::pose_ui::pose_panel(
                    ui,
                    &mut scene.objects[sel],
                    &mut self.pose_bone,
                    &mut self.pose_filter,
                    &mut self.pose_ik,
                    &mut self.pose_chain,
                );
                changed |= c;
                checkpoint |= k;
            }
        }

        egui::CollapsingHeader::new(tr("Camera")).id_salt("scene_camera").default_open(true).show(ui, |ui| {
            let cam = &mut scene.camera;
            ui.horizontal(|ui| {
                let persp = matches!(cam.projection, Projection::Perspective { .. });
                if ui.selectable_label(persp, tr("Perspective")).clicked() && !persp {
                    cam.toggle_projection();
                    changed = true;
                }
                if ui.selectable_label(!persp, tr("Orthographic")).on_hover_text(tr("5")).clicked() && persp {
                    cam.toggle_projection();
                    changed = true;
                }
            });
            if let Projection::Perspective { fov_y } = &mut cam.projection {
                let mut deg = fov_y.to_degrees();
                if ui.add(egui::Slider::new(&mut deg, 10.0..=120.0).text(tr("FOV")).suffix("°")).changed() {
                    *fov_y = deg.to_radians();
                    changed = true;
                }
            }
            ui.horizontal_wrapped(|ui| {
                for (label, view, key) in [
                    ("Front", ViewPreset::Front, "1"),
                    ("Back", ViewPreset::Back, "Ctrl+1"),
                    ("Right", ViewPreset::Right, "3"),
                    ("Left", ViewPreset::Left, "Ctrl+3"),
                    ("Top", ViewPreset::Top, "7"),
                    ("Bottom", ViewPreset::Bottom, "Ctrl+7"),
                ] {
                    if ui.small_button(label).on_hover_text(key).clicked() {
                        cam.set_view(view);
                        changed = true;
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label(tr("Orbit"));
                ui.selectable_value(&mut cam.orbit_style, OrbitStyle::Turntable, tr("Turntable"));
                ui.selectable_value(&mut cam.orbit_style, OrbitStyle::Trackball, tr("Trackball"));
            });
            ui.horizontal_wrapped(|ui| {
                if let Some(name) = &blend_camera_name {
                    blend_view = ui.button(tr("Blender Camera")).on_hover_text(format!("Back to the view through camera {name}")).clicked();
                }
                view_ruler = ui
                    .button(tr("Perspective Ruler from View"))
                    .on_hover_text(tr("Set this layer's perspective ruler to the vanishing points of the scene's axes in this view"))
                    .clicked();
            });
            ui.small(tr("Middle drag orbits · Shift pans · Ctrl or wheel zooms"));
        });

        egui::CollapsingHeader::new(tr("Lights")).id_salt("scene_lights").default_open(true).show(ui, |ui| {
            let r = ui.add(egui::Slider::new(&mut scene.ambient, 0.0..=1.0).text(tr("Ambient")));
            edit(&r, &mut changed, &mut checkpoint);
            let mut remove = None;
            for (i, light) in scene.lights.iter_mut().enumerate() {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(format!("Light {}", i + 1));
                    let r = ui.color_edit_button_rgb(&mut light.color);
                    edit(&r, &mut changed, &mut checkpoint);
                    if crate::icons::icon_button(ui, crate::icons::Icon::Delete, 12.0, false, "Remove the light").clicked() {
                        remove = Some(i);
                    }
                });
                let r = ui.add(egui::Slider::new(&mut light.intensity, 0.0..=3.0).text(tr("Intensity")));
                edit(&r, &mut changed, &mut checkpoint);
                match &mut light.kind {
                    LightKind::Sun { direction } => {
                        // Direction the light comes from, as azimuth / elevation.
                        let from = -*direction;
                        let mut az = from.y.atan2(from.x).to_degrees();
                        let mut el = from.z.clamp(-1.0, 1.0).asin().to_degrees();
                        let a = ui.add(egui::Slider::new(&mut az, -180.0..=180.0).text(tr("Azimuth")).suffix("°"));
                        let e = ui.add(egui::Slider::new(&mut el, -89.0..=89.0).text(tr("Elevation")).suffix("°"));
                        if a.changed() || e.changed() {
                            let (az, el) = (az.to_radians(), el.to_radians());
                            *direction = -vec3(el.cos() * az.cos(), el.cos() * az.sin(), el.sin());
                        }
                        edit(&a, &mut changed, &mut checkpoint);
                        edit(&e, &mut changed, &mut checkpoint);
                    }
                    LightKind::Point { position, range } => {
                        ui.horizontal(|ui| {
                            ui.label(tr("Position"));
                            let mut p = position.to_array();
                            for v in &mut p {
                                let r = ui.add(egui::DragValue::new(v).speed(0.05));
                                edit(&r, &mut changed, &mut checkpoint);
                            }
                            *position = Vec3::from_array(p);
                        });
                        let r = ui.add(egui::Slider::new(range, 0.1..=100.0).text(tr("Range")));
                        edit(&r, &mut changed, &mut checkpoint);
                    }
                }
            }
            if let Some(i) = remove {
                scene.lights.remove(i);
                changed = true;
                checkpoint = true;
            }
            ui.horizontal(|ui| {
                let room = scene.lights.len() < MAX_LIGHTS;
                if ui.add_enabled(room, egui::Button::new(tr("+ Sun"))).clicked() {
                    scene.lights.push(Light {
                        kind: LightKind::Sun { direction: vec3(0.5, -0.5, -0.7).normalize() },
                        color: [1.0; 3],
                        intensity: 0.6,
                    });
                    changed = true;
                    checkpoint = true;
                }
                if ui.add_enabled(room, egui::Button::new(tr("+ Point"))).clicked() {
                    let at = scene.camera.eye();
                    scene.lights.push(Light { kind: LightKind::Point { position: at, range: 20.0 }, color: [1.0; 3], intensity: 0.8 });
                    changed = true;
                    checkpoint = true;
                }
            });
        });

        if checkpoint {
            // The edit is already applied: record the state before it.
            let mut scenes = snapshot(&self.scenes);
            if let Some(before) = before {
                scenes.insert(id, before);
            }
            self.history.push(&self.doc);
            self.dirty = true;
            self.push_side_history(self.comic.clone(), scenes);
        }
        if changed {
            self.scene_changed(id);
        }
        if add_model {
            self.add_model_dialog();
        }
        if blend_view {
            if let Some(layer) = self.scenes.get(&id) {
                if let Some(cam) = layer.blend_camera.clone() {
                    let view = self.blend_view(&layer.scene, &cam);
                    self.checkpoint();
                    if let Some(layer) = self.scenes.get_mut(&id) {
                        layer.scene.camera = view;
                    }
                    self.scene_changed(id);
                }
            }
        }
        if view_ruler {
            self.checkpoint();
            self.status = match self.set_view_ruler(id) {
                Some(n) => format!("Perspective ruler with {n} vanishing points from this view"),
                None => "No perspective ruler: the axes stay parallel in this view (orthographic?)".into(),
            };
        }
        if flatten {
            self.commit_scenes(true);
            self.scenes.remove(&id);
            self.recomposite = true;
            self.status = tr("3D layer flattened to pixels").into();
        }
        self.texture_panel(ui, id);
    }
}

/// The GPU description of one 3D layer.
fn preview(id: LayerId, layer: &SceneLayer, (w, h): (u32, u32)) -> ScenePreview {
    let scene = &layer.scene;
    ScenePreview {
        layer: id,
        revision: layer.revision,
        view_proj: scene.camera.view_projection(w as f32 / h.max(1) as f32).0,
        draws: scene
            .objects
            .iter()
            .enumerate()
            .filter(|(_, o)| o.visible)
            .map(|(i, o)| SceneDraw {
                mesh: o.mesh.clone(),
                model: o.transform.matrix().0,
                color: o.color,
                selected: scene.selected == Some(i),
                texture: o.texture.clone(),
            })
            .collect(),
        lights: scene
            .lights
            .iter()
            .map(|l| {
                let color = l.color.map(|c| c * l.intensity);
                match l.kind {
                    LightKind::Sun { direction } => SceneLight::Sun { direction: direction.to_array(), color },
                    LightKind::Point { position, .. } => SceneLight::Point { position: position.to_array(), color },
                }
            })
            .collect(),
        ambient: scene.ambient,
    }
}

/// Snapshots of every scene, kept next to the undo history.
pub(crate) type SceneSnapshot = HashMap<LayerId, Scene3D>;

pub(crate) fn snapshot(scenes: &HashMap<LayerId, SceneLayer>) -> SceneSnapshot {
    scenes.iter().map(|(id, l)| (*id, l.scene.clone())).collect()
}

/// Restores scenes from a snapshot, keeping pending state for redraws.
pub(crate) fn restore(scenes: &mut HashMap<LayerId, SceneLayer>, snapshot: SceneSnapshot) {
    scenes.clear();
    for (id, scene) in snapshot {
        scenes.insert(id, SceneLayer { scene, revision: 0, commit_at: None, blend_camera: None });
    }
}

/// Where a model with world `bounds` lands when dropped at canvas point `at`:
/// the ground (z = 0) under the pointer (ahead of the camera when the pointer
/// is above the horizon), the model's bottom centre that goes there, and a
/// scale for models far too big or too small for that spot (else 1).
fn ground_placement(camera: &ViewCamera, bounds: &scene_3d::mesh::Aabb, at: [f32; 2], w: f32, h: f32) -> (Vec3, Vec3, f32) {
    let ray = camera.ray(at[0], at[1], w, h);
    let hit = if ray.dir.z < -1e-4 { ray.at(-ray.origin.z / ray.dir.z) } else { vec3(camera.pivot.x, camera.pivot.y, 0.0) };
    let size = bounds.max - bounds.min;
    let extent = size.x.max(size.y).max(size.z).max(1e-6);
    let distance = (hit - camera.eye()).length().max(0.1);
    let scale = if extent > 2.0 * distance || extent < 0.02 * distance { 0.4 * distance / extent } else { 1.0 };
    let centre = bounds.center();
    (hit, vec3(centre.x, centre.y, bounds.min.z), scale)
}

#[cfg(test)]
mod placement_tests {
    use super::*;
    use scene_3d::mesh::Aabb;

    #[test]
    fn dropped_models_stand_under_the_pointer() {
        let (w, h) = (1600.0, 1000.0);
        // A 2-point perspective drawn on the canvas: horizon at 40% of the height.
        let camera = ViewCamera::from_vanishing_points(&[[-300.0, 400.0], [2100.0, 400.0]], w, h, EYE_HEIGHT).unwrap();
        // A person-sized model 10 cm off the ground.
        let person = Aabb { min: vec3(-0.3, -0.2, 0.1), max: vec3(0.3, 0.2, 1.9) };
        let at = [500.0, 800.0];
        let (hit, base, scale) = ground_placement(&camera, &person, at, w, h);
        assert!(hit.z.abs() < 1e-3, "on the ground: {hit:?}");
        let back = camera.to_screen(hit, w, h).unwrap();
        assert!((back[0] - at[0]).abs() < 0.5 && (back[1] - at[1]).abs() < 0.5, "under the pointer: {back:?}");
        assert_eq!(base, vec3(0.0, 0.0, 0.1));
        assert_eq!(scale, 1.0, "real size kept");
        // A 500 m model is brought down to fit.
        let tower = Aabb { min: vec3(0.0, 0.0, 0.0), max: vec3(10.0, 10.0, 500.0) };
        assert!(ground_placement(&camera, &tower, at, w, h).2 < 0.1);
        // Above the horizon: in front of the camera, still on the ground.
        let (sky, _, _) = ground_placement(&camera, &person, [800.0, 100.0], w, h);
        assert!(sky.z.abs() < 1e-6);
    }
}
