// Live preview of a plane (layer colour or mask) with the in-progress stroke
// applied, limited by the selection. Mirrors painter_core::raster::merge_stroke.

struct MergeParams {
    opacity: f32,
    erase: u32,
    alpha_locked: u32,
    has_selection: u32,
};

@group(0) @binding(0) var atlas: texture_2d_array<f32>;
@group(0) @binding(1) var layer_index: texture_2d<u32>;
@group(0) @binding(2) var stroke: texture_2d<f32>;
@group(0) @binding(3) var selection_index: texture_2d<u32>;
@group(0) @binding(4) var<uniform> params: MergeParams;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = vec2<i32>(position.xy);
    let b = atlas_load(layer_index, texel);
    var selected = 1.0;
    if (params.has_selection != 0u) {
        selected = atlas_load(selection_index, texel).a;
    }
    let s = textureLoad(stroke, texel, 0) * (params.opacity * selected);
    if (params.erase != 0u) {
        return b * (1.0 - s.a);
    }
    if (params.alpha_locked != 0u) {
        // Source-atop: recolour existing pixels, keep their alpha.
        return vec4<f32>(s.rgb * b.a + b.rgb * (1.0 - s.a), b.a);
    }
    return s + b * (1.0 - s.a);
}
