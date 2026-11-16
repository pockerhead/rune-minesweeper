//! Replays a run code on a fresh board with the game's rules: only a real win, in plausible
//! time, on the board the code says it was played on, passes.

use crate::board::{Board, Dims, Status};
use crate::run::{Kind, Run};
use crate::{LEVELS, Level};

/// The least time between two opens, milliseconds. The boat has to sail to every cell it
/// opens: sent on the moment it opens one, it opens the next cell over 83 ms later at the
/// quickest (`a_played_win_replays_in_the_verifier` in the game measures it). Half that
/// leaves room for a frame either way.
pub const MIN_OPEN_GAP_MS: u32 = 40;

/// The boat's top speed, cells a second (`VMAX` in the game's `boat.rs`, whose tests hold it
/// to this).
pub const BOAT_TOP_SPEED: f32 = 5.0;

/// The least time the boat takes from opening cell `a` to opening cell `b`, milliseconds.
/// A cell opens as the boat comes within `DOOR` of its middle along the route, whose last
/// stretch runs along the cell's side and into it: so the boat is inside the cell's square
/// (or on its edge) at that moment (`boat.rs` tests it). Between two opens it crosses at
/// least the straight gap between the two squares, at top speed at most. Kept low on
/// purpose (a tenth off, and a frame of the clock's jitter), so no played game falls under it.
pub fn sail_ms(dims: Dims, a: usize, b: usize) -> u32 {
    let gap = |p: usize, q: usize| p.abs_diff(q).saturating_sub(1) as f32;
    let (dx, dz) = (gap(a % dims.w, b % dims.w), gap(a / dims.w, b / dims.w));
    (dx.hypot(dz) / BOAT_TOP_SPEED * 1000.0 * 0.9 - 20.0).max(0.0) as u32
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reject {
    /// The level is not one of the ranked ones.
    Level,
    NoMoves,
    /// A move names a cell the board doesn't have.
    Cell,
    /// The first move is not at 0 ms, or a move is not later than the one before.
    Timing,
    /// Two opens closer together than the boat can sail.
    TooFast,
    /// The mines came out elsewhere than in the game that was played.
    BoardMismatch,
    /// The board takes too few clicks to clear (its 3BV is under the level's `min_3bv`).
    EasyBoard,
    /// A move that changes nothing: the game never records one.
    IdleMove,
    /// A move after the game was over.
    AfterEnd,
    /// The run opens a mine.
    Lost,
    /// The run ends with the board not cleared.
    Unfinished,
}

impl std::fmt::Display for Reject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Reject::Level => "the run is not on a ranked level (custom games are not ranked)",
            Reject::NoMoves => "the run has no moves",
            Reject::Cell => "a move names a cell that is not on the board",
            Reject::Timing => "the move times do not start at zero and keep increasing",
            Reject::TooFast => "two cells were opened faster than the boat can sail between them",
            Reject::BoardMismatch => "the replayed board does not match the one the run was played on",
            Reject::EasyBoard => "the board takes too few clicks to clear to be ranked",
            Reject::IdleMove => "a move changes nothing, which the game never records",
            Reject::AfterEnd => "there are moves after the game was over",
            Reject::Lost => "the run opens a mine",
            Reject::Unfinished => "the run does not clear the board",
        })
    }
}

/// A run that replays to a win.
#[derive(Debug)]
pub struct Verified {
    pub level: &'static Level,
    pub time_ms: u32,
}

pub fn verify(run: &Run) -> Result<Verified, Reject> {
    let level = LEVELS.get(run.level as usize).ok_or(Reject::Level)?;
    let first = run.moves.first().ok_or(Reject::NoMoves)?;
    if first.ms != 0 || run.moves.windows(2).any(|w| w[1].ms <= w[0].ms) {
        return Err(Reject::Timing);
    }
    let mut board = Board::new(level.dims, level.mines, run.seed);
    let mut last_open: Option<(u32, usize)> = None;
    for m in &run.moves {
        if m.cell >= level.dims.count() {
            return Err(Reject::Cell);
        }
        if board.status != Status::Playing {
            return Err(Reject::AfterEnd);
        }
        let changed = match m.kind {
            Kind::Flag => board.toggle_flag(m.cell),
            Kind::Open => {
                if last_open.is_some_and(|(t, c)| m.ms - t < MIN_OPEN_GAP_MS.max(sail_ms(level.dims, c, m.cell))) {
                    return Err(Reject::TooFast);
                }
                last_open = Some((m.ms, m.cell));
                let was_placed = board.placed();
                let opened = board.open(m.cell);
                if !was_placed && board.placed() {
                    if board.fingerprint() != run.fingerprint {
                        return Err(Reject::BoardMismatch);
                    }
                    if board.three_bv() < level.min_3bv {
                        return Err(Reject::EasyBoard);
                    }
                }
                !opened.is_empty() || board.status == Status::Lost
            }
        };
        if !changed {
            return Err(Reject::IdleMove);
        }
    }
    match board.status {
        Status::Won => Ok(Verified { level, time_ms: run.time_ms() }),
        Status::Lost => Err(Reject::Lost),
        Status::Playing => Err(Reject::Unfinished),
    }
}

/// Test helper: a won run played by a bot that knows the board.
#[cfg(test)]
pub mod bot {
    use super::*;

    /// A win on `level` from `seed`: opens `first`, then every closed safe cell in order,
    /// `gap_ms` apart (plus the boat's sail between two opens), flagging each mine on the way
    /// when `flags`.
    pub fn win(level: u8, seed: u64, first: usize, gap_ms: u32, flags: bool) -> Run {
        let l = &LEVELS[level as usize];
        let mut b = Board::new(l.dims, l.mines, seed);
        let mut moves = vec![(Kind::Open, first, 0.0)];
        b.open(first);
        let (mut t, mut last) = (0.0, first);
        for c in 0..l.dims.count() {
            if b.status != Status::Playing {
                break;
            }
            t += gap_ms as f64 / 1000.0;
            if b.cells[c].mine {
                if flags {
                    b.toggle_flag(c);
                    moves.push((Kind::Flag, c, t));
                }
            } else if !b.cells[c].open {
                t += sail_ms(l.dims, last, c) as f64 / 1000.0;
                b.open(c);
                moves.push((Kind::Open, c, t));
                last = c;
            }
        }
        assert_eq!(b.status, Status::Won);
        Run::from_times(level, [0, 3, 0], seed, b.fingerprint(), &moves)
    }
}

#[cfg(test)]
mod tests {
    use super::bot::win;
    use super::*;
    use crate::run::Move;

    #[test]
    fn a_win_replays() {
        for level in 0..3u8 {
            let run = win(level, 99 + level as u64, 40, 400, true);
            let v = verify(&run).expect("a real win");
            assert_eq!(v.level.key, LEVELS[level as usize].key);
            assert_eq!(v.time_ms, run.moves.last().unwrap().ms);
            // and survives the trip through a code
            let back = Run::decode(&run.encode()).unwrap();
            assert_eq!(verify(&back).map(|v| v.time_ms), Ok(run.time_ms()));
        }
    }

    #[test]
    fn a_loss_is_refused() {
        let mut run = win(0, 5, 40, 400, false);
        let b = {
            let mut b = Board::new(LEVELS[0].dims, LEVELS[0].mines, 5);
            b.open(40);
            b
        };
        let mine = (0..b.dims.count()).find(|&c| b.cells[c].mine).unwrap();
        run.moves.truncate(3);
        let ms = run.moves.last().unwrap().ms + 500;
        run.moves.push(Move { kind: Kind::Open, cell: mine, ms });
        assert_eq!(verify(&run).unwrap_err(), Reject::Lost);
    }

    #[test]
    fn an_unfinished_run_is_refused() {
        let mut run = win(1, 5, 40, 400, false);
        run.moves.pop();
        assert_eq!(verify(&run).unwrap_err(), Reject::Unfinished);
    }

    #[test]
    fn a_tampered_move_is_refused() {
        let run = win(0, 11, 40, 400, true);
        // an open moved onto another cell: a mine, a cell already open, or one the moves open later
        let k = run.moves.iter().rposition(|m| m.kind == Kind::Open && m.cell > 0).unwrap();
        for cell in [run.moves[k].cell - 1, 40, 0] {
            let mut t = run.clone();
            t.moves[k].cell = cell;
            assert!(verify(&t).is_err(), "cell {cell} in place of {}", run.moves[k].cell);
        }
        let mut t = run.clone();
        t.moves[k].cell = 81;
        assert_eq!(verify(&t).unwrap_err(), Reject::Cell);
        // a move added after the win
        let mut t = run.clone();
        t.moves.push(Move { kind: Kind::Flag, cell: 0, ms: run.time_ms() + 300 });
        assert_eq!(verify(&t).unwrap_err(), Reject::AfterEnd);
        // the level swapped
        let mut t = run.clone();
        t.level = 3;
        assert_eq!(verify(&t).unwrap_err(), Reject::Level);
    }

    #[test]
    fn a_board_mismatch_is_refused() {
        let run = win(2, 1234, 200, 400, false);
        let mut t = run.clone();
        t.fingerprint ^= 1;
        assert_eq!(verify(&t).unwrap_err(), Reject::BoardMismatch);
        // another seed lays another board: the fingerprint gives it away before anything else
        let mut t = run.clone();
        t.seed += 1;
        assert_eq!(verify(&t).unwrap_err(), Reject::BoardMismatch);
    }

    #[test]
    fn bad_timing_is_refused() {
        let run = win(0, 21, 40, 400, true);
        let mut t = run.clone();
        t.moves[2].ms = t.moves[1].ms;
        assert_eq!(verify(&t).unwrap_err(), Reject::Timing, "two moves at once");
        let mut t = run.clone();
        t.moves[2].ms = t.moves[1].ms - 1;
        assert_eq!(verify(&t).unwrap_err(), Reject::Timing, "back in time");
        let mut t = run.clone();
        for m in &mut t.moves {
            m.ms += 5;
        }
        assert_eq!(verify(&t).unwrap_err(), Reject::Timing, "not from zero");
        let fast = win(0, 21, 40, 400, false);
        let fast = Run {
            moves: fast.moves.iter().enumerate().map(|(k, m)| Move { ms: k as u32 * (MIN_OPEN_GAP_MS - 1), ..*m }).collect(),
            ..fast
        };
        assert_eq!(verify(&fast).unwrap_err(), Reject::TooFast);
        let mut empty = run.clone();
        empty.moves.clear();
        assert_eq!(verify(&empty).unwrap_err(), Reject::NoMoves);
    }

    #[test]
    fn an_idle_move_is_refused() {
        let run = win(0, 5, 40, 400, false);
        let mut t = run.clone();
        // the first cell opened again
        let at = (t.moves[0].ms + t.moves[1].ms) / 2;
        if at > t.moves[0].ms && at < t.moves[1].ms {
            t.moves.insert(1, Move { kind: Kind::Open, cell: 40, ms: at });
            assert_eq!(verify(&t).unwrap_err(), Reject::IdleMove);
        }
    }

    /// Boards searched for because a click or two clears them (the review's seeds 16219 and
    /// 1264 from cell 40, 3BV 1 and 2) are not ranked, however the run is timed.
    #[test]
    fn an_easy_board_is_not_ranked() {
        for seed in [16219, 1264, 3] {
            let run = win(0, seed, 40, 400, false);
            let mut b = Board::new(LEVELS[0].dims, LEVELS[0].mines, seed);
            b.open(40);
            assert!(b.three_bv() < LEVELS[0].min_3bv, "seed {seed}: 3BV {}", b.three_bv());
            assert_eq!(verify(&run).unwrap_err(), Reject::EasyBoard, "seed {seed}");
        }
        // the review's code: one open, 0.000 s
        let one = Run::decode("AQAAAwBbPwAAAAAAABvvzrwBUABIwoDW").unwrap();
        assert_eq!(one.moves.len(), 1);
        assert_eq!(verify(&one).unwrap_err(), Reject::EasyBoard);
    }

    /// Two opens far apart need the boat's sail between them: a run that opens the board's
    /// corners 40 ms apart is too fast, one that leaves the sail's time is not.
    #[test]
    fn opens_far_apart_need_the_boats_sail() {
        let dims = LEVELS[2].dims;
        assert_eq!(sail_ms(dims, 0, 1), 0, "next door: only the gap between opens counts");
        assert_eq!(sail_ms(dims, 0, 31), 0, "diagonal next door");
        let across = sail_ms(dims, 0, dims.count() - 1);
        // 28 cells across and 14 down between the two corner squares, at 5 cells a second
        assert_eq!(sail_ms(dims, 0, 2), 160, "one cell between: 0.2 s, less the margins");
        assert!((5_400..6_200).contains(&across), "{across} ms");
        let run = win(2, 1234, 200, 40, false);
        assert!(verify(&run).is_ok(), "the bot leaves the sail's time");
        let squeezed = Run {
            moves: run.moves.iter().enumerate().map(|(k, m)| Move { ms: k as u32 * MIN_OPEN_GAP_MS, ..*m }).collect(),
            ..run.clone()
        };
        assert_eq!(verify(&squeezed).unwrap_err(), Reject::TooFast);
    }
}
