//! Live stroke preview on the GPU.
//!
//! While a stroke is in progress, new dabs are drawn as instanced quads into a
//! stroke texture (canvas-sized, or in low-VRAM mode sized to the stroke's
//! bounding box and grown as it spreads), and the edited plane (layer colour or mask) is
//! shown as `merge(plane, stroke × opacity × selection)`. The CPU rasterises the same dabs in the
//! background and becomes authoritative when the stroke ends; both paths use
//! the same formulas (`dab.wgsl` ↔ `raster::stamp_dab`,
//! `stroke_merge.wgsl` ↔ `raster::merge_stroke`).

use bytemuck::{Pod, Zeroable};
use painter_core::{BrushSettings, Dab, DabTexture, DabTip, DualTip, TipBitmap};
use wgpu::util::DeviceExt;

use crate::assets::TextureAssets;
use crate::atlas::Plane;

use crate::gpu::{self, PipelineDesc, Region, CANVAS_FORMAT, STROKE_FORMAT, STROKE_FORMAT_8BIT};

/// Brush appearance shared by every dab of a stroke.
#[derive(Clone, Debug)]
pub struct DabStyle {
    /// Straight sRGB colour.
    pub color: [f32; 3],
    pub hardness: f32,
    /// Anti-aliasing edge width in pixels.
    pub antialias: f32,
    pub tip: DabTip,
    pub texture: Option<DabTexture>,
    pub dual: Option<DualTip>,
    /// Photoshop's noise in the soft parts of the tip.
    pub noise: bool,
}

impl DabStyle {
    /// The dab appearance of `settings` painted in `color`.
    pub fn from_settings(color: [f32; 3], settings: &BrushSettings) -> Self {
        Self {
            color,
            hardness: settings.hardness,
            antialias: settings.antialias.width(),
            tip: settings.tip.clone(),
            texture: settings.texture.clone(),
            dual: settings.dual.clone(),
            noise: settings.noise,
        }
    }
}

/// One frame's worth of stroke for [`crate::GpuCanvas`].
pub struct StrokePreview {
    /// Changes for every new stroke; a new id clears the stroke texture.
    pub id: u64,
    /// `Plane::Color` for painting, `Plane::Mask` for editing a layer mask.
    pub plane: Plane,
    /// Dabs placed since the previous frame.
    pub dabs: Vec<Dab>,
    pub style: DabStyle,
    pub opacity: f32,
    pub erase: bool,
    /// The brush's blend mode (`painter_core::BrushBlend::shader_id`; 0 = Normal).
    pub blend: u32,
    pub alpha_locked: bool,
    /// Limit the preview to the selection (false for vector layers, which ignore it).
    pub respect_selection: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DabInstance {
    center: [f32; 2],
    radius: f32,
    alpha: f32,
    reach: f32,
    level: u32,
    dual_level: u32,
    /// `[cos, sin]` of the tip angle plus this dab's rotation.
    rotation: [f32; 2],
    /// The tip's thickness × this dab's (thickness dynamics), 0.01..=1;
    /// negative when the dab is mirrored (`Dab::mirror`).
    flatness: f32,
    /// The dab's own colour with alpha 1 (`Dab::color`), else zero.
    color: [f32; 4],
    /// Multiplies the texture density (`Dab::texture`).
    texture: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DabParams {
    canvas_size: [f32; 2],
    hardness: f32,
    aa: f32,
    color: [f32; 4],
    /// 1: dither each dab's coverage (8-bit stroke targets).
    dither: f32,
    /// 2 for a sampled tip, 0 for round (disc or ellipse is decided per dab).
    kind: u32,
    tip_aspect: [f32; 2],
    dual_aspect: [f32; 2],
    texture_scale: f32,
    texture_density: f32,
    texture_invert: u32,
    has_texture: u32,
    dual_scale: f32,
    has_dual: u32,
    /// Canvas position of the target's top-left pixel.
    origin: [f32; 2],
    texture_mode: u32,
    dual_mode: u32,
    texture_brightness: f32,
    texture_contrast: f32,
    noise: u32,
    texture_each_dab: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MergeParams {
    opacity: f32,
    erase: u32,
    alpha_locked: u32,
    has_selection: u32,
    blend: u32,
    _pad: u32,
    /// Canvas position of the target's (and the stroke texture's) top-left pixel.
    origin: [i32; 2],
    has_texture: u32,
    texture_mode: u32,
    texture_scale: f32,
    texture_density: f32,
    texture_invert: u32,
    texture_brightness: f32,
    texture_contrast: f32,
    _pad1: u32,
}

const DAB_ATTRIBUTES: [wgpu::VertexAttribute; 10] = wgpu::vertex_attr_array![
    0 => Float32x2, 1 => Float32, 2 => Float32, 3 => Float32, 4 => Uint32, 5 => Uint32, 6 => Float32x2, 7 => Float32, 8 => Float32x4,
    9 => Float32
];

/// Draws dabs with premultiplied source-over into an `Rgba16Float` stroke target
/// (or, for low-VRAM profiles, a dithered `Rgba8Unorm` one).
pub struct DabRenderer {
    pipeline: wgpu::RenderPipeline,
    /// The same pipeline for `Rgba8Unorm` stroke targets.
    pipeline_8bit: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    /// Tips, dual tips and paper textures, uploaded once and reused.
    assets: TextureAssets,
}

impl DabRenderer {
    pub fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dab"),
            entries: &[
                gpu::uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT),
                gpu::texture_entry(1),
                gpu::texture_entry(2),
                gpu::texture_entry(3),
            ],
        });
        let shader = gpu::shader(device, "dab", concat!(include_str!("shaders/brush_mask.wgsl"), include_str!("shaders/dab.wgsl")));
        let pipeline = |format| {
            gpu::pipeline(
                device,
                PipelineDesc {
                    label: "dab",
                    shader: &shader,
                    layout: &layout,
                    format,
                    vertex_buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<DabInstance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &DAB_ATTRIBUTES,
                    }],
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                },
            )
        };
        Self {
            pipeline: pipeline(STROKE_FORMAT),
            pipeline_8bit: pipeline(STROKE_FORMAT_8BIT),
            layout,
            assets: TextureAssets::new(device),
        }
    }

    /// Canvas area `[min_x, min_y, max_x, max_y]` the quads of `dabs` cover
    /// (`None` when no dab is visible).
    pub(crate) fn bounds(dabs: &[Dab], style: &DabStyle) -> Option<[f32; 4]> {
        Self::instances(dabs, style).iter().fold(None, |acc, d| {
            // The vertex shader pads each quad by one pixel.
            let r = d.reach + 1.0;
            let b = [d.center[0] - r, d.center[1] - r, d.center[0] + r, d.center[1] + r];
            Some(acc.map_or(b, |a: [f32; 4]| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]))
        })
    }

    fn instances(dabs: &[Dab], style: &DabStyle) -> Vec<DabInstance> {
        let tip = &style.tip;
        dabs.iter()
            .map(|d| d.normalized())
            .filter(|d| d.alpha > 0.0)
            .map(|d| {
                let level = tip.mip_level(d.radius);
                let reach = tip.reach(d.radius, style.antialias, level);
                let dual_level = style.dual.as_ref().map_or(0, |dual| dual.mip_level(d.radius));
                let rotation = tip.rotation(d.angle);
                let flatness = tip.flatness_for(d.thickness) * if d.mirror { -1.0 } else { 1.0 };
                let color = d.color.map_or([0.0; 4], |c| [c[0], c[1], c[2], 1.0]);
                DabInstance { center: [d.x, d.y], radius: d.radius, alpha: d.alpha, reach, level, dual_level, rotation, flatness, color, texture: d.texture }
            })
            .collect()
    }

    /// Uploads `bitmap` if it isn't yet (a whole-stroke paper texture for the merge).
    pub(crate) fn ensure_texture(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, bitmap: &std::sync::Arc<TipBitmap>) {
        self.assets.ensure(device, queue, bitmap);
    }

    /// An uploaded bitmap's texture, or the placeholder bound when there's none.
    pub(crate) fn texture_view(&self, bitmap: Option<&std::sync::Arc<TipBitmap>>) -> &wgpu::TextureView {
        self.assets.view(bitmap)
    }

    /// (resident textures, resident bytes, uploads so far) of the brush texture cache.
    pub fn asset_stats(&self) -> (usize, u64, u64) {
        self.assets.stats()
    }

    /// Appends `dabs` to `target` (a canvas-sized `Rgba16Float` texture), in order.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        canvas: (u32, u32),
        dabs: &[Dab],
        style: &DabStyle,
    ) {
        self.draw_region(device, queue, encoder, target, Region::full(canvas), false, dabs, style);
    }

    /// Appends `dabs` to `target`, a texture covering canvas rectangle `at`, in
    /// order. `low_precision`: `target` is `Rgba8Unorm` and dabs are dithered.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_region(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        at: Region,
        low_precision: bool,
        dabs: &[Dab],
        style: &DabStyle,
    ) {
        let tip = &style.tip;
        let instances = Self::instances(dabs, style);
        if instances.is_empty() {
            return;
        }
        let texture = style.texture.as_ref();
        let dual = style.dual.as_ref();
        for bitmap in [tip.bitmap(), texture.map(|t| &t.bitmap), dual.map(|d| &d.bitmap)].into_iter().flatten() {
            self.assets.ensure(device, queue, bitmap);
        }
        let tip_view = self.assets.view(tip.bitmap());
        let texture_view = self.assets.view(texture.map(|t| &t.bitmap));
        let dual_view = self.assets.view(dual.map(|d| &d.bitmap));
        let params = DabParams {
            canvas_size: [at.width as f32, at.height as f32],
            hardness: style.hardness,
            aa: style.antialias,
            color: [style.color[0], style.color[1], style.color[2], 1.0],
            dither: if low_precision { 1.0 } else { 0.0 },
            kind: if tip.bitmap().is_some() { 2 } else { 0 },
            tip_aspect: tip.bitmap().map_or([1.0; 2], |b| b.aspect()),
            dual_aspect: dual.map_or([1.0; 2], |d| d.bitmap.aspect()),
            texture_scale: texture.map_or(1.0, DabTexture::scale),
            texture_density: texture.map_or(0.0, |t| t.density.clamp(0.0, 1.0)),
            texture_invert: u32::from(texture.is_some_and(|t| t.invert)),
            has_texture: u32::from(texture.is_some()),
            dual_scale: dual.map_or(1.0, DualTip::scale),
            has_dual: u32::from(dual.is_some()),
            origin: [at.x as f32, at.y as f32],
            texture_mode: texture.map_or(0, |t| t.mode.shader_id()),
            dual_mode: dual.map_or(0, |d| d.mode.shader_id()),
            texture_brightness: texture.map_or(0.0, |t| t.brightness),
            texture_contrast: texture.map_or(1.0, |t| t.contrast),
            noise: u32::from(style.noise),
            texture_each_dab: u32::from(texture.is_none_or(|t| t.each_dab)),
        };
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("dab params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let instance_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("dab instances"),
            contents: bytemuck::cast_slice(&instances),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dab"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(tip_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(texture_view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(dual_view) },
            ],
        });
        // Instances rasterise in submission order, so blending matches sequential stamping.
        let mut pass = gpu::begin_pass(encoder, target, wgpu::LoadOp::Load, "dabs");
        pass.set_pipeline(if low_precision { &self.pipeline_8bit } else { &self.pipeline });
        pass.set_bind_group(0, &bind_group, &[]);
        pass.set_vertex_buffer(0, instance_buffer.slice(..));
        pass.draw(0..6, 0..instances.len() as u32);
    }
}

/// Renders `merge(plane, stroke × opacity × selection)` into a preview target.
/// How a merge lays its source over the plane.
#[derive(Clone, Copy)]
pub(crate) struct MergeOptions {
    pub opacity: f32,
    pub erase: bool,
    pub alpha_locked: bool,
    pub blend: u32,
}

impl From<&StrokePreview> for MergeOptions {
    fn from(s: &StrokePreview) -> Self {
        Self { opacity: s.opacity, erase: s.erase, alpha_locked: s.alpha_locked, blend: s.blend }
    }
}

/// Lays a stroke (or transformed floating pixels) over a plane: the live preview.
pub(crate) struct StrokeMerger {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

impl StrokeMerger {
    pub fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("stroke merge"),
            entries: &[
                gpu::texture_array_entry(0),
                gpu::index_entry(1),
                gpu::texture_entry(2),
                gpu::index_entry(3),
                gpu::uniform_entry(4, wgpu::ShaderStages::FRAGMENT),
                gpu::texture_entry(5),
            ],
        });
        let shader = gpu::shader(
            device,
            "stroke merge",
            concat!(
                include_str!("shaders/atlas.wgsl"),
                include_str!("shaders/brush_mask.wgsl"),
                include_str!("shaders/stroke_merge.wgsl")
            ),
        );
        let pipeline = gpu::pipeline(
            device,
            PipelineDesc {
                label: "stroke merge",
                shader: &shader,
                layout: &layout,
                format: CANVAS_FORMAT,
                vertex_buffers: &[],
                blend: None,
            },
        );
        Self { pipeline, layout }
    }

    /// `stroke` and `target` cover the same canvas rectangle, with its top-left at `origin`.
    /// `paper` shapes the whole stroke (its view and settings); `empty` is bound otherwise.
    #[allow(clippy::too_many_arguments)]
    pub fn merge(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        atlas: &wgpu::TextureView,
        plane_index: &wgpu::TextureView,
        stroke: &wgpu::TextureView,
        selection_index: Option<&wgpu::TextureView>,
        no_index: &wgpu::TextureView,
        target: &wgpu::TextureView,
        origin: [i32; 2],
        how: MergeOptions,
        paper: Option<(&wgpu::TextureView, &DabTexture)>,
        empty: &wgpu::TextureView,
    ) {
        let texture = paper.map(|(_, t)| t);
        let params = MergeParams {
            opacity: how.opacity,
            erase: u32::from(how.erase),
            alpha_locked: u32::from(how.alpha_locked),
            has_selection: u32::from(selection_index.is_some()),
            blend: how.blend,
            _pad: 0,
            origin,
            has_texture: u32::from(texture.is_some()),
            texture_mode: texture.map_or(0, |t| t.mode.shader_id()),
            texture_scale: texture.map_or(1.0, DabTexture::scale),
            texture_density: texture.map_or(0.0, |t| t.density.clamp(0.0, 1.0)),
            texture_invert: u32::from(texture.is_some_and(|t| t.invert)),
            texture_brightness: texture.map_or(0.0, |t| t.brightness),
            texture_contrast: texture.map_or(1.0, |t| t.contrast),
            _pad1: 0,
        };
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("stroke merge params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stroke merge"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(atlas) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(plane_index) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(stroke) },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(selection_index.unwrap_or(no_index)),
                },
                wgpu::BindGroupEntry { binding: 4, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(paper.map_or(empty, |(v, _)| v)) },
            ],
        });
        let mut pass =
            gpu::begin_pass(encoder, target, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), "stroke merge");
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dab_layouts_match_wgsl() {
        // WGSL `DabParams` is 112 bytes (vec2s at 8-byte offsets, the colour
        // vec4 at 16, size a multiple of 16); `DabIn` takes 60 bytes per
        // instance; `MergeParams` 64.
        assert_eq!(std::mem::size_of::<DabParams>(), 112);
        assert_eq!(std::mem::size_of::<DabInstance>(), 60);
        assert_eq!(std::mem::size_of::<MergeParams>(), 64);
    }
}
