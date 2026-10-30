// Brush textures and second tips, shared by dab.wgsl (per dab) and
// stroke_merge.wgsl (a texture over the whole stroke). Mirrors
// painter_core::DabTexture::value and painter_core::MaskMode::mix.

// i mod n in 0..n (i32::rem_euclid). `%` only ever sees non-negative operands:
// GLSL ES, which the GL backend compiles to, leaves it undefined otherwise, and
// Mesa's llvmpipe picked another texel for the canvas's first column (i = -1).
fn wrap(i: i32, n: i32) -> i32 {
    if (i >= 0) {
        return i % n;
    }
    return n - 1 - (-i - 1) % n;
}

// Bilinear sample of `level` with the bitmap repeating (TipBitmap::sample_wrapped_level).
fn sample_wrapped(bitmap: texture_2d<f32>, level: u32, x: f32, y: f32) -> f32 {
    let size = vec2<i32>(textureDimensions(bitmap, level));
    let x0 = floor(x);
    let y0 = floor(y);
    let tx = x - x0;
    let ty = y - y0;
    let i = i32(x0);
    let j = i32(y0);
    let l = i32(level);
    let a = textureLoad(bitmap, vec2<i32>(wrap(i, size.x), wrap(j, size.y)), l).r;
    let b = textureLoad(bitmap, vec2<i32>(wrap(i + 1, size.x), wrap(j, size.y)), l).r;
    let c = textureLoad(bitmap, vec2<i32>(wrap(i, size.x), wrap(j + 1, size.y)), l).r;
    let d = textureLoad(bitmap, vec2<i32>(wrap(i + 1, size.x), wrap(j + 1, size.y)), l).r;
    let top = a + (b - a) * tx;
    let bottom = c + (d - c) * tx;
    return top + (bottom - top) * ty;
}

// painter_core::DabTexture::value: the paper at canvas point `p` as the ink
// sees it (1 lets ink through).
fn texture_value(paper: texture_2d<f32>, p: vec2<f32>, scale: f32, density: f32, invert: u32, brightness: f32, contrast: f32) -> f32 {
    var t = sample_wrapped(paper, 0u, p.x / scale - 0.5, p.y / scale - 0.5);
    if (invert != 0u) {
        t = 1.0 - t;
    }
    t = clamp((t - 0.5) * clamp(contrast, 0.0, 3.0) + 0.5 + clamp(brightness, -1.0, 1.0), 0.0, 1.0);
    return 1.0 - clamp(density, 0.0, 1.0) * (1.0 - t);
}

// painter_core::MaskMode::mix: coverage combined with a texture or second tip.
fn mask_mix(cov: f32, t: f32, mode: u32) -> f32 {
    var out = cov * t;
    switch mode {
        case 1u: { out = cov - (1.0 - t); }
        case 2u: { out = min(cov, t); }
        case 3u: {
            if (cov >= 1.0) {
                out = 1.0;
            } else if (t <= 0.0) {
                out = 0.0;
            } else {
                out = 1.0 - min((1.0 - cov) / t, 1.0);
            }
        }
        // A one-level ramp, not a step (see MaskMode::mix).
        case 4u: { out = cov * clamp((cov + t - 1.0) * 255.0 + 1.0, 0.0, 1.0); }
        default: {}
    }
    return clamp(out, 0.0, 1.0);
}
