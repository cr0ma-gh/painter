// Draws a source image through a projective transform (scale, rotate, skew,
// perspective, free distort) into a canvas-sized target, for live transform
// previews and GPU commits. One full-screen triangle: each target pixel centre
// goes back through the inverse homography and is filtered in the source.
// Mirrors transform_tools::resample: same f32 matrix, same tap positions,
// same kernels; premultiplied in and out, transparent outside the source.

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // A triangle covering the whole target.
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    // `position.xy` is the target pixel centre (x + 0.5, y + 0.5).
    let p = vec3<f32>(position.xy, 1.0);
    let w = dot(params.inv2.xyz, p);
    if (w <= 0.0) {
        // Behind the vanishing line.
        return vec4<f32>(0.0);
    }
    let s = vec2<f32>(dot(params.inv0.xyz, p), dot(params.inv1.xyz, p)) / w;
    return finish(sample_source(s));
}
