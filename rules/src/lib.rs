//! Mines Afloat's rules without the sea: the board (`board`), the standard levels, run codes
//! that record a game move by move (`run`), their replay (`replay`), the shared leaderboard
//! (`leaderboard`) and the issue form a score is submitted with (`submit`).

pub mod board;
pub mod leaderboard;
pub mod replay;
pub mod run;
pub mod submit;

use board::Dims;

#[derive(Debug)]
pub struct Level {
    /// The records and leaderboard key.
    pub key: &'static str,
    pub name: &'static str,
    pub dims: Dims,
    pub mines: usize,
    /// The least 3BV (`Board::three_bv`) a board needs to be ranked: about the easiest 5% of
    /// boards are not, so a searched-for board that a few clicks clear gets nowhere.
    /// Measured over 200,000 random seeds and first cells, the 5th percentile is 8, 45 and
    /// 140 (medians 15, 63, 171).
    pub min_3bv: usize,
}

/// The ranked levels; a run code names one by its index here.
pub const LEVELS: [Level; 3] = [
    Level { key: "beginner", name: "Beginner", dims: Dims { w: 9, h: 9 }, mines: 10, min_3bv: 8 },
    Level { key: "intermediate", name: "Intermediate", dims: Dims { w: 16, h: 16 }, mines: 40, min_3bv: 45 },
    Level { key: "expert", name: "Expert", dims: Dims { w: 30, h: 16 }, mines: 99, min_3bv: 140 },
];

/// The level with records key `key`, and its index in `LEVELS`.
pub fn level(key: &str) -> Option<(usize, &'static Level)> {
    LEVELS.iter().enumerate().find(|(_, l)| l.key == key)
}

/// Milliseconds as the leaderboard shows them: "12.345 s".
pub fn fmt_ms(ms: u32) -> String {
    format!("{}.{:03} s", ms / 1000, ms % 1000)
}
