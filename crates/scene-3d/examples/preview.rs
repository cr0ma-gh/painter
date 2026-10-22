//! Renders a `.blend` (or `.obj`) file to PNG on the CPU: z-buffered,
//! flat Lambert shading, with the viewport camera framing the scene. A
//! reference for the wgpu pass and a quick check of what was imported.
//!
//! ```text
//! cargo run -p painter-scene-3d --example preview -- model.blend out.png [orbit_x orbit_y]
//! ```

use scene_3d::blend_loader::{load_objects, BlendFile};
use scene_3d::math::Vec3;
use scene_3d::scene::{LightKind, Scene3D, SceneObject};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = std::path::Path::new(&args[0]);
    let mut scene = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("obj")) {
        let mesh = scene_3d::mesh::load_obj(&std::fs::read_to_string(path).unwrap(), "obj").unwrap();
        let mut s = Scene3D::default();
        s.objects.push(SceneObject { name: "obj".into(), mesh: std::sync::Arc::new(mesh), transform: Default::default(), visible: true, color: scene_3d::scene::CLAY, texture: None });
        s.camera.frame(&s.bounds().unwrap());
        s
    } else {
        Scene3D::from_blend(load_objects(&BlendFile::open(path).unwrap()).unwrap())
    };
    if let (Some(x), Some(y)) = (args.get(2).and_then(|v| v.parse().ok()), args.get(3).and_then(|v| v.parse().ok())) {
        scene.camera.orbit(x, y);
    }
    let (w, h) = (640u32, 640u32);
    let vp = scene.camera.view_projection(w as f32 / h as f32);
    let light = match scene.lights[0].kind {
        LightKind::Sun { direction } => -direction,
        LightKind::Point { position, .. } => position.normalize(),
    };
    let mut depth = vec![f32::MAX; (w * h) as usize];
    let mut img = image::RgbImage::from_pixel(w, h, image::Rgb([48, 48, 52]));
    let mut triangles = 0;
    for obj in &scene.objects {
        let m = obj.transform.matrix();
        for t in obj.mesh.triangles() {
            let world = t.map(|p| m.transform_point(p));
            let n = (world[1] - world[0]).cross(world[2] - world[0]).normalize();
            let shade = scene.ambient + (1.0 - scene.ambient) * n.dot(light).abs();
            let s = world.map(|p| vp.transform_point(p));
            let px = s.map(|p| [(p.x + 1.0) * 0.5 * w as f32, (1.0 - p.y) * 0.5 * h as f32, p.z]);
            let area = (px[1][0] - px[0][0]) * (px[2][1] - px[0][1]) - (px[1][1] - px[0][1]) * (px[2][0] - px[0][0]);
            if area.abs() < 1e-9 || s.iter().any(|p| p.z < 0.0 || p.z > 1.0) {
                continue;
            }
            triangles += 1;
            let x0 = px.iter().map(|p| p[0]).fold(f32::MAX, f32::min).floor().max(0.0) as u32;
            let x1 = (px.iter().map(|p| p[0]).fold(f32::MIN, f32::max).ceil() as u32).min(w);
            let y0 = px.iter().map(|p| p[1]).fold(f32::MAX, f32::min).floor().max(0.0) as u32;
            let y1 = (px.iter().map(|p| p[1]).fold(f32::MIN, f32::max).ceil() as u32).min(h);
            for y in y0..y1 {
                for x in x0..x1 {
                    let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                    let e = |a: [f32; 3], b: [f32; 3]| ((b[0] - a[0]) * (fy - a[1]) - (b[1] - a[1]) * (fx - a[0])) / area;
                    let (u, v, k) = (e(px[1], px[2]), e(px[2], px[0]), e(px[0], px[1]));
                    if u < 0.0 || v < 0.0 || k < 0.0 {
                        continue;
                    }
                    let z = u * px[0][2] + v * px[1][2] + k * px[2][2];
                    let i = (y * w + x) as usize;
                    if z < depth[i] {
                        depth[i] = z;
                        let c = (shade.clamp(0.0, 1.0) * 220.0) as u8;
                        img.put_pixel(x, y, image::Rgb([c, c, (c as f32 * 1.05).min(255.0) as u8]));
                    }
                }
            }
        }
    }
    img.save(&args[1]).unwrap();
    let names: Vec<&str> = scene.objects.iter().map(|o| o.name.as_str()).collect();
    println!("{} objects {:?}, {triangles} triangles drawn, eye {:?}", names.len(), names, scene.camera.eye().to_array().map(|v| (v * 100.0).round() / 100.0));
    let _ = Vec3::ZERO;
}
