//! The world leaderboard in the game: `leaderboard/leaderboard.json` from the repository's
//! main branch, fetched in the background (a thread with `ureq` on desktop, the browser's
//! `fetch` on the web) so no frame ever waits on it. It is kept for the session and fetched
//! again at most every few minutes; after a failed fetch, sooner. Offline, or with anything
//! wrong with the answer, there is simply no world board: the game shows the player's own
//! records.

use mines_rules::leaderboard::Leaderboard;
use mines_rules::submit::LEADERBOARD_URL;

/// Seconds before the board is fetched again (raw.githubusercontent.com caches it five minutes).
const REFRESH: f64 = 300.0;
/// Seconds before a failed fetch is tried again; each failure in a row doubles it, up to
/// `REFRESH`.
const RETRY: f64 = 30.0;
/// The largest answer read, bytes.
const LIMIT: u64 = 1 << 20;
/// How long a fetch may take, seconds.
const TIMEOUT: u64 = 10;

/// A finished fetch's text (None: it failed), waiting for the app to take it.
#[cfg(not(target_arch = "wasm32"))]
type Inbox = std::sync::Arc<std::sync::Mutex<Option<Option<String>>>>;
#[cfg(target_arch = "wasm32")]
type Inbox = std::rc::Rc<std::cell::RefCell<Option<Option<String>>>>;

#[derive(Default)]
pub struct World {
    /// The last board fetched.
    pub board: Option<Leaderboard>,
    /// Whether this app fetches at all (not in headless runs and tests).
    online: bool,
    inbox: Inbox,
    pub in_flight: bool,
    /// When the last fetch started (`App::clock`).
    asked_at: Option<f64>,
    /// Fetches failed in a row since the last one that came through.
    failures: u32,
}

impl World {
    /// A world board that is fetched from GitHub.
    pub fn online() -> World {
        World { online: true, ..World::default() }
    }

    /// Starts a fetch at `clock` (seconds) unless one is running or the last began too
    /// recently: `REFRESH` after one that came through, `RETRY` (doubling) after a failure.
    pub fn refresh(&mut self, clock: f64) {
        if !self.online || self.in_flight || self.asked_at.is_some_and(|t| clock - t < self.wait()) {
            return;
        }
        self.in_flight = true;
        self.asked_at = Some(clock);
        fetch(self.inbox.clone());
    }

    /// Seconds from one fetch to the next.
    fn wait(&self) -> f64 {
        match self.failures {
            0 => REFRESH,
            n => (RETRY * 2f64.powi(n.min(8) as i32 - 1)).min(REFRESH),
        }
    }

    /// Takes a fetch that has finished; true when the board shown should change.
    pub fn poll(&mut self) -> bool {
        if !self.in_flight {
            return false;
        }
        #[cfg(not(target_arch = "wasm32"))]
        let done = self.inbox.lock().unwrap_or_else(|e| e.into_inner()).take();
        #[cfg(target_arch = "wasm32")]
        let done = self.inbox.borrow_mut().take();
        let Some(text) = done else { return false };
        self.in_flight = false;
        // a failed fetch or a file that doesn't parse keeps what was there
        match text.and_then(|t| Leaderboard::parse(&t).ok()) {
            Some(board) => {
                self.board = Some(board);
                self.failures = 0;
            }
            None => self.failures += 1,
        }
        true
    }

    /// Test and demo use: a board as if fetched.
    #[allow(dead_code)]
    pub fn set(&mut self, board: Leaderboard) {
        self.board = Some(board);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch(inbox: Inbox) {
    fn get() -> Result<String, ureq::Error> {
        let config = ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(TIMEOUT))).build();
        let agent: ureq::Agent = config.into();
        agent.get(LEADERBOARD_URL).call()?.body_mut().with_config().limit(LIMIT).read_to_string()
    }
    let answer = inbox.clone();
    let spawned = std::thread::Builder::new().name("leaderboard".into()).spawn(move || {
        // a panic in the client is one more failed fetch, not one that never ends
        let text = std::panic::catch_unwind(get).ok().and_then(Result::ok);
        *answer.lock().unwrap_or_else(|e| e.into_inner()) = Some(text);
    });
    if spawned.is_err() {
        // no thread, no fetch: the next `poll` takes it as a failed one
        *inbox.lock().unwrap_or_else(|e| e.into_inner()) = Some(None);
    }
}

#[cfg(target_arch = "wasm32")]
fn fetch(inbox: Inbox) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen_futures::JsFuture;
    wasm_bindgen_futures::spawn_local(async move {
        let text = async {
            let window = web_sys::window()?;
            // a fetch that hangs is aborted after TIMEOUT seconds, body and all
            let abort = web_sys::AbortController::new().ok()?;
            let init = web_sys::RequestInit::new();
            init.set_signal(Some(&abort.signal()));
            let stop = abort.clone();
            let timer = Closure::once_into_js(move || stop.abort());
            window.set_timeout_with_callback_and_timeout_and_arguments_0(timer.unchecked_ref(), (TIMEOUT * 1000) as i32).ok()?;
            let response: web_sys::Response =
                JsFuture::from(window.fetch_with_str_and_init(LEADERBOARD_URL, &init)).await.ok()?.dyn_into().ok()?;
            if !response.ok() {
                return None;
            }
            // read chunk by chunk, and stop past LIMIT bytes
            let reader: web_sys::ReadableStreamDefaultReader = response.body()?.get_reader().dyn_into().ok()?;
            let mut bytes = Vec::new();
            loop {
                let chunk: web_sys::ReadableStreamReadResult = JsFuture::from(reader.read()).await.ok()?.unchecked_into();
                if chunk.get_done().unwrap_or(true) {
                    break;
                }
                let part = js_sys::Uint8Array::new(&chunk.get_value());
                if bytes.len() as u64 + part.length() as u64 > LIMIT {
                    abort.abort();
                    return None;
                }
                bytes.extend(part.to_vec());
            }
            String::from_utf8(bytes).ok()
        }
        .await;
        *inbox.borrow_mut() = Some(text);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deliver(w: &mut World, text: Option<&str>) -> bool {
        w.in_flight = true;
        *w.inbox.lock().unwrap() = Some(text.map(str::to_string));
        w.poll()
    }

    #[test]
    fn a_fetch_lands_and_a_failure_keeps_what_was_there() {
        let mut w = World::default();
        w.refresh(0.0);
        assert!(!w.in_flight, "an offline world never fetches");
        assert!(!w.poll());
        assert!(deliver(&mut w, None) && w.board.is_none() && !w.in_flight, "offline: no board, no error");
        let mut board = Leaderboard::default();
        board.levels.get_mut("expert").unwrap().push(mines_rules::leaderboard::Entry {
            login: "ann".into(),
            time_ms: 99_000,
            date: "2026-11-16".into(),
            version: "0.3.0".into(),
            issue: 1,
            seed: "1".into(),
        });
        assert!(deliver(&mut w, Some(&board.to_json())));
        assert_eq!(w.board.as_ref(), Some(&board));
        assert!(deliver(&mut w, Some("<html>rate limited</html>")));
        assert_eq!(w.board.as_ref(), Some(&board), "a bad answer keeps the last board");
        assert!(deliver(&mut w, None));
        assert_eq!(w.board.as_ref(), Some(&board));
    }

    /// After a failed fetch the next comes sooner: 30 s, then 60, 120, 240, never over the
    /// usual five minutes; one that comes through goes back to every five minutes.
    #[test]
    fn a_failed_fetch_is_tried_again_soon() {
        let mut w = World { online: true, ..World::default() };
        let mut at = 0.0;
        for wait in [RETRY, 60.0, 120.0, 240.0, REFRESH, REFRESH] {
            w.asked_at = Some(at);
            assert!(deliver(&mut w, None));
            w.refresh(at + wait - 1.0);
            assert!(!w.in_flight, "not before {wait} s");
            assert_eq!(w.wait(), wait);
            at += wait;
        }
        assert!(deliver(&mut w, Some(&Leaderboard::default().to_json())));
        assert_eq!(w.wait(), REFRESH);
        assert!(deliver(&mut w, Some("not json")));
        assert_eq!(w.wait(), RETRY, "a broken answer counts as a failure");
    }

    #[test]
    fn an_online_world_fetches_at_most_every_few_minutes() {
        let mut w = World { online: true, ..World::default() };
        // pretend a fetch began at 10 s and has finished
        w.asked_at = Some(10.0);
        w.refresh(10.0 + REFRESH - 1.0);
        assert!(!w.in_flight);
        w.in_flight = true;
        w.refresh(10.0 + REFRESH + 1.0);
        assert_eq!(w.asked_at, Some(10.0), "not while one is running");
    }
}
