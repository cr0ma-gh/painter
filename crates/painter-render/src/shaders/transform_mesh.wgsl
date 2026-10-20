// Mesh warp: textured triangles whose vertices carry canvas positions and
// source positions (transform_tools::MeshWarp::tessellate). Uses the same
// filters as transform.wgsl through transform_sample.wgsl.

struct MeshOut {
    @builtin(position) position: vec4<f32>,
    @location(0) source_pos: vec2<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec2<f32>, @location(1) source_pos: vec2<f32>) -> MeshOut {
    var out: MeshOut;
    out.position = vec4<f32>(pos.x / params.target_size.x * 2.0 - 1.0, 1.0 - pos.y / params.target_size.y * 2.0, 0.0, 1.0);
    out.source_pos = source_pos;
    return out;
}

@fragment
fn fs_main(input: MeshOut) -> @location(0) vec4<f32> {
    return finish(sample_source(input.source_pos));
}