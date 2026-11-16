//! The 3D scene: rendered into an HDR offscreen (colour + depth) with our own passes, then
//! blitted (ACES) into Rune's `El::effect` slot from the compositor hook.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec2, Vec3};
use rune::compose::{DeviceKey, Frame};
use rune::render::EffectSite;

use crate::ripple;

const HDR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Globals {
    pub view_proj: [[f32; 4]; 4],
    pub inv_view_proj: [[f32; 4]; 4],
    pub cam: [f32; 4],
    pub sun: [f32; 4],
    /// x the cell under the pointer (-1 none), y and z the camera's pan (x, z), w status (0
    /// play, 1 lost, 2 won).
    pub info: [f32; 4],
    /// The board's viewport in offscreen pixels: x, y, w, h (set by `Gpu::render`).
    pub viewport: [f32; 4],
    /// The cursor's light: xyz position, w strength (0 when off the board).
    pub cursor: [f32; 4],
    /// The last blast: x, z, seconds since, 1 while it shows.
    pub boom: [f32; 4],
    /// The cell grid: width, depth, 1 to draw its lines, height the clouds float at.
    pub board: [f32; 4],
    /// The interactive water square: low corner, side, texels per side.
    pub sim: [f32; 4],
    /// Per cell: 0 closed, 1 open.
    pub cells: [[f32; 4]; MAX_CELLS / 4],
    /// Per cell: its number cloud code, 0 for none (see shaders/clouds.wgsl).
    pub clouds: [[f32; 4]; MAX_CELLS / 4],
}

/// The largest board the shaders take (30 x 16).
pub const MAX_CELLS: usize = 480;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Instance {
    pub model: [[f32; 4]; 4],
    /// xyz scale, w kind (0 plain, 1 seabed)
    pub scale: [f32; 4],
    pub color: [f32; 4],
    /// metallic, roughness, emissive, and for a menu tile its label's look (0 at rest, 1
    /// hovered, 2 pressed)
    pub mat: [f32; 4],
    /// A menu tile's label on its top face: its row on the label sheet (v top, v bottom, see
    /// `labels::row_v`), and how much of it shows (0 none).
    pub label: [f32; 4],
}

impl Instance {
    pub fn new(model: Mat4, scale: Vec3, color: [f32; 3], metal: f32, rough: f32, emissive: f32) -> Instance {
        Instance {
            model: model.to_cols_array_2d(),
            scale: [scale.x, scale.y, scale.z, 0.0],
            color: [color[0], color[1], color[2], 1.0],
            mat: [metal, rough, emissive, 0.0],
            label: [0.0; 4],
        }
    }
}

#[derive(Clone, Copy)]
pub enum MeshKind {
    Cube = 0,
    Sphere = 1,
    Cone = 2,
}

#[derive(Default)]
pub struct Scene {
    pub globals: Option<Globals>,
    pub inst: [Vec<Instance>; 3],
    /// The interactive water, as `Ripple::texels` lays it out.
    pub texels: Vec<f32>,
}

impl Scene {
    pub fn push(&mut self, mesh: MeshKind, i: Instance) {
        self.inst[mesh as usize].push(i);
    }
}

pub struct Camera {
    pub view_proj: Mat4,
    pub eye: Vec3,
    /// How far the camera is pulled back: to fit a narrow board (1 at aspect 1.25 and wider),
    /// divided by the zoom.
    pub back: f32,
    /// How far the camera is moved over the water (x, z) from where the fit puts it.
    pub pan: Vec2,
}

impl Camera {
    /// The water point (x, z) under a point of the view in normalised device coordinates.
    pub fn water_at(&self, ndc: Vec2) -> Option<Vec2> {
        let inv = self.view_proj.inverse();
        let near = inv.project_point3(Vec3::new(ndc.x, ndc.y, 0.0));
        let far = inv.project_point3(Vec3::new(ndc.x, ndc.y, 1.0));
        let d = far - near;
        if d.y.abs() < 1e-5 {
            return None;
        }
        let t = -near.y / d.y;
        (t > 0.0).then_some(Vec2::new(near.x + d.x * t, near.z + d.z * t))
    }

    /// Where a world point shows, from the view's top-left (0, 0) to its bottom-right (1, 1).
    pub fn project(&self, p: Vec3) -> Vec2 {
        let c = self.view_proj * p.extend(1.0);
        Vec2::new(c.x / c.w * 0.5 + 0.5, 0.5 - c.y / c.w * 0.5)
    }
}

/// How far the camera pulls back to fit a board of half-extents `half` (x, z) in an aspect
/// ratio (w / h): framed for a 10 x 10 board in a 1.25 window, then pulled back to fit wider or
/// deeper. Boards under 10 wide are framed as 10 wide, except in a window narrower than 1.25,
/// where the board's own width counts (with a slimmer side margin) so a portrait phone does
/// not lose a third of its width.
fn fit_back(aspect: f32, half: (f32, f32)) -> f32 {
    let narrow = ((1.25 - aspect) / 0.45).clamp(0.0, 1.0);
    let wide = half.0.max(5.0);
    let width = wide + (half.0 * 0.87 - wide) * narrow;
    (half.1 / 5.0).max(width / 5.0 * 1.25 / aspect).max(0.85)
}

/// The camera over a board of half-extents `half` (x, z) for an aspect ratio (w / h), `zoom`
/// times closer than the fit (1 shows it all) and moved by `pan` over the water. The look
/// direction never changes: a pan moves eye and target alike.
pub fn look(aspect: f32, half: (f32, f32), zoom: f32, pan: Vec2) -> Camera {
    let back = fit_back(aspect, half) / zoom;
    let shift = Vec3::new(pan.x, 0.0, pan.y);
    let eye = Vec3::new(0.0, 12.0, 8.6) * back + shift;
    let target = Vec3::new(0.0, -0.3, 0.55 * back) + shift;
    let view = Mat4::look_at_rh(eye, target, Vec3::Y);
    let proj = Mat4::perspective_rh(38f32.to_radians(), aspect, 0.1, 200.0);
    Camera { view_proj: proj * view, eye, back, pan }
}

struct Mesh {
    vb: wgpu::Buffer,
    ib: wgpu::Buffer,
    n: u32,
}

struct Target {
    size: (u32, u32),
    color_tex: wgpu::Texture,
    color: wgpu::TextureView,
    refr_tex: wgpu::Texture,
    depth: wgpu::TextureView,
    blit_bg: wgpu::BindGroup,
    glass_bg: wgpu::BindGroup,
    water_bg: wgpu::BindGroup,
}

/// Render scale per axis at full quality for a frame of `size` device pixels, on a device whose
/// textures can be `max_dim` pixels wide: 2x supersampling where it fits, 1 otherwise.
fn supersample(size: (u32, u32), max_dim: u32) -> u32 {
    if size.0 * size.1 <= 2_600_000 && size.0.max(size.1) * 2 <= max_dim { 2 } else { 1 }
}

pub struct Gpu {
    key: DeviceKey,
    lit: wgpu::RenderPipeline,
    water: wgpu::RenderPipeline,
    clouds: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    water_bgl: wgpu::BindGroupLayout,
    scene_bg: wgpu::BindGroup,
    /// The label sheet the tiles print from (`labels`), with its sampler.
    label_bgl: wgpu::BindGroupLayout,
    label_sampler: wgpu::Sampler,
    label_bg: wgpu::BindGroup,
    ripple_tex: wgpu::Texture,
    blit_mod: wgpu::ShaderModule,
    blit_layout: wgpu::PipelineLayout,
    blit_bgl: wgpu::BindGroupLayout,
    blit_buf: wgpu::Buffer,
    blit_pipes: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    /// The end banner's glass (shaders/glass.wgsl): it reads the scene like the blit does.
    glass_mod: wgpu::ShaderModule,
    glass_buf: wgpu::Buffer,
    glass_pipes: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    sampler: wgpu::Sampler,
    meshes: [Mesh; 3],
    inst: [(wgpu::Buffer, usize); 3],
    water_mesh: Mesh,
    target: Option<Target>,
    res: Resolution,
    pace: GpuPace,
    /// When the 3D scene was last drawn (`scene_due`).
    last_scene: f64,
}

/// Dynamic resolution: the scene renders at `scale` device pixels per frame pixel and the blit
/// filters it to the frame. The scale follows the frame time, from `MIN_SCALE` up to the full
/// quality the device takes, so a phone keeps its frame rate and a desktop keeps supersampling.
/// Headless runs always render at full quality.
struct Resolution {
    scale: f32,
    /// Smoothed seconds per frame.
    frame: f32,
    /// Seconds since the scale last moved.
    since: f32,
}

/// Frames the GPU has really finished, counted from the queue's work-done callbacks.
///
/// In a browser the page can hand WebGPU frames faster than the GPU draws them (Safari on a
/// phone: 30 to 60 a second handed over, a handful drawn); the gap between frames then says
/// nothing about how the GPU copes, but these callbacks do. Native windows wait for the GPU
/// at present, so their frame gaps already tell the truth and this stays off there.
struct GpuPace {
    done: Arc<AtomicU32>,
    /// `done` and the time at the last measurement.
    seen: (u32, f64),
    /// Seconds per finished frame over the last window, and finished frames not yet reported.
    frame: f32,
    unreported: u32,
}

fn now_seconds() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now() / 1000.0
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
    }
}

impl GpuPace {
    fn new() -> GpuPace {
        GpuPace { done: Arc::new(AtomicU32::new(0)), seen: (0, now_seconds()), frame: 1.0 / 60.0, unreported: 0 }
    }

    /// Asks to be told when what has been submitted so far is done, and every quarter second
    /// turns the count of finished frames into seconds per frame.
    fn track(&mut self, queue: &wgpu::Queue) {
        let done = self.done.clone();
        queue.on_submitted_work_done(move || {
            done.fetch_add(1, Ordering::Relaxed);
        });
        let (now, count) = (now_seconds(), self.done.load(Ordering::Relaxed));
        let (count0, at0) = self.seen;
        if now - at0 >= 0.25 {
            let finished = count.wrapping_sub(count0);
            // none finished in the window: the GPU is at least that far behind
            self.frame = ((now - at0) / finished.max(1) as f64) as f32;
            self.unreported += finished;
            self.seen = (count, now);
        }
    }

    /// Finished frames since the last call, for the frame counter.
    fn take_finished(&mut self) -> u32 {
        std::mem::take(&mut self.unreported)
    }
}

/// The lowest render scale: in a browser down to a quarter of native resolution (a phone's
/// screen is dense enough for it), in a native window a third.
const MIN_SCALE: f32 = if cfg!(target_arch = "wasm32") { 0.25 } else { 0.35 };
/// Frame times that make the scale drop or rise (about 45 and 56 frames a second).
const SLOW_FRAME: f32 = 1.0 / 45.0;
const FAST_FRAME: f32 = 1.0 / 56.0;

/// Knobs a page's address can set, for finding out what a slow device chokes on:
/// `?scale=0.5` pins the render scale, `?cap=30` draws the 3D scene at most that many times a
/// second (the world still moves), `?lite=1` runs the cheapest shader loops (`?lite=0` turns
/// them off on a phone). Native builds and pages without them use the defaults.
///
/// Phones and tablets get a lighter default: the render scale tops out at `PHONE_SCALE` and the
/// shader loops are the cheap ones. Their browsers can report frames done that the screen never
/// shows (Safari on an iPhone 13 reports 58 a second at 0.7x while it shows a handful), so
/// the frame-time controller alone keeps them too heavy.
#[derive(Default, Clone, Copy)]
struct Tuning {
    scale: Option<f32>,
    cap: Option<f32>,
    lite: bool,
    /// The highest render scale the controller may reach.
    max_scale: Option<f32>,
}

#[cfg(target_arch = "wasm32")]
const PHONE_SCALE: f32 = 0.45;

/// A phone or tablet by its browser: a mobile user agent, or an iPad (which calls itself a Mac)
/// by its touch points.
#[cfg(target_arch = "wasm32")]
fn handheld() -> bool {
    let Some(nav) = web_sys::window().map(|w| w.navigator()) else { return false };
    let agent = nav.user_agent().unwrap_or_default();
    ["iPhone", "iPad", "Android", "Mobile"].iter().any(|k| agent.contains(k)) || (agent.contains("Macintosh") && nav.max_touch_points() > 1)
}

fn tuning() -> Tuning {
    static TUNING: std::sync::OnceLock<Tuning> = std::sync::OnceLock::new();
    *TUNING.get_or_init(|| {
        #[cfg(target_arch = "wasm32")]
        {
            let query = web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default();
            let mut t = Tuning::default();
            if handheld() {
                t.max_scale = Some(PHONE_SCALE);
                t.lite = true;
            }
            for pair in query.trim_start_matches('?').split('&') {
                let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                match key {
                    "scale" => t.scale = value.parse::<f32>().ok().map(|v| v.clamp(0.1, 2.0)),
                    "cap" => t.cap = value.parse::<f32>().ok().filter(|v| *v >= 1.0),
                    "lite" => t.lite = value != "0",
                    _ => {}
                }
            }
            t
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            Tuning::default()
        }
    })
}

impl Resolution {
    fn new() -> Resolution {
        // the browser starts at native resolution and earns supersampling; a native window
        // starts at full quality and gives it up only when it has to
        let scale = if cfg!(target_arch = "wasm32") { 1.0 } else { 4.0 };
        Resolution { scale, frame: 1.0 / 60.0, since: 0.0 }
    }

    /// The scale for this frame, after a frame that took `dt` seconds, at most `full`.
    fn step(&mut self, dt: f32, full: f32) -> f32 {
        if rune::is_headless() {
            self.scale = full;
            return full;
        }
        if let Some(pinned) = tuning().scale {
            self.scale = pinned.min(full);
            return self.scale;
        }
        let full = tuning().max_scale.map_or(full, |m| m.min(full));
        self.scale = self.scale.min(full);
        if dt > 0.0 && dt < 0.5 {
            self.frame += (dt - self.frame) * 0.1;
        }
        self.since += dt;
        if self.since > 0.4 {
            if self.frame > SLOW_FRAME && self.scale > MIN_SCALE {
                self.scale = (self.scale * 0.85).max(MIN_SCALE);
                self.since = 0.0;
            } else if self.frame < FAST_FRAME && self.scale < full {
                self.scale = (self.scale * 1.08).min(full);
                self.since = 0.0;
            }
        }
        self.scale
    }

    /// 0 at the lowest scale, 1 at native resolution and above: how many steps the costly
    /// shader loops (reflections, clouds) take.
    fn quality(&self) -> f32 {
        if tuning().lite {
            return 0.0;
        }
        ((self.scale - MIN_SCALE) / (1.0 - MIN_SCALE)).clamp(0.0, 1.0)
    }
}

fn label_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    view: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("labels"),
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
        ],
    })
}

fn shader(device: &wgpu::Device, label: &str, src: String) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(Cow::Owned(src)) })
}

fn buffer(device: &wgpu::Device, label: &str, data: &[u8], usage: wgpu::BufferUsages) -> wgpu::Buffer {
    use wgpu::util::DeviceExt;
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents: data, usage })
}

fn mesh(device: &wgpu::Device, (v, i): (Vec<[f32; 6]>, Vec<u32>)) -> Mesh {
    Mesh {
        vb: buffer(device, "vb", bytemuck::cast_slice(&v), wgpu::BufferUsages::VERTEX),
        ib: buffer(device, "ib", bytemuck::cast_slice(&i), wgpu::BufferUsages::INDEX),
        n: i.len() as u32,
    }
}

fn cube() -> (Vec<[f32; 6]>, Vec<u32>) {
    let mut v = Vec::new();
    let mut idx = Vec::new();
    let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
        ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        ([-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0]),
        ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
        ([0.0, -1.0, 0.0], [0.0, 0.0, -1.0], [1.0, 0.0, 0.0]),
        ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
    ];
    for (n, u, w) in faces {
        let base = v.len() as u32;
        for (su, sw) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let p = |k: usize| 0.5 * (n[k] + su * u[k] + sw * w[k]);
            v.push([p(0), p(1), p(2), n[0], n[1], n[2]]);
        }
        idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    (v, idx)
}

fn sphere() -> (Vec<[f32; 6]>, Vec<u32>) {
    let (lat, lon) = (18u32, 28u32);
    let mut v = Vec::new();
    for a in 0..=lat {
        let th = std::f32::consts::PI * a as f32 / lat as f32;
        for b in 0..=lon {
            let ph = std::f32::consts::TAU * b as f32 / lon as f32;
            let n = [th.sin() * ph.cos(), th.cos(), th.sin() * ph.sin()];
            v.push([n[0], n[1], n[2], n[0], n[1], n[2]]);
        }
    }
    let mut idx = Vec::new();
    for a in 0..lat {
        for b in 0..lon {
            let (i0, i1) = (a * (lon + 1) + b, (a + 1) * (lon + 1) + b);
            idx.extend_from_slice(&[i0, i1, i0 + 1, i0 + 1, i1, i1 + 1]);
        }
    }
    (v, idx)
}

fn cone() -> (Vec<[f32; 6]>, Vec<u32>) {
    let seg = 14u32;
    let mut v = Vec::new();
    let mut idx = Vec::new();
    let slope = 1.0f32 / (1.0f32 + 1.0).sqrt();
    for s in 0..seg {
        let (a0, a1) = (std::f32::consts::TAU * s as f32 / seg as f32, std::f32::consts::TAU * (s + 1) as f32 / seg as f32);
        let am = 0.5 * (a0 + a1);
        let base = v.len() as u32;
        let n = |a: f32| [a.cos() * slope, slope, a.sin() * slope];
        let (n0, n1, nm) = (n(a0), n(a1), n(am));
        v.push([a0.cos(), 0.0, a0.sin(), n0[0], n0[1], n0[2]]);
        v.push([a1.cos(), 0.0, a1.sin(), n1[0], n1[1], n1[2]]);
        v.push([0.0, 1.0, 0.0, nm[0], nm[1], nm[2]]);
        idx.extend_from_slice(&[base, base + 2, base + 1]);
    }
    (v, idx)
}

/// Coordinates along one axis: `step` apart over [lo, hi], then each gap 12 % wider than the
/// last out to `lo_end` and `hi_end`.
fn graded_axis(lo: f32, hi: f32, step: f32, lo_end: f32, hi_end: f32) -> Vec<f32> {
    let outward = |from: f32, to: f32, dir: f32| {
        let (mut x, mut s, mut out) = (from, step, Vec::new());
        while (to - x) * dir > 0.0 {
            s *= 1.12;
            x += s * dir;
            out.push(x);
        }
        out
    };
    let n = ((hi - lo) / step).round() as usize;
    let mut v: Vec<f32> = outward(lo, lo_end, -1.0).into_iter().rev().collect();
    v.extend((0..=n).map(|i| lo + i as f32 * step));
    v.extend(outward(hi, hi_end, 1.0));
    v
}

fn water_grid() -> (Vec<[f32; 2]>, Vec<u32>) {
    // fine (14 cm) over the largest board and its margin, coarser out to the horizon: the
    // swell is long, and the ripples and glints are shaded per pixel anyway
    let xs = graded_axis(-19.0, 19.0, 0.14, -200.0, 200.0);
    let zs = graded_axis(-14.0, 12.0, 0.14, -140.0, 40.0);
    let (nx, nz) = (xs.len() as u32, zs.len() as u32);
    let mut v = Vec::with_capacity((nx * nz) as usize);
    for &z in &zs {
        for &x in &xs {
            v.push([x, z]);
        }
    }
    let mut idx = Vec::with_capacity(((nx - 1) * (nz - 1) * 6) as usize);
    for j in 0..nz - 1 {
        for i in 0..nx - 1 {
            let a = j * nx + i;
            idx.extend_from_slice(&[a, a + nx, a + 1, a + 1, a + nx, a + nx + 1]);
        }
    }
    (v, idx)
}

impl Gpu {
    pub fn new<S: 'static>(f: &Frame<'_, S>) -> Gpu {
        let device = f.device;
        let common = include_str!("shaders/common.wgsl");
        let clouds_src = include_str!("shaders/clouds.wgsl");
        let lit_mod = shader(device, "lit", format!("{common}\n{clouds_src}\n{}", include_str!("shaders/lit.wgsl")));
        let water_mod = shader(device, "water", format!("{common}\n{clouds_src}\n{}", include_str!("shaders/water.wgsl")));
        let blit_mod = shader(device, "blit", include_str!("shaders/blit.wgsl").to_string());
        let glass_mod = shader(device, "glass", include_str!("shaders/glass.wgsl").to_string());

        let scene_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let ripple_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ripple"),
            size: wgpu::Extent3d { width: ripple::N as u32, height: ripple::N as u32, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rg32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let ripple_view = ripple_tex.create_view(&Default::default());
        let scene_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene"),
            layout: &scene_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&ripple_view) },
            ],
        });
        let label_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("labels"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        // trilinear and anisotropic: the planks are seen at a slant
        let label_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("labels"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
        // until the first labels are painted, a sheet with nothing on it
        let blank = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("no labels"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let label_bg = label_bind_group(device, &label_bgl, &label_sampler, &blank.create_view(&Default::default()));
        let lit_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lit"),
            bind_group_layouts: &[Some(&scene_bgl), Some(&label_bgl)],
            immediate_size: 0,
        });

        let f4 = |loc: u32, off: u64| wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: off, shader_location: loc };
        let mesh_attrs = [
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 12, shader_location: 1 },
        ];
        let inst_attrs = [f4(2, 0), f4(3, 16), f4(4, 32), f4(5, 48), f4(6, 64), f4(7, 80), f4(8, 96), f4(9, 112)];
        let lit = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lit"),
            layout: Some(&lit_layout),
            vertex: wgpu::VertexState {
                module: &lit_mod,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[
                    Some(wgpu::VertexBufferLayout { array_stride: 24, step_mode: wgpu::VertexStepMode::Vertex, attributes: &mesh_attrs }),
                    Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Instance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &inst_attrs,
                    }),
                ],
            },
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &lit_mod,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: HDR, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        // the scene behind the water: a copy of the opaque colour, and the depth buffer
        let tex_entry = |binding: u32, sample_type| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture { sample_type, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
            count: None,
        };
        let water_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("water scene"),
            entries: &[tex_entry(0, wgpu::TextureSampleType::Float { filterable: false }), tex_entry(1, wgpu::TextureSampleType::Depth)],
        });
        let water_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("water"),
            bind_group_layouts: &[Some(&scene_bgl), Some(&water_bgl)],
            immediate_size: 0,
        });
        let water_attrs = [wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 0, shader_location: 0 }];
        let water = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("water"),
            layout: Some(&water_layout),
            vertex: wgpu::VertexState {
                module: &water_mod,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 8,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &water_attrs,
                })],
            },
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &water_mod,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: HDR, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });

        // the number clouds: drawn in the water pass, blended over what is there
        let clouds = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("clouds"),
            layout: Some(&water_layout),
            vertex: wgpu::VertexState {
                module: &water_mod,
                entry_point: Some("cloud_vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &water_mod,
                entry_point: Some("cloud_fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: HDR, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let blit_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blit"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let blit_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit"),
            bind_group_layouts: &[Some(&blit_bgl)],
            immediate_size: 0,
        });
        let blit_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("blit"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let inst = std::array::from_fn(|_| {
            (
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("inst"),
                    size: 16,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                0,
            )
        });
        let (wv, wi) = water_grid();
        let water_mesh = Mesh {
            vb: buffer(device, "water vb", bytemuck::cast_slice(&wv), wgpu::BufferUsages::VERTEX),
            ib: buffer(device, "water ib", bytemuck::cast_slice(&wi), wgpu::BufferUsages::INDEX),
            n: wi.len() as u32,
        };
        Gpu {
            key: f.device_key(),
            lit,
            water,
            clouds,
            globals,
            water_bgl,
            scene_bg,
            label_bgl,
            label_sampler,
            label_bg,
            ripple_tex,
            blit_mod,
            blit_layout,
            blit_bgl,
            blit_buf,
            blit_pipes: HashMap::new(),
            glass_mod,
            glass_buf: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("glass"),
                size: 64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            glass_pipes: HashMap::new(),
            meshes: [mesh(device, cube()), mesh(device, sphere()), mesh(device, cone())],
            inst,
            water_mesh,
            target: None,
            sampler,
            res: Resolution::new(),
            pace: GpuPace::new(),
            last_scene: 0.0,
        }
    }

    /// Frames the GPU finished since the last call, in a browser; None in a native window,
    /// whose frames are finished when they are presented.
    pub fn finished_frames(&mut self) -> Option<u32> {
        cfg!(target_arch = "wasm32").then(|| self.pace.take_finished())
    }

    /// The render scale the last frame used (scene pixels per frame pixel).
    pub fn scale(&self) -> f32 {
        self.res.scale
    }

    /// Whether the 3D scene should be drawn this frame, under a `?cap=` limit (always without
    /// one). A skipped frame shows the last scene again.
    pub fn scene_due(&mut self) -> bool {
        let Some(cap) = tuning().cap else { return true };
        let now = now_seconds();
        if now - self.last_scene < 1.0 / cap as f64 - 0.002 {
            return false;
        }
        self.last_scene = now;
        true
    }

    pub fn key(&self) -> DeviceKey {
        self.key
    }

    /// The label sheet the menu tiles print from from now on (`labels::LabelAtlas::view`).
    pub fn set_labels(&mut self, device: &wgpu::Device, view: &wgpu::TextureView) {
        self.label_bg = label_bind_group(device, &self.label_bgl, &self.label_sampler, view);
    }

    fn ensure_blit(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) {
        if self.blit_pipes.contains_key(&format) {
            return;
        }
        let p = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&self.blit_layout),
            vertex: wgpu::VertexState {
                module: &self.blit_mod,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &self.blit_mod,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        self.blit_pipes.insert(format, p);
    }

    /// The glass pipeline for `format`: premultiplied, over what the slot's pass holds.
    fn ensure_glass(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) {
        if self.glass_pipes.contains_key(&format) {
            return;
        }
        let p = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glass"),
            layout: Some(&self.blit_layout),
            vertex: wgpu::VertexState {
                module: &self.glass_mod,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &self.glass_mod,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        self.glass_pipes.insert(format, p);
    }

    fn ensure_target(&mut self, device: &wgpu::Device, size: (u32, u32)) {
        if self.target.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let tex = |format, usage, label| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        use wgpu::TextureUsages as U;
        let color_tex = tex(HDR, U::RENDER_ATTACHMENT | U::TEXTURE_BINDING | U::COPY_SRC, "scene color");
        let refr_tex = tex(HDR, U::TEXTURE_BINDING | U::COPY_DST, "scene behind water");
        let depth_tex = tex(DEPTH, U::RENDER_ATTACHMENT | U::TEXTURE_BINDING, "scene depth");
        let color = color_tex.create_view(&Default::default());
        let refr = refr_tex.create_view(&Default::default());
        let depth = depth_tex.create_view(&wgpu::TextureViewDescriptor { aspect: wgpu::TextureAspect::DepthOnly, ..Default::default() });
        let water_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("water scene"),
            layout: &self.water_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&refr) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&depth) },
            ],
        });
        let blit_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit"),
            layout: &self.blit_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&color) },
                wgpu::BindGroupEntry { binding: 1, resource: self.blit_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        let glass_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glass"),
            layout: &self.blit_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&color) },
                wgpu::BindGroupEntry { binding: 1, resource: self.glass_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        self.target = Some(Target { size, color_tex, color, refr_tex, depth, blit_bg, glass_bg, water_bg });
    }

    /// Renders the scene into the offscreen at `viewport` (device px of the full frame);
    /// `blit` later copies it into the slot.
    pub fn render<S: 'static>(&mut self, f: &mut Frame<'_, S>, scene: &Scene, viewport: [f32; 4]) {
        let device = f.device;
        let queue = f.queue;
        // the target is sized for full quality; a lower scale uses its top-left part
        let full = supersample(f.size, device.limits().max_texture_dimension_2d);
        let size = (f.size.0 * full, f.size.1 * full);
        // the slower of the frame gaps and the GPU's own pace (browsers only, see `GpuPace`)
        let mut frame = f.dt as f32;
        if cfg!(target_arch = "wasm32") {
            self.pace.track(queue);
            frame = frame.max(self.pace.frame);
        }
        let ss = self.res.step(frame, full as f32);
        self.ensure_target(device, size);
        self.ensure_blit(device, f.format);
        self.ensure_blit(device, HDR);
        self.ensure_glass(device, f.format);
        self.ensure_glass(device, HDR);

        let [vx, vy, vw, vh] = viewport.map(|v| v * ss);
        let (fw, fh) = (size.0 as f32, size.1 as f32);
        let vx = vx.clamp(0.0, fw - 1.0);
        let vy = vy.clamp(0.0, fh - 1.0);
        let (vw, vh) = (vw.min(fw - vx).max(1.0), vh.min(fh - vy).max(1.0));
        if let Some(mut g) = scene.globals {
            g.viewport = [vx, vy, vw, vh];
            g.sim[3] = self.res.quality();
            queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&g));
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.ripple_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&scene.texels),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(ripple::N as u32 * 8), rows_per_image: None },
            wgpu::Extent3d { width: ripple::N as u32, height: ripple::N as u32, depth_or_array_layers: 1 },
        );
        for (k, list) in scene.inst.iter().enumerate() {
            let bytes = (list.len() * std::mem::size_of::<Instance>()) as u64;
            if bytes > self.inst[k].1 as u64 {
                let size = bytes.next_power_of_two().max(1024);
                self.inst[k] = (
                    device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("inst"),
                        size,
                        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }),
                    size as usize,
                );
            }
            if !list.is_empty() {
                queue.write_buffer(&self.inst[k].0, 0, bytemuck::cast_slice(list));
            }
        }
        let srgb = if f.srgb_blend() { 1.0 } else { 0.0 };
        let [bx, by, bw, bh] = viewport;
        queue.write_buffer(&self.blit_buf, 0, bytemuck::cast_slice(&[bx, by, srgb, 1.0, ss, 0.0, bw, bh]));

        let t = self.target.as_ref().expect("target");
        // 1. everything solid: seabed, tiles, mines
        let mut pass = f.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene opaque"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &t.color,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.02, g: 0.05, b: 0.07, a: 1.0 }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &t.depth,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_viewport(vx, vy, vw, vh, 0.0, 1.0);
        pass.set_bind_group(0, &self.scene_bg, &[]);
        pass.set_bind_group(1, &self.label_bg, &[]);
        pass.set_pipeline(&self.lit);
        for (k, m) in self.meshes.iter().enumerate() {
            let n = scene.inst[k].len() as u32;
            if n == 0 {
                continue;
            }
            pass.set_vertex_buffer(0, m.vb.slice(..));
            pass.set_vertex_buffer(1, self.inst[k].0.slice(..));
            pass.set_index_buffer(m.ib.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..m.n, 0, 0..n);
        }
        drop(pass);

        // 2. what the water refracts: a copy of the opaque scene
        let whole = wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 };
        f.encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &t.color_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &t.refr_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            whole,
        );

        // 3. the water, reading that copy and the depth (attached read-only)
        let mut pass = f.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene water"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &t.color,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &t.depth, depth_ops: None, stencil_ops: None }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_viewport(vx, vy, vw, vh, 0.0, 1.0);
        pass.set_bind_group(0, &self.scene_bg, &[]);
        pass.set_bind_group(1, &t.water_bg, &[]);
        pass.set_pipeline(&self.water);
        pass.set_vertex_buffer(0, self.water_mesh.vb.slice(..));
        pass.set_index_buffer(self.water_mesh.ib.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..self.water_mesh.n, 0, 0..1);
        pass.set_pipeline(&self.clouds);
        pass.draw(0..3, 0..1);
    }

    /// Whether the end banner can be drawn as glass over the scene in a frame of `format` (a
    /// scene has been drawn and the glass pipeline is built); `backdrop_blur` stands in until
    /// then.
    pub fn glass_ready(&self, format: wgpu::TextureFormat) -> bool {
        self.target.is_some() && self.glass_pipes.contains_key(&format)
    }

    /// Sets the glass for this frame: the banner's `rect` in frame device pixels, its corner
    /// `radius` and `scale` device pixels per point, at `seconds`, in a frame blended in sRGB
    /// (`srgb`) or not.
    pub fn set_glass(&self, queue: &wgpu::Queue, rect: [f32; 4], radius: f32, scale: f32, seconds: f32, srgb: bool) {
        // the rim bends the sea at most 0.3 of its width: 2 * bend < rim keeps it from folding
        let rim = 20.0 * scale;
        #[rustfmt::skip]
        let data = [
            0.0, 0.0, if srgb { 1.0 } else { 0.0 }, 1.0,
            self.res.scale, 0.0, 0.0, seconds,
            rect[0], rect[1], rect[2], rect[3],
            radius, rim, rim * 0.3, scale,
        ];
        queue.write_buffer(&self.glass_buf, 0, bytemuck::cast_slice(&data));
    }

    /// Draws the banner's glass into its effect slot's pass, over `rect` (frame device pixels,
    /// as given to `set_glass`).
    pub fn glass(&self, pass: &mut wgpu::RenderPass<'_>, site: &EffectSite<'_>, rect: [f32; 4]) {
        let (Some(p), Some(t)) = (self.glass_pipes.get(&site.format), self.target.as_ref()) else { return };
        let [ox, oy] = site.origin;
        let [x, y, w, h] = [rect[0] - ox, rect[1] - oy, rect[2], rect[3]];
        let c = site.instance.clip;
        let (sw, sh) = (site.size[0] as f32, site.size[1] as f32);
        let x0 = x.max(c[0].floor() - ox).clamp(0.0, sw).floor();
        let y0 = y.max(c[1].floor() - oy).clamp(0.0, sh).floor();
        let x1 = (x + w).min(c[2].ceil() - ox).clamp(0.0, sw).ceil();
        let y1 = (y + h).min(c[3].ceil() - oy).clamp(0.0, sh).ceil();
        if x1 <= x0 || y1 <= y0 || w < 1.0 || h < 1.0 {
            return;
        }
        pass.set_viewport(x, y, w, h, 0.0, 1.0);
        pass.set_scissor_rect(x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32);
        pass.set_pipeline(p);
        pass.set_bind_group(0, &t.glass_bg, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Draws the offscreen into an effect slot's pass. `board` is the board's rect in frame
    /// device pixels, as given to `render`; the slot's pass may be an isolated layer whose
    /// top-left sits at `site.origin` in the frame.
    pub fn blit(&self, pass: &mut wgpu::RenderPass<'_>, site: &EffectSite<'_>, board: [f32; 4]) {
        let (Some(p), Some(t)) = (self.blit_pipes.get(&site.format), self.target.as_ref()) else { return };
        let [ox, oy] = site.origin;
        let [x, y, w, h] = [board[0] - ox, board[1] - oy, board[2], board[3]];
        // draw only where the board and the slot's clip (Rune's scissor before the hook) overlap
        let c = site.instance.clip;
        let (sw, sh) = (site.size[0] as f32, site.size[1] as f32);
        let x0 = x.max(c[0].floor() - ox).clamp(0.0, sw).floor();
        let y0 = y.max(c[1].floor() - oy).clamp(0.0, sh).floor();
        let x1 = (x + w).min(c[2].ceil() - ox).clamp(0.0, sw).ceil();
        let y1 = (y + h).min(c[3].ceil() - oy).clamp(0.0, sh).ceil();
        if x1 <= x0 || y1 <= y0 || w < 1.0 || h < 1.0 {
            return;
        }
        // the viewport is the whole board, even where it hangs off the target: the shader maps
        // the fragment back to the frame pixel through it
        pass.set_viewport(x, y, w, h, 0.0, 1.0);
        pass.set_scissor_rect(x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32);
        pass.set_pipeline(p);
        pass.set_bind_group(0, &t.blit_bg, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::supersample;

    #[test]
    fn supersampling_stays_inside_the_texture_limit() {
        assert_eq!(supersample((1100, 860), 8192), 2);
        assert_eq!(supersample((4200, 600), 16384), 2);
        assert_eq!(supersample((4200, 600), 8192), 1);
        assert_eq!(supersample((2560, 1440), 16384), 1);
    }
}
