// One image quad: affine placement, alpha background (solid / dots / checker), crossfade alpha.

struct Uniforms {
    // screen = mat2(m.xy, m.zw) * uv + t, in physical pixels
    m: vec4<f32>,
    t: vec2<f32>,
    viewport: vec2<f32>,
    bg: vec4<f32>,
    dot_color: vec4<f32>,
    // x: opacity, y: alpha-bg mode (0 solid, 1 dots, 2 checker), z: cell px, w: dot radius px
    params: vec4<f32>,
    // x: gamma, y: brightness, z: contrast, w: 1 when any is not neutral
    adjust: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(1) @binding(0) var samp: sampler;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VOut {
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0),
    );
    let uv = corners[i];
    let p = vec2(u.m.x * uv.x + u.m.y * uv.y + u.t.x, u.m.z * uv.x + u.m.w * uv.y + u.t.y);
    var out: VOut;
    out.pos = vec4(p.x / u.viewport.x * 2.0 - 1.0, 1.0 - p.y / u.viewport.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    var c = textureSample(tex, samp, in.uv);
    if u.adjust.w > 0.5 {
        // Adjust in display (sRGB-ish) space, as viewers do, then back to linear.
        var v = pow(max(c.rgb, vec3(0.0)), vec3(1.0 / 2.2));
        v = (v - 0.5) * u.adjust.z + 0.5 + u.adjust.y;
        v = pow(clamp(v, vec3(0.0), vec3(1.0)), vec3(1.0 / u.adjust.x));
        c = vec4(pow(v, vec3(2.2)), c.a);
    }
    var bg = u.bg.rgb;
    let mode = u32(u.params.y);
    let cell = u.params.z;
    if mode == 1u {
        let d = length(in.pos.xy - (floor(in.pos.xy / cell) + 0.5) * cell);
        let k = 1.0 - smoothstep(u.params.w - 0.5, u.params.w + 0.5, d);
        bg = mix(bg, u.dot_color.rgb, k);
    } else if mode == 2u {
        let q = vec2<u32>(floor(in.pos.xy / cell));
        bg = mix(bg, u.dot_color.rgb, f32((q.x + q.y) & 1u));
    }
    return vec4(mix(bg, c.rgb, c.a), u.params.x);
}
