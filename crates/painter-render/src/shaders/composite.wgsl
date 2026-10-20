// Blends one layer (premultiplied RGBA) onto the backdrop with a separable
// blend mode, after applying its mask, clipping and opacity.
// Mirrors painter_core::blend and io::blend_source; keep them in sync.

struct Params {
    mode: u32,
    opacity: f32,
    // 0 atlas plane, 1 direct texture, 2 solid, 3 gradient, 4 tone, 5 adjustment
    source_kind: u32,
    // 0 none, 1 atlas plane, 2 direct texture
    mask_kind: u32,
    clip_kind: u32,
    // gradient: shape | adjustment: 0 channel table, 1 luminance table, 2 HSL
    gradient_shape: u32,
    _pad0: u32,
    _pad1: u32,
    // solid colour (premultiplied) | gradient start colour | tone colour (straight)
    color0: vec4<f32>,
    // gradient end colour (straight)
    color1: vec4<f32>,
    // gradient: start.xy, end.xy | tone: cell, angle (radians), density | HSL: hue (radians), saturation, lightness
    geom: vec4<f32>,
};

@group(0) @binding(0) var backdrop: texture_2d<f32>;
@group(0) @binding(1) var atlas: texture_2d_array<f32>;
@group(0) @binding(2) var source_index: texture_2d<u32>;
@group(0) @binding(3) var source_direct: texture_2d<f32>;
@group(0) @binding(4) var mask_index: texture_2d<u32>;
@group(0) @binding(5) var mask_direct: texture_2d<f32>;
@group(0) @binding(6) var clip_index: texture_2d<u32>;
@group(0) @binding(7) var clip_direct: texture_2d<f32>;
@group(0) @binding(8) var<uniform> params: Params;

// Fullscreen triangle.
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

fn screen(cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    return cb + cs - cb * cs;
}

fn hard_light(cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    return select(screen(cb, 2.0 * cs - 1.0), cb * 2.0 * cs, cs <= vec3<f32>(0.5));
}

fn soft_light(cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    let d = select(sqrt(cb), ((16.0 * cb - 12.0) * cb + 4.0) * cb, cb <= vec3<f32>(0.25));
    return select(cb + (2.0 * cs - 1.0) * (d - cb), cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb), cs <= vec3<f32>(0.5));
}

fn color_dodge(cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    let v = min(vec3<f32>(1.0), cb / max(1.0 - cs, vec3<f32>(1e-6)));
    return select(select(v, vec3<f32>(1.0), cs >= vec3<f32>(1.0)), vec3<f32>(0.0), cb <= vec3<f32>(0.0));
}

fn color_burn(cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    let v = 1.0 - min(vec3<f32>(1.0), (1.0 - cb) / max(cs, vec3<f32>(1e-6)));
    return select(select(v, vec3<f32>(0.0), cs <= vec3<f32>(0.0)), vec3<f32>(1.0), cb >= vec3<f32>(1.0));
}

// Mode ids match painter_core::BlendMode.
fn blend(cb: vec3<f32>, cs: vec3<f32>, mode: u32) -> vec3<f32> {
    var result = cs;
    switch mode {
        case 1u: { result = min(cb, cs); }
        case 2u: { result = cb * cs; }
        case 3u: { result = color_burn(cb, cs); }
        case 4u: { result = max(cb + cs - 1.0, vec3<f32>(0.0)); }
        case 5u: { result = max(cb, cs); }
        case 6u: { result = screen(cb, cs); }
        case 7u: { result = color_dodge(cb, cs); }
        case 8u: { result = min(cb + cs, vec3<f32>(1.0)); }
        case 9u: { result = hard_light(cs, cb); }
        case 10u: { result = soft_light(cb, cs); }
        case 11u: { result = hard_light(cb, cs); }
        case 12u: { result = abs(cb - cs); }
        default: {}
    }
    return result;
}

fn unpremultiply(c: vec4<f32>) -> vec3<f32> {
    if (c.a <= 0.0) {
        return vec3<f32>(0.0);
    }
    return min(c.rgb / c.a, vec3<f32>(1.0));
}

// painter_core::procedural::Gradient::sample
fn gradient(p: vec2<f32>) -> vec4<f32> {
    let d = params.geom.zw - params.geom.xy;
    let q = p - params.geom.xy;
    let len2 = max(dot(d, d), 1e-6);
    var t = dot(q, d) / len2;
    if (params.gradient_shape == 1u) {
        t = sqrt(dot(q, q) / len2);
    }
    t = clamp(t, 0.0, 1.0);
    let c = mix(params.color0, params.color1, t);
    return vec4<f32>(c.rgb * c.a, c.a);
}

// painter_core::procedural::Tone::sample
fn tone(p: vec2<f32>) -> vec4<f32> {
    let cell = max(params.geom.x, 2.0);
    let s = sin(params.geom.y);
    let c = cos(params.geom.y);
    let q = vec2<f32>(p.x * c + p.y * s, -p.x * s + p.y * c);
    let f = fract(q / cell) - 0.5;
    let d = length(f) * cell;
    let r = cell * sqrt(clamp(params.geom.z, 0.0, 1.0) / 3.14159265);
    let a = clamp(r - d + 0.5, 0.0, 1.0);
    return vec4<f32>(params.color0.rgb * a, a);
}

// painter_core::adjust: the lookup table sits in `source_direct` (256 × 1).
fn lut(i: u32) -> vec4<f32> {
    return textureLoad(source_direct, vec2<i32>(i32(min(i, 255u)), 0), 0);
}

fn rgb_to_hsl(c: vec3<f32>) -> vec3<f32> {
    let mx = max(max(c.r, c.g), c.b);
    let mn = min(min(c.r, c.g), c.b);
    let l = (mx + mn) * 0.5;
    let d = mx - mn;
    if (d <= 1e-6) {
        return vec3<f32>(0.0, 0.0, l);
    }
    let s = select(d / (mx + mn), d / (2.0 - mx - mn), l > 0.5);
    var h = 0.0;
    if (mx == c.r) {
        h = (c.g - c.b) / d + select(0.0, 6.0, c.g < c.b);
    } else if (mx == c.g) {
        h = (c.b - c.r) / d + 2.0;
    } else {
        h = (c.r - c.g) / d + 4.0;
    }
    return vec3<f32>(h / 6.0, s, l);
}

fn hue_channel(p: f32, q: f32, t0: f32) -> f32 {
    let t = t0 - floor(t0);
    if (t < 1.0 / 6.0) {
        return p + (q - p) * 6.0 * t;
    }
    if (t < 0.5) {
        return q;
    }
    if (t < 2.0 / 3.0) {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    return p;
}

fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    if (hsl.y <= 1e-6) {
        return vec3<f32>(hsl.z);
    }
    let q = select(hsl.z + hsl.y - hsl.z * hsl.y, hsl.z * (1.0 + hsl.y), hsl.z < 0.5);
    let p = 2.0 * hsl.z - q;
    return vec3<f32>(hue_channel(p, q, hsl.x + 1.0 / 3.0), hue_channel(p, q, hsl.x), hue_channel(p, q, hsl.x - 1.0 / 3.0));
}

// Table indices in integers, exactly as AdjustOp::apply_pixel computes them.
fn adjust(b: vec4<f32>) -> vec3<f32> {
    let ai = u32(round(b.a * 255.0));
    let ci = vec3<u32>(round(b.rgb * 255.0));
    let si = min((ci * 255u + vec3<u32>(ai / 2u)) / max(ai, 1u), vec3<u32>(255u));
    let c = unpremultiply(b);
    switch params.gradient_shape {
        case 0u: { return vec3<f32>(lut(si.r).r, lut(si.g).g, lut(si.b).b); }
        case 1u: { return lut((299u * si.r + 587u * si.g + 114u * si.b + 500u) / 1000u).rgb; }
        default: {
            let hsl = rgb_to_hsl(c);
            let h = fract(hsl.x + params.geom.x / 6.28318530718);
            let s = clamp(hsl.y * (1.0 + params.geom.y), 0.0, 1.0);
            let lt = params.geom.z;
            let l = clamp(select(hsl.z * (1.0 + lt), hsl.z + (1.0 - hsl.z) * lt, lt >= 0.0), 0.0, 1.0);
            return hsl_to_rgb(vec3<f32>(h, s, l));
        }
    }
}

fn coverage(texel: vec2<i32>) -> f32 {
    var k = params.opacity;
    if (params.mask_kind == 1u) {
        k = k * (1.0 - atlas_load(mask_index, texel).a);
    } else if (params.mask_kind == 2u) {
        k = k * (1.0 - textureLoad(mask_direct, texel, 0).a);
    }
    if (params.clip_kind == 1u) {
        k = k * atlas_load(clip_index, texel).a;
    } else if (params.clip_kind == 2u) {
        k = k * textureLoad(clip_direct, texel, 0).a;
    }
    return k;
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = vec2<i32>(position.xy);
    let b = textureLoad(backdrop, texel, 0);
    // A correction layer: recolour the backdrop, keep its alpha (io::apply_adjustment).
    if (params.source_kind == 5u) {
        if (b.a <= 0.0) {
            return b;
        }
        let k = coverage(texel);
        if (k <= 0.0) {
            return b;
        }
        let a = adjust(b);
        return vec4<f32>(b.rgb + (a * b.a - b.rgb) * k, b.a);
    }
    var s = vec4<f32>(0.0);
    switch params.source_kind {
        case 0u: { s = atlas_load(source_index, texel); }
        case 1u: { s = textureLoad(source_direct, texel, 0); }
        case 2u: { s = params.color0; }
        case 3u: { s = gradient(position.xy); }
        default: { s = tone(position.xy); }
    }
    if (s.a <= 0.0) {
        return b;
    }
    let k = coverage(texel);
    if (k <= 0.0) {
        return b;
    }
    s = s * k;
    let mixed = blend(unpremultiply(b), unpremultiply(s), params.mode);
    let rgb = s.rgb * (1.0 - b.a) + b.rgb * (1.0 - s.a) + s.a * b.a * mixed;
    return vec4<f32>(rgb, s.a + b.a * (1.0 - s.a));
}
