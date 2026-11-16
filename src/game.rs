//! Minesweeper rules on the floating world: tiles bob on the water, an opened tile melts in
//! front of the boat, a blast throws the tiles near it, and the mines rise from the deep when
//! the game ends.

use oxijolt::prelude::math::*;
use oxijolt::prelude::*;

use crate::boat::{self, Boat};
use crate::sea::{self, Body, Floating, MINE_R, Sea, spawn_anchored};

pub const TILE_HALF: [f32; 3] = [0.34, 0.07, 0.34];
/// Seconds between the rings of a flood: the tiles dissolve outward, not all at once.
const RING_DELAY: f32 = 0.04;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Playing,
    Lost,
    Won,
}

#[derive(Clone, Copy, Default)]
pub struct Cell {
    pub mine: bool,
    pub open: bool,
    pub flag: bool,
    pub near: u8,
}

/// A board's size in cells: `w` along x, `h` along z, centred on the origin, one unit a cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Dims {
    pub w: usize,
    pub h: usize,
}

impl Dims {
    pub fn count(self) -> usize {
        self.w * self.h
    }

    /// Half the board's width and depth.
    pub fn half(self) -> (f32, f32) {
        (self.w as f32 * 0.5, self.h as f32 * 0.5)
    }

    pub fn center(self, i: usize) -> (f32, f32) {
        let (hw, hh) = self.half();
        ((i % self.w) as f32 + 0.5 - hw, (i / self.w) as f32 + 0.5 - hh)
    }

    pub fn cell_at(self, x: f32, z: f32) -> Option<usize> {
        let (hw, hh) = self.half();
        let (i, j) = ((x + hw).floor(), (z + hh).floor());
        (0.0..self.w as f32).contains(&i).then_some(())?;
        (0.0..self.h as f32).contains(&j).then_some(())?;
        Some(i as usize + j as usize * self.w)
    }

    pub fn neighbours(self, i: usize) -> impl Iterator<Item = usize> {
        let (w, h) = (self.w as isize, self.h as isize);
        let (x, y) = ((i % self.w) as isize, (i / self.w) as isize);
        (-1..=1)
            .flat_map(move |dy| (-1..=1).map(move |dx| (x + dx, y + dy)))
            .filter(move |&(nx, ny)| (nx, ny) != (x, y) && (0..w).contains(&nx) && (0..h).contains(&ny))
            .map(move |(nx, ny)| (nx + ny * w) as usize)
    }
}

/// A floating tile over a board cell.
pub struct Tile {
    pub body: Body,
    pub cell: usize,
}

impl Floating for Tile {
    fn body(&self) -> &Body {
        &self.body
    }
    fn body_mut(&mut self) -> &mut Body {
        &mut self.body
    }
}

pub struct Game {
    pub dims: Dims,
    /// Mines on this board.
    pub mine_total: usize,
    pub cells: Vec<Cell>,
    pub status: Status,
    /// Mines are laid out when the boat reaches its first cell, around that cell.
    placed: bool,
    /// The cell whose mine went off.
    pub boom: Option<usize>,
    pub sea: Sea,
    pub tiles: Vec<Tile>,
    pub mines: Vec<Body>,
    mine_shape: Shape,
    seed: u64,
    /// A mine cell whose tile has dissolved: the boat sails to its centre, then it goes off.
    doom: Option<usize>,
    /// Water-clock time at which each open cell's number cloud appears (as its tile melts).
    pub cloud_at: Vec<f32>,
}

impl Game {
    /// A `dims` board with `mines` mines. Tiles start a little under water and bob up.
    pub fn with(dims: Dims, mines: usize, seed: u64) -> Game {
        let mines = mines.min(dims.count().saturating_sub(9));
        let mut sea = Sea::new(dims.w.max(dims.h) as f32 * 0.5 + 3.0, Boat::new(dims));
        let tile_shape = Shape::new_box(Vec3::new(TILE_HALF[0], TILE_HALF[1], TILE_HALF[2])).expect("tile shape");
        let tiles = (0..dims.count())
            .map(|cell| {
                let (x, z) = dims.center(cell);
                let y = -0.55 - (cell % 7) as f32 * 0.04;
                let settings = BodySettings::new_dynamic().friction(0.4).restitution(0.1);
                let body = spawn_anchored(&mut sea.world, &tile_shape, settings, [x, y, z], sea::TILE_BUOYANCY, 4.0).expect("tile body");
                Tile { body, cell }
            })
            .collect();
        Game {
            dims,
            mine_total: mines,
            cells: vec![Cell::default(); dims.count()],
            status: Status::Playing,
            placed: false,
            boom: None,
            sea,
            tiles,
            mines: Vec::new(),
            mine_shape: Shape::new_sphere(MINE_R).expect("mine shape"),
            seed,
            doom: None,
            cloud_at: vec![f32::INFINITY; dims.count()],
        }
    }

    /// Lays the mines out anywhere but `safe` and its neighbours.
    fn place(&mut self, safe: usize) {
        let mut rng = fastrand::Rng::with_seed(self.seed);
        let dims = self.dims;
        let keep: Vec<usize> = std::iter::once(safe).chain(dims.neighbours(safe)).collect();
        let mut left = self.mine_total;
        while left > 0 {
            let i = rng.usize(0..dims.count());
            if !self.cells[i].mine && !keep.contains(&i) {
                self.cells[i].mine = true;
                left -= 1;
            }
        }
        for i in 0..dims.count() {
            self.cells[i].near = dims.neighbours(i).filter(|&n| self.cells[n].mine).count() as u8;
        }
        self.placed = true;
    }

    /// A left click on the water over cell `i`: a small splash, and the boat sets off for it.
    /// A second click before it gets there re-routes it.
    pub fn click(&mut self, i: usize) {
        let (x, z) = self.dims.center(i);
        self.sea.ripple.splash(x, z, 0.5);
        if self.status != Status::Playing || self.cells[i].flag || self.cells[i].open || self.doom.is_some() {
            return;
        }
        self.sea.boat.sail_to(i, self.dims);
    }

    /// The boat's bow is at cell `i`: the tile melts in front of it. A safe cell opens (and
    /// floods); over a mine only the tile goes, and the mine waits for the boat in the middle.
    fn door(&mut self, i: usize) {
        if self.status != Status::Playing || self.cells[i].flag || self.cells[i].open {
            return;
        }
        // the first cell the boat reaches is the one kept clear, whichever was clicked first
        if !self.placed {
            self.place(i);
        }
        if self.cells[i].mine {
            self.dissolve(i, 0.0);
            self.doom = Some(i);
        } else {
            self.open(i);
        }
    }

    /// The boat stopped in the middle of cell `i`; on a mine, that is the end of it.
    fn arrive(&mut self, i: usize) {
        if Some(i) == self.doom {
            self.doom = None;
            self.open(i);
        }
    }

    /// Opens cell `i` at once (flood fill through zeros); a mine loses the game.
    pub fn open(&mut self, i: usize) {
        if self.status != Status::Playing || self.cells[i].flag || (self.cells[i].open && !self.cells[i].mine) {
            return;
        }
        if !self.placed {
            self.place(i);
        }
        if self.cells[i].mine {
            self.cells[i].open = true;
            self.status = Status::Lost;
            self.boom = Some(i);
            self.explode(i);
            self.sea.wreck_boat(self.dims.center(i), self.seed);
            // tiles the blast didn't reach still cover mines: toss them so every mine shows
            for c in 0..self.dims.count() {
                if self.cells[c].mine {
                    self.launch(c, 0.5);
                }
            }
            self.raise_mines();
            return;
        }
        // breadth first, so the flood dissolves outward in rings (a stagger, not all at once)
        let mut queue = std::collections::VecDeque::from([(i, 0u32)]);
        while let Some((c, ring)) = queue.pop_front() {
            if self.cells[c].open || self.cells[c].flag {
                continue;
            }
            self.cells[c].open = true;
            self.dissolve(c, ring as f32 * RING_DELAY);
            self.cloud_at[c] = self.sea.time + ring as f32 * RING_DELAY + sea::FADE * 0.55;
            if self.cells[c].near == 0 {
                queue.extend(self.dims.neighbours(c).filter(|&n| !self.cells[n].open && !self.cells[n].mine).map(|n| (n, ring + 1)));
            }
        }
        if self.cells.iter().filter(|c| c.open).count() == self.dims.count() - self.mine_total {
            self.status = Status::Won;
            for c in self.cells.iter_mut().filter(|c| c.mine) {
                c.flag = true;
            }
            self.raise_mines();
        }
    }

    /// A right click on cell `i`: flags it, or takes the flag off. A cell whose tile has
    /// already melted over a mine (the boat is on its way in) can no longer be flagged.
    pub fn toggle_flag(&mut self, i: usize) {
        let (x, z) = self.dims.center(i);
        self.sea.ripple.splash(x, z, 0.35);
        if self.status == Status::Playing && !self.cells[i].open && self.doom != Some(i) {
            self.cells[i].flag = !self.cells[i].flag;
            if let Some(t) = self.tiles.iter().find(|t| t.cell == i) {
                let m = t.body.mass;
                let _ = self.sea.world.body_mut(t.body.id).and_then(|mut b| b.add_impulse(Vec3::new(0.0, -1.2 * m, 0.0)));
            }
        }
    }

    /// The tile over `cell` melts into the water like a film after `delay` seconds.
    fn dissolve(&mut self, cell: usize, delay: f32) {
        if let Some(t) = self.tiles.iter_mut().find(|t| t.cell == cell && t.body.anchored && t.body.fade.is_none()) {
            t.body.fade = Some(-delay);
        }
    }

    /// The tile over `cell` jumps off the water, spinning, and from then on sinks.
    fn launch(&mut self, cell: usize, power: f32) {
        let mut rng = fastrand::Rng::with_seed(self.seed ^ (cell as u64 * 7919));
        if let Some(t) = self.tiles.iter_mut().find(|t| t.cell == cell && t.body.anchored && t.body.fade.is_none()) {
            let t = &mut t.body;
            t.anchored = false;
            t.buoyancy = sea::THROWN_BUOYANCY;
            t.splash = true;
            let m = t.mass;
            if let Ok(mut b) = self.sea.world.body_mut(t.id) {
                let _ =
                    b.add_impulse(Vec3::new((rng.f32() - 0.5) * 0.4 * m, (2.4 + rng.f32() * 0.8) * power * m, (rng.f32() - 0.5) * 0.4 * m));
                let _ =
                    b.add_angular_impulse(Vec3::new((rng.f32() - 0.5) * 0.9 * m, (rng.f32() - 0.5) * 0.4 * m, (rng.f32() - 0.5) * 0.9 * m));
            }
        }
    }

    /// The mine under `cell` goes off: the tiles near it are thrown outward.
    fn explode(&mut self, cell: usize) {
        let (bx, bz) = self.dims.center(cell);
        self.sea.blast(bx, bz, 2.5);
        let ids: Vec<(usize, f32)> = self
            .tiles
            .iter()
            .filter(|t| t.body.anchored)
            .map(|t| (t.cell, ((self.dims.center(t.cell).0 - bx).powi(2) + (self.dims.center(t.cell).1 - bz).powi(2)).sqrt()))
            .filter(|&(_, d)| d < 3.6)
            .collect();
        for (c, d) in ids {
            self.launch(c, 1.0 + 2.2 * (1.0 - d / 3.6));
            if let Some(t) = self.tiles.iter().find(|t| t.cell == c) {
                let (tx, tz) = self.dims.center(c);
                let (dx, dz) = ((tx - bx) / d.max(0.3), (tz - bz) / d.max(0.3));
                let m = t.body.mass * 3.5 * (1.0 - d / 3.6);
                let _ = self.sea.world.body_mut(t.body.id).and_then(|mut b| b.add_impulse(Vec3::new(dx * m, 0.0, dz * m)));
            }
        }
    }

    fn raise_mines(&mut self) {
        for cell in (0..self.dims.count()).filter(|&c| self.cells[c].mine) {
            let (x, z) = self.dims.center(cell);
            let depth = if Some(cell) == self.boom { -0.4 } else { -1.4 - (cell % 3) as f32 * 0.25 };
            let settings = BodySettings::new_dynamic();
            let mine =
                spawn_anchored(&mut self.sea.world, &self.mine_shape, settings, [x, depth, z], sea::MINE_BUOYANCY, 4.0).expect("mine body");
            if Some(cell) == self.boom {
                let _ = self.sea.world.body_mut(mine.id).and_then(|mut b| b.add_impulse(Vec3::new(0.0, 9.0 * mine.mass, 0.0)));
            }
            self.mines.push(mine);
        }
    }

    /// Advances water and physics to `dt` seconds later in fixed steps.
    pub fn advance(&mut self, dt: f32) {
        for _ in 0..self.sea.due_steps(dt) {
            self.step();
        }
    }

    fn step(&mut self) {
        match self.sea.begin_step() {
            Some(boat::Event::Door(cell)) => self.door(cell),
            Some(boat::Event::Arrived(cell)) => self.arrive(cell),
            None => {}
        }
        for (x, z) in self.sea.melt(&mut self.tiles) {
            // the film breaks: a ring of lace foam where it was
            self.sea.ripple.push(x, z, 0.5, -0.01, 0.9);
        }
        self.sea.float(&mut self.tiles, TILE_HALF[1]);
        self.sea.float(&mut self.mines, MINE_R);
        self.sea.end_step();
    }
}

impl Drop for Game {
    fn drop(&mut self) {
        sea::drop_bodies(&mut self.sea.world, &mut self.tiles, |_| true);
        sea::drop_bodies(&mut self.sea.world, &mut self.mines, |_| true);
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::sea::STEP;
    use crate::water;

    /// The classic small board most tests use.
    pub const TEST_DIMS: Dims = Dims { w: 10, h: 10 };
    const TEST_MINES: usize = 14;
    /// Expert's shape: wider than deep, so a swapped width and height shows.
    const WIDE: Dims = Dims { w: 30, h: 16 };

    fn new_game(seed: u64) -> Game {
        Game::with(TEST_DIMS, TEST_MINES, seed)
    }

    /// Every body in the world is on one of the game's lists, and each one is finite.
    pub fn assert_bodies_tracked(g: &Game, what: &str) {
        let tracked = g.tiles.len() + g.mines.len() + g.sea.debris.len();
        assert_eq!(g.sea.world.body_count() as usize, tracked, "{what}: the world has bodies no list tracks");
        let bodies = g.tiles.iter().map(|t| &t.body).chain(&g.mines).chain(g.sea.debris.iter().map(|d| &d.body));
        for b in bodies {
            let p = sea::position_of(&g.sea.world, b.id).unwrap_or_else(|| panic!("{what}: a listed body is gone"));
            assert!(p.is_finite(), "{what}: a body at {p}");
        }
    }

    /// Over the anchored tiles: furthest from its cell centre, furthest off the water surface,
    /// and the smallest cosine between a tile's up axis and world up.
    fn anchored_stats(g: &Game) -> (f32, f32, f32) {
        let (mut dxz, mut dy, mut up) = (0.0f32, 0.0f32, 1.0f32);
        for t in g.tiles.iter().filter(|t| t.body.anchored) {
            let b = g.sea.world.body(t.body.id).expect("tile body");
            let (p, q) = (sea::xyz(b.position()), b.rotation());
            assert!(p.is_finite(), "tile {} at a non-finite position", t.cell);
            let (cx, cz) = g.dims.center(t.cell);
            let h = water::surface(p.x, p.z, g.sea.swell).0 + g.sea.ripple.height(p.x, p.z);
            dxz = dxz.max(((p.x - cx).powi(2) + (p.z - cz).powi(2)).sqrt());
            dy = dy.max((p.y - h).abs());
            up = up.min(1.0 - 2.0 * (q.x * q.x + q.z * q.z));
        }
        (dxz, dy, up)
    }

    fn ripple_range(g: &Game) -> (f32, f32) {
        let mut t = Vec::new();
        g.sea.ripple.texels(&mut t);
        let h = t.iter().step_by(2);
        assert!(h.clone().all(|v| v.is_finite()), "ripple height went non-finite");
        h.fold((0.0f32, 0.0f32), |(lo, hi), &v| (lo.min(v), hi.max(v)))
    }

    fn reference_open(g: &Game, first: usize) -> Vec<bool> {
        let mut open = vec![false; g.dims.count()];
        let mut stack = vec![first];
        while let Some(c) = stack.pop() {
            if open[c] || g.cells[c].mine {
                continue;
            }
            open[c] = true;
            if g.cells[c].near == 0 {
                stack.extend(g.dims.neighbours(c));
            }
        }
        open
    }

    /// Steps until the boat has stopped (and a few steps more), at most `limit` steps.
    fn run_until_moored(g: &mut Game, limit: usize) {
        let mut n = 0;
        while (matches!(g.sea.boat.phase, boat::Phase::WindUp | boat::Phase::Sailing) || n < 5) && n < limit {
            g.step();
            n += 1;
        }
    }

    #[test]
    fn cells_map_to_their_centres_on_a_wide_board() {
        for dims in [WIDE, Dims { w: 7, h: 5 }, Dims { w: 5, h: 9 }] {
            let (hw, hh) = dims.half();
            for i in 0..dims.count() {
                let (x, z) = dims.center(i);
                assert!(x.abs() < hw && z.abs() < hh, "{dims:?} cell {i} centred off the board at ({x}, {z})");
                assert_eq!(dims.cell_at(x, z), Some(i), "{dims:?} cell {i}");
                let want: Vec<usize> = (0..dims.count())
                    .filter(|&n| n != i && (n % dims.w).abs_diff(i % dims.w) <= 1 && (n / dims.w).abs_diff(i / dims.w) <= 1)
                    .collect();
                let mut got: Vec<usize> = dims.neighbours(i).collect();
                got.sort();
                assert_eq!(got, want, "{dims:?} neighbours of {i}");
            }
            assert_eq!(dims.cell_at(hw + 0.1, 0.0), None);
            assert_eq!(dims.cell_at(0.0, hh + 0.1), None);
        }
    }

    #[test]
    fn first_click_is_safe_and_floods_like_a_bfs() {
        for (dims, mines) in [(TEST_DIMS, TEST_MINES), (Dims { w: 7, h: 5 }, 8)] {
            for seed in [0x5eedu64, 1, 2, 0xdead_beef] {
                for first in 0..dims.count() {
                    let mut g = Game::with(dims, mines, seed);
                    g.open(first);
                    let at = format!("{dims:?} seed {seed:x} first {first}");
                    assert_eq!(g.cells.iter().filter(|c| c.mine).count(), g.mine_total, "{at}");
                    assert!(!g.cells[first].mine && g.dims.neighbours(first).all(|n| !g.cells[n].mine), "{at}");
                    for c in 0..g.dims.count() {
                        assert_eq!(g.cells[c].near as usize, g.dims.neighbours(c).filter(|&n| g.cells[n].mine).count());
                    }
                    let want = reference_open(&g, first);
                    assert!((0..g.dims.count()).all(|c| g.cells[c].open == want[c]), "flood fill, {at}");
                    assert_ne!(g.status, Status::Lost);
                }
            }
        }
    }

    /// Click a far cell, change your mind before the boat is there and click a near one: the
    /// cell the boat opens first is the one kept clear of mines.
    #[test]
    fn the_first_cell_the_boat_opens_is_safe_after_a_change_of_mind() {
        for (dims, mines, games) in [(Dims { w: 9, h: 9 }, 10, 40), (WIDE, 99, 6)] {
            for seed in 0..games {
                let mut g = Game::with(dims, mines, seed);
                for _ in 0..30 {
                    g.step();
                }
                g.click(dims.count() - 1);
                for _ in 0..20 {
                    g.step();
                }
                let near = (seed as usize * 7) % (dims.count() / 2);
                g.click(near);
                run_until_moored(&mut g, 1500);
                let at = format!("{dims:?} seed {seed}");
                assert_ne!(g.status, Status::Lost, "{at}: the first cell opened was a mine");
                assert!(g.cells[near].open, "{at}: the boat did not open the cell it was sent to");
                assert!(dims.neighbours(near).all(|n| !g.cells[n].mine), "{at}: a mine next to the first cell");
                assert_eq!(g.cells.iter().filter(|c| c.mine).count(), g.mine_total, "{at}");
            }
        }
    }

    #[test]
    fn a_boat_sent_onto_a_mine_loses_the_game_in_the_middle_of_the_cell() {
        let mut g = new_game(7);
        g.open(44);
        let m = (0..g.dims.count()).find(|&c| g.cells[c].mine).expect("a mine");
        g.click(m);
        run_until_moored(&mut g, 1200);
        assert_eq!(g.status, Status::Lost);
        assert_eq!(g.boom, Some(m));
        assert!(!g.sea.boat.alive() && !g.sea.debris.is_empty(), "the boat went up with the mine");
        let (x, z) = g.dims.center(m);
        let (bx, bz) = g.sea.boom.expect("a blast");
        assert!((bx - x).abs() < 1e-6 && (bz - z).abs() < 1e-6);
    }

    /// The tile over a mine melts in front of the boat; a right click before the boat is in
    /// must not defuse the mine.
    #[test]
    fn a_flag_after_the_tile_melted_does_not_stop_the_mine() {
        let mut g = new_game(7);
        g.open(44);
        let m = (0..g.dims.count()).find(|&c| g.cells[c].mine).expect("a mine");
        g.click(m);
        let mut n = 0;
        while g.doom.is_none() && n < 1200 {
            g.step();
            n += 1;
        }
        assert_eq!(g.doom, Some(m), "the bow never reached the mine's cell");
        g.toggle_flag(m);
        assert!(!g.cells[m].flag, "the doomed cell took a flag");
        run_until_moored(&mut g, 600);
        assert_eq!(g.status, Status::Lost);
    }

    #[test]
    fn win_on_the_last_safe_cell_and_nothing_after() {
        let mut g = new_game(7);
        g.open(44);
        let safe: Vec<usize> = (0..g.dims.count()).filter(|&c| !g.cells[c].mine).collect();
        for &c in &safe {
            g.open(c);
            let all = g.cells.iter().filter(|c| c.open).count() == g.dims.count() - g.mine_total;
            assert_eq!(g.status == Status::Won, all);
        }
        assert_eq!(g.status, Status::Won);
        assert_eq!(g.cells.iter().filter(|c| c.flag).count(), g.mine_total);
        assert_eq!(g.mines.len(), g.mine_total);
        let m = (0..g.dims.count()).find(|&c| g.cells[c].mine).expect("a mine");
        g.open(m);
        assert_eq!(g.status, Status::Won);
    }

    #[test]
    fn a_flagged_mine_holds_and_an_open_one_loses() {
        let mut g = new_game(7);
        g.open(44);
        let m = (0..g.dims.count()).find(|&c| g.cells[c].mine).expect("a mine");
        g.toggle_flag(m);
        g.open(m);
        assert_eq!(g.status, Status::Playing);
        g.toggle_flag(m);
        g.open(m);
        assert_eq!(g.status, Status::Lost);
        assert_eq!(g.mines.len(), g.mine_total);
        // every mine's tile is thrown off so the mines can be seen
        assert!(g.tiles.iter().filter(|t| g.cells[t.cell].mine).all(|t| !t.body.anchored));
        let opened = g.cells.iter().filter(|c| c.open).count();
        let other = (0..g.dims.count()).find(|&c| !g.cells[c].open && !g.cells[c].mine).expect("a closed cell");
        g.open(other);
        g.toggle_flag(other);
        assert_eq!(g.cells.iter().filter(|c| c.open).count(), opened);
        assert!(!g.cells[other].flag);
    }

    #[test]
    fn after_a_click_the_water_calms_down() {
        let mut g = new_game(0x5eed);
        for _ in 0..60 {
            g.step();
        }
        g.open(44);
        let mut peak = 0usize;
        for _ in 0..480 {
            g.step();
            peak = peak.max(g.sea.ripple.foam.iter().filter(|&&f| f > 0.12).count());
        }
        let foamy = g.sea.ripple.foam.iter().filter(|&&f| f > 0.12).count();
        let (lo, hi) = ripple_range(&g);
        assert!(peak > 50, "the click left no visible foam ({peak} texels)");
        assert!(foamy < peak / 10, "foam still on {foamy} texels of a peak {peak}");
        assert!(lo > -0.01 && hi < 0.01, "water still moving: {lo}..{hi}");
    }

    #[test]
    fn idle_tiles_stay_on_their_cells() {
        for dims in [TEST_DIMS, WIDE] {
            let mut g = Game::with(dims, 10, 0x5eed);
            for _ in 0..1200 {
                g.step();
            }
            let (dxz, dy, up) = anchored_stats(&g);
            assert!(dxz < 0.05 && dy < 0.1 && up > 0.95, "{dims:?} idle: off cell {dxz}, off surface {dy}, up {up}");
        }
    }

    /// How much the board sways on a sea at `level`, once settled, over ten seconds: the
    /// steepest tilt of any tile (degrees), the most any tile heaves (metres, top to bottom), and
    /// the furthest a tile is off its water (metres).
    fn sway(level: crate::water::SeaLevel) -> (f32, f32, f32) {
        let mut g = Game::with(TEST_DIMS, TEST_MINES, 0x5eed);
        g.sea.settle_at(level);
        for _ in 0..300 {
            g.step();
        }
        let mut range = vec![(f32::MAX, f32::MIN); g.tiles.len()];
        let (mut tilt, mut off) = (0.0f32, 0.0f32);
        for _ in 0..600 {
            g.step();
            let (_, dy, up) = anchored_stats(&g);
            tilt = tilt.max(up.clamp(-1.0, 1.0).acos().to_degrees());
            off = off.max(dy);
            for (r, t) in range.iter_mut().zip(&g.tiles) {
                let y = sea::position_of(&g.sea.world, t.body.id).expect("tile body").y;
                *r = (r.0.min(y), r.1.max(y));
            }
        }
        (tilt, range.iter().map(|(lo, hi)| hi - lo).fold(0.0, f32::max), off)
    }

    /// A calm sea steadies the board: the tiles tilt and heave well under half as much as on the
    /// lively one; a rough sea sways them more, and they still ride their water upright.
    #[test]
    fn a_calm_sea_steadies_the_board() {
        use crate::water::SeaLevel;
        let [calm, lively, rough] = SeaLevel::ALL.map(sway);
        for (level, (tilt, heave, off)) in SeaLevel::ALL.iter().zip([calm, lively, rough]) {
            println!("{level:?}: tilt {tilt:.2} deg, heave {heave:.3} m, off the water {off:.3} m");
        }
        assert!(calm.0 < lively.0 * 0.5 && calm.1 < lively.1 * 0.5, "calm {calm:?} against lively {lively:?}");
        assert!(rough.0 > lively.0 && rough.1 > lively.1, "rough {rough:?} against lively {lively:?}");
        assert!(rough.0 < 20.0 && rough.2 < 0.1, "rough {rough:?}");
    }

    /// The cursor sweeping a row at a mouse speed, then resting: the water may ripple and foam
    /// but must not be dug out, and the tiles must stay afloat and upright.
    #[test]
    fn a_cursor_sweep_leaves_foam_not_a_trench() {
        for speed in [0.2f32, 1.0, 3.0, 6.0, 14.0] {
            let mut g = new_game(0x5eed);
            for _ in 0..120 {
                g.step();
            }
            let (mut lo, mut hi, mut dy, mut up) = (0.0f32, 0.0f32, 0.0f32, 1.0f32);
            let (mut x, mut dir, mut foamy) = (-4.0f32, 1.0f32, 0usize);
            for _ in 0..240 {
                x += dir * speed / 60.0;
                if x.abs() > 4.0 {
                    dir = -dir;
                }
                g.sea.cursor = Some((x, 0.5));
                g.step();
                let (a, b) = ripple_range(&g);
                (lo, hi) = (lo.min(a), hi.max(b));
                let st = anchored_stats(&g);
                (dy, up) = (dy.max(st.1), up.min(st.2));
                foamy = foamy.max(g.sea.ripple.foam.iter().filter(|&&f| f > 0.3).count());
            }
            println!("wake {speed:4.1} u/s: ripple {lo:.4}..{hi:.4}, tiles off surface {dy:.3}, min up {up:.3}, foamy texels {foamy}");
            assert!(lo > -0.06 && hi < 0.06, "wake at {speed} u/s: ripple {lo}..{hi}");
            assert!(dy < 0.1 && up > 0.93, "wake at {speed} u/s: tiles off surface {dy}, up {up}");
            assert!(foamy > 10, "wake at {speed} u/s left no foam trail");
        }
    }

    #[test]
    fn frames_between_steps_draw_between_poses() {
        // opened tiles dissolve in place now; a blast still throws tiles
        let mut g = Game::with(TEST_DIMS, TEST_MINES, 1);
        g.open(44);
        let mine = (0..g.dims.count()).find(|&i| g.cells[i].mine).expect("a mine");
        g.open(mine);
        for _ in 0..5 {
            g.step();
        }
        g.advance(STEP * 0.5);
        let t = g.tiles.iter().find(|t| !t.body.anchored).expect("a launched tile");
        let now = sea::position_of(&g.sea.world, t.body.id).expect("tile body");
        let was = glam::Vec3::from_array(t.body.prev.0);
        let (drawn, _) = g.sea.draw_pose(&t.body).expect("pose");
        assert!(now.distance(was) > 0.01, "the tile isn't moving");
        assert!(drawn.distance((was + now) * 0.5) < 1e-4, "drawn at {drawn}, between {was} and {now}");
        assert!((g.sea.draw_lag() - STEP * 0.5).abs() < 1e-6);
    }

    /// A game played to a loss and left running: every body stays on a list (none leaks out of
    /// the bookkeeping as tiles melt, are thrown and sink), and everything stays finite.
    #[test]
    fn a_lost_game_keeps_its_bodies_tracked_and_finite() {
        for dims in [TEST_DIMS, WIDE] {
            let mut g = Game::with(dims, 20, 0x5eed);
            let middle = dims.w * (dims.h / 2) + dims.w / 2;
            g.click(middle);
            run_until_moored(&mut g, 1200);
            for _ in 0..60 {
                g.step();
                assert_bodies_tracked(&g, "sailing and melting");
            }
            assert!(g.tiles.len() < dims.count(), "{dims:?}: no tile melted away");
            let m = (0..dims.count()).find(|&c| g.cells[c].mine && !g.cells[c].flag).expect("a mine");
            g.click(m);
            for _ in 0..1800 {
                g.step();
                assert_bodies_tracked(&g, "after the blast");
            }
            assert_eq!(g.status, Status::Lost, "{dims:?}");
            assert!(g.tiles.iter().all(|t| t.body.anchored), "{dims:?}: thrown tiles never sank out of the lists");
            anchored_stats(&g);
            ripple_range(&g);
        }
    }
}
