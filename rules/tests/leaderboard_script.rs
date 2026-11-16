//! `.github/scripts/leaderboard.sh` run for real: a stand-in `gh` serves sample issues and
//! records what the script does with them, `origin` is a local bare repository, and
//! verify-score is the one built here. Needs `bash` and `git` (Git Bash on Windows); set
//! LEADERBOARD_BASH to use another bash.

use std::path::{Path, PathBuf};
use std::process::Command;

use mines_rules::LEVELS;
use mines_rules::board::{Board, Status};
use mines_rules::leaderboard::{History, Leaderboard};
use mines_rules::replay::sail_ms;
use mines_rules::run::{Kind, Run};

/// The stand-in for gh: issues from $FIXTURES, its state (closed issues, comments, failures
/// to fake) under $STATE, and a line in $STATE/log for each thing it is asked to do. A file
/// $STATE/fail/NAME makes that call fail (once, with NAME.once next to it).
const GH: &str = r#"#!/usr/bin/env bash
log() { printf '%s\n' "$*" >>"$STATE/log"; }
fail() {
    if [[ -e $STATE/fail/$1 ]]; then
        [[ -e $STATE/fail/$1.once ]] && rm -f "$STATE/fail/$1" "$STATE/fail/$1.once"
        log "FAIL $1"
        exit 1
    fi
}
case "$1 $2" in
    "label create") log "label create $3" ;;
    "issue list")
        for f in "$FIXTURES"/*.json; do
            n=$(basename "$f" .json)
            [[ -e $STATE/closed/$n ]] || echo "$n"
        done | sort -rn # newest first, as GitHub lists them
        ;;
    "api repos/"*/comments)
        n=${2%/comments}
        n=${n##*/}
        if [[ -e $STATE/comments/$n ]]; then wc -l <"$STATE/comments/$n"; else echo 0; fi
        ;;
    "api repos/"*)
        n=${2##*/}
        fail "api-$n"
        cat "$FIXTURES/$n.json"
        ;;
    "issue comment")
        fail "comment-$3"
        echo 1 >>"$STATE/comments/$3"
        log "comment #$3 after $(git --git-dir="$REMOTE" rev-list --count main) commits: $(tr '\n' ' ' <"$7")"
        ;;
    "issue edit") log "label #$3 $7" ;;
    "issue close")
        fail "close-$3"
        touch "$STATE/closed/$3"
        log "close #$3 $7"
        ;;
    *)
        log "UNEXPECTED gh $*"
        exit 1
        ;;
esac
"#;

/// A won beginner run from `seed` (first cell 40), as a bot that knows the board plays it,
/// leaving the boat its time between opens.
fn won_code(seed: u64) -> String {
    let l = &LEVELS[0];
    let mut b = Board::new(l.dims, l.mines, seed);
    let mut moves = vec![(Kind::Open, 40, 0.0)];
    b.open(40);
    let (mut t, mut last) = (0.0, 40);
    for c in 0..l.dims.count() {
        if !b.cells[c].mine && !b.cells[c].open {
            b.open(c);
            t += 0.3 + sail_ms(l.dims, last, c) as f64 / 1000.0;
            moves.push((Kind::Open, c, t));
            last = c;
        }
    }
    assert_eq!(b.status, Status::Won);
    Run::from_times(0, [0, 3, 0], seed, b.fingerprint(), &moves).encode()
}

fn form(code: &str) -> String {
    format!("### Level\n\nBeginner\n\n### Time\n\n1.000 s\n\n### Run code\n\n{code}\n")
}

fn issue(dir: &Path, number: u64, login: &str, body: &str) {
    let json = serde_json::json!({
        "number": number, "state": "open", "body": body, "created_at": "2026-11-16T10:00:00Z",
        "user": { "login": login, "type": "User" }, "labels": [{ "name": "score" }],
    });
    std::fs::write(dir.join(format!("{number}.json")), json.to_string()).unwrap();
}

/// A path as bash and git take it on every system (forward slashes).
fn slashed(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").current_dir(dir).args(args).output().expect("git runs");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

struct Sandbox {
    root: PathBuf,
    repo: PathBuf,
    remote: PathBuf,
    state: PathBuf,
    fixtures: PathBuf,
    bin: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("leaderboard-script");
        let _ = std::fs::remove_dir_all(&root);
        let s = Sandbox {
            repo: root.join("repo"),
            remote: root.join("remote.git"),
            state: root.join("state"),
            fixtures: root.join("issues"),
            bin: root.join("bin"),
            root,
        };
        for d in [&s.repo, &s.state, &s.fixtures, &s.bin] {
            std::fs::create_dir_all(d).unwrap();
        }
        for d in ["closed", "comments", "fail"] {
            std::fs::create_dir_all(s.state.join(d)).unwrap();
        }
        std::fs::write(s.bin.join("gh"), GH).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(s.bin.join("gh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // the repository: the real README, leaderboard and script, pushed to a bare origin
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        std::fs::create_dir_all(s.repo.join("leaderboard")).unwrap();
        std::fs::create_dir_all(s.repo.join(".github/scripts")).unwrap();
        for f in ["README.md", "leaderboard/leaderboard.json", "leaderboard/history.json", ".github/scripts/leaderboard.sh"] {
            std::fs::copy(src.join(f), s.repo.join(f)).unwrap();
        }
        git(&s.root, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        git(&s.repo, &["init", "-q", "-b", "main"]);
        git(&s.repo, &["config", "core.autocrlf", "false"]);
        git(&s.repo, &["add", "-A"]);
        git(&s.repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
        git(&s.repo, &["remote", "add", "origin", &slashed(&s.remote)]);
        git(&s.repo, &["push", "-q", "origin", "main"]);
        // origin turns pushes away while $STATE/fail/push is there
        let hook = s.remote.join("hooks/pre-receive");
        std::fs::write(
            &hook,
            "#!/bin/sh
if [ -e \"$STATE/fail/push\" ]; then echo 'push refused' >&2; exit 1; fi
",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        s
    }

    fn fail(&self, name: &str, once: bool) {
        std::fs::write(self.state.join("fail").join(name), "").unwrap();
        if once {
            std::fs::write(self.state.join("fail").join(format!("{name}.once")), "").unwrap();
        }
    }

    /// Runs the script; its exit code and output.
    fn run(&self) -> (i32, String) {
        let bash = std::env::var("LEADERBOARD_BASH").unwrap_or_else(|_| "bash".into());
        let path = std::env::join_paths(std::iter::once(self.bin.clone()).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())))
            .unwrap();
        let out = Command::new(bash)
            .current_dir(&self.repo)
            .arg(".github/scripts/leaderboard.sh")
            .env("PATH", path)
            .env_remove("GH_TOKEN")
            .env("REPO", "owner/game")
            .env("VERIFY", slashed(Path::new(env!("CARGO_BIN_EXE_verify-score"))))
            .env("RUNNER_TEMP", slashed(&self.root.join("tmp")))
            .env("FIXTURES", slashed(&self.fixtures))
            .env("STATE", slashed(&self.state))
            .env("REMOTE", slashed(&self.remote))
            .output()
            .expect("bash runs");
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), text)
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.state.join("log")).unwrap_or_default()
    }

    /// A file as it is on origin's main.
    fn on_main(&self, file: &str) -> String {
        git(&self.remote, &["show", &format!("main:{file}")])
    }
}

#[test]
fn the_workflow_script_handles_a_batch() {
    let s = Sandbox::new();
    let ann = won_code(77);
    issue(&s.fixtures, 201, "ann", &form(&ann));
    issue(&s.fixtures, 202, "dan", &form(&won_code(5)));
    issue(&s.fixtures, 203, "bob", &form(&won_code(1)));
    issue(&s.fixtures, 204, "zed", "The boat is too slow <b>please</b>");
    issue(&s.fixtures, 205, "eve", &form(&ann));
    issue(&s.fixtures, 206, "carl", &form(&won_code(99)));
    issue(&s.fixtures, 207, "mal", &form("`$(touch pwned)`<script>"));
    // the API fails for 202 throughout this run; 206 cannot be closed once
    s.fail("api-202", false);
    s.fail("close-206", true);

    let (code, out) = s.run();
    let log = s.log();
    assert_eq!(code, 1, "a failed issue shows: {out}");
    assert!(out.contains("#202 could not be handled") && out.contains("#206 could not be handled"), "{out}");
    // the issues after a failed one are still checked
    for n in [201, 203, 204, 205, 207] {
        assert!(log.contains(&format!("close #{n} ")), "#{n} not closed:\n{log}");
    }
    assert!(log.contains("label #201 score-accepted") && log.contains("label #203 score-accepted"), "{log}");
    assert!(log.contains("label #204 score-rejected") && log.contains("label #205 score-rejected"), "{log}");
    assert!(log.contains("label #207 score-rejected"), "{log}");
    assert!(log.contains("comment #204 after 3 commits: Not ranked: this issue is not the score form"), "{log}");
    assert!(log.contains("already ranked for another player"), "{log}");
    assert!(!log.contains("UNEXPECTED") && !s.repo.join("pwned").exists());
    // oldest first (so the original beats a copy filed later), and a placing is on main
    // before it is announced
    assert!(log.contains("comment #201 after 2 commits: Verified: Beginner in"), "{log}");
    assert!(log.contains("comment #203 after 3 commits: Verified: Beginner in"), "{log}");
    assert!(log.contains("comment #206 after 4 commits: Verified: Beginner in"), "{log}");

    let board = Leaderboard::parse(&s.on_main("leaderboard/leaderboard.json")).unwrap();
    let mut players: Vec<&str> = board.entries("beginner").iter().map(|e| e.login.as_str()).collect();
    players.sort();
    assert_eq!(players, ["ann", "bob", "carl"]);
    let history = History::parse(&s.on_main("leaderboard/history.json")).unwrap();
    assert_eq!(history.runs.len(), 3);
    assert!(s.on_main("README.md").contains("[ann](https://github.com/ann)"));
    assert_eq!(git(&s.repo, &["status", "--porcelain"]), "", "nothing left half done");

    // the next run: 206 is closed without a second comment or commit; 202 reads now, but
    // origin turns its commit away, so it waits and nothing of it stays behind
    std::fs::remove_file(s.state.join("fail/api-202")).unwrap();
    s.fail("push", false);
    let (code, out) = s.run();
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("#202 could not be handled"), "{out}");
    let log = s.log();
    assert_eq!(log.matches("comment #206 ").count(), 1, "{log}");
    assert_eq!(log.matches("close #206 ").count(), 1, "{log}");
    assert!(!log.contains("#202 "), "{log}");
    assert_eq!(git(&s.repo, &["rev-parse", "HEAD"]), git(&s.remote, &["rev-parse", "main"]), "the refused commit is dropped");
    assert_eq!(git(&s.repo, &["status", "--porcelain"]), "");

    // and then it comes through
    std::fs::remove_file(s.state.join("fail/push")).unwrap();
    let (code, out) = s.run();
    assert_eq!(code, 0, "{out}");
    let log = s.log();
    assert!(log.contains("label #202 score-accepted"), "{log}");
    assert_eq!(git(&s.remote, &["rev-list", "--count", "main"]).trim(), "5", "init, 201, 203, 206, then 202");
    let history = History::parse(&s.on_main("leaderboard/history.json")).unwrap();
    assert_eq!(history.runs.len(), 4);

    // and one after that has nothing left to do
    let (code, out) = s.run();
    assert_eq!(code, 0, "{out}");
    let issue_calls = |log: &str| log.lines().filter(|l| !l.starts_with("label create")).count();
    assert_eq!(issue_calls(&s.log()), issue_calls(&log), "no issue is touched again");
}
