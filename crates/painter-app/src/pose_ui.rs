//! Posing rigged models (FBX, glTF / GLB with a skeleton) on a 3D layer.
//!
//! With the 3D tool and a rigged object selected, its bones show over it:
//! click a joint to pick that bone, drag to turn it around the line of sight
//! (as Blender's R does). The 3D panel's Pose section lists the bones and
//! sets a bone's turn exactly (X, Y, Z in the bone's own axes), resets a bone
//! or the whole pose. Every change is one undo step; the pose is saved with
//! the project.

use std::sync::Arc;

use eframe::egui::{self, Color32, Pos2, Rect, Stroke};
use painter_core::SelectionOp;
use scene_3d::math::{Quat, Vec3};
use scene_3d::rig::{decompose, Posed};
use scene_3d::scene::SceneObject;

use crate::app::PainterApp;
use crate::tools::ToolDrag;

/// A joint on the canvas (canvas pixels; `None` behind the camera) and its bone's parent.
type Joint = (Option<Pos2>, Option<usize>);

/// Joints within this many screen points of a click pick their bone.
const PICK: f32 = 10.0;

/// A bone being turned on the canvas.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PoseDrag {
    object: usize,
    bone: usize,
    /// The joint, in canvas pixels.
    centre: Pos2,
    start_angle: f32,
    start: Quat,
}

/// Recomputes the object's mesh for its pose.
pub(crate) fn apply_pose(o: &mut SceneObject) {
    if let Some(p) = &o.rig {
        o.mesh = Arc::new(p.rig.deform(&o.mesh, &p.pose));
    }
}

/// Bone `bone`'s parent frame rotation in the scene, under the current pose.
fn parent_rotation(o: &SceneObject, p: &Posed, bone: usize) -> Quat {
    let globals = p.rig.globals(&p.pose);
    let parent = p.rig.bones[bone].parent.map_or(scene_3d::math::Mat4::IDENTITY, |i| globals[i]);
    decompose(&(o.transform.matrix() * p.rig.base * parent)).1
}

/// The pose rotation that turns `bone` by `angle` around world `axis` on top of `start`.
fn turned(o: &SceneObject, p: &Posed, bone: usize, axis: Vec3, angle: f32, start: Quat) -> Quat {
    let frame = parent_rotation(o, p, bone);
    let local_axis = frame.conjugate().rotate(axis);
    let rest = p.rig.bones[bone].rotation;
    (rest.conjugate() * Quat::from_axis_angle(local_axis, angle) * rest * start).normalize()
}

/// Euler XYZ in degrees for the panel, and back.
fn degrees(q: Quat) -> [f32; 3] {
    q.to_euler_xyz().map(f32::to_degrees)
}

impl PainterApp {
    /// Joints of the selected rigged object on the canvas (canvas pixels), with their bones' parents.
    fn joints_on_canvas(&self) -> Option<(usize, Vec<Joint>)> {
        let layer = self.scenes.get(&self.doc.active)?;
        let i = layer.scene.selected?;
        let o = layer.scene.objects.get(i)?;
        let p = o.rig.as_ref()?;
        let (w, h) = self.doc.canvas();
        let m = o.transform.matrix();
        let cam = &layer.scene.camera;
        let joints = p
            .rig
            .joints_in_object(&p.pose)
            .into_iter()
            .zip(&p.rig.bones)
            .map(|(j, b)| (cam.to_screen(m.transform_point(j), w as f32, h as f32).map(|[x, y]| Pos2::new(x, y)), b.parent))
            .collect();
        Some((i, joints))
    }

    /// A press with the 3D tool: on a joint of the selected rigged object, picks its
    /// bone and starts turning it. Returns whether it did.
    pub(crate) fn pose_press(&mut self, at: Pos2) -> bool {
        let Some((object, joints)) = self.joints_on_canvas() else { return false };
        let reach = PICK / self.view.zoom.max(1e-3);
        let hit = joints
            .iter()
            .enumerate()
            .filter_map(|(k, (p, _))| p.map(|p| (k, p, p.distance(at))))
            .filter(|x| x.2 <= reach)
            .min_by(|a, b| a.2.total_cmp(&b.2));
        let Some((bone, centre, _)) = hit else { return false };
        let start = self
            .scenes
            .get(&self.doc.active)
            .and_then(|l| l.scene.objects.get(object))
            .and_then(|o| o.rig.as_ref())
            .map_or(Quat::IDENTITY, |p| p.pose[bone]);
        self.pose_bone = Some(bone);
        self.checkpoint();
        let d = at - centre;
        self.pose_drag = Some(PoseDrag { object, bone, centre, start_angle: d.y.atan2(d.x), start });
        self.drag = Some(ToolDrag { start: at, current: at, points: Vec::new(), op: SelectionOp::Replace });
        true
    }

    /// Turning a bone: the angle swept around its joint on screen.
    pub(crate) fn pose_move(&mut self, at: Pos2) -> bool {
        let Some(drag) = self.pose_drag else { return false };
        let id = self.doc.active;
        let Some(layer) = self.scenes.get_mut(&id) else { return true };
        let axis = layer.scene.camera.forward();
        let d = at - drag.centre;
        if d.length() < 2.0 {
            return true;
        }
        let angle = d.y.atan2(d.x) - drag.start_angle;
        let Some(o) = layer.scene.objects.get_mut(drag.object) else { return true };
        let Some(p) = o.rig.as_ref() else { return true };
        let q = turned(o, p, drag.bone, axis, angle, drag.start);
        if let Some(p) = &mut o.rig {
            p.pose[drag.bone] = q;
        }
        apply_pose(o);
        self.scene_changed(id);
        true
    }

    pub(crate) fn pose_release(&mut self) -> bool {
        self.pose_drag.take().is_some()
    }

    /// The bones over the selected rigged object.
    pub(crate) fn draw_bones(&self, painter: &egui::Painter, rect: Rect) {
        let Some((_, joints)) = self.joints_on_canvas() else { return };
        let doc = self.doc_size();
        let screen = |p: Pos2| self.view.doc_to_screen(rect, doc, p);
        let bone_color = Color32::from_rgb(120, 200, 255);
        for (p, parent) in &joints {
            if let (Some(p), Some(Some(q))) = (p, parent.map(|i| joints[i].0)) {
                painter.line_segment([screen(q), screen(*p)], Stroke::new(3.0_f32, Color32::from_black_alpha(140)));
                painter.line_segment([screen(q), screen(*p)], Stroke::new(1.5_f32, bone_color));
            }
        }
        for (k, (p, _)) in joints.iter().enumerate() {
            let Some(p) = p else { continue };
            let selected = self.pose_bone == Some(k);
            let r = if selected { 6.0 } else { 4.0 };
            painter.circle_filled(screen(*p), r, if selected { crate::theme::accent() } else { Color32::WHITE });
            painter.circle_stroke(screen(*p), r, Stroke::new(1.5_f32, bone_color));
        }
    }
}

/// The 3D panel's Pose section for a rigged object. Returns (changed, needs an undo step).
pub(crate) fn pose_panel(ui: &mut egui::Ui, o: &mut SceneObject, bone: &mut Option<usize>, filter: &mut String) -> (bool, bool) {
    let Some(p) = &mut o.rig else { return (false, false) };
    let (mut changed, mut checkpoint) = (false, false);
    let n = p.rig.bones.len();
    egui::CollapsingHeader::new(format!("Pose  ({n} bones)")).id_salt("scene_pose").default_open(true).show(ui, |ui| {
        ui.small("Click a joint on the canvas and drag to turn its bone; or pick it here.");
        ui.add(egui::TextEdit::singleline(filter).hint_text("Find a bone").desired_width(f32::INFINITY));
        // Depth of each bone, for indenting.
        let mut depth = vec![0usize; n];
        for i in 0..n {
            if let Some(par) = p.rig.bones[i].parent {
                depth[i] = depth[par] + 1;
            }
        }
        egui::ScrollArea::vertical().id_salt("pose_bones").max_height(170.0).show(ui, |ui| {
            let needle = filter.to_lowercase();
            for (i, b) in p.rig.bones.iter().enumerate() {
                // Mixamo and Blender prefixes add nothing to read.
                let short = b.name.rsplit(':').next().unwrap_or(&b.name);
                if !needle.is_empty() && !b.name.to_lowercase().contains(&needle) {
                    continue;
                }
                let posed = p.pose[i] != Quat::IDENTITY;
                let label = format!("{}{}{}", "  ".repeat(depth[i].min(12)), short, if posed { "  •" } else { "" });
                if ui.selectable_label(*bone == Some(i), label).clicked() {
                    *bone = Some(i);
                }
            }
        });
        if let Some(i) = bone.filter(|&i| i < n) {
            ui.separator();
            ui.label(egui::RichText::new(&p.rig.bones[i].name).strong());
            let mut angles = degrees(p.pose[i]);
            let before = angles;
            ui.horizontal(|ui| {
                for (axis, v) in ["X", "Y", "Z"].iter().zip(angles.iter_mut()) {
                    ui.label(*axis);
                    let r = ui.add(egui::DragValue::new(v).speed(0.5).range(-180.0..=180.0).suffix("°").max_decimals(1));
                    if r.drag_started() || (r.changed() && !r.dragged()) {
                        checkpoint = true;
                    }
                }
            });
            if angles != before {
                p.pose[i] = Quat::from_euler_xyz(angles.map(f32::to_radians));
                changed = true;
            }
            ui.horizontal(|ui| {
                if ui.button("Reset bone").clicked() {
                    p.pose[i] = Quat::IDENTITY;
                    (changed, checkpoint) = (true, true);
                }
                if ui.button("Reset pose").on_hover_text("Every bone back to the model's rest pose").clicked() {
                    p.pose.iter_mut().for_each(|q| *q = Quat::IDENTITY);
                    (changed, checkpoint) = (true, true);
                }
            });
        }
    });
    if changed {
        apply_pose(o);
    }
    (changed, checkpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use scene_3d::gizmo::Transform;
    use scene_3d::math::{vec3, Mat4};
    use scene_3d::mesh::{Mesh, Vertex};
    use scene_3d::rig::{Bone, Rig};

    #[test]
    fn dragging_turns_the_bone_about_the_line_of_sight() {
        // One bone along +Z whose rest is turned 90° about Z, inside an object turned too:
        // the pose must still turn it about the world axis asked for.
        let rest = Quat::from_axis_angle(Vec3::Z, std::f32::consts::FRAC_PI_2);
        let rig = Arc::new(Rig {
            bones: vec![Bone {
                name: "b".into(),
                parent: None,
                translation: Vec3::ZERO,
                rotation: rest,
                scale: vec3(1.0, 1.0, 1.0),
                inverse_bind: Mat4::from_quat(rest.conjugate()),
            }],
            base: Mat4::IDENTITY,
            centre: Vec3::ZERO,
            positions: vec![[0.0, 0.0, 1.0]],
            normals: vec![[0.0, 0.0, 1.0]],
            joints: vec![[0; 4]],
            weights: vec![[1.0, 0.0, 0.0, 0.0]],
        });
        let mesh =
            Mesh::from_buffers("m", vec![Vertex { position: [0.0, 0.0, 1.0], normal: [0.0, 0.0, 1.0], uv: [0.0; 2] }], vec![0, 0, 0]);
        let mut o = SceneObject {
            name: "o".into(),
            mesh: Arc::new(mesh),
            transform: Transform { rotation: Quat::from_axis_angle(Vec3::X, 0.4), ..Transform::default() },
            visible: true,
            color: [1.0; 3],
            texture: None,
            rig: Some(Posed::new(rig)),
        };
        // Turn 90° about world +Y.
        let p = o.rig.clone().unwrap();
        let q = turned(&o, &p, 0, Vec3::Y, std::f32::consts::FRAC_PI_2, Quat::IDENTITY);
        o.rig.as_mut().unwrap().pose[0] = q;
        apply_pose(&mut o);
        let local = Vec3::from_array(o.mesh.vertices[0].position);
        let world = o.transform.matrix().transform_point(local);
        // The tip started at the object's +Z in the world, and turned 90° about world Y.
        let start = o.transform.rotation.rotate(Vec3::Z);
        let want = Quat::from_axis_angle(Vec3::Y, std::f32::consts::FRAC_PI_2).rotate(start);
        assert!((world - want).length() < 1e-4, "{world:?} vs {want:?}");
    }
}
