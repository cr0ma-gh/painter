// Shared tile-atlas lookup, prepended to shaders that read planes.
// Including shaders declare `atlas: texture_2d_array<f32>`.
// Must match painter-render/src/atlas.rs (64 px tiles, 2048 px pages).

fn atlas_load(index: texture_2d<u32>, p: vec2<i32>) -> vec4<f32> {
    let slot = textureLoad(index, p / 64, 0).r;
    if (slot == 0u) {
        return vec4<f32>(0.0);
    }
    let s = slot - 1u;
    let page = s / 1024u;
    let i = s % 1024u;
    let q = vec2<i32>(i32(i % 32u) * 64 + p.x % 64, i32(i / 32u) * 64 + p.y % 64);
    return textureLoad(atlas, q, i32(page), 0);
}
