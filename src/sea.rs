//! The floating world both screens share: a physics world on the swell, the interactive
//! ripples, the water clock, the cursor's wake, the boat and what is left of it after a blast.
//! `Game` adds the minesweeper rules on top, `Lobby` the menu.
//!
//! Every list of floating things is a `Vec` of something that holds a `Body`; entries leave
//! those lists only through `drop_bodies` (or a `Drop`), which also removes their physics body,
//! so a list and the world cannot drift apart.

use oxijolt::BuoyancySettings;
use oxijolt::prelude::math::*;
use oxijolt::prelude::*;

use crate::boat::{self, Boat};
use crate::ripple::Ripple;
use crate::water;

/// The fixed physics step, seconds.
pub const STEP: f32 = 1.0 / 60.0;
/// Seconds an opened tile takes to dissolve into the water.
pub const FADE: f32 = 0.5;
/// A sea mine's radius.
pub const MINE_R: f32 = 0.3;
/// How strongly a whole tile floats (a multiple of what keeps it level with the surface).
pub const TILE_BUOYANCY: f32 = 3.2;
/// A thrown tile barely floats: it falls back in and sinks.
pub const THROWN_BUOYANCY: f32 = 0.12;
pub const MINE_BUOYANCY: f32 = 1.9;
const DEBRIS_BUOYANCY: f32 = 2.6;
/// A loose body this far under water is out of sight for good and is removed.
const SINK_DEPTH: f32 = -1.9;
/// Half the thickness the splash test uses for a piece of the wreck.
const DEBRIS_HALF_H: f32 = 0.03;

/// The physics side of a floating thing.
pub struct Body {
    pub id: BodyId,
    pub mass: f32,
    /// Tied to `home` by a soft spring; false once it flies off.
    pub anchored: bool,
    pub buoyancy: f32,
    /// A launched body splashes once, when it falls back in.
    pub splash: bool,
    /// Where an anchored body is held (world x, z).
    pub home: (f32, f32),
    /// Rotational symmetry the anchor keeps it square to: 4 for a square tile, 2 for an
    /// oblong one.
    pub sym: f32,
    /// Position and rotation (x, y, z, w) before the last step, to draw between steps.
    pub prev: ([f32; 3], [f32; 4]),
    /// Seconds into dissolving (negative: still waiting its turn); None while whole.
    pub fade: Option<f32>,
}

/// Anything that floats: it has a `Body`, and maybe more (a cell, a menu entry, a size).
pub trait Floating {
    fn body(&self) -> &Body;
    fn body_mut(&mut self) -> &mut Body;
}

impl Floating for Body {
    fn body(&self) -> &Body {
        self
    }
    fn body_mut(&mut self) -> &mut Body {
        self
    }
}

/// A charred piece of the boat after a blast, with its size (world units).
pub struct Debris {
    pub body: Body,
    pub size: [f32; 3],
}

impl Floating for Debris {
    fn body(&self) -> &Body {
        &self.body
    }
    fn body_mut(&mut self) -> &mut Body {
        &mut self.body
    }
}

/// A body's position as glam. oxijolt's `Real` is `f32` unless its `double-precision`
/// feature is on; the casts keep this building either way.
#[allow(clippy::unnecessary_cast)]
pub fn xyz(p: RVec3) -> glam::Vec3 {
    glam::Vec3::new(p.x as f32, p.y as f32, p.z as f32)
}

/// Where body `id` is now, if it still exists.
pub fn position_of(world: &PhysicsWorld, id: BodyId) -> Option<glam::Vec3> {
    world.body(id).ok().map(|b| xyz(b.position()))
}

/// Creates a dynamic body of `shape` at `at`, held there (x, z) by the anchor spring.
pub fn spawn_anchored(
    world: &mut PhysicsWorld,
    shape: &Shape,
    settings: BodySettings,
    at: [f32; 3],
    buoyancy: f32,
    sym: f32,
) -> Result<Body, BodyError> {
    let id = world.create_body(shape, &settings.position(RVec3::new(at[0], at[1], at[2])))?;
    let mass = world.body(id).ok().and_then(|b| b.mass()).unwrap_or(1.0);
    Ok(Body { id, mass, anchored: true, buoyancy, splash: false, home: (at[0], at[2]), sym, prev: (at, [0.0, 0.0, 0.0, 1.0]), fade: None })
}

/// Removes the entries of `list` that `gone` picks, each with its physics body.
pub fn drop_bodies<T: Floating>(world: &mut PhysicsWorld, list: &mut Vec<T>, mut gone: impl FnMut(&T) -> bool) {
    list.retain(|e| {
        let go = gone(e);
        if go {
            let _ = world.remove_body(e.body().id);
        }
        !go
    });
}

pub struct Sea {
    pub world: PhysicsWorld,
    pub ripple: Ripple,
    /// Water clock, seconds.
    pub time: f32,
    /// Time not yet stepped, under one `STEP`.
    acc: f32,
    /// Cursor on the water (world xz), and where it was at the last step.
    pub cursor: Option<(f32, f32)>,
    last_cursor: Option<(f32, f32)>,
    pub boat: Boat,
    /// What is left of the boat after a blast.
    pub debris: Vec<Debris>,
    /// The last blast (world x, z) and seconds since.
    pub boom: Option<(f32, f32)>,
    pub boom_t: f32,
}

impl Sea {
    /// A sea whose ripples cover the square [-half, half]², with the boat in it.
    pub fn new(half: f32, boat: Boat) -> Sea {
        Sea {
            world: PhysicsWorld::new(WorldSettings::default()).expect("physics world"),
            ripple: Ripple::covering(half),
            time: 0.0,
            acc: 0.0,
            cursor: None,
            last_cursor: None,
            boat,
            debris: Vec::new(),
            boom: None,
            boom_t: 0.0,
        }
    }

    /// Moves the cursor without a wake between the old and the new spot (the screen changed
    /// under a still pointer).
    pub fn warp_cursor(&mut self, at: Option<(f32, f32)>) {
        self.cursor = at;
        self.last_cursor = None;
    }

    /// Adds `dt` seconds of wall time and returns how many fixed steps are now due.
    pub fn due_steps(&mut self, dt: f32) -> usize {
        self.acc = (self.acc + dt.min(0.1)).min(STEP * 5.0);
        let mut n = 0;
        while self.acc >= STEP {
            self.acc -= STEP;
            n += 1;
        }
        n
    }

    /// How far behind the last step the frame is drawn, seconds: frames show the world between
    /// its last two steps, so motion stays even when frames come faster than the 60 Hz steps.
    pub fn draw_lag(&self) -> f32 {
        STEP - self.acc
    }

    /// Where `body` is drawn this frame: between its pose before the last step and its pose now.
    pub fn draw_pose(&self, body: &Body) -> Option<(glam::Vec3, glam::Quat)> {
        pose_between(&self.world, body, self.acc / STEP)
    }

    /// The boat's pose this frame.
    pub fn draw_boat(&self) -> boat::Pose {
        self.boat.draw_pose(1.0 - self.draw_lag() / STEP)
    }

    /// How far a dissolving tile has gone, 0 (whole) to 1 (gone), drawn between steps.
    pub fn fade_of(&self, body: &Body) -> f32 {
        body.fade.map_or(0.0, |f| ((f + self.acc) / FADE).clamp(0.0, 1.0))
    }

    /// A blast at (x, z): the water is thrown up and the clock for its glow starts.
    pub fn blast(&mut self, x: f32, z: f32, power: f32) {
        self.boom = Some((x, z));
        self.boom_t = 0.0;
        self.ripple.splash(x, z, power);
    }

    /// The boat goes up in a blast at `at` (world x, z): its parts become charred pieces
    /// thrown a little way off, which then float.
    pub fn wreck_boat(&mut self, at: (f32, f32), seed: u64) {
        if !self.boat.alive() {
            return;
        }
        let pose = self.boat.pose();
        self.boat.wreck();
        self.debris.extend(wreck_pieces(&mut self.world, &pose, at, seed));
    }

    pub fn clear_debris(&mut self) {
        drop_bodies(&mut self.world, &mut self.debris, |_| true);
    }

    /// The first part of a fixed step: the clock, the cursor's wake and the boat. Returns what
    /// the boat reached.
    pub fn begin_step(&mut self) -> Option<boat::Event> {
        self.time += STEP;
        self.boom_t += STEP;
        if let (Some(a), Some(b)) = (self.last_cursor, self.cursor)
            && (a.0 - b.0).abs() + (a.1 - b.1).abs() > 0.002
        {
            self.ripple.drag(a, b);
        }
        self.last_cursor = self.cursor;
        self.boat.step(STEP, self.time, &mut self.ripple)
    }

    /// Advances the dissolving tiles of `tiles` and removes those that are gone. Returns where
    /// (each tile's home) a film broke this step, the moment it starts to melt.
    pub fn melt<T: Floating>(&mut self, tiles: &mut Vec<T>) -> Vec<(f32, f32)> {
        let mut broke = Vec::new();
        for t in tiles.iter_mut() {
            let b = t.body_mut();
            if let Some(f) = b.fade.as_mut() {
                let was = *f;
                *f += STEP;
                if was < 0.0 && *f >= 0.0 {
                    broke.push(b.home);
                }
            }
        }
        drop_bodies(&mut self.world, tiles, |t| t.body().fade.is_some_and(|f| f >= FADE));
        broke
    }

    /// One step of water for `list` (see `float_bodies`); what sank out of reach is removed.
    pub fn float<T: Floating>(&mut self, list: &mut Vec<T>, half_h: f32) {
        let sunk = float_bodies(&mut self.world, &mut self.ripple, self.time, list, half_h);
        if !sunk.is_empty() {
            drop_bodies(&mut self.world, list, |e| sunk.contains(&e.body().id));
        }
    }

    /// The last part of a fixed step: the wreck floats, physics and ripples move on.
    pub fn end_step(&mut self) {
        let mut debris = std::mem::take(&mut self.debris);
        self.float(&mut debris, DEBRIS_HALF_H);
        self.debris = debris;
        let _ = self.world.step(STEP);
        self.ripple.step();
    }
}

impl Drop for Sea {
    fn drop(&mut self) {
        self.clear_debris();
    }
}

/// One fixed step of water for a list of floating bodies: buoyancy against the swell plus the
/// ripples, a splash when a thrown body falls back in, and for anchored ones a soft spring to
/// `home` that also keeps them square. Returns the loose bodies that sank out of reach.
fn float_bodies<T: Floating>(world: &mut PhysicsWorld, ripple: &mut Ripple, t: f32, list: &mut [T], half_h: f32) -> Vec<BodyId> {
    let gravity = world.gravity();
    let mut sunk = Vec::new();
    for entry in list.iter_mut() {
        let body = entry.body_mut();
        let Ok(b) = world.body(body.id) else { continue };
        let p = xyz(b.position());
        let v = b.linear_velocity();
        let w = b.angular_velocity();
        let q = b.rotation();
        body.prev = (p.to_array(), [q.x, q.y, q.z, q.w]);
        if !body.anchored && p.y < SINK_DEPTH {
            sunk.push(body.id);
            continue;
        }
        let (h, n) = water::surface(p.x, p.z, t);
        let (sx, sz) = ripple.slope(p.x, p.z);
        let h = h + ripple.height(p.x, p.z);
        let nl = ((n[0] - sx).powi(2) + n[1].powi(2) + (n[2] - sz).powi(2)).sqrt();
        let normal = Vec3::new((n[0] - sx) / nl, n[1] / nl, (n[2] - sz) / nl);
        // only a real hit stirs it: a slow bob feeding back would ring the tiles up
        if body.splash && p.y - h < half_h && v.y < -0.8 {
            body.splash = false;
            ripple.splash(p.x, p.z, (-v.y * 0.25).min(0.8));
        }
        let fluid = BuoyancySettings::default()
            .surface(RVec3::new(p.x, h, p.z), normal)
            .buoyancy(body.buoyancy)
            .linear_drag(1.6)
            .angular_drag(if body.anchored { 2.5 } else { 0.4 });
        let Ok(mut bm) = world.body_mut(body.id) else { continue };
        let _ = bm.apply_buoyancy_impulse(&fluid, gravity, STEP);
        if body.anchored {
            let (ax, az) = body.home;
            let m = body.mass;
            // a spring with damping toward home, in the water plane
            let _ = bm.add_force(Vec3::new(m * (14.0 * (ax - p.x) - 4.0 * v.x), 0.0, m * (14.0 * (az - p.z) - 4.0 * v.z)));
            // keep it square to the grid (any multiple of 360° / sym)
            let xa = (1.0 - 2.0 * (q.y * q.y + q.z * q.z), 2.0 * (q.x * q.z - q.w * q.y));
            let yaw = (-xa.1).atan2(xa.0);
            let k = body.sym;
            let yaw_k = (k * yaw).sin().atan2((k * yaw).cos()) / k;
            let inertia = m * 0.12;
            let _ = bm.add_torque(Vec3::new(0.0, -inertia * (30.0 * yaw_k + 6.0 * w.y), 0.0));
        }
    }
    sunk
}

/// Where `body` is drawn: `a` of the way from its pose before the last step to its pose now.
pub fn pose_between(world: &PhysicsWorld, body: &Body, a: f32) -> Option<(glam::Vec3, glam::Quat)> {
    let b = world.body(body.id).ok()?;
    let q = b.rotation();
    let now = (xyz(b.position()), glam::Quat::from_xyzw(q.x, q.y, q.z, q.w));
    let was = (glam::Vec3::from_array(body.prev.0), glam::Quat::from_array(body.prev.1));
    Some((was.0.lerp(now.0, a), was.1.slerp(now.1, a)))
}

/// The boat goes up: each of its parts (and a few splinters) becomes a charred floating piece,
/// thrown away from `blast` (world x, z).
fn wreck_pieces(world: &mut PhysicsWorld, pose: &boat::Pose, blast: (f32, f32), seed: u64) -> Vec<Debris> {
    let hull = pose.hull();
    let (bx, bz) = blast;
    let mut rng = fastrand::Rng::with_seed(seed ^ 0xb0a7);
    let mut pieces: Vec<(glam::Vec3, [f32; 3], glam::Quat)> = boat::PARTS
        .iter()
        .enumerate()
        .map(|(k, part)| {
            let turn = if k == boat::BOW { glam::Quat::from_rotation_y(std::f32::consts::FRAC_PI_4) } else { glam::Quat::IDENTITY };
            (glam::Vec3::from_array(part.offset), part.size, turn)
        })
        .collect();
    // splinters off the hull
    for _ in 0..6 {
        let off = glam::Vec3::new(rng.f32() * 0.3 - 0.15, 0.05, rng.f32() * 0.14 - 0.07);
        pieces.push((off, [0.05 + rng.f32() * 0.08, 0.015, 0.02 + rng.f32() * 0.02], glam::Quat::IDENTITY));
    }
    let (_, hull_rot, _) = hull.to_scale_rotation_translation();
    let mut out = Vec::new();
    for (off, size, turn) in pieces {
        let p = hull.transform_point3(off);
        let rot = hull_rot * turn;
        let size = size.map(|v| v * boat::SCALE);
        let half = Vec3::new((size[0] * 0.5).max(0.012), (size[1] * 0.5).max(0.012), (size[2] * 0.5).max(0.012));
        let Ok(shape) = Shape::new_box_with_convex_radius(half, 0.005) else { continue };
        let Ok(id) = world.create_body(
            &shape,
            &BodySettings::new_dynamic().position(RVec3::new(p.x, p.y, p.z)).rotation(Quat::from_xyzw(rot.x, rot.y, rot.z, rot.w)),
        ) else {
            continue;
        };
        let mass = world.body(id).ok().and_then(|b| b.mass()).unwrap_or(1.0);
        let away =
            glam::Vec2::new(p.x - bx, p.z - bz).normalize_or(glam::Vec2::new(rng.f32() - 0.5, rng.f32() - 0.5).normalize_or(glam::Vec2::X));
        let throw = 1.2 + rng.f32() * 1.6;
        if let Ok(mut b) = world.body_mut(id) {
            let _ = b.add_impulse(Vec3::new(away.x * throw * mass, (2.8 + rng.f32() * 2.4) * mass, away.y * throw * mass));
            let spin = mass * (size[0] + size[2]) * 0.6;
            let _ = b.add_angular_impulse(Vec3::new((rng.f32() - 0.5) * spin, (rng.f32() - 0.5) * spin, (rng.f32() - 0.5) * spin));
        }
        let body = Body {
            id,
            mass,
            anchored: false,
            buoyancy: DEBRIS_BUOYANCY,
            splash: true,
            home: (p.x, p.z),
            sym: 4.0,
            prev: ([p.x, p.y, p.z], [rot.x, rot.y, rot.z, rot.w]),
            fade: None,
        };
        out.push(Debris { body, size });
    }
    out
}
