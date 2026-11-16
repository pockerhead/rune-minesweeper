//! The 3D scene: rendered into an HDR offscreen (colour + depth) with our own passes, then
//! blitted (ACES) into Rune's `El::effect` slot from the compositor hook.

use std::borrow::Cow;
use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
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
    /// x the cell under the pointer (-1 none), y and z unused, w status (0 play, 1 lost, 2 won).
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
    /// metallic, roughness, emissive
    pub mat: [f32; 4],
}

impl Instance {
    pub fn new(model: Mat4, scale: Vec3, color: [f32; 3], metal: f32, rough: f32, emissive: f32) -> Instance {
        Instance {
            model: model.to_cols_array_2d(),
            scale: [scale.x, scale.y, scale.z, 0.0],
            color: [color[0], color[1], color[2], 1.0],
            mat: [metal, rough, emissive, 0.0],
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
    /// How far the camera is pulled back to fit a narrow board (1 at aspect 1.25 and wider).
    pub back: f32,
}

/// The fixed camera over a board of half-extents `half` (x, z) for an aspect ratio (w / h).
pub fn camera(aspect: f32, half: (f32, f32)) -> Camera {
    // framed for a 10 x 10 board in a 1.25 window, then pulled back to fit wider or deeper
    let back = (half.1 / 5.0).max(half.0 / 5.0 * 1.25 / aspect).max(1.25 / aspect).max(0.85);
    let eye = Vec3::new(0.0, 12.0, 8.6) * back;
    let target = Vec3::new(0.0, -0.3, 0.55 * back);
    let view = Mat4::look_at_rh(eye, target, Vec3::Y);
    let proj = Mat4::perspective_rh(38f32.to_radians(), aspect, 0.1, 200.0);
    Camera { view_proj: proj * view, eye, back }
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
    water_bg: wgpu::BindGroup,
}

/// Supersampling factor per axis for a frame of `size` device pixels, on a device whose
/// textures can be `max_dim` pixels wide.
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
    ripple_tex: wgpu::Texture,
    blit_mod: wgpu::ShaderModule,
    blit_layout: wgpu::PipelineLayout,
    blit_bgl: wgpu::BindGroupLayout,
    blit_buf: wgpu::Buffer,
    blit_pipes: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    meshes: [Mesh; 3],
    inst: [(wgpu::Buffer, usize); 3],
    water_mesh: Mesh,
    target: Option<Target>,
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

fn water_grid() -> (Vec<[f32; 2]>, Vec<u32>) {
    let step = 0.1;
    // fine over x in [-32, 32]; past that (only very wide windows look so far sideways) the
    // columns widen by 15 % each until x reaches 200, so the water never ends in view
    let mut outer = Vec::new();
    let (mut x, mut s) = (32.0f32, step);
    while x < 200.0 {
        s *= 1.15;
        x += s;
        outer.push(x);
    }
    let xs: Vec<f32> =
        outer.iter().rev().map(|x| -x).chain((0..=640).map(|i| -32.0 + i as f32 * step)).chain(outer.iter().copied()).collect();
    let zs: Vec<f32> = (0..=580).map(|j| -42.0 + j as f32 * step).collect();
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
        let scene_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene"),
            bind_group_layouts: &[Some(&scene_bgl)],
            immediate_size: 0,
        });

        let f4 = |loc: u32, off: u64| wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: off, shader_location: loc };
        let mesh_attrs = [
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 12, shader_location: 1 },
        ];
        let inst_attrs = [f4(2, 0), f4(3, 16), f4(4, 32), f4(5, 48), f4(6, 64), f4(7, 80), f4(8, 96)];
        let lit = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lit"),
            layout: Some(&scene_layout),
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
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
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
            ],
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
            ripple_tex,
            blit_mod,
            blit_layout,
            blit_bgl,
            blit_buf,
            blit_pipes: HashMap::new(),
            meshes: [mesh(device, cube()), mesh(device, sphere()), mesh(device, cone())],
            inst,
            water_mesh,
            target: None,
        }
    }

    pub fn key(&self) -> DeviceKey {
        self.key
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
            ],
        });
        self.target = Some(Target { size, color_tex, color, refr_tex, depth, blit_bg, water_bg });
    }

    /// Renders the scene into the offscreen at `viewport` (device px of the full frame);
    /// `blit` later copies it into the slot.
    pub fn render<S: 'static>(&mut self, f: &mut Frame<'_, S>, scene: &Scene, viewport: [f32; 4]) {
        let device = f.device;
        let queue = f.queue;
        let ss = supersample(f.size, device.limits().max_texture_dimension_2d);
        let size = (f.size.0 * ss, f.size.1 * ss);
        self.ensure_target(device, size);
        self.ensure_blit(device, f.format);
        self.ensure_blit(device, HDR);

        let [vx, vy, vw, vh] = viewport.map(|v| v * ss as f32);
        let (fw, fh) = (size.0 as f32, size.1 as f32);
        let vx = vx.clamp(0.0, fw - 1.0);
        let vy = vy.clamp(0.0, fh - 1.0);
        let (vw, vh) = (vw.min(fw - vx).max(1.0), vh.min(fh - vy).max(1.0));
        if let Some(mut g) = scene.globals {
            g.viewport = [vx, vy, vw, vh];
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
        queue.write_buffer(&self.blit_buf, 0, bytemuck::cast_slice(&[bx, by, srgb, 1.0, ss as f32, 0.0, bw, bh]));

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
