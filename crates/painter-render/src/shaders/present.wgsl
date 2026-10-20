// Draws the composited canvas as a quad under the view transform
// (pan / zoom / rotate / mirror), over a transparency checkerboard, with marching ants
// along the selection edge.
//
// Coordinates are in UI points relative to the canvas widget's centre; the
// render pass viewport is the widget rect, so dividing by half its size gives
// clip space.

struct View {
    rect_size: vec2<f32>,
    doc_size: vec2<f32>,
    pan: vec2<f32>,
    zoom: f32,
    rotation: f32,
    checker: f32,
    time: f32,
    show_selection: f32,
    flip: f32,
};

@group(0) @binding(0) var canvas: texture_2d<f32>;
@group(0) @binding(1) var canvas_sampler: sampler;
@group(0) @binding(2) var<uniform> view: View;
@group(0) @binding(3) var atlas: texture_2d_array<f32>;
@group(0) @binding(4) var selection_index: texture_2d<u32>;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let uv = corners[index];
    var d = (uv - 0.5) * view.doc_size * view.zoom;
    if (view.flip > 0.5) {
        d.x = -d.x;
    }
    let c = cos(view.rotation);
    let s = sin(view.rotation);
    let p = vec2<f32>(d.x * c - d.y * s, d.x * s + d.y * c) + view.pan;
    var out: VertexOut;
    out.position = vec4<f32>(p.x / (view.rect_size.x * 0.5), -p.y / (view.rect_size.y * 0.5), 0.0, 1.0);
    out.uv = uv;
    return out;
}

fn selected(p: vec2<i32>) -> bool {
    let size = vec2<i32>(view.doc_size);
    if (p.x < 0 || p.y < 0 || p.x >= size.x || p.y >= size.y) {
        return false;
    }
    return atlas_load(selection_index, p).a >= 0.5;
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    let color = textureSample(canvas, canvas_sampler, input.uv);
    let cell = floor(input.uv * view.doc_size / view.checker);
    let odd = (i32(cell.x) + i32(cell.y)) & 1;
    let checker = select(1.0, 0.82, odd == 1);
    var rgb = color.rgb + vec3<f32>(checker) * (1.0 - color.a);

    if (view.show_selection > 0.5) {
        let p = vec2<i32>(floor(input.uv * view.doc_size));
        // Neighbour distance of about one screen pixel, so the edge survives zooming out.
        let step = max(1, i32(round(1.0 / view.zoom)));
        if (selected(p) && !(selected(p + vec2<i32>(step, 0)) && selected(p - vec2<i32>(step, 0))
            && selected(p + vec2<i32>(0, step)) && selected(p - vec2<i32>(0, step)))) {
            let dash = fract((input.position.x + input.position.y) / 8.0 - view.time * 2.0) < 0.5;
            rgb = select(vec3<f32>(1.0), vec3<f32>(0.0), dash);
        }
    }
    return vec4<f32>(rgb, 1.0);
}
