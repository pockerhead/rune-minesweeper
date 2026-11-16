//! The player's settings, kept like the records (`records.rs`): a small text file (`name=value`
//! per line) next to them in the user's data folder, or the page's `localStorage` in a browser.
//! Settings without a store (headless runs, tests) stay in memory. For now there is one: how
//! rough the sea is.

use std::path::{Path, PathBuf};

use crate::water::SeaLevel;

#[derive(Default)]
pub struct Settings {
    /// The sea level the player last chose; None if they never have.
    sea: Option<SeaLevel>,
    /// Where they are saved; None keeps them in memory.
    file: Option<PathBuf>,
    /// Saved in the browser's `localStorage` instead.
    browser: bool,
}

#[cfg(target_arch = "wasm32")]
const STORAGE_KEY: &str = "mines-afloat.settings";

#[cfg(target_arch = "wasm32")]
fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// The settings file in this user's data folder, beside the records.
#[cfg(not(target_arch = "wasm32"))]
pub fn default_file() -> Option<PathBuf> {
    crate::records::default_file().map(|records| records.with_file_name("settings.txt"))
}

fn parse(text: &str) -> Option<SeaLevel> {
    text.lines().filter_map(|line| line.split_once('=')).find(|(name, _)| name.trim() == "sea").and_then(|(_, v)| SeaLevel::from_key(v))
}

impl Settings {
    /// The settings in `file` (none chosen yet if it does not exist); changes are saved there.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load(file: PathBuf) -> Settings {
        let sea = match std::fs::read_to_string(&file) {
            Ok(text) => parse(&text),
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("mines: could not read the settings in {}: {e}", file.display());
                }
                None
            }
        };
        Settings { sea, file: Some(file), browser: false }
    }

    /// The settings this browser keeps for the page; changes are saved there.
    #[cfg(target_arch = "wasm32")]
    pub fn in_browser() -> Settings {
        let text = local_storage().and_then(|s| s.get_item(STORAGE_KEY).ok().flatten()).unwrap_or_default();
        Settings { sea: parse(&text), file: None, browser: true }
    }

    pub fn sea(&self) -> Option<SeaLevel> {
        self.sea
    }

    /// Chooses `level` and saves. A choice that could not be saved still holds for this
    /// session, and the error goes to stderr.
    pub fn set_sea(&mut self, level: SeaLevel) {
        self.sea = Some(level);
        if let Some(file) = &self.file
            && let Err(e) = self.save(file)
        {
            eprintln!("mines: could not save the settings to {}: {e}", file.display());
        }
        if self.browser {
            self.save_browser();
        }
    }

    fn text(&self) -> String {
        self.sea.map_or(String::new(), |s| format!("sea={}\n", s.key()))
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
            web_sys::console::warn_1(&"mines: could not save the settings to localStorage".into());
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn save_browser(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sea_level_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("mines-afloat-test-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("nested").join("settings.txt");
        for level in SeaLevel::ALL {
            let mut s = Settings::load(file.clone());
            s.set_sea(level);
            assert_eq!(Settings::load(file.clone()).sea(), Some(level));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_file_or_a_bad_line_is_no_choice() {
        let dir = std::env::temp_dir().join(format!("mines-afloat-test-settings-none-{}", std::process::id()));
        assert_eq!(Settings::load(dir.join("settings.txt")).sea(), None);
        assert_eq!(parse("sea=stormy\n"), None);
        assert_eq!(parse("nonsense\nsea = calm\n"), Some(SeaLevel::Calm));
    }

    #[test]
    fn the_file_sits_beside_the_records() {
        let file = default_file().expect("a home folder");
        assert_eq!(file.parent(), crate::records::default_file().expect("a home folder").parent());
        assert!(file.ends_with("settings.txt"));
    }
}
