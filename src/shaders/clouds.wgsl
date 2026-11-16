// ---- number clouds: stylised volumetric clouds shaped as digits, by signed distance ----
// Shared by the lit and water shaders (shadows, reflections) and the cloud pass itself.
// The clouds live on the cell grid: g.clouds holds one code per cell, 0 for none, else
// digit * 4 + s, where s (under 1) is its size while it puffs in. Rays find them by walking the cells they cross (a 2D DDA), so the cost
// stays the same on a 30 x 16 board as on a 9 x 9 one.

const GLYPH_H: f32 = 0.6;
const CLOUD_R: f32 = 0.62;
/// Half the height of the slab the clouds float in (around g.board.w).
const SLAB: f32 = 0.42;
/// Cells a cloud ray walks at most, and for a shadow ray, with its march steps per cloud.
const HIT_CELLS: i32 = 24;
const SHADOW_CELLS: i32 = 12;
const SHADOW_STEPS: i32 = 12;

fn cloud_code(k: i32) -> f32 {
    return g.clouds[k / 4][k % 4];
}

fn cloud_digit(k: i32) -> i32 {
    return i32(floor(cloud_code(k) / 4.0));
}

// its size: under 1 while it puffs in
fn cloud_size(k: i32) -> f32 {
    return cloud_code(k) - f32(cloud_digit(k)) * 4.0;
}

// where cloud k floats: over its cell, bobbing; rising into place while it puffs in
fn cloud_center(k: i32) -> vec3<f32> {
    let c = cell_center(k);
    let size = cloud_size(k);
    let rise = 0.25 * (1.0 - size) * (1.0 - size);
    let bob = sin(g.cam.w * 1.3 + f32(k) * 0.7) * 0.04;
    return vec3<f32>(c.x, g.board.w + bob - rise, c.y);
}

// a camera-facing frame shared by all clouds, so every digit reads upright on screen
fn glyph_frame() -> mat3x3<f32> {
    let f = normalize(g.cam.xyz - vec3<f32>(0.0, 0.0, 0.5));
    let r = normalize(cross(vec3<f32>(0.0, 1.0, 0.0), f));
    let u = cross(f, r);
    return mat3x3<f32>(r, u, f);
}

fn seg_dist(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);
    return length(pa - ba * h);
}

// segments a..g as bits 0..6 for the digits 1..8
fn seven_mask(n: i32) -> u32 {
    switch n {
        case 1: { return 6u; }
        case 2: { return 91u; }
        case 3: { return 79u; }
        case 4: { return 102u; }
        case 5: { return 109u; }
        case 6: { return 125u; }
        case 7: { return 7u; }
        default: { return 127u; }
    }
}

// distance to the digit's centre lines; glyph space x -0.3..0.3, y -0.5..0.5
fn digit_lines(q: vec2<f32>, n: i32) -> f32 {
    var p = q;
    p.x -= p.y * 0.12; // a little italic, livelier than upright
    if (n == 1) { p.x += 0.25; }
    let x0 = -0.25; let x1 = 0.25; let y0 = -0.41; let y1 = 0.41;
    let m = seven_mask(n);
    var d = 1e3;
    if ((m & 1u) != 0u) { d = min(d, seg_dist(p, vec2<f32>(x0, y1), vec2<f32>(x1, y1))); }
    if ((m & 2u) != 0u) { d = min(d, seg_dist(p, vec2<f32>(x1, y1), vec2<f32>(x1, 0.0))); }
    if ((m & 4u) != 0u) { d = min(d, seg_dist(p, vec2<f32>(x1, 0.0), vec2<f32>(x1, y0))); }
    if ((m & 8u) != 0u) { d = min(d, seg_dist(p, vec2<f32>(x0, y0), vec2<f32>(x1, y0))); }
    if ((m & 16u) != 0u) { d = min(d, seg_dist(p, vec2<f32>(x0, 0.0), vec2<f32>(x0, y0))); }
    if ((m & 32u) != 0u) { d = min(d, seg_dist(p, vec2<f32>(x0, y1), vec2<f32>(x0, 0.0))); }
    if ((m & 64u) != 0u) { d = min(d, seg_dist(p, vec2<f32>(x0, 0.0), vec2<f32>(x1, 0.0))); }
    return d;
}

fn hash3(p: vec3<f32>) -> f32 {
    var q = fract(p * 0.3183099 + vec3<f32>(0.71, 0.113, 0.419));
    q *= 17.0;
    return fract(q.x * q.y * q.z * (q.x + q.y + q.z));
}

fn vnoise(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(mix(hash3(i), hash3(i + vec3<f32>(1.0, 0.0, 0.0)), u.x),
            mix(hash3(i + vec3<f32>(0.0, 1.0, 0.0)), hash3(i + vec3<f32>(1.0, 1.0, 0.0)), u.x), u.y),
        mix(mix(hash3(i + vec3<f32>(0.0, 0.0, 1.0)), hash3(i + vec3<f32>(1.0, 0.0, 1.0)), u.x),
            mix(hash3(i + vec3<f32>(0.0, 1.0, 1.0)), hash3(i + vec3<f32>(1.0, 1.0, 1.0)), u.x), u.y),
        u.z);
}

// signed distance to cloud k at world point p (approximate: bumps make it non-Euclidean, so
// marching steps are scaled down by the callers)
// The cheapest render quality (phones, a struggling GPU): every cloud costs in every pixel of
// water and tile it could shade, so their shadows, reflections and fine curls go, and a long
// game with many clouds stays as light as the first move.
fn lite() -> bool {
    return g.sim.w <= 0.0;
}

fn cloud_sdf(p: vec3<f32>, k: i32, frame: mat3x3<f32>) -> f32 {
    let size = max(cloud_size(k), 0.02);
    let scale = GLYPH_H * size;
    let q = (p - cloud_center(k)) * frame / scale; // world -> glyph space
    let lines = digit_lines(q.xy, cloud_digit(k));
    // a fat rounded tube along the strokes...
    var d = length(vec2<f32>(lines, q.z * 1.15)) - 0.115;
    // ...made puffy: big soft lumps plus small curls, slowly churning
    let t = g.cam.w * 0.25;
    let lumps = vnoise(q * 5.0 + vec3<f32>(t, 0.0, t * 0.7));
    d -= (lumps - 0.45) * 0.075;
    if (!lite()) {
        let curls = vnoise(q * 11.0 - vec3<f32>(0.0, t * 1.3, 0.0));
        d -= (curls - 0.5) * 0.035;
    }
    return d * scale;
}

// ray-sphere entry and exit for cloud k's bounds; (1e9, -1e9) when missed
fn cloud_span(ro: vec3<f32>, rd: vec3<f32>, k: i32) -> vec2<f32> {
    let r = CLOUD_R * max(cloud_size(k), 0.02) * 1.25;
    let oc = ro - cloud_center(k);
    let b = dot(oc, rd);
    let h = b * b - (dot(oc, oc) - r * r);
    if (h < 0.0) { return vec2<f32>(1e9, -1e9); }
    let s = sqrt(h);
    return vec2<f32>(-b - s, -b + s);
}

// the part of a ray inside the clouds' slab: (t0, t1), t1 < t0 when it never enters
fn slab_span(ro: vec3<f32>, rd: vec3<f32>) -> vec2<f32> {
    let lo = g.board.w - SLAB;
    let hi = g.board.w + SLAB;
    if (abs(rd.y) < 1e-4) {
        if (ro.y < lo || ro.y > hi) { return vec2<f32>(1.0, -1.0); }
        return vec2<f32>(0.0, 60.0);
    }
    let ta = (lo - ro.y) / rd.y;
    let tb = (hi - ro.y) / rd.y;
    return vec2<f32>(max(min(ta, tb), 0.0), min(max(ta, tb), 60.0));
}

// One cell step of a 2D DDA over the board: the cells a ray's xz track crosses inside the
// slab, nearest first. State: current cell (x, z), per-axis step, next crossing t, t per cell.
struct Walk {
    cell: vec2<i32>,
    step: vec2<i32>,
    next: vec2<f32>,
    delta: vec2<f32>,
    t_end: f32,
};

fn walk_start(ro: vec3<f32>, rd: vec3<f32>, span: vec2<f32>) -> Walk {
    let half = board_half();
    let p = ro.xz + rd.xz * span.x + half; // grid space, cells are unit squares from 0
    var w: Walk;
    w.cell = vec2<i32>(floor(p));
    let d = rd.xz;
    w.step = vec2<i32>(select(-1, 1, d.x >= 0.0), select(-1, 1, d.y >= 0.0));
    let inv = vec2<f32>(1.0 / max(abs(d.x), 1e-6), 1.0 / max(abs(d.y), 1e-6));
    let frac_to = vec2<f32>(
        select(p.x - floor(p.x), floor(p.x) + 1.0 - p.x, d.x >= 0.0),
        select(p.y - floor(p.y), floor(p.y) + 1.0 - p.y, d.y >= 0.0),
    );
    w.next = vec2<f32>(span.x) + frac_to * inv;
    w.delta = inv;
    w.t_end = span.y;
    return w;
}

// the cloud index of the walk's current cell, -1 off the board or without a cloud
fn walk_cloud(w: Walk) -> i32 {
    if (w.cell.x < 0 || w.cell.y < 0 || w.cell.x >= board_w() || w.cell.y >= board_h()) { return -1; }
    let k = w.cell.x + w.cell.y * board_w();
    if (cloud_code(k) <= 0.0) { return -1; }
    return k;
}

fn walk_next(w: ptr<function, Walk>) -> f32 {
    if ((*w).next.x < (*w).next.y) {
        let t = (*w).next.x;
        (*w).cell.x += (*w).step.x;
        (*w).next.x += (*w).delta.x;
        return t;
    }
    let t = (*w).next.y;
    (*w).cell.y += (*w).step.y;
    (*w).next.y += (*w).delta.y;
    return t;
}

// soft shadow of the clouds at p toward the sun: 1 lit, 0 fully shaded
fn cloud_shadow(p: vec3<f32>, l: vec3<f32>) -> f32 {
    if (lite()) { return 1.0; }
    let span = slab_span(p, l);
    if (span.y <= span.x) { return 1.0; }
    let frame = glyph_frame();
    var w = walk_start(p, l, span);
    var lit = 1.0;
    var t_cell = span.x;
    for (var n = 0; n < SHADOW_CELLS; n++) {
        let k = walk_cloud(w);
        if (k >= 0) {
            let s = cloud_span(p, l, k);
            var t = max(s.x, 0.0);
            for (var i = 0; i < SHADOW_STEPS; i++) {
                if (t > s.y) { break; }
                let d = cloud_sdf(p + l * t, k, frame);
                lit = min(lit, clamp(5.0 * d / max(t, 0.05), 0.0, 1.0));
                t += max(d * 0.8, 0.03);
            }
            if (lit < 0.02) { break; }
        }
        t_cell = walk_next(&w);
        if (t_cell > w.t_end) { break; }
    }
    return lit;
}

// first cloud hit along a ray: (t, cloud index), t < 0 when nothing
fn cloud_hit(ro: vec3<f32>, rd: vec3<f32>, max_steps: i32) -> vec2<f32> {
    let span = slab_span(ro, rd);
    if (span.y <= span.x) { return vec2<f32>(-1.0, -1.0); }
    let frame = glyph_frame();
    var w = walk_start(ro, rd, span);
    for (var n = 0; n < HIT_CELLS; n++) {
        let k = walk_cloud(w);
        if (k >= 0) {
            let s = cloud_span(ro, rd, k);
            var t = max(s.x, 0.0);
            for (var i = 0; i < max_steps; i++) {
                if (t > s.y) { break; }
                let d = cloud_sdf(ro + rd * t, k, frame);
                if (d < 0.002) { return vec2<f32>(t, f32(k)); }
                t += d * 0.7;
            }
        }
        let t_cell = walk_next(&w);
        if (t_cell > w.t_end) { break; }
    }
    return vec2<f32>(-1.0, -1.0);
}

fn cloud_normal(p: vec3<f32>, k: i32) -> vec3<f32> {
    let frame = glyph_frame();
    let e = vec2<f32>(0.006, -0.006);
    return normalize(
        e.xyy * cloud_sdf(p + e.xyy, k, frame) + e.yyx * cloud_sdf(p + e.yyx, k, frame)
        + e.yxy * cloud_sdf(p + e.yxy, k, frame) + e.xxx * cloud_sdf(p + e.xxx, k, frame));
}

fn cloud_tint(n: i32) -> vec3<f32> {
    switch n {
        case 1: { return vec3<f32>(0.10, 0.36, 1.00); }
        case 2: { return vec3<f32>(0.10, 0.78, 0.22); }
        case 3: { return vec3<f32>(1.00, 0.16, 0.10); }
        case 4: { return vec3<f32>(0.55, 0.22, 1.00); }
        case 5: { return vec3<f32>(1.00, 0.48, 0.06); }
        case 6: { return vec3<f32>(0.04, 0.78, 0.80); }
        case 7: { return vec3<f32>(0.85, 0.85, 0.85); }
        default: { return vec3<f32>(0.50, 0.50, 0.56); }
    }
}

// stylised cloud lighting at a surface point: wrapped sun, sky fill, curl occlusion, a
// silver lining where the sun comes from behind, and the cloud's own shadow
fn cloud_shade(p: vec3<f32>, k: i32, v: vec3<f32>, full: bool) -> vec3<f32> {
    let n = cloud_normal(p, k);
    let l = g.sun.xyz;
    let frame = glyph_frame();
    let base = mix(vec3<f32>(1.0, 0.98, 0.95), cloud_tint(cloud_digit(k)), 0.9);
    let wrap = pow(clamp(dot(n, l) * 0.5 + 0.5, 0.0, 1.0), 1.6);
    var shade = 1.0;
    var ao = 1.0;
    if (full && !lite()) {
        // occlusion from the curls around it, and self-shadow toward the sun
        ao = clamp(0.35 + 0.65 * (cloud_sdf(p + n * 0.05, k, frame) / 0.05), 0.25, 1.0);
        var t = 0.03;
        for (var i = 0; i < 8; i++) {
            let d = cloud_sdf(p + l * t, k, frame);
            shade = min(shade, clamp(6.0 * d / t, 0.0, 1.0));
            t += max(d, 0.02);
        }
        shade = 0.35 + 0.65 * shade;
    }
    let sun_col = vec3<f32>(1.0, 0.82, 0.62) * 1.55;
    let sky_fill = mix(vec3<f32>(0.10, 0.12, 0.16), vec3<f32>(0.22, 0.32, 0.52), n.y * 0.5 + 0.5);
    let rim = pow(1.0 - clamp(dot(n, v), 0.0, 1.0), 3.0);
    let lining = pow(clamp(dot(v, -l), 0.0, 1.0), 2.0) * rim;
    var c = base * (sun_col * wrap * shade + sky_fill * 0.9) * ao;
    c += base * vec3<f32>(1.0, 0.85, 0.65) * (rim * 0.3 + lining * 1.2);
    return c;
}
