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
    // x metallic, y roughness, z emissive
    @location(8) mat: vec4<f32>,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) nrm: vec3<f32>,
    @location(2) color: vec4<f32>,
    @location(3) mat: vec4<f32>,
    @location(4) kind: f32,
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
    let rough = max(i.mat.y, 0.05);

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
    c += spec * sun_col * lit_by_sun;
    let fv = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - nv, 5.0);
    c += sky(reflect(-v, n)) * fv * (1.0 - rough) * 0.8;
    c += base * i.mat.z * 5.0;

    // under the surface: light falls off with depth, caustics dance on what faces up
    // (the water pass absorbs along the view ray; here only the light that reaches down)
    let depth = -i.world.y;
    if (depth > 0.0) {
        let k = exp(-depth * vec3<f32>(0.60, 0.28, 0.20));
        let ca = caustics(i.world.xz + l.xz * depth * 0.4, t) * max(n.y, 0.0);
        c = c * k + ca * k * vec3<f32>(0.85, 1.0, 0.9) * 1.4;
    }

    let dist = length(g.cam.xyz - i.world) / g.sun.w;
    c = mix(c, sky(normalize(i.world - g.cam.xyz)) * 0.9, clamp((dist - 25.0) / 30.0, 0.0, 1.0));
    return vec4<f32>(c, 1.0);
}
