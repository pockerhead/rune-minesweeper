//! The minesweeper rules on their own: where the mines go (never next to the first cell
//! opened), the flood through zeros, flags, and when a game is won or lost. The game floats
//! this board on the sea (`game.rs`); the leaderboard's verifier replays runs on it.

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

pub struct Board {
    pub dims: Dims,
    /// Mines on this board.
    pub mine_total: usize,
    pub cells: Vec<Cell>,
    pub status: Status,
    /// The cell whose mine went off.
    pub boom: Option<usize>,
    /// Mines are laid out when the first cell opens, around that cell.
    placed: bool,
    pub seed: u64,
}

impl Board {
    /// A `dims` board with `mines` mines (at most all but nine cells, so a first cell and its
    /// neighbours always fit), laid out from `seed` once the first cell opens.
    pub fn new(dims: Dims, mines: usize, seed: u64) -> Board {
        Board {
            dims,
            mine_total: mines.min(dims.count().saturating_sub(9)),
            cells: vec![Cell::default(); dims.count()],
            status: Status::Playing,
            boom: None,
            placed: false,
            seed,
        }
    }

    pub fn placed(&self) -> bool {
        self.placed
    }

    /// Lays the mines out anywhere but `safe` and its neighbours.
    pub fn place(&mut self, safe: usize) {
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

    /// Opens cell `i`, laying the mines out first if none are yet. A mine loses the game;
    /// a safe cell opens and floods through zeros, breadth first. Returns the cells it opened
    /// with their ring of the flood (0 the cell itself), in the order they opened; empty when
    /// the cell could not be opened or was a mine.
    pub fn open(&mut self, i: usize) -> Vec<(usize, u32)> {
        let mut opened = Vec::new();
        if self.status != Status::Playing || self.cells[i].flag || (self.cells[i].open && !self.cells[i].mine) {
            return opened;
        }
        if !self.placed {
            self.place(i);
        }
        if self.cells[i].mine {
            self.cells[i].open = true;
            self.status = Status::Lost;
            self.boom = Some(i);
            return opened;
        }
        let mut queue = std::collections::VecDeque::from([(i, 0u32)]);
        while let Some((c, ring)) = queue.pop_front() {
            if self.cells[c].open || self.cells[c].flag {
                continue;
            }
            self.cells[c].open = true;
            opened.push((c, ring));
            if self.cells[c].near == 0 {
                queue.extend(self.dims.neighbours(c).filter(|&n| !self.cells[n].open && !self.cells[n].mine).map(|n| (n, ring + 1)));
            }
        }
        if self.cells.iter().filter(|c| c.open).count() == self.dims.count() - self.mine_total {
            self.status = Status::Won;
            for c in self.cells.iter_mut().filter(|c| c.mine) {
                c.flag = true;
            }
        }
        opened
    }

    /// Flags cell `i` or takes its flag off; true when it did (not on an open cell or after
    /// the game).
    pub fn toggle_flag(&mut self, i: usize) -> bool {
        if self.status != Status::Playing || self.cells[i].open {
            return false;
        }
        self.cells[i].flag = !self.cells[i].flag;
        true
    }

    /// A short hash of where the mines are (FNV-1a over the mine cells): a run code carries it
    /// so a replay can tell it got the same board. 0 before the mines are laid.
    pub fn fingerprint(&self) -> u32 {
        if !self.placed {
            return 0;
        }
        let mines = (0..self.dims.count()).filter(|&i| self.cells[i].mine);
        mines.fold(0x811c_9dc5u32, |h, i| (h ^ i as u32).wrapping_mul(0x0100_0193))
    }

    /// The board's 3BV: the fewest opens that clear it. Each region of zeros is one open (it
    /// floods to the numbers round it), every other safe cell one more. 0 before the mines
    /// are laid.
    pub fn three_bv(&self) -> usize {
        if !self.placed {
            return 0;
        }
        let dims = self.dims;
        let mut reached = vec![false; dims.count()];
        let mut opens = 0;
        for i in 0..dims.count() {
            if self.cells[i].mine || self.cells[i].near != 0 || reached[i] {
                continue;
            }
            opens += 1;
            reached[i] = true;
            let mut stack = vec![i];
            while let Some(c) = stack.pop() {
                if self.cells[c].near != 0 {
                    continue;
                }
                for n in dims.neighbours(c) {
                    if !reached[n] {
                        reached[n] = true;
                        stack.push(n);
                    }
                }
            }
        }
        opens + (0..dims.count()).filter(|&i| !self.cells[i].mine && !reached[i]).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_open(b: &Board, first: usize) -> Vec<bool> {
        let mut open = vec![false; b.dims.count()];
        let mut stack = vec![first];
        while let Some(c) = stack.pop() {
            if open[c] || b.cells[c].mine {
                continue;
            }
            open[c] = true;
            if b.cells[c].near == 0 {
                stack.extend(b.dims.neighbours(c));
            }
        }
        open
    }

    #[test]
    fn the_first_cell_is_safe_and_floods_like_a_bfs() {
        for (dims, mines) in [(Dims { w: 9, h: 9 }, 10), (Dims { w: 30, h: 16 }, 99), (Dims { w: 7, h: 5 }, 8)] {
            for seed in [0x5eedu64, 1, 2, 0xdead_beef] {
                for first in (0..dims.count()).step_by(7) {
                    let mut b = Board::new(dims, mines, seed);
                    let opened = b.open(first);
                    assert_eq!(b.cells.iter().filter(|c| c.mine).count(), b.mine_total);
                    assert!(!b.cells[first].mine && dims.neighbours(first).all(|n| !b.cells[n].mine));
                    let want = reference_open(&b, first);
                    assert!((0..dims.count()).all(|c| b.cells[c].open == want[c]));
                    assert_eq!(opened.len(), want.iter().filter(|&&o| o).count());
                    assert_eq!(opened[0], (first, 0));
                    assert!(opened.windows(2).all(|w| w[0].1 <= w[1].1), "rings in order");
                }
            }
        }
    }

    #[test]
    fn the_same_seed_and_first_cell_lay_the_same_board() {
        let dims = Dims { w: 30, h: 16 };
        let mut a = Board::new(dims, 99, 42);
        let mut b = Board::new(dims, 99, 42);
        let mut c = Board::new(dims, 99, 43);
        assert_eq!(a.fingerprint(), 0);
        a.open(100);
        b.open(100);
        c.open(100);
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert_ne!(a.fingerprint(), c.fingerprint());
        let mut d = Board::new(dims, 99, 42);
        d.open(479);
        assert_ne!(a.fingerprint(), d.fingerprint(), "the first cell shapes the board too");
    }

    /// Boards as the game laid them out before the rules moved here (v0.3.0, `Game::open`):
    /// the mines' fingerprint and the cells the first open floods. A run code from that game
    /// replays on the same board.
    const GOLDEN: [(Dims, usize, u64, usize, u32, usize); 4] = [
        (Dims { w: 9, h: 9 }, 10, 0x5eed, 40, 0x1df1_a882, 53),
        (Dims { w: 16, h: 16 }, 40, 7, 0, 0x109d_c7d0, 108),
        (Dims { w: 30, h: 16 }, 99, 0xdead_beef, 240, 0xb3ad_4752, 9),
        (Dims { w: 30, h: 16 }, 99, 1, 479, 0x565c_3565, 12),
    ];

    #[test]
    fn boards_are_laid_as_before() {
        for (dims, mines, seed, first, fingerprint, opened) in GOLDEN {
            let mut b = Board::new(dims, mines, seed);
            assert_eq!(b.open(first).len(), opened, "{dims:?} seed {seed:#x}");
            assert_eq!(b.fingerprint(), fingerprint, "{dims:?} seed {seed:#x}");
        }
    }

    #[test]
    fn flags_hold_off_opens_and_floods() {
        let mut b = Board::new(Dims { w: 9, h: 9 }, 10, 7);
        assert!(b.toggle_flag(0));
        assert!(b.open(0).is_empty(), "a flagged cell does not open");
        assert!(b.toggle_flag(0));
        assert!(!b.open(40).is_empty());
        assert!(!b.toggle_flag(40), "an open cell takes no flag");
    }

    /// 3BV is what a player who knows the board needs: one open in each region of zeros,
    /// then one in every safe cell still closed; that wins in exactly `three_bv` opens.
    #[test]
    fn three_bv_is_the_fewest_opens() {
        assert_eq!(Board::new(Dims { w: 9, h: 9 }, 10, 1).three_bv(), 0, "not laid yet");
        for (dims, mines) in [(Dims { w: 9, h: 9 }, 10), (Dims { w: 16, h: 16 }, 40), (Dims { w: 30, h: 16 }, 99)] {
            for seed in 0..40u64 {
                let mut b = Board::new(dims, mines, seed);
                b.open(seed as usize % dims.count());
                let bv = b.three_bv();
                let mut opens = 1;
                for zeros in [true, false] {
                    for c in 0..dims.count() {
                        if !b.cells[c].mine && !b.cells[c].open && (b.cells[c].near == 0) == zeros {
                            b.open(c);
                            opens += 1;
                        }
                    }
                }
                assert_eq!(b.status, Status::Won);
                // the first open is one of the 3BV's (it is a zero: the first cell has no mine near)
                assert_eq!(opens, bv, "{dims:?} seed {seed}");
            }
        }
    }
}
