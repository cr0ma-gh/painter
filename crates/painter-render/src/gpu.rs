//! Small wgpu helpers shared by the canvas passes.

/// Pixels stay sRGB-encoded end to end; blending happens on encoded values.
pub(crate) const CANVAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Stroke accumulation needs more than 8 bits, or faint dabs stall (matches the CPU's f32 buffer).
pub(crate) const STROKE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Low-VRAM stroke accumulation: half the bytes of [`STROKE_FORMAT`], dithered so faint dabs still build up.
pub(crate) const STROKE_FORMAT_8BIT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

pub(crate) const TARGET_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::TEXTURE_BINDING
    .union(wgpu::TextureUsages::RENDER_ATTACHMENT)
    .union(wgpu::TextureUsages::COPY_SRC)
    .union(wgpu::TextureUsages::COPY_DST);

/// Cached intermediate composites: filled by copy, read by blend passes, and restored by copy.
pub(crate) const CACHE_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::TEXTURE_BINDING
    .union(wgpu::TextureUsages::COPY_DST)
    .union(wgpu::TextureUsages::COPY_SRC);

pub(crate) struct Target {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
}

impl Target {
    pub fn new(device: &wgpu::Device, size: (u32, u32), usage: wgpu::TextureUsages, label: &str) -> Self {
        Self::with_format(device, size, CANVAS_FORMAT, usage, label)
    }

    pub fn with_format(
        device: &wgpu::Device,
        (width, height): (u32, u32),
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
        label: &str,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { texture, view }
    }

    /// GPU bytes held by the texture's pixels.
    pub fn bytes(&self) -> u64 {
        let texel = self.texture.format().block_copy_size(None).unwrap_or(4) as u64;
        self.texture.width() as u64 * self.texture.height() as u64 * texel
    }
}

/// A rectangle of canvas pixels: where an intermediate target sits on the canvas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    /// The whole canvas.
    pub fn full((width, height): (u32, u32)) -> Self {
        Self { x: 0, y: 0, width, height }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn origin(&self) -> [i32; 2] {
        [self.x as i32, self.y as i32]
    }

    pub fn area(&self) -> u64 {
        self.width as u64 * self.height as u64
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub fn contains(&self, other: &Region) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.x + other.width <= self.x + self.width
            && other.y + other.height <= self.y + self.height
    }

    pub fn union(&self, other: &Region) -> Region {
        let (x, y) = (self.x.min(other.x), self.y.min(other.y));
        let right = (self.x + self.width).max(other.x + other.width);
        let bottom = (self.y + self.height).max(other.y + other.height);
        Region { x, y, width: right - x, height: bottom - y }
    }

    /// The canvas pixels touched by `[min_x, min_y, max_x, max_y]` (canvas
    /// coordinates, possibly fractional or off-canvas) grown by `margin`, snapped
    /// outwards to a `snap`-pixel grid and clipped to the canvas. `None` when
    /// nothing of it is on the canvas.
    pub fn covering(bounds: [f32; 4], margin: f32, snap: u32, (width, height): (u32, u32)) -> Option<Region> {
        let lo = |v: f32, max: u32| ((v - margin).floor().max(0.0) as u32 / snap * snap).min(max);
        let hi = |v: f32, max: u32| ((v + margin).ceil().max(0.0) as u32).div_ceil(snap).saturating_mul(snap).min(max);
        let (x0, y0) = (lo(bounds[0], width), lo(bounds[1], height));
        let (x1, y1) = (hi(bounds[2], width), hi(bounds[3], height));
        (x1 > x0 && y1 > y0).then(|| Region { x: x0, y: y0, width: x1 - x0, height: y1 - y0 })
    }
}

pub(crate) fn begin_pass<'e>(
    encoder: &'e mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    load: wgpu::LoadOp<wgpu::Color>,
    label: &str,
) -> wgpu::RenderPass<'e> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
    })
}

pub(crate) fn copy_texture(encoder: &mut wgpu::CommandEncoder, src: &Target, dst: &Target, (width, height): (u32, u32)) {
    encoder.copy_texture_to_texture(
        src.texture.as_image_copy(),
        dst.texture.as_image_copy(),
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
}

/// Copies the pixels of canvas rectangle `area` from `src` (placed at `src_at`)
/// to `dst` (placed at `dst_at`); `area` must lie inside both.
pub(crate) fn copy_region(
    encoder: &mut wgpu::CommandEncoder,
    src: &Target,
    src_at: &Region,
    dst: &Target,
    dst_at: &Region,
    area: &Region,
) {
    let origin = |at: &Region| wgpu::Origin3d { x: area.x - at.x, y: area.y - at.y, z: 0 };
    encoder.copy_texture_to_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &src.texture,
            mip_level: 0,
            origin: origin(src_at),
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyTextureInfo {
            texture: &dst.texture,
            mip_level: 0,
            origin: origin(dst_at),
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::Extent3d { width: area.width, height: area.height, depth_or_array_layers: 1 },
    );
}

pub(crate) fn clear(encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
    drop(begin_pass(encoder, view, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), "clear"));
}

pub(crate) fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

pub(crate) fn texture_array_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        },
        count: None,
    }
}

/// An `R32Uint` plane indirection table.
pub(crate) fn index_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Uint,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

pub(crate) fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

pub(crate) struct PipelineDesc<'a> {
    pub label: &'a str,
    pub shader: &'a wgpu::ShaderModule,
    pub layout: &'a wgpu::BindGroupLayout,
    pub format: wgpu::TextureFormat,
    pub vertex_buffers: &'a [wgpu::VertexBufferLayout<'a>],
    pub blend: Option<wgpu::BlendState>,
}

pub(crate) fn pipeline(device: &wgpu::Device, desc: PipelineDesc<'_>) -> wgpu::RenderPipeline {
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(desc.label),
        bind_group_layouts: &[desc.layout],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(desc.label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: desc.shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: desc.vertex_buffers,
        },
        fragment: Some(wgpu::FragmentState {
            module: desc.shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: desc.format,
                blend: desc.blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    })
}

pub(crate) fn shader(device: &wgpu::Device, label: &str, source: &str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    })
}

/// Blocking readback of an RGBA8 texture into tightly packed rows.
pub(crate) fn read_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    (width, height): (u32, u32),
) -> Vec<u8> {
    let row = width * 4;
    let padded_row = row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded_row * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |result| result.expect("failed to map readback buffer"));
    device.poll(wgpu::Maintain::Wait);
    let mapped = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((row * height) as usize);
    for y in 0..height as usize {
        let start = y * padded_row as usize;
        pixels.extend_from_slice(&mapped[start..start + row as usize]);
    }
    pixels
}
