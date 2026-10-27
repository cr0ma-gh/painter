// Instanced brush dabs drawn into the stroke texture (premultiplied), which covers
// the canvas or, for low-VRAM profiles, the rectangle at `origin`.
// Blending is premultiplied source-over, so each dab does
//   dst = color·a + dst·(1 − a)
// exactly like painter_core::raster::stamp_dab. `coverage`, `ellipse_coverage`
// and `sampled_coverage` mirror the CPU ones; each dab's reach and mip level
// come from painter_core::DabTip, computed on the CPU.

struct DabParams {
    // Size of the stroke texture in pixels.
    canvas_size: vec2<f32>,
    hardness: f32,
    aa: f32,
    color: vec4<f32>,
    // 1: dither coverage by ±½ of an 8-bit step (Rgba8Unorm stroke targets).
    dither: f32,
    // 2 sampled; otherwise round: a disc or an ellipse by the dab's flatness
    // (painter_core::DabTip::kind_for).
    kind: u32,
    // Sampled tip width and height over its longer side.
    tip_aspect: vec2<f32>,
    dual_aspect: vec2<f32>,
    // Paper texture (painter_core::DabTexture): canvas px per texel, density, invert.
    texture_scale: f32,
    texture_density: f32,
    texture_invert: u32,
    has_texture: u32,
    // Dual tip (painter_core::DualTip): diameter relative to the dab.
    dual_scale: f32,
    has_dual: u32,
    // Canvas position of the stroke texture's top-left pixel.
    origin: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> params: DabParams;
// R8Unorm tip bitmap with mips (1×1 placeholder for round tips).
@group(0) @binding(1) var tip: texture_2d<f32>;
// R8Unorm paper texture (level 0 sampled, wrapped).
@group(0) @binding(2) var paper: texture_2d<f32>;
// R8Unorm dual tip with mips.
@group(0) @binding(3) var dual_tip: texture_2d<f32>;

struct DabIn {
    @location(0) center: vec2<f32>,
    @location(1) radius: f32,
    @location(2) alpha: f32,
    @location(3) reach: f32,
    @location(4) level: u32,
    @location(5) dual_level: u32,
    // [cos, sin] of the tip angle plus the dab's own rotation.
    @location(6) rotation: vec2<f32>,
    // Tip thickness × the dab's own (painter_core::DabTip::flatness_for),
    // negative for a mirrored dab (painter_core::Dab::mirror).
    @location(7) flatness: f32,
};

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) center: vec2<f32>,
    @location(1) @interpolate(flat) radius: f32,
    @location(2) @interpolate(flat) alpha: f32,
    @location(3) @interpolate(flat) level: u32,
    @location(4) @interpolate(flat) dual_level: u32,
    @location(5) @interpolate(flat) rotation: vec2<f32>,
    @location(6) @interpolate(flat) flatness: f32,
    // -1 mirrors the tip left-right, else 1.
    @location(7) @interpolate(flat) mirror: f32,
    // The dab's index in the draw, varying the dither from dab to dab.
    @location(8) @interpolate(flat) seed: u32,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32, @builtin(instance_index) instance: u32, dab: DabIn) -> VertexOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    );
    // One extra pixel so every pixel centre with non-zero coverage is rasterised.
    let reach = dab.reach + 1.0;
    let p = dab.center + corners[index] * reach - params.origin;
    var out: VertexOut;
    out.position = vec4<f32>(p.x / params.canvas_size.x * 2.0 - 1.0, 1.0 - p.y / params.canvas_size.y * 2.0, 0.0, 1.0);
    out.center = dab.center;
    out.radius = dab.radius;
    out.alpha = dab.alpha;
    out.level = dab.level;
    out.dual_level = dab.dual_level;
    out.rotation = dab.rotation;
    out.flatness = abs(dab.flatness);
    out.mirror = select(1.0, -1.0, dab.flatness < 0.0);
    out.seed = instance;
    return out;
}

fn coverage(d: f32, radius: f32, hardness: f32, aa: f32) -> f32 {
    var edge = 0.0;
    if (aa <= 0.0) {
        edge = select(0.0, 1.0, d <= radius);
    } else {
        edge = clamp((radius - d) / aa + 0.5, 0.0, 1.0);
    }
    if (edge <= 0.0) {
        return 0.0;
    }
    let inner = radius * clamp(hardness, 0.0, 1.0);
    var soft = 1.0;
    if (d > inner && radius - inner > 1e-3) {
        let t = clamp((d - inner) / (radius - inner), 0.0, 1.0);
        soft = 1.0 - t * t * (3.0 - 2.0 * t);
    }
    return edge * soft;
}

fn ellipse_coverage(p: vec2<f32>, radius: f32, flatness: f32, hardness: f32, aa: f32) -> f32 {
    let n = length(p) / radius;
    var edge = 0.0;
    if (aa <= 0.0) {
        edge = select(0.0, 1.0, n <= 1.0);
    } else {
        let gradient = length(vec2<f32>(p.x, p.y / flatness));
        if (gradient <= 0.0) {
            edge = 1.0;
        } else {
            let circle = length(vec2<f32>(p.x, p.y * flatness)) - radius;
            let distance = max((n - 1.0) * radius * radius * n / gradient, circle);
            edge = clamp(0.5 - distance / aa, 0.0, 1.0);
        }
    }
    if (edge <= 0.0) {
        return 0.0;
    }
    let inner = clamp(hardness, 0.0, 1.0);
    var soft = 1.0;
    if (n > inner && radius * (1.0 - inner) > 1e-3) {
        let t = clamp((n - inner) / (1.0 - inner), 0.0, 1.0);
        soft = 1.0 - t * t * (3.0 - 2.0 * t);
    }
    return edge * soft;
}

fn bitmap_texel(bitmap: texture_2d<f32>, i: i32, j: i32, level: u32, size: vec2<i32>) -> f32 {
    if (i < 0 || j < 0 || i >= size.x || j >= size.y) {
        return 0.0;
    }
    return textureLoad(bitmap, vec2<i32>(i, j), i32(level)).r;
}

// painter_core::raster::sampled_coverage: the bitmap's longer side spans the dab diameter.
fn sampled_coverage(bitmap: texture_2d<f32>, aspect: vec2<f32>, p: vec2<f32>, radius: f32, level: u32) -> f32 {
    let size = vec2<i32>(textureDimensions(bitmap, level));
    let fx = p.x / (2.0 * radius * aspect.x) + 0.5;
    let fy = p.y / (2.0 * radius * aspect.y) + 0.5;
    let x = fx * f32(size.x) - 0.5;
    let y = fy * f32(size.y) - 0.5;
    if (x <= -1.0 || y <= -1.0 || x >= f32(size.x) || y >= f32(size.y)) {
        return 0.0;
    }
    let x0 = floor(x);
    let y0 = floor(y);
    let tx = x - x0;
    let ty = y - y0;
    let i = i32(x0);
    let j = i32(y0);
    let a = bitmap_texel(bitmap, i, j, level, size);
    let b = bitmap_texel(bitmap, i + 1, j, level, size);
    let c = bitmap_texel(bitmap, i, j + 1, level, size);
    let d = bitmap_texel(bitmap, i + 1, j + 1, level, size);
    let top = a + (b - a) * tx;
    let bottom = c + (d - c) * tx;
    return top + (bottom - top) * ty;
}

fn wrap(i: i32, n: i32) -> i32 {
    return ((i % n) + n) % n;
}

// painter_core::DabTexture::factor: wrapped bilinear paper sample at a canvas point.
fn texture_factor(p: vec2<f32>) -> f32 {
    let size = vec2<i32>(textureDimensions(paper, 0));
    let x = p.x / params.texture_scale - 0.5;
    let y = p.y / params.texture_scale - 0.5;
    let x0 = floor(x);
    let y0 = floor(y);
    let tx = x - x0;
    let ty = y - y0;
    let i = i32(x0);
    let j = i32(y0);
    let a = textureLoad(paper, vec2<i32>(wrap(i, size.x), wrap(j, size.y)), 0).r;
    let b = textureLoad(paper, vec2<i32>(wrap(i + 1, size.x), wrap(j, size.y)), 0).r;
    let c = textureLoad(paper, vec2<i32>(wrap(i, size.x), wrap(j + 1, size.y)), 0).r;
    let d = textureLoad(paper, vec2<i32>(wrap(i + 1, size.x), wrap(j + 1, size.y)), 0).r;
    let top = a + (b - a) * tx;
    let bottom = c + (d - c) * tx;
    var t = top + (bottom - top) * ty;
    if (params.texture_invert != 0u) {
        t = 1.0 - t;
    }
    return 1.0 - params.texture_density * (1.0 - t);
}

// Uniform noise in [0, 1) per pixel and dab (PCG-style integer hash).
fn hash(v: vec3<u32>) -> f32 {
    var h = v.x * 1664525u + v.y * 1013904223u + v.z * 2654435769u;
    h = h ^ (h >> 16u);
    h = h * 2246822519u;
    h = h ^ (h >> 13u);
    h = h * 3266489917u;
    h = h ^ (h >> 16u);
    return f32(h >> 8u) / 16777216.0;
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    // `position.xy` is the pixel centre, matching the CPU's `px + 0.5`.
    let at = input.position.xy + params.origin;
    let delta = at - input.center;
    var cov = 0.0;
    if (params.kind != 2u && input.flatness >= 1.0) {
        cov = coverage(length(delta), input.radius, params.hardness, params.aa);
    } else {
        // Into tip space: undo the rotation, then the flattening.
        let c = input.rotation.x;
        let s = input.rotation.y;
        let p = vec2<f32>((c * delta.x - s * delta.y) * input.mirror, (s * delta.x + c * delta.y) / input.flatness);
        if (params.kind == 2u) {
            cov = sampled_coverage(tip, params.tip_aspect, p, input.radius, input.level);
        } else {
            cov = ellipse_coverage(p, input.radius, input.flatness, params.hardness, params.aa);
        }
    }
    // Dual tip: unrotated, centred on the dab, masks it.
    if (params.has_dual != 0u && cov > 0.0) {
        cov = cov * sampled_coverage(dual_tip, params.dual_aspect, delta, input.radius * params.dual_scale, input.dual_level);
    }
    // Paper texture, fixed to the canvas.
    if (params.has_texture != 0u && cov > 0.0) {
        cov = cov * texture_factor(at);
    }
    var a = cov * input.alpha;
    if (a <= 0.0) {
        discard;
    }
    // 8-bit targets round every blend: ±½ step of noise per pixel and dab turns
    // that rounding into noise rather than a fixed bias. Alpha is also the blend
    // factor, so very faint, heavily overlapping dabs still build up a little
    // less than in the float target.
    if (params.dither > 0.0) {
        a = clamp(a + (hash(vec3<u32>(vec2<u32>(at), input.seed)) - 0.5) / 255.0, 0.0, 1.0);
    }
    return vec4<f32>(params.color.rgb * a, a);
}
