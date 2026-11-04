// Instanced SDF primitives: rects, rounded rects, borders and dots. Output is premultiplied.

struct Globals {
    viewport: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;

struct Instance {
    @location(0) rect: vec4<f32>,         // x, y, w, h in physical px
    @location(1) color: vec4<f32>,        // linear, straight alpha
    @location(2) border_color: vec4<f32>,
    @location(3) params: vec4<f32>,       // x: corner radius, y: border width
};

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) half: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) border_color: vec4<f32>,
    @location(4) params: vec4<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32, inst: Instance) -> VOut {
    var corners = array<vec2<f32>, 6>(
        vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 1.0),
        vec2(-1.0, 1.0), vec2(1.0, -1.0), vec2(1.0, 1.0),
    );
    let half = inst.rect.zw * 0.5;
    // Pad by a pixel so the anti-aliased edge isn't clipped.
    let local = corners[i] * (half + 1.0);
    let p = inst.rect.xy + half + local;
    var out: VOut;
    out.pos = vec4(p.x / g.viewport.x * 2.0 - 1.0, 1.0 - p.y / g.viewport.y * 2.0, 0.0, 1.0);
    out.local = local;
    out.half = half;
    out.color = inst.color;
    out.border_color = inst.border_color;
    out.params = inst.params;
    return out;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let r = min(in.params.x, min(in.half.x, in.half.y));
    let q = abs(in.local) - in.half + r;
    let d = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
    let coverage = clamp(0.5 - d, 0.0, 1.0);
    let fill = vec4(in.color.rgb * in.color.a, in.color.a);
    var c = fill;
    let bw = in.params.y;
    if bw > 0.0 {
        let border = vec4(in.border_color.rgb * in.border_color.a, in.border_color.a);
        c = mix(border, fill, clamp(0.5 - (d + bw), 0.0, 1.0));
    }
    return c * coverage;
}
