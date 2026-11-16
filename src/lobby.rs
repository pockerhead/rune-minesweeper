//! The menu's world: the same sea, a boat you can sail anywhere, a mine adrift that blows it
//! up (both come back after ten seconds), a few number clouds, and the menu itself on floating
//! tiles. Picking an item blows its tile up like a mine; the rest melt; the next menu surfaces
//! while the boat heads back to its mooring.

use glam::Vec2;
use oxijolt::prelude::math::*;
use oxijolt::prelude::*;

use crate::boat::Boat;
use crate::game::Dims;
use crate::sea::{self, Body, Floating, MINE_R, Sea, position_of, spawn_anchored};

/// A virtual grid the decor clouds sit on (and the camera frames).
pub const GRID: Dims = Dims { w: 12, h: 10 };
/// Where the boat waits: above the menu.
pub const BASE: Vec2 = Vec2::new(0.0, -4.4);
/// Where the mine drifts, off to the side of the menu, until it has a lane (`Lobby::set_lane`).
pub const MINE_HOME: (f32, f32) = (4.4, 1.2);
/// Seconds the mine takes to drift across its lane, one way.
pub const DRIFT_CROSSING: f32 = 20.0;
/// How far past the view's side edges the mine turns back, metres: out of sight.
const DRIFT_OFF: f32 = 1.4;
/// The nearest (in z) the mine's lane runs to the menu: past the lowest tile of the longest
/// menu (five tiles), with room for the mine and its bobbing.
pub const DRIFT_MIN_Z: f32 = 3.9;
/// How far along its lane the mine starts, so it shows from the first frame.
const DRIFT_START: f32 = 0.3;
/// Seconds after a blast until the boat and the mine are back.
const RESPAWN: f32 = 10.0;
/// A menu tile's size: width, thickness, depth.
pub const ITEM: [f32; 3] = [4.4, 0.14, 0.86];
const ITEM_GAP: f32 = 1.12;
/// How close the boat may come to the mine before it goes off.
const MINE_REACH: f32 = 0.55;

/// The line the mine drifts along, across the lower part of the view: from x0 (the view's left
/// edge) to x1 (its right edge) at depth z.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Lane {
    pub x0: f32,
    pub x1: f32,
    pub z: f32,
}

/// Where the mine is held `t` seconds in: it sets off from part way along `lane`, crosses
/// from beyond one side of the view to beyond the other in `DRIFT_CROSSING` seconds, turns
/// back out of sight, and crosses again.
pub fn drift_at(lane: Lane, t: f32) -> (f32, f32) {
    let (a, b) = (lane.x0 - DRIFT_OFF, lane.x1 + DRIFT_OFF);
    let phase = (t / DRIFT_CROSSING + DRIFT_START).rem_euclid(2.0);
    let s = if phase < 1.0 { phase } else { 2.0 - phase };
    (a + (b - a) * s, lane.z.max(DRIFT_MIN_Z))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Play,
    Custom,
    Records,
    Level(usize),
    Start,
    Back,
    /// A value the custom game adjusts: 0 width, 1 height, 2 mines. The tile's left half
    /// lowers it, its right half raises it.
    Adjust(usize),
    /// A tile that only shows something (a record).
    Show,
}

/// A menu entry on its floating tile.
pub struct MenuTile {
    pub body: Body,
    pub label: String,
    pub action: Action,
    /// Thrown off by a pick (it no longer takes clicks or shows its label).
    pub blown: bool,
}

impl Floating for MenuTile {
    fn body(&self) -> &Body {
        &self.body
    }
    fn body_mut(&mut self) -> &mut Body {
        &mut self.body
    }
}

pub struct Lobby {
    pub sea: Sea,
    /// The mine adrift: none from a blast until it respawns.
    pub mine: Vec<Body>,
    pub tiles: Vec<MenuTile>,
    respawn_at: Option<f32>,
    /// The mine's lane across the view, once the app has framed it.
    lane: Option<Lane>,
    mine_shape: Shape,
    tile_shape: Shape,
    seed: u64,
    /// Decor number clouds: (cell of `GRID`, digit).
    pub clouds: Vec<(usize, u8)>,
}

impl Lobby {
    pub fn new(seed: u64) -> Lobby {
        let mut lobby = Lobby {
            sea: Sea::new(9.5, Boat::at(BASE)),
            mine: Vec::new(),
            tiles: Vec::new(),
            respawn_at: None,
            lane: None,
            mine_shape: Shape::new_sphere(MINE_R).expect("mine shape"),
            tile_shape: Shape::new_box(Vec3::new(ITEM[0] * 0.5, ITEM[1] * 0.5, ITEM[2] * 0.5)).expect("item shape"),
            seed,
            // a 1, a 2 and a 3 floating around the menu, away from the tiles
            clouds: vec![(9 * GRID.w + 1, 1), (2 * GRID.w + 10, 2), (8 * GRID.w + 10, 3), (GRID.w + 1, 1)],
        };
        lobby.spawn_mine(-0.3);
        lobby
    }

    /// Where the mine is held now: on its lane, or at `MINE_HOME` without one.
    fn mine_home(&self) -> (f32, f32) {
        self.lane.map_or(MINE_HOME, |lane| drift_at(lane, self.sea.time))
    }

    /// Sets the lane the mine drifts along (see `drift_at`). The first one puts the mine there
    /// at once, bobbing up, rather than towing it across the sea.
    pub fn set_lane(&mut self, lane: Lane) {
        let first = self.lane.is_none();
        self.lane = Some(lane);
        if first && !self.mine.is_empty() {
            sea::drop_bodies(&mut self.sea.world, &mut self.mine, |_| true);
            self.spawn_mine(-0.3);
        }
    }

    fn spawn_mine(&mut self, depth: f32) {
        let (x, z) = self.mine_home();
        let settings = BodySettings::new_dynamic();
        if let Ok(mine) = spawn_anchored(&mut self.sea.world, &self.mine_shape, settings, [x, depth, z], sea::MINE_BUOYANCY, 4.0) {
            self.mine.push(mine);
        }
    }

    /// Replaces the menu: what is left of the old one goes, the new tiles surface from below
    /// one after another, and the boat heads home.
    pub fn set_menu(&mut self, entries: Vec<(String, Action)>) {
        sea::drop_bodies(&mut self.sea.world, &mut self.tiles, |_| true);
        let n = entries.len();
        for (k, (label, action)) in entries.into_iter().enumerate() {
            let z = (k as f32 - (n as f32 - 1.0) * 0.5) * ITEM_GAP + 0.3;
            // deeper the further down the list: they surface in order, top first
            let y = -0.7 - k as f32 * 0.22;
            let settings = BodySettings::new_dynamic().friction(0.4);
            let Ok(body) = spawn_anchored(&mut self.sea.world, &self.tile_shape, settings, [0.0, y, z], sea::TILE_BUOYANCY, 2.0) else {
                continue;
            };
            self.tiles.push(MenuTile { body, label, action, blown: false });
        }
        self.sea.boat.sail_free(BASE);
    }

    /// The menu tile under world point (x, z), with where on it (-1 left edge .. 1 right edge).
    /// A tile takes presses half way to its neighbours, so the gaps between tiles pick too.
    pub fn item_at(&self, x: f32, z: f32) -> Option<(usize, f32)> {
        self.tiles.iter().enumerate().find_map(|(k, t)| {
            if t.blown || t.body.fade.is_some() {
                return None;
            }
            let p = position_of(&self.sea.world, t.body.id)?;
            let (dx, dz) = (x - p.x, z - p.z);
            (dx.abs() < ITEM[0] * 0.5 && dz.abs() < ITEM_GAP * 0.5).then_some((k, dx / (ITEM[0] * 0.5)))
        })
    }

    pub fn relabel(&mut self, k: usize, label: String) {
        if let Some(t) = self.tiles.get_mut(k) {
            t.label = label;
        }
    }

    /// A picked item: its tile goes off like a mine and the others melt.
    pub fn blow(&mut self, k: usize) {
        let Some(t) = self.tiles.get_mut(k) else { return };
        let Some(p) = position_of(&self.sea.world, t.body.id) else { return };
        t.blown = true;
        t.body.anchored = false;
        t.body.buoyancy = sea::THROWN_BUOYANCY;
        t.body.splash = true;
        let (m, id) = (t.body.mass, t.body.id);
        self.sea.blast(p.x, p.z, 2.2);
        let mut rng = fastrand::Rng::with_seed(self.seed ^ (k as u64 * 977) ^ self.sea.time.to_bits() as u64);
        if let Ok(mut body) = self.sea.world.body_mut(id) {
            let _ = body.add_impulse(Vec3::new((rng.f32() - 0.5) * 2.0 * m, 7.5 * m, (rng.f32() - 0.5) * 2.0 * m));
            let _ =
                body.add_angular_impulse(Vec3::new((rng.f32() - 0.5) * 3.0 * m, (rng.f32() - 0.5) * 1.5 * m, (rng.f32() - 0.5) * 3.0 * m));
        }
        // the rest melt away, nearest first
        for (j, t) in self.tiles.iter_mut().enumerate() {
            if j != k && t.body.fade.is_none() {
                t.body.fade = Some(-0.15 - 0.07 * (j as f32 - k as f32).abs());
            }
        }
    }

    /// A tile that only takes a nudge (adjusting a value, a record): it dips and splashes.
    pub fn poke(&mut self, k: usize) {
        let Some(t) = self.tiles.get(k) else { return };
        let Some(p) = position_of(&self.sea.world, t.body.id) else { return };
        self.sea.ripple.splash(p.x, p.z, 0.4);
        let m = t.body.mass;
        let _ = self.sea.world.body_mut(t.body.id).and_then(|mut body| body.add_impulse(Vec3::new(0.0, -1.6 * m, 0.0)));
    }

    /// A click on open water: the boat sets off for it.
    pub fn sail(&mut self, x: f32, z: f32) {
        self.sea.ripple.splash(x, z, 0.35);
        self.sea.boat.sail_free(Vec2::new(x, z));
    }

    pub fn advance(&mut self, dt: f32) {
        for _ in 0..self.sea.due_steps(dt) {
            self.step();
        }
    }

    fn explode_mine(&mut self) {
        let Some(mine) = self.mine.pop() else { return };
        let (x, z) = position_of(&self.sea.world, mine.id).map_or(self.mine_home(), |p| (p.x, p.z));
        let _ = self.sea.world.remove_body(mine.id);
        self.sea.blast(x, z, 2.5);
        self.sea.wreck_boat((x, z), self.seed ^ self.sea.time.to_bits() as u64);
        // the blast shoves the menu tiles about without breaking them
        for t in &self.tiles {
            let Some(p) = position_of(&self.sea.world, t.body.id) else { continue };
            let (dx, dz) = (p.x - x, p.z - z);
            let d = (dx * dx + dz * dz).sqrt();
            if d < 4.0 {
                let m = t.body.mass;
                let k = m * 2.5 * (1.0 - d / 4.0) / d.max(0.3);
                let push = Vec3::new(dx * k, m * 2.0 * (1.0 - d / 4.0), dz * k);
                let _ = self.sea.world.body_mut(t.body.id).and_then(|mut b| b.add_impulse(push));
            }
        }
        self.respawn_at = Some(self.sea.time + RESPAWN);
    }

    fn respawn(&mut self) {
        self.sea.clear_debris();
        // the boat sails back in from beyond the top of the view, the mine bobs up again
        self.sea.boat = Boat::at(BASE + Vec2::new(-1.5, -4.0));
        self.sea.boat.sail_free(BASE);
        self.spawn_mine(-1.6);
        self.respawn_at = None;
    }

    fn step(&mut self) {
        let _ = self.sea.begin_step();

        // the boat runs onto the mine
        if self.sea.boat.alive()
            && let Some(p) = self.mine.first().and_then(|m| position_of(&self.sea.world, m.id))
            && self.sea.boat.pos.distance(Vec2::new(p.x, p.z)) < MINE_REACH
        {
            self.explode_mine();
        }
        if self.respawn_at.is_some_and(|t| self.sea.time >= t) {
            self.respawn();
        }

        // the mine drifts along its lane, held by its anchor's spring
        let home = self.mine_home();
        for mine in &mut self.mine {
            mine.home = home;
        }

        // menu tiles melt without the lace ring a game tile leaves
        let _ = self.sea.melt(&mut self.tiles);
        self.sea.float(&mut self.tiles, ITEM[1] * 0.5);
        self.sea.float(&mut self.mine, MINE_R);
        self.sea.end_step();
    }
}

impl Drop for Lobby {
    fn drop(&mut self) {
        sea::drop_bodies(&mut self.sea.world, &mut self.tiles, |_| true);
        sea::drop_bodies(&mut self.sea.world, &mut self.mine, |_| true);
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::sea::STEP;

    /// Every body in the world is on one of the lobby's lists, each one finite, at most one mine.
    pub fn assert_bodies_tracked(l: &Lobby, what: &str) {
        let tracked = l.tiles.len() + l.mine.len() + l.sea.debris.len();
        assert_eq!(l.sea.world.body_count() as usize, tracked, "{what}: the world has bodies no list tracks");
        assert!(l.mine.len() <= 1, "{what}: {} mines", l.mine.len());
        let bodies = l.tiles.iter().map(|t| &t.body).chain(&l.mine).chain(l.sea.debris.iter().map(|d| &d.body));
        for b in bodies {
            let p = position_of(&l.sea.world, b.id).unwrap_or_else(|| panic!("{what}: a listed body is gone"));
            assert!(p.is_finite(), "{what}: a body at {p}");
        }
    }

    fn run(l: &mut Lobby, seconds: f32, what: &str) {
        for _ in 0..(seconds / STEP) as usize {
            l.step();
            assert_bodies_tracked(l, what);
        }
    }

    fn menu(n: usize) -> Vec<(String, Action)> {
        (0..n).map(|k| (format!("Item {k}"), if k == 0 { Action::Play } else { Action::Back })).collect()
    }

    #[test]
    fn a_menu_surfaces_and_a_pick_blows_its_tile_and_melts_the_rest() {
        let mut l = Lobby::new(1);
        l.set_menu(vec![("Play".into(), Action::Play), ("Records".into(), Action::Records)]);
        run(&mut l, 2.0, "surfacing");
        let (k, _) = {
            let p = position_of(&l.sea.world, l.tiles[0].body.id).unwrap();
            l.item_at(p.x, p.z).expect("the first tile takes a click")
        };
        assert_eq!(k, 0);
        l.blow(0);
        assert!(l.sea.boom.is_some());
        run(&mut l, 1.5, "blown");
        assert!(l.tiles.len() == 1 && l.tiles[0].blown, "only the blown tile is left: the other melted away");
    }

    #[test]
    fn the_boat_blows_up_on_the_mine_and_both_come_back() {
        let mut l = Lobby::new(2);
        run(&mut l, 1.0, "start");
        l.sail(MINE_HOME.0, MINE_HOME.1);
        run(&mut l, 4.0, "to the mine");
        assert!(!l.sea.boat.alive(), "the boat hit the mine");
        assert!(l.mine.is_empty() && !l.sea.debris.is_empty());
        run(&mut l, RESPAWN, "respawn");
        assert!(l.sea.boat.alive() && l.mine.len() == 1 && l.sea.debris.is_empty(), "both are back");
    }

    /// Menus replaced, picks blown, the boat blown up and respawned, many times over: the
    /// session-long world ends every cycle with the same bodies, all of them on a list.
    #[test]
    fn menu_cycles_leave_no_bodies_behind() {
        let mut l = Lobby::new(1);
        let mut counts = Vec::new();
        for cycle in 0..6 {
            l.set_menu(menu(3));
            run(&mut l, 1.5, "main up");
            l.blow(cycle % 3);
            run(&mut l, 1.1, "main blown");
            l.set_menu(menu(5));
            run(&mut l, 1.0, "custom up");
            l.poke(0);
            l.blow(4);
            run(&mut l, 1.1, "custom blown");
            l.set_menu(menu(4));
            l.sail(MINE_HOME.0, MINE_HOME.1);
            run(&mut l, 5.0, "to the mine");
            assert!(!l.sea.boat.alive(), "cycle {cycle}: the boat never reached the mine");
            run(&mut l, RESPAWN + 0.5, "respawn");
            counts.push(l.sea.world.body_count());
        }
        assert!(counts.iter().all(|&c| c == counts[0]), "bodies after each cycle: {counts:?}");
    }
}
