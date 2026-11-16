//! What the renderer draws this frame: instances for the tiles, mines, boat and wreck, and the
//! uniforms for the water and the number clouds, built from a `Game` or the `Lobby`.

use glam::{Mat4, Quat, Vec3};

use crate::boat;
use crate::game::{self, Dims, Game, Status};
use crate::gfx::{self, Instance, MeshKind, Scene};
use crate::labels;
use crate::lobby::{self, Lobby};
use crate::ripple::{self, Ripple};
use crate::sea::{MINE_R, Sea};
use crate::water;

/// Seconds a number cloud takes to puff in.
const CLOUD_IN: f32 = 0.55;
/// Height the number clouds float at. `SLAB` in shaders/clouds.wgsl is the half-height of the
/// layer around it that the cloud rays march through.
const CLOUD_HEIGHT: f32 = 0.9;
/// Seconds the lobby's blast stays lit.
const LOBBY_BOOM_SHOWS: f32 = 6.0;
/// A menu label fades out over the first part of its tile's melt and is gone after it.
pub const LABEL_FADE: f32 = 0.6;

/// How a menu label looks: at rest, under the pointer, or under a press.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Look {
    Rest,
    Hovered,
    Pressed,
}

/// How much of a menu tile's label shows as the tile melts by `fade` (0 to 1), or once blown.
pub fn label_alpha(fade: f32, blown: bool) -> f32 {
    if blown { 0.0 } else { (1.0 - fade / LABEL_FADE).clamp(0.0, 1.0) }
}

/// The boat, part by part, around its hull frame. Squash keeps volume: taller is thinner.
fn boat_instances(scene: &mut Scene, pose: &boat::Pose) {
    let sq = 1.0 + pose.squash;
    let hull = pose.hull() * Mat4::from_scale(Vec3::new(1.0 / sq.sqrt(), sq, 1.0 / sq.sqrt()));
    for (k, part) in boat::PARTS.iter().enumerate() {
        let off = Vec3::from_array(part.offset);
        let size = part.size;
        let local = if k == boat::BOW {
            Mat4::from_rotation_translation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_4), off)
        } else if k == boat::MAST {
            // the mast swings about its foot, lagging the hull
            let foot = off - Vec3::Y * size[1] * 0.5;
            Mat4::from_translation(foot) * Mat4::from_rotation_z(pose.mast) * Mat4::from_translation(Vec3::Y * size[1] * 0.5)
        } else {
            Mat4::from_translation(off)
        };
        scene.push(MeshKind::Cube, Instance::new(hull * local, Vec3::from_array(size), part.color, part.metal, part.rough, 0.0));
        if k == boat::MAST {
            // the flag at the top flutters about the mast
            let top = hull
                * local
                * Mat4::from_translation(Vec3::Y * (size[1] * 0.5 - 0.035))
                * Mat4::from_rotation_y(pose.flag)
                * Mat4::from_translation(Vec3::new(-0.05, 0.0, 0.0));
            scene.push(MeshKind::Cube, Instance::new(top, Vec3::new(0.10, 0.055, 0.006), [0.92, 0.15, 0.10], 0.0, 0.6, 0.1));
        }
    }
}

/// The charred wreck: pieces with embers that flicker and die down.
fn debris_instances(scene: &mut Scene, sea: &Sea) {
    let since = sea.boom_t;
    let heat = (1.0 - since / 9.0).max(0.0);
    for (k, piece) in sea.debris.iter().enumerate() {
        let Some((pos, rot)) = sea.draw_pose(&piece.body) else { continue };
        let flicker = 0.55 + 0.45 * (sea.time * (11.0 + k as f32 * 1.7) + k as f32).sin();
        let e = heat * flicker;
        let char_col = [0.035, 0.03, 0.026];
        let ember = [0.95, 0.32, 0.06];
        let col = [0, 1, 2].map(|i| char_col[i] + (ember[i] - char_col[i]) * e * 0.18);
        scene.push(
            MeshKind::Cube,
            Instance::new(Mat4::from_rotation_translation(rot, pos), Vec3::from_array(piece.size), col, 0.0, 0.9, e * 0.5),
        );
    }
}

/// A floating tile, melting into a film as `fade` goes from 0 to 1.
fn tile_instance(scene: &mut Scene, pos: Vec3, rot: Quat, size: [f32; 3], color: [f32; 3], fade: f32, glow: f32) -> Mat4 {
    let ease = fade * fade * (3.0 - 2.0 * fade);
    let model = Mat4::from_rotation_translation(rot, pos - Vec3::Y * 0.07 * ease);
    let film = [0.05, 0.20, 0.21];
    let color = [0, 1, 2].map(|i| color[i] + (film[i] - color[i]) * ease);
    let spread = 1.0 + 0.18 * ease;
    let thick = size[1] * (1.0 - ease) + 0.004;
    scene.push(
        MeshKind::Cube,
        Instance::new(model, Vec3::new(size[0] * spread, thick, size[2] * spread), color, 0.0, 0.42 - 0.35 * ease, glow),
    );
    model
}

/// A sea mine: a dark sphere with a brass band, horns all round, and a lamp on top.
fn mine_instances(scene: &mut Scene, pos: Vec3, rot: Quat, lamp: [f32; 3], t: f32) {
    let model = Mat4::from_rotation_translation(rot, pos);
    let r = MINE_R;
    scene.push(MeshKind::Sphere, Instance::new(model, Vec3::splat(r), [0.06, 0.07, 0.08], 0.85, 0.32, 0.0));
    // a band round the waist
    scene.push(MeshKind::Sphere, Instance::new(model, Vec3::new(r * 1.06, 0.05, r * 1.06), [0.35, 0.28, 0.12], 0.9, 0.25, 0.0));
    let s = 1.0 / 3f32.sqrt();
    #[rustfmt::skip]
    let dirs = [
        [1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, -1.0],
        [s, s, s], [-s, s, s], [s, s, -s], [-s, s, -s], [s, -s, s], [-s, -s, s], [s, -s, -s], [-s, -s, -s],
    ];
    for d in dirs {
        let d = Vec3::from_array(d);
        let q = Quat::from_rotation_arc(Vec3::Y, d);
        let m = model * Mat4::from_rotation_translation(q, d * (r * 0.85));
        scene.push(MeshKind::Cone, Instance::new(m, Vec3::new(0.055, 0.17, 0.055), [0.12, 0.12, 0.12], 0.9, 0.3, 0.0));
    }
    let pulse = 0.6 + 0.4 * (t * 6.0).sin();
    let m = model * Mat4::from_translation(Vec3::new(0.0, r * 1.02, 0.0));
    scene.push(MeshKind::Sphere, Instance::new(m, Vec3::splat(0.06), lamp, 0.0, 0.3, 1.5 * pulse));
}

/// The cursor: a small warm light hovering over the water, bobbing on it.
fn cursor_light(scene: &mut Scene, at: Option<(f32, f32)>, ripple: &Ripple, time: f32) -> [f32; 4] {
    let Some((x, z)) = at else { return [0.0; 4] };
    let (h, _) = water::surface(x, z, time);
    let y = h + ripple.height(x, z) + 0.32 + (time * 2.4).sin() * 0.025;
    scene.push(
        MeshKind::Sphere,
        Instance::new(Mat4::from_translation(Vec3::new(x, y, z)), Vec3::splat(0.055), [1.0, 0.8, 0.5], 0.0, 0.3, 3.0),
    );
    [x, y, z, 1.0]
}

fn seabed(scene: &mut Scene) {
    let mut bed =
        Instance::new(Mat4::from_translation(Vec3::new(0.0, -3.4, -10.0)), Vec3::new(140.0, 0.2, 140.0), [0.6, 0.55, 0.4], 0.0, 0.9, 0.0);
    bed.scale[3] = 1.0;
    scene.push(MeshKind::Cube, bed);
}

/// What the shaders need beyond the instances.
struct Uniforms<'a> {
    time: f32,
    dims: Dims,
    grid: bool,
    cells: &'a [f32],
    clouds: &'a [f32],
    /// The cell under the pointer, lit while it can be clicked.
    hover: Option<usize>,
    /// 0 playing, 1 lost, 2 won.
    status: f32,
    boom: [f32; 4],
    cursor: [f32; 4],
}

/// The shared tail of a scene: camera, sun, grid, clouds, water.
fn finish(scene: &mut Scene, cam: &gfx::Camera, u: Uniforms<'_>, ripple: &Ripple) {
    let mut cell_lanes = [[0.0f32; 4]; gfx::MAX_CELLS / 4];
    let mut cloud_lanes = [[0.0f32; 4]; gfx::MAX_CELLS / 4];
    for (i, v) in u.cells.iter().enumerate().take(gfx::MAX_CELLS) {
        cell_lanes[i / 4][i % 4] = *v;
    }
    for (i, v) in u.clouds.iter().enumerate().take(gfx::MAX_CELLS) {
        cloud_lanes[i / 4][i % 4] = *v;
    }
    let sun = Vec3::new(-0.35, 0.42, -0.84).normalize();
    ripple.texels(&mut scene.texels);
    scene.globals = Some(gfx::Globals {
        view_proj: cam.view_proj.to_cols_array_2d(),
        inv_view_proj: cam.view_proj.inverse().to_cols_array_2d(),
        cam: [cam.eye.x, cam.eye.y, cam.eye.z, u.time],
        sun: [sun.x, sun.y, sun.z, cam.back],
        info: [u.hover.map_or(-1.0, |h| h as f32), cam.pan.x, cam.pan.y, u.status],
        viewport: [0.0; 4],
        cursor: u.cursor,
        boom: u.boom,
        board: [u.dims.w as f32, u.dims.h as f32, if u.grid { 1.0 } else { 0.0 }, CLOUD_HEIGHT],
        sim: [ripple.min, ripple.size, ripple::N as f32, 1.0],
        cells: cell_lanes,
        clouds: cloud_lanes,
    });
}

/// A number cloud's code for the shaders: digit * 4 + its size while it puffs in.
fn cloud_code(digit: u8, age: f32) -> f32 {
    if age <= 0.0 {
        return 0.0;
    }
    let k = (age / CLOUD_IN).min(1.0);
    let grow = 1.0 - (1.0 - k).powi(3); // ease-out: puffs up fast, settles gently
    digit as f32 * 4.0 + (grow * 0.999).max(0.02)
}

/// A game's board: tiles (the one under `hover` lit), flags, mines, boat, wreck and clouds,
/// seen through `cam` (the app's, which picking uses too).
pub fn build_game(g: &Game, hover: Option<usize>, cam: &gfx::Camera) -> Scene {
    let sea = &g.sea;
    let mut scene = Scene::default();
    seabed(&mut scene);
    let time = sea.time - sea.draw_lag();
    let size = game::TILE_HALF.map(|v| v * 2.0);
    for tile in &g.tiles {
        let Some((pos, rot)) = sea.draw_pose(&tile.body) else { continue };
        let c = g.cells[tile.cell];
        let hovered = hover == Some(tile.cell) && g.status == Status::Playing && !c.open;
        let shade = 0.94 + 0.06 * ((tile.cell * 37 % 11) as f32 / 10.0);
        let color = if hovered { [0.95, 0.80, 0.55] } else { [0.52 * shade, 0.43 * shade, 0.32 * shade] };
        let model = tile_instance(&mut scene, pos, rot, size, color, sea.fade_of(&tile.body), if hovered { 0.05 } else { 0.0 });
        if c.flag {
            let pole = model * Mat4::from_translation(Vec3::new(-0.08, 0.07 + 0.24, 0.0));
            scene.push(MeshKind::Cube, Instance::new(pole, Vec3::new(0.035, 0.48, 0.035), [0.75, 0.75, 0.78], 0.9, 0.25, 0.0));
            let cloth = model * Mat4::from_translation(Vec3::new(0.05, 0.07 + 0.4, 0.0));
            scene.push(MeshKind::Cube, Instance::new(cloth, Vec3::new(0.24, 0.15, 0.02), [0.9, 0.12, 0.08], 0.0, 0.6, 0.12));
        }
    }
    let lamp = if g.status == Status::Won { [0.2, 1.0, 0.4] } else { [1.0, 0.1, 0.05] };
    for mine in &g.mines {
        let Some((pos, rot)) = sea.draw_pose(mine) else { continue };
        mine_instances(&mut scene, pos, rot, lamp, time);
    }
    let cursor = if g.status == Status::Playing { cursor_light(&mut scene, sea.cursor, &sea.ripple, time) } else { [0.0; 4] };
    if sea.boat.alive() {
        boat_instances(&mut scene, &sea.draw_boat());
    }
    debris_instances(&mut scene, sea);

    let cells: Vec<f32> = g.cells.iter().map(|c| if c.open { 1.0 } else { 0.0 }).collect();
    let clouds: Vec<f32> = g
        .cells
        .iter()
        .enumerate()
        .map(|(i, c)| if c.open && !c.mine && c.near > 0 { cloud_code(c.near, time - g.cloud_at[i]) } else { 0.0 })
        .collect();
    let status = match g.status {
        Status::Playing => 0.0,
        Status::Lost => 1.0,
        Status::Won => 2.0,
    };
    let boom = sea.boom.map_or([0.0; 4], |(x, z)| [x, z, (sea.boom_t - sea.draw_lag()).max(0.0), 1.0]);
    let u = Uniforms { time, dims: g.dims, grid: true, cells: &cells, clouds: &clouds, hover, status, boom, cursor };
    finish(&mut scene, cam, u, &sea.ripple);
    scene
}

/// The menu's sea: the menu tiles with their labels (the one `under` the pointer in its look), the
/// mine adrift, the boat or its wreck, decor clouds, seen through `cam` (the app's).
pub fn build_lobby(l: &Lobby, cam: &gfx::Camera, under: Option<(usize, Look)>) -> Scene {
    let sea = &l.sea;
    let mut scene = Scene::default();
    seabed(&mut scene);
    let time = sea.time - sea.draw_lag();
    for (k, tile) in l.tiles.iter().enumerate() {
        let Some((pos, rot)) = sea.draw_pose(&tile.body) else { continue };
        let shade = 0.95 + 0.05 * ((k * 37 % 11) as f32 / 10.0);
        let color = [0.62 * shade, 0.52 * shade, 0.38 * shade];
        let fade = sea.fade_of(&tile.body);
        tile_instance(&mut scene, pos, rot, lobby::ITEM, color, fade, 0.0);
        let (Some([v0, v1]), Some(inst)) = (labels::row_v(k), scene.inst[MeshKind::Cube as usize].last_mut()) else { continue };
        let look = match under {
            Some((u, look)) if u == k => look,
            _ => Look::Rest,
        };
        inst.label = [v0, v1, label_alpha(fade, tile.blown), 0.0];
        inst.mat[3] = look as u8 as f32;
    }
    for mine in &l.mine {
        let Some((pos, rot)) = sea.draw_pose(mine) else { continue };
        mine_instances(&mut scene, pos, rot, [1.0, 0.1, 0.05], time);
    }
    let cursor = cursor_light(&mut scene, sea.cursor, &sea.ripple, time);
    if sea.boat.alive() {
        boat_instances(&mut scene, &sea.draw_boat());
    }
    debris_instances(&mut scene, sea);

    let n = lobby::GRID.count();
    let cells = vec![0.0; n];
    let mut clouds = vec![0.0; n];
    for (k, &(cell, digit)) in l.clouds.iter().enumerate() {
        clouds[cell] = cloud_code(digit, time - 0.4 - k as f32 * 0.25);
    }
    let shows = if sea.boom_t < LOBBY_BOOM_SHOWS { 1.0 } else { 0.0 };
    let boom = sea.boom.map_or([0.0; 4], |(x, z)| [x, z, (sea.boom_t - sea.draw_lag()).max(0.0), shows]);
    let u = Uniforms { time, dims: lobby::GRID, grid: false, cells: &cells, clouds: &clouds, hover: None, status: 0.0, boom, cursor };
    finish(&mut scene, cam, u, &sea.ripple);
    scene
}
