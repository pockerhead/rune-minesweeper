//! Interactive water on the CPU: a heightfield wave equation (finite differences, two buffers)
//! plus a foam field that decays. The cursor drags a foam wake through it, clicks splash, and
//! floating tiles ride it (buoyancy reads `height`) and splash it when they fall back in.
//! Uploaded to the GPU every frame as an `Rg32Float` texture: r height, g foam.

pub const N: usize = 160;
/// (c·dt/dx)², stable below 0.5.
const ALPHA: f32 = 0.33;
const DAMP: f32 = 0.986;
const FOAM_DECAY: f32 = 0.975;
/// Cursor wake: metres of surface pressed down per world unit the cursor travels, the
/// longest step that counts in full, the disc it presses, and foam laid per step.
const WAKE_PRESS: f32 = 0.05;
const WAKE_MAX_LEN: f32 = 0.5;
const WAKE_RADIUS: f32 = 0.26;
const WAKE_FOAM: f32 = 0.88;

pub struct Ripple {
    h: Vec<f32>,
    prev: Vec<f32>,
    pub foam: Vec<f32>,
    /// Scratch for the smoothing pass, kept to save an allocation per step.
    smooth: Vec<f32>,
    /// The simulated square: its low corner (x and z) and side, world units.
    pub min: f32,
    pub size: f32,
    dx: f32,
}

impl Ripple {
    #[cfg(test)]
    pub fn new() -> Ripple {
        Ripple::covering(8.0)
    }

    /// Water simulated over the square [-half, half]² (the board plus a margin).
    pub fn covering(half: f32) -> Ripple {
        let size = 2.0 * half;
        Ripple {
            h: vec![0.0; N * N],
            prev: vec![0.0; N * N],
            foam: vec![0.0; N * N],
            smooth: Vec::with_capacity(N * N),
            min: -half,
            size,
            dx: size / N as f32,
        }
    }

    pub fn step(&mut self) {
        let mut next = std::mem::take(&mut self.prev);
        let h = &self.h;
        let foam = &mut self.foam;
        let at = |ii: isize, jj: isize| {
            let ii = ii.clamp(0, N as isize - 1) as usize;
            let jj = jj.clamp(0, N as isize - 1) as usize;
            h[jj * N + ii]
        };
        for j in 0..N {
            for i in 0..N {
                let k = j * N + i;
                let (ii, jj) = (i as isize, j as isize);
                let lap = at(ii - 1, jj) + at(ii + 1, jj) + at(ii, jj - 1) + at(ii, jj + 1) - 4.0 * h[k];
                // `next` holds the previous step here
                let v = 2.0 * h[k] - next[k] + ALPHA * lap;
                // edges soak waves up instead of bouncing them back
                let edge = i.min(j).min(N - 1 - i).min(N - 1 - j) as f32;
                let soak = if edge < 12.0 { 0.9 + 0.0075 * edge } else { 1.0 };
                next[k] = v * DAMP * soak;
                let speed = (h[k] - next[k]).abs();
                foam[k] = (foam[k] * FOAM_DECAY + (speed - 0.006).max(0.0) * 6.0).min(1.5);
            }
        }
        // a little viscosity: blend toward the neighbours' mean, which kills the grid-scale
        // checkerboard that pure finite differences leave ringing
        let mut src = std::mem::take(&mut self.smooth);
        src.clear();
        src.extend_from_slice(&next);
        for j in 1..N - 1 {
            for i in 1..N - 1 {
                let k = j * N + i;
                let avg = 0.25 * (src[k - 1] + src[k + 1] + src[k - N] + src[k + N]);
                next[k] = src[k] * 0.85 + avg * 0.15;
            }
        }
        self.smooth = src;
        self.prev = std::mem::replace(&mut self.h, next);
    }

    fn cell(&self, x: f32, z: f32) -> (f32, f32) {
        ((x - self.min) / self.dx - 0.5, (z - self.min) / self.dx - 0.5)
    }

    /// Bilinear height at world (x, z); 0 outside.
    pub fn height(&self, x: f32, z: f32) -> f32 {
        let (fx, fz) = self.cell(x, z);
        if fx < 0.0 || fz < 0.0 || fx >= (N - 1) as f32 || fz >= (N - 1) as f32 {
            return 0.0;
        }
        let (i, j) = (fx as usize, fz as usize);
        let (tx, tz) = (fx - i as f32, fz - j as f32);
        let a = self.h[j * N + i] * (1.0 - tx) + self.h[j * N + i + 1] * tx;
        let b = self.h[(j + 1) * N + i] * (1.0 - tx) + self.h[(j + 1) * N + i + 1] * tx;
        a * (1.0 - tz) + b * tz
    }

    pub fn slope(&self, x: f32, z: f32) -> (f32, f32) {
        let e = self.dx;
        ((self.height(x + e, z) - self.height(x - e, z)) / (2.0 * e), (self.height(x, z + e) - self.height(x, z - e)) / (2.0 * e))
    }

    /// Pushes the surface by `dh` (negative presses it down) in a smooth disc, adds foam.
    pub fn push(&mut self, x: f32, z: f32, radius: f32, dh: f32, foam: f32) {
        let (cx, cz) = self.cell(x, z);
        let r = radius / self.dx;
        let (i0, i1) = ((cx - r).floor().max(0.0) as usize, ((cx + r).ceil() as usize).min(N - 1));
        let (j0, j1) = ((cz - r).floor().max(0.0) as usize, ((cz + r).ceil() as usize).min(N - 1));
        if cx + r < 0.0 || cz + r < 0.0 || i0 > i1 || j0 > j1 {
            return;
        }
        for j in j0..=j1 {
            for i in i0..=i1 {
                let d = ((i as f32 - cx).powi(2) + (j as f32 - cz).powi(2)).sqrt() / r;
                if d < 1.0 {
                    let w = 0.5 + 0.5 * (d * std::f32::consts::PI).cos();
                    let k = j * N + i;
                    self.h[k] += dh * w;
                    self.foam[k] = (self.foam[k] + foam * w).min(1.5);
                }
            }
        }
    }

    /// A foam wake along a segment (the cursor between two steps). The surface is pressed in
    /// proportion to the distance moved, so a slow cursor that lingers over a spot doesn't dig a
    /// pit into it; the foam is laid per step, so a slow cursor still leaves a dense trail.
    pub fn drag(&mut self, a: (f32, f32), b: (f32, f32)) {
        let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        let n = (len / (self.dx * 1.5)).ceil().clamp(1.0, 40.0) as usize;
        let press = WAKE_PRESS * len.min(WAKE_MAX_LEN) / n as f32;
        let foam = WAKE_FOAM / n as f32;
        // `a` is where the previous segment ended and was already pushed
        for s in 1..=n {
            let t = s as f32 / n as f32;
            let (x, z) = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
            self.push(x, z, WAKE_RADIUS, -press, foam);
        }
    }

    /// A splash: a deep press in the middle ringed by a lift, so it bulges out as a ring.
    pub fn splash(&mut self, x: f32, z: f32, power: f32) {
        self.push(x, z, 0.4, -0.14 * power, 0.9 * power);
        self.push(x, z, 0.8, 0.035 * power, 0.0);
    }

    /// Texels for the GPU: (height, foam) pairs, row by row.
    pub fn texels(&self, out: &mut Vec<f32>) {
        out.clear();
        out.reserve(N * N * 2);
        for k in 0..N * N {
            out.push(self.h[k]);
            out.push(self.foam[k]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Largest |height|; infinite if any texel went non-finite (`f32::max` would skip a NaN).
    fn max_abs(r: &Ripple) -> f32 {
        r.h.iter().fold(0.0f32, |m, v| if v.is_finite() { m.max(v.abs()) } else { f32::INFINITY })
    }

    #[test]
    fn a_splash_dies_out() {
        let mut r = Ripple::new();
        r.splash(0.0, 0.0, 1.0);
        let start = max_abs(&r);
        assert!(start > 0.05, "the splash moved nothing: {start}");
        for s in 1..=600 {
            r.step();
            let mh = max_abs(&r);
            // the ring may focus for a moment, but never runs away
            assert!(mh < 3.0 * start, "step {s}: max|h| {mh} against the splash's {start}");
        }
        let mh = max_abs(&r);
        let foamy = r.foam.iter().filter(|&&f| f > 0.12).count();
        assert!(mh < 0.001, "still {mh} after 10 s");
        assert_eq!(foamy, 0, "foam left after 10 s");
    }

    #[test]
    fn a_slow_drag_does_not_dig_in() {
        // a cursor creeping 1 cm per step, back and forth, for 10 s
        let mut r = Ripple::new();
        let mut x = -1.0f32;
        for s in 0..600 {
            let nx = if (s / 200) % 2 == 0 { x + 0.01 } else { x - 0.01 };
            r.drag((x, 0.0), (nx, 0.0));
            x = nx;
            r.step();
        }
        let mh = max_abs(&r);
        assert!(mh < 0.01, "a slow drag pressed the water {mh} m");
        assert!(r.foam.iter().any(|&f| f > 0.5), "a slow drag left no foam");
    }
}
