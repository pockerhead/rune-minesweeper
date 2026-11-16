//! The world leaderboard: `leaderboard/leaderboard.json` in the repository, the best time of
//! each GitHub user per level, fastest first, and the top-ten tables in the README.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{LEVELS, fmt_ms};

/// Entries kept per level.
pub const TOP: usize = 50;
/// Rows per level in the README.
pub const README_ROWS: usize = 10;
pub const README_START: &str = "<!-- leaderboard:start -->";
pub const README_END: &str = "<!-- leaderboard:end -->";

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub login: String,
    pub time_ms: u32,
    /// The day it was submitted, YYYY-MM-DD.
    pub date: String,
    /// The game version it was played on.
    pub version: String,
    /// The issue it was submitted with.
    pub issue: u64,
    /// The board's seed in hex: a board is ranked once, so a copied run code gets nowhere.
    pub seed: String,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Leaderboard {
    pub format: u32,
    /// Level key to its entries, fastest first.
    pub levels: BTreeMap<String, Vec<Entry>>,
}

impl Default for Leaderboard {
    fn default() -> Leaderboard {
        Leaderboard { format: 1, levels: LEVELS.iter().map(|l| (l.key.to_string(), Vec::new())).collect() }
    }
}

/// Where a submitted time ended up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Placing {
    /// On the board at this place (1 the fastest), the player's first time there or a better one.
    Placed(usize),
    /// The player already has this time or a faster one on the board.
    NotBetter { best_ms: u32 },
    /// Slower than every one of the `TOP` entries.
    Outside,
    /// The board (seed) is already ranked for another player.
    BoardTaken,
}

/// A GitHub login as GitHub allows them: letters, digits and single hyphens, at most 39.
pub fn valid_login(login: &str) -> bool {
    (1..=39).contains(&login.len())
        && login.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !login.starts_with('-')
        && !login.ends_with('-')
}

impl Leaderboard {
    pub fn parse(json: &str) -> Result<Leaderboard, serde_json::Error> {
        serde_json::from_str(json)
    }

    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("plain data serializes");
        s.push('\n');
        s
    }

    pub fn entries(&self, level: &str) -> &[Entry] {
        self.levels.get(level).map_or(&[], Vec::as_slice)
    }

    /// Puts `entry` on `level`'s board if it is the player's best, keeping one entry per
    /// login, fastest first (on a tie the earlier issue), at most `TOP`.
    pub fn submit(&mut self, level: &str, entry: Entry) -> Placing {
        let list = self.levels.entry(level.to_string()).or_default();
        if list.iter().any(|e| e.seed == entry.seed && e.login != entry.login) {
            return Placing::BoardTaken;
        }
        if let Some(best) = list.iter().find(|e| e.login == entry.login)
            && best.time_ms <= entry.time_ms
        {
            return Placing::NotBetter { best_ms: best.time_ms };
        }
        let mut next = list.clone();
        next.retain(|e| e.login != entry.login);
        let login = entry.login.clone();
        next.push(entry);
        next.sort_by_key(|e| (e.time_ms, e.issue));
        let Some(place) = next.iter().position(|e| e.login == login) else { unreachable!() };
        if place >= TOP {
            return Placing::Outside;
        }
        next.truncate(TOP);
        *list = next;
        Placing::Placed(place + 1)
    }

    /// The README's leaderboard: a top-ten table per level.
    pub fn readme_tables(&self) -> String {
        let mut s = String::new();
        for level in &LEVELS {
            s.push_str(&format!("\n**{}**\n\n", level.name));
            let rows = &self.entries(level.key)[..self.entries(level.key).len().min(README_ROWS)];
            if rows.is_empty() {
                s.push_str("No scores yet.\n");
                continue;
            }
            s.push_str("| # | Player | Time | Date | Version |\n|---:|---|---:|---|---|\n");
            for (k, e) in rows.iter().enumerate() {
                // logins come from GitHub, but the file is data: keep anything else out of the table
                let player = if valid_login(&e.login) { format!("[{0}](https://github.com/{0})", e.login) } else { "?".to_string() };
                let date: String = e.date.chars().filter(|c| c.is_ascii_digit() || *c == '-').take(10).collect();
                let version: String = e.version.chars().filter(|c| c.is_ascii_digit() || *c == '.').take(16).collect();
                s.push_str(&format!("| {} | {player} | {} | {date} | {version} |\n", k + 1, fmt_ms(e.time_ms)));
            }
        }
        s.push('\n');
        s
    }
}

/// Every run the workflow accepted, `leaderboard/history.json`: ranked or not, still on the
/// board or long pushed off it. Run codes are public in the issues, so a board (seed) stays
/// its first player's for good, and an issue is recorded once. Kept apart from the
/// leaderboard so the file the game fetches stays small.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct History {
    pub format: u32,
    pub runs: Vec<Accepted>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Accepted {
    pub level: String,
    /// The board's seed in hex, as in `Entry::seed`.
    pub seed: String,
    pub login: String,
    pub time_ms: u32,
    pub issue: u64,
}

impl Default for History {
    fn default() -> History {
        History { format: 1, runs: Vec::new() }
    }
}

impl History {
    pub fn parse(json: &str) -> Result<History, serde_json::Error> {
        serde_json::from_str(json)
    }

    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("plain data serializes");
        s.push('\n');
        s
    }

    /// The run accepted from issue `issue`, if one was.
    pub fn of_issue(&self, issue: u64) -> Option<&Accepted> {
        self.runs.iter().find(|r| r.issue == issue)
    }

    /// Whether `level`'s board `seed` was accepted for someone other than `login`.
    pub fn taken(&self, level: &str, seed: &str, login: &str) -> bool {
        self.runs.iter().any(|r| r.level == level && r.seed == seed && r.login != login)
    }
}

/// `readme` with the text between the leaderboard markers replaced by `tables`; None without
/// both markers in order.
pub fn splice_readme(readme: &str, tables: &str) -> Option<String> {
    let start = readme.find(README_START)? + README_START.len();
    let end = start + readme[start..].find(README_END)?;
    Some(format!("{}{tables}{}", &readme[..start], &readme[end..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(login: &str, time_ms: u32, issue: u64) -> Entry {
        Entry { login: login.into(), time_ms, date: "2026-11-16".into(), version: "0.3.0".into(), issue, seed: format!("{issue:x}") }
    }

    #[test]
    fn one_best_per_player_fastest_first() {
        let mut b = Leaderboard::default();
        assert_eq!(b.submit("beginner", entry("ann", 20_000, 1)), Placing::Placed(1));
        assert_eq!(b.submit("beginner", entry("bob", 15_000, 2)), Placing::Placed(1));
        assert_eq!(b.submit("beginner", entry("cat", 17_000, 3)), Placing::Placed(2));
        assert_eq!(b.submit("beginner", entry("ann", 21_000, 4)), Placing::NotBetter { best_ms: 20_000 });
        assert_eq!(b.submit("beginner", entry("ann", 20_000, 5)), Placing::NotBetter { best_ms: 20_000 });
        assert_eq!(b.submit("beginner", entry("ann", 16_000, 6)), Placing::Placed(2));
        // a tie goes to the earlier issue
        assert_eq!(b.submit("beginner", entry("dan", 15_000, 7)), Placing::Placed(2));
        let order: Vec<(&str, u32)> = b.entries("beginner").iter().map(|e| (e.login.as_str(), e.time_ms)).collect();
        assert_eq!(order, [("bob", 15_000), ("dan", 15_000), ("ann", 16_000), ("cat", 17_000)]);
        assert!(b.entries("expert").is_empty());
    }

    #[test]
    fn the_board_keeps_the_top_fifty() {
        let mut b = Leaderboard::default();
        for k in 0..TOP as u64 {
            assert!(matches!(b.submit("expert", entry(&format!("p{k}"), 100_000 + k as u32, k)), Placing::Placed(_)));
        }
        assert_eq!(b.submit("expert", entry("slow", 200_000, 99)), Placing::Outside);
        assert_eq!(b.entries("expert").len(), TOP);
        assert_eq!(b.submit("expert", entry("fast", 1_000, 100)), Placing::Placed(1));
        assert_eq!(b.entries("expert").len(), TOP);
        assert_eq!(b.entries("expert").last().unwrap().login, "p48", "the slowest fell off");
    }

    #[test]
    fn a_ranked_board_is_not_ranked_again_for_someone_else() {
        let mut b = Leaderboard::default();
        b.submit("beginner", entry("ann", 20_000, 1));
        let mut copy = entry("eve", 19_000, 2);
        copy.seed = "1".into();
        assert_eq!(b.submit("beginner", copy), Placing::BoardTaken);
        let mut again = entry("ann", 19_000, 3);
        again.seed = "1".into();
        assert_eq!(b.submit("beginner", again), Placing::Placed(1), "the same player may better a time on a board");
    }

    #[test]
    fn json_round_trips() {
        let mut b = Leaderboard::default();
        b.submit("intermediate", entry("ann", 61_234, 9));
        assert_eq!(Leaderboard::parse(&b.to_json()).unwrap(), b);
        let empty = Leaderboard::parse(include_str!("../../leaderboard/leaderboard.json")).expect("the committed file parses");
        assert_eq!(empty, Leaderboard::default());
    }

    #[test]
    fn readme_tables_render_and_splice() {
        let mut b = Leaderboard::default();
        for k in 0..12u64 {
            b.submit("beginner", entry(&format!("p{k}"), 10_000 + k as u32 * 1000, k));
        }
        let mut odd = entry("x", 5_000, 50);
        odd.login = "<script>|".into();
        odd.version = "1.0|<b>".into();
        b.levels.get_mut("expert").unwrap().push(odd);
        let tables = b.readme_tables();
        assert!(tables.contains("| 1 | [p0](https://github.com/p0) | 10.000 s | 2026-11-16 | 0.3.0 |"), "{tables}");
        assert!(tables.contains("| 10 | [p9](https://github.com/p9) | 19.000 s |"));
        assert!(!tables.contains("p10"), "ten rows only");
        assert!(tables.contains("**Intermediate**\n\nNo scores yet."));
        assert!(tables.contains("| 1 | ? | 5.000 s | 2026-11-16 | 1.0 |"), "{tables}");
        assert!(!tables.contains('<'));

        let readme = format!("# Game\n\n## Leaderboard\n\n{README_START}\nold\n{README_END}\n\n## More\n");
        let new = splice_readme(&readme, &tables).unwrap();
        assert!(new.starts_with("# Game\n\n## Leaderboard\n\n<!-- leaderboard:start -->\n**Beginner**"));
        assert!(new.ends_with(&format!("{README_END}\n\n## More\n")));
        assert!(!new.contains("old"));
        assert_eq!(splice_readme(&new, &tables).unwrap(), new, "splicing is idempotent");
        assert_eq!(splice_readme("no markers", &tables), None);
        assert_eq!(splice_readme(&format!("{README_END}{README_START}"), &tables), None);
    }

    /// The README's tables are the ones the committed leaderboard renders to.
    #[test]
    fn the_readme_shows_the_committed_board() {
        let readme = include_str!("../../README.md");
        let board = Leaderboard::parse(include_str!("../../leaderboard/leaderboard.json")).unwrap();
        assert_eq!(splice_readme(readme, &board.readme_tables()).as_deref(), Some(readme));
    }

    #[test]
    fn logins_are_checked() {
        assert!(valid_login("pockerhead") && valid_login("a-b") && valid_login("A1"));
        assert!(!valid_login("") && !valid_login("-a") && !valid_login("a-") && !valid_login("a b") && !valid_login(&"a".repeat(40)));
    }

    #[test]
    fn history_keeps_boards_for_their_first_player() {
        let mut h = History::default();
        let run = |login: &str, seed: &str, issue| Accepted {
            level: "beginner".into(),
            seed: seed.into(),
            login: login.into(),
            time_ms: 9_000,
            issue,
        };
        h.runs.push(run("ann", "00000000000000ab", 4));
        assert!(h.taken("beginner", "00000000000000ab", "eve"));
        assert!(!h.taken("beginner", "00000000000000ab", "ann"), "her own board");
        assert!(!h.taken("expert", "00000000000000ab", "eve"), "another level's board");
        assert_eq!(h.of_issue(4).map(|r| r.login.as_str()), Some("ann"));
        assert!(h.of_issue(5).is_none());
        assert_eq!(History::parse(&h.to_json()).unwrap(), h);
        let empty = History::parse(include_str!("../../leaderboard/history.json")).expect("the committed file parses");
        assert_eq!(empty, History::default());
    }
}
