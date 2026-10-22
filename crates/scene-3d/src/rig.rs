//! Rigged models: a skeleton of bones and, for each vertex of the mesh, up
//! to four bones it follows (linear blend skinning, as glTF and FBX describe
//! it). A pose is one extra rotation per bone, in the bone's own frame on
//! top of its rest rotation; the mesh is recomputed on the CPU when it
//! changes (a few milliseconds for a 100 000-vertex character).
//!
//! Spaces: a vertex in skin space `v` lands at
//! `base · Σ wᵢ · Gᵢ · inverse_bindᵢ · v − centre` in the object's own
//! space, where `Gᵢ` is bone i's global transform in the file's space
//! (parents' transforms included) and `base` turns the file's axes and
//! unit into the scene's.

use crate::math::{vec3, Mat4, Quat, Vec3};
use crate::mesh::{Mesh, Vertex};

#[derive(Debug, Clone, PartialEq)]
pub struct Bone {
    pub name: String,
    pub parent: Option<usize>,
    /// Rest pose, relative to the parent (or to `base` for a root).
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
    /// Skin space to the bone's space at bind time.
    pub inverse_bind: Mat4,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rig {
    /// Parents before their children.
    pub bones: Vec<Bone>,
    pub base: Mat4,
    /// Subtracted after skinning: the object sits at its centre.
    pub centre: Vec3,
    /// Per mesh vertex, in skin space.
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
}

/// A rig and the pose it's in (one rotation per bone; identity = rest).
#[derive(Debug, Clone, PartialEq)]
pub struct Posed {
    pub rig: std::sync::Arc<Rig>,
    pub pose: Vec<Quat>,
}

impl Posed {
    pub fn new(rig: std::sync::Arc<Rig>) -> Self {
        let pose = vec![Quat::IDENTITY; rig.bones.len()];
        Self { rig, pose }
    }

    pub fn is_rest(&self) -> bool {
        self.pose.iter().all(|q| *q == Quat::IDENTITY)
    }
}

/// Translation, rotation and scale of an affine matrix (no shear).
pub fn decompose(m: &Mat4) -> (Vec3, Quat, Vec3) {
    let c = m.0;
    let t = vec3(c[3][0], c[3][1], c[3][2]);
    let mut axes = [0, 1, 2].map(|i| vec3(c[i][0], c[i][1], c[i][2]));
    let mut s = vec3(axes[0].length(), axes[1].length(), axes[2].length());
    // A mirrored matrix: flip one axis so the rest is a rotation.
    if axes[0].cross(axes[1]).dot(axes[2]) < 0.0 {
        s.x = -s.x;
    }
    for (i, k) in [s.x, s.y, s.z].into_iter().enumerate() {
        axes[i] = axes[i] * (1.0 / if k.abs() < 1e-12 { 1.0 } else { k });
    }
    (t, crate::camera::quat_from_axes(axes[0], axes[1], axes[2]), s)
}

/// Sorts bones so parents come first; returns the old → new index map.
pub(crate) fn parents_first(bones: &mut Vec<Bone>) -> Vec<usize> {
    let n = bones.len();
    let mut order = Vec::with_capacity(n);
    let mut placed = vec![false; n];
    while order.len() < n {
        let before = order.len();
        for i in 0..n {
            if !placed[i] && bones[i].parent.is_none_or(|p| placed[p]) {
                placed[i] = true;
                order.push(i);
            }
        }
        if order.len() == before {
            // A cycle (a broken file): the rest become roots.
            for (i, done) in placed.iter_mut().enumerate() {
                if !*done {
                    *done = true;
                    bones[i].parent = None;
                    order.push(i);
                }
            }
        }
    }
    let mut new_index = vec![0; n];
    for (new, &old) in order.iter().enumerate() {
        new_index[old] = new;
    }
    let mut sorted: Vec<Bone> = order.iter().map(|&i| bones[i].clone()).collect();
    for b in &mut sorted {
        b.parent = b.parent.map(|p| new_index[p]);
    }
    *bones = sorted;
    new_index
}

impl Rig {
    /// Each bone's global transform in the file's space under `pose`.
    pub fn globals(&self, pose: &[Quat]) -> Vec<Mat4> {
        let mut out: Vec<Mat4> = Vec::with_capacity(self.bones.len());
        for (i, b) in self.bones.iter().enumerate() {
            let turn = pose.get(i).copied().unwrap_or(Quat::IDENTITY);
            let local = Mat4::from_trs(b.translation, (b.rotation * turn).normalize(), b.scale);
            out.push(match b.parent {
                Some(p) if p < i => out[p] * local,
                _ => local,
            });
        }
        out
    }

    /// Where each bone's origin (its joint) is, in the object's space.
    pub fn joints_in_object(&self, pose: &[Quat]) -> Vec<Vec3> {
        self.globals(pose).iter().map(|g| self.base.transform_point(g.transform_point(Vec3::ZERO)) - self.centre).collect()
    }

    /// The mesh in `pose`: `mesh`'s triangles and UVs, skinned positions and normals.
    pub fn deform(&self, mesh: &Mesh, pose: &[Quat]) -> Mesh {
        let skin: Vec<Mat4> = self.globals(pose).iter().zip(&self.bones).map(|(g, b)| self.base * *g * b.inverse_bind).collect();
        let vertices: Vec<Vertex> = (0..self.positions.len())
            .map(|k| {
                let p = Vec3::from_array(self.positions[k]);
                let n = Vec3::from_array(self.normals.get(k).copied().unwrap_or([0.0, 0.0, 1.0]));
                let (mut pos, mut nor, mut total) = (Vec3::ZERO, Vec3::ZERO, 0.0);
                for j in 0..4 {
                    let w = self.weights[k][j];
                    let Some(m) = skin.get(self.joints[k][j] as usize).filter(|_| w > 0.0) else { continue };
                    pos = pos + m.transform_point(p) * w;
                    nor = nor + m.transform_vector(n) * w;
                    total += w;
                }
                // Unweighted vertices follow the base only.
                if total <= 0.0 {
                    pos = self.base.transform_point(p);
                    nor = self.base.transform_vector(n);
                }
                Vertex {
                    position: (pos - self.centre).to_array(),
                    normal: nor.normalize().to_array(),
                    uv: mesh.vertices.get(k).map_or([0.0; 2], |v| v.uv),
                }
            })
            .collect();
        Mesh::from_buffers(&mesh.name, vertices, mesh.indices.clone())
    }
}

/// Up to four strongest (bone, weight) pairs per vertex, normalised.
pub(crate) fn strongest_four(influences: &[(u16, f32)]) -> ([u16; 4], [f32; 4]) {
    let mut v: Vec<(u16, f32)> = influences.iter().copied().filter(|(_, w)| *w > 0.0).collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v.truncate(4);
    let total: f32 = v.iter().map(|x| x.1).sum();
    let (mut joints, mut weights) = ([0u16; 4], [0f32; 4]);
    for (i, (j, w)) in v.into_iter().enumerate() {
        joints[i] = j;
        weights[i] = if total > 0.0 { w / total } else { 0.0 };
    }
    (joints, weights)
}

/// Smooth normals (area-weighted) for skin-space positions.
pub(crate) fn smooth_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut acc = vec![Vec3::ZERO; positions.len()];
    for t in indices.as_chunks::<3>().0 {
        let [a, b, c] = t.map(|i| Vec3::from_array(positions[i as usize]));
        let n = (b - a).cross(c - a);
        for &i in t {
            acc[i as usize] = acc[i as usize] + n;
        }
    }
    acc.into_iter().map(|n| if n.length() > 1e-12 { n.normalize().to_array() } else { [0.0, 0.0, 1.0] }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two bones up +Z, a bar of vertices along them.
    fn arm() -> (Rig, Mesh) {
        let bones = vec![
            Bone {
                name: "upper".into(),
                parent: None,
                translation: Vec3::ZERO,
                rotation: Quat::IDENTITY,
                scale: vec3(1.0, 1.0, 1.0),
                inverse_bind: Mat4::IDENTITY,
            },
            Bone {
                name: "lower".into(),
                parent: Some(0),
                translation: vec3(0.0, 0.0, 1.0),
                rotation: Quat::IDENTITY,
                scale: vec3(1.0, 1.0, 1.0),
                inverse_bind: Mat4::from_trs(vec3(0.0, 0.0, -1.0), Quat::IDENTITY, vec3(1.0, 1.0, 1.0)),
            },
        ];
        let positions = vec![[0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, 2.0]];
        let rig = Rig {
            bones,
            base: Mat4::IDENTITY,
            centre: Vec3::ZERO,
            normals: vec![[1.0, 0.0, 0.0]; 3],
            joints: vec![[0, 0, 0, 0], [0, 1, 0, 0], [1, 0, 0, 0]],
            weights: vec![[1.0, 0.0, 0.0, 0.0], [0.5, 0.5, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]],
            positions,
        };
        let mesh =
            Mesh::from_buffers("arm", vec![Vertex { position: [0.0; 3], normal: [1.0, 0.0, 0.0], uv: [0.0; 2] }; 3], vec![0, 1, 2]);
        (rig, mesh)
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        (Vec3::from_array(a) - Vec3::from_array(b)).length() < 1e-4
    }

    #[test]
    fn bending_the_elbow_moves_the_hand() {
        let (rig, mesh) = arm();
        // At rest the mesh is as bound.
        let rest = rig.deform(&mesh, &[Quat::IDENTITY; 2]);
        assert!(close(rest.vertices[2].position, [0.0, 0.0, 2.0]));
        // 90° about X at the elbow: the hand swings from +Z to −Y (right-handed).
        let pose = [Quat::IDENTITY, Quat::from_axis_angle(Vec3::X, std::f32::consts::FRAC_PI_2)];
        let bent = rig.deform(&mesh, &pose);
        assert!(close(bent.vertices[0].position, [0.0, 0.0, 0.0]), "the shoulder stays");
        assert!(close(bent.vertices[2].position, [0.0, -1.0, 1.0]), "{:?}", bent.vertices[2].position);
        // The elbow vertex is half one bone, half the other: still at the joint.
        assert!(close(bent.vertices[1].position, [0.0, 0.0, 1.0]));
        assert!(close(rig.joints_in_object(&pose)[1].to_array(), [0.0, 0.0, 1.0]));
    }

    #[test]
    fn matrices_decompose_and_bones_sort() {
        let q = Quat::from_axis_angle(vec3(1.0, 2.0, 3.0).normalize(), 0.7);
        let m = Mat4::from_trs(vec3(1.0, 2.0, 3.0), q, vec3(2.0, 2.0, 2.0));
        let (t, r, s) = decompose(&m);
        assert!((t - vec3(1.0, 2.0, 3.0)).length() < 1e-5 && (s - vec3(2.0, 2.0, 2.0)).length() < 1e-5);
        let back = Mat4::from_trs(t, r, s);
        for c in 0..4 {
            for k in 0..4 {
                assert!((back.0[c][k] - m.0[c][k]).abs() < 1e-4);
            }
        }
        let mut bones = arm().0.bones;
        bones.swap(0, 1);
        bones[0].parent = Some(1);
        bones[1].parent = None;
        let map = parents_first(&mut bones);
        assert_eq!(bones[0].name, "upper");
        assert_eq!(bones[1].parent, Some(0));
        assert_eq!(map, vec![1, 0]);
        assert_eq!(strongest_four(&[(3, 2.0), (1, 0.0), (2, 6.0)]), ([2, 3, 0, 0], [0.75, 0.25, 0.0, 0.0]));
    }
}
