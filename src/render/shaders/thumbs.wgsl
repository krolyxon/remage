// Instanced thumbnail quads sampled from atlas pages. Output is straight alpha.

struct Globals {
    viewport: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var samp: sampler;
@group(1) @binding(0) var page: texture_2d<f32>;

struct Instance {
    @location(0) rect: vec4<f32>, // x, y, w, h in physical px
    @location(1) uv: vec4<f32>,   // u, v, du, dv in the page
    @location(2) params: vec4<f32>, // x: opacity
};

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha: f32,
};

@vertex
fn vs(@builtin(vertex_index) i: u32, inst: Instance) -> VOut {
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0),
    );
    let c = corners[i];
    let p = inst.rect.xy + c * inst.rect.zw;
    var out: VOut;
    out.pos = vec4(p.x / g.viewport.x * 2.0 - 1.0, 1.0 - p.y / g.viewport.y * 2.0, 0.0, 1.0);
    out.uv = inst.uv.xy + c * inst.uv.zw;
    out.alpha = inst.params.x;
    return out;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let c = textureSample(page, samp, in.uv);
    return vec4(c.rgb, c.a * in.alpha);
}
