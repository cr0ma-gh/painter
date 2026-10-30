//! The transform preview shader must match the CPU resampler the commit uses
//! (`transform_tools::resample`), for every filter and transform kind.
//! Skipped (with a note) without a usable GPU adapter.

use std::sync::{Mutex, MutexGuard, OnceLock};

use std::sync::Arc;

use painter_core::{io, Document, LayerKind, Mask, TileStore};
use painter_render::{build_plan, full_upload, live_planes, CanvasFrame, GpuCanvas, TransformGeometry, TransformPreview, TransformRenderer, ViewUniform};
use transform_tools::{commit, resample, AffineParams, Image, Interpolation, Mat3, MeshWarp, Placement};

/// Shared device, one test at a time (see `gpu_parity.rs`).
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
    let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    Some((device, queue, turn))
}

/// Line art with hard edges, a soft alpha ramp and colour: premultiplied.
fn source() -> Image {
    let (w, h) = (37u32, 29u32);
    let rgba = (0..w * h)
        .flat_map(|i| {
            let (x, y) = ((i % w) as f32, (i / w) as f32);
            let ring = (((x - 12.0).powi(2) + (y - 14.0).powi(2)).sqrt() - 8.0).abs() < 1.5;
            let bar = (x - y * 0.8 - 14.0).abs() < 1.2;
            let ramp = (x / w as f32 * 255.0) as u8;
            if ring {
                [20, 30, 160, 255]
            } else if bar {
                [200, 40, 10, 255]
            } else if y > 22.0 {
                let a = ramp as u32;
                [(250 * a / 255) as u8, (200 * a / 255) as u8, 0, ramp]
            } else {
                [0; 4]
            }
        })
        .collect();
    Image { width: w, height: h, rgba }
}

/// Matrix rounding and evaluation order differ slightly between f32 on the
/// GPU and on the CPU; a filter or tap mismatch would be far bigger.
const TOLERANCE: u8 = 2;

fn check(forward: Mat3, size: (u32, u32), what: &str) {
    let Some((device, queue, _turn)) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let renderer = TransformRenderer::new(device);
    let src = source();
    let inverse = forward.inverse().expect("invertible").to_f32();
    for filter in Interpolation::ALL {
        let cpu = resample(&src, &forward, size.0, size.1, filter).unwrap();
        let gpu = renderer.render_to_rgba(device, queue, &src.rgba, (src.width, src.height), size, &TransformGeometry::Inverse(inverse), filter.shader_id());
        assert_eq!(gpu.len(), cpu.rgba.len());
        let (diff, at) = gpu.iter().zip(&cpu.rgba).enumerate().map(|(i, (a, b))| (a.abs_diff(*b), i / 4)).max().unwrap();
        // Count pixels over a tighter bound too, so a systematic offset can't hide under the max.
        let off = gpu.iter().zip(&cpu.rgba).filter(|(a, b)| a.abs_diff(**b) > 1).count();
        assert!(diff <= TOLERANCE, "{what}, {filter:?}: max difference {diff} at pixel {at} ({}, {})", at % size.0 as usize, at / size.0 as usize);
        assert!(off * 200 < gpu.len(), "{what}, {filter:?}: {off} channels differ by more than 1");
        assert!(cpu.rgba.chunks(4).any(|p| p[3] == 255), "{what}: the transform shows something");
    }
}

#[test]
fn identity_and_translation_match() {
    check(Mat3::IDENTITY, (37, 29), "identity");
    check(Mat3::translate(5.25, -3.5), (48, 40), "sub-pixel translation");
}

#[test]
fn scale_rotate_skew_match() {
    let p = AffineParams { scale: [1.7, 1.3], rotation: 30.0, skew: [0.2, 0.0], translate: [14.0, 9.0], ..AffineParams::identity([18.5, 14.5]) };
    check(p.matrix(), (80, 64), "scale + rotate + skew");
    let down = AffineParams { scale: [0.6, 0.6], rotation: -12.0, ..AffineParams::identity([0.0, 0.0]) };
    check(down.matrix(), (30, 24), "downscale");
}

#[test]
fn perspective_and_free_distort_match() {
    let rect = [0.0, 0.0, 37.0, 29.0];
    let perspective = Mat3::rect_to_quad(rect, [[18.0, 6.0], [52.0, 6.0], [68.0, 50.0], [2.0, 50.0]]).unwrap();
    check(perspective, (72, 56), "perspective");
    let free = Mat3::rect_to_quad(rect, [[4.0, 10.0], [60.0, 2.0], [50.0, 54.0], [10.0, 40.0]]).unwrap();
    check(free, (64, 60), "free distort");
}

/// The whole preview path: `GpuCanvas` shows the layer's rest with the
/// transformed floating pixels over it, and must match the CPU commit.
fn check_canvas(placement: Placement, geometry: TransformGeometry, filter: Interpolation, what: &str) {
    let Some((device, queue, _turn)) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let (w, h) = (96u32, 80u32);
    let mut doc = Document::new(w, h);
    // What stays on the layer: a green band.
    let rest_rgba: Vec<u8> = (0..w * h).flat_map(|i| if (30..40).contains(&(i / w)) { [0, 160, 40, 255] } else { [0; 4] }).collect();
    let rest = TileStore::from_rgba(w, h, &rest_rgba);
    let layer = doc.active;
    doc.find_mut(layer).unwrap().kind = LayerKind::Raster(rest.clone());
    let src = source();

    let mut canvas = GpuCanvas::new(device, wgpu::TextureFormat::Rgba8Unorm);
    let size = [w as f32, h as f32];
    let frame = CanvasFrame {
        epoch: 1,
        doc_size: (w, h),
        plan: build_plan(&doc),
        uploads: full_upload(&doc),
        live_planes: live_planes(&doc),
        selection: false,
        recomposite: true,
        stroke: None,
        transform: Some(TransformPreview {
            id: 1,
            revision: 1,
            layer,
            mask: false,
            source: Arc::new(src.rgba.clone()),
            source_size: (src.width, src.height),
            geometry,
            filter: filter.shader_id(),
        }),
        scenes: Vec::new(),
        view: ViewUniform::new(size, size, [0.0, 0.0], 1.0, 0.0, 8.0),
        ink_lut: None,
        mask_view: None,
    };
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    canvas.prepare(device, queue, &mut encoder, &frame);
    queue.submit([encoder.finish()]);
    let gpu = canvas.read_composite(device, queue).unwrap();

    let committed = commit(&rest, &src, &placement, filter, (w, h));
    doc.find_mut(layer).unwrap().kind = LayerKind::Raster(committed);
    let cpu = io::flatten(&doc);
    let (diff, at) = gpu.iter().zip(&cpu).enumerate().map(|(i, (a, b))| (a.abs_diff(*b), i / 4)).max().unwrap();
    let off = gpu.iter().zip(&cpu).filter(|(a, b)| a.abs_diff(**b) > 1).count();
    assert!(diff <= 3, "{what}: max difference {diff} at ({}, {})", at % w as usize, at / w as usize);
    assert!(off * 200 < gpu.len(), "{what}: {off} channels differ by more than 1");
}

#[test]
fn canvas_preview_matches_the_commit() {
    let quad = [[30.0, 8.0], [80.0, 14.0], [90.0, 70.0], [8.0, 60.0]];
    let m = Mat3::rect_to_quad([0.0, 0.0, 37.0, 29.0], quad).unwrap();
    let inverse = m.inverse().unwrap().to_f32();
    for filter in [Interpolation::Bilinear, Interpolation::Lanczos3] {
        check_canvas(Placement::Matrix(m), TransformGeometry::Inverse(inverse), filter, "perspective");
    }

    // A warp: the middle of the grid pushed down and right.
    let mut warp = MeshWarp::new([10.0, 10.0, 74.0, 58.0], 3, 3);
    let p = warp.point(1, 1);
    *warp.point_mut(1, 1) = [p[0] + 9.0, p[1] + 7.0];
    let mesh = warp.tessellate(6);
    // Sources in floating-image pixels: the grid spans the image.
    let sources: Vec<[f64; 2]> = mesh.uvs.iter().map(|uv| [(uv[0] - 10.0) / 74.0 * 37.0, (uv[1] - 10.0) / 58.0 * 29.0]).collect();
    let geometry = TransformGeometry::Mesh {
        positions: mesh.positions.iter().map(|p| [p[0] as f32, p[1] as f32]).collect(),
        sources: sources.iter().map(|s| [s[0] as f32, s[1] as f32]).collect(),
        indices: mesh.indices.clone(),
    };
    let placement = Placement::Mesh { positions: mesh.positions, sources, indices: mesh.indices };
    check_canvas(placement, geometry, Interpolation::Bilinear, "mesh warp");
}
/// Transforming a layer mask alone: the preview reads the mask plane from the
/// transform's target (rest + floating, the hidden amount in red) and must
/// match the committed, settled mask.
#[test]
fn mask_transform_preview_matches_the_commit() {
    let Some((device, queue, _turn)) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let (w, h) = (96u32, 80u32);
    let mut doc = Document::new(w, h);
    let layer = doc.active;
    let paint: Vec<u8> = (0..w * h).flat_map(|i| [(i % w * 2) as u8, 90, 200, 255]).collect();
    doc.find_mut(layer).unwrap().kind = LayerKind::Raster(TileStore::from_rgba(w, h, &paint));
    // What stays of the mask: a hidden band.
    let rest_rgba: Vec<u8> = (0..w * h).flat_map(|i| if (30..40).contains(&(i / w)) { [255; 4] } else { [0; 4] }).collect();
    let rest = TileStore::from_rgba(w, h, &rest_rgba);
    doc.find_mut(layer).unwrap().mask = Some(Mask { tiles: rest.clone(), enabled: true });
    // Floating mask pixels: hidden amount (premultiplied) in colour, coverage in alpha.
    let src = source();
    let floating = Image { width: src.width, height: src.height, rgba: src.rgba.chunks(4).flat_map(|p| [p[0], p[0], p[0], p[3]]).collect() };
    let m = AffineParams { scale: [1.5, 1.2], rotation: 20.0, translate: [30.0, 20.0], ..AffineParams::identity([18.5, 14.5]) }.matrix();
    let filter = Interpolation::Bilinear;

    let mut canvas = GpuCanvas::new(device, wgpu::TextureFormat::Rgba8Unorm);
    let size = [w as f32, h as f32];
    let frame = CanvasFrame {
        epoch: 1,
        doc_size: (w, h),
        plan: build_plan(&doc),
        uploads: full_upload(&doc),
        live_planes: live_planes(&doc),
        selection: false,
        recomposite: true,
        stroke: None,
        transform: Some(TransformPreview {
            id: 1,
            revision: 1,
            layer,
            mask: true,
            source: Arc::new(floating.rgba.clone()),
            source_size: (floating.width, floating.height),
            geometry: TransformGeometry::Inverse(m.inverse().unwrap().to_f32()),
            filter: filter.shader_id(),
        }),
        scenes: Vec::new(),
        view: ViewUniform::new(size, size, [0.0, 0.0], 1.0, 0.0, 8.0),
        ink_lut: None,
        mask_view: None,
    };
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    canvas.prepare(device, queue, &mut encoder, &frame);
    queue.submit([encoder.finish()]);
    let gpu = canvas.read_composite(device, queue).unwrap();

    let mut committed = commit(&rest, &floating, &Placement::Matrix(m), filter, (w, h));
    Mask::settle_all(&mut committed);
    doc.find_mut(layer).unwrap().mask.as_mut().unwrap().tiles = committed;
    let cpu = io::flatten(&doc);
    let (diff, at) = gpu.iter().zip(&cpu).enumerate().map(|(i, (a, b))| (a.abs_diff(*b), i / 4)).max().unwrap();
    assert!(diff <= 3, "mask transform: max difference {diff} at ({}, {})", at % w as usize, at / w as usize);
}
