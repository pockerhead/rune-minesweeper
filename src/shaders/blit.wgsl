struct Blit {
    // xy the board's top-left in frame device pixels, z srgb encode (1/0), w exposure
    p: vec4<f32>,
    // x scene pixels per frame pixel (dynamic resolution), zw the board's size in frame device pixels
    q: vec4<f32>,
};

struct VOut {
    @builtin(position) pos: vec4<f32>,
    // 0..1 across the board, top-left origin; the pass viewport is the board's rect
    @location(0) uv: vec2<f32>,
};

@group(0) @binding(0) var scene: texture_2d<f32>;
@group(0) @binding(1) var<uniform> b: Blit;
@group(0) @binding(2) var smooth_sampler: sampler;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VOut {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: VOut;
    o.pos = vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2<f32>(uv.x, 1.0 - uv.y);
    return o;
}

fn aces(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51; let bb = 0.03; let c = 2.43; let d = 0.59; let e = 0.14;
    return clamp((x * (a * x + bb)) / (x * (c * x + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn encode(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(scene));
    let scale = b.q.x;
    // where this fragment sits in the frame, wherever the pass's target sits in it
    let frame_px = b.p.xy + i.uv * b.q.zw;
    // four filtered taps a quarter pixel around it: a 2x2 box at 2x supersampling, a smooth
    // upscale below native resolution. Tonemap each tap, then average, so bright glints don't
    // fatten the edges they sit on.
    var c = vec3<f32>(0.0);
    for (var k = 0; k < 4; k++) {
        let o = vec2<f32>(f32(k & 1), f32(k >> 1u)) * 0.5 - 0.25;
        let uv = (frame_px + o) * scale / dims;
        c += aces(textureSampleLevel(scene, smooth_sampler, uv, 0.0).rgb * b.p.w);
    }
    c *= 0.25;
    if (b.p.z > 0.5) { c = encode(c); }
    return vec4<f32>(c, 1.0);
}
