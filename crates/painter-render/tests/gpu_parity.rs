//! The GPU must show what the CPU will commit: these tests render through
//! `GpuCanvas` on a headless device and compare against `painter-core`.
//! They are skipped (with a note) on machines without a usable GPU adapter.

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use painter_core::raster::paint_stroke;
use painter_core::{
    io, selection, vector, AntiAlias, BlendMode, BrushMode, BrushSettings, Dab, DabTexture, DabTip, Document, DualTip, Gradient,
    GradientShape, Layer, LayerId, LayerKind, Mask, StrokePoint, TileStore, TipBitmap, TipShape, Tone, VectorStroke,
};
use painter_render::{build_plan, full_upload, live_planes, CanvasFrame, DabStyle, GpuCanvas, Plane, StrokePreview, ViewUniform};

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
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
            let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))?;
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
    let color = if case.edit_mask { [1.0; 3] } else { [0.95, 0.2, 0.1] };
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
