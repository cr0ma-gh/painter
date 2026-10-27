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
    hidden: vec4<f32>,
    inks: vec4<f32>,
    ink_view: vec4<f32>,
    // The canvas rectangle the composite texture covers: origin xy, size zw (canvas px).
    region: vec4<f32>,
};

@group(0) @binding(0) var canvas: texture_2d<f32>;
@group(0) @binding(1) var canvas_sampler: sampler;
@group(0) @binding(2) var<uniform> view: View;
@group(0) @binding(3) var atlas: texture_2d_array<f32>;
@group(0) @binding(4) var selection_index: texture_2d<u32>;
@group(0) @binding(5) var ink_lut: texture_3d<f32>;
@group(0) @binding(6) var ink_sampler: sampler;

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
    // Low-VRAM profiles composite only the visible part; the rest shows as empty.
    let at = (input.uv * view.doc_size - view.region.xy) / view.region.zw;
    let inside = all(at >= vec2<f32>(0.0)) && all(at <= vec2<f32>(1.0));
    let color = select(vec4<f32>(0.0), textureSample(canvas, canvas_sampler, at), inside);
    let cell = floor(input.uv * view.doc_size / view.checker);
    let odd = (i32(cell.x) + i32(cell.y)) & 1;
    let checker = select(1.0, 0.82, odd == 1);
    var rgb = color.rgb + vec3<f32>(checker) * (1.0 - color.a);
    // Channels: only the visible ones; one alone as grey.
    let shown = vec3<f32>(1.0) - view.hidden.rgb;
    let count = shown.r + shown.g + shown.b;
    if (count < 0.5) {
        rgb = vec3<f32>(0.0);
    } else if (count < 1.5) {
        rgb = vec3<f32>(dot(rgb, shown));
    } else if (count < 2.5) {
        rgb = rgb * shown;
    }
    // A table from these colours to the ones shown (Duotone, Lab and Multichannel views).
    if (view.ink_view.y > 0.5) {
        let n = f32(textureDimensions(ink_lut).x);
        let at = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)) * ((n - 1.0) / n) + vec3<f32>(0.5 / n);
        rgb = textureSampleLevel(ink_lut, ink_sampler, at, 0.0).rgb;
    }
    // CMYK: the shown inks, from the profile's table (texel centres at i / (n - 1)).
    if (view.ink_view.x > 0.5) {
        let n = f32(textureDimensions(ink_lut).x);
        let at = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)) * ((n - 1.0) / n) + vec3<f32>(0.5 / n);
        let ink = textureSampleLevel(ink_lut, ink_sampler, at, 0.0) * view.inks;
        let inks = view.inks.x + view.inks.y + view.inks.z + view.inks.w;
        if (inks < 0.5) {
            rgb = vec3<f32>(1.0);
        } else if (inks < 1.5) {
            // One ink alone: grey, white where there's none.
            rgb = vec3<f32>(1.0 - (ink.x + ink.y + ink.z + ink.w));
        } else if (inks < 3.5) {
            // Some inks: printed one over the other.
            let k = 1.0 - ink.w;
            rgb = vec3<f32>((1.0 - ink.x) * k, (1.0 - ink.y) * k, (1.0 - ink.z) * k);
        }
    }

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
