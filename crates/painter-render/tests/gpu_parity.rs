//! The GPU must show what the CPU will commit: these tests render through
//! `GpuCanvas` on a headless device and compare against `painter-core`.
//! They are skipped (with a note) on machines without a usable GPU adapter.

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use painter_core::raster::paint_stroke;
use painter_core::{
    io, selection, vector, AntiAlias, BlendMode, BrushMode, BrushSettings, Dab, DabTexture, DabTip, Document, DualTip, Gradient,
    GradientShape, Layer, LayerId, LayerKind, Mask, StrokePoint, TileStore, TipBitmap, TipShape, Tone, VectorStroke,
};
use painter_render::{
    build_plan, full_upload, live_planes, CanvasFrame, DabStyle, GpuCanvas, Plane, RenderProfile, StrokePreview, ViewUniform,
};

/// Rounding differs slightly between hardware blending and the CPU's per-dab
/// quantisation; anything visible would be far above this.
const TOLERANCE: u8 = 3;

/// One device shared by every test, used by one test at a time. Creating a
/// device per test in parallel intermittently hung the driver.
fn gpu() -> Option<(&'static wgpu::Device, &'static wgpu::Queue, MutexGuard<'static, ()>)> {
    static DEVICE: OnceLock<Option<(wgpu::Device, wgpu::Queue)>> = OnceLock::new();
    static TURN: Mutex<()> = Mutex::new(());
    let (device, queue) = DEVICE
        .get_or_init(|| {
            // WGPU_BACKEND (e.g. "gl") picks the API, to test the OpenGL path where it exists.
            let backends = wgpu::Backends::from_env().unwrap_or(wgpu::Backends::all());
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends, ..Default::default() });
            let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))?;
            let info = adapter.get_info();
            eprintln!("GPU tests on {} ({:?}, {})", info.name, info.backend, info.driver);
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None)).ok()
        })
        .as_ref()?;
    // A failed test poisons the lock; the device itself is still fine.
    let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    Some((device, queue, turn))
}

fn frame(doc: &Document, upload: bool, stroke: Option<StrokePreview>) -> CanvasFrame {
    let size = [doc.width as f32, doc.height as f32];
    CanvasFrame {
        epoch: 1,
        doc_size: (doc.width, doc.height),
        plan: build_plan(doc),
        uploads: if upload { full_upload(doc) } else { Vec::new() },
        live_planes: live_planes(doc),
        selection: doc.selection.is_some(),
        recomposite: true,
        stroke,
        transform: None,
        scenes: Vec::new(),
        view: ViewUniform::new(size, size, [0.0, 0.0], 1.0, 0.0, 8.0),
        ink_lut: None,
        mask_view: None,
    }
}

fn run(device: &wgpu::Device, queue: &wgpu::Queue, canvas: &mut GpuCanvas, frame: &CanvasFrame) {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    canvas.prepare(device, queue, &mut encoder, frame);
    queue.submit([encoder.finish()]);
}

fn assert_close(gpu: &[u8], cpu: &[u8], what: &str) {
    assert_eq!(gpu.len(), cpu.len());
    let (diff, at) = gpu
        .iter()
        .zip(cpu)
        .enumerate()
        .map(|(i, (x, y))| (x.abs_diff(*y), i / 4))
        .max_by_key(|(d, _)| *d)
        .unwrap_or((0, 0));
    assert!(diff <= TOLERANCE, "{what}: max channel difference {diff} at pixel {at}");
}

fn paint(tiles: &mut TileStore, dabs: &[Dab], color: [f32; 3], settings: &BrushSettings, canvas: (u32, u32)) {
    paint_stroke(tiles, dabs, color, settings, canvas, false, None);
}

fn line(from: (f32, f32), to: (f32, f32), n: usize, radius: f32, alpha: f32) -> Vec<Dab> {
    (0..n)
        .map(|i| {
            let t = i as f32 / (n - 1).max(1) as f32;
            Dab { x: from.0 + (to.0 - from.0) * t, y: from.1 + (to.1 - from.1) * t, radius, alpha, angle: 0.0, thickness: 1.0, mirror: false }
        })
        .collect()
}

fn add_raster(doc: &mut Document, name: &str, blend: BlendMode, dabs: &[Dab], color: [f32; 3]) -> LayerId {
    let id = doc.alloc_id();
    let mut layer = Layer::raster(id, name);
    layer.blend = blend;
    paint(layer.tiles_mut().unwrap(), dabs, color, &BrushSettings::g_pen(), doc.canvas());
    doc.add_layer(layer);
    id
}

/// A document with content on two layers and an isolated folder.
fn sample_document() -> Document {
    let mut doc = Document::new(150, 90);
    let canvas = doc.canvas();
    let base = doc.active;
    paint(
        doc.find_mut(base).unwrap().tiles_mut().unwrap(),
        &line((10.0, 20.0), (140.0, 70.0), 60, 9.0, 1.0),
        [0.1, 0.3, 0.9],
        &BrushSettings::g_pen(),
        canvas,
    );
    add_raster(&mut doc, "Multiply", BlendMode::Multiply, &line((20.0, 80.0), (130.0, 10.0), 40, 14.0, 0.7), [0.9, 0.6, 0.1]);

    let folder_id = doc.alloc_id();
    let mut folder = Layer::folder(folder_id, "Folder");
    folder.opacity = 0.6;
    folder.blend = BlendMode::Overlay;
    if let LayerKind::Folder(f) = &mut folder.kind {
        f.pass_through = false;
        let mut screen = Layer::raster(doc.alloc_id(), "Screen");
        screen.blend = BlendMode::Screen;
        paint(
            screen.tiles_mut().unwrap(),
            &line((75.0, 5.0), (75.0, 85.0), 30, 20.0, 0.5),
            [0.8, 0.2, 0.5],
            &BrushSettings::airbrush(),
            canvas,
        );
        f.children.push(screen);
    }
    doc.add_layer(folder);
    doc.active = base;
    doc
}

fn mask_with(dabs: &[Dab], canvas: (u32, u32)) -> Mask {
    let mut mask = Mask { enabled: true, ..Mask::default() };
    paint(&mut mask.tiles, dabs, [1.0; 3], &BrushSettings::airbrush(), canvas);
    mask
}

/// Every layer feature at once: gradient, masked tone, clipping, a masked
/// pass-through folder (forced isolated) and a vector layer.
fn feature_document() -> Document {
    let mut doc = Document::new(160, 100);
    let canvas = doc.canvas();
    let gradient = Gradient {
        start: [80.0, 50.0],
        end: [150.0, 90.0],
        from: [250, 200, 40, 255],
        to: [20, 60, 200, 120],
        shape: GradientShape::Radial,
    };
    let g = doc.alloc_id();
    doc.add_layer(Layer::gradient(g, "Gradient", gradient));

    let base = add_raster(&mut doc, "Base", BlendMode::Normal, &line((10.0, 50.0), (150.0, 50.0), 50, 18.0, 1.0), [0.2, 0.5, 0.3]);
    let clipped = add_raster(&mut doc, "Shade", BlendMode::Multiply, &line((40.0, 0.0), (40.0, 100.0), 30, 25.0, 1.0), [0.4, 0.2, 0.6]);
    doc.find_mut(clipped).unwrap().clip = true;

    let tone_id = doc.alloc_id();
    let mut tone = Layer::tone(tone_id, "Tone", Tone { color: [30, 30, 30], cell: 7.0, angle: 30.0, density: 0.35 });
    tone.mask = Some(mask_with(&line((0.0, 20.0), (160.0, 20.0), 40, 15.0, 1.0), canvas));
    doc.add_layer(tone);

    let folder_id = doc.alloc_id();
    let mut folder = Layer::folder(folder_id, "Masked folder");
    folder.mask = Some(mask_with(&line((120.0, 0.0), (120.0, 100.0), 30, 12.0, 0.8), canvas));
    if let LayerKind::Folder(f) = &mut folder.kind {
        let mut inner = Layer::raster(doc.alloc_id(), "Inner screen");
        inner.blend = BlendMode::Screen;
        paint(inner.tiles_mut().unwrap(), &line((100.0, 10.0), (150.0, 90.0), 30, 16.0, 0.9), [0.9, 0.3, 0.3], &BrushSettings::g_pen(), canvas);
        f.children.push(inner);
    }
    doc.add_layer(folder);

    let vector_id = doc.alloc_id();
    let mut vector_layer = Layer::vector(vector_id, "Ink");
    if let LayerKind::Vector(v) = &mut vector_layer.kind {
        v.strokes.push(VectorStroke {
            points: (0..30).map(|i| StrokePoint::new(10.0 + i as f32 * 5.0, 85.0 - i as f32, 0.5 + i as f32 / 60.0)).collect(),
            settings: BrushSettings::g_pen(),
            color: [0.05, 0.05, 0.1],
            seed: 3,
        });
        vector::rasterize(v, canvas);
    }
    doc.add_layer(vector_layer);
    doc.active = base;
    doc
}

fn check_composite(doc: &Document) {
    check_composite_named(doc, "composite");
}

fn check_composite_named(doc: &Document, what: &str) {
    let Some((device, queue, _turn)) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let mut canvas = GpuCanvas::new(device, wgpu::TextureFormat::Rgba8Unorm);
    run(device, queue, &mut canvas, &frame(doc, true, None));
    let gpu = canvas.read_composite(device, queue).unwrap();
    assert_close(&gpu, &io::flatten(doc), what);
}

#[test]
fn composite_matches_cpu_flatten() {
    check_composite(&sample_document());
}

#[test]
fn layer_features_match_cpu_flatten() {
    check_composite(&feature_document());
}

/// Every correction kind over the feature document: one on top with a mask
/// and half opacity, one clipped to a layer, one inside an isolated folder.
#[test]
fn correction_layers_match_cpu_flatten() {
    for adjustment in painter_core::Adjustment::defaults() {
        let mut doc = feature_document();
        let canvas = doc.canvas();
        let base = doc.active;
        let top = doc.alloc_id();
        let mut layer = Layer::adjustment(top, adjustment.clone());
        layer.opacity = 0.6;
        layer.mask = Some(mask_with(&line((0.0, 60.0), (160.0, 60.0), 40, 14.0, 1.0), canvas));
        doc.add_layer(layer);
        // Clipped to the base stroke, right above it.
        let clipped = doc.alloc_id();
        let mut c = Layer::adjustment(clipped, adjustment.clone());
        c.clip = true;
        let index = doc.layers.iter().position(|l| l.id == base).unwrap();
        doc.layers.insert(index + 1, c);
        if matches!(adjustment, painter_core::Adjustment::Posterize { .. } | painter_core::Adjustment::Threshold { .. }) {
            // Steps turn the backdrop's 1-level rounding differences into
            // jumps on the odd pixel; only a handful may differ.
            let Some((device, queue, _turn)) = gpu() else { return };
            let mut canvas = GpuCanvas::new(device, wgpu::TextureFormat::Rgba8Unorm);
            run(device, queue, &mut canvas, &frame(&doc, true, None));
            let gpu = canvas.read_composite(device, queue).unwrap();
            let cpu = io::flatten(&doc);
            let off = gpu.as_chunks::<4>().0.iter().zip(cpu.as_chunks::<4>().0).filter(|(a, b)| (0..4).any(|i| a[i].abs_diff(b[i]) > TOLERANCE)).count();
            let total = gpu.len() / 4;
            assert!(off * 200 < total, "{}: {off} of {total} pixels differ", adjustment.name());
        } else {
            check_composite_named(&doc, adjustment.name());
        }
    }
    // Non-default settings, so the tables aren't near identity.
    let mut doc = sample_document();
    let id = doc.alloc_id();
    doc.add_layer(Layer::adjustment(id, painter_core::Adjustment::HueSaturation { hue: 95.0, saturation: 0.6, lightness: -0.2 }));
    let id = doc.alloc_id();
    doc.add_layer(Layer::adjustment(
        id,
        painter_core::Adjustment::Levels { input_black: 0.1, input_white: 0.7, gamma: 1.6, output_black: 0.05, output_white: 0.9 },
    ));
    check_composite(&doc);
}

#[test]
fn atlas_grows_past_one_page() {
    // 2560² px = 1600 tiles > 1024 per atlas page.
    let mut doc = Document::new(2560, 2560);
    let base = doc.active;
    let mut tiles = TileStore::new();
    for y in 0..40 {
        for x in 0..40 {
            let coord = painter_core::TileCoord::new(x, y);
            let shade = ((x * 6 + y * 3) % 255) as u8;
            tiles.get_or_create_mut(coord).set_pixel(5, 5, [shade, 0, 255 - shade, 255]);
        }
    }
    *doc.find_mut(base).unwrap().tiles_mut().unwrap() = tiles;
    check_composite(&doc);
}

#[derive(Default)]
struct StrokeCase {
    alpha_locked: bool,
    /// Paint the active layer's mask instead of its pixels.
    edit_mask: bool,
    /// On the mask, paint this grey (the amount hidden) instead of hiding fully.
    mask_grey: Option<f32>,
    mid_stroke: Option<fn(&mut Document)>,
    /// Per-dab rotation step in degrees (rotation dynamics).
    angle_step: f32,
    /// Thin the tip dab by dab from 1 down to 0.2 (thickness dynamics).
    thinning: bool,
    /// Mirror every third dab (random flips).
    mirroring: bool,
}

/// Renders a stroke as a two-frame GPU preview and as a CPU commit, checks they
/// agree, and returns how many blend passes the second (steady-state) frame took.
/// `mid_stroke` edits the document between the frames (forcing a cache rebuild).
fn check_stroke(mut doc: Document, settings: BrushSettings, case: StrokeCase) -> Option<usize> {
    let Some((device, queue, _turn)) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return None;
    };
    let canvas_size = doc.canvas();
    let layer_id = doc.active;
    doc.find_mut(layer_id).unwrap().alpha_locked = case.alpha_locked;
    let color = if case.edit_mask { [case.mask_grey.unwrap_or(1.0); 3] } else { [0.95, 0.2, 0.1] };
    let plane = if case.edit_mask { Plane::Mask(layer_id) } else { Plane::Color(layer_id) };
    let dabs: Vec<Dab> = line((5.0, 45.0), (145.0, 50.0), 120, settings.size * 0.5, settings.density)
        .into_iter()
        .enumerate()
        .map(|(i, d)| Dab {
            radius: d.radius * (0.3 + 0.7 * (i as f32 / 119.0)),
            angle: i as f32 * case.angle_step,
            // Starts at exactly 1 (a disc), then ellipses: the disc/ellipse choice is per dab.
            thickness: if case.thinning { 1.0 - 0.8 * (i as f32 / 119.0) } else { 1.0 },
            mirror: case.mirroring && i % 3 == 0,
            ..d
        })
        .collect();

    // GPU: preview over two frames, as dabs arrive while drawing.
    let mut canvas = GpuCanvas::new(device, wgpu::TextureFormat::Rgba8Unorm);
    let style =
        DabStyle::from_settings(color, &settings);
    let alpha_locked = case.alpha_locked && !case.edit_mask;
    let preview = |dabs: &[Dab]| StrokePreview {
        id: 7,
        plane,
        dabs: dabs.to_vec(),
        style: style.clone(),
        opacity: settings.opacity,
        erase: settings.mode == BrushMode::Erase,
        blend: settings.blend.shader_id(),
        alpha_locked,
        respect_selection: true,
    };
    let (first, second) = dabs.split_at(dabs.len() / 2);
    run(device, queue, &mut canvas, &frame(&doc, true, Some(preview(first))));
    let mut next = frame(&doc, false, Some(preview(second)));
    next.recomposite = false;
    if let Some(edit) = case.mid_stroke {
        edit(&mut doc);
        next = frame(&doc, false, Some(preview(second)));
    }
    run(device, queue, &mut canvas, &next);
    let passes = canvas.last_composite_passes();
    let gpu = canvas.read_composite(device, queue).unwrap();

    // CPU: stamp into a stroke buffer and merge into the plane, as on pen-up.
    let selection = doc.selection.clone();
    let layer = doc.find_mut(layer_id).unwrap();
    let tiles = if case.edit_mask { &mut layer.mask.as_mut().unwrap().tiles } else { layer.tiles_mut().unwrap() };
    paint_stroke(tiles, &dabs, color, &settings, canvas_size, alpha_locked, selection.as_ref());
    if case.edit_mask {
        // As the app commits mask edits: the grey settles into every channel.
        Mask::settle_all(tiles);
    }
    let cpu = io::flatten(&doc);

    assert_close(&gpu, &cpu, &settings.name);
    Some(passes)
}

fn pen() -> BrushSettings {
    BrushSettings { opacity: 0.8, ..BrushSettings::g_pen() }
}

/// Paper, two layers below the active one, and Normal layers (plus a Normal
/// isolated folder) above it: the whole stack collapses to below ⊕ preview ⊕ above.
fn normal_stack_document() -> Document {
    let mut doc = Document::new(150, 90);
    let canvas = doc.canvas();
    let soft = BrushSettings::airbrush();
    let first = doc.active;
    paint(doc.find_mut(first).unwrap().tiles_mut().unwrap(), &line((0.0, 10.0), (150.0, 80.0), 40, 25.0, 0.4), [0.2, 0.7, 0.3], &soft, canvas);
    add_raster(&mut doc, "Below 2", BlendMode::Normal, &line((0.0, 80.0), (150.0, 10.0), 40, 12.0, 1.0), [0.1, 0.1, 0.6]);
    let active = doc.alloc_id();
    doc.add_layer(Layer::raster(active, "Active"));
    for (i, color) in [[0.9, 0.9, 0.1], [0.6, 0.1, 0.8]].into_iter().enumerate() {
        let x = 40.0 + i as f32 * 50.0;
        let id = add_raster(&mut doc, &format!("Above {i}"), BlendMode::Normal, &line((x, 0.0), (x, 90.0), 30, 10.0, 1.0), color);
        doc.find_mut(id).unwrap().opacity = 0.7;
    }
    let folder_id = doc.alloc_id();
    let mut folder = Layer::folder(folder_id, "Normal folder");
    if let LayerKind::Folder(f) = &mut folder.kind {
        f.pass_through = false;
        let mut inner = Layer::raster(doc.alloc_id(), "Inner multiply");
        inner.blend = BlendMode::Multiply;
        paint(inner.tiles_mut().unwrap(), &line((0.0, 45.0), (150.0, 45.0), 30, 15.0, 0.8), [0.9, 0.5, 0.5], &soft, canvas);
        f.children.push(inner);
    }
    doc.add_layer(folder);
    doc.active = active;
    doc
}

/// The active layer sits inside an isolated Multiply folder, under a Screen layer.
fn nested_document() -> Document {
    let mut doc = Document::new(150, 90);
    let canvas = doc.canvas();
    let base = doc.active;
    paint(doc.find_mut(base).unwrap().tiles_mut().unwrap(), &line((0.0, 45.0), (150.0, 45.0), 30, 30.0, 0.6), [0.3, 0.5, 0.9], &BrushSettings::airbrush(), canvas);

    let folder_id = doc.alloc_id();
    let mut folder = Layer::folder(folder_id, "Folder");
    folder.blend = BlendMode::Multiply;
    folder.opacity = 0.7;
    let active = doc.alloc_id();
    if let LayerKind::Folder(f) = &mut folder.kind {
        f.pass_through = false;
        let mut below = Layer::raster(doc.alloc_id(), "Inner below");
        paint(below.tiles_mut().unwrap(), &line((20.0, 0.0), (20.0, 90.0), 20, 12.0, 1.0), [0.9, 0.8, 0.2], &BrushSettings::g_pen(), canvas);
        f.children.push(below);
        f.children.push(Layer::raster(active, "Active"));
        let mut above = Layer::raster(doc.alloc_id(), "Inner above");
        paint(above.tiles_mut().unwrap(), &line((120.0, 0.0), (120.0, 90.0), 20, 12.0, 1.0), [0.2, 0.9, 0.4], &BrushSettings::g_pen(), canvas);
        f.children.push(above);
    }
    doc.add_layer(folder);

    let screen_id = doc.alloc_id();
    let mut screen = Layer::raster(screen_id, "Screen");
    screen.blend = BlendMode::Screen;
    paint(screen.tiles_mut().unwrap(), &line((0.0, 0.0), (150.0, 90.0), 30, 18.0, 0.6), [0.5, 0.2, 0.2], &BrushSettings::airbrush(), canvas);
    doc.layers.push(screen);
    doc.active = active;
    doc
}

#[test]
fn pen_stroke_preview_matches_commit() {
    check_stroke(sample_document(), pen(), StrokeCase::default());
}

#[test]
fn airbrush_stroke_preview_matches_commit() {
    check_stroke(sample_document(), BrushSettings { size: 40.0, density: 0.15, ..BrushSettings::airbrush() }, StrokeCase::default());
}

#[test]
fn eraser_stroke_preview_matches_commit() {
    check_stroke(sample_document(), BrushSettings { size: 24.0, ..BrushSettings::soft_eraser() }, StrokeCase::default());
}

#[test]
fn alpha_locked_stroke_preview_matches_commit() {
    let case = StrokeCase { alpha_locked: true, ..StrokeCase::default() };
    check_stroke(sample_document(), BrushSettings { size: 30.0, ..BrushSettings::pencil() }, case);
}

#[test]
fn normal_stack_strokes_blend_three_textures() {
    if let Some(passes) = check_stroke(normal_stack_document(), pen(), StrokeCase::default()) {
        // below ⊕ preview, then ⊕ above.
        assert_eq!(passes, 2);
    }
}

#[test]
fn non_normal_layers_above_are_reblended() {
    // sample_document: Multiply layer and an isolated Overlay folder (one child) above.
    if let Some(passes) = check_stroke(sample_document(), pen(), StrokeCase::default()) {
        assert_eq!(passes, 1 + 1 + 2);
    }
}

#[test]
fn nested_folder_strokes_use_per_level_caches() {
    if let Some(passes) = check_stroke(nested_document(), pen(), StrokeCase::default()) {
        // Inner: below ⊕ preview ⊕ above-cache. Root: below ⊕ folder, then the Screen layer.
        assert_eq!(passes, 2 + 2);
    }
}

#[test]
fn mid_stroke_layer_edit_rebuilds_caches() {
    fn hide_top_layer(doc: &mut Document) {
        let top = doc.rows_top_down()[0].0;
        doc.find_mut(top).unwrap().visible = false;
    }
    fn fade_folder(doc: &mut Document) {
        let folder = doc.rows_top_down()[1].0;
        doc.find_mut(folder).unwrap().opacity = 0.3;
    }
    check_stroke(normal_stack_document(), pen(), StrokeCase { mid_stroke: Some(hide_top_layer), ..StrokeCase::default() });
    check_stroke(nested_document(), pen(), StrokeCase { mid_stroke: Some(fade_folder), ..StrokeCase::default() });
}

#[test]
fn painting_a_clipping_base_updates_clipped_layers_live() {
    // feature_document's active layer is the base of a clipped Multiply layer.
    if let Some(passes) = check_stroke(feature_document(), pen(), StrokeCase::default()) {
        // The clipped layer above depends on the base: nothing above may be pre-flattened.
        assert!(passes > 2, "{passes}");
    }
}

#[test]
fn mask_stroke_preview_matches_commit() {
    let mut doc = feature_document();
    let base = doc.active;
    doc.find_mut(base).unwrap().mask = Some(Mask { enabled: true, ..Mask::default() });
    let hide = BrushSettings { size: 30.0, ..BrushSettings::airbrush() };
    check_stroke(doc.clone(), hide, StrokeCase { edit_mask: true, ..StrokeCase::default() });
    // Revealing again with the eraser.
    let reveal = BrushSettings { size: 20.0, ..BrushSettings::hard_eraser() };
    check_stroke(doc, reveal, StrokeCase { edit_mask: true, ..StrokeCase::default() });
}

#[test]
fn grey_mask_stroke_preview_matches_commit() {
    // Photoshop's mask painting: grey blends the mask towards it, over hidden
    // and shown parts alike (a light grey reveals part of what was hidden).
    let mut doc = feature_document();
    let base = doc.active;
    let canvas = doc.canvas();
    let hidden = selection::rect_mask([0.0, 0.0], [75.0, 90.0], canvas);
    doc.find_mut(base).unwrap().mask = Some(Mask { tiles: hidden, enabled: true });
    for grey in [0.25, 0.7] {
        let brush = BrushSettings { size: 30.0, ..BrushSettings::airbrush() };
        check_stroke(doc.clone(), brush, StrokeCase { edit_mask: true, mask_grey: Some(grey), ..StrokeCase::default() });
    }
}

/// An asymmetric tip with soft and hard edges, holes and a gradient, so any
/// mismatch in scale, rotation, flattening or filtering shows.
fn sampled_tip(width: u32, height: u32) -> TipShape {
    let alpha = (0..height)
        .flat_map(|y| {
            (0..width).map(move |x| {
                let (u, v) = (x as f32 / width as f32 - 0.4, y as f32 / height as f32 - 0.5);
                let blob = (1.0 - (u * u * 4.0 + v * v * 6.0).sqrt()).clamp(0.0, 1.0);
                let grain = if (x * 7 + y * 13) % 11 < 3 { 0.3 } else { 1.0 };
                (blob * grain * 255.0) as u8
            })
        })
        .collect();
    TipShape::Sampled(Arc::new(TipBitmap::new(width, height, alpha).unwrap()))
}

#[test]
fn sampled_tip_stroke_preview_matches_commit() {
    // Magnified, rotated and flattened.
    let tip = DabTip { shape: sampled_tip(23, 17), thickness: 0.7, angle: 25.0 };
    check_stroke(sample_document(), BrushSettings { size: 36.0, density: 0.6, tip, ..pen() }, StrokeCase::default());
    // Minified across several mip levels as the radius grows along the stroke.
    let tip = DabTip { shape: sampled_tip(300, 180), thickness: 1.0, angle: -70.0 };
    check_stroke(sample_document(), BrushSettings { size: 24.0, density: 0.4, tip, ..pen() }, StrokeCase::default());
}

#[test]
fn sampled_tip_eraser_and_mask_match_commit() {
    let tip = DabTip { shape: sampled_tip(64, 64), thickness: 0.5, angle: 140.0 };
    let eraser = BrushSettings { size: 30.0, tip: tip.clone(), ..BrushSettings::hard_eraser() };
    check_stroke(sample_document(), eraser, StrokeCase::default());
    let mut doc = feature_document();
    let base = doc.active;
    doc.find_mut(base).unwrap().mask = Some(Mask { enabled: true, ..Mask::default() });
    check_stroke(doc, BrushSettings { size: 30.0, tip, ..BrushSettings::airbrush() }, StrokeCase { edit_mask: true, ..StrokeCase::default() });
}

#[test]
fn flat_rotated_round_tip_preview_matches_commit() {
    let tip = DabTip { thickness: 0.2, angle: 35.0, ..DabTip::default() };
    check_stroke(sample_document(), BrushSettings { size: 30.0, tip: tip.clone(), ..pen() }, StrokeCase::default());
    // Soft falloff and a wide anti-aliasing ramp.
    let soft = BrushSettings { size: 40.0, density: 0.3, tip: DabTip { angle: -60.0, ..tip.clone() }, ..BrushSettings::airbrush() };
    check_stroke(sample_document(), soft, StrokeCase::default());
    // No anti-aliasing: a hard edge.
    let aliased = BrushSettings { size: 26.0, antialias: AntiAlias::None, tip, ..pen() };
    check_stroke(sample_document(), aliased, StrokeCase::default());
}

#[test]
fn selection_limits_stroke_preview() {
    let mut doc = sample_document();
    let canvas = doc.canvas();
    doc.selection = Some(selection::ellipse_mask([30.0, 20.0], [120.0, 80.0], canvas));
    check_stroke(doc.clone(), pen(), StrokeCase::default());
    // An empty selection protects everything.
    doc.selection = Some(TileStore::new());
    check_stroke(doc, pen(), StrokeCase::default());
}

fn bitmap(width: u32, height: u32, seed: u32) -> Arc<TipBitmap> {
    let alpha = (0..width * height).map(|i| ((i * 97 + seed * 31) % 256) as u8).collect();
    Arc::new(TipBitmap::new(width, height, alpha).unwrap())
}

#[test]
fn textured_stroke_preview_matches_commit() {
    // Non-integer scale exercises wrapped bilinear sampling across texel seams.
    let texture = DabTexture { bitmap: bitmap(7, 5, 1), scale: 2.5, density: 0.8, invert: false };
    check_stroke(sample_document(), BrushSettings { size: 30.0, texture: Some(texture.clone()), ..pen() }, StrokeCase::default());
    let inverted = DabTexture { invert: true, scale: 0.6, ..texture };
    let airbrush = BrushSettings { size: 40.0, density: 0.2, texture: Some(inverted), ..BrushSettings::airbrush() };
    check_stroke(sample_document(), airbrush, StrokeCase::default());
}

#[test]
fn dual_tip_stroke_preview_matches_commit() {
    let dual = DualTip { bitmap: bitmap(13, 9, 2), scale: 1.4 };
    check_stroke(sample_document(), BrushSettings { size: 34.0, dual: Some(dual.clone()), ..pen() }, StrokeCase::default());
    // Everything together: sampled, rotated main tip + large dual tip (mips) + texture.
    let all = BrushSettings {
        size: 28.0,
        tip: DabTip { shape: sampled_tip(23, 17), thickness: 0.8, angle: 60.0 },
        dual: Some(DualTip { bitmap: bitmap(200, 150, 3), scale: 0.7 }),
        texture: Some(DabTexture { bitmap: bitmap(16, 16, 4), scale: 1.0, density: 0.5, invert: false }),
        ..pen()
    };
    check_stroke(sample_document(), all, StrokeCase::default());
}

#[test]
fn per_dab_rotation_preview_matches_commit() {
    let flat_sampled = DabTip { shape: sampled_tip(23, 17), thickness: 0.6, angle: 15.0 };
    let case = StrokeCase { angle_step: 7.3, ..StrokeCase::default() };
    check_stroke(sample_document(), BrushSettings { size: 32.0, tip: flat_sampled, ..pen() }, case);
    let flat_round = DabTip { thickness: 0.3, ..DabTip::default() };
    let case = StrokeCase { angle_step: -11.0, ..StrokeCase::default() };
    check_stroke(sample_document(), BrushSettings { size: 28.0, tip: flat_round, ..pen() }, case);
}

#[test]
fn per_dab_thickness_preview_matches_commit() {
    let case = StrokeCase { thinning: true, angle_step: 3.0, ..StrokeCase::default() };
    check_stroke(sample_document(), BrushSettings { size: 30.0, ..pen() }, case);
    let sampled = DabTip { shape: sampled_tip(23, 17), thickness: 0.9, angle: 0.0 };
    let case = StrokeCase { thinning: true, ..StrokeCase::default() };
    check_stroke(sample_document(), BrushSettings { size: 30.0, tip: sampled, ..pen() }, case);
}

#[test]
fn mirrored_dabs_preview_matches_commit() {
    // An asymmetric tip, so a mirrored dab differs from a turned one.
    let sampled = DabTip { shape: sampled_tip(23, 17), thickness: 0.8, angle: 20.0 };
    let case = StrokeCase { mirroring: true, angle_step: 5.0, ..StrokeCase::default() };
    check_stroke(sample_document(), BrushSettings { size: 30.0, tip: sampled, ..pen() }, case);
}

#[test]
fn uniform_mask_tiles_share_one_atlas_slot() {
    let Some((device, queue, _turn)) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    // A 640 × 640 canvas: 100 tiles. A comic-panel style mask hides everything
    // but a small polygon, so nearly every mask tile is the same full tile.
    let mut doc = Document::new(640, 640);
    let red: Vec<u8> = (0..640 * 640).flat_map(|_| [200, 30, 30, 255]).collect();
    let id = doc.active;
    let layer = doc.find_mut(id).unwrap();
    layer.kind = LayerKind::Raster(TileStore::from_rgba(640, 640, &red));
    let inside = selection::polygon_mask(&[[100.0, 100.0], [300.0, 120.0], [260.0, 330.0], [90.0, 280.0]], (640, 640));
    layer.mask = Some(Mask { tiles: selection::invert(Some(&inside), (640, 640)), enabled: true });
    let mut canvas = GpuCanvas::new(device, wgpu::TextureFormat::Rgba8Unorm);
    run(device, queue, &mut canvas, &frame(&doc, true, None));
    let gpu = canvas.read_composite(device, queue).unwrap();
    assert_close(&gpu, &io::flatten(&doc), "masked layer");
    // 100 colour tiles share one slot too (solid red), plus the mask's edge tiles.
    let (slots, _) = canvas.atlas_usage();
    assert!(slots < 30, "{slots} slots in use");
}

/// Every pixel says where it is (red = x, green = y, blue = which tile), so
/// a wrong composite shows exactly which texel each pixel read.
#[test]
fn tiles_read_their_own_texels() {
    let Some((device, queue, _turn)) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let (w, h) = (192u32, 128u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[(x % 256) as u8, (y % 256) as u8, (40 * (x / 64 + 3 * (y / 64))) as u8, 255]);
        }
    }
    let mut doc = Document::from_parts(w, h, Vec::new(), LayerId(0), 1);
    let id = doc.alloc_id();
    doc.layers.push(Layer::raster_with(id, "Probe", TileStore::from_rgba(w, h, &rgba)));
    doc.active = id;
    let mut canvas = GpuCanvas::new(device, wgpu::TextureFormat::Rgba8Unorm);
    run(device, queue, &mut canvas, &frame(&doc, true, None));
    let gpu = canvas.read_composite(device, queue).unwrap();
    let cpu = io::flatten(&doc);
    let mut report = String::new();
    let mut bad = 0;
    for (tx, ty) in [(0, 0), (1, 0), (2, 0), (0, 1), (1, 1), (2, 1)] {
        for (dx, dy) in [(0, 0), (1, 0), (63, 0), (0, 1), (5, 9), (32, 32), (63, 63)] {
            let (x, y) = (tx * 64 + dx, ty * 64 + dy);
            let i = ((y * w + x) * 4) as usize;
            let (g, c) = (&gpu[i..i + 4], &cpu[i..i + 4]);
            if (0..4).any(|k| g[k].abs_diff(c[k]) > TOLERANCE) {
                bad += 1;
            }
            report += &format!("tile ({tx},{ty}) at ({x},{y}): gpu {g:?} cpu {c:?}\n");
        }
    }
    eprintln!("{report}");
    assert_eq!(bad, 0, "{bad} probes read the wrong texel:\n{report}");
}

/// Layer colour: black lines show in the colour on screen, and only there.
#[test]
fn layer_colour_shows_black_lines_in_its_colour() {
    let Some((device, queue, _turn)) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let dabs = line((10.0, 20.0), (140.0, 70.0), 40, 6.0, 1.0);
    let tint = [0.0, 150.0 / 255.0, 1.0];
    let mut shown = Document::new(150, 90);
    let id = add_raster(&mut shown, "Inks", BlendMode::Normal, &dabs, [0.0; 3]);
    shown.find_mut(id).unwrap().layer_color = Some([0, 150, 255]);
    // What the screen should look like: the same line drawn in the colour.
    let mut expected = Document::new(150, 90);
    add_raster(&mut expected, "Inks", BlendMode::Normal, &dabs, tint);
    let mut canvas = GpuCanvas::new(device, wgpu::TextureFormat::Rgba8Unorm);
    run(device, queue, &mut canvas, &frame(&shown, true, None));
    let gpu = canvas.read_composite(device, queue).unwrap();
    assert_close(&gpu, &io::flatten(&expected), "layer colour");
    // Exports keep black.
    let mut black = Document::new(150, 90);
    add_raster(&mut black, "Inks", BlendMode::Normal, &dabs, [0.0; 3]);
    assert_eq!(io::flatten(&shown), io::flatten(&black));
}

/// A canvas larger than the low-VRAM margins, so the composite is cropped to
/// the view and stroke targets to the stroke: a gradient, a Multiply layer, the
/// active layer with a Multiply layer clipped to it, a masked layer and an
/// isolated Overlay folder.
fn large_document() -> Document {
    let mut doc = Document::new(1024, 704);
    let canvas = doc.canvas();
    let base = doc.active;
    let gradient = Gradient { start: [100.0, 80.0], end: [900.0, 600.0], from: [240, 230, 200, 255], to: [60, 90, 160, 255], shape: GradientShape::Linear };
    *doc.find_mut(base).unwrap() = Layer::gradient(base, "Paper", gradient);
    add_raster(&mut doc, "Multiply", BlendMode::Multiply, &line((0.0, 650.0), (1024.0, 50.0), 80, 40.0, 0.7), [0.9, 0.6, 0.1]);
    let active = add_raster(&mut doc, "Active", BlendMode::Normal, &line((200.0, 100.0), (800.0, 600.0), 80, 30.0, 1.0), [0.2, 0.5, 0.3]);
    let clipped = add_raster(&mut doc, "Shade", BlendMode::Multiply, &line((500.0, 0.0), (500.0, 704.0), 60, 60.0, 1.0), [0.4, 0.2, 0.6]);
    doc.find_mut(clipped).unwrap().clip = true;
    let masked = add_raster(&mut doc, "Masked", BlendMode::Screen, &line((0.0, 200.0), (1024.0, 500.0), 60, 70.0, 0.8), [0.1, 0.4, 0.9]);
    doc.find_mut(masked).unwrap().mask = Some(mask_with(&line((0.0, 300.0), (1024.0, 380.0), 60, 50.0, 1.0), canvas));
    let folder_id = doc.alloc_id();
    let mut folder = Layer::folder(folder_id, "Folder");
    folder.blend = BlendMode::Overlay;
    folder.opacity = 0.8;
    if let LayerKind::Folder(f) = &mut folder.kind {
        f.pass_through = false;
        let mut screen = Layer::raster(doc.alloc_id(), "Screen");
        screen.blend = BlendMode::Screen;
        paint(screen.tiles_mut().unwrap(), &line((300.0, 650.0), (700.0, 50.0), 50, 45.0, 0.6), [0.8, 0.2, 0.5], &BrushSettings::airbrush(), canvas);
        f.children.push(screen);
    }
    doc.add_layer(folder);
    doc.active = active;
    doc
}

/// Zoomed in, panned and rotated: the widget shows a few hundred canvas pixels
/// around (430, 300), so the composite region is a fraction of the canvas.
fn zoomed_view(doc: &Document) -> ViewUniform {
    ViewUniform::new([320.0, 200.0], [doc.width as f32, doc.height as f32], [160.0, 120.0], 2.0, 0.35, 8.0)
}

/// Pixels of `rgba` (a `width`-wide canvas) inside the rectangle `(x, y, w, h)`.
fn crop(rgba: &[u8], width: u32, (x, y, w, h): (u32, u32, u32, u32)) -> Vec<u8> {
    (y..y + h)
        .flat_map(|row| {
            let start = ((row * width + x) * 4) as usize;
            rgba[start..start + (w * 4) as usize].iter().copied()
        })
        .collect()
}

/// Checks a low-VRAM region is a real crop of the canvas.
fn assert_cropped(region: (u32, u32, u32, u32), doc: &Document) {
    let (_, _, w, h) = region;
    assert!(w > 0 && h > 0 && (w < doc.width || h < doc.height), "region {region:?} of {}x{}", doc.width, doc.height);
}

/// Viewport-constrained compositing gives exactly the full-canvas composite
/// inside the visible region, with a fraction of the intermediate memory.
#[test]
fn viewport_composite_matches_full_composite() {
    let Some((device, queue, _turn)) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let mut doc = large_document();
    let mut full = GpuCanvas::new(device, wgpu::TextureFormat::Rgba8Unorm);
    run(device, queue, &mut full, &frame(&doc, true, None));
    assert_close(&full.read_composite(device, queue).unwrap(), &io::flatten(&doc), "full composite");
    // A rotated tone, fixed to the canvas, must line up in the cropped composite too.
    // (Only GPU against GPU: the tone's dot edges drift a few levels from the CPU's
    // at large canvas coordinates, with or without cropping.)
    let tone_id = doc.alloc_id();
    doc.add_layer(Layer::tone(tone_id, "Tone", Tone { color: [30, 30, 30], cell: 9.0, angle: 30.0, density: 0.3 }));
    run(device, queue, &mut full, &frame(&doc, true, None));
    let reference = full.read_composite(device, queue).unwrap();

    let mut low = GpuCanvas::with_profile(device, wgpu::TextureFormat::Rgba8Unorm, RenderProfile::low_vram());
    let mut zoomed = frame(&doc, true, None);
    zoomed.view = zoomed_view(&doc);
    run(device, queue, &mut low, &zoomed);
    let region = low.composite_region();
    assert_cropped(region, &doc);
    let cropped = low.read_composite(device, queue).unwrap();
    assert_eq!(crop(&cropped, doc.width, region), crop(&reference, doc.width, region), "cropped composite");
    assert!(
        low.intermediate_memory_bytes() * 2 < full.intermediate_memory_bytes(),
        "{} vs {} bytes",
        low.intermediate_memory_bytes(),
        full.intermediate_memory_bytes()
    );

    // Panning to the far corner moves the region and recomposites there.
    let mut panned = frame(&doc, false, None);
    panned.recomposite = false;
    panned.view = ViewUniform::new([320.0, 200.0], [doc.width as f32, doc.height as f32], [-900.0, -600.0], 2.0, 0.0, 8.0);
    run(device, queue, &mut low, &panned);
    let moved = low.composite_region();
    assert_ne!(moved, region);
    assert_cropped(moved, &doc);
    let cropped = low.read_composite(device, queue).unwrap();
    assert_eq!(crop(&cropped, doc.width, moved), crop(&reference, doc.width, moved), "panned composite");
}

#[derive(Clone, Copy)]
struct LowVramCase {
    edit_mask: bool,
    alpha_locked: bool,
    selection: bool,
}

/// Paints a stroke that spreads over several frames (growing its bounding-box
/// target) with the full and the low-VRAM profile; inside the visible region
/// both previews must be identical and match the CPU commit.
fn check_low_vram_stroke(settings: BrushSettings, case: LowVramCase, profile: RenderProfile) {
    let Some((device, queue, _turn)) = gpu() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let mut doc = large_document();
    let canvas_size = doc.canvas();
    let layer_id = doc.active;
    doc.find_mut(layer_id).unwrap().alpha_locked = case.alpha_locked;
    if case.edit_mask {
        doc.find_mut(layer_id).unwrap().mask = Some(Mask { enabled: true, ..Mask::default() });
    }
    if case.selection {
        doc.selection = Some(selection::ellipse_mask([250.0, 150.0], [650.0, 500.0], canvas_size));
    }
    let color = if case.edit_mask { [1.0; 3] } else { [0.95, 0.2, 0.1] };
    let plane = if case.edit_mask { Plane::Mask(layer_id) } else { Plane::Color(layer_id) };
    let alpha_locked = case.alpha_locked && !case.edit_mask;
    // Starts inside the view, then leaves it.
    let dabs: Vec<Dab> = line((380.0, 260.0), (900.0, 640.0), 240, settings.size * 0.5, settings.density)
        .into_iter()
        .enumerate()
        .map(|(i, d)| Dab { angle: i as f32 * 4.0, mirror: i % 5 == 0, ..d })
        .collect();
    let style = DabStyle::from_settings(color, &settings);
    let preview = |dabs: &[Dab]| StrokePreview {
        id: 11,
        plane,
        dabs: dabs.to_vec(),
        style: style.clone(),
        opacity: settings.opacity,
        erase: settings.mode == BrushMode::Erase,
        blend: settings.blend.shader_id(),
        alpha_locked,
        respect_selection: true,
    };
    let render = |canvas: &mut GpuCanvas| {
        for (i, chunk) in dabs.chunks(dabs.len() / 4).enumerate() {
            let mut f = frame(&doc, i == 0, Some(preview(chunk)));
            f.recomposite = i == 0;
            f.view = zoomed_view(&doc);
            run(device, queue, canvas, &f);
        }
        canvas.read_composite(device, queue).unwrap()
    };
    let mut full = GpuCanvas::new(device, wgpu::TextureFormat::Rgba8Unorm);
    let reference = render(&mut full);
    let mut low = GpuCanvas::with_profile(device, wgpu::TextureFormat::Rgba8Unorm, profile);
    let cropped = render(&mut low);
    let region = low.composite_region();
    assert_cropped(region, &doc);
    let what = format!("{} ({profile:?})", settings.name);
    let (gpu_low, gpu_full) = (crop(&cropped, doc.width, region), crop(&reference, doc.width, region));
    assert!(low.intermediate_memory_bytes() < full.intermediate_memory_bytes(), "{what}");
    if profile.low_precision_strokes {
        // The 8-bit stroke target is a preview approximation: close, not exact.
        let max = gpu_low.iter().zip(&gpu_full).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
        let mean = gpu_low.iter().zip(&gpu_full).map(|(a, b)| a.abs_diff(*b) as f64).sum::<f64>() / gpu_low.len() as f64;
        assert!(max <= 8 && mean < 0.1, "{what}: 8-bit preview off by up to {max} (mean {mean:.4})");
        return;
    }
    assert_eq!(gpu_low, gpu_full, "{what}: low-VRAM preview differs from the full-canvas one");

    let selection = doc.selection.clone();
    let layer = doc.find_mut(layer_id).unwrap();
    let tiles = if case.edit_mask { &mut layer.mask.as_mut().unwrap().tiles } else { layer.tiles_mut().unwrap() };
    paint_stroke(tiles, &dabs, color, &settings, canvas_size, alpha_locked, selection.as_ref());
    assert_close(&gpu_low, &crop(&io::flatten(&doc), doc.width, region), &what);
}

const PLAIN: LowVramCase = LowVramCase { edit_mask: false, alpha_locked: false, selection: false };

#[test]
fn clipped_stroke_targets_match_full_targets() {
    let low = RenderProfile::low_vram();
    check_low_vram_stroke(BrushSettings { size: 40.0, ..pen() }, PLAIN, low);
    // Canvas-fixed paper texture and a rotated, mirrored sampled tip with a dual tip.
    let textured = BrushSettings {
        size: 50.0,
        tip: DabTip { shape: sampled_tip(23, 17), thickness: 0.8, angle: 30.0 },
        dual: Some(DualTip { bitmap: bitmap(13, 9, 2), scale: 1.4 }),
        texture: Some(DabTexture { bitmap: bitmap(7, 5, 1), scale: 2.5, density: 0.8, invert: false }),
        ..pen()
    };
    check_low_vram_stroke(textured, PLAIN, low);
    check_low_vram_stroke(BrushSettings { size: 60.0, density: 0.2, ..BrushSettings::airbrush() }, LowVramCase { selection: true, ..PLAIN }, low);
    check_low_vram_stroke(BrushSettings { size: 50.0, ..BrushSettings::soft_eraser() }, PLAIN, low);
    check_low_vram_stroke(BrushSettings { size: 40.0, ..BrushSettings::pencil() }, LowVramCase { alpha_locked: true, ..PLAIN }, low);
    check_low_vram_stroke(BrushSettings { size: 50.0, ..BrushSettings::airbrush() }, LowVramCase { edit_mask: true, ..PLAIN }, low);
}

/// Each low-VRAM feature on its own, and the dithered 8-bit stroke target
/// (compared with the full-precision preview, within a looser bound).
#[test]
fn low_vram_profile_options_match_commit() {
    let low = RenderProfile::low_vram();
    check_low_vram_stroke(pen(), PLAIN, RenderProfile { clipped_strokes: false, ..low });
    check_low_vram_stroke(BrushSettings { size: 40.0, ..pen() }, PLAIN, RenderProfile { low_precision_strokes: true, ..low });
    check_low_vram_stroke(
        BrushSettings { size: 50.0, density: 0.3, ..BrushSettings::airbrush() },
        PLAIN,
        RenderProfile { low_precision_strokes: true, ..low },
    );
}

/// The legacy profile (low-VRAM targets in a compact 1024² atlas), with and
/// without 8-bit strokes, as legacy machines run it.
#[test]
fn legacy_profile_matches_commit() {
    let legacy = RenderProfile::legacy();
    check_low_vram_stroke(BrushSettings { size: 40.0, ..pen() }, LowVramCase { selection: true, ..PLAIN }, legacy);
    check_low_vram_stroke(
        BrushSettings { size: 50.0, ..BrushSettings::airbrush() },
        LowVramCase { edit_mask: true, ..PLAIN },
        RenderProfile { low_precision_strokes: true, ..legacy },
    );
}

/// Brush blend modes: the live preview mixes with the layer as the commit does.
#[test]
fn brush_blend_modes_preview_matches_commit() {
    use painter_core::BrushBlend;
    for blend in [BrushBlend::Multiply, BrushBlend::Screen, BrushBlend::Overlay, BrushBlend::Difference, BrushBlend::Behind] {
        let settings = BrushSettings { name: blend.name().into(), size: 30.0, blend, ..pen() };
        check_stroke(sample_document(), settings.clone(), StrokeCase::default());
        check_stroke(sample_document(), settings, StrokeCase { alpha_locked: true, ..StrokeCase::default() });
    }
}

/// Drawing onto a clipped layer whose base is empty triggers cached composite
/// fallback (node_pass returning false) and must not fail wgpu texture copy validation.
#[test]
fn clipped_layer_preview_with_empty_base_does_not_crash() {
    let mut doc = Document::new(160, 100);
    let base = doc.alloc_id();
    doc.add_layer(Layer::raster(base, "Empty Base"));
    let clipped = doc.alloc_id();
    let mut clipped_layer = Layer::raster(clipped, "Clipped");
    clipped_layer.clip = true;
    doc.add_layer(clipped_layer);
    doc.active = clipped;
    check_stroke(doc, BrushSettings { size: 20.0, ..pen() }, StrokeCase::default());
}

#[test]
fn mask_preview_grayscale_and_rubylith_render_accurately() {
    use painter_render::MaskViewMode;
    let Some((device, queue, _turn)) = gpu() else { return };
    let mut doc = Document::new(64, 64);
    let base = doc.alloc_id();
    doc.add_layer(Layer::raster_with(
        base,
        "Base",
        TileStore::from_rgba(64, 64, &vec![255u8; 64 * 64 * 4]),
    ));
    let masked_id = doc.alloc_id();
    let mut masked_layer = Layer::raster_with(
        masked_id,
        "Masked",
        TileStore::from_rgba(64, 64, &vec![200u8; 64 * 64 * 4]),
    );
    // Mask covering left half: hides left half (alpha = 255), reveals right half (alpha = 0)
    let mut mask_rgba = vec![0u8; 64 * 64 * 4];
    for y in 0..64 {
        for x in 0..32 {
            let i = ((y * 64 + x) * 4) as usize;
            mask_rgba[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }
    masked_layer.mask = Some(painter_core::Mask {
        tiles: TileStore::from_rgba(64, 64, &mask_rgba),
        enabled: true,
    });
    doc.add_layer(masked_layer);
    doc.active = masked_id;

    let mut canvas = GpuCanvas::new(device, wgpu::TextureFormat::Rgba8Unorm);

    // 1. Grayscale Mask View
    let mut f_gray = frame(&doc, true, None);
    f_gray.mask_view = Some((masked_id, MaskViewMode::Grayscale));
    run(device, queue, &mut canvas, &f_gray);
    let gray_rgba = canvas.read_composite(device, queue).unwrap();
    // Left half is masked (hidden) -> displayed as BLACK
    let left_pixel = &gray_rgba[(32 * 64 + 10) * 4..][..4];
    assert!(left_pixel[0] < 5 && left_pixel[1] < 5 && left_pixel[2] < 5, "masked area should be black in grayscale view: {left_pixel:?}");
    // Right half is unmasked (revealed) -> displayed as WHITE
    let right_pixel = &gray_rgba[(32 * 64 + 50) * 4..][..4];
    assert!(right_pixel[0] > 250 && right_pixel[1] > 250 && right_pixel[2] > 250, "unmasked area should be white in grayscale view: {right_pixel:?}");

    // 2. Rubylith Mask View
    let mut f_ruby = frame(&doc, true, None);
    f_ruby.mask_view = Some((masked_id, MaskViewMode::Rubylith));
    run(device, queue, &mut canvas, &f_ruby);
    let ruby_rgba = canvas.read_composite(device, queue).unwrap();
    // Left half (masked) has ruby red tint
    let left_ruby = &ruby_rgba[(32 * 64 + 10) * 4..][..4];
    assert!(left_ruby[0] > left_ruby[1] + 50, "left half should have ruby red tint: {left_ruby:?}");
    // Right half (unmasked) should not have ruby tint
    let right_ruby = &ruby_rgba[(32 * 64 + 50) * 4..][..4];
    assert!(right_ruby[0].abs_diff(right_ruby[1]) < 10, "right half should not have ruby tint: {right_ruby:?}");
}

