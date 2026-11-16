// the opaque scene under the water and its depth (group 1, water pass only)
@group(1) @binding(0) var behind: texture_2d<f32>;
@group(1) @binding(1) var depth_tex: texture_depth_2d;

// march steps: the screen-space reflection, a cloud seen in the water, a cloud seen directly
const SSR_STEPS: i32 = 40;
const CLOUD_REFLECT_STEPS: i32 = 28;
const CLOUD_VIEW_STEPS: i32 = 72;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) rest: vec2<f32>,
    @location(2) nrm: vec3<f32>,
    @location(3) crest: f32,
};

@vertex
fn vs(@location(0) xz: vec2<f32>) -> VOut {
    let t = g.cam.w;
    var p = vec3<f32>(xz.x, 0.0, xz.y);
    var nx = 0.0;
    var nz = 0.0;
    var ny = 1.0;
    for (var i = 0; i < 4; i++) {
        let w = WAVES[i];
        let d = normalize(w.xy);
        let k = 2.0 * PI / w.w;
        let c = sqrt(9.8 / k);
        let f = k * (dot(d, xz) - c * t);
        let a = w.z;
        let q = STEEP / (k * a * 4.0);
        let cf = cos(f);
        let sf = sin(f);
        p.x += q * a * d.x * cf;
        p.z += q * a * d.y * cf;
        p.y += a * sf;
        let wa = k * a;
        nx -= d.x * wa * cf;
        nz -= d.y * wa * cf;
        ny -= q * wa * sf;
    }
    p.y += ripple_at(xz).x;
    var o: VOut;
    o.clip = g.view_proj * vec4<f32>(p, 1.0);
    o.world = p;
    o.rest = xz;
    o.nrm = normalize(vec3<f32>(nx, ny, nz));
    o.crest = 1.0 - ny;
    return o;
}

// fine ripples: analytic slopes of small travelling sines, two octaves of six
fn detail(p: vec2<f32>, t: f32) -> vec2<f32> {
    var s = vec2<f32>(0.0);
    for (var i = 0; i < 12; i++) {
        let fi = f32(i);
        let ang = fi * 2.399 + 0.6;
        let d = vec2<f32>(cos(ang), sin(ang));
        let k = 4.0 + fi * 2.6;
        let a = 0.010 / (1.0 + fi * 0.5);
        let ph = k * dot(d, p) - t * sqrt(9.8 * k) * 0.55;
        s += d * (k * a * cos(ph));
    }
    return s;
}

// world position of the opaque surface seen at offscreen pixel px
fn scene_at(px: vec2<i32>) -> vec3<f32> {
    let d = textureLoad(depth_tex, px, 0);
    let uv = (vec2<f32>(px) + 0.5 - g.viewport.xy) / g.viewport.zw;
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let w = g.inv_view_proj * vec4<f32>(ndc, d, 1.0);
    return w.xyz / w.w;
}

// Screen-space reflection: march the reflected ray through the solid scene's depth buffer and
// take the colour where it passes behind a surface (tiles, the boat, mines, the wreck, the
// cursor light). (colour, t along the ray, weight); weight 0 when nothing was hit.
fn ssr(p: vec3<f32>, r: vec3<f32>) -> vec4<f32> {
    let dims = vec2<i32>(textureDimensions(behind));
    var t = 0.04;
    for (var k = 0; k < SSR_STEPS; k++) {
        let q = p + r * t;
        let clip = g.view_proj * vec4<f32>(q, 1.0);
        if (clip.w <= 0.0) { break; }
        let ndc = clip.xyz / clip.w;
        if (abs(ndc.x) > 1.0 || abs(ndc.y) > 1.0) { break; }
        let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
        let px = vec2<i32>(g.viewport.xy + uv * g.viewport.zw);
        if (any(px < vec2<i32>(0)) || any(px >= dims)) { break; }
        let d = textureLoad(depth_tex, px, 0);
        if (ndc.z > d && d < 1.0) {
            // behind a surface: count it only when the ray is just behind it (not passing
            // under an object far in front)
            let surf = scene_at(px);
            let cam_to_q = distance(g.cam.xyz, q);
            let cam_to_s = distance(g.cam.xyz, surf);
            if (cam_to_q - cam_to_s < 0.35 + t * 0.12) {
                let edge = smoothstep(1.0, 0.85, max(abs(ndc.x), abs(ndc.y)));
                let fade = 1.0 - smoothstep(2.0, 3.5, t);
                return vec4<f32>(textureLoad(behind, px, 0).rgb, edge * fade);
            }
        }
        t = t * 1.13 + 0.015;
        if (t > 3.5) { break; }
    }
    return vec4<f32>(0.0);
}

@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    let t = g.cam.w;
    // fewer fine ripples where a pixel spans a lot of water: no shimmering far away
    let footprint = length(fwidth(i.rest));
    let fade = 1.0 / (1.0 + footprint * footprint * 900.0);
    let sl = detail(i.world.xz, t) * fade;
    let rp = ripple_at(i.rest);
    let rs = ripple_slope(i.rest);
    let n = normalize(i.nrm + vec3<f32>(-sl.x - rs.x, 0.0, -sl.y - rs.y));
    let v = normalize(g.cam.xyz - i.world);
    let l = g.sun.xyz;
    let ndv = max(dot(n, v), 0.0);
    let fres = 0.02 + 0.98 * pow(1.0 - ndv, 5.0);

    // reflection: the sky, never below the horizon
    var r = reflect(-v, n);
    r.y = max(r.y, 0.015);
    var refl = sky(normalize(r));
    // what stands on and above the water is mirrored in it: tiles, the boat, mines, the
    // cursor light (screen space) and the number clouds (ray-traced)
    let rn = normalize(r);
    var mirror = ssr(i.world, rn);
    let mirrored = cloud_hit(i.world, rn, CLOUD_REFLECT_STEPS);
    if (mirrored.x > 0.0) {
        mirror = vec4<f32>(cloud_shade(i.world + rn * mirrored.x, i32(mirrored.y), -rn, false), 1.0);
    }
    let sunlit = cloud_shadow(i.world, l);

    // refraction: what lies behind, bent by the normal, then absorbed along the path
    let dims = vec2<i32>(textureDimensions(behind));
    let px = vec2<i32>(i.clip.xy);
    let straight = scene_at(px);
    let thick0 = clamp(distance(straight, i.world), 0.0, 3.0);
    let bend = vec2<f32>(n.x, -n.z) * g.viewport.w * 0.045 * min(thick0, 1.2);
    var spx = clamp(px + vec2<i32>(bend), vec2<i32>(0), dims - vec2<i32>(1));
    var hit = scene_at(spx);
    if (hit.y > i.world.y) {
        // bent onto something above the water (a tile's top): don't pull it under
        spx = px;
        hit = straight;
    }
    let thick = min(distance(hit, i.world), 40.0);
    let sigma = vec3<f32>(1.10, 0.52, 0.36);
    let trans = exp(-sigma * thick);
    // light scattered back out of the water body; brighter up a lit crest
    let sss = pow(max(dot(v, -l) * 0.5 + 0.5, 0.0), 4.0) * max(i.world.y + 0.06, 0.0) * 5.0;
    var inscatter = vec3<f32>(0.002, 0.014, 0.020) + vec3<f32>(0.0, 0.10, 0.09) * sss;

    // the board: glow under revealed cells, hover, grid lines
    let idx = cell_index(i.rest);
    if (idx >= 0) {
        if (cell_state(idx) > 0.5) {
            let dc = length(i.rest - cell_center(idx));
            inscatter += vec3<f32>(0.0, 0.10, 0.09) * (1.0 - smoothstep(0.15, 0.75, dc));
        }
        if (f32(idx) == g.info.x && g.info.w < 0.5) {
            inscatter += vec3<f32>(0.05, 0.07, 0.08);
        }
    }
    let refr = textureLoad(behind, spx, 0).rgb * trans + inscatter * (1.0 - trans);
    var c = mix(refr, refl, fres);
    // mirrored things read stronger than physics would give from this steep a view: a
    // stylised floor of 30% so the boat, tiles and clouds show in the water
    c = mix(c, mirror.rgb, mirror.a * max(fres, 0.3));

    if (idx >= 0 && g.board.z > 0.5) {
        let gd = abs(i.rest + board_half() - round(i.rest + board_half()));
        let aa = max(footprint * 1.5, 0.012);
        let line = 1.0 - smoothstep(aa * 0.5, aa * 1.8, min(gd.x, gd.y));
        c = mix(c, vec3<f32>(0.30, 0.45, 0.50), line * 0.12);
    }
    // the board's outline (off in the menu, which has no grid)
    let half = board_half();
    let out = max(abs(i.rest.x) - half.x, abs(i.rest.y) - half.y);
    c = mix(c, vec3<f32>(0.6, 0.75, 0.8), (1.0 - smoothstep(0.0, 0.05, abs(out))) * 0.35 * g.board.z);

    // sun glints, widened as the pixel grows so they stay round instead of sparkling
    let h = normalize(l + v);
    let shine = mix(1400.0, 200.0, clamp(footprint * 25.0, 0.0, 1.0));
    c += vec3<f32>(1.0, 0.72, 0.45) * pow(max(dot(n, h), 0.0), shine) * (shine / 1400.0) * 9.0 * sunlit;

    // foam: steep crests, the cursor wake, splashes; laced like the real thing
    let lace = caustics(i.rest * 1.9 + sl * 3.0, t * 0.4);
    var foam = smoothstep(0.26, 0.40, i.crest + length(sl) * 0.4) * lace * 0.5;
    foam = max(foam, smoothstep(0.08, 0.8, rp.y) * (0.25 + 1.1 * lace));

    if (g.boom.w > 0.5) {
        let bc = g.boom.xy;
        let bt = g.boom.z;
        let d = length(i.rest - bc);
        let ring = 1.0 - smoothstep(0.0, 0.35, abs(d - bt * 7.0));
        foam = max(foam, ring * exp(-bt * 0.9));
        c += vec3<f32>(1.0, 0.25, 0.05) * exp(-d * 1.2) * exp(-bt * 1.6) * 3.0;
    }
    // foam is lit, not glowing: sun plus a share of the sky
    let foam_col = vec3<f32>(0.75, 0.78, 0.80) * (0.35 + 0.65 * max(l.y, 0.0));
    c = mix(c, foam_col, clamp(foam, 0.0, 1.0) * 0.75);

    // under a cloud's shadow the water dims
    c *= 0.62 + 0.38 * sunlit;

    // the cursor's light: a warm pool on the water and its glint dancing on the ripples
    if (g.cursor.w > 0.0) {
        let to_l = g.cursor.xyz - i.world;
        let dl = length(to_l);
        let ld = to_l / dl;
        let hl = normalize(ld + v);
        let glow_col = vec3<f32>(1.0, 0.78, 0.45) * g.cursor.w;
        c += glow_col * pow(max(dot(n, hl), 0.0), 220.0) * 6.0 / (1.0 + dl * dl * 2.0);
        c += glow_col * 0.25 * max(dot(n, ld), 0.0) / (1.0 + dl * dl * 6.0);
    }
    let dist = length(g.cam.xyz - i.world) / g.sun.w;
    let fog = clamp((dist - 22.0) / 30.0, 0.0, 1.0);
    c = mix(c, sky(normalize(vec3<f32>(i.world.x - g.cam.x, 0.02, i.world.z - g.cam.z))) * 0.8, fog);
    return vec4<f32>(c, 1.0);
}

// ---- the cloud pass: a full-viewport triangle that ray-marches the number clouds ----

@vertex
fn cloud_vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn cloud_fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = (pos.xy - g.viewport.xy) / g.viewport.zw;
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let near = g.inv_view_proj * vec4<f32>(ndc, 0.0, 1.0);
    let far = g.inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
    let ro = g.cam.xyz;
    let rd = normalize(far.xyz / far.w - near.xyz / near.w);
    let hit = cloud_hit(ro, rd, CLOUD_VIEW_STEPS);
    if (hit.x < 0.0) { discard; }
    // behind something solid (the boat's mast, say): not drawn
    let behind_px = vec2<i32>(pos.xy);
    if (distance(scene_at(behind_px), ro) < hit.x) { discard; }
    let p = ro + rd * hit.x;
    let c = cloud_shade(p, i32(hit.y), -rd, true);
    return vec4<f32>(c, 1.0);
}
