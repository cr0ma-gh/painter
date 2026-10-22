//! Painting models: an object's colour texture, and projection painting.
//!
//! A texture is a plain RGBA8 image (straight alpha) laid over the object's
//! base colour through the mesh's UVs (`v` up, so texel row 0 is `v` = 1).
//!
//! Projection painting works like painting in a 3D viewport: strokes go onto
//! a screen-sized picture (any tool, on a layer over the 3D layer), then
//! [`project`] carries that paint onto the texture. Each texel of the
//! object's triangles is placed on screen through the camera and takes the
//! paint there, when that point of the surface is the one in view (a depth
//! buffer of the whole scene hides what's behind) and faces the camera
//! enough (paint fades on surfaces seen edge-on). Texels just outside a
//! triangle's edge get paint too, so UV seams don't show cracks.

use crate::math::{Mat4, Vec3};
use crate::mesh::Mesh;
use crate::scene::Scene3D;

#[derive(Debug, Clone, PartialEq)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    /// Straight-alpha RGBA8, row 0 at the top (`v` = 1).
    pub rgba: Vec<u8>,
}

impl Texture {
    /// A transparent texture: the base colour shows until painted.
    pub fn new(width: u32, height: u32) -> Self {
        let (width, height) = (width.max(1), height.max(1));
        Self { width, height, rgba: vec![0; (width * height * 4) as usize] }
    }

    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Option<Self> {
        (rgba.len() == (width as usize * height as usize * 4) && width > 0 && height > 0).then_some(Self { width, height, rgba })
    }
}

/// The edges of a mesh's UV layout, each once, in texel space of a
/// `width` × `height` texture: what a UV editor draws over the texture.
pub fn uv_edges(mesh: &Mesh, width: u32, height: u32) -> Vec<[[f32; 2]; 2]> {
    let to = |i: u32| {
        let uv = mesh.vertices[i as usize].uv;
        [uv[0] * width as f32, (1.0 - uv[1]) * height as f32]
    };
    let key = |p: [f32; 2]| ((p[0] * 64.0).round() as i64, (p[1] * 64.0).round() as i64);
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for t in mesh.indices.as_chunks::<3>().0 {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let (pa, pb) = (to(a), to(b));
            let (ka, kb) = (key(pa), key(pb));
            if seen.insert(if ka <= kb { (ka, kb) } else { (kb, ka) }) {
                out.push([pa, pb]);
            }
        }
    }
    out
}

/// Clip-space position (x, y, z, w) of a world point.
fn clip(m: &Mat4, p: Vec3) -> [f32; 4] {
    let v = [p.x, p.y, p.z, 1.0];
    std::array::from_fn(|r| (0..4).map(|c| m.0[c][r] * v[c]).sum())
}

/// Screen position (pixels) and depth (0..1) of a clip-space point, if in front.
fn screen(c: [f32; 4], w: f32, h: f32) -> Option<(f32, f32, f32)> {
    if c[3] <= 1e-6 {
        return None;
    }
    let (x, y, z) = (c[0] / c[3], c[1] / c[3], c[2] / c[3]);
    Some(((x + 1.0) * 0.5 * w, (1.0 - y) * 0.5 * h, z))
}

/// Depth of the nearest surface at each pixel of `region` (x0, y0, x1, y1).
struct DepthBuffer {
    x0: i32,
    y0: i32,
    w: i32,
    h: i32,
    depth: Vec<f32>,
}

impl DepthBuffer {
    fn new(region: (i32, i32, i32, i32)) -> Self {
        let (w, h) = ((region.2 - region.0).max(0), (region.3 - region.1).max(0));
        Self { x0: region.0, y0: region.1, w, h, depth: vec![f32::INFINITY; (w * h) as usize] }
    }

    fn triangle(&mut self, p: [(f32, f32, f32); 3]) {
        let min_x = (p.iter().map(|v| v.0).fold(f32::MAX, f32::min).floor() as i32).max(self.x0);
        let max_x = (p.iter().map(|v| v.0).fold(f32::MIN, f32::max).ceil() as i32).min(self.x0 + self.w - 1);
        let min_y = (p.iter().map(|v| v.1).fold(f32::MAX, f32::min).floor() as i32).max(self.y0);
        let max_y = (p.iter().map(|v| v.1).fold(f32::MIN, f32::max).ceil() as i32).min(self.y0 + self.h - 1);
        let area = (p[1].0 - p[0].0) * (p[2].1 - p[0].1) - (p[2].0 - p[0].0) * (p[1].1 - p[0].1);
        if area.abs() < 1e-9 {
            return;
        }
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let b0 = ((p[1].0 - px) * (p[2].1 - py) - (p[2].0 - px) * (p[1].1 - py)) / area;
                let b1 = ((p[2].0 - px) * (p[0].1 - py) - (p[0].0 - px) * (p[2].1 - py)) / area;
                let b2 = 1.0 - b0 - b1;
                if b0 < 0.0 || b1 < 0.0 || b2 < 0.0 {
                    continue;
                }
                let z = b0 * p[0].2 + b1 * p[1].2 + b2 * p[2].2;
                let i = ((y - self.y0) * self.w + (x - self.x0)) as usize;
                if z < self.depth[i] {
                    self.depth[i] = z;
                }
            }
        }
    }

    /// The nearest depth around pixel (x, y): a little slack at silhouettes.
    fn at(&self, x: f32, y: f32) -> f32 {
        let (cx, cy) = (x.floor() as i32, y.floor() as i32);
        let mut best = f32::INFINITY;
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (px, py) = (cx + dx - self.x0, cy + dy - self.y0);
                if px >= 0 && py >= 0 && px < self.w && py < self.h {
                    best = best.min(self.depth[(py * self.w + px) as usize]);
                }
            }
        }
        best
    }
}

/// Bilinear sample of premultiplied RGBA8 at pixel position (x, y).
fn sample(rgba: &[u8], w: u32, h: u32, x: f32, y: f32) -> [f32; 4] {
    let (fx, fy) = (x - 0.5, y - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let mut out = [0.0f32; 4];
    for (dx, dy, k) in [(0, 0, (1.0 - tx) * (1.0 - ty)), (1, 0, tx * (1.0 - ty)), (0, 1, (1.0 - tx) * ty), (1, 1, tx * ty)] {
        let (sx, sy) = (x0 as i64 + dx, y0 as i64 + dy);
        if sx < 0 || sy < 0 || sx >= w as i64 || sy >= h as i64 || k == 0.0 {
            continue;
        }
        let i = ((sy as u64 * w as u64 + sx as u64) * 4) as usize;
        for c in 0..4 {
            out[c] += rgba[i + c] as f32 / 255.0 * k;
        }
    }
    out
}

/// The painted area of a premultiplied picture: (x0, y0, x1, y1), exclusive.
fn painted(rgba: &[u8], w: u32, h: u32) -> Option<(i32, i32, i32, i32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for y in 0..h as i32 {
        let row = &rgba[(y as usize * w as usize * 4)..((y as usize + 1) * w as usize * 4)];
        if let Some(first) = row.as_chunks::<4>().0.iter().position(|p| p[3] != 0) {
            let last = row.as_chunks::<4>().0.iter().rposition(|p| p[3] != 0).unwrap_or(first);
            (x0, x1) = (x0.min(first as i32), x1.max(last as i32 + 1));
            (y0, y1) = (y0.min(y), y + 1);
        }
    }
    (x0 < x1).then_some((x0, y0, x1, y1))
}

/// Carries the paint of `overlay` (premultiplied RGBA8, `size` = the 3D
/// layer's canvas, seen through the scene's camera) onto the texture of
/// object `object`. Returns the changed texels' bounds (x0, y0, x1, y1), or
/// `None` when nothing landed on the object.
pub fn project(
    scene: &Scene3D,
    object: usize,
    overlay: &[u8],
    size: (u32, u32),
    texture: &mut Texture,
) -> Option<(u32, u32, u32, u32)> {
    let (w, h) = (size.0 as f32, size.1 as f32);
    let target = scene.objects.get(object)?;
    let area = painted(overlay, size.0, size.1)?;
    let view_proj = scene.camera.view_projection(w / h.max(1.0));
    // What's in view in the painted area: every visible object's surfaces.
    let mut depth = DepthBuffer::new((area.0 - 2, area.1 - 2, area.2 + 2, area.3 + 2));
    for o in scene.objects.iter().filter(|o| o.visible) {
        let m = view_proj * o.transform.matrix();
        for t in o.mesh.indices.as_chunks::<3>().0 {
            let p = t.map(|i| screen(clip(&m, Vec3::from_array(o.mesh.vertices[i as usize].position)), w, h));
            if let [Some(a), Some(b), Some(c)] = p {
                depth.triangle([a, b, c]);
            }
        }
    }
    let model = target.transform.matrix();
    let m = view_proj * model;
    let eye = scene.camera.eye();
    let ortho = matches!(scene.camera.projection, crate::camera::Projection::Orthographic);
    let (tw, th) = (texture.width as f32, texture.height as f32);
    let mut changed: Option<(u32, u32, u32, u32)> = None;
    let mesh = &target.mesh;
    for t in mesh.indices.as_chunks::<3>().0 {
        let v = t.map(|i| mesh.vertices[i as usize]);
        let uv = v.map(|v| (v.uv[0] * tw, (1.0 - v.uv[1]) * th));
        let world = v.map(|v| model.transform_point(Vec3::from_array(v.position)));
        let cs = v.map(|v| clip(&m, Vec3::from_array(v.position)));
        // Skip triangles wholly off the painted area.
        let on: Vec<(f32, f32, f32)> = cs.iter().filter_map(|c| screen(*c, w, h)).collect();
        if on.len() < 3 {
            continue;
        }
        let (sx0, sx1) = (on.iter().map(|p| p.0).fold(f32::MAX, f32::min), on.iter().map(|p| p.0).fold(f32::MIN, f32::max));
        let (sy0, sy1) = (on.iter().map(|p| p.1).fold(f32::MAX, f32::min), on.iter().map(|p| p.1).fold(f32::MIN, f32::max));
        if sx1 < area.0 as f32 - 1.0 || sx0 > area.2 as f32 + 1.0 || sy1 < area.1 as f32 - 1.0 || sy0 > area.3 as f32 + 1.0 {
            continue;
        }
        let normal = (world[1] - world[0]).cross(world[2] - world[0]);
        if normal.length() < 1e-12 {
            continue;
        }
        let normal = normal.normalize();
        let uv_area = (uv[1].0 - uv[0].0) * (uv[2].1 - uv[0].1) - (uv[2].0 - uv[0].0) * (uv[1].1 - uv[0].1);
        if uv_area.abs() < 1e-9 {
            continue;
        }
        // Texels up to about one texel outside the edges count, for the seams.
        let longest = [(0, 1), (1, 2), (2, 0)]
            .iter()
            .map(|&(a, b)| ((uv[a].0 - uv[b].0).powi(2) + (uv[a].1 - uv[b].1).powi(2)).sqrt())
            .fold(1.0f32, f32::max);
        let slack = (1.5 * longest / uv_area.abs()).min(0.5);
        let min_x = (uv.iter().map(|p| p.0).fold(f32::MAX, f32::min).floor() as i64 - 1).max(0);
        let max_x = (uv.iter().map(|p| p.0).fold(f32::MIN, f32::max).ceil() as i64 + 1).min(texture.width as i64 - 1);
        let min_y = (uv.iter().map(|p| p.1).fold(f32::MAX, f32::min).floor() as i64 - 1).max(0);
        let max_y = (uv.iter().map(|p| p.1).fold(f32::MIN, f32::max).ceil() as i64 + 1).min(texture.height as i64 - 1);
        for ty in min_y..=max_y {
            for tx in min_x..=max_x {
                let (px, py) = (tx as f32 + 0.5, ty as f32 + 0.5);
                let mut b = [
                    ((uv[1].0 - px) * (uv[2].1 - py) - (uv[2].0 - px) * (uv[1].1 - py)) / uv_area,
                    ((uv[2].0 - px) * (uv[0].1 - py) - (uv[0].0 - px) * (uv[2].1 - py)) / uv_area,
                    0.0,
                ];
                b[2] = 1.0 - b[0] - b[1];
                if b.iter().any(|&k| k < -slack) {
                    continue;
                }
                // Points just outside take the nearest edge's place.
                let b = {
                    let c = b.map(|k| k.max(0.0));
                    let s = c[0] + c[1] + c[2];
                    c.map(|k| k / s)
                };
                let c: [f32; 4] = std::array::from_fn(|r| b[0] * cs[0][r] + b[1] * cs[1][r] + b[2] * cs[2][r]);
                let Some((sx, sy, sz)) = screen(c, w, h) else { continue };
                if sx < 0.0 || sy < 0.0 || sx >= w || sy >= h {
                    continue;
                }
                if sz > depth.at(sx, sy) + 2e-3 * (1.0 - sz).max(1e-3).sqrt() {
                    continue; // hidden behind something
                }
                let p = paint_at(overlay, size, sx, sy);
                if p[3] <= 0.0 {
                    continue;
                }
                let point = world[0] * b[0] + world[1] * b[1] + world[2] * b[2];
                let to_eye = if ortho { -scene.camera.forward() } else { (eye - point).normalize() };
                let facing = normal.dot(to_eye).abs();
                // Full strength from head-on to 60°, fading out towards edge-on.
                let fade = ((facing - 0.05) / 0.45).clamp(0.0, 1.0);
                if fade <= 0.0 {
                    continue;
                }
                let i = ((ty as u64 * texture.width as u64 + tx as u64) * 4) as usize;
                let dst = &mut texture.rgba[i..i + 4];
                let src = p.map(|v| v * fade);
                let da = dst[3] as f32 / 255.0;
                let dc = [dst[0], dst[1], dst[2]].map(|v| v as f32 / 255.0 * da);
                let out_a = src[3] + da * (1.0 - src[3]);
                for k in 0..3 {
                    let premul = src[k] + dc[k] * (1.0 - src[3]);
                    dst[k] = if out_a > 0.0 { ((premul / out_a).clamp(0.0, 1.0) * 255.0).round() as u8 } else { 0 };
                }
                dst[3] = (out_a.clamp(0.0, 1.0) * 255.0).round() as u8;
                let (x, y) = (tx as u32, ty as u32);
                changed = Some(match changed {
                    None => (x, y, x + 1, y + 1),
                    Some((a, b, c, d)) => (a.min(x), b.min(y), c.max(x + 1), d.max(y + 1)),
                });
            }
        }
    }
    changed
}

fn paint_at(overlay: &[u8], size: (u32, u32), x: f32, y: f32) -> [f32; 4] {
    sample(overlay, size.0, size.1, x, y)
}

/// The UV coordinates under canvas pixel (x, y) on object `object`, if it's the nearest hit.
pub fn uv_at(scene: &Scene3D, object: usize, x: f32, y: f32, size: (u32, u32)) -> Option<[f32; 2]> {
    let ray = scene.camera.ray(x, y, size.0 as f32, size.1 as f32);
    let o = scene.objects.get(object)?;
    let m = o.transform.matrix();
    let mut best: Option<(f32, [f32; 2])> = None;
    for t in o.mesh.indices.as_chunks::<3>().0 {
        let v = t.map(|i| o.mesh.vertices[i as usize]);
        let p = v.map(|v| m.transform_point(Vec3::from_array(v.position)));
        // Möller–Trumbore.
        let (e1, e2) = (p[1] - p[0], p[2] - p[0]);
        let q = ray.dir.cross(e2);
        let det = e1.dot(q);
        if det.abs() < 1e-12 {
            continue;
        }
        let s = ray.origin - p[0];
        let u = s.dot(q) / det;
        let r = s.cross(e1);
        let w = ray.dir.dot(r) / det;
        let dist = e2.dot(r) / det;
        if u < 0.0 || w < 0.0 || u + w > 1.0 || dist <= 0.0 || best.is_some_and(|(d, _)| d <= dist) {
            continue;
        }
        let b = [1.0 - u - w, u, w];
        best = Some((dist, [0, 1].map(|k| b[0] * v[0].uv[k] + b[1] * v[1].uv[k] + b[2] * v[2].uv[k])));
    }
    best.map(|(_, uv)| uv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gizmo::Transform;
    use crate::math::vec3;
    use crate::scene::{SceneObject, CLAY};
    use std::sync::Arc;

    /// A unit quad facing the camera with UVs over the whole texture.
    fn quad_scene() -> Scene3D {
        let corners = [[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0]];
        let uvs = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let mesh = Mesh::from_polygons("quad", &[vec![0, 1, 2, 3]], |c| crate::mesh::Vertex {
            position: corners[c],
            normal: [0.0; 3],
            uv: uvs[c],
        });
        let mut scene = Scene3D::default();
        scene.objects.push(SceneObject {
            name: "quad".into(),
            mesh: Arc::new(mesh),
            transform: Transform::default(),
            visible: true,
            color: CLAY,
            texture: None,
            rig: None,
        });
        scene.camera.set_view(crate::camera::ViewPreset::Front);
        scene.camera.pivot = Vec3::ZERO;
        scene.camera.distance = 5.0;
        scene
    }

    #[test]
    fn paint_lands_where_it_was_drawn() {
        let mut scene = quad_scene();
        // The camera looks along -Y in front view; turn the quad to face it.
        scene.objects[0].transform.rotation = crate::math::Quat::from_axis_angle(Vec3::X, std::f32::consts::FRAC_PI_2);
        let size = (200, 200);
        let centre = scene.camera.to_screen(vec3(0.0, 0.0, 0.0), 200.0, 200.0).unwrap();
        // A red dot at the centre of the screen.
        let mut overlay = vec![0u8; 200 * 200 * 4];
        for y in -3..=3 {
            for x in -3..=3 {
                let (px, py) = ((centre[0] as i32 + x) as usize, (centre[1] as i32 + y) as usize);
                overlay[(py * 200 + px) * 4..(py * 200 + px) * 4 + 4].copy_from_slice(&[255, 0, 0, 255]);
            }
        }
        let mut tex = Texture::new(64, 64);
        let changed = project(&scene, 0, &overlay, size, &mut tex).expect("paint landed");
        // The centre of the quad is the centre of the texture.
        let mid = ((32 * 64 + 32) * 4) as usize;
        assert_eq!(&tex.rgba[mid..mid + 4], &[255, 0, 0, 255]);
        assert!(changed.0 > 16 && changed.2 < 48, "only around the middle: {changed:?}");
        let corner = 0;
        assert_eq!(tex.rgba[corner + 3], 0, "nothing far from the dot");

        // Behind another quad nothing lands.
        let mut hidden = scene.clone();
        let mut front = hidden.objects[0].clone();
        front.transform.location = hidden.camera.eye() * 0.5;
        hidden.objects.push(front);
        let mut tex = Texture::new(64, 64);
        assert!(project(&hidden, 0, &overlay, size, &mut tex).is_none());
        assert!(
            uv_at(&scene, 0, centre[0], centre[1], size).is_some_and(|uv| (uv[0] - 0.5).abs() < 0.05 && (uv[1] - 0.5).abs() < 0.05)
        );
    }

    #[test]
    fn uv_edges_are_listed_once() {
        let scene = quad_scene();
        // Four sides and the diagonal of the fan.
        assert_eq!(uv_edges(&scene.objects[0].mesh, 10, 10).len(), 5);
    }
}
