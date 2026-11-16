struct Globals {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    // xyz camera position, w water time in seconds
    cam: vec4<f32>,
    // xyz direction toward the sun, w camera pull-back (fog distances scale with it)
    sun: vec4<f32>,
    // x hover cell (-1 none), y and z the camera's pan (x, z), w status (0 play, 1 lost, 2 won)
    info: vec4<f32>,
    // the board's viewport in offscreen pixels: x, y, w, h
    viewport: vec4<f32>,
    // the cursor's light over the water: xyz position, w strength (0 when off the board)
    cursor: vec4<f32>,
    // the last blast: x, z, seconds since, 1 while it shows
    boom: vec4<f32>,
    // the cell grid: x width, y depth (cells), z 1 to draw its lines, w height the clouds float at
    board: vec4<f32>,
    // the interactive water square: x low corner, y side, z texels per side
    sim: vec4<f32>,
    // per cell: 0 closed, 1 open
    cells: array<vec4<f32>, 120>,
    // per cell: its number cloud, 0 for none (see clouds.wgsl)
    clouds: array<vec4<f32>, 120>,
};

@group(0) @binding(0) var<uniform> g: Globals;
// interactive water from ripple.rs: r height, g foam
@group(0) @binding(1) var ripple: texture_2d<f32>;

fn ripple_texel(i: i32, j: i32) -> vec2<f32> {
    let n = i32(g.sim.z);
    return textureLoad(ripple, vec2<i32>(clamp(i, 0, n - 1), clamp(j, 0, n - 1)), 0).rg;
}

// bilinear (height, foam) at world xz; zero outside the simulated square
fn ripple_at(p: vec2<f32>) -> vec2<f32> {
    let n = g.sim.z;
    let f = (p - vec2<f32>(g.sim.x)) / g.sim.y * n - 0.5;
    if (f.x < 0.0 || f.y < 0.0 || f.x >= n - 1.0 || f.y >= n - 1.0) { return vec2<f32>(0.0); }
    let i = vec2<i32>(floor(f));
    let t = f - floor(f);
    let a = mix(ripple_texel(i.x, i.y), ripple_texel(i.x + 1, i.y), t.x);
    let b = mix(ripple_texel(i.x, i.y + 1), ripple_texel(i.x + 1, i.y + 1), t.x);
    return mix(a, b, t.y);
}

fn ripple_slope(p: vec2<f32>) -> vec2<f32> {
    let e = g.sim.y / g.sim.z;
    return vec2<f32>(
        ripple_at(p + vec2<f32>(e, 0.0)).x - ripple_at(p - vec2<f32>(e, 0.0)).x,
        ripple_at(p + vec2<f32>(0.0, e)).x - ripple_at(p - vec2<f32>(0.0, e)).x,
    ) / (2.0 * e);
}

const PI: f32 = 3.14159265;
// dir.x, dir.z, amplitude, wavelength. The same table lives in water.rs for buoyancy.
const WAVES = array<vec4<f32>, 4>(
    vec4<f32>(0.8, -0.6, 0.075, 7.0),
    vec4<f32>(-0.35, -0.94, 0.045, 3.9),
    vec4<f32>(0.95, 0.3, 0.028, 2.3),
    vec4<f32>(-0.7, 0.7, 0.016, 1.4),
);
const STEEP: f32 = 0.55;

fn board_w() -> i32 { return i32(g.board.x); }
fn board_h() -> i32 { return i32(g.board.y); }
fn board_half() -> vec2<f32> { return g.board.xy * 0.5; }

fn cell_state(idx: i32) -> f32 {
    if (idx < 0 || idx >= board_w() * board_h()) { return -1.0; }
    return g.cells[idx / 4][idx % 4];
}

fn cell_index(p: vec2<f32>) -> i32 {
    let half = board_half();
    if (abs(p.x) >= half.x || abs(p.y) >= half.y) { return -1; }
    let i = i32(floor(p.x + half.x));
    let j = i32(floor(p.y + half.y));
    return i + j * board_w();
}

fn cell_center(idx: i32) -> vec2<f32> {
    let w = board_w();
    return vec2<f32>(f32(idx % w) + 0.5, f32(idx / w) + 0.5) - board_half();
}

fn sky(dir: vec3<f32>) -> vec3<f32> {
    let up = clamp(dir.y, -1.0, 1.0);
    // late evening: a deep blue dome over a warm, dim horizon
    let zenith = vec3<f32>(0.035, 0.075, 0.17);
    let horizon = vec3<f32>(0.46, 0.38, 0.34);
    var c = mix(horizon, zenith, pow(max(up, 0.0), 0.4));
    if (up < 0.0) { c = mix(horizon * 0.5, vec3<f32>(0.01, 0.03, 0.05), min(-up * 3.0, 1.0)); }
    let s = max(dot(dir, g.sun.xyz), 0.0);
    c += vec3<f32>(1.0, 0.62, 0.32) * pow(s, 8.0) * 0.35;
    c += vec3<f32>(1.0, 0.9, 0.7) * pow(s, 1200.0) * 60.0;
    return c;
}

fn caustics(p: vec2<f32>, t: f32) -> f32 {
    var uv = p * 1.6;
    var v = 0.0;
    for (var i = 0; i < 3; i++) {
        let fi = f32(i);
        uv += vec2<f32>(sin(uv.y * 1.3 + t * (0.7 + fi * 0.2)), cos(uv.x * 1.1 - t * (0.6 + fi * 0.15))) * 0.45;
        v += abs(sin(uv.x + fi) * sin(uv.y - fi));
    }
    return pow(clamp(1.0 - v / 1.8, 0.0, 1.0), 3.0);
}

// the pointer's ring round the point it picks: about the same size on screen however far out
// the camera is (never under 0.17 m), breathing slowly
fn cursor_ring_r(t: f32) -> f32 {
    return max(0.17, 0.016 * length(g.cam.xyz - g.cursor.xyz)) + 0.015 * sin(t * 3.0);
}
