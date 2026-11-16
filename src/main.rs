//! Mines Afloat: minesweeper on water. Rune draws the UI and hosts the 3D world in an
//! `El::effect` slot; oxijolt floats the tiles; `ripple` makes the water answer the cursor.
//! The menu is a world of its own (`lobby`): its items are floating tiles with Rune labels on
//! them, and picking one blows its tile up like a mine.
//!
//! This file holds the app's state and menu flow, the Rune view and the compositor that draws
//! the 3D scene (`scene`, `gfx`) into the view's slot.

mod boat;
mod game;
mod gfx;
mod lobby;
mod records;
mod ripple;
mod scene;
mod sea;
mod water;

use glam::{Vec3, Vec4};
use rune::compose::{Compositor, Frame};
use rune::tok::{self, TextStyle};
use rune::{AppSpec, Color, Cx, Drag, El, Key, KeyEvent, Phase, PointerPress, col, el, row, text};

use game::{Dims, Game, Status};
use gfx::Gpu;
use lobby::{Action, Lobby};
use records::Records;

const BOARD_MARK: u32 = 1;
/// How long a picked menu tile's blast plays before the next screen.
const PICK_DELAY: f64 = 1.1;
/// How long a finished game holds before a click returns to the menu.
const END_HOLD: f64 = 1.2;
/// A menu label fades out over the first part of its tile's melt and is gone after it.
const LABEL_FADE: f32 = 0.6;

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
}

/// Seconds a still press takes to plant a flag.
const LONG_PRESS: f64 = 0.45;
/// Points a press may wander and still count as held still.
const LONG_PRESS_SLOP: f32 = 12.0;

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

    /// The board half-extents the camera frames on this screen.
    fn framed(&self) -> (f32, f32) {
        match (&self.screen, &self.game) {
            (Screen::Game, Some(g)) => g.dims.half(),
            _ => lobby::GRID.half(),
        }
    }

    /// The camera over this screen's board, for the board element's layout rect.
    fn camera(&self) -> gfx::Camera {
        let [_, _, bw, bh] = self.board;
        gfx::camera(bw.max(1.0) / bh.max(1.0), self.framed())
    }

    /// The water point under window point (x, y), if it is over the board element.
    fn water_at(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        let [bx, by, bw, bh] = self.board;
        let (u, v) = ((x - bx) / bw.max(1.0), (y - by) / bh.max(1.0));
        if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
            return None;
        }
        let inv = self.camera().view_proj.inverse();
        let ndc = (u * 2.0 - 1.0, 1.0 - v * 2.0);
        let near = inv.project_point3(Vec3::new(ndc.0, ndc.1, 0.0));
        let far = inv.project_point3(Vec3::new(ndc.0, ndc.1, 1.0));
        let d = far - near;
        if d.y.abs() < 1e-5 {
            return None;
        }
        let t = -near.y / d.y;
        (t > 0.0).then_some((near.x + d.x * t, near.z + d.z * t))
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

    /// Down, move and up of any press on the board, mouse or touch: the press is aimed where it
    /// went down (a touch has no hover before it), and a still one turns into a flag.
    fn drag(&mut self, d: &Drag) {
        let [x, y] = d.window_point();
        match d.phase {
            Phase::Down => {
                self.held = Some((self.clock, (x, y)));
                self.held_flagged = false;
            }
            Phase::Move => {
                if self.held.is_some_and(|(_, (x0, y0))| (x - x0).hypot(y - y0) > LONG_PRESS_SLOP) {
                    self.held = None;
                }
            }
            Phase::Up => self.held = None,
        }
        self.pointer_moved(Some(PointerPress { x, y }));
    }

    /// A press held still for `LONG_PRESS` plants a flag, once.
    fn watch_hold(&mut self) {
        if let Some((at, (x, y))) = self.held
            && !self.held_flagged
            && self.clock - at >= LONG_PRESS
        {
            self.held_flagged = true;
            self.secondary_press(PointerPress { x, y });
        }
    }

    fn press(&mut self) {
        if std::mem::take(&mut self.held_flagged) {
            return;
        }
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
        match (self.screen, self.game.as_mut()) {
            (Screen::Game, Some(g)) => g.advance(dt as f32),
            _ => self.lobby.advance(dt as f32),
        }
        self.watch_hold();
        self.watch_end();
        self.settle();
        self.fps_frames += 1;
        self.fps_time += dt;
    }

    /// The menu's labels as they land on their tiles this frame: projected from each tile's
    /// top face, all at one face size, small enough for the longest label to stay on its tile.
    fn menu_labels(&self) -> Vec<Label<'_>> {
        let l = &self.lobby;
        let [_, _, bw, bh] = self.board;
        let cam = self.camera();
        let project = |p: Vec3| {
            let c = cam.view_proj * Vec4::new(p.x, p.y, p.z, 1.0);
            (c.x / c.w * 0.5 + 0.5, 0.5 - c.y / c.w * 0.5)
        };
        let tile_px = {
            let (ul, _) = project(-Vec3::X * lobby::ITEM[0] * 0.5);
            let (ur, _) = project(Vec3::X * lobby::ITEM[0] * 0.5);
            (ur - ul) * bw * 0.86
        };
        let longest = l.tiles.iter().map(|t| t.label.chars().count()).max().unwrap_or(1).max(1) as f32;
        let size = (bh / 30.0).clamp(14.0, 26.0).min(tile_px / (longest * 0.56));
        let mut labels = Vec::new();
        for (k, tile) in l.tiles.iter().enumerate() {
            let fade = l.sea.fade_of(&tile.body);
            if tile.blown || fade > LABEL_FADE {
                continue;
            }
            let Some((pos, _)) = l.sea.draw_pose(&tile.body) else { continue };
            let (u, v) = project(pos + Vec3::Y * lobby::ITEM[1] * 0.5);
            labels.push(Label { key: k, text: &tile.label, x: u * bw, y: v * bh, size, fade });
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

fn view(s: &App, _: &mut Cx<App>) -> El {
    let t = tok::theme(true);
    let [_, _, bw, _] = s.board;

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
        .on_drag(|s: &mut App, d: &mut Drag, _| s.drag(d))
        .on_press(|s: &mut App| s.press())
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
            let mut banner = col()
                .key("banner")
                .absolute(bw / 2.0 - 200.0, 18.0)
                .size(400.0, if since > END_HOLD { 64.0 } else { 44.0 })
                .radius(18.0)
                .fill(Color::BLACK.alpha(0.5))
                .center()
                .gap(4.0)
                .child(text(line).font(TextStyle::new(17.0, 700.0)).color(c));
            if since > END_HOLD {
                banner = banner.child(text("Click to return to the menu").font(TextStyle::new(12.5, 500.0)).color(Color::WHITE.alpha(0.7)));
            }
            board = board.child(banner);
        }
        _ => {}
    }

    let hint = "Click the water to sail  ·  right-click or long-press to flag  ·  Esc for the menu";
    let bar = row()
        .w_full()
        .padding(14.0)
        .gap(18.0)
        .items_center()
        .child(text("Mines Afloat").font(TextStyle::new(18.0, 700.0)).color(t.text))
        .child(el().grow(1.0))
        .child(text(hint).font(TextStyle::new(12.0, 500.0)).color(t.text.alpha(0.55)))
        .child(
            text(format!("{} FPS", s.fps)).font(TextStyle::new(13.0, 600.0).mono()).color(rune::color::hex("#7fe0c0").unwrap_or(t.text)),
        );
    col().key("root").w_full().h_full().fill(rune::color::hex("#0b1419").unwrap_or(t.bg)).child(bar).child(board)
}

/// Steps the world and draws it into the board's effect slot.
#[derive(Default)]
struct SceneCompositor {
    gpu: Option<Gpu>,
    /// `Runtime::clock` at the last frame. The clock is the wall in a window and virtual time
    /// in headless runs, so a recorded video plays at the speed the game really runs;
    /// `Frame::dt` is wall time even headless.
    last_clock: Option<f64>,
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
        let aspect = board[2].max(1.0) / board[3].max(1.0);
        let s = &f.rt.state;
        let scene = match (s.screen, &s.game) {
            (Screen::Game, Some(g)) => scene::build_game(g, s.hover, aspect),
            _ => scene::build_lobby(&s.lobby, aspect),
        };

        if self.gpu.as_ref().is_none_or(|g| g.key() != f.device_key()) {
            self.gpu = Some(Gpu::new(f));
        }
        let gpu = self.gpu.as_mut().expect("gpu");
        gpu.render(f, &scene, viewport);

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
    let changed = fps != s.fps;
    s.fps = fps;
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
}
