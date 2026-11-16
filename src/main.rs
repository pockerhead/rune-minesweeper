//! Mines Afloat: minesweeper on water. Rune draws the UI and hosts the 3D world in an
//! `El::effect` slot; oxijolt floats the tiles; `ripple` makes the water answer the cursor.
//! The menu is a world of its own (`lobby`): its items are floating tiles with Rune labels on
//! them, and picking one blows its tile up like a mine.
//!
//! This file holds the app's state and menu flow, the Rune view and the compositor that draws
//! the 3D scene (`scene`, `gfx`) into the view's slot.

mod boat;
mod camera;
mod game;
mod gfx;
mod lobby;
mod pinch;
mod records;
mod ripple;
mod scene;
mod sea;
mod water;

use glam::{Vec2, Vec3};
use rune::compose::{Compositor, Frame};
use rune::tok::{self, TextStyle};
use rune::{
    AppSpec, Color, Cx, Drag, El, Key, KeyEvent, MagnifyEvent, Phase, PointerButton, PointerPress, Tx, WheelEvent, col, el, row, text,
};

use camera::{Stage, View};
use game::{Dims, Game, Status};
use gfx::Gpu;
use lobby::{Action, Lobby};
use pinch::Touches;
use records::Records;

const BOARD_MARK: u32 = 1;
/// How long a picked menu tile's blast plays before the next screen.
const PICK_DELAY: f64 = 1.1;
/// How long a finished game holds before a click returns to the menu.
const END_HOLD: f64 = 1.2;
/// A menu label fades out over the first part of its tile's melt and is gone after it.
const LABEL_FADE: f32 = 0.6;
/// Points a menu label never shrinks under, however far out the menu is zoomed.
const MIN_LABEL: f32 = 12.0;
/// The top bar's row height, and the window width from which its hint fits in the row.
const BAR: f32 = 50.5;
const ONE_ROW: f32 = 1000.0;

struct Level {
    key: &'static str,
    name: &'static str,
    dims: Dims,
    mines: usize,
}

const LEVELS: [Level; 3] = [
    Level { key: "beginner", name: "Beginner", dims: Dims { w: 9, h: 9 }, mines: 10 },
    Level { key: "intermediate", name: "Intermediate", dims: Dims { w: 16, h: 16 }, mines: 40 },
    Level { key: "expert", name: "Expert", dims: Dims { w: 30, h: 16 }, mines: 99 },
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Menu {
    Main,
    Levels,
    Custom,
    Records,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Screen {
    Menu(Menu),
    Game,
}

/// Where a picked menu item leads, once its blast has played.
#[derive(Clone, Copy, Debug)]
enum Next {
    Menu(Menu),
    /// A game: the level's records key (None for a custom board), size and mines.
    Start(Option<&'static str>, Dims, usize),
}

/// A menu label as the view draws it: on its tile's top face, fading as the tile melts.
struct Label<'a> {
    key: usize,
    text: &'a str,
    /// Where it sits on the board element, points.
    x: f32,
    y: f32,
    size: f32,
    /// How far its tile has melted, 0 to `LABEL_FADE`.
    fade: f32,
}

pub struct App {
    screen: Screen,
    lobby: Lobby,
    game: Option<Game>,
    /// The records key of the game being played (None: a custom board, not recorded).
    level: Option<&'static str>,
    /// The custom game: width, height, mines.
    custom: (usize, usize, usize),
    records: Records,
    pending: Option<(Next, f64)>,
    started: f64,
    finished: Option<f64>,
    new_record: bool,
    seed: u64,
    round: u64,
    /// The cell under the pointer in a game.
    hover: Option<usize>,
    /// The pointer in window points, while it is over the board.
    pointer: Option<(f32, f32)>,
    /// The board element's rect in window points, from layout.
    board: [f32; 4],
    /// Frames counted since the shown value was updated.
    fps_frames: u32,
    fps_time: f64,
    fps: u32,
    /// `Runtime::clock` as of the last frame.
    clock: f64,
    /// What the view drew last from moving state (see `App::moving_key`).
    shown_key: u64,
    /// A press being held (clock when it went down, where): held still long enough, it plants a
    /// flag, so touch screens without a right button can flag too.
    held: Option<(f64, (f32, f32))>,
    /// The held press already planted its flag: its release opens nothing.
    held_flagged: bool,
    /// The render scale of the last frame, and the one the frame counter shows.
    render_scale: f32,
    shown_scale: f32,
    /// How the player has zoomed and panned this screen's board (`App::stage` keeps it within
    /// limits). Set afresh for each game and menu.
    view: View,
    /// Whether the board was wider than tall when `view` was set (None: not set for a laid-out
    /// board yet). Turning the screen sets it afresh.
    view_landscape: Option<bool>,
    /// The water point the current press grabbed, and whether it is panning: a middle-button
    /// press pans from the start, a primary one once it wanders past `LONG_PRESS_SLOP`.
    grab: Option<Vec2>,
    panning: bool,
    /// Steps a held press on a custom game value has repeated.
    repeats: u32,
    /// The page's fingers as last read: two zoom and pan the board.
    touches: Touches,
}

/// Seconds a still press takes to plant a flag (or, on a custom game value, to start repeating).
const LONG_PRESS: f64 = 0.45;
/// Points a press may wander and still count as held still.
const LONG_PRESS_SLOP: f32 = 12.0;
/// A held custom game value steps every so often, faster the longer it is held.
const REPEAT_FIRST: f64 = 0.12;
const REPEAT_FASTEST: f64 = 0.03;
/// A game whose cells show under this many points across opens zoomed in to `START_CELL`.
const START_BELOW: f32 = 16.0;
const START_CELL: f32 = 28.0;
/// A menu whose tiles show under this many points deep opens zoomed in on its column of tiles.
const MENU_START_BELOW: f32 = 40.0;
/// The menu's column of tiles, half-extents about its middle (five tiles, as the custom game has).
const MENU_COLUMN: (f32, f32) = (2.5, 2.9);
const MENU_COLUMN_Z: f32 = 0.3;

/// Something different every launch to seed the boards with.
fn wall_clock_seed() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        // the browser has no system clock for std; ask JavaScript
        (js_sys::Date::now() * 1000.0) as u64 ^ (js_sys::Math::random() * u32::MAX as f64) as u64
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0x5eed, |d| d.as_nanos() as u64)
    }
}

fn init() -> App {
    // a new world every launch; headless runs (snapshots, scripts, scenes) stay reproducible
    // and never touch the records file
    let headless = rune::is_headless();
    let seed = if headless { 0x5eed } else { wall_clock_seed() };
    #[cfg(target_arch = "wasm32")]
    let records = if headless { Records::default() } else { Records::in_browser() };
    #[cfg(not(target_arch = "wasm32"))]
    let records = match records::default_file() {
        Some(file) if !headless => Records::load(file),
        _ => Records::default(),
    };
    App::new(seed, records)
}

fn fmt_time(secs: f64) -> String {
    format!("{secs:.1} s")
}

impl App {
    fn new(seed: u64, records: Records) -> App {
        let mut app = App {
            screen: Screen::Menu(Menu::Main),
            lobby: Lobby::new(seed),
            game: None,
            level: None,
            custom: (12, 12, 24),
            records,
            pending: None,
            started: 0.0,
            finished: None,
            new_record: false,
            seed,
            round: 0,
            hover: None,
            pointer: None,
            board: [0.0, 0.0, 1.0, 1.0],
            fps_frames: 0,
            fps_time: 0.0,
            fps: 0,
            clock: 0.0,
            shown_key: 0,
            held: None,
            held_flagged: false,
            render_scale: 1.0,
            shown_scale: 1.0,
            view: View::default(),
            view_landscape: None,
            grab: None,
            panning: false,
            repeats: 0,
            touches: Touches::default(),
        };
        let items = app.menu_items(Menu::Main);
        app.lobby.set_menu(items);
        app
    }

    fn custom_label(&self, k: usize) -> String {
        let (w, h, m) = self.custom;
        match k {
            0 => format!("‹    Width  {w}    ›"),
            1 => format!("‹    Height  {h}    ›"),
            _ => format!("‹    Mines  {m}    ›"),
        }
    }

    fn menu_items(&self, menu: Menu) -> Vec<(String, Action)> {
        match menu {
            Menu::Main => vec![("Play".into(), Action::Play), ("Custom game".into(), Action::Custom), ("Records".into(), Action::Records)],
            Menu::Levels => {
                let mut v: Vec<(String, Action)> = LEVELS
                    .iter()
                    .enumerate()
                    .map(|(k, l)| (format!("{}  ·  {}×{}  ·  {}", l.name, l.dims.w, l.dims.h, l.mines), Action::Level(k)))
                    .collect();
                v.push(("Back".into(), Action::Back));
                v
            }
            Menu::Custom => vec![
                (self.custom_label(0), Action::Adjust(0)),
                (self.custom_label(1), Action::Adjust(1)),
                (self.custom_label(2), Action::Adjust(2)),
                ("Start".into(), Action::Start),
                ("Back".into(), Action::Back),
            ],
            Menu::Records => {
                let mut v: Vec<(String, Action)> = LEVELS
                    .iter()
                    .map(|l| {
                        let best = self.records.best(l.key).map_or("no record yet".to_string(), fmt_time);
                        (format!("{}   ·   {best}", l.name), Action::Show)
                    })
                    .collect();
                v.push(("Back".into(), Action::Back));
                v
            }
        }
    }

    /// This screen's board as the camera sees it: the fit frames the game's board (or the
    /// menu's grid), and a pan reaches a little past the board's edge (or the menu's grid).
    fn stage(&self) -> Stage {
        let (fit, area) = match (&self.screen, &self.game) {
            (Screen::Game, Some(g)) => {
                let (hx, hz) = g.dims.half();
                ((hx, hz), (hx + 0.5, hz + 0.5))
            }
            _ => (lobby::GRID.half(), lobby::GRID.half()),
        };
        Stage { rect: self.board, fit, area }
    }

    /// The camera over this screen's board: the one the scene is drawn with and picking,
    /// labels and the cursor light use.
    fn camera(&self) -> gfx::Camera {
        self.stage().camera(self.view)
    }

    /// The water point under window point (x, y), if it is over the board element.
    fn water_at(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        let stage = self.stage();
        stage.water_at(&stage.camera(self.view), x, y).map(|w| (w.x, w.y))
    }

    /// What the compositor draws this frame, through `App::camera`.
    fn scene(&self) -> gfx::Scene {
        let cam = self.camera();
        match (self.screen, &self.game) {
            (Screen::Game, Some(g)) => scene::build_game(g, self.hover, &cam),
            _ => scene::build_lobby(&self.lobby, &cam),
        }
    }

    /// The view a screen opens with. A board that shows too small to tap (a big board on a
    /// phone) opens zoomed in on its middle; a menu whose tiles show too small opens framing
    /// its column of tiles. Pinching out shows the rest.
    fn start_view(&self) -> View {
        let stage = self.stage();
        match self.screen {
            Screen::Game => {
                let cell = stage.unit_points();
                if cell < START_BELOW { stage.zoomed(START_CELL / cell) } else { View::default() }
            }
            Screen::Menu(_) => {
                let cam = stage.camera(View::default());
                let near = cam.project(Vec3::new(0.0, 0.0, MENU_COLUMN_Z + lobby::ITEM[2] * 0.5));
                let far = cam.project(Vec3::new(0.0, 0.0, MENU_COLUMN_Z - lobby::ITEM[2] * 0.5));
                if (near.y - far.y) * self.board[3] >= MENU_START_BELOW {
                    return View::default();
                }
                let shows = stage.fit_extent();
                let zoom = (shows.x / (MENU_COLUMN.0 * 2.0)).min(shows.y / (MENU_COLUMN.1 * 2.0));
                stage.centred(stage.zoomed(zoom), Vec2::new(0.0, MENU_COLUMN_Z))
            }
        }
    }

    /// Sets the view afresh for this screen and the board's current layout.
    fn reset_view(&mut self) {
        let [_, _, bw, bh] = self.board;
        self.view_landscape = (bw > 1.0 && bh > 1.0).then_some(bw >= bh);
        self.view = self.start_view();
        self.grab = None;
        self.panning = false;
    }

    /// A wheel turn over the board zooms about the pointer; a pinch on a trackpad (Ctrl + wheel
    /// off macOS) comes as `on_magnify` instead.
    fn wheel(&mut self, e: &WheelEvent) {
        let [bx, by, _, _] = self.board;
        self.zoom_at(bx + e.x, by + e.y, (e.dy * 0.0025).clamp(-0.35, 0.35).exp());
    }

    fn magnify(&mut self, e: &MagnifyEvent) {
        let [bx, by, _, _] = self.board;
        self.zoom_at(bx + e.x, by + e.y, 1.0 + e.delta);
    }

    /// Zooms by `factor` about window point (x, y).
    fn zoom_at(&mut self, x: f32, y: f32, factor: f32) {
        self.view = self.stage().zoom_about(self.view, x, y, factor);
        self.aim(false);
    }

    /// Reads the page's fingers (in a browser; elsewhere there are none to read).
    fn sync_touches(&mut self) {
        if cfg!(target_arch = "wasm32") {
            self.touch(pinch::read());
        }
    }

    /// The page's fingers. Two zoom about their middle as they spread or close and pan as it
    /// moves. A pinch also cancels the press its first finger began (Rune still reports that
    /// finger as a press): it neither opens a cell nor plants a flag, nor pans by itself.
    fn touch(&mut self, t: Touches) {
        let last = std::mem::replace(&mut self.touches, t);
        if t.pinched {
            self.held = None;
            self.grab = None;
            self.panning = false;
        }
        if t.fingers >= 2 && last.fingers >= 2 && last.generation == t.generation && last.spread > 0.0 {
            let stage = self.stage();
            let zoomed = stage.zoom_about(self.view, last.centre.x, last.centre.y, t.spread / last.spread);
            // the water under the fingers' old middle follows them to the new one
            self.view = match stage.under(zoomed, last.centre.x, last.centre.y) {
                Some(grab) => stage.drag(zoomed, grab, t.centre.x, t.centre.y),
                None => zoomed,
            };
            self.aim(false);
        }
    }

    fn pointer_moved(&mut self, p: Option<PointerPress>) {
        self.pointer = p.map(|p| (p.x, p.y));
        self.aim(false);
    }

    /// Points the active screen's cursor (and in a game, `hover`) at what is under the pointer.
    /// `warp`: the screen changed under a still pointer, so the cursor jumps without a wake.
    fn aim(&mut self, warp: bool) {
        let w = self.pointer.and_then(|(x, y)| self.water_at(x, y));
        let (sea, hover) = match (self.screen, self.game.as_mut()) {
            (Screen::Game, Some(g)) => (&mut g.sea, w.and_then(|(x, z)| g.dims.cell_at(x, z))),
            _ => (&mut self.lobby.sea, None),
        };
        if warp {
            sea.warp_cursor(w);
        } else {
            sea.cursor = w;
        }
        self.hover = hover;
    }

    fn drag(&mut self, d: &Drag, button: PointerButton) {
        let [x, y] = d.window_point();
        self.press_event(d.phase, (x, y), d.cancelled(), button);
    }

    /// Down, move and up of any press on the board, mouse or touch, at window point (x, y).
    /// The press is aimed where it went down (a touch has no hover before it). Released where
    /// it went down it is a click; held still for `LONG_PRESS` it plants a flag instead (touch
    /// screens have no right button); moved past `LONG_PRESS_SLOP` it pans the board and
    /// clicks nothing. A middle-button press only pans.
    fn press_event(&mut self, phase: Phase, (x, y): (f32, f32), cancelled: bool, button: PointerButton) {
        // a second finger may have landed since the last frame: the press is then a pinch's,
        // and reading the fingers now cancels it before this event can click or pan
        self.sync_touches();
        self.pointer_moved(Some(PointerPress { x, y }));
        match phase {
            Phase::Down => {
                let primary = button == PointerButton::Primary;
                self.held = primary.then_some((self.clock, (x, y)));
                self.held_flagged = false;
                self.repeats = 0;
                self.grab = self.stage().under(self.view, x, y);
                self.panning = !primary;
            }
            Phase::Move => {
                if self.held.is_some_and(|(_, (x0, y0))| (x - x0).hypot(y - y0) > LONG_PRESS_SLOP) {
                    self.held = None;
                    self.panning = true;
                }
                if self.panning
                    && let Some(grab) = self.grab
                {
                    self.view = self.stage().drag(self.view, grab, x, y);
                    self.aim(false);
                }
            }
            Phase::Up => {
                let still = self.held.take().is_some();
                (self.grab, self.panning) = (None, false);
                if still && !cancelled && !std::mem::take(&mut self.held_flagged) {
                    self.press();
                }
            }
        }
    }

    /// A press held still for `LONG_PRESS`: in a game it plants a flag, once; on a custom game
    /// value it keeps stepping the value until it lets go.
    fn watch_hold(&mut self) {
        let Some((at, (x, y))) = self.held else { return };
        if self.clock - at < LONG_PRESS {
            return;
        }
        match self.screen {
            Screen::Game if !self.held_flagged => {
                self.held_flagged = true;
                self.secondary_press(PointerPress { x, y });
            }
            Screen::Game => {}
            Screen::Menu(_) => self.repeat_adjust(at),
        }
    }

    /// A press held on a custom game value steps it on its own, faster as it goes on; its
    /// release then steps nothing more. Held on any other item it is a plain press.
    fn repeat_adjust(&mut self, at: f64) {
        if self.pending.is_some() {
            return;
        }
        let Some((x, z)) = self.lobby.sea.cursor else { return };
        let Some((k, along)) = self.lobby.item_at(x, z) else { return };
        let Action::Adjust(value) = self.lobby.tiles[k].action else { return };
        let due = at + LONG_PRESS + (0..self.repeats).map(|n| (REPEAT_FIRST * 0.9f64.powi(n as i32)).max(REPEAT_FASTEST)).sum::<f64>();
        if self.clock < due {
            return;
        }
        if self.repeats == 0 {
            self.lobby.poke(k);
        }
        self.adjust(value, along > 0.0);
        self.repeats += 1;
        self.held_flagged = true;
    }

    fn press(&mut self) {
        match self.screen {
            Screen::Game => {
                let Some(g) = self.game.as_mut() else { return };
                if g.status != Status::Playing {
                    if self.finished.is_some_and(|t| self.clock - t > END_HOLD) {
                        self.open_menu(Menu::Main);
                    }
                    return;
                }
                if let Some(i) = self.hover {
                    g.click(i);
                }
            }
            Screen::Menu(_) => self.press_menu(),
        }
    }

    fn press_menu(&mut self) {
        if self.pending.is_some() {
            return;
        }
        let Some((x, z)) = self.lobby.sea.cursor else { return };
        let Some((k, along)) = self.lobby.item_at(x, z) else {
            self.lobby.sail(x, z);
            return;
        };
        let next = match self.lobby.tiles[k].action {
            Action::Play => Next::Menu(Menu::Levels),
            Action::Custom => Next::Menu(Menu::Custom),
            Action::Records => Next::Menu(Menu::Records),
            Action::Back => Next::Menu(Menu::Main),
            Action::Level(l) => Next::Start(Some(LEVELS[l].key), LEVELS[l].dims, LEVELS[l].mines),
            Action::Start => {
                let (w, h, m) = self.custom;
                Next::Start(None, Dims { w, h }, m)
            }
            Action::Adjust(v) => {
                self.adjust(v, along > 0.0);
                self.lobby.poke(k);
                return;
            }
            Action::Show => {
                self.lobby.poke(k);
                return;
            }
        };
        self.lobby.blow(k);
        self.pending = Some((next, self.clock + PICK_DELAY));
    }

    fn secondary_press(&mut self, p: PointerPress) {
        self.pointer_moved(Some(p));
        if let (Screen::Game, Some(i), Some(g)) = (self.screen, self.hover, self.game.as_mut()) {
            g.toggle_flag(i);
        }
    }

    /// The custom game's width (0), height (1) or mines (2) one up or down; the labels follow.
    fn adjust(&mut self, value: usize, up: bool) {
        let (w, h, m) = &mut self.custom;
        let step = |x: &mut usize, lo: usize, hi: usize| *x = if up { (*x + 1).min(hi) } else { x.saturating_sub(1).max(lo) };
        // never more mines than the board leaves room for around a first click
        let most = |w: usize, h: usize| w * h - 9;
        match value {
            0 => step(w, 5, 30),
            1 => step(h, 5, 16),
            _ => step(m, 1, most(*w, *h)),
        }
        *m = (*m).min(most(*w, *h)).max(1);
        for k in 0..self.lobby.tiles.len() {
            if let Action::Adjust(v) = self.lobby.tiles[k].action {
                let label = self.custom_label(v);
                self.lobby.relabel(k, label);
            }
        }
    }

    fn open_menu(&mut self, menu: Menu) {
        self.game = None;
        self.pending = None;
        self.screen = Screen::Menu(menu);
        let items = self.menu_items(menu);
        self.lobby.set_menu(items);
        self.reset_view();
        self.aim(true);
    }

    /// Runs what a picked item asked for once its blast has played.
    fn settle(&mut self) {
        let Some((next, at)) = self.pending else { return };
        if self.clock < at {
            return;
        }
        self.pending = None;
        match next {
            Next::Menu(m) => self.open_menu(m),
            Next::Start(level, dims, mines) => {
                self.round += 1;
                self.game = Some(Game::with(dims, mines, self.seed ^ self.round.wrapping_mul(0x9e37_79b9_7f4a_7c15)));
                self.level = level;
                self.screen = Screen::Game;
                self.started = self.clock;
                self.finished = None;
                self.new_record = false;
                self.reset_view();
                self.aim(true);
            }
        }
    }

    /// Notes the end of a game (and a record, on a win).
    fn watch_end(&mut self) {
        let Some(g) = &self.game else { return };
        if g.status == Status::Playing || self.finished.is_some() {
            return;
        }
        self.finished = Some(self.clock);
        if g.status == Status::Won
            && let Some(level) = self.level
        {
            self.new_record = self.records.offer(level, self.clock - self.started);
        }
    }

    /// One frame of the world: `dt` seconds later, at `clock`. The world steps as frames are
    /// drawn (the compositor calls this), so it pauses with the window's drawing.
    fn update(&mut self, clock: f64, dt: f64) {
        self.clock = clock;
        // the first layout of the board, or the screen turned: frame it afresh
        let [_, _, bw, bh] = self.board;
        if bw > 1.0 && bh > 1.0 && self.view_landscape != Some(bw >= bh) {
            self.reset_view();
            self.aim(true);
        }
        self.sync_touches();
        match (self.screen, self.game.as_mut()) {
            (Screen::Game, Some(g)) => g.advance(dt as f32),
            _ => self.lobby.advance(dt as f32),
        }
        self.watch_hold();
        self.watch_end();
        self.settle();
        self.fps_frames += 1; // a browser replaces this with the GPU's finished frames
        self.fps_time += dt;
    }

    /// The menu's labels as they land on their tiles this frame: projected from each tile's
    /// top face, all at one face size, small enough for the longest label to stay on its tile.
    fn menu_labels(&self) -> Vec<Label<'_>> {
        let l = &self.lobby;
        let [_, _, bw, bh] = self.board;
        let stage = self.stage();
        let zoom = stage.clamp(self.view).zoom;
        let cam = stage.camera(self.view);
        let tile_px = (cam.project(Vec3::X * lobby::ITEM[0] * 0.5).x - cam.project(-Vec3::X * lobby::ITEM[0] * 0.5).x) * bw * 0.86;
        let depth_px = (cam.project(Vec3::Z * lobby::ITEM[2] * 0.5).y - cam.project(-Vec3::Z * lobby::ITEM[2] * 0.5).y) * bh;
        let longest = l.tiles.iter().map(|t| t.label.chars().count()).max().unwrap_or(1).max(1) as f32;
        // grows with the zoom, never past the tile's depth or width, and never too small to read
        let size = (bh / 30.0 * zoom).clamp(14.0, 26.0 * zoom).min(depth_px * 0.56).min(tile_px / (longest * 0.56)).max(MIN_LABEL);
        let mut labels = Vec::new();
        for (k, tile) in l.tiles.iter().enumerate() {
            let fade = l.sea.fade_of(&tile.body);
            if tile.blown || fade > LABEL_FADE {
                continue;
            }
            let Some((pos, _)) = l.sea.draw_pose(&tile.body) else { continue };
            let at = cam.project(pos + Vec3::Y * lobby::ITEM[1] * 0.5);
            labels.push(Label { key: k, text: &tile.label, x: at.x * bw, y: at.y * bh, size, fade });
        }
        labels
    }

    /// A digest of what the view draws from moving state: each menu label (to the half point,
    /// its fade in twentieths) and whether the end banner offers the way back yet. The view
    /// only needs rebuilding when it changes.
    fn moving_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::mem::discriminant(&self.screen).hash(&mut h);
        if let Screen::Menu(_) = self.screen {
            for label in self.menu_labels() {
                (label.key, label.text, label.size.to_bits()).hash(&mut h);
                ((label.x * 2.0) as i32, (label.y * 2.0) as i32, (label.fade * 20.0) as i32).hash(&mut h);
            }
        }
        self.finished.map(|f| self.clock - f > END_HOLD).hash(&mut h);
        h.finish()
    }
}

/// A label printed on a floating tile: dark ink with a pale lower edge, as if pressed in.
fn tile_label(label: &Label<'_>) -> El {
    let (size, alpha) = (label.size, 1.0 - label.fade / LABEL_FADE);
    let font = TextStyle::new(size, 750.0);
    let (w, h) = (size * 18.0, size * 2.0);
    let ink = rune::color::hex("#2b1d10").unwrap_or(Color::BLACK).alpha(alpha);
    let lit = Color::WHITE.alpha(0.5 * alpha);
    el().key(format!("item{}", label.key))
        .absolute(label.x - w * 0.5, label.y - h * 0.5)
        .size(w, h)
        .child(el().absolute(0.0, 1.2).size(w, h).center().child(text(label.text.to_string()).font(font).color(lit)))
        .child(el().absolute(0.0, 0.0).size(w, h).center().child(text(label.text.to_string()).font(font).color(ink)))
}

fn view(s: &App, cx: &mut Cx<App>) -> El {
    let t = tok::theme(true);
    let [_, _, bw, _] = s.board;
    // a touch screen gets touch wording; a notch or a status bar keeps controls clear of it
    let touch = cx.hover_none();
    let [top_inset, right_inset, _, left_inset] = cx.safe_area();

    let mut board = el()
        .key("board")
        .w_full()
        .grow(1.0)
        .effect(BOARD_MARK)
        .on_drawn_layout(|s: &mut App, r| s.board = r)
        .on_pointer_move(|s: &mut App, p: PointerPress| s.pointer_moved(Some(p)))
        .on_hover_change(|s: &mut App, inside: bool, _| {
            if !inside {
                s.pointer_moved(None);
            }
        })
        // presses come through `drag` (down, hold, up): a finger is aimed where it lands and
        // can be held for a flag or dragged to pan; `touch_drags_at_once` hands it over without
        // the page's own long-press and slop. The middle button only pans. Two fingers come
        // from the page (`pinch`), which Rune does not report.
        .pointer_buttons([PointerButton::Middle])
        .on_drag(|s: &mut App, d: &mut Drag, tx: &mut Tx| s.drag(d, tx.button()))
        .touch_drags_at_once()
        .on_wheel(|s: &mut App, e: WheelEvent, _| {
            s.wheel(&e);
            true
        })
        .on_magnify(|s: &mut App, e: MagnifyEvent, _| {
            s.magnify(&e);
            true
        })
        .on_secondary_press(|s: &mut App, p: PointerPress, _| s.secondary_press(p));

    match (s.screen, &s.game) {
        (Screen::Menu(_), _) => {
            // labels ride their tiles: projected from each tile's pose every frame
            for label in s.menu_labels() {
                board = board.child(tile_label(&label));
            }
        }
        (Screen::Game, Some(g)) if g.status != Status::Playing => {
            let since = s.finished.map_or(0.0, |f| s.clock - f);
            let (line, color) = match g.status {
                Status::Lost => ("Boom. The boat is gone.".to_string(), "#ff6b4a"),
                _ => {
                    let secs = s.finished.unwrap_or(s.clock) - s.started;
                    let mut m = format!("Cleared in {}", fmt_time(secs));
                    if s.new_record {
                        m.push_str("  ·  new record!");
                    }
                    (m, "#5cf0a0")
                }
            };
            let c = rune::color::hex(color).unwrap_or(Color::WHITE);
            // 400 points wide, or the board's width less a margin (and any notch) on a phone
            let room = (bw - left_inset - right_inset - 24.0).max(1.0);
            let w = room.min(400.0);
            let mut banner = col()
                .key("banner")
                .absolute(left_inset + 12.0 + (room - w) * 0.5, 18.0)
                .size(w, if since > END_HOLD { 64.0 } else { 44.0 })
                .radius(18.0)
                .fill(Color::BLACK.alpha(0.5))
                .center()
                .gap(4.0)
                .child(text(line).font(TextStyle::new(17.0, 700.0)).color(c));
            if since > END_HOLD {
                let back = if touch { "Tap to return to the menu" } else { "Click to return to the menu" };
                banner = banner.child(text(back).font(TextStyle::new(12.5, 500.0)).color(Color::WHITE.alpha(0.7)));
            }
            board = board.child(banner);
        }
        _ => {}
    }

    // a wide window keeps one row; a narrow one (a phone) puts the hint on a line of its own
    let one_row = bw >= ONE_ROW;
    let hint = if touch {
        "Tap to sail  ·  hold to flag  ·  pinch to zoom, drag to pan"
    } else {
        "Click the water to sail  ·  right-click or long-press to flag  ·  wheel to zoom, drag to pan  ·  Esc for the menu"
    };
    let hint = || text(hint).font(TextStyle::new(12.0, 500.0)).color(t.text.alpha(0.55));
    let fps = match (cfg!(target_arch = "wasm32"), one_row) {
        (true, true) => format!("{} FPS · {:.2}x", s.fps, s.shown_scale),
        (true, false) => format!("{} · {:.2}x", s.fps, s.shown_scale),
        _ => format!("{} FPS", s.fps),
    };
    let mut top = row()
        .w_full()
        .h(BAR)
        .gap(if one_row { 18.0 } else { 10.0 })
        .items_center()
        .child(text("Mines Afloat").font(TextStyle::new(18.0, 700.0)).color(t.text))
        .child(el().grow(1.0));
    if one_row {
        top = top.child(hint());
    }
    top = top.child(text(fps).font(TextStyle::new(13.0, 600.0).mono()).color(rune::color::hex("#7fe0c0").unwrap_or(t.text)));
    if s.screen == Screen::Game {
        top = top.child(menu_button(t.text));
    }
    // the bar's fill runs to the screen's edges, its content stays clear of a notch
    let mut bar = col().w_full().pt(top_inset).pl(14.0 + left_inset).pr(14.0 + right_inset).child(top);
    if !one_row {
        bar = bar.pb(10.0).child(hint().w_full().wrap());
    }
    col().key("root").w_full().h_full().fill(rune::color::hex("#0b1419").unwrap_or(t.bg)).child(bar).child(board)
}

/// The way back to the menu from a game: a pill in the bar with a finger-sized target round it.
fn menu_button(ink: Color) -> El {
    el().key("menu").h(44.0).center().on_press(|s: &mut App| s.open_menu(Menu::Main)).child(
        el().h(30.0)
            .px(14.0)
            .radius(15.0)
            .fill(Color::WHITE.alpha(0.1))
            .center()
            .child(text("Menu").font(TextStyle::new(13.0, 600.0)).color(ink)),
    )
}

/// Steps the world and draws it into the board's effect slot.
#[derive(Default)]
struct SceneCompositor {
    gpu: Option<Gpu>,
    /// `Runtime::clock` at the last frame. The clock is the wall in a window and virtual time
    /// in headless runs, so a recorded video plays at the speed the game really runs;
    /// `Frame::dt` is wall time even headless.
    last_clock: Option<f64>,
    /// A scene has been drawn into the GPU's offscreen at least once.
    drawn: bool,
}

impl Compositor<App> for SceneCompositor {
    fn render(&mut self, f: &mut Frame<'_, App>) {
        let clock = f.rt.clock;
        let dt = self.last_clock.map_or(0.0, |c| (clock - c).max(0.0));
        self.last_clock = Some(clock);
        f.rt.state.update(clock, dt);

        let root = f.rt.presented("root").map(|r| r.0).unwrap_or([0.0, 0.0, 1.0, 1.0]);
        let board = f.rt.presented("board").map(|r| r.0).unwrap_or(root);
        let scale = f.size.0 as f32 / root[2].max(1.0);
        let viewport = [board[0] * scale, board[1] * scale, board[2] * scale, board[3] * scale];
        // drawn through the camera picking uses (`App::camera`), from the board's layout rect
        let scene = f.rt.state.scene();

        if self.gpu.as_ref().is_none_or(|g| g.key() != f.device_key()) {
            self.gpu = Some(Gpu::new(f));
        }
        let gpu = self.gpu.as_mut().expect("gpu");
        // with no scene drawn yet there is nothing to show again, so a capped frame draws too
        if gpu.scene_due() || !self.drawn {
            gpu.render(f, &scene, viewport);
            self.drawn = true;
        }
        f.rt.state.render_scale = gpu.scale();
        // in a browser count the frames the GPU finished, not the ones handed to it
        if let Some(finished) = gpu.finished_frames() {
            let s = &mut f.rt.state;
            s.fps_frames = s.fps_frames.saturating_sub(1) + finished;
        }

        let clear = f.rt.clear_color();
        f.ui.prepare(f.device, f.queue, &mut f.rt.text.atlas, &f.rt.instances, f.size);
        let gpu = &*gpu;
        f.ui.draw_with(f.encoder, f.target, clear, &mut |pass, site| {
            if site.mark == BOARD_MARK {
                gpu.blit(pass, site, viewport);
            }
        });
    }

    fn animating(&self) -> bool {
        true
    }
}

/// The view is rebuilt when a menu label moves or fades, when the end banner offers the way
/// back, and when the frame counter's value changes (checked twice a second).
fn poll(s: &mut App, _t: f64, _tx: &mut rune::Tx) -> bool {
    let key = s.moving_key();
    let moving = key != s.shown_key;
    s.shown_key = key;
    if s.fps_time < 0.5 {
        return moving;
    }
    let fps = (s.fps_frames as f64 / s.fps_time).round() as u32;
    (s.fps_frames, s.fps_time) = (0, 0.0);
    let scale = (s.render_scale * 100.0).round() / 100.0;
    let changed = fps != s.fps || scale != s.shown_scale;
    (s.fps, s.shown_scale) = (fps, scale);
    changed || moving
}

fn on_key(s: &mut App, e: &KeyEvent, _tx: &mut rune::Tx) -> bool {
    if e.key != Key::Escape {
        return false;
    }
    match s.screen {
        Screen::Menu(Menu::Main) => return false,
        Screen::Game | Screen::Menu(_) => s.open_menu(Menu::Main),
    }
    true
}

/// Headless demo: a beginner board, the middle opened, then the boat sent to a mine.
fn boom_scene(s: &mut App) {
    s.pending = Some((Next::Start(Some("beginner"), LEVELS[0].dims, LEVELS[0].mines), 0.0));
    s.settle();
    if let Some(g) = s.game.as_mut() {
        let middle = g.dims.count() / 2;
        g.open(middle);
        if let Some(m) = (0..g.dims.count()).find(|&i| g.cells[i].mine) {
            g.click(m);
        }
    }
}

/// Headless: straight into an expert board with its middle opened.
fn expert_scene(s: &mut App) {
    s.pending = Some((Next::Start(Some("expert"), LEVELS[2].dims, LEVELS[2].mines), 0.0));
    s.settle();
    if let Some(g) = s.game.as_mut() {
        let middle = g.dims.w * (g.dims.h / 2) + g.dims.w / 2;
        g.open(middle);
    }
}

fn main() {
    rune::run_with(
        AppSpec::<App> {
            min_size: Some((640.0, 520.0)),
            scenes: &[("boom", boom_scene), ("expert", expert_scene)],
            poll: Some(poll),
            on_key: Some(on_key),
            ..AppSpec::new("Mines Afloat", (1100.0, 860.0), init, view)
        },
        SceneCompositor::default(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: f64 = 1.0 / 60.0;

    /// An app with no records file and the board laid out as in the default window.
    fn app() -> App {
        let mut s = App::new(0x5eed, Records::default());
        s.board = [0.0, 50.5, 1100.0, 809.5];
        s
    }

    fn run(s: &mut App, seconds: f64) {
        for _ in 0..(seconds / FRAME).round() as usize {
            s.update(s.clock + FRAME, FRAME);
        }
    }

    /// The window point over the middle of a menu tile.
    fn point_at_item(s: &App, want: Action) -> (f32, f32) {
        let k = s.lobby.tiles.iter().position(|t| t.action == want).expect("the item is on the menu");
        let label = s.menu_labels().into_iter().find(|l| l.key == k).expect("its label is shown");
        (label.x + s.board[0], label.y + s.board[1])
    }

    /// Moves the pointer onto a menu item and clicks it.
    fn pick(s: &mut App, want: Action) {
        let (x, y) = point_at_item(s, want);
        s.pointer_moved(Some(PointerPress { x, y }));
        s.press();
        assert!(s.pending.is_some() || matches!(want, Action::Adjust(_) | Action::Show), "{want:?} was not picked");
    }

    #[test]
    fn a_click_lands_where_the_label_is_drawn() {
        let mut s = app();
        run(&mut s, 3.0);
        pick(&mut s, Action::Play);
        run(&mut s, 3.0);
        assert_eq!(s.screen, Screen::Menu(Menu::Levels));
        pick(&mut s, Action::Level(0));
        run(&mut s, 2.0);
        assert_eq!(s.screen, Screen::Game);
        assert_eq!(s.game.as_ref().map(|g| g.dims), Some(LEVELS[0].dims));
    }

    /// A game starts under a still pointer: the cell under it is hovered, and a click sails.
    #[test]
    fn a_new_game_knows_where_the_pointer_is() {
        let mut s = app();
        run(&mut s, 3.0);
        pick(&mut s, Action::Play);
        run(&mut s, 3.0);
        pick(&mut s, Action::Level(0));
        run(&mut s, 3.0);
        let (x, y) = s.pointer.expect("the pointer");
        let under = s.water_at(x, y).and_then(|(wx, wz)| LEVELS[0].dims.cell_at(wx, wz));
        assert!(under.is_some(), "the test's pointer is off the board");
        assert_eq!(s.hover, under);
        s.press();
        assert_eq!(s.game.as_ref().and_then(|g| g.sea.boat.target), under, "the click did not send the boat");
    }

    /// Back in the menu after a game, the cursor is where the pointer is now, not where it was
    /// when the game was picked.
    #[test]
    fn back_in_the_menu_the_cursor_follows_the_pointer() {
        let mut s = app();
        run(&mut s, 3.0);
        pick(&mut s, Action::Custom);
        run(&mut s, 3.0);
        pick(&mut s, Action::Start);
        run(&mut s, 3.0);
        assert_eq!(s.screen, Screen::Game);
        // the pointer moves about in the game, then Esc
        s.pointer_moved(Some(PointerPress { x: 900.0, y: 700.0 }));
        s.open_menu(Menu::Main);
        assert_eq!(s.lobby.sea.cursor, s.water_at(900.0, 700.0));
        run(&mut s, 3.0);
        s.press();
        assert!(s.pending.is_none(), "a click on open water picked a menu item");
    }

    #[test]
    fn the_custom_game_takes_up_to_all_but_nine_cells_of_mines() {
        let mut s = app();
        for _ in 0..40 {
            s.adjust(0, true);
            s.adjust(1, true);
        }
        for _ in 0..600 {
            s.adjust(2, true);
        }
        assert_eq!(s.custom, (30, 16, 30 * 16 - 9));
        for _ in 0..40 {
            s.adjust(0, false);
        }
        assert_eq!(s.custom, (5, 16, 5 * 16 - 9), "a narrower board takes fewer mines");
        for _ in 0..40 {
            s.adjust(1, false);
            s.adjust(2, false);
        }
        assert_eq!(s.custom, (5, 5, 1));
    }

    /// After a pick only the blown tile moves (it carries no label): the view has nothing to
    /// rebuild for, so `poll` must not ask for it.
    #[test]
    fn a_flying_tile_without_a_label_asks_for_no_rebuild() {
        let mut s = app();
        run(&mut s, 3.0);
        pick(&mut s, Action::Play);
        run(&mut s, 0.75);
        assert!(s.menu_labels().is_empty(), "labels still shown 0.75 s after the pick");
        let key = s.moving_key();
        for _ in 0..15 {
            run(&mut s, FRAME);
            assert_eq!(s.moving_key(), key);
        }
    }

    #[test]
    fn a_win_on_a_level_is_a_record() {
        let mut s = app();
        s.pending = Some((Next::Start(Some("beginner"), LEVELS[0].dims, LEVELS[0].mines), 0.0));
        s.settle();
        run(&mut s, 1.0);
        let g = s.game.as_mut().expect("a game");
        g.open(40);
        for c in 0..g.dims.count() {
            if !g.cells[c].mine {
                g.open(c);
            }
        }
        assert_eq!(g.status, Status::Won);
        run(&mut s, FRAME);
        assert!(s.new_record);
        assert!(s.records.best("beginner").is_some_and(|t| (t - 1.0).abs() < 0.1));
        run(&mut s, END_HOLD + 0.1);
        s.press();
        assert_eq!(s.screen, Screen::Menu(Menu::Main));
        assert!(s.game.is_none());
    }

    /// The swell the tiles float on (water.rs) is the one the shaders draw (common.wgsl).
    #[test]
    fn cpu_and_gpu_waves_agree() {
        let src = include_str!("shaders/common.wgsl");
        let table = &src[src.find("const WAVES").expect("WAVES in common.wgsl")..];
        let rows: Vec<[f32; 4]> = table
            .lines()
            .skip(1)
            .take_while(|l| l.trim_start().starts_with("vec4<f32>("))
            .map(|l| {
                let inner = &l[l.find('(').unwrap() + 1..l.rfind(')').unwrap()];
                let v: Vec<f32> = inner.split(',').map(|x| x.trim().parse().unwrap()).collect();
                [v[0], v[1], v[2], v[3]]
            })
            .collect();
        assert_eq!(rows, water::WAVES.to_vec());
    }

    // ---- the camera: zoom, pan, limits, and everything that looks through it ----

    /// The board element of a 390 x 844 phone window under the two-line bar.
    const PHONE: [f32; 4] = [0.0, 90.5, 390.0, 753.5];
    const DESKTOP: [f32; 4] = [0.0, 50.5, 1100.0, 809.5];

    fn near(a: Vec2, b: Vec2, what: &str) {
        assert!(a.distance(b) < 1e-3, "{what}: {a} vs {b}");
    }

    /// A game of `level` under the board rect `board`, its view as it starts.
    fn game_in(board: [f32; 4], level: usize) -> App {
        let mut s = App::new(0x5eed, Records::default());
        s.board = board;
        let l = &LEVELS[level];
        s.pending = Some((Next::Start(Some(l.key), l.dims, l.mines), 0.0));
        s.settle();
        run(&mut s, 1.0);
        s
    }

    fn middle(s: &App) -> (f32, f32) {
        let [bx, by, bw, bh] = s.board;
        (bx + bw * 0.5, by + bh * 0.5)
    }

    fn press_at(s: &mut App, phase: Phase, (x, y): (f32, f32), button: PointerButton) {
        s.press_event(phase, (x, y), false, button);
    }

    fn nothing_opened(s: &App) -> bool {
        s.game.as_ref().is_some_and(|g| g.cells.iter().all(|c| !c.open && !c.flag) && g.sea.boat.target.is_none())
    }

    /// The camera at zoom 1 with no pan is the one the game had before zoom existed, bit for
    /// bit, wherever the window is at least as wide as the 1.25 it was framed for.
    #[test]
    fn at_zoom_one_the_camera_is_the_fixed_one() {
        fn old(aspect: f32, half: (f32, f32)) -> glam::Mat4 {
            let back = (half.1 / 5.0).max(half.0 / 5.0 * 1.25 / aspect).max(1.25 / aspect).max(0.85);
            let eye = Vec3::new(0.0, 12.0, 8.6) * back;
            let target = Vec3::new(0.0, -0.3, 0.55 * back);
            glam::Mat4::perspective_rh(38f32.to_radians(), aspect, 0.1, 200.0) * glam::Mat4::look_at_rh(eye, target, Vec3::Y)
        }
        let halves = [(4.5, 4.5), (8.0, 8.0), (15.0, 8.0), (2.5, 2.5), (15.0, 2.5), (6.0, 5.0)];
        for aspect in [1.25, 1100.0 / 809.5, 1.6, 844.0 / 340.0, 3.0] {
            for half in halves {
                assert_eq!(gfx::look(aspect, half, 1.0, Vec2::ZERO).view_proj, old(aspect, half), "aspect {aspect}, board {half:?}");
            }
        }
        // and through the app: every level and the menu on the desktop window start unzoomed
        for (k, level) in LEVELS.iter().enumerate() {
            let s = game_in(DESKTOP, k);
            assert_eq!(s.view, View::default(), "{} starts zoomed on the desktop", level.name);
            assert_eq!(s.camera().view_proj, old(1100.0 / 809.5, level.dims.half()));
        }
        let mut s = app();
        run(&mut s, 1.0);
        assert_eq!(s.view, View::default(), "the menu starts zoomed on the desktop");
        assert_eq!(s.camera().view_proj, old(1100.0 / 809.5, lobby::GRID.half()));
    }

    /// Zooming about a window point keeps the water under it, at the middle and near a corner,
    /// on a phone and on the desktop.
    #[test]
    fn a_zoom_keeps_the_water_under_its_point() {
        // close enough that the board overfills the view both ways: where it fits one way it
        // stays centred that way (as a photo viewer keeps a small photo), see the next tests
        for (board, level, zoom) in [(PHONE, 2, 4.0), (DESKTOP, 2, 1.6), (DESKTOP, 1, 1.3)] {
            let mut s = game_in(board, level);
            let stage = s.stage();
            s.view = stage.zoomed(zoom);
            let [bx, by, bw, bh] = board;
            for (x, y) in [middle(&s), (bx + bw * 0.3, by + bh * 0.45), (bx + bw * 0.7, by + bh * 0.55)] {
                let before = stage.under(s.view, x, y).expect("water under the point");
                let zoomed = stage.zoom_about(s.view, x, y, 1.5);
                assert!((zoomed.zoom - zoom * 1.5).abs() < 1e-4, "the zoom hit a limit: {zoomed:?}");
                near(stage.under(zoomed, x, y).expect("water under the point"), before, "zoom about a point");
            }
        }
    }

    /// A drag carries the water it grabbed under the pointer, and opens nothing.
    #[test]
    fn a_drag_pans_with_the_pointer_and_opens_nothing() {
        let mut s = game_in(PHONE, 2);
        let (x, y) = middle(&s);
        let grab = s.stage().under(s.view, x, y).expect("water");
        press_at(&mut s, Phase::Down, (x, y), PointerButton::Primary);
        for k in 1..=10 {
            let p = (x + 9.0 * k as f32, y);
            press_at(&mut s, Phase::Move, p, PointerButton::Primary);
            if k > 2 {
                near(s.stage().under(s.view, p.0, p.1).expect("water"), grab, "the grabbed water follows the pointer");
            }
        }
        run(&mut s, 1.0);
        press_at(&mut s, Phase::Up, (x + 90.0, y), PointerButton::Primary);
        assert_ne!(s.view.pan, Vec2::ZERO, "the drag panned nothing");
        assert!(nothing_opened(&s), "a pan opened or flagged a cell");
    }

    /// The middle button pans and never clicks, even released where it went down.
    #[test]
    fn the_middle_button_only_pans() {
        let mut s = game_in(DESKTOP, 2);
        s.view = s.stage().zoomed(2.5);
        let (x, y) = middle(&s);
        press_at(&mut s, Phase::Down, (x, y), PointerButton::Middle);
        press_at(&mut s, Phase::Move, (x + 4.0, y), PointerButton::Middle);
        let panned = s.view.pan;
        press_at(&mut s, Phase::Up, (x + 4.0, y), PointerButton::Middle);
        press_at(&mut s, Phase::Down, (x, y), PointerButton::Middle);
        run(&mut s, 1.0);
        press_at(&mut s, Phase::Up, (x, y), PointerButton::Middle);
        assert_ne!(panned, s.stage().zoomed(2.5).pan, "a small middle drag did not pan");
        assert!(nothing_opened(&s), "the middle button opened or flagged a cell");
        // the primary button still clicks
        press_at(&mut s, Phase::Down, (x, y), PointerButton::Primary);
        press_at(&mut s, Phase::Up, (x, y), PointerButton::Primary);
        assert!(!nothing_opened(&s), "a click did nothing");
    }

    /// A second finger landing on a held press makes it a pinch's: no flag however long it is
    /// held, no click on release, no pan from the first finger. Two fingers spreading zoom
    /// about their middle.
    #[test]
    fn a_pinch_cancels_the_press_and_zooms() {
        let mut s = game_in(PHONE, 2);
        s.view = s.stage().zoomed(4.0);
        let (x, y) = middle(&s);
        press_at(&mut s, Phase::Down, (x, y), PointerButton::Primary);
        let centre = Vec2::new(x + 30.0, y);
        let water = s.stage().under(s.view, centre.x, centre.y).expect("water");
        let zoom = s.stage().clamp(s.view).zoom;
        let fingers = |spread: f32| Touches { fingers: 2, generation: 7, centre, spread, pinched: true };
        s.touch(fingers(60.0));
        run(&mut s, 1.0);
        s.touch(fingers(90.0));
        press_at(&mut s, Phase::Move, (x + 3.0, y + 1.0), PointerButton::Primary);
        press_at(&mut s, Phase::Up, (x + 3.0, y + 1.0), PointerButton::Primary);
        assert!(nothing_opened(&s), "the pinch's first finger opened or flagged a cell");
        assert!((s.stage().clamp(s.view).zoom - zoom * 1.5).abs() < 1e-3, "the spread did not zoom 1.5x");
        near(s.stage().under(s.view, centre.x, centre.y).expect("water"), water, "the pinch's middle kept its water");
        // a fresh touch is a tap again
        s.touch(Touches::default());
        press_at(&mut s, Phase::Down, (x, y), PointerButton::Primary);
        press_at(&mut s, Phase::Up, (x, y), PointerButton::Primary);
        assert!(!nothing_opened(&s), "a tap after the pinch did nothing");
    }

    /// Every cell centre shown under a zoomed and panned camera picks its own cell.
    #[test]
    fn picking_finds_each_shown_cell_under_zoom_and_pan() {
        let mut s = game_in(PHONE, 2);
        let stage = s.stage();
        s.view = stage.clamp(View { zoom: 4.0, pan: Vec2::new(-6.0, 1.0) });
        assert!(s.view.zoom > 3.9 && s.view.pan.x < -5.9, "the test's view was clamped: {:?}", s.view);
        let cam = s.camera();
        let [bx, by, bw, bh] = s.board;
        let dims = LEVELS[2].dims;
        let mut shown = 0;
        for i in 0..dims.count() {
            let (cx, cz) = dims.center(i);
            let at = cam.project(Vec3::new(cx, 0.0, cz));
            if !(0.02..0.98).contains(&at.x) || !(0.02..0.98).contains(&at.y) {
                continue;
            }
            shown += 1;
            let w = s.water_at(bx + at.x * bw, by + at.y * bh).expect("water under a shown cell");
            assert_eq!(dims.cell_at(w.0, w.1), Some(i), "cell {i}");
        }
        assert!(shown > 20, "only {shown} cells shown");
    }

    /// The scene is drawn through the camera picking uses, zoomed and panned, in a game and in
    /// the menu.
    #[test]
    fn the_drawn_camera_is_the_picking_camera() {
        let mut s = game_in(PHONE, 2);
        s.view = s.stage().clamp(View { zoom: 3.0, pan: Vec2::new(4.0, -0.5) });
        assert_ne!(s.view.pan, Vec2::ZERO);
        let drawn = s.scene().globals.expect("globals").view_proj;
        assert_eq!(drawn, s.camera().view_proj.to_cols_array_2d(), "game");
        s.open_menu(Menu::Main);
        s.view = s.stage().clamp(View { zoom: 1.8, pan: Vec2::new(1.0, 0.5) });
        assert_ne!(s.view.pan, Vec2::ZERO);
        let drawn = s.scene().globals.expect("globals").view_proj;
        assert_eq!(drawn, s.camera().view_proj.to_cols_array_2d(), "menu");
    }

    /// The zoom stays between the fit and its limit, a pan cannot lose the board, a turned
    /// screen keeps both in limits, and a new game or menu starts afresh.
    #[test]
    fn the_view_keeps_within_limits_and_starts_afresh() {
        let mut s = game_in(PHONE, 2);
        let stage = s.stage();
        assert_eq!(stage.clamp(View { zoom: 0.2, pan: Vec2::ZERO }).zoom, 1.0);
        let most = stage.clamp(View { zoom: 1e6, pan: Vec2::ZERO }).zoom;
        assert!(most == stage.max_zoom() && most > 5.0, "closest zoom on a phone's expert board: {most}");
        let (x, y) = middle(&s);
        let area = (LEVELS[2].dims.half().0 + 0.5, LEVELS[2].dims.half().1 + 0.5);
        for pan in [Vec2::new(1e4, 0.0), Vec2::new(-1e4, 1e4), Vec2::new(0.0, -1e4)] {
            for zoom in [1.0, 2.0, 6.0] {
                let v = stage.clamp(View { zoom, pan });
                let c = stage.under(v, x, y).expect("water");
                assert!(c.x.abs() <= area.0 + 1e-3 && c.y.abs() <= area.1 + 1e-3, "the board left the view: {v:?} looks at {c}");
            }
        }
        assert_eq!(stage.clamp(View { zoom: 1.0, pan: Vec2::new(3.0, 3.0) }).pan, Vec2::ZERO, "at the fit the board moved");
        // the screen turns: the stored view is far out of the new limits, the camera is not
        s.view = View { zoom: 6.0, pan: Vec2::new(14.0, 0.0) };
        s.board = [0.0, 50.5, 844.0, 339.5];
        let turned = s.stage();
        let v = turned.clamp(s.view);
        assert!(v.zoom <= turned.max_zoom() && v.pan.x < 14.0, "not re-clamped for the turned screen: {v:?}");
        run(&mut s, FRAME);
        assert_eq!(s.view, s.start_view(), "a turned screen keeps the old view");
        // a new game and the menu start afresh
        s.view = View { zoom: 2.0, pan: Vec2::new(3.0, 0.0) };
        s.open_menu(Menu::Main);
        assert_eq!(s.view, s.start_view());
        s.view = View { zoom: 2.0, pan: Vec2::new(1.0, 0.0) };
        s.pending = Some((Next::Start(Some("beginner"), LEVELS[0].dims, LEVELS[0].mines), 0.0));
        s.settle();
        assert_eq!(s.view, s.start_view());
    }

    /// After a game, a pan looks at the wreck; only a tap returns to the menu.
    #[test]
    fn after_a_game_a_pan_is_not_a_tap() {
        let mut s = game_in(PHONE, 2);
        let g = s.game.as_mut().expect("a game");
        g.open(g.dims.count() / 2);
        let mine = (0..g.dims.count()).find(|&i| g.cells[i].mine).expect("a mine");
        g.open(mine);
        assert_eq!(g.status, Status::Lost);
        run(&mut s, END_HOLD + 0.5);
        let (x, y) = middle(&s);
        press_at(&mut s, Phase::Down, (x, y), PointerButton::Primary);
        press_at(&mut s, Phase::Move, (x + 80.0, y), PointerButton::Primary);
        press_at(&mut s, Phase::Up, (x + 80.0, y), PointerButton::Primary);
        assert_eq!(s.screen, Screen::Game, "a pan went back to the menu");
        press_at(&mut s, Phase::Down, (x, y), PointerButton::Primary);
        press_at(&mut s, Phase::Up, (x, y), PointerButton::Primary);
        assert_eq!(s.screen, Screen::Menu(Menu::Main));
    }

    /// The menu picks right with its camera zoomed and panned: labels and picking share it.
    #[test]
    fn menu_items_pick_under_zoom_and_pan() {
        let mut s = app();
        run(&mut s, 3.0);
        s.view = s.stage().clamp(View { zoom: 1.7, pan: Vec2::new(0.8, 0.6) });
        assert!(s.view.zoom > 1.6 && s.view.pan != Vec2::ZERO);
        pick(&mut s, Action::Records);
        run(&mut s, 3.0);
        assert_eq!(s.screen, Screen::Menu(Menu::Records));
        s.view = s.stage().clamp(View { zoom: 1.4, pan: Vec2::new(-0.5, -0.4) });
        pick(&mut s, Action::Back);
        run(&mut s, 3.0);
        assert_eq!(s.screen, Screen::Menu(Menu::Main));
    }

    /// On a phone the menu opens on its column: tiles a finger can hit, labels one can read,
    /// and they still pick. An expert board opens zoomed to cells a finger can hit.
    #[test]
    fn a_phone_gets_finger_sized_menus_and_cells() {
        let mut s = App::new(0x5eed, Records::default());
        s.board = PHONE;
        run(&mut s, 3.0);
        for (menu, item) in
            [(Menu::Main, Action::Play), (Menu::Levels, Action::Level(2)), (Menu::Records, Action::Back), (Menu::Custom, Action::Start)]
        {
            s.open_menu(menu);
            run(&mut s, 3.0);
            let cam = s.camera();
            let depth = (cam.project(Vec3::Z * lobby::ITEM[2] * 0.5).y - cam.project(-Vec3::Z * lobby::ITEM[2] * 0.5).y) * s.board[3];
            assert!(depth >= 44.0, "{menu:?}: tiles {depth} pt deep");
            let labels = s.menu_labels();
            assert!(labels.iter().all(|l| l.size >= 13.0), "{menu:?}: labels at {} pt", labels[0].size);
            pick(&mut s, item);
        }
        let s = game_in(PHONE, 2);
        let cell = s.stage().unit_points() * s.view.zoom;
        assert!((cell - START_CELL).abs() < 1.0, "an expert cell starts at {cell} pt");
    }

    /// A held custom game value keeps stepping; letting go steps nothing more.
    #[test]
    fn a_held_value_repeats() {
        let mut s = app();
        run(&mut s, 3.0);
        pick(&mut s, Action::Custom);
        run(&mut s, 3.0);
        let (x, y) = point_at_item(&s, Action::Adjust(0));
        let right = (x + 60.0, y);
        press_at(&mut s, Phase::Down, right, PointerButton::Primary);
        run(&mut s, 1.5);
        let held = s.custom.0;
        assert!(held >= 12 + 8, "width after a 1.5 s hold: {held}");
        press_at(&mut s, Phase::Up, right, PointerButton::Primary);
        assert_eq!(s.custom.0, held, "the release stepped once more");
        // a quick tap is one step
        press_at(&mut s, Phase::Down, right, PointerButton::Primary);
        press_at(&mut s, Phase::Up, right, PointerButton::Primary);
        assert_eq!(s.custom.0, (held + 1).min(30));
    }
}
