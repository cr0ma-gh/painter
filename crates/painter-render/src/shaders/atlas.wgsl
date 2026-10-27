// Shared tile-atlas lookup, prepended to shaders that read planes.
// Including shaders declare `atlas: texture_2d_array<f32>`.
// Must match painter-render/src/atlas.rs (64 px tiles). The page size is read
// from the atlas itself: 2048 px pages normally, 1024 px on low-VRAM devices.

fn atlas_load(index: texture_2d<u32>, p: vec2<i32>) -> vec4<f32> {
    let slot = textureLoad(index, p / 64, 0).r;
    if (slot == 0u) {
        return vec4<f32>(0.0);
    }
    let s = slot - 1u;
    let per_row = textureDimensions(atlas).x / 64u;
    let per_page = per_row * per_row;
    let page = s / per_page;
    let i = s % per_page;
    let q = vec2<i32>(i32(i % per_row) * 64 + p.x % 64, i32(i / per_row) * 64 + p.y % 64);
    return textureLoad(atlas, q, i32(page), 0);
}
