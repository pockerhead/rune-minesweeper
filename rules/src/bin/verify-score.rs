//! Checks one leaderboard submission: the issue (as `GET /repos/{repo}/issues/{n}` returns
//! it), replayed with the game's rules. On a ranked win it rewrites the leaderboard JSON and the
//! README's tables in place.
//!
//! ```text
//! verify-score --issue issue.json --leaderboard leaderboard/leaderboard.json \
//!     --history leaderboard/history.json --readme README.md --out DIR
//! ```
//!
//! Writes to DIR: `outcome` (`placed`: a new place on the board; `verified`: a real win that
//! is not a new place; `rejected`; `ignored`: not a submission, leave the issue alone),
//! `comment.md` for the issue (not for `ignored`) and, when it changed the files (every
//! newly accepted run goes into the history), `commit` with the commit message. An issue
//! already in the history is not counted again: it gets its verdict again, for a run that
//! stopped before closing it.
//! The issue is untrusted: nothing of its body reaches the comment, the commit or the files
//! except what the replay verified.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use mines_rules::leaderboard::{Accepted, Entry, History, Leaderboard, Placing, splice_readme, valid_login};
use mines_rules::replay::verify;
use mines_rules::run::Run;
use mines_rules::submit::{LABEL, LEVEL_HEADING, MAX_BODY, RUN_HEADING, field};
use mines_rules::{LEVELS, fmt_ms};
use serde::Deserialize;

#[derive(Deserialize)]
struct Issue {
    number: u64,
    state: String,
    #[serde(default)]
    body: Option<String>,
    user: User,
    #[serde(default)]
    labels: Vec<Label>,
    created_at: String,
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct User {
    login: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct Label {
    name: String,
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// A new place on the board: commit, comment, close.
    Placed { comment: String, commit: String },
    /// A real win that is no new place (or one recorded before): comment, close.
    Verified(String),
    /// Not ranked: comment why, close.
    Rejected(String),
    /// Not a submission (or already handled).
    Ignored,
}

/// Ends every comment; its marker lets `leaderboard.sh` see an issue already answered.
const FOOTER: &str =
    "\n\n<sub>Checked by the leaderboard workflow: the run code was replayed with the game's rules.</sub>\n<!-- leaderboard-verdict -->\n";

/// The oldest game version that writes run codes, and the largest part of a version read.
const FIRST_VERSION: [u32; 3] = [0, 3, 0];
const MAX_VERSION_PART: u32 = 999;

fn rejected(why: &str) -> Outcome {
    Outcome::Rejected(format!("Not ranked: {why}.{FOOTER}"))
}

/// Judges `issue` against `board` and `history` (changing `board` when the run places, and
/// `history` for every run it accepts).
fn judge(issue: &Issue, board: &mut Leaderboard, history: &mut History) -> Outcome {
    if issue.state != "open" || issue.pull_request.is_some() || !issue.labels.iter().any(|l| l.name == LABEL) {
        return Outcome::Ignored;
    }
    let body = issue.body.as_deref().unwrap_or("");
    if field(body, LEVEL_HEADING).is_none() && field(body, RUN_HEADING).is_none() && !body.contains("### Run code") {
        // labelled but not the form (or its headings were edited away): closed, or it would
        // be fetched again on every run
        return rejected("this issue is not the score form (submit from the game's win screen)");
    }
    if let Some(done) = history.of_issue(issue.number) {
        return recorded(done, board);
    }
    if issue.user.kind != "User" {
        return rejected("only people's accounts are ranked, not bots or apps");
    }
    if !valid_login(&issue.user.login) {
        return rejected("the account name could not be read");
    }
    if body.len() > MAX_BODY {
        return rejected("the issue is too long to be a score form");
    }
    let Some(code) = field(body, RUN_HEADING) else {
        return rejected("the form has no run code (submit from the game's win screen)");
    };
    let run = match Run::decode(code) {
        Ok(run) => run,
        Err(e) => return rejected(&e.to_string()),
    };
    if run.version < FIRST_VERSION || run.version.iter().any(|&v| v > MAX_VERSION_PART) {
        return rejected("the run code names a game version that does not exist");
    }
    let verified = match verify(&run) {
        Ok(v) => v,
        Err(why) => return rejected(&why.to_string()),
    };
    let (level, time) = (verified.level, fmt_ms(verified.time_ms));
    let date: String = issue.created_at.chars().take(10).collect();
    let entry = Entry {
        login: issue.user.login.clone(),
        time_ms: verified.time_ms,
        date,
        version: run.version_text(),
        issue: issue.number,
        seed: format!("{:016x}", run.seed),
    };
    let login = &issue.user.login;
    if history.taken(level.key, &entry.seed, login) {
        return rejected("this board is already ranked for another player");
    }
    let accepted =
        Accepted { level: level.key.into(), seed: entry.seed.clone(), login: login.clone(), time_ms: entry.time_ms, issue: issue.number };
    let placing = board.submit(level.key, entry);
    if placing != Placing::BoardTaken {
        history.runs.push(accepted);
    }
    match placing {
        Placing::Placed(place) => Outcome::Placed {
            comment: format!("Verified: {} in {time}. @{login} is **#{place}** on the {} leaderboard.{FOOTER}", level.name, level.name),
            commit: format!("Leaderboard: {login} #{place} on {} in {time} (#{})", level.name, issue.number),
        },
        Placing::NotBetter { best_ms } => Outcome::Verified(format!(
            "Verified: {} in {time}, but your best on the board, {}, is as fast or faster, so it stays.{FOOTER}",
            level.name,
            fmt_ms(best_ms)
        )),
        Placing::Outside => Outcome::Verified(format!(
            "Verified: {} in {time}, but the {} board keeps the {} fastest players and this time is slower than all of them.{FOOTER}",
            level.name,
            level.name,
            mines_rules::leaderboard::TOP
        )),
        Placing::BoardTaken => rejected("this board is already ranked for another player"),
    }
}

/// The verdict again for an issue whose run is in the history already: the workflow
/// recorded it, then stopped before closing the issue.
fn recorded(done: &Accepted, board: &Leaderboard) -> Outcome {
    let name = mines_rules::level(&done.level).map_or("?", |(_, l)| l.name);
    let (time, login) = (fmt_ms(done.time_ms), &done.login);
    match board.entries(&done.level).iter().position(|e| e.issue == done.issue) {
        Some(k) => Outcome::Verified(format!("Verified: {name} in {time}. @{login} is **#{}** on the {name} leaderboard.{FOOTER}", k + 1)),
        None => Outcome::Verified(format!("Verified: {name} in {time}. The run is recorded; it is not on the {name} board.{FOOTER}")),
    }
}

struct Args {
    issue: PathBuf,
    leaderboard: PathBuf,
    history: PathBuf,
    readme: PathBuf,
    out: PathBuf,
}

fn args() -> Result<Args, String> {
    let mut a = std::env::args().skip(1);
    let (mut issue, mut leaderboard, mut history, mut readme, mut out) = (None, None, None, None, None);
    while let Some(k) = a.next() {
        let v = a.next().map(PathBuf::from).ok_or(format!("{k} needs a value"))?;
        match k.as_str() {
            "--issue" => issue = Some(v),
            "--leaderboard" => leaderboard = Some(v),
            "--history" => history = Some(v),
            "--readme" => readme = Some(v),
            "--out" => out = Some(v),
            _ => return Err(format!("unknown argument {k}")),
        }
    }
    let need = |p: Option<PathBuf>, name: &str| p.ok_or(format!("--{name} is required"));
    Ok(Args {
        issue: need(issue, "issue")?,
        leaderboard: need(leaderboard, "leaderboard")?,
        history: need(history, "history")?,
        readme: need(readme, "readme")?,
        out: need(out, "out")?,
    })
}

fn read(p: &Path) -> Result<String, String> {
    std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))
}

fn write(p: &Path, text: &str) -> Result<(), String> {
    std::fs::write(p, text).map_err(|e| format!("{}: {e}", p.display()))
}

fn run() -> Result<(), String> {
    let a = args()?;
    let issue: Issue = serde_json::from_str(&read(&a.issue)?).map_err(|e| format!("the issue JSON: {e}"))?;
    let mut board = Leaderboard::parse(&read(&a.leaderboard)?).map_err(|e| format!("{}: {e}", a.leaderboard.display()))?;
    let mut history = History::parse(&read(&a.history)?).map_err(|e| format!("{}: {e}", a.history.display()))?;
    for l in &LEVELS {
        board.levels.entry(l.key.to_string()).or_default();
    }
    let recorded = history.runs.len();
    let outcome = judge(&issue, &mut board, &mut history);
    std::fs::create_dir_all(&a.out).map_err(|e| format!("{}: {e}", a.out.display()))?;
    let (name, comment, commit) = match &outcome {
        Outcome::Placed { comment, commit } => {
            let readme = read(&a.readme)?;
            let readme = splice_readme(&readme, &board.readme_tables()).ok_or("the README has no leaderboard markers")?;
            write(&a.leaderboard, &board.to_json())?;
            write(&a.readme, &readme)?;
            ("placed", Some(comment), Some(commit.clone()))
        }
        Outcome::Verified(c) => {
            let commit = (history.runs.len() > recorded).then(|| format!("Leaderboard: record the run of #{}", issue.number));
            ("verified", Some(c), commit)
        }
        Outcome::Rejected(c) => ("rejected", Some(c), None),
        Outcome::Ignored => ("ignored", None, None),
    };
    if let Some(commit) = commit {
        write(&a.history, &history.to_json())?;
        write(&a.out.join("commit"), &commit)?;
    }
    if let Some(c) = comment {
        write(&a.out.join("comment.md"), c)?;
    }
    write(&a.out.join("outcome"), name)?;
    println!("issue #{}: {name}", issue.number);
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("verify-score: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mines_rules::board::{Board, Status};
    use mines_rules::replay::sail_ms;
    use mines_rules::run::Kind;

    /// A won beginner game as a bot that knows the board plays it.
    fn won_code(seed: u64) -> String {
        let l = &LEVELS[0];
        let mut b = Board::new(l.dims, l.mines, seed);
        let mut moves = vec![(Kind::Open, 40, 0.0)];
        b.open(40);
        let (mut t, mut last) = (0.0, 40);
        for c in 0..l.dims.count() {
            if !b.cells[c].mine && !b.cells[c].open {
                b.open(c);
                t += 0.5 + sail_ms(l.dims, last, c) as f64 / 1000.0;
                moves.push((Kind::Open, c, t));
                last = c;
            }
        }
        assert_eq!(b.status, Status::Won);
        Run::from_times(0, [0, 3, 0], seed, b.fingerprint(), &moves).encode()
    }

    fn issue(number: u64, login: &str, body: &str) -> Issue {
        let json = serde_json::json!({
            "number": number, "state": "open", "body": body, "created_at": "2026-11-16T10:00:00Z",
            "user": { "login": login, "type": "User" }, "labels": [{ "name": "score" }],
        });
        serde_json::from_value(json).unwrap()
    }

    fn form(code: &str) -> String {
        format!("### Level\n\nBeginner\n\n### Time\n\n1.000 s\n\n### Run code\n\n{code}\n")
    }

    fn coded(code: &str, change: impl FnOnce(&mut Run)) -> String {
        let mut run = Run::decode(code).unwrap();
        change(&mut run);
        run.encode()
    }

    #[test]
    fn a_win_places_and_a_copy_of_it_does_not() {
        let (mut board, mut history) = (Leaderboard::default(), History::default());
        let code = won_code(77);
        let Outcome::Placed { comment, commit } = judge(&issue(5, "ann", &form(&code)), &mut board, &mut history) else { panic!() };
        assert!(comment.contains("**#1** on the Beginner leaderboard"), "{comment}");
        assert!(comment.ends_with("<!-- leaderboard-verdict -->\n"), "the marker leaderboard.sh looks for");
        assert!(commit.starts_with("Leaderboard: ann #1 on Beginner in "), "{commit}");
        assert_eq!(board.entries("beginner")[0].issue, 5);
        assert_eq!(board.entries("beginner")[0].date, "2026-11-16");
        assert_eq!(history.runs.len(), 1);
        assert!(matches!(judge(&issue(6, "ann", &form(&code)), &mut board, &mut history), Outcome::Verified(_)));
        assert_eq!(history.runs.len(), 2, "a verified run is recorded too");
        let Outcome::Rejected(why) = judge(&issue(7, "eve", &form(&code)), &mut board, &mut history) else { panic!() };
        assert!(why.contains("already ranked"));
        // long after ann's entry has left the board, her code is still hers
        board = Leaderboard::default();
        let Outcome::Rejected(why) = judge(&issue(8, "eve", &form(&code)), &mut board, &mut history) else { panic!() };
        assert!(why.contains("already ranked"));
        assert_eq!(history.runs.len(), 2);
    }

    /// A run already recorded (the workflow stopped between the push and the close) gets its
    /// verdict again and is not counted twice.
    #[test]
    fn a_recorded_issue_is_not_counted_again() {
        let (mut board, mut history) = (Leaderboard::default(), History::default());
        let code = won_code(77);
        assert!(matches!(judge(&issue(5, "ann", &form(&code)), &mut board, &mut history), Outcome::Placed { .. }));
        let (board0, history0) = (board.clone(), history.clone());
        let Outcome::Verified(again) = judge(&issue(5, "ann", &form(&code)), &mut board, &mut history) else { panic!() };
        assert!(again.contains("@ann is **#1** on the Beginner leaderboard"), "{again}");
        assert_eq!((board, history), (board0, history0.clone()), "nothing changes");
        // recorded, then pushed off the board
        let Outcome::Verified(again) = judge(&issue(5, "ann", &form(&code)), &mut Leaderboard::default(), &mut history0.clone()) else {
            panic!()
        };
        assert!(again.contains("not on the Beginner board"), "{again}");
    }

    #[test]
    fn a_slower_time_than_the_whole_board_is_verified_but_not_placed() {
        let (mut board, mut history) = (Leaderboard::default(), History::default());
        for k in 0..mines_rules::leaderboard::TOP as u64 {
            let e = Entry {
                login: format!("p{k}"),
                time_ms: 1,
                date: "2026-11-16".into(),
                version: "0.3.0".into(),
                issue: 100 + k,
                seed: format!("{k}"),
            };
            board.submit("beginner", e);
        }
        let before = board.clone();
        let Outcome::Verified(c) = judge(&issue(5, "ann", &form(&won_code(77))), &mut board, &mut history) else { panic!() };
        assert!(c.contains("keeps the 50 fastest players"), "{c}");
        assert_eq!(board, before);
        assert_eq!(history.runs.len(), 1, "recorded all the same: the board is ann's");
    }

    #[test]
    fn the_game_version_is_checked() {
        let (mut board, mut history) = (Leaderboard::default(), History::default());
        let code = won_code(77);
        for version in [[0, 2, 9], [1000, 0, 0], [0, 3, u32::MAX]] {
            let c = coded(&code, |r| r.version = version);
            let Outcome::Rejected(why) = judge(&issue(5, "ann", &form(&c)), &mut board, &mut history) else { panic!("{version:?}") };
            assert!(why.contains("version"), "{why}");
        }
        let c = coded(&code, |r| r.version = [2, 10, 999]);
        assert!(matches!(judge(&issue(5, "ann", &form(&c)), &mut board, &mut history), Outcome::Placed { .. }));
        assert_eq!(board.entries("beginner")[0].version, "2.10.999");
    }

    #[test]
    fn untrusted_bodies_are_refused_without_echo() {
        let (mut board, mut history) = (Leaderboard::default(), History::default());
        let evil = "<img src=x onerror=alert(1)>`$(rm -rf /)`";
        for body in [form(evil), form(&format!("{}{evil}", won_code(1))), "### Run code\n\n_No response_\n".to_string()] {
            let Outcome::Rejected(c) = judge(&issue(9, "bob", &body), &mut board, &mut history) else { panic!("{body}") };
            assert!(!c.contains("img") && !c.contains("rm -rf"), "{c}");
        }
        let huge = form(&"A".repeat(MAX_BODY));
        assert!(matches!(judge(&issue(9, "bob", &huge), &mut board, &mut history), Outcome::Rejected(c) if c.contains("too long")));
        assert_eq!(board, Leaderboard::default());
        assert_eq!(history, History::default());
    }

    #[test]
    fn non_submissions_are_ignored_or_closed_and_bots_refused() {
        let (mut board, mut history) = (Leaderboard::default(), History::default());
        let code = won_code(77);
        let mut i = issue(1, "ann", &form(&code));
        i.labels.clear();
        assert_eq!(judge(&i, &mut board, &mut history), Outcome::Ignored, "no score label");
        let mut i = issue(1, "ann", &form(&code));
        i.state = "closed".into();
        assert_eq!(judge(&i, &mut board, &mut history), Outcome::Ignored, "already handled");
        // labelled, but not the form: closed with a fixed text, so it is not fetched forever
        let mut no_body = issue(1, "ann", "");
        no_body.body = None;
        for i in [issue(1, "ann", "A bug report <b>"), no_body] {
            let Outcome::Rejected(c) = judge(&i, &mut board, &mut history) else { panic!("{:?}", i.body) };
            assert!(c.starts_with("Not ranked: this issue is not the score form"), "{c}");
        }
        let mut i = issue(1, "helper", &form(&code));
        i.user.kind = "Bot".into();
        assert!(matches!(judge(&i, &mut board, &mut history), Outcome::Rejected(_)));
        assert_eq!(board, Leaderboard::default());
        assert_eq!(history, History::default());
    }
}
