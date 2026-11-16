// The end-of-game banner as a slab of clear glass over the live sea: the scene shows through
// it nearly as is, bent hard at the rounded rim like a thick lens edge (a little apart in red
// and blue there), with a thin bright rim lit from a slowly swinging light. Rune's text is
// drawn over it.

struct Glass {
    // xy unused, z srgb encode (1/0), w exposure
    p: vec4<f32>,
    // x scene pixels per frame pixel (dynamic resolution), w seconds
    q: vec4<f32>,
    // the banner's rect in frame device pixels: x, y, w, h
    rect: vec4<f32>,
    // x corner radius, y the rim's width (where the glass bends), z the most it bends, w device
    // pixels per point; all in device pixels
    look: vec4<f32>,
};

@group(0) @binding(0) var scene: texture_2d<f32>;
@group(0) @binding(1) var<uniform> gl: Glass;
@group(0) @binding(2) var smooth_sampler: sampler;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    // 0..1 across the banner, top-left origin; the pass viewport is the banner's rect
    @location(0) uv: vec2<f32>,
};

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

// signed distance to a rounded rect of half size `half` and corner radius `r` (negative inside)
fn rounded(p: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - half + r;
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// the scene, tonemapped, at frame pixel `px`
fn sea_at(px: vec2<f32>) -> vec3<f32> {
    let dims = vec2<f32>(textureDimensions(scene));
    return aces(textureSampleLevel(scene, smooth_sampler, px * gl.q.x / dims, 0.0).rgb * gl.p.w);
}

@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    let size = gl.rect.zw;
    let half = size * 0.5;
    let local = (i.uv - 0.5) * size;
    let r = min(gl.look.x, min(half.x, half.y));
    let d = rounded(local, half, r);
    let cover = clamp(0.5 - d, 0.0, 1.0);
    if (cover <= 0.0) {
        return vec4<f32>(0.0);
    }
    // the rim's outward normal, from the distance field
    let e = vec2<f32>(1.0, 0.0);
    let n = normalize(vec2<f32>(
        rounded(local + e.xy, half, r) - rounded(local - e.xy, half, r),
        rounded(local + e.yx, half, r) - rounded(local - e.yx, half, r),
    ) + vec2<f32>(1e-5));
    // 1 at the edge, 0 past the rim: the glass is flat in the middle and bends at the rim,
    // pulling the sea from further in, as a thick rounded edge does. The bend never grows
    // faster inward than the pixel moves (2 * look.z < look.y; at 0.6 of it the squeeze stays
    // within 2.5x), so the sea is pressed toward the edge and never folds into smeared bands.
    let edge = clamp(1.0 + d / gl.look.y, 0.0, 1.0);
    let bend = edge * edge * gl.look.z;
    let px = gl.rect.xy + i.uv * size;
    // 0 at the rim, 1 well inside it: where the text sits
    let mid = smoothstep(gl.look.y * 0.3, gl.look.y * 0.9, -d);
    var c: vec3<f32>;
    if (edge > 0.0) {
        // red and blue bend a little apart at the rim; one tap past it
        c = vec3<f32>(
            sea_at(px - n * bend * 1.12).r,
            sea_at(px - n * bend).g,
            sea_at(px - n * bend * 0.88).b,
        );
    } else {
        c = sea_at(px);
    }
    // the glints and pale tiles held down, and a dark tint that deepens smoothly from a clear
    // rim to the middle, so the text reads over anything behind it
    let lum = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    c = mix(c, c * mix(0.6, 0.3, mid), smoothstep(0.1, 0.7, lum));
    c = mix(c, vec3<f32>(0.012, 0.035, 0.05), mix(0.22, 0.72, mid));
    // the rim catches a light swinging slowly overhead, and faintly on the far side
    let t = gl.q.w;
    let light = normalize(vec2<f32>(-0.55 + 0.35 * sin(t * 0.5), -1.0));
    let rim = 1.0 - smoothstep(0.0, 1.6 * gl.look.w, -d);
    let catch_light = pow(max(dot(n, light), 0.0), 2.0) + 0.35 * pow(max(dot(n, -light), 0.0), 2.0);
    c += vec3<f32>(1.0) * rim * (0.18 + 0.75 * catch_light);
    // a soft sheen inside the rim, brighter on the lit side
    c += vec3<f32>(0.9, 0.97, 1.0) * edge * edge * 0.08 * (0.5 + catch_light);
    c = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    if (gl.p.z > 0.5) { c = encode(c); }
    return vec4<f32>(c * cover, cover);
}
