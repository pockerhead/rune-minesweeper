struct VIn {
    @location(0) pos: vec3<f32>,
    @location(1) nrm: vec3<f32>,
    @location(2) m0: vec4<f32>,
    @location(3) m1: vec4<f32>,
    @location(4) m2: vec4<f32>,
    @location(5) m3: vec4<f32>,
    // xyz scale, w kind (0 plain, 1 seabed)
    @location(6) scale: vec4<f32>,
    @location(7) color: vec4<f32>,
    // x metallic, y roughness, z emissive, w a menu label's look (0 rest, 1 hovered, 2 pressed)
    @location(8) mat: vec4<f32>,
    // a menu tile's label: its row on the sheet (v top, v bottom), z how much of it shows
    @location(9) label: vec4<f32>,
};

// the menu labels, painted by Rune (labels.rs): coverage in alpha, one row per tile
@group(1) @binding(0) var label_sheet: texture_2d<f32>;
@group(1) @binding(1) var label_sampler: sampler;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) nrm: vec3<f32>,
    @location(2) color: vec4<f32>,
    @location(3) mat: vec4<f32>,
    @location(4) kind: f32,
    // where on the label sheet this point of a tile's top face is (0 elsewhere)
    @location(5) sheet_uv: vec2<f32>,
    // x 1 on a labelled top face, y how much of the label shows, z the label's look, w the row's
    // height on the sheet
    @location(6) @interpolate(flat) label: vec4<f32>,
};

@vertex
fn vs(v: VIn) -> VOut {
    let m = mat4x4<f32>(v.m0, v.m1, v.m2, v.m3);
    let world = (m * vec4<f32>(v.pos * v.scale.xyz, 1.0)).xyz;
    let n = normalize((m * vec4<f32>(v.nrm / v.scale.xyz, 0.0)).xyz);
    var o: VOut;
    o.clip = g.view_proj * vec4<f32>(world, 1.0);
    o.world = world;
    o.nrm = n;
    o.color = v.color;
    o.mat = v.mat;
    o.kind = v.scale.w;
    // a labelled tile prints its row across its top face: u along its width, v from its far
    // edge (the top of the view) to its near one
    let top = v.label.z > 0.0 && v.nrm.y > 0.5;
    let rows = v.label.y - v.label.x;
    o.sheet_uv = vec2<f32>(v.pos.x + 0.5, v.label.x + (v.pos.z + 0.5) * rows);
    o.label = vec4<f32>(select(0.0, 1.0, top), v.label.z, v.mat.w, rows);
    return o;
}

fn ggx(nh: f32, a: f32) -> f32 {
    let a2 = a * a;
    let d = nh * nh * (a2 - 1.0) + 1.0;
    return a2 / (PI * d * d);
}

@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    let t = g.cam.w;
    var n = normalize(i.nrm);
    let v = normalize(g.cam.xyz - i.world);
    let l = g.sun.xyz;
    var base = i.color.rgb;
    let metal = i.mat.x;
    var rough = max(i.mat.y, 0.05);

    // a menu label pressed into the plank: dark ink with a pale lower edge, as Rune drew it
    // over the tile before. Sampled with explicit gradients, so the branch may be per tile.
    let duv_x = dpdx(i.sheet_uv);
    let duv_y = dpdy(i.sheet_uv);
    // the pointer's ring, also on what floats over the water (closed tiles, planks): centred
    // where the pick ray (through the water point under the pointer) crosses this height, so
    // it sits under the pointer on top of a tile as it does on the water
    let ring_at = g.cursor.xz + (g.cam.xz - g.cursor.xz) * (i.world.y / g.cam.y);
    let ring_d = abs(length(i.world.xz - ring_at) - cursor_ring_r(t));
    let ring_w = max(fwidth(ring_d) * 1.5, 0.02);
    // how much ink covers this point: the plank's sheen and the sky it mirrors stay off the
    // ink, so a label reads wherever the plank shines
    var ink_cover = 0.0;
    if (i.label.x > 0.5) {
        let look = i.label.z;
        let ink_at = textureSampleGrad(label_sheet, label_sampler, i.sheet_uv, duv_x, duv_y).a * i.label.y;
        // the pale edge: the ink's shape a little lower down the plank, where there is no ink
        let lip = vec2<f32>(0.0, i.label.w * select(0.016, 0.026, look > 1.5));
        let edge = textureSampleGrad(label_sheet, label_sampler, i.sheet_uv - lip, duv_x, duv_y).a * i.label.y * (1.0 - ink_at);
        // at rest dark brown; hovered a warm red-brown; pressed a deeper red on a plank in
        // its own shade, sunk deeper
        var ink = vec3<f32>(0.024, 0.012, 0.005);
        if (look > 1.5) {
            ink = vec3<f32>(0.16, 0.025, 0.004);
            base *= 0.8;
        } else if (look > 0.5) {
            ink = vec3<f32>(0.30, 0.06, 0.012);
        }
        base = mix(base * (1.0 + edge * 0.45), ink, ink_at);
        rough = mix(rough, 0.75, ink_at);
        ink_cover = ink_at;
    }

    if (i.kind > 0.5) {
        // seabed: rippled sand
        let p = i.world.xz;
        let ripple = sin(p.x * 3.1 + sin(p.y * 1.7) * 1.5) * 0.5 + 0.5;
        base = mix(vec3<f32>(0.26, 0.23, 0.17), vec3<f32>(0.36, 0.32, 0.23), ripple);
    }

    let nl = max(dot(n, l), 0.0);
    let nv = max(dot(n, v), 0.05);
    let h = normalize(l + v);
    let nh = max(dot(n, h), 0.0);
    let f0 = mix(vec3<f32>(0.04), base, metal);
    let fres = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - max(dot(v, h), 0.0), 5.0);
    let spec = ggx(nh, rough * rough) * fres * nl / (4.0 * nv + 0.1);
    let sun_col = vec3<f32>(1.0, 0.88, 0.72) * 3.2;
    let amb = mix(vec3<f32>(0.10, 0.12, 0.14), vec3<f32>(0.30, 0.42, 0.60), n.y * 0.5 + 0.5);
    // the number clouds overhead throw soft shadows
    let lit_by_sun = select(1.0, cloud_shadow(i.world, l), i.world.y > -0.5);
    var c = base * (1.0 - metal) * (nl * sun_col * lit_by_sun + amb);
    let sheen = 1.0 - ink_cover * 0.85;
    c += spec * sun_col * lit_by_sun * sheen;
    let fv = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - nv, 5.0);
    c += sky(reflect(-v, n)) * fv * (1.0 - rough) * 0.8 * sheen;
    c += base * i.mat.z * 5.0;
    // a hovered label glows a little, so it reads as lit even in the plank's shade
    if (i.label.x > 0.5 && i.label.z > 0.5 && i.label.z < 1.5) {
        c += vec3<f32>(0.9, 0.25, 0.04) * ink_cover * 0.25;
    }

    // under the surface: light falls off with depth, caustics dance on what faces up
    // (the water pass absorbs along the view ray; here only the light that reaches down)
    let depth = -i.world.y;
    if (depth > 0.0) {
        let k = exp(-depth * vec3<f32>(0.60, 0.28, 0.20));
        let ca = caustics(i.world.xz + l.xz * depth * 0.4, t) * max(n.y, 0.0) * sheen;
        c = c * k + ca * k * vec3<f32>(0.85, 1.0, 0.9) * 1.4;
    }

    if (g.cursor.w > 0.0 && n.y > 0.5 && i.kind < 0.5) {
        let ring = 1.0 - smoothstep(ring_w * 0.5, ring_w * 1.5, ring_d);
        // a dark rim round a warm line, so it reads on pale tiles and in the sun
        let rim = 1.0 - smoothstep(ring_w * 1.5, ring_w * 3.0, ring_d);
        c = mix(c, c * 0.3, rim * 0.7 * g.cursor.w);
        c = mix(c, vec3<f32>(0.95, 0.36, 0.05), ring * 0.95 * g.cursor.w);
    }

    let dist = length(g.cam.xyz - i.world) / g.sun.w;
    c = mix(c, sky(normalize(i.world - g.cam.xyz)) * 0.9, clamp((dist - 25.0) / 30.0, 0.0, 1.0));
    return vec4<f32>(c, 1.0);
}
