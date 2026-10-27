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
use scene_3d::texture::Texture;
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
    /// Painted over the colour through the UVs.
    pub texture: Option<Arc<Texture>>,
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
    /// x: 1 when textured.
    params: [f32; 4],
}

struct MeshBuffers {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
    /// Watches the mesh without keeping it alive: the buffers are dropped once
    /// it is (posing makes a new mesh per change), and the weak count keeps its
    /// address from being reused by another mesh meanwhile.
    mesh: std::sync::Weak<Mesh>,
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
    /// GPU buffers by mesh address, uploaded once; dropped once the mesh is.
    meshes: HashMap<usize, MeshBuffers>,
    /// Uploaded textures by address; dropped once the texture is (painting makes new ones).
    textures: HashMap<usize, (std::sync::Weak<Texture>, wgpu::TextureView)>,
    /// Bound when an object has no texture (made on the first draw).
    blank: Option<wgpu::TextureView>,
    sampler: wgpu::Sampler,
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
        let object_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene object"),
            entries: &[
                gpu::uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT),
                gpu::texture_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
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
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("scene texture"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self { pipeline, globals_layout, object_layout, meshes: HashMap::new(), textures: HashMap::new(), blank: None, sampler }
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
            mesh: Arc::downgrade(mesh),
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
        // Meshes: drop the buffers of the ones no longer alive. Anything this
        // or an earlier draw in the open encoder used is still held by its scene.
        self.meshes.retain(|_, b| b.mesh.strong_count() > 0);
        // Textures: drop the ones no longer alive, upload new ones.
        self.textures.retain(|_, (weak, _)| weak.strong_count() > 0);
        if self.blank.is_none() {
            self.blank = Some(upload_texture(device, encoder, &Texture::new(1, 1)));
        }
        for t in scene.draws.iter().filter_map(|d| d.texture.as_ref()) {
            let key = Arc::as_ptr(t) as usize;
            if let std::collections::hash_map::Entry::Vacant(slot) = self.textures.entry(key) {
                slot.insert((Arc::downgrade(t), upload_texture(device, encoder, t)));
            }
        }
        let objects: Vec<wgpu::BindGroup> = scene
            .draws
            .iter()
            .map(|d| {
                let o = ObjectUniform {
                    model: d.model,
                    color: [d.color[0], d.color[1], d.color[2], if d.selected { 0.08 } else { 0.0 }],
                    params: [d.texture.is_some() as u8 as f32, 0.0, 0.0, 0.0],
                };
                let view = d
                    .texture
                    .as_ref()
                    .and_then(|t| self.textures.get(&(Arc::as_ptr(t) as usize)))
                    .map(|(_, v)| v)
                    .or(self.blank.as_ref())
                    .expect("blank texture made above");
                let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("scene object"),
                    contents: bytemuck::bytes_of(&o),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("scene object"),
                    layout: &self.object_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(view) },
                        wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    ],
                })
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

/// A texture on the GPU (premultiplied, so filtering doesn't darken painted edges),
/// copied in through `encoder`.
fn upload_texture(device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, t: &Texture) -> wgpu::TextureView {
    let size = wgpu::Extent3d { width: t.width, height: t.height, depth_or_array_layers: 1 };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("scene texture"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: CANVAS_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let row = t.width * 4;
    let padded = row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let mut data = vec![0u8; (padded * t.height) as usize];
    for y in 0..t.height as usize {
        let src = &t.rgba[y * row as usize..(y + 1) * row as usize];
        let dst = &mut data[y * padded as usize..y * padded as usize + row as usize];
        for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
            let a = s[3] as u32;
            for c in 0..3 {
                d[c] = ((s[c] as u32 * a + 127) / 255) as u8;
            }
            d[3] = s[3];
        }
    }
    let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("scene texture data"), contents: &data, usage: wgpu::BufferUsages::COPY_SRC });
    encoder.copy_buffer_to_texture(
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(t.height) },
        },
        texture.as_image_copy(),
        size,
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniforms_match_wgsl() {
        assert_eq!(std::mem::size_of::<Globals>(), 208);
        assert_eq!(std::mem::size_of::<ObjectUniform>(), 96);
    }

    fn preview(meshes: &[Arc<Mesh>]) -> ScenePreview {
        let identity = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
        ScenePreview {
            layer: LayerId(1),
            revision: 1,
            view_proj: identity,
            draws: meshes.iter().map(|m| SceneDraw { mesh: m.clone(), model: identity, color: [1.0; 3], selected: false, texture: None }).collect(),
            lights: Vec::new(),
            ambient: 1.0,
        }
    }

    /// Posing makes a new mesh per change: buffers of dropped meshes must go
    /// on the next draw. Skipped without a GPU.
    #[test]
    fn dropped_meshes_release_their_buffers() {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let Some(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        let Ok((device, queue)) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None)) else {
            eprintln!("skipped: no GPU device");
            return;
        };
        let mut renderer = SceneRenderer::new(&device);
        let kept = Arc::new(Mesh::cube());
        for _ in 0..4 {
            let posed = Arc::new(Mesh::cube());
            renderer.render_to_rgba(&device, &queue, &preview(&[kept.clone(), posed]), (8, 8));
        }
        // Each frame's posed mesh is gone by the next one.
        assert_eq!(renderer.meshes.len(), 2);
        renderer.render_to_rgba(&device, &queue, &preview(std::slice::from_ref(&kept)), (8, 8));
        assert_eq!(renderer.meshes.len(), 1);
        assert!(renderer.meshes.contains_key(&(Arc::as_ptr(&kept) as usize)), "the live mesh keeps its buffers");
        drop(kept);
        renderer.render_to_rgba(&device, &queue, &preview(&[]), (8, 8));
        assert!(renderer.meshes.is_empty());
    }
}
