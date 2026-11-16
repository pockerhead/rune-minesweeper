//! Submitting a score: the prefilled issue the game opens, and reading that issue's body back.
//! The form is `.github/ISSUE_TEMPLATE/score.yml`; its field ids are the query keys here and
//! its labels the headings GitHub writes into the body.

use crate::run::Run;
use crate::{LEVELS, fmt_ms};

pub const REPO: &str = "pockerhead/rune-minesweeper";
/// The issue form's file name in `.github/ISSUE_TEMPLATE`.
pub const TEMPLATE: &str = "score.yml";
/// The label the form puts on its issues.
pub const LABEL: &str = "score";
/// The leaderboard as the game reads it: main's copy, served with `Access-Control-Allow-Origin: *`.
pub const LEADERBOARD_URL: &str = "https://raw.githubusercontent.com/pockerhead/rune-minesweeper/main/leaderboard/leaderboard.json";
/// The README's leaderboard section.
pub const LEADERBOARD_PAGE: &str = "https://github.com/pockerhead/rune-minesweeper#leaderboard";
/// The longest issue body read, bytes.
pub const MAX_BODY: usize = 16 * 1024;

/// The form's headings, as GitHub writes them into the issue body.
pub const LEVEL_HEADING: &str = "Level";
pub const TIME_HEADING: &str = "Time";
pub const RUN_HEADING: &str = "Run code";

/// The new-issue link for `run`, its fields filled in.
pub fn issue_url(run: &Run) -> String {
    let level = LEVELS.get(run.level as usize).map_or("?", |l| l.name);
    let time = fmt_ms(run.time_ms());
    let title = format!("Score: {level} in {time}");
    format!(
        "https://github.com/{REPO}/issues/new?template={TEMPLATE}&title={}&level={}&time={}&run={}",
        encode(&title),
        encode(level),
        encode(&time),
        run.encode()
    )
}

/// Percent-encodes everything but unreserved characters.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// The value under `### heading` in an issue form's body: the text up to the next heading,
/// trimmed; None when the heading is missing or GitHub wrote "_No response_".
pub fn field<'a>(body: &'a str, heading: &str) -> Option<&'a str> {
    let mut lines = body.lines();
    lines.find(|l| l.trim_end().strip_prefix("### ").is_some_and(|h| h.trim() == heading))?;
    let start = lines.clone().next().map(|l| l.as_ptr() as usize - body.as_ptr() as usize)?;
    let rest = &body[start..];
    let end = rest.lines().find(|l| l.starts_with("### ")).map_or(rest.len(), |l| l.as_ptr() as usize - rest.as_ptr() as usize);
    let value = rest[..end].trim();
    (!value.is_empty() && value != "_No response_").then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::bot::win;
    use crate::run::{Kind, Move};

    #[test]
    fn the_issue_link_fills_the_form() {
        let run = win(0, 7, 40, 400, true);
        let url = issue_url(&run);
        let time = fmt_ms(run.time_ms()).replace(' ', "%20");
        assert!(
            url.starts_with(
                "https://github.com/pockerhead/rune-minesweeper/issues/new?template=score.yml&title=Score%3A%20Beginner%20in%20"
            )
        );
        assert!(url.contains(&format!("&level=Beginner&time={time}&run={}", run.encode())), "{url}");
    }

    /// A long expert game, slower and fussier than any real one: every safe cell opened one
    /// by one (no flood helps), every mine flagged, unflagged and flagged again, minutes
    /// between moves. Its link still fits the 8 KB browsers and GitHub take.
    #[test]
    fn a_long_expert_run_fits_in_a_link() {
        let real = win(2, 3, 0, 400, false);
        let l = &LEVELS[2];
        let mut moves = Vec::new();
        let mut ms = 0u32;
        let mut push = |kind, cell| {
            moves.push(Move { kind, cell, ms });
            ms += 180_000; // three minutes a move: three varint bytes
        };
        for c in 0..l.dims.count() {
            push(Kind::Open, c);
        }
        for c in 0..l.mines {
            for _ in 0..3 {
                push(Kind::Flag, l.dims.count() - 1 - c);
            }
        }
        let long = Run { moves, ..real };
        assert_eq!(long.moves.len(), 480 + 3 * 99);
        let url = issue_url(&long);
        println!("long expert run: {} moves, {} byte link", long.moves.len(), url.len());
        assert!(url.len() < 7000, "{} bytes", url.len());
        assert_eq!(Run::decode(&long.encode()).unwrap(), long);
        // a realistic one, from the bot that opens what floods do not
        let real_url = issue_url(&win(2, 3, 200, 2000, true));
        println!("bot expert run: {} byte link", real_url.len());
        assert!(real_url.len() < 3000);
    }

    #[test]
    fn the_form_body_reads_back() {
        let body = "### Level\n\nExpert\n\n### Time\n\n12.345 s\n\n### Run code\n\nAbc_-123\n\n### Notes\n\n_No response_\n";
        assert_eq!(field(body, LEVEL_HEADING), Some("Expert"));
        assert_eq!(field(body, TIME_HEADING), Some("12.345 s"));
        assert_eq!(field(body, RUN_HEADING), Some("Abc_-123"));
        assert_eq!(field(body, "Notes"), None);
        assert_eq!(field(body, "Missing"), None);
        let crlf = body.replace('\n', "\r\n");
        assert_eq!(field(&crlf, RUN_HEADING), Some("Abc_-123"));
        assert_eq!(field("### Run code\n", RUN_HEADING), None);
        assert_eq!(field("### Run code", RUN_HEADING), None);
    }
}
