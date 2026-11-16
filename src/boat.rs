//! The little boat that sails to the clicked cell through the channels between the tiles.
//!
//! Its motion follows Disney's principles of animation:
//! - anticipation: before it sets off it squats on its stern for a beat, then lurches forward;
//! - slow in / slow out: speed ramps up and brakes into the stop, never linear;
//! - arcs: corners of the channel route are rounded, the heading turns on a spring;
//! - exaggeration + secondary action: a theatrical side-to-side rock that grows with speed,
//!   a lean into turns, a nose-up under acceleration and a nose-dip when braking;
//! - follow-through / overlapping action: mast and flag are looser springs that trail the hull
//!   and keep swinging after it stops;
//! - squash & stretch: the hull squashes a little as it drops onto a swell and stretches rising.

use glam::{Mat4, Quat, Vec2, Vec3};

use crate::game::Dims;
use crate::ripple::Ripple;
use crate::water;

/// Where the boat waits: off the board's top-left corner.
pub fn start(dims: Dims) -> Vec2 {
    let (hw, hh) = dims.half();
    Vec2::new(-hw - 1.4, -hh - 1.4)
}
/// The model's size: big enough to read from the camera, narrow enough for the channels.
pub const SCALE: f32 = 1.55;
const VMAX: f32 = 5.0;
const ACCEL: f32 = 14.0;
const BRAKE: f32 = 9.0;
/// How far before the cell's middle the tile starts to melt in front of the bow.
const DOOR: f32 = 0.75;
const CORNER: f32 = 0.16;
const WINDUP: f32 = 0.16;

#[derive(Clone, Copy)]
struct Spring {
    x: f32,
    v: f32,
}

impl Spring {
    const fn rest() -> Spring {
        Spring { x: 0.0, v: 0.0 }
    }
    /// Damped spring toward `target`: k stiffness, c damping (low c overshoots more).
    fn step(&mut self, target: f32, k: f32, c: f32, dt: f32) {
        let a = k * (target - self.x) - c * self.v;
        self.v += a * dt;
        self.x += self.v * dt;
    }
}

/// What a frame needs to draw the boat.
#[derive(Clone, Copy)]
pub struct Pose {
    pub pos: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
    pub squash: f32,
    pub mast: f32,
    pub flag: f32,
}

impl Pose {
    fn lerp(a: &Pose, b: &Pose, t: f32) -> Pose {
        let l = |x: f32, y: f32| x + (y - x) * t;
        let mut dyaw = b.yaw - a.yaw;
        dyaw = (dyaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        Pose {
            pos: a.pos.lerp(b.pos, t),
            yaw: a.yaw + dyaw * t,
            pitch: l(a.pitch, b.pitch),
            roll: l(a.roll, b.roll),
            squash: l(a.squash, b.squash),
            mast: l(a.mast, b.mast),
            flag: l(a.flag, b.flag),
        }
    }

    /// The hull's frame: forward is local +X, up +Y.
    pub fn hull(&self) -> Mat4 {
        let rot = Quat::from_rotation_y(self.yaw) * Quat::from_rotation_z(self.pitch) * Quat::from_rotation_x(self.roll);
        Mat4::from_scale_rotation_translation(Vec3::splat(SCALE), rot, self.pos)
    }
}

/// What a step of the boat tells the game.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    /// The bow is at the cell: its tile should melt now.
    Door(usize),
    /// The boat stopped in the middle of the cell.
    Arrived(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Idle,
    WindUp,
    Sailing,
    Wrecked,
}

pub struct Boat {
    pub pos: Vec2,
    pub phase: Phase,
    /// The cell it is sailing to; opened when it gets there.
    pub target: Option<usize>,
    path: Vec<Vec2>,
    door: bool,
    s: f32,
    len: f32,
    speed: f32,
    yaw: f32,
    phase_t: f32,
    pitch: Spring,
    roll: Spring,
    squash: Spring,
    mast: Spring,
    heave: Spring,
    flag: f32,
    t: f32,
    pose: Pose,
    prev: Pose,
}

impl Boat {
    pub fn new(dims: Dims) -> Boat {
        Boat::at(start(dims))
    }

    pub fn at(at: Vec2) -> Boat {
        let (h, _) = water::surface(at.x, at.y, water::Swell::default());
        let yaw = -std::f32::consts::FRAC_PI_4; // facing into the board, south-east
        let pose = Pose { pos: Vec3::new(at.x, h, at.y), yaw, pitch: 0.0, roll: 0.0, squash: 0.0, mast: 0.0, flag: 0.0 };
        Boat {
            pos: at,
            phase: Phase::Idle,
            target: None,
            path: Vec::new(),
            door: false,
            s: 0.0,
            len: 0.0,
            speed: 0.0,
            yaw,
            phase_t: 0.0,
            pitch: Spring::rest(),
            roll: Spring::rest(),
            squash: Spring::rest(),
            mast: Spring::rest(),
            heave: Spring { x: h, v: 0.0 },
            flag: 0.0,
            t: 0.0,
            pose,
            prev: pose,
        }
    }

    pub fn alive(&self) -> bool {
        self.phase != Phase::Wrecked
    }

    /// The pose drawn this frame, `a` of the way from the previous step to the last one.
    pub fn draw_pose(&self, a: f32) -> Pose {
        Pose::lerp(&self.prev, &self.pose, a)
    }

    /// The pose after the last step (where the wreck starts from).
    pub fn pose(&self) -> Pose {
        self.pose
    }

    /// Sets course for `cell` along the channels. From rest it winds up first; already under
    /// way it simply re-routes from where it is.
    pub fn sail_to(&mut self, cell: usize, dims: Dims) {
        if !self.alive() {
            return;
        }
        self.path = route(self.pos, cell, dims);
        self.len = length(&self.path);
        self.s = 0.0;
        self.door = false;
        self.target = Some(cell);
        if self.phase == Phase::Idle {
            self.phase = Phase::WindUp;
            self.phase_t = 0.0;
        }
    }

    /// Sets off for any point of open water along a smooth curve out of its current heading
    /// (the menu: no channels there). Nothing happens on arrival but a stop.
    pub fn sail_free(&mut self, to: Vec2) {
        if !self.alive() {
            return;
        }
        let d = to.distance(self.pos);
        if d < 0.05 {
            return;
        }
        let fwd = Vec2::new(self.yaw.cos(), -self.yaw.sin());
        let ctrl = self.pos + fwd * (d * 0.35).min(1.0);
        self.path = (0..=16)
            .map(|k| {
                let t = k as f32 / 16.0;
                self.pos * (1.0 - t) * (1.0 - t) + ctrl * 2.0 * t * (1.0 - t) + to * t * t
            })
            .collect();
        self.len = length(&self.path);
        self.s = 0.0;
        self.door = true;
        self.target = None;
        if self.phase == Phase::Idle {
            self.phase = Phase::WindUp;
            self.phase_t = 0.0;
        }
    }

    pub fn wreck(&mut self) {
        self.phase = Phase::Wrecked;
        self.target = None;
    }

    /// One fixed step. Returns `Door` when the bow has just reached the target cell and
    /// `Arrived` when the boat has just stopped in its middle.
    pub fn step(&mut self, dt: f32, swell: water::Swell, ripple: &mut Ripple) -> Option<Event> {
        self.prev = self.pose;
        self.t += dt;
        if !self.alive() {
            return None;
        }
        self.phase_t += dt;
        let old_speed = self.speed;
        let mut arrived = None;
        let mut squat = 0.0;
        match self.phase {
            Phase::Idle => self.speed = 0.0,
            Phase::WindUp => {
                // anticipation: sink onto the stern, nose up, a hair backwards
                let k = (self.phase_t / WINDUP).min(1.0);
                squat = (k * std::f32::consts::PI).sin();
                self.speed = -0.12 * squat;
                if self.phase_t >= WINDUP {
                    self.phase = Phase::Sailing;
                    self.speed = 0.35; // the lurch
                }
            }
            Phase::Sailing => {
                let left = (self.len - self.s).max(0.0);
                let brake = (2.0 * BRAKE * left).sqrt();
                self.speed = (self.speed + ACCEL * dt).min(VMAX).min(brake.max(0.08));
                self.s = (self.s + self.speed * dt).min(self.len);
                if !self.door && self.len - self.s <= DOOR {
                    self.door = true;
                    arrived = self.target.map(Event::Door);
                }
                if self.len - self.s < 0.004 {
                    self.speed = 0.0;
                    self.phase = Phase::Idle;
                    arrived = self.target.take().map(Event::Arrived);
                }
            }
            Phase::Wrecked => {}
        }

        let old = self.pos;
        let tangent;
        if self.phase == Phase::WindUp {
            // settle back along the old heading while already swinging round to the new course
            let dir = Vec2::new(self.yaw.cos(), -self.yaw.sin());
            self.pos += dir * self.speed * dt;
            tangent = if self.path.is_empty() { dir } else { sample(&self.path, 0.05).1 };
        } else if !self.path.is_empty() {
            let (p, tan) = sample(&self.path, self.s);
            self.pos = p;
            tangent = tan;
        } else {
            tangent = Vec2::new(self.yaw.cos(), -self.yaw.sin());
        }

        // heading on a spring-like chase: arcs, never snapping
        let want = (-tangent.y).atan2(tangent.x);
        let mut d = want - self.yaw;
        d = (d + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        let turn = d * (14.0 * dt).min(1.0);
        self.yaw += turn;
        let yaw_rate = turn / dt;
        let accel = (self.speed - old_speed) / dt;
        let v = self.speed.max(0.0) / VMAX;

        // the water under it
        let (h, n) = water::surface(self.pos.x, self.pos.y, swell);
        let h = h + ripple.height(self.pos.x, self.pos.y);
        let old_heave = self.heave.x;
        self.heave.step(h + 0.01 - squat * 0.035, 160.0, 18.0, dt);
        let vy = (self.heave.x - old_heave) / dt;

        // the swell tilts the hull: slope along and across the heading
        let fwd = Vec2::new(self.yaw.cos(), -self.yaw.sin());
        let side = Vec2::new(-fwd.y, fwd.x);
        let slope_fwd = -(n[0] * fwd.x + n[2] * fwd.y) / n[1];
        let slope_side = -(n[0] * side.x + n[2] * side.y) / n[1];

        // exaggerated rock: a slow theatrical sway that grows with speed, plus a lean into turns
        let sway = (self.t * 2.0 * std::f32::consts::PI * 0.85).sin() * (0.05 + 0.16 * v);
        let roll_target = sway - yaw_rate * 0.10 * (0.3 + v) + slope_side.atan() * 0.8;
        self.roll.step(roll_target, 40.0, 4.5, dt);
        // nose up when pulling away (and squatting in the wind-up), nose down when braking
        let bob = (self.t * 2.0 * std::f32::consts::PI * 1.3).sin() * 0.035 * v;
        let pitch_target = (accel * 0.07).clamp(-0.35, 0.35) + squat * 0.22 + bob + slope_fwd.atan() * 0.8;
        self.pitch.step(pitch_target, 55.0, 5.0, dt);
        // squash on the way down, stretch on the way up; the wind-up squats it
        let squash_target = (vy * 0.5).clamp(-0.12, 0.12) - squat * 0.12;
        self.squash.step(squash_target, 120.0, 9.0, dt);
        // mast: a looser spring that trails the hull's tilt and the pull of acceleration
        let mast_target = -(accel * 0.05).clamp(-0.25, 0.25) - self.pitch.x * 0.6;
        self.mast.step(mast_target, 26.0, 2.2, dt);
        self.flag = (self.t * (9.0 + 8.0 * v)).sin() * (0.15 + 0.45 * v);

        // wake: foam and a little trough behind it while it moves
        if (self.pos - old).length() > 0.0005 && self.phase == Phase::Sailing {
            let stern = self.pos - fwd * 0.2;
            let stern_old = old - fwd * 0.2;
            ripple.drag((stern_old.x, stern_old.y), (stern.x, stern.y));
        }

        self.pose = Pose {
            pos: Vec3::new(self.pos.x, self.heave.x, self.pos.y),
            yaw: self.yaw,
            pitch: self.pitch.x,
            roll: self.roll.x,
            squash: self.squash.x,
            mast: self.mast.x,
            flag: self.flag,
        };
        arrived
    }
}

/// A route from `from` to the side of `cell`, only along the channels (integer x or z lines
/// inside the board), with rounded corners.
pub fn route(from: Vec2, cell: usize, dims: Dims) -> Vec<Vec2> {
    let (hw, hh) = dims.half();
    let (ci, cj) = ((cell % dims.w) as f32, (cell / dims.w) as f32);
    let (x0, x1, z0, z1) = (ci - hw, ci + 1.0 - hw, cj - hh, cj + 1.0 - hh);
    let middle = Vec2::new(ci + 0.5 - hw, cj + 0.5 - hh);
    // join the channels at one of the four crossings around the boat, whichever makes the
    // whole trip shortest (the nearest one can lie behind it, which made it turn back first)
    // crossings sit at integer offsets from the board's corner
    let (ox, oz) = (hw.fract(), hh.fract());
    let cx = |v: f32, f: fn(f32) -> f32| (f(v - ox) + ox).clamp(-hw, hw);
    let cz = |v: f32, f: fn(f32) -> f32| (f(v - oz) + oz).clamp(-hh, hh);
    let entries = [
        Vec2::new(cx(from.x, f32::floor), cz(from.y, f32::floor)),
        Vec2::new(cx(from.x, f32::ceil), cz(from.y, f32::floor)),
        Vec2::new(cx(from.x, f32::floor), cz(from.y, f32::ceil)),
        Vec2::new(cx(from.x, f32::ceil), cz(from.y, f32::ceil)),
    ];
    // each side of the cell: its midpoint and its two corners
    let sides = [
        (Vec2::new(middle.x, z0), [Vec2::new(x0, z0), Vec2::new(x1, z0)]),
        (Vec2::new(middle.x, z1), [Vec2::new(x0, z1), Vec2::new(x1, z1)]),
        (Vec2::new(x0, middle.y), [Vec2::new(x0, z0), Vec2::new(x0, z1)]),
        (Vec2::new(x1, middle.y), [Vec2::new(x1, z0), Vec2::new(x1, z1)]),
    ];
    let manhattan = |a: Vec2, b: Vec2| (a.x - b.x).abs() + (a.y - b.y).abs();
    let mut best: Option<(f32, Vec<Vec2>)> = None;
    for entry in entries {
        for (mid, corners) in sides {
            for c in corners {
                for bend in [Vec2::new(c.x, entry.y), Vec2::new(entry.x, c.y)] {
                    // then straight in across the side and on to the middle of the cell
                    let pts = vec![from, entry, bend, c, mid, middle];
                    let cost = from.distance(entry) + manhattan(entry, c) + manhattan(c, mid);
                    if best.as_ref().is_none_or(|(b, _)| cost < *b - 1e-4) {
                        best = Some((cost, pts));
                    }
                }
            }
        }
    }
    let mut pts = best.map(|(_, p)| p).unwrap_or_else(|| vec![from]);
    pts.dedup_by(|a, b| a.distance(*b) < 1e-3);
    round_corners(&pts, CORNER)
}

fn round_corners(pts: &[Vec2], r: f32) -> Vec<Vec2> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let mut out = vec![pts[0]];
    for w in pts.windows(3) {
        let (a, p, b) = (w[0], w[1], w[2]);
        let (d1, d2) = ((p - a).normalize_or_zero(), (b - p).normalize_or_zero());
        let r = r.min((p - a).length() * 0.5).min((b - p).length() * 0.5);
        if d1.dot(d2) > 0.999 || r < 1e-3 {
            out.push(p);
            continue;
        }
        let (s, e) = (p - d1 * r, p + d2 * r);
        for k in 0..=6 {
            let t = k as f32 / 6.0;
            // quadratic Bezier through the corner: a smooth arc
            out.push(s * (1.0 - t) * (1.0 - t) + p * 2.0 * t * (1.0 - t) + e * t * t);
        }
    }
    out.push(*pts.last().expect("non-empty"));
    out.dedup_by(|a, b| a.distance(*b) < 1e-4);
    out
}

fn length(path: &[Vec2]) -> f32 {
    path.windows(2).map(|w| w[0].distance(w[1])).sum()
}

/// Point and unit tangent at arc length `s`.
fn sample(path: &[Vec2], s: f32) -> (Vec2, Vec2) {
    let mut left = s;
    for w in path.windows(2) {
        let seg = w[0].distance(w[1]);
        if seg < 1e-6 {
            continue;
        }
        if left <= seg {
            return (w[0] + (w[1] - w[0]) * (left / seg), (w[1] - w[0]) / seg);
        }
        left -= seg;
    }
    let n = path.len();
    let tan = if n >= 2 { (path[n - 1] - path[n - 2]).normalize_or_zero() } else { Vec2::X };
    (path[n - 1], tan)
}

/// One box of the boat, in the boat's own frame.
pub struct Part {
    pub offset: [f32; 3],
    pub size: [f32; 3],
    pub color: [f32; 3],
    pub metal: f32,
    pub rough: f32,
}

const fn part(offset: [f32; 3], size: [f32; 3], color: [f32; 3], metal: f32, rough: f32) -> Part {
    Part { offset, size, color, metal, rough }
}

/// The boat's parts. Shared by the renderer and by the wreck, which turns each part into a
/// floating piece.
pub const PARTS: [Part; 7] = [
    part([0.0, 0.03, 0.0], [0.36, 0.09, 0.18], [0.72, 0.10, 0.07], 0.0, 0.45), // hull
    part([0.18, 0.03, 0.0], [0.125, 0.09, 0.125], [0.72, 0.10, 0.07], 0.0, 0.45), // bow (turned 45°)
    part([0.0, 0.08, 0.0], [0.37, 0.022, 0.186], [0.92, 0.90, 0.85], 0.0, 0.5), // white rail
    part([-0.06, 0.14, 0.0], [0.14, 0.10, 0.12], [0.93, 0.86, 0.72], 0.0, 0.6), // cabin
    part([-0.06, 0.20, 0.0], [0.17, 0.022, 0.15], [0.12, 0.30, 0.62], 0.0, 0.35), // roof
    part([-0.11, 0.25, 0.0], [0.04, 0.08, 0.04], [0.95, 0.72, 0.10], 0.0, 0.4), // funnel
    part([0.09, 0.25, 0.0], [0.016, 0.32, 0.016], [0.80, 0.80, 0.82], 0.8, 0.3), // mast
];
pub const BOW: usize = 1;
pub const MAST: usize = 6;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::tests::TEST_DIMS;

    /// Sails from the mooring to `cell`: the boat, and when its bow reached the cell and when
    /// it stopped, seconds.
    fn sail(dims: Dims, cell: usize) -> (Boat, f32, f32) {
        let mut ripple = Ripple::new();
        let mut boat = Boat::new(dims);
        boat.sail_to(cell, dims);
        let (mut door, mut arrived, mut t) = (None, None, 0.0);
        while arrived.is_none() && t < 20.0 {
            match boat.step(1.0 / 60.0, water::Swell { clock: t, ..Default::default() }, &mut ripple) {
                Some(Event::Door(c)) => door = Some((c, t)),
                Some(Event::Arrived(c)) => arrived = Some((c, t)),
                None => {}
            }
            t += 1.0 / 60.0;
        }
        let (dc, dt) = door.expect("the bow reached the cell");
        let (ac, at) = arrived.expect("it arrived");
        assert_eq!((dc, ac), (cell, cell));
        (boat, dt, at)
    }

    #[test]
    fn it_sails_into_the_middle_of_the_cell_door_first() {
        let (boat, dt, at) = sail(TEST_DIMS, 57);
        assert!(dt < at, "the tile melts before the boat is in");
        let middle = Vec2::new(7.0 - 4.5, 5.0 - 4.5);
        assert!(boat.pos.distance(middle) < 0.01, "stopped at {} not in the middle", boat.pos);
        assert!(at < 4.0, "took {at:.2} s: too slow");
    }

    #[test]
    fn on_a_wide_board_it_stops_in_the_middle_of_the_cell() {
        let dims = Dims { w: 30, h: 16 };
        for cell in [0, 29, 7 * 30 + 3, 15 * 30, dims.count() - 1] {
            let (boat, _, _) = sail(dims, cell);
            let (x, z) = dims.center(cell);
            assert!(boat.pos.distance(Vec2::new(x, z)) < 0.01, "cell {cell}: stopped at {} not at ({x}, {z})", boat.pos);
        }
    }

    #[test]
    fn the_route_never_starts_by_going_away_from_the_target() {
        // from the middle of an opened cell, to a cell to the right of it
        let from = Vec2::new(-0.5, -0.5);
        let path = route(from, 4 * 10 + 8, TEST_DIMS);
        let first = (path[1] - path[0]).normalize_or_zero();
        let goal = (Vec2::new(8.0 - 4.5, 4.0 - 4.5) - from).normalize_or_zero();
        assert!(first.dot(goal) > 0.0, "first leg {first} points away from {goal}");
    }
}
