// Shared by transform.wgsl (full-screen, inverse homography) and
// transform_mesh.wgsl (warp mesh with per-vertex source positions).

struct Params {
    // Rows of the inverse matrix (target px → source px); xyz used.
    inv0: vec4<f32>,
    inv1: vec4<f32>,
    inv2: vec4<f32>,
    // 0 nearest, 1 bilinear, 2 bicubic (Catmull-Rom), 3 Lanczos3.
    filter_mode: u32,
    _pad0: u32,
    // Target size in pixels (mesh vertices are in target pixels).
    target_size: vec2<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var source: texture_2d<f32>;

fn texel(x: i32, y: i32) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(source, 0));
    if (x < 0 || y < 0 || x >= size.x || y >= size.y) {
        return vec4<f32>(0.0);
    }
    return textureLoad(source, vec2<i32>(x, y), 0);
}

fn cubic(t: f32) -> f32 {
    let x = abs(t);
    if (x < 1.0) {
        return (1.5 * x - 2.5) * x * x + 1.0;
    }
    if (x < 2.0) {
        return ((-0.5 * x + 2.5) * x - 4.0) * x + 2.0;
    }
    return 0.0;
}

fn lanczos3(t: f32) -> f32 {
    let x = abs(t);
    if (x < 1e-5) {
        return 1.0;
    }
    if (x >= 3.0) {
        return 0.0;
    }
    let px = 3.14159265358979 * x;
    return 3.0 * sin(px) * sin(px / 3.0) / (px * px);
}

fn kernel(t: f32, lanczos: bool) -> f32 {
    if (lanczos) {
        return lanczos3(t);
    }
    return cubic(t);
}

// Separable filter with `radius` taps either side, normalised weights.
fn separable(f: vec2<f32>, radius: i32, lanczos: bool) -> vec4<f32> {
    let base = floor(f);
    let t = f - base;
    let x0 = i32(base.x);
    let y0 = i32(base.y);
    var sum = vec4<f32>(0.0);
    var total = 0.0;
    for (var ky = 1 - radius; ky <= radius; ky++) {
        let wy = kernel(f32(ky) - t.y, lanczos);
        for (var kx = 1 - radius; kx <= radius; kx++) {
            let w = kernel(f32(kx) - t.x, lanczos) * wy;
            sum += texel(x0 + kx, y0 + ky) * w;
            total += w;
        }
    }
    if (abs(total) > 1e-6) {
        return sum / total;
    }
    return sum;
}

fn sample_source(s: vec2<f32>) -> vec4<f32> {
    let f = s - vec2<f32>(0.5);
    switch params.filter_mode {
        case 0u: {
            return texel(i32(floor(s.x)), i32(floor(s.y)));
        }
        case 2u: {
            return separable(f, 2, false);
        }
        case 3u: {
            return separable(f, 3, true);
        }
        default: {
            let base = floor(f);
            let t = f - base;
            let x0 = i32(base.x);
            let y0 = i32(base.y);
            let top = mix(texel(x0, y0), texel(x0 + 1, y0), t.x);
            let bottom = mix(texel(x0, y0 + 1), texel(x0 + 1, y0 + 1), t.x);
            return mix(top, bottom, t.y);
        }
    }
}

// Premultiplied colour never exceeds alpha (ringing filters can overshoot).
fn finish(c: vec4<f32>) -> vec4<f32> {
    let a = clamp(c.a, 0.0, 1.0);
    return vec4<f32>(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(a)), a);
}