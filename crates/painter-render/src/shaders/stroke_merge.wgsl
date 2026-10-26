// Live preview of a plane (layer colour or mask) with the in-progress stroke
// applied, limited by the selection. Mirrors painter_core::raster::merge_stroke,
// brush blend modes included (ids of painter_core::BrushBlend::shader_id).

struct MergeParams {
    opacity: f32,
    erase: u32,
    alpha_locked: u32,
    has_selection: u32,
    // 0 Normal, 1..=12 the layer blend modes' ids, 100 Behind.
    blend: u32,
    _pad0: u32,
    // Canvas position of the target's (and the stroke texture's) top-left pixel.
    origin: vec2<i32>,
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

// painter_core::blend::BlendMode::channel, per channel.
fn screen_c(cb: f32, cs: f32) -> f32 {
    return cb + cs - cb * cs;
}

fn hard_light_c(cb: f32, cs: f32) -> f32 {
    if (cs <= 0.5) {
        return cb * 2.0 * cs;
    }
    return screen_c(cb, 2.0 * cs - 1.0);
}

fn soft_light_c(cb: f32, cs: f32) -> f32 {
    if (cs <= 0.5) {
        return cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb);
    }
    var d = sqrt(cb);
    if (cb <= 0.25) {
        d = ((16.0 * cb - 12.0) * cb + 4.0) * cb;
    }
    return cb + (2.0 * cs - 1.0) * (d - cb);
}

fn channel(mode: u32, cb: f32, cs: f32) -> f32 {
    switch mode {
        case 1u: { return min(cb, cs); }
        case 2u: { return cb * cs; }
        case 3u: {
            if (cb >= 1.0) { return 1.0; }
            if (cs <= 0.0) { return 0.0; }
            return 1.0 - min((1.0 - cb) / cs, 1.0);
        }
        case 4u: { return max(cb + cs - 1.0, 0.0); }
        case 5u: { return max(cb, cs); }
        case 6u: { return screen_c(cb, cs); }
        case 7u: {
            if (cb <= 0.0) { return 0.0; }
            if (cs >= 1.0) { return 1.0; }
            return min(cb / (1.0 - cs), 1.0);
        }
        case 8u: { return min(cb + cs, 1.0); }
        case 9u: { return hard_light_c(cs, cb); }
        case 10u: { return soft_light_c(cb, cs); }
        case 11u: { return hard_light_c(cb, cs); }
        case 12u: { return abs(cb - cs); }
        default: { return cs; }
    }
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let local = vec2<i32>(position.xy);
    let texel = local + params.origin;
    let b = atlas_load(layer_index, texel);
    var selected = 1.0;
    if (params.has_selection != 0u) {
        selected = atlas_load(selection_index, texel).a;
    }
    var s = textureLoad(stroke, local, 0) * (params.opacity * selected);
    if (s.a <= 0.0) {
        return b;
    }
    if (params.erase != 0u) {
        return b * (1.0 - s.a);
    }
    let locked = params.alpha_locked != 0u;
    // Behind: under what's there.
    if (params.blend == 100u) {
        if (locked) {
            return b;
        }
        return b + s * (1.0 - b.a);
    }
    // A blend mode: the equivalent premultiplied source of the same alpha.
    if (params.blend != 0u && b.a > 0.0) {
        let cs = min(s.rgb / s.a, vec3<f32>(1.0));
        let cb = min(b.rgb / b.a, vec3<f32>(1.0));
        let mixed = vec3<f32>(channel(params.blend, cb.r, cs.r), channel(params.blend, cb.g, cs.g), channel(params.blend, cb.b, cs.b));
        if (locked) {
            s = vec4<f32>(s.a * mixed, s.a);
        } else {
            s = vec4<f32>(s.rgb * (1.0 - b.a) + s.a * b.a * mixed, s.a);
        }
    }
    if (locked) {
        // Source-atop: recolour existing pixels, keep their alpha.
        return vec4<f32>(s.rgb * b.a + b.rgb * (1.0 - s.a), b.a);
    }
    return s + b * (1.0 - s.a);
}
