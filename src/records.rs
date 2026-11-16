//! Best times per level, kept in a small text file (`name=seconds` per line) in the user's
//! data folder: `%APPDATA%\mines-afloat` on Windows, `~/Library/Application Support/mines-afloat`
//! on macOS, `$XDG_DATA_HOME/mines-afloat` (`~/.local/share`) elsewhere, and the page's
//! `localStorage` in a browser. Records without a store (headless runs, tests) stay in memory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const APP_ID: &str = "mines-afloat";

#[derive(Default)]
pub struct Records {
    best: BTreeMap<String, f64>,
    /// Where they are saved; None keeps them in memory.
    file: Option<PathBuf>,
    /// Saved in the browser's `localStorage` instead.
    browser: bool,
}

#[cfg(target_arch = "wasm32")]
const STORAGE_KEY: &str = "mines-afloat.records";

#[cfg(target_arch = "wasm32")]
fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// The records file in this user's data folder, if there is a home folder to find it by.
pub fn default_file() -> Option<PathBuf> {
    rune::dirs::AppDirs::new(APP_ID).map(|d| d.data().join("records.txt"))
}

fn parse(text: &str) -> BTreeMap<String, f64> {
    text.lines()
        .filter_map(|line| {
            let (name, secs) = line.split_once('=')?;
            Some((name.trim().to_string(), secs.trim().parse::<f64>().ok()?))
        })
        .collect()
}

impl Records {
    /// The records in `file` (none yet if it does not exist); new records are saved there.
    pub fn load(file: PathBuf) -> Records {
        let best = match std::fs::read_to_string(&file) {
            Ok(text) => parse(&text),
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("mines: could not read the records in {}: {e}", file.display());
                }
                BTreeMap::new()
            }
        };
        Records { best, file: Some(file), browser: false }
    }

    /// The records this browser keeps for the page; new records are saved there.
    #[cfg(target_arch = "wasm32")]
    pub fn in_browser() -> Records {
        let text = local_storage().and_then(|s| s.get_item(STORAGE_KEY).ok().flatten()).unwrap_or_default();
        Records { best: parse(&text), file: None, browser: true }
    }

    pub fn best(&self, level: &str) -> Option<f64> {
        self.best.get(level).copied()
    }

    /// Records `secs` for `level` if it beats the best, and saves; true when it did. A record
    /// that could not be saved still counts for this session, and the error goes to stderr.
    pub fn offer(&mut self, level: &str, secs: f64) -> bool {
        if self.best(level).is_some_and(|b| b <= secs) {
            return false;
        }
        self.best.insert(level.to_string(), secs);
        if let Some(file) = &self.file
            && let Err(e) = self.save(file)
        {
            eprintln!("mines: could not save the records to {}: {e}", file.display());
        }
        if self.browser {
            self.save_browser();
        }
        true
    }

    fn text(&self) -> String {
        self.best.iter().map(|(k, v)| format!("{k}={v:.3}\n")).collect()
    }

    fn save(&self, file: &Path) -> std::io::Result<()> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(file, self.text())
    }

    #[cfg(target_arch = "wasm32")]
    fn save_browser(&self) {
        if local_storage().and_then(|s| s.set_item(STORAGE_KEY, &self.text()).ok()).is_none() {
            web_sys::console::warn_1(&"mines: could not save the records to localStorage".into());
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn save_browser(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder under the system temp folder, for one test.
    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mines-afloat-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn only_a_faster_time_counts() {
        let mut r = Records::default();
        assert!(r.offer("beginner", 30.0));
        assert!(!r.offer("beginner", 31.0));
        assert!(!r.offer("beginner", 30.0));
        assert!(r.offer("beginner", 29.5));
        assert_eq!(r.best("beginner"), Some(29.5));
        assert_eq!(r.best("expert"), None);
    }

    #[test]
    fn records_survive_a_restart() {
        let dir = scratch_dir("restart");
        let file = dir.join("nested").join("records.txt");
        let mut r = Records::load(file.clone());
        assert_eq!(r.best("beginner"), None, "no file yet: no records");
        assert!(r.offer("beginner", 41.25));
        assert!(r.offer("expert", 300.5));
        let again = Records::load(file);
        assert_eq!(again.best("beginner"), Some(41.25));
        assert_eq!(again.best("expert"), Some(300.5));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_record_that_cannot_be_saved_still_counts() {
        let dir = scratch_dir("unsaved");
        std::fs::create_dir_all(&dir).unwrap();
        // the file's folder is taken by a plain file, so saving fails
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, "").unwrap();
        let file = blocker.join("records.txt");
        let mut r = Records::load(file.clone());
        assert!(r.save(&file).is_err());
        assert!(r.offer("beginner", 12.0));
        assert_eq!(r.best("beginner"), Some(12.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lines_that_are_not_records_are_skipped() {
        let best = parse("beginner=12.5\nnonsense\nexpert = 99\nintermediate=abc\n");
        assert_eq!(best.len(), 2);
        assert_eq!(best["beginner"], 12.5);
        assert_eq!(best["expert"], 99.0);
    }

    #[test]
    fn the_file_is_in_the_platform_data_folder() {
        let file = default_file().expect("a home folder");
        assert!(file.ends_with(Path::new(APP_ID).join("records.txt")), "{}", file.display());
        if cfg!(windows) {
            let appdata = std::env::var_os("APPDATA").expect("APPDATA");
            assert!(file.starts_with(appdata), "{}", file.display());
        }
    }
}
