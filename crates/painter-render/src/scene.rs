//! 3D reference layers on the GPU (`shaders/scene.wgsl`): meshes from
//! `scene_3d` drawn with a `Depth32Float` depth buffer into a canvas-sized
//! colour target. [`crate::GpuCanvas`] shows that target as the layer's
//! plane while the scene lives in the app; [`SceneRenderer::render_to_rgba`]
//! reads it back so the layer's pixels (export, CPU compositing) match.

use std::collections::HashMap;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use painter_core::LayerId;
use scene_3d::mesh::Mesh;
use wgpu::util::DeviceExt;

use crate::gpu::{self, Target, CANVAS_FORMAT, TARGET_USAGE};

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
pub const MAX_LIGHTS: usize = 4;

/// One mesh instance to draw.
#[derive(Clone, Debug)]
pub struct SceneDraw {
    pub mesh: Arc<Mesh>,
    /// Column-major model matrix.
    pub model: [[f32; 4]; 4],
    pub color: [f32; 3],
    /// Highlight the selected object a little.
    pub selected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SceneLight {
    /// Direction the light travels.
    Sun { direction: [f32; 3], color: [f32; 3] },
    Point { position: [f32; 3], color: [f32; 3] },
}

/// A 3D layer for one frame: what to draw and when it last changed.
#[derive(Clone, Debug)]
pub struct ScenePreview {
    pub layer: LayerId,
    /// Changes whenever anything below changes; the layer is redrawn then.
    pub revision: u64,
    pub view_proj: [[f32; 4]; 4],
    pub draws: Vec<SceneDraw>,
    pub lights: Vec<SceneLight>,
    pub ambient: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    light_vec: [[f32; 4]; MAX_LIGHTS],
    light_color: [[f32; 4]; MAX_LIGHTS],
    params: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ObjectUniform {
    model: [[f32; 4]; 4],
    color: [f32; 4],
}

struct MeshBuffers {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
    /// Keeps the mesh alive so its address can't be reused by another one.
    _mesh: Arc<Mesh>,
}

/// Colour and depth targets of one 3D layer.
pub(crate) struct SceneTarget {
    pub color: Target,
    depth: wgpu::TextureView,
    pub revision: u64,
}

pub struct SceneRenderer {
    pipeline: wgpu::RenderPipeline,
    globals_layout: wgpu::BindGroupLayout,
    object_layout: wgpu::BindGroupLayout,
    /// GPU buffers by mesh address, uploaded once.
    meshes: HashMap<usize, MeshBuffers>,
}

impl SceneRenderer {
    pub fn new(device: &wgpu::Device) -> Self {
        let uniform = |label| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries: &[gpu::uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT)],
            })
        };
        let globals_layout = uniform("scene globals");
        let object_layout = uniform("scene object");
        let shader = gpu::shader(device, "scene", include_str!("shaders/scene.wgsl"));
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene"),
            bind_group_layouts: &[&globals_layout, &object_layout],
            push_constant_ranges: &[],
        });
        let attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scene"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout { array_stride: 32, step_mode: wgpu::VertexStepMode::Vertex, attributes: &attributes }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: CANVAS_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            // Both sides: imported meshes aren't always closed or consistently wound.
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        Self { pipeline, globals_layout, object_layout, meshes: HashMap::new() }
    }

    pub(crate) fn target(device: &wgpu::Device, size: (u32, u32)) -> SceneTarget {
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scene depth"),
            size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        SceneTarget {
            color: Target::new(device, size, TARGET_USAGE, "scene colour"),
            depth: depth.create_view(&wgpu::TextureViewDescriptor::default()),
            revision: u64::MAX,
        }
    }

    fn buffers(&mut self, device: &wgpu::Device, mesh: &Arc<Mesh>) -> &MeshBuffers {
        let key = Arc::as_ptr(mesh) as usize;
        self.meshes.entry(key).or_insert_with(|| MeshBuffers {
            vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("scene vertices"),
                contents: mesh.vertex_bytes(),
                usage: wgpu::BufferUsages::VERTEX,
            }),
            indices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("scene indices"),
                contents: mesh.index_bytes(),
                usage: wgpu::BufferUsages::INDEX,
            }),
            count: mesh.indices.len() as u32,
            _mesh: mesh.clone(),
        })
    }

    /// Drops GPU buffers of meshes no scene uses any more.
    pub fn retain_meshes<'a>(&mut self, live: impl IntoIterator<Item = &'a Arc<Mesh>>) {
        let keep: std::collections::HashSet<usize> = live.into_iter().map(|m| Arc::as_ptr(m) as usize).collect();
        self.meshes.retain(|k, _| keep.contains(k));
    }

    pub(crate) fn draw(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, target: &SceneTarget, scene: &ScenePreview) {
        let mut globals = Globals { view_proj: scene.view_proj, light_vec: [[0.0; 4]; MAX_LIGHTS], light_color: [[0.0; 4]; MAX_LIGHTS], params: [0.0; 4] };
        for (i, light) in scene.lights.iter().take(MAX_LIGHTS).enumerate() {
            let (v, w, c) = match *light {
                SceneLight::Sun { direction, color } => (direction, 0.0, color),
                SceneLight::Point { position, color } => (position, 1.0, color),
            };
            globals.light_vec[i] = [v[0], v[1], v[2], w];
            globals.light_color[i] = [c[0], c[1], c[2], 0.0];
        }
        globals.params = [scene.lights.len().min(MAX_LIGHTS) as f32, scene.ambient, 0.0, 0.0];
        let uniform = |label, bytes: &[u8], layout: &wgpu::BindGroupLayout| {
            let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents: bytes, usage: wgpu::BufferUsages::UNIFORM });
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }],
            })
        };
        let globals_group = uniform("scene globals", bytemuck::bytes_of(&globals), &self.globals_layout);
        let objects: Vec<wgpu::BindGroup> = scene
            .draws
            .iter()
            .map(|d| {
                let o = ObjectUniform { model: d.model, color: [d.color[0], d.color[1], d.color[2], if d.selected { 0.08 } else { 0.0 }] };
                uniform("scene object", bytemuck::bytes_of(&o), &self.object_layout)
            })
            .collect();
        for d in &scene.draws {
            self.buffers(device, &d.mesh);
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.color.view,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &target.depth,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &globals_group, &[]);
        for (d, group) in scene.draws.iter().zip(&objects) {
            let Some(buffers) = self.meshes.get(&(Arc::as_ptr(&d.mesh) as usize)) else { continue };
            if buffers.count == 0 {
                continue;
            }
            pass.set_bind_group(1, group, &[]);
            pass.set_vertex_buffer(0, buffers.vertices.slice(..));
            pass.set_index_buffer(buffers.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..buffers.count, 0, 0..1);
        }
    }

    /// Renders `scene` at `size` and reads it back as premultiplied RGBA8:
    /// the layer's pixels for saving, export and CPU compositing.
    pub fn render_to_rgba(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, scene: &ScenePreview, size: (u32, u32)) -> Vec<u8> {
        let target = Self::target(device, size);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("scene readback") });
        self.draw(device, &mut encoder, &target, scene);
        queue.submit([encoder.finish()]);
        gpu::read_texture(device, queue, &target.color.texture, size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniforms_match_wgsl() {
        assert_eq!(std::mem::size_of::<Globals>(), 208);
        assert_eq!(std::mem::size_of::<ObjectUniform>(), 80);
    }
}
