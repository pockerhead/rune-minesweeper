//! The same Gerstner waves as `shaders/common.wgsl`, on the CPU, for buoyancy.

use std::f32::consts::PI;

/// dir.x, dir.z, amplitude, wavelength.
pub const WAVES: [[f32; 4]; 4] = [[0.8, -0.6, 0.075, 7.0], [-0.35, -0.94, 0.045, 3.9], [0.95, 0.3, 0.028, 2.3], [-0.7, 0.7, 0.016, 1.4]];

/// Surface height and normal at rest position (x, z), time t. Horizontal Gerstner drift (up
/// to about 24 cm) is left out; the height error it leaves against the rendered surface is
/// about 1.5 cm rms.
pub fn surface(x: f32, z: f32, t: f32) -> (f32, [f32; 3]) {
    let (mut h, mut nx, mut nz) = (0.0, 0.0, 0.0);
    for [dx, dz, a, len] in WAVES {
        let l = (dx * dx + dz * dz).sqrt();
        let (dx, dz) = (dx / l, dz / l);
        let k = 2.0 * PI / len;
        let c = (9.8 / k).sqrt();
        let f = k * (dx * x + dz * z - c * t);
        h += a * f.sin();
        nx -= dx * k * a * f.cos();
        nz -= dz * k * a * f.cos();
    }
    let l = (nx * nx + 1.0 + nz * nz).sqrt();
    (h, [nx / l, 1.0 / l, nz / l])
}
