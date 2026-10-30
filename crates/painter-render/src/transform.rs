//! GPU resampling through a projective transform (`shaders/transform.wgsl`)
//! or a warp mesh (`shaders/transform_mesh.wgsl`): the live preview of the
//! transform tools, and a GPU path for committing. `transform_tools::resample`
//! is the CPU reference it matches.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use painter_core::LayerId;
use wgpu::util::DeviceExt;

use crate::gpu::{self, PipelineDesc, Target, CANVAS_FORMAT, TARGET_USAGE};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TransformParams {
    inv: [[f32; 4]; 3],
    filter: u32,
    _pad: u32,
    target_size: [f32; 2],
}

/// How the floating pixels are placed on the canvas.
#[derive(Clone, Debug, PartialEq)]
pub enum TransformGeometry {
    /// Canvas pixels → source pixels (row-major inverse homography).
    Inverse([[f32; 3]; 3]),
    /// Triangles: canvas positions, source positions, indices.
    Mesh { positions: Vec<[f32; 2]>, sources: Vec<[f32; 2]>, indices: Vec<u32> },
}

/// One frame of an open transform for [`crate::GpuCanvas`]: the layer's
/// colour plane is shown as its remaining pixels with the transformed
/// floating pixels drawn over them.
pub struct TransformPreview {
    /// Changes when the floating pixels change; the source is uploaded once per id.
    pub id: u64,
    /// Changes whenever anything below changes; the preview is redrawn then.
    pub revision: u64,
    pub layer: LayerId,
    /// The floating pixels are the layer's mask (`Plane::Mask`), not its colour.
    pub mask: bool,
    /// Premultiplied RGBA8 floating pixels and their size.
    pub source: Arc<Vec<u8>>,
    pub source_size: (u32, u32),
    pub geometry: TransformGeometry,
    /// `transform_tools::Interpolation::shader_id`.
    pub filter: u32,
}

/// Draws a premultiplied RGBA8 source through a 3×3 inverse homography or a mesh.
pub struct TransformRenderer {
    pipeline: wgpu::RenderPipeline,
    mesh_pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

const MESH_ATTRIBUTES: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2];

impl TransformRenderer {
    pub fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("transform"),
            entries: &[gpu::uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT), gpu::texture_entry(1)],
        });
        let shader = gpu::shader(
            device,
            "transform",
            concat!(include_str!("shaders/transform_sample.wgsl"), include_str!("shaders/transform.wgsl")),
        );
        let pipeline = gpu::pipeline(
            device,
            PipelineDesc { label: "transform", shader: &shader, layout: &layout, format: CANVAS_FORMAT, vertex_buffers: &[], blend: None },
        );
        let mesh_shader = gpu::shader(
            device,
            "transform mesh",
            concat!(include_str!("shaders/transform_sample.wgsl"), include_str!("shaders/transform_mesh.wgsl")),
        );
        let mesh_pipeline = gpu::pipeline(
            device,
            PipelineDesc {
                label: "transform mesh",
                shader: &mesh_shader,
                layout: &layout,
                format: CANVAS_FORMAT,
                vertex_buffers: &[wgpu::VertexBufferLayout {
                    array_stride: 16,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &MESH_ATTRIBUTES,
                }],
                // Overlapping folds of a warp stack like paper.
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
            },
        );
        Self { pipeline, mesh_pipeline, layout }
    }

    fn bind_group(&self, device: &wgpu::Device, source: &wgpu::TextureView, params: TransformParams) -> wgpu::BindGroup {
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("transform params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("transform"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(source) },
            ],
        })
    }

    /// Clears `target` (`target_size` pixels) and draws `source` into it.
    /// `filter` is `transform_tools::Interpolation::shader_id` (0 nearest,
    /// 1 bilinear, 2 bicubic, 3 Lanczos3).
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        target_size: (u32, u32),
        geometry: &TransformGeometry,
        filter: u32,
    ) {
        let inverse = match geometry {
            TransformGeometry::Inverse(m) => *m,
            TransformGeometry::Mesh { .. } => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        };
        let params = TransformParams {
            inv: inverse.map(|r| [r[0], r[1], r[2], 0.0]),
            filter,
            _pad: 0,
            target_size: [target_size.0 as f32, target_size.1 as f32],
        };
        let bind_group = self.bind_group(device, source, params);
        let mesh_buffers = match geometry {
            TransformGeometry::Mesh { positions, sources, indices } if !indices.is_empty() => {
                let vertices: Vec<f32> = positions.iter().zip(sources).flat_map(|(p, s)| [p[0], p[1], s[0], s[1]]).collect();
                let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("warp vertices"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                let ib = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("warp indices"),
                    contents: bytemuck::cast_slice(indices),
                    usage: wgpu::BufferUsages::INDEX,
                });
                Some((vb, ib, indices.len() as u32))
            }
            _ => None,
        };
        let mut pass = gpu::begin_pass(encoder, target, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), "transform");
        pass.set_bind_group(0, &bind_group, &[]);
        match (geometry, &mesh_buffers) {
            (TransformGeometry::Inverse(_), _) => {
                pass.set_pipeline(&self.pipeline);
                pass.draw(0..3, 0..1);
            }
            (TransformGeometry::Mesh { .. }, Some((vb, ib, count))) => {
                pass.set_pipeline(&self.mesh_pipeline);
                pass.set_vertex_buffer(0, vb.slice(..));
                pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..*count, 0, 0..1);
            }
            _ => {}
        }
    }

    /// A texture holding `rgba` (premultiplied RGBA8).
    pub(crate) fn upload(device: &wgpu::Device, queue: &wgpu::Queue, rgba: &[u8], size: (u32, u32)) -> Target {
        let target = Target::new(device, size, TARGET_USAGE, "transform source");
        queue.write_texture(
            target.texture.as_image_copy(),
            rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(size.0 * 4), rows_per_image: Some(size.1) },
            wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
        );
        target
    }

    /// Uploads `source` (premultiplied RGBA8), renders it into a
    /// `target_size` image and reads it back. For committing a transform on
    /// the GPU, and for tests.
    #[allow(clippy::too_many_arguments)]
    pub fn render_to_rgba(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &[u8],
        source_size: (u32, u32),
        target_size: (u32, u32),
        geometry: &TransformGeometry,
        filter: u32,
    ) -> Vec<u8> {
        let src = Self::upload(device, queue, source, source_size);
        let dst = Target::new(device, target_size, TARGET_USAGE, "transform target");
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("transform") });
        self.draw(device, &mut encoder, &src.view, &dst.view, target_size, geometry, filter);
        queue.submit([encoder.finish()]);
        gpu::read_texture(device, queue, &dst.texture, target_size)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn params_match_wgsl() {
        // WGSL `Params`: three vec4, two u32, a vec2 = 64 bytes.
        assert_eq!(std::mem::size_of::<super::TransformParams>(), 64);
    }
}
