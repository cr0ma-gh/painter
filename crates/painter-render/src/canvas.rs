//! GPU canvas: sparse tile residency, stroke preview, blend-mode compositing and presentation.
//!
//! Each frame the app hands over a [`CanvasFrame`]: dirty tiles to upload, the
//! composite plan, the in-progress stroke (if any) and the view transform.
//! Compositing only re-runs when something changed; panning, zooming and
//! rotating just redraw the cached result.
//!
//! Layer pixels, masks and the selection live in one tile atlas (`atlas.rs`).
//! Compositing ping-pongs between two full-canvas targets per isolation depth:
//! every layer is one fullscreen pass `target[next] = blend(target[cur], layer)`
//! with its mask, clipping base and opacity applied. Isolated folders composite
//! their children on the next depth's targets, then blend that result onto
//! their parent like a single layer. While a stroke is in progress the edited
//! plane (layer colour, layer mask, or a clipping base) is read from the
//! preview texture instead (see `stroke.rs`).
//!
//! During a stroke only the painted layer changes, so the stack is split
//! around it (`plan::split_at_layer`) and, per nesting level, the composite
//! *below* it is cached, as is the composite *above* it when every layer there
//! is Normal and none clips to the painted layer. A frame with new dabs then
//! costs `below ⊕ preview ⊕ above` per level instead of the whole stack.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use painter_core::{AdjustOp, BlendMode, Gradient, Tile, TileCoord, Tone};
use wgpu::util::DeviceExt;

use crate::atlas::{index_texture, Plane, Residency};
use crate::gpu::{self, PipelineDesc, Target, CACHE_USAGE, CANVAS_FORMAT, STROKE_FORMAT, TARGET_USAGE};
use crate::plan::{isolation_depth, split_at_layer, LayerPath, NodeKind, PlanNode};
use crate::stroke::{DabRenderer, MergeOptions, StrokeMerger, StrokePreview};
use crate::scene::{ScenePreview, SceneRenderer, SceneTarget};
use crate::transform::{TransformPreview, TransformRenderer};

#[derive(Clone)]
pub enum Upload {
    /// `None` clears the tile.
    Tile { plane: Plane, coord: TileCoord, tile: Option<Arc<Tile>> },
    /// Drops every tile of a plane.
    Clear(Plane),
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct ViewUniform {
    rect_size: [f32; 2],
    doc_size: [f32; 2],
    pan: [f32; 2],
    zoom: f32,
    rotation: f32,
    checker: f32,
    time: f32,
    show_selection: f32,
    /// 1 = mirrored left-right.
    flip: f32,
    /// 1 per colour channel hidden (red, green, blue; the 4th is padding).
    hidden: [f32; 4],
    /// CMYK documents' ink channels shown (C, M, Y, K), when `ink_view.x` is 1.
    inks: [f32; 4],
    ink_view: [f32; 4],
}

impl ViewUniform {
    /// Sizes and pan are in UI points; `pan` offsets the canvas centre from
    /// the widget centre. `checker` is the checkerboard cell size in canvas pixels.
    pub fn new(rect_size: [f32; 2], doc_size: [f32; 2], pan: [f32; 2], zoom: f32, rotation: f32, checker: f32) -> Self {
        Self { rect_size, doc_size, pan, zoom, rotation, checker, ..Self::default() }
    }

    /// Mirrors the view left-right.
    pub fn with_flip(self, flip: bool) -> Self {
        Self { flip: if flip { 1.0 } else { 0.0 }, ..self }
    }

    /// Shows only the visible colour channels; one alone shows as grey.
    pub fn with_channels(self, visible: [bool; 3]) -> Self {
        let h = |v: bool| if v { 0.0 } else { 1.0 };
        Self { hidden: [h(visible[0]), h(visible[1]), h(visible[2]), 0.0], ..self }
    }

    /// A CMYK document showing some of its inks (C, M, Y, K): one alone as
    /// grey (white: no ink), several mixed. Needs [`CanvasFrame::ink_lut`].
    pub fn with_inks(self, shown: [bool; 4]) -> Self {
        let f = |v: bool| if v { 1.0 } else { 0.0 };
        Self { inks: shown.map(f), ink_view: [1.0, 0.0, 0.0, 0.0], ..self }
    }

    /// Shows the canvas through [`CanvasFrame::ink_lut`] as an RGB → RGB table
    /// (Duotone's inks, Lab and Multichannel channel views).
    pub fn with_display_lut(self) -> Self {
        Self { ink_view: [0.0, 1.0, 0.0, 0.0], ..self }
    }

    /// Draws animated marching ants along the selection edge.
    pub fn with_selection_outline(self, time: f32) -> Self {
        Self { time, show_selection: 1.0, ..self }
    }
}

pub struct CanvasFrame {
    /// Bumped by the app whenever the document is replaced; drops all GPU state.
    pub epoch: u64,
    pub doc_size: (u32, u32),
    pub plan: Vec<PlanNode>,
    pub uploads: Vec<Upload>,
    /// Planes that still exist; tiles of others are freed.
    pub live_planes: Vec<Plane>,
    /// The document has a selection (`Plane::Selection`, possibly empty).
    pub selection: bool,
    pub recomposite: bool,
    pub stroke: Option<StrokePreview>,
    /// An open transform: the layer shows its rest plus the transformed floating pixels.
    pub transform: Option<TransformPreview>,
    /// 3D layers: their planes come from these scenes, rendered on the GPU.
    pub scenes: Vec<ScenePreview>,
    pub view: ViewUniform,
    /// RGB → CMYK table of a CMYK document (`painter_core::cmyk::Proofer::lut`)
    /// and a key that changes with it; uploaded when the key changes.
    pub ink_lut: Option<(u64, std::sync::Arc<Vec<u8>>)>,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CompositeParams {
    mode: u32,
    opacity: f32,
    source_kind: u32,
    mask_kind: u32,
    clip_kind: u32,
    gradient_shape: u32,
    /// 1: show the source in the layer colour `color1`.
    tint: u32,
    _pad: u32,
    color0: [f32; 4],
    color1: [f32; 4],
    geom: [f32; 4],
}

impl CompositeParams {
    fn new(blend: BlendMode, opacity: f32) -> Self {
        Self {
            mode: blend.shader_id(),
            opacity,
            source_kind: 0,
            mask_kind: 0,
            clip_kind: 0,
            gradient_shape: 0,
            tint: 0,
            _pad: 0,
            color0: [0.0; 4],
            color1: [0.0; 4],
            geom: [0.0; 4],
        }
    }
}

/// Where a pass reads colour from.
enum Source<'a> {
    Atlas(&'a wgpu::TextureView),
    Direct(&'a wgpu::TextureView),
    Solid([f32; 4]),
    Gradient(&'a Gradient),
    Tone(&'a Tone),
    /// A correction: the backdrop recoloured by an op, with its lookup table (if any).
    Adjust(&'a AdjustOp, &'a wgpu::TextureView),
}

/// Where a pass reads a mask or clipping alpha from.
#[derive(Clone, Copy)]
enum Alpha<'a> {
    None,
    Atlas(&'a wgpu::TextureView),
    Direct(&'a wgpu::TextureView),
}

impl Alpha<'_> {
    fn kind(self) -> u32 {
        match self {
            Alpha::None => 0,
            Alpha::Atlas(_) => 1,
            Alpha::Direct(_) => 2,
        }
    }
}

/// Composites cached for one nesting level of the path to the painted layer.
struct LevelCache {
    below: Target,
    /// Only meaningful when that level's `above` is flattenable.
    above: Target,
}

/// The transformed floating pixels and the layer preview they're merged into.
struct TransformTargets {
    floating: Target,
    preview: Target,
}

/// Stroke texture plus the merged preview of the plane being painted.
struct StrokeTargets {
    stroke: Target,
    preview: Target,
}

pub struct GpuCanvas {
    composite_pipeline: wgpu::RenderPipeline,
    composite_layout: wgpu::BindGroupLayout,
    present_pipeline: wgpu::RenderPipeline,
    present_layout: wgpu::BindGroupLayout,
    dabs: DabRenderer,
    merger: StrokeMerger,
    transformer: TransformRenderer,
    scene_renderer: SceneRenderer,
    /// Colour and depth targets of each 3D layer.
    scene_targets: HashMap<painter_core::LayerId, SceneTarget>,
    /// Correction layers' lookup tables (256 × 1), with the bytes last written.
    adjust_luts: HashMap<painter_core::LayerId, (Vec<u8>, Target)>,
    linear_sampler: wgpu::Sampler,
    nearest_sampler: wgpu::Sampler,
    view_buffer: wgpu::Buffer,
    thumb_buffer: wgpu::Buffer,
    /// Bound wherever a direct texture is unused.
    dummy: Target,
    /// An all-empty plane index.
    no_index: wgpu::TextureView,
    /// The CMYK ink table (a 3D texture) and the key of what it holds.
    ink_lut: (wgpu::TextureView, u64),
    lut_sampler: wgpu::Sampler,

    epoch: u64,
    size: (u32, u32),
    residency: Residency,
    selection: bool,
    /// Ping-pong composite targets, one pair per isolation depth.
    chains: Vec<[Target; 2]>,
    final_index: usize,
    /// Allocated on the first stroke and kept for later strokes.
    stroke_targets: Option<StrokeTargets>,
    stroke_id: Option<u64>,
    /// Plane read from the stroke preview while compositing.
    preview: Option<Plane>,
    /// Per-level caches, allocated on the first stroke and reused.
    level_caches: Vec<LevelCache>,
    /// Stroke the level caches were built for.
    cache_stroke: Option<u64>,
    /// Source of the open transform, by preview id.
    transform_source: Option<(u64, Target)>,
    transform_targets: Option<TransformTargets>,
    /// Revision the transform preview was last drawn at.
    transform_revision: Option<u64>,
    /// Plane read from the transform preview while compositing.
    transform_plane: Option<Plane>,
    /// Blend passes recorded by the last `prepare`.
    passes: AtomicUsize,
    present_bind_group: Option<wgpu::BindGroup>,
    thumb_bind_group: Option<wgpu::BindGroup>,
}

impl GpuCanvas {
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("composite"),
            entries: &[
                gpu::texture_entry(0),
                gpu::texture_array_entry(1),
                gpu::index_entry(2),
                gpu::texture_entry(3),
                gpu::index_entry(4),
                gpu::texture_entry(5),
                gpu::index_entry(6),
                gpu::texture_entry(7),
                gpu::uniform_entry(8, wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let present_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("present"),
            entries: &[
                gpu::texture_entry(0),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                gpu::uniform_entry(2, wgpu::ShaderStages::VERTEX_FRAGMENT),
                gpu::texture_array_entry(3),
                gpu::index_entry(4),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let composite_shader = gpu::shader(
            device,
            "composite",
            concat!(include_str!("shaders/atlas.wgsl"), include_str!("shaders/composite.wgsl")),
        );
        let present_shader =
            gpu::shader(device, "present", concat!(include_str!("shaders/atlas.wgsl"), include_str!("shaders/present.wgsl")));
        let uniform = |label| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: std::mem::size_of::<ViewUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };

        Self {
            composite_pipeline: gpu::pipeline(
                device,
                PipelineDesc {
                    label: "composite",
                    shader: &composite_shader,
                    layout: &composite_layout,
                    format: CANVAS_FORMAT,
                    vertex_buffers: &[],
                    blend: None,
                },
            ),
            present_pipeline: gpu::pipeline(
                device,
                PipelineDesc {
                    label: "present",
                    shader: &present_shader,
                    layout: &present_layout,
                    format: surface_format,
                    vertex_buffers: &[],
                    blend: None,
                },
            ),
            composite_layout,
            present_layout,
            dabs: DabRenderer::new(device),
            merger: StrokeMerger::new(device),
            transformer: TransformRenderer::new(device),
            scene_renderer: SceneRenderer::new(device),
            scene_targets: HashMap::new(),
            adjust_luts: HashMap::new(),
            linear_sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("canvas linear"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            nearest_sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("canvas nearest"),
                ..Default::default()
            }),
            view_buffer: uniform("view"),
            thumb_buffer: uniform("thumbnail view"),
            dummy: Target::new(device, (1, 1), wgpu::TextureUsages::TEXTURE_BINDING, "dummy"),
            no_index: index_texture(device, (1, 1), "empty index").create_view(&wgpu::TextureViewDescriptor::default()),
            ink_lut: (ink_lut_texture(device, 1).create_view(&wgpu::TextureViewDescriptor::default()), 0),
            lut_sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("ink table"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            epoch: 0,
            size: (0, 0),
            residency: Residency::new(device),
            selection: false,
            chains: Vec::new(),
            final_index: 0,
            stroke_targets: None,
            stroke_id: None,
            preview: None,
            level_caches: Vec::new(),
            cache_stroke: None,
            transform_source: None,
            transform_targets: None,
            transform_revision: None,
            transform_plane: None,
            passes: AtomicUsize::new(0),
            present_bind_group: None,
            thumb_bind_group: None,
        }
    }

    /// Uploads tiles, draws new dabs, recomposites if needed and prepares the
    /// present pass. Records into `encoder`, which is submitted before the UI pass.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame: &CanvasFrame,
    ) {
        // A CMYK document's ink table, when it changed.
        if let Some((key, lut)) = &frame.ink_lut {
            if *key != self.ink_lut.1 {
                let n = painter_core::cmyk::LUT_SIZE as u32;
                if lut.len() == (n * n * n * 4) as usize {
                    let texture = ink_lut_texture(device, n);
                    queue.write_texture(
                        texture.as_image_copy(),
                        lut,
                        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(n * 4), rows_per_image: Some(n) },
                        wgpu::Extent3d { width: n, height: n, depth_or_array_layers: n },
                    );
                    self.ink_lut = (texture.create_view(&wgpu::TextureViewDescriptor::default()), *key);
                }
            }
        }
        // Anything other than new dabs invalidates the stroke-time caches.
        let mut content_changed = frame.recomposite || !frame.uploads.is_empty() || frame.selection != self.selection;
        if frame.epoch != self.epoch || frame.doc_size != self.size || self.chains.is_empty() {
            self.epoch = frame.epoch;
            self.size = frame.doc_size;
            self.residency.reset(device, self.size);
            self.chains.clear();
            self.stroke_targets = None;
            self.stroke_id = None;
            self.level_caches.clear();
            self.cache_stroke = None;
            self.transform_source = None;
            self.transform_targets = None;
            self.transform_revision = None;
            self.scene_targets.clear();
            self.adjust_luts.clear();
            content_changed = true;
        }
        self.selection = frame.selection;

        let live: HashSet<Plane> = frame.live_planes.iter().copied().collect();
        self.residency.retain(|plane| live.contains(plane));
        for upload in &frame.uploads {
            match upload {
                Upload::Clear(plane) => self.residency.clear(*plane),
                Upload::Tile { plane, coord, tile } => {
                    self.residency.write_tile(device, queue, *plane, *coord, tile.as_deref().map(Tile::bytes));
                }
            }
        }
        // After layers were deleted or cleared, hand the emptied atlas pages back.
        self.residency.shrink(device, queue);
        self.residency.flush(queue);
        self.update_adjust_luts(device, queue, &frame.plan);

        let stroke_changed = self.update_stroke_preview(device, queue, encoder, frame.stroke.as_ref());
        let transform_changed = self.update_transform_preview(device, queue, encoder, frame.transform.as_ref(), content_changed);
        let scenes_changed = self.update_scenes(device, encoder, &frame.scenes);
        let preview_changed = stroke_changed || transform_changed || scenes_changed;

        let depth = isolation_depth(&frame.plan);
        while self.chains.len() <= depth {
            self.chains.push([
                Target::new(device, self.size, TARGET_USAGE, "composite a"),
                Target::new(device, self.size, TARGET_USAGE, "composite b"),
            ]);
        }

        self.passes.store(0, Ordering::Relaxed);
        let path = frame.stroke.as_ref().and_then(|s| match s.plane {
            Plane::Color(id) | Plane::Mask(id) => Some((s.id, split_at_layer(&frame.plan, id)?)),
            Plane::Selection => None,
        });
        match path {
            Some((stroke_id, path)) => {
                let rebuild = content_changed || self.cache_stroke != Some(stroke_id);
                if rebuild {
                    self.ensure_level_caches(device, path.levels.len());
                    self.build_level_caches(device, encoder, &path);
                    self.cache_stroke = Some(stroke_id);
                }
                if rebuild || preview_changed {
                    self.final_index = self.composite_cached(device, encoder, &path);
                }
            }
            None => {
                self.cache_stroke = None;
                if content_changed || preview_changed {
                    let mut cur = vec![0usize; self.chains.len()];
                    gpu::clear(encoder, &self.chains[0][0].view);
                    self.composite_seq(device, encoder, &frame.plan, 0, &mut cur);
                    self.final_index = cur[0];
                }
            }
        }

        let view = if frame.selection { frame.view } else { ViewUniform { show_selection: 0.0, ..frame.view } };
        queue.write_buffer(&self.view_buffer, 0, bytemuck::bytes_of(&view));
        self.present_bind_group = Some(self.present_bind_group_for(device, &self.view_buffer, view.zoom, frame.selection));
    }

    fn present_bind_group_for(&self, device: &wgpu::Device, uniform: &wgpu::Buffer, zoom: f32, selection: bool) -> wgpu::BindGroup {
        // Show crisp pixels when zoomed in, smooth them when zoomed out.
        let sampler = if zoom >= 2.0 { &self.nearest_sampler } else { &self.linear_sampler };
        let selection_index =
            selection.then(|| self.residency.get(Plane::Selection)).flatten().map_or(&self.no_index, |p| &p.view);
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("present"),
            layout: &self.present_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.chains[0][self.final_index].view),
                },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&self.residency.atlas.view) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(selection_index) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&self.ink_lut.0) },
                wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(&self.lut_sampler) },
            ],
        })
    }

    /// Draws the canvas into the UI render pass (viewport = canvas widget).
    pub fn paint(&self, pass: &mut wgpu::RenderPass<'static>) {
        let Some(bind_group) = &self.present_bind_group else {
            return;
        };
        pass.set_pipeline(&self.present_pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..6, 0..1);
    }

    /// Prepares a second, independent presentation (e.g. the navigator
    /// thumbnail). Call after `prepare` has run for the frame.
    pub fn prepare_thumbnail(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, view: ViewUniform) {
        if self.chains.is_empty() {
            return;
        }
        let view = ViewUniform { show_selection: 0.0, ..view };
        queue.write_buffer(&self.thumb_buffer, 0, bytemuck::bytes_of(&view));
        self.thumb_bind_group = Some(self.present_bind_group_for(device, &self.thumb_buffer, view.zoom, false));
    }

    pub fn paint_thumbnail(&self, pass: &mut wgpu::RenderPass<'static>) {
        let Some(bind_group) = &self.thumb_bind_group else {
            return;
        };
        pass.set_pipeline(&self.present_pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..6, 0..1);
    }

    /// Blend passes the last `prepare` recorded (for tests and profiling).
    pub fn last_composite_passes(&self) -> usize {
        self.passes.load(Ordering::Relaxed)
    }

    /// Tile slots in use and GPU bytes held by the tile atlas.
    pub fn atlas_usage(&self) -> (u32, u64) {
        (self.residency.atlas.used_slots(), self.residency.atlas.memory_bytes())
    }

    /// Reads back the last composite as premultiplied RGBA8 (for tests and GPU export).
    pub fn read_composite(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Vec<u8>> {
        let target = self.chains.first()?.get(self.final_index)?;
        Some(gpu::read_texture(device, queue, &target.texture, self.size))
    }

    /// Returns whether the composite needs to be rebuilt.
    fn update_stroke_preview(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        stroke: Option<&StrokePreview>,
    ) -> bool {
        let Some(stroke) = stroke else {
            self.preview = None;
            // The stroke just ended: show the committed tiles instead of the preview.
            return self.stroke_id.take().is_some();
        };

        let size = self.size;
        let targets = self.stroke_targets.get_or_insert_with(|| StrokeTargets {
            stroke: Target::with_format(device, size, STROKE_FORMAT, TARGET_USAGE, "stroke"),
            preview: Target::new(device, size, TARGET_USAGE, "stroke preview"),
        });
        let new_stroke = self.stroke_id != Some(stroke.id);
        if new_stroke {
            gpu::clear(encoder, &targets.stroke.view);
            self.stroke_id = Some(stroke.id);
        }
        self.preview = Some(stroke.plane);
        if !new_stroke && stroke.dabs.is_empty() {
            return false;
        }

        self.dabs.draw(device, queue, encoder, &targets.stroke.view, size, &stroke.dabs, &stroke.style);
        let plane_index = self.residency.get(stroke.plane).map_or(&self.no_index, |p| &p.view);
        // An existing but empty selection protects everything: bind the empty index.
        let selection = (self.selection && stroke.respect_selection)
            .then(|| self.residency.get(Plane::Selection).map_or(&self.no_index, |p| &p.view));
        self.merger.merge(
            device,
            encoder,
            &self.residency.atlas.view,
            plane_index,
            &targets.stroke.view,
            selection,
            &self.no_index,
            &targets.preview.view,
            MergeOptions::from(stroke),
        );
        true
    }

    /// Renders 3D layers whose scene changed; drops targets of layers that are
    /// no longer 3D. Returns whether the composite needs to be rebuilt.
    fn update_scenes(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, scenes: &[ScenePreview]) -> bool {
        let before = self.scene_targets.len();
        self.scene_targets.retain(|id, _| scenes.iter().any(|s| s.layer == *id));
        let mut changed = self.scene_targets.len() != before;
        for scene in scenes {
            let size = self.size;
            let target = self.scene_targets.entry(scene.layer).or_insert_with(|| SceneRenderer::target(device, size));
            if target.revision != scene.revision {
                self.scene_renderer.draw(device, encoder, target, scene);
                target.revision = scene.revision;
                changed = true;
            }
        }
        self.scene_renderer.retain_meshes(scenes.iter().flat_map(|s| s.draws.iter().map(|d| &d.mesh)));
        changed
    }

    /// Redraws the transform preview when it or the layer under it changed.
    /// Returns whether the composite needs to be rebuilt.
    fn update_transform_preview(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        transform: Option<&TransformPreview>,
        content_changed: bool,
    ) -> bool {
        let Some(transform) = transform else {
            self.transform_plane = None;
            self.transform_source = None;
            // The transform just closed: show the layer's own tiles again.
            return self.transform_revision.take().is_some();
        };
        let plane = Plane::Color(transform.layer);
        self.transform_plane = Some(plane);
        if !content_changed && self.transform_revision == Some(transform.revision) {
            return false;
        }
        if self.transform_source.as_ref().is_none_or(|(id, _)| *id != transform.id) {
            let source = TransformRenderer::upload(device, queue, &transform.source, transform.source_size);
            self.transform_source = Some((transform.id, source));
        }
        let size = self.size;
        let targets = self.transform_targets.get_or_insert_with(|| TransformTargets {
            floating: Target::new(device, size, TARGET_USAGE, "transform floating"),
            preview: Target::new(device, size, TARGET_USAGE, "transform preview"),
        });
        let Some((_, source)) = &self.transform_source else { return false };
        self.transformer.draw(device, encoder, &source.view, &targets.floating.view, size, &transform.geometry, transform.filter);
        // The layer's remaining pixels with the floating ones over them (source-over).
        let plane_index = self.residency.get(plane).map_or(&self.no_index, |p| &p.view);
        self.merger.merge(
            device,
            encoder,
            &self.residency.atlas.view,
            plane_index,
            &targets.floating.view,
            None,
            &self.no_index,
            &targets.preview.view,
            MergeOptions { opacity: 1.0, erase: false, alpha_locked: false, blend: 0 },
        );
        self.transform_revision = Some(transform.revision);
        true
    }

    /// Writes the lookup tables of the plan's correction layers; drops the rest.
    fn update_adjust_luts(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, plan: &[PlanNode]) {
        fn collect(nodes: &[PlanNode], out: &mut Vec<(painter_core::LayerId, Vec<u8>)>) {
            for n in nodes {
                match &n.kind {
                    NodeKind::Adjust(op) => out.push((n.id, op.lut_bytes().unwrap_or_else(|| vec![0; 256 * 4]))),
                    NodeKind::Group { children, .. } => collect(children, out),
                    _ => {}
                }
            }
        }
        let mut wanted = Vec::new();
        collect(plan, &mut wanted);
        self.adjust_luts.retain(|id, _| wanted.iter().any(|(w, _)| w == id));
        for (id, bytes) in wanted {
            if self.adjust_luts.get(&id).is_some_and(|(old, _)| *old == bytes) {
                continue;
            }
            let usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
            let target = match self.adjust_luts.remove(&id) {
                Some((_, t)) => t,
                None => Target::with_format(device, (256, 1), gpu::CANVAS_FORMAT, usage, "adjustment lut"),
            };
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &target.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &bytes,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256 * 4), rows_per_image: Some(1) },
                wgpu::Extent3d { width: 256, height: 1, depth_or_array_layers: 1 },
            );
            self.adjust_luts.insert(id, (bytes, target));
        }
    }

    fn ensure_level_caches(&mut self, device: &wgpu::Device, levels: usize) {
        while self.level_caches.len() < levels {
            self.level_caches.push(LevelCache {
                below: Target::new(device, self.size, CACHE_USAGE, "cache below"),
                above: Target::new(device, self.size, CACHE_USAGE, "cache above"),
            });
        }
    }

    /// Composites `below` (and a flattenable `above`) of every level into the caches.
    fn build_level_caches(&self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, path: &LayerPath<'_>) {
        for (depth, level) in path.levels.iter().enumerate() {
            let cache = &self.level_caches[depth];
            let mut cur = vec![0usize; self.chains.len()];
            gpu::clear(encoder, &self.chains[depth][0].view);
            self.composite_seq(device, encoder, level.below.iter().copied(), depth, &mut cur);
            gpu::copy_texture(encoder, &self.chains[depth][cur[depth]], &cache.below, self.size);
            if level.above_is_flattenable(path.node.id) && !level.above.is_empty() {
                cur[depth] = 0;
                gpu::clear(encoder, &self.chains[depth][0].view);
                self.composite_seq(device, encoder, level.above.iter().copied(), depth, &mut cur);
                gpu::copy_texture(encoder, &self.chains[depth][cur[depth]], &cache.above, self.size);
            }
        }
    }

    /// Rebuilds the composite from the level caches and the stroke preview,
    /// innermost level first. Returns the root chain index holding the result.
    fn composite_cached(&self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, path: &LayerPath<'_>) -> usize {
        let mut cur = vec![0usize; self.chains.len()];
        let mut inner_result: Option<&wgpu::TextureView> = None;
        for depth in (0..path.levels.len()).rev() {
            let level = &path.levels[depth];
            let cache = &self.level_caches[depth];
            let node = level.enter.unwrap_or(path.node);
            if !self.node_pass(device, encoder, node, depth, &mut cur, Some(&cache.below.view), inner_result) {
                // Nothing drawn: the cached backdrop is the result so far.
                gpu::copy_texture(encoder, &cache.below, &self.chains[depth][cur[depth]], self.size);
            }
            if level.above_is_flattenable(path.node.id) {
                if !level.above.is_empty() {
                    let over = CompositeParams::new(BlendMode::Normal, 1.0);
                    let above = Source::Direct(&cache.above.view);
                    self.blend_pass(device, encoder, depth, &mut cur, None, above, Alpha::None, Alpha::None, over);
                }
            } else {
                self.composite_seq(device, encoder, level.above.iter().copied(), depth, &mut cur);
            }
            inner_result = Some(&self.chains[depth][cur[depth]].view);
        }
        cur[0]
    }

    fn composite_seq<'n>(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        nodes: impl IntoIterator<Item = &'n PlanNode>,
        depth: usize,
        cur: &mut [usize],
    ) {
        for node in nodes {
            match &node.kind {
                NodeKind::Group { isolated: false, children } => self.composite_seq(device, encoder, children, depth, cur),
                NodeKind::Group { isolated: true, children } => {
                    let inner = depth + 1;
                    cur[inner] = 0;
                    gpu::clear(encoder, &self.chains[inner][0].view);
                    self.composite_seq(device, encoder, children, inner, cur);
                    let result = &self.chains[inner][cur[inner]].view;
                    self.node_pass(device, encoder, node, depth, cur, None, Some(result));
                }
                _ => {
                    self.node_pass(device, encoder, node, depth, cur, None, None);
                }
            }
        }
    }

    /// `plane` as read by a pass: the stroke preview while it is being edited, else its atlas index.
    fn plane_alpha(&self, plane: Plane) -> Option<Alpha<'_>> {
        if let Plane::Color(id) = plane {
            if let Some(target) = self.scene_targets.get(&id) {
                return Some(Alpha::Direct(&target.color.view));
            }
        }
        if self.transform_plane == Some(plane) {
            return self.transform_targets.as_ref().map(|t| Alpha::Direct(&t.preview.view));
        }
        if self.preview == Some(plane) {
            return self.stroke_targets.as_ref().map(|t| Alpha::Direct(&t.preview.view));
        }
        self.residency.get(plane).map(|p| Alpha::Atlas(&p.view))
    }

    /// Blends one node. `source` overrides its colour (a group's composite).
    /// Returns false when the node contributes nothing and no pass was recorded.
    #[allow(clippy::too_many_arguments)]
    fn node_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        node: &PlanNode,
        depth: usize,
        cur: &mut [usize],
        backdrop: Option<&wgpu::TextureView>,
        source: Option<&wgpu::TextureView>,
    ) -> bool {
        let source = match (source, &node.kind) {
            (Some(view), _) => Source::Direct(view),
            (None, NodeKind::Pixels) => match self.plane_alpha(Plane::Color(node.id)) {
                Some(Alpha::Direct(v)) => Source::Direct(v),
                Some(Alpha::Atlas(v)) => Source::Atlas(v),
                // Never painted.
                _ => return false,
            },
            (None, NodeKind::Solid(color)) => Source::Solid(*color),
            (None, NodeKind::Gradient(g)) => Source::Gradient(g),
            (None, NodeKind::Tone(t)) => Source::Tone(t),
            (None, NodeKind::Adjust(_)) => Source::Solid([0.0; 4]),
            (None, NodeKind::Group { .. }) => unreachable!("isolated groups pass their composite as the source"),
        };
        // Corrections bind their lookup table as the source texture.
        let source = match &node.kind {
            NodeKind::Adjust(op) => match self.adjust_luts.get(&node.id) {
                Some((_, lut)) => Source::Adjust(op, &lut.view),
                None => return false,
            },
            _ => source,
        };
        let mask = if node.mask { self.plane_alpha(Plane::Mask(node.id)).unwrap_or(Alpha::None) } else { Alpha::None };
        let clip = match node.clip {
            None => Alpha::None,
            Some(base) => match self.plane_alpha(Plane::Color(base)) {
                Some(alpha) => alpha,
                // An empty base hides everything clipped to it.
                None => return false,
            },
        };
        let mut params = CompositeParams::new(node.blend, node.opacity);
        if let Some([r, g, b]) = node.tint {
            params.tint = 1;
            params.color1 = [r, g, b, 1.0];
        }
        self.blend_pass(device, encoder, depth, cur, backdrop, source, mask, clip, params);
        true
    }

    /// The (index, direct) texture pair a pass binds for an alpha input.
    fn alpha_bindings<'a>(&'a self, alpha: Alpha<'a>) -> (&'a wgpu::TextureView, &'a wgpu::TextureView) {
        match alpha {
            Alpha::Atlas(v) => (v, &self.dummy.view),
            Alpha::Direct(v) => (&self.no_index, v),
            Alpha::None => (&self.no_index, &self.dummy.view),
        }
    }

    /// `chains[depth][next] = blend(backdrop or chains[depth][cur], source)`.
    #[allow(clippy::too_many_arguments)]
    fn blend_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        depth: usize,
        cur: &mut [usize],
        backdrop: Option<&wgpu::TextureView>,
        source: Source<'_>,
        mask: Alpha<'_>,
        clip: Alpha<'_>,
        mut params: CompositeParams,
    ) {
        self.passes.fetch_add(1, Ordering::Relaxed);
        let from = cur[depth];
        let to = 1 - from;
        let backdrop = backdrop.unwrap_or(&self.chains[depth][from].view);
        let (mut source_index, mut source_direct) = (&self.no_index, &self.dummy.view);
        match source {
            Source::Atlas(v) => source_index = v,
            Source::Direct(v) => {
                params.source_kind = 1;
                source_direct = v;
            }
            Source::Solid(color) => {
                params.source_kind = 2;
                params.color0 = color;
            }
            Source::Gradient(g) => {
                params.source_kind = 3;
                params.gradient_shape = g.shape.shader_id();
                params.color0 = g.from.map(|c| c as f32 / 255.0);
                params.color1 = g.to.map(|c| c as f32 / 255.0);
                params.geom = [g.start[0], g.start[1], g.end[0], g.end[1]];
            }
            Source::Adjust(op, lut) => {
                params.source_kind = 5;
                params.gradient_shape = op.shader_id();
                if let AdjustOp::Hsl { hue, saturation, lightness } = op {
                    params.geom = [*hue, *saturation, *lightness, 0.0];
                }
                source_direct = lut;
            }
            Source::Tone(t) => {
                params.source_kind = 4;
                params.color0 = [t.color[0] as f32 / 255.0, t.color[1] as f32 / 255.0, t.color[2] as f32 / 255.0, 1.0];
                params.geom = [t.cell, t.angle.to_radians(), t.density, 0.0];
            }
        }
        params.mask_kind = mask.kind();
        params.clip_kind = clip.kind();
        let (mask_index, mask_direct) = self.alpha_bindings(mask);
        let (clip_index, clip_direct) = self.alpha_bindings(clip);

        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("composite params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let views =
            [backdrop, &self.residency.atlas.view, source_index, source_direct, mask_index, mask_direct, clip_index, clip_direct];
        let mut entries: Vec<wgpu::BindGroupEntry> = views
            .iter()
            .enumerate()
            .map(|(i, v)| wgpu::BindGroupEntry { binding: i as u32, resource: wgpu::BindingResource::TextureView(v) })
            .collect();
        entries.push(wgpu::BindGroupEntry { binding: 8, resource: uniform.as_entire_binding() });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("composite"),
            layout: &self.composite_layout,
            entries: &entries,
        });
        let mut pass = gpu::begin_pass(
            encoder,
            &self.chains[depth][to].view,
            wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            "composite",
        );
        pass.set_pipeline(&self.composite_pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);
        cur[depth] = to;
    }
}

/// An `n`³ RGBA8 3D texture for the CMYK ink table.
fn ink_lut_texture(device: &wgpu::Device, n: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ink table"),
        size: wgpu::Extent3d { width: n, height: n, depth_or_array_layers: n },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_layouts_match_wgsl() {
        // WGSL `View` and `Params` structs are 96 and 80 bytes.
        assert_eq!(std::mem::size_of::<ViewUniform>(), 96);
        assert_eq!(std::mem::size_of::<CompositeParams>(), 80);
    }
}
