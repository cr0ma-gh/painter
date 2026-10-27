//! 3D layers: the depth buffer decides visibility (not draw order), and the
//! canvas shows the rendered scene as the layer. Skipped without a GPU.

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use painter_core::{Document, Layer};
use painter_render::{build_plan, full_upload, live_planes, CanvasFrame, GpuCanvas, SceneDraw, SceneLight, ScenePreview, SceneRenderer, ViewUniform};
use scene_3d::camera::{ViewCamera, ViewPreset};
use scene_3d::gizmo::Transform;
use scene_3d::math::vec3;
use scene_3d::mesh::Mesh;

fn gpu() -> Option<(&'static wgpu::Device, &'static wgpu::Queue, MutexGuard<'static, ()>)> {
    static DEVICE: OnceLock<Option<(wgpu::Device, wgpu::Queue)>> = OnceLock::new();
    static TURN: Mutex<()> = Mutex::new(());
    let (device, queue) = DEVICE
        .get_or_init(|| {
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
            let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))?;
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None)).ok()
        })
        .as_ref()?;
    Some((device, queue, TURN.lock().unwrap_or_else(|e| e.into_inner())))
}

/// A red cube in front of a bigger blue one, seen from the front; `far_first`
/// swaps the draw order.
fn scene(layer: painter_core::LayerId, far_first: bool, size: (u32, u32)) -> ScenePreview {
    let mut cam = ViewCamera::default();
    cam.set_view(ViewPreset::Front);
    cam.distance = 8.0;
    let cube = Arc::new(Mesh::cube());
    let near = Transform { location: vec3(0.0, -1.0, 0.0), ..Transform::default() };
    let far = Transform { location: vec3(0.0, 2.0, 0.0), scale: vec3(3.0, 3.0, 3.0), ..Transform::default() };
    let draw = |t: Transform, color| SceneDraw { mesh: cube.clone(), model: t.matrix().0, color, selected: false, texture: None };
    let mut draws = vec![draw(near, [1.0, 0.1, 0.1]), draw(far, [0.1, 0.1, 1.0])];
    if far_first {
        draws.reverse();
    }
    ScenePreview {
        layer,
        revision: 1,
        view_proj: cam.view_projection(size.0 as f32 / size.1 as f32).0,
        draws,
        // Light from the camera so the front faces are bright.
        lights: vec![SceneLight::Sun { direction: [0.0, 1.0, 0.0], color: [1.0; 3] }],
        ambient: 0.0,
    }
}

#[test]
fn depth_decides_what_is_in_front() {
    let Some((device, queue, _turn)) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let size = (96, 64);
    let mut renderer = SceneRenderer::new(device);
    let px = |rgba: &[u8], x: usize, y: usize| rgba[(y * 96 + x) * 4..][..4].to_vec();
    for far_first in [false, true] {
        let rgba = renderer.render_to_rgba(device, queue, &scene(painter_core::LayerId(1), far_first, size), size);
        let centre = px(&rgba, 48, 32);
        assert!(centre[0] > 200 && centre[2] < 60, "near red cube wins (far first: {far_first}): {centre:?}");
        // The blue cube spans about x 36..60, the red one x 43..53.
        let side = px(&rgba, 38, 32);
        assert!(side[2] > 200 && side[0] < 60, "far blue cube beside it: {side:?}");
        assert_eq!(px(&rgba, 0, 0), [0, 0, 0, 0], "transparent background");
    }
}

#[test]
fn canvas_shows_the_scene_as_the_layer() {
    let Some((device, queue, _turn)) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let mut doc = Document::new(96, 64);
    let id = doc.alloc_id();
    doc.layers.push(Layer::raster(id, "3D"));
    let mut canvas = GpuCanvas::new(device, wgpu::TextureFormat::Rgba8Unorm);
    let size = [96.0, 64.0];
    let frame = CanvasFrame {
        epoch: 1,
        doc_size: (96, 64),
        plan: build_plan(&doc),
        uploads: full_upload(&doc),
        live_planes: live_planes(&doc),
        selection: false,
        recomposite: true,
        stroke: None,
        transform: None,
        scenes: vec![scene(id, true, (96, 64))],
        view: ViewUniform::new(size, size, [0.0, 0.0], 1.0, 0.0, 8.0),
        ink_lut: None,
    };
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    canvas.prepare(device, queue, &mut encoder, &frame);
    queue.submit([encoder.finish()]);
    let rgba = canvas.read_composite(device, queue).unwrap();
    let centre = &rgba[(32 * 96 + 48) * 4..][..4];
    assert!(centre[0] > 200 && centre[2] < 60, "the red cube over the white paper: {centre:?}");
    assert_eq!(&rgba[..4], &[255, 255, 255, 255], "paper where the scene is empty");
}

#[test]
fn textures_paint_over_the_colour() {
    let Some((device, queue, _turn)) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let size = (96, 64);
    let mut renderer = SceneRenderer::new(device);
    let mut preview = scene(painter_core::LayerId(1), false, size);
    let green = scene_3d::texture::Texture::from_rgba(1, 1, vec![0, 255, 0, 255]).unwrap();
    preview.draws[0].texture = Some(Arc::new(green));
    let rgba = renderer.render_to_rgba(device, queue, &preview, size);
    let centre = &rgba[(32 * 96 + 48) * 4..][..4];
    assert!(centre[1] > 200 && centre[0] < 60, "the texture covers the red: {centre:?}");
    // A half-transparent texture lets the colour through.
    let half = scene_3d::texture::Texture::from_rgba(1, 1, vec![0, 255, 0, 128]).unwrap();
    preview.draws[0].texture = Some(Arc::new(half));
    let rgba = renderer.render_to_rgba(device, queue, &preview, size);
    let centre = &rgba[(32 * 96 + 48) * 4..][..4];
    assert!(centre[0] > 90 && centre[1] > 90, "mixed: {centre:?}");
}

#[test]
fn normals_stay_perpendicular_under_non_uniform_scale() {
    let Some((device, queue, _turn)) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    // A quad on the plane z = y, facing the camera at 45°.
    let normal = [0.0, -std::f32::consts::FRAC_1_SQRT_2, std::f32::consts::FRAC_1_SQRT_2];
    let corners = [[-0.5, -0.5, -0.5], [0.5, -0.5, -0.5], [0.5, 0.5, 0.5], [-0.5, 0.5, 0.5]];
    let vertices = corners.map(|position| scene_3d::mesh::Vertex { position, normal, uv: [0.0; 2] }).to_vec();
    let quad = Arc::new(Mesh::from_buffers("Quad", vertices, vec![0, 1, 2, 0, 2, 3]));
    // Stretched 4× along z the surface becomes z = 4y: its true normal is
    // (0, -4, 1) / √17, nearly facing the camera; scaling the normal like a
    // position would give (0, -1, 4) / √17, nearly edge-on to it.
    let size = (96, 64);
    let mut cam = ViewCamera::default();
    cam.set_view(ViewPreset::Front);
    cam.distance = 8.0;
    let stretch = Transform { scale: vec3(1.0, 1.0, 4.0), ..Transform::default() };
    let preview = ScenePreview {
        layer: painter_core::LayerId(1),
        revision: 1,
        view_proj: cam.view_projection(size.0 as f32 / size.1 as f32).0,
        draws: vec![SceneDraw { mesh: quad, model: stretch.matrix().0, color: [1.0; 3], selected: false, texture: None }],
        // Light from the camera: brightness is the normal's facing.
        lights: vec![SceneLight::Sun { direction: [0.0, 1.0, 0.0], color: [1.0; 3] }],
        ambient: 0.0,
    };
    let mut renderer = SceneRenderer::new(device);
    let rgba = renderer.render_to_rgba(device, queue, &preview, size);
    let centre = &rgba[(32 * 96 + 48) * 4..][..4];
    let expected = 255.0 * 4.0 / 17f32.sqrt();
    for c in &centre[..3] {
        assert!((*c as f32 - expected).abs() <= 3.0, "lit by the true normal ({expected:.0}): {centre:?}");
    }
    assert_eq!(centre[3], 255);
}

#[test]
fn point_light_fades_out_at_its_range() {
    let Some((device, queue, _turn)) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    // A white unit cube seen from the front, its front face (y = -0.5) facing
    // a point light 50 units away on the view axis.
    let size = (96, 64);
    let mut cam = ViewCamera::default();
    cam.set_view(ViewPreset::Front);
    cam.distance = 8.0;
    let cube = Arc::new(Mesh::cube());
    let lit = |range: f32| {
        let preview = ScenePreview {
            layer: painter_core::LayerId(1),
            revision: 1,
            view_proj: cam.view_projection(size.0 as f32 / size.1 as f32).0,
            draws: vec![SceneDraw { mesh: cube.clone(), model: Transform::default().matrix().0, color: [1.0; 3], selected: false, texture: None }],
            lights: vec![SceneLight::Point { position: [0.0, -50.5, 0.0], range, color: [1.0; 3] }],
            ambient: 0.0,
        };
        let mut renderer = SceneRenderer::new(device);
        let rgba = renderer.render_to_rgba(device, queue, &preview, size);
        rgba[(32 * 96 + 48) * 4..][..4].to_vec()
    };
    assert_eq!(lit(10.0), [0, 0, 0, 255], "out of range: no light at all");
    // Halfway through a range of 100: (1 - 0.5)² of full brightness.
    let half = lit(100.0);
    for c in &half[..3] {
        assert!((*c as f32 - 255.0 * 0.25).abs() <= 3.0, "quadratic falloff: {half:?}");
    }
}
