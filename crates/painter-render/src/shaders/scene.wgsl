// 3D reference layers: meshes drawn with a depth buffer into a canvas-sized
// target, Lambert-lit by up to four lights plus ambient. Output is
// premultiplied (opaque where geometry is), so the layer composites like any
// other plane.

struct Globals {
    view_proj: mat4x4<f32>,
    // xyz: direction the light travels (sun) or position (point); w: 0 sun, 1 point.
    light_vec: array<vec4<f32>, 4>,
    // rgb: colour × intensity; w unused.
    light_color: array<vec4<f32>, 4>,
    // x: light count, y: ambient.
    params: vec4<f32>,
};

struct Object {
    model: mat4x4<f32>,
    // Base colour (rgb) and how much to brighten the selected object (a).
    color: vec4<f32>,
    // x: 1 when the texture is painted over the colour.
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var<uniform> object: Object;
// Premultiplied colour over the base colour through the UVs (v up).
@group(1) @binding(1) var paint: texture_2d<f32>;
@group(1) @binding(2) var paint_sampler: sampler;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

@vertex
fn vs_main(@location(0) position: vec3<f32>, @location(1) normal: vec3<f32>, @location(2) uv: vec2<f32>) -> VertexOut {
    var out: VertexOut;
    let world = object.model * vec4<f32>(position, 1.0);
    out.position = globals.view_proj * world;
    out.world = world.xyz;
    // Fine for the uniform-ish scales of reference models.
    out.normal = (object.model * vec4<f32>(normal, 0.0)).xyz;
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    let n = normalize(input.normal);
    var light = vec3<f32>(globals.params.y);
    let count = u32(globals.params.x);
    for (var i = 0u; i < count; i++) {
        let v = globals.light_vec[i];
        var to_light = -v.xyz;
        if (v.w > 0.5) {
            to_light = v.xyz - input.world;
        }
        // Two-sided: reference meshes are often open or have flipped faces.
        light += globals.light_color[i].rgb * abs(dot(n, normalize(to_light)));
    }
    let painted = textureSample(paint, paint_sampler, vec2<f32>(input.uv.x, 1.0 - input.uv.y)) * object.params.x;
    let base = object.color.rgb * (1.0 - painted.a) + painted.rgb;
    let rgb = clamp(base * light + vec3<f32>(object.color.a), vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(rgb, 1.0);
}
