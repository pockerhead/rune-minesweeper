//! The same Gerstner waves as `shaders/common.wgsl`, on the CPU, for buoyancy; and how rough the
//! player wants them (`SeaLevel`), which scales both alike through `Swell`.

use std::f32::consts::PI;

/// dir.x, dir.z, amplitude, wavelength.
pub const WAVES: [[f32; 4]; 4] = [[0.8, -0.6, 0.075, 7.0], [-0.35, -0.94, 0.045, 3.9], [0.95, 0.3, 0.028, 2.3], [-0.7, 0.7, 0.016, 1.4]];

/// Seconds the swell takes to come about two thirds of the way to a new level (nearly all the
/// way in two seconds).
const EASE: f32 = 0.6;

/// How rough the sea is: the player's setting.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SeaLevel {
    /// A low, slow swell: the board barely sways.
    Calm,
    /// The sea as it always was.
    Lively,
    /// A bit more than that.
    Rough,
}

impl SeaLevel {
    pub const ALL: [SeaLevel; 3] = [SeaLevel::Calm, SeaLevel::Lively, SeaLevel::Rough];

    /// The level a session starts with: the page's `?sea=` knob, else the player's saved choice,
    /// else calm on a phone (the sway is hard on the eyes on a small screen), lively elsewhere.
    pub fn initial(knob: Option<SeaLevel>, saved: Option<SeaLevel>, handheld: bool) -> SeaLevel {
        knob.or(saved).unwrap_or(if handheld { SeaLevel::Calm } else { SeaLevel::Lively })
    }

    /// The level a pick on the menu's sea tile turns to.
    pub fn next(self) -> SeaLevel {
        match self {
            SeaLevel::Calm => SeaLevel::Lively,
            SeaLevel::Lively => SeaLevel::Rough,
            SeaLevel::Rough => SeaLevel::Calm,
        }
    }

    /// Its name as saved and in the page's `?sea=` knob.
    pub fn key(self) -> &'static str {
        match self {
            SeaLevel::Calm => "calm",
            SeaLevel::Lively => "lively",
            SeaLevel::Rough => "rough",
        }
    }

    pub fn from_key(key: &str) -> Option<SeaLevel> {
        SeaLevel::ALL.into_iter().find(|l| l.key() == key.trim())
    }

    pub fn name(self) -> &'static str {
        match self {
            SeaLevel::Calm => "Calm",
            SeaLevel::Lively => "Lively",
            SeaLevel::Rough => "Rough",
        }
    }

    /// The swell at this level: a scale on the waves' height (and their sideways drift), and how
    /// fast they run against the water clock.
    fn swell(self) -> (f32, f32) {
        match self {
            SeaLevel::Calm => (0.35, 0.8),
            SeaLevel::Lively => (1.0, 1.0),
            SeaLevel::Rough => (1.12, 1.04),
        }
    }
}

/// The swell as it is now. Buoyancy reads it here and the shaders read it from the globals
/// (`Swell::uniform`), so both see the same waves at any level, and while it eases between two.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Swell {
    /// A scale on every wave's height: 1 at `SeaLevel::Lively`.
    pub height: f32,
    /// How fast `clock` runs against the water clock.
    pub pace: f32,
    /// The waves' own clock, seconds. It runs at `pace`, so a change of pace never jumps them.
    pub clock: f32,
}

impl Swell {
    /// The swell settled at `level`, its clock at 0.
    pub fn settled(level: SeaLevel) -> Swell {
        let (height, pace) = level.swell();
        Swell { height, pace, clock: 0.0 }
    }

    /// `dt` seconds on, easing toward `level`.
    pub fn step(&mut self, dt: f32, level: SeaLevel) {
        let (height, pace) = level.swell();
        let k = 1.0 - (-dt / EASE).exp();
        self.height += (height - self.height) * k;
        self.pace += (pace - self.pace) * k;
        self.clock += dt * self.pace;
    }

    /// The swell `dt` seconds of water clock ago (a frame drawn between steps).
    pub fn back(self, dt: f32) -> Swell {
        Swell { clock: self.clock - dt * self.pace, ..self }
    }

    /// For the shaders' globals (`swell` in common.wgsl): x height, y clock.
    pub fn uniform(self) -> [f32; 4] {
        [self.height, self.clock, 0.0, 0.0]
    }
}

impl Default for Swell {
    fn default() -> Swell {
        Swell::settled(SeaLevel::Lively)
    }
}

/// Surface height and normal at rest position (x, z) under `swell`. Horizontal Gerstner drift
/// (up to about 24 cm at `Lively`) is left out; the height error it leaves against the rendered
/// surface is about 1.5 cm rms.
pub fn surface(x: f32, z: f32, swell: Swell) -> (f32, [f32; 3]) {
    let (mut h, mut nx, mut nz) = (0.0, 0.0, 0.0);
    for [dx, dz, a, len] in WAVES {
        let a = a * swell.height;
        let l = (dx * dx + dz * dz).sqrt();
        let (dx, dz) = (dx / l, dz / l);
        let k = 2.0 * PI / len;
        let c = (9.8 / k).sqrt();
        let f = k * (dx * x + dz * z - c * swell.clock);
        h += a * f.sin();
        nx -= dx * k * a * f.cos();
        nz -= dz * k * a * f.cos();
    }
    let l = (nx * nx + 1.0 + nz * nz).sqrt();
    (h, [nx / l, 1.0 / l, nz / l])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phone_starts_calm_unless_told_otherwise() {
        use SeaLevel::*;
        assert_eq!(SeaLevel::initial(None, None, true), Calm);
        assert_eq!(SeaLevel::initial(None, None, false), Lively);
        assert_eq!(SeaLevel::initial(None, Some(Rough), true), Rough, "the player's choice");
        assert_eq!(SeaLevel::initial(Some(Lively), Some(Rough), true), Lively, "the page's knob");
    }

    #[test]
    fn levels_cycle_and_go_by_their_keys() {
        let mut l = SeaLevel::Calm;
        for want in [SeaLevel::Lively, SeaLevel::Rough, SeaLevel::Calm] {
            l = l.next();
            assert_eq!(l, want);
        }
        for l in SeaLevel::ALL {
            assert_eq!(SeaLevel::from_key(l.key()), Some(l));
        }
        assert_eq!(SeaLevel::from_key("stormy"), None);
    }

    /// The height scales the waves and nothing else: heights and slopes in proportion.
    #[test]
    fn the_height_scales_the_waves() {
        let lively = Swell { clock: 3.7, ..Swell::settled(SeaLevel::Lively) };
        let calm = Swell { height: 0.35, ..lively };
        for (x, z) in [(0.0, 0.0), (1.3, -2.1), (-4.0, 3.5)] {
            let (h1, n1) = surface(x, z, lively);
            let (h2, n2) = surface(x, z, calm);
            assert!((h2 - 0.35 * h1).abs() < 1e-6, "height at ({x}, {z})");
            assert!((n2[0] / n2[1] - 0.35 * n1[0] / n1[1]).abs() < 1e-5, "x slope at ({x}, {z})");
            assert!((n2[2] / n2[1] - 0.35 * n1[2] / n1[1]).abs() < 1e-5, "z slope at ({x}, {z})");
        }
    }

    /// A new level eases in over a second or two, and the waves' phase never jumps.
    #[test]
    fn a_new_level_eases_in() {
        let mut s = Swell::settled(SeaLevel::Lively);
        let dt = 1.0 / 60.0;
        let mut last = s.clock;
        for k in 1..=240 {
            s.step(dt, SeaLevel::Calm);
            assert!(s.clock > last && s.clock - last <= dt + 1e-6, "the clock stepped {}", s.clock - last);
            last = s.clock;
            if k == 15 {
                assert!(s.height > 0.6, "a quarter second in it snapped to {}", s.height);
            }
        }
        assert!((s.height - 0.35).abs() < 0.01 && (s.pace - 0.8).abs() < 0.01, "settled at {s:?}");
    }
}
