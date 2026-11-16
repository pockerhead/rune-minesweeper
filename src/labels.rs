//! The menu's labels, painted on the planks: a second Rune runtime lays the labels out on a
//! sheet with one row per tile, a second Rune renderer draws the sheet into a mipmapped texture
//! when the labels change, and the plank shader (shaders/lit.wgsl) prints each tile's row on
//! its top face. The text then rides the plank: its tilt, bob, light, cloud shadows and its
//! reflection in the water.

use rune::render::{OFFSCREEN_FORMAT, Renderer};
use rune::{Color, Cx, El, Env, FontSmoothing, Runtime, TextStyle, el, row, text};

/// Rows on the sheet: the longest menu (the custom game) has five items.
pub const ROWS: usize = 5;
/// A row in points: the plank's top face (`lobby::ITEM`, 4.4 x 0.86) at 100 points a metre.
pub const STRIP: (f32, f32) = (440.0, 86.0);
/// Texture pixels per sheet point. A row is then 1540 px wide, more than a plank ever shows
/// on a phone (about 1050 device px at the menu's opening zoom) or a desktop.
const DENSITY: f32 = 3.5;
/// The labels' font size on the sheet, unless the longest needs less to fit.
pub const BASE_SIZE: f32 = 40.0;
/// How much of a row's width a label may take.
const ROOM: f32 = 0.9;
/// A custom game value's arrows sit this far in from the plank's ends.
const ARROW_INSET: f32 = 26.0;

/// A tile's row on the sheet as v from its top to its bottom (u always spans the sheet).
pub fn row_v(k: usize) -> Option<[f32; 2]> {
    (k < ROWS).then(|| [k as f32 / ROWS as f32, (k + 1) as f32 / ROWS as f32])
}

/// The font size that fits labels whose widest is `widest` points at `size` into `room`.
pub fn fit_size(size: f32, widest: f32, room: f32) -> f32 {
    if widest <= room || widest <= 0.0 { size } else { (size * room / widest * 0.98).floor() }
}

/// A label split for the sheet: a custom game value (`‹ Width 12 ›`) keeps its arrows at the
/// plank's ends, over the halves that step it; anything else is one centred text.
fn split(label: &str) -> (Option<(&str, &str)>, &str) {
    let trimmed = label.trim();
    match trimmed.strip_prefix('‹').and_then(|s| s.strip_suffix('›')) {
        Some(mid) => (Some(("‹", "›")), mid.trim()),
        None => (None, trimmed),
    }
}

struct Sheet {
    labels: Vec<String>,
    size: f32,
}

fn sheet_view(s: &Sheet, _cx: &mut Cx<Sheet>) -> El {
    let font = TextStyle::new(s.size, 750.0);
    let mut sheet = rune::col().w(STRIP.0).h(STRIP.1 * ROWS as f32);
    for (k, label) in s.labels.iter().take(ROWS).enumerate() {
        let (arrows, mid) = split(label);
        let body = text(mid.to_string()).key(format!("text{k}")).font(font).color(Color::WHITE);
        let strip = match arrows {
            Some((lo, hi)) => row()
                .size(STRIP.0, STRIP.1)
                .items_center()
                .justify_between()
                .pl(ARROW_INSET)
                .pr(ARROW_INSET)
                .child(text(lo).font(font).color(Color::WHITE))
                .child(body)
                .child(text(hi).font(font).color(Color::WHITE)),
            None => el().size(STRIP.0, STRIP.1).center().child(body),
        };
        sheet = sheet.child(strip.key(format!("row{k}")));
    }
    sheet
}

/// The sheet's layout: the labels and the one font size they all print at.
pub struct LabelSheet {
    rt: Runtime<Sheet>,
}

impl Default for LabelSheet {
    fn default() -> Self {
        let sheet = Sheet { labels: Vec::new(), size: BASE_SIZE };
        let mut rt = Runtime::new(sheet, sheet_view, (STRIP.0, STRIP.1 * ROWS as f32), Env::new(true, DENSITY));
        // coverage as is, the same on every platform: no text gamma keyed to the ink's colour,
        // since the shader picks the ink
        rt.set_font_smoothing(FontSmoothing::Windfoil);
        LabelSheet { rt }
    }
}

impl LabelSheet {
    /// Lays `labels` out, one per row, at `BASE_SIZE` or smaller so the widest fits its row
    /// (measured from Rune's layout). Returns the font size, in sheet points.
    pub fn lay_out(&mut self, labels: &[String]) -> f32 {
        self.build(labels, BASE_SIZE);
        let room = STRIP.0 * ROOM;
        let widest = (0..labels.len().min(ROWS))
            .filter_map(|k| {
                let width = self.rt.presented(&format!("text{k}"))?.0[2];
                // a value with arrows has the room between them, an arrow and a gap each side
                let ends = if split(&labels[k]).0.is_some() { 2.0 * (ARROW_INSET + BASE_SIZE * 0.9) } else { 0.0 };
                Some(width + ends)
            })
            .fold(0.0, f32::max);
        let size = fit_size(BASE_SIZE, widest, room);
        if size < BASE_SIZE {
            self.build(labels, size);
        }
        size
    }

    fn build(&mut self, labels: &[String], size: f32) {
        self.rt.state = Sheet { labels: labels.to_vec(), size };
        self.rt.settle_all();
        self.rt.frame(0.0);
    }
}

/// The sheet's texture and what draws it: Rune's renderer for the text, then a chain of
/// box-filtered mip levels so the labels don't shimmer at a distance or a slant.
pub struct LabelAtlas {
    renderer: Renderer,
    texture: wgpu::Texture,
    /// All mip levels, for the plank shader.
    pub view: wgpu::TextureView,
    /// Level 0 alone, which Rune draws into.
    top: wgpu::TextureView,
    mips: Vec<(wgpu::BindGroup, wgpu::TextureView)>,
    mip_pipe: wgpu::RenderPipeline,
    sheet: LabelSheet,
    schedule: Schedule,
    /// A digest of the sheet as painted.
    painted: u64,
}

/// Seconds between the once-a-second checks of a sheet at rest.
const RECHECK: f64 = 1.0;
/// Seconds between repaints while a custom game value keeps stepping (a held arrow).
const VALUE_REPAINT: f64 = 0.25;

/// What `LabelAtlas::update` does this frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    /// Lay the labels out and paint them.
    Paint,
    /// Lay the shown labels out again and paint only if that changed the sheet.
    Recheck,
    /// Nothing: the sheet shows these labels, or a value stepped too soon after the last paint
    /// (a later frame paints it).
    Idle,
}

/// When the sheet is painted: at once for new labels, at most every `VALUE_REPAINT` seconds
/// while only custom game values change, and a recheck every `RECHECK` seconds at rest.
#[derive(Default)]
struct Schedule {
    /// The labels the texture shows, and the clock it was painted and last compared at.
    shown: Option<Vec<String>>,
    painted_at: f64,
    checked: f64,
}

impl Schedule {
    fn step(&mut self, labels: &[String], clock: f64) -> Step {
        let Some(shown) = &self.shown else { return Step::Paint };
        if shown.as_slice() == labels {
            if (clock - self.checked).abs() < RECHECK {
                return Step::Idle;
            }
            self.checked = clock;
            return Step::Recheck;
        }
        if only_values_differ(shown, labels) && (clock - self.painted_at).abs() < VALUE_REPAINT {
            return Step::Idle;
        }
        Step::Paint
    }

    fn painted(&mut self, labels: &[String], clock: f64) {
        self.shown = Some(labels.to_vec());
        self.painted_at = clock;
        self.checked = clock;
    }
}

/// Whether `new` is `old` with only custom game values (`‹ Width 12 ›`) changed: the same menu
/// with a value stepped, not another menu.
fn only_values_differ(old: &[String], new: &[String]) -> bool {
    old.len() == new.len() && old.iter().zip(new).all(|(a, b)| a == b || (split(a).0.is_some() && split(b).0.is_some()))
}

fn digest(instances: &[rune::render::Instance]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytemuck::cast_slice::<_, u8>(instances).hash(&mut h);
    h.finish()
}

const MIP_SHADER: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex
fn vs(@builtin(vertex_index) i: u32) -> VOut {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: VOut;
    o.pos = vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2<f32>(uv.x, 1.0 - uv.y);
    return o;
}
@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    return textureSampleLevel(src, smp, i.uv, 0.0);
}
"#;

impl LabelAtlas {
    pub fn new(device: &wgpu::Device) -> LabelAtlas {
        let (w, h) = ((STRIP.0 * DENSITY) as u32, (STRIP.1 * DENSITY) as u32 * ROWS as u32);
        let levels = 32 - w.max(h).leading_zeros();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("labels"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OFFSCREEN_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let level = |l: u32| {
            texture.create_view(&wgpu::TextureViewDescriptor { base_mip_level: l, mip_level_count: Some(1), ..Default::default() })
        };
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("label mips"),
            source: wgpu::ShaderSource::Wgsl(MIP_SHADER.into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("label mips"),
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
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("label mips"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let mip_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("label mips"),
            layout: Some(&layout),
            vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: OFFSCREEN_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let linear = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("label mips"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        // each level is drawn from the one above it: a 2x2 box through the linear sampler
        let mips = (1..levels)
            .map(|l| {
                let src = level(l - 1);
                let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("label mip"),
                    layout: &bgl,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&src) },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&linear) },
                    ],
                });
                (bg, level(l))
            })
            .collect();
        LabelAtlas {
            renderer: Renderer::new(device, OFFSCREEN_FORMAT),
            view: texture.create_view(&Default::default()),
            top: level(0),
            texture,
            mips,
            mip_pipe,
            sheet: LabelSheet::default(),
            schedule: Schedule::default(),
            painted: 0,
        }
    }

    /// Paints `labels` into the texture if they differ from the ones it shows, in a submission
    /// of its own ahead of the frame's; a held custom value repaints a few times a second, and
    /// its last value once the hold ends (see `Schedule`). Once a second (`clock`, seconds) it also lays the
    /// sheet out again and repaints if that changed it: a browser fetches the full Inter faces
    /// after the first frame, and the text is then shaped anew.
    pub fn update(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, labels: &[String], clock: f64) {
        match self.schedule.step(labels, clock) {
            Step::Idle => return,
            Step::Recheck => {
                self.sheet.rt.frame(0.0);
                if digest(&self.sheet.rt.instances) == self.painted {
                    return;
                }
            }
            Step::Paint => {}
        }
        self.sheet.lay_out(labels);
        let rt = &mut self.sheet.rt;
        let size = (self.texture.width(), self.texture.height());
        self.renderer.prepare(device, queue, &mut rt.text.atlas, &rt.instances, size);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("labels") });
        self.renderer.draw_timed(&mut encoder, &self.top, [0.0; 4], None);
        for (bg, dst) in &self.mips {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("label mip"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dst,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.mip_pipe);
            pass.set_bind_group(0, bg, &[]);
            pass.draw(0..3, 0..1);
        }
        self.renderer.submit(queue, encoder);
        self.painted = digest(&rt.instances);
        self.schedule.painted(labels, clock);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_tile_the_sheet_without_overlap() {
        let mut last = 0.0;
        for k in 0..ROWS {
            let [v0, v1] = row_v(k).expect("a row");
            assert_eq!(v0, last, "row {k} does not start where the last ended");
            assert!(v1 > v0);
            last = v1;
        }
        assert_eq!(last, 1.0);
        assert_eq!(row_v(ROWS), None);
    }

    #[test]
    fn a_label_too_wide_shrinks_to_fit() {
        assert_eq!(fit_size(40.0, 300.0, 396.0), 40.0);
        let size = fit_size(40.0, 600.0, 396.0);
        assert!(size * 600.0 / 40.0 <= 396.0, "{size} pt still overflows");
        assert!(size >= 25.0);
    }

    #[test]
    fn custom_values_keep_their_arrows_apart() {
        assert_eq!(split("‹    Width  12    ›"), (Some(("‹", "›")), "Width  12"));
        assert_eq!(split("Play"), (None, "Play"));
    }

    fn texts(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// New labels paint at once; the same ones only get a recheck once a second.
    #[test]
    fn the_sheet_paints_new_labels_and_rechecks_at_rest() {
        let mut s = Schedule::default();
        let main = texts(&["Play", "Custom game", "Records"]);
        assert_eq!(s.step(&main, 0.0), Step::Paint);
        s.painted(&main, 0.0);
        assert_eq!(s.step(&main, 0.5), Step::Idle);
        assert_eq!(s.step(&main, 1.2), Step::Recheck);
        assert_eq!(s.step(&main, 1.5), Step::Idle, "a recheck waits another second");
        // another menu right after a paint still paints at once
        let levels = texts(&["Beginner", "Intermediate", "Expert", "Back"]);
        assert_eq!(s.step(&levels, 1.55), Step::Paint);
    }

    /// A held custom value repaints a few times a second, and its last value once let go.
    #[test]
    fn a_stepping_value_repaints_a_few_times_a_second() {
        let mut s = Schedule::default();
        let at = |w: u32| texts(&[&format!("‹  Width  {w}  ›"), "‹  Height  9  ›", "‹  Mines  10  ›", "Start", "Back"]);
        s.painted(&at(9), 0.0);
        // the first step after a while shows at once
        assert_eq!(s.step(&at(10), 2.0), Step::Paint);
        s.painted(&at(10), 2.0);
        // held: a step every 30 ms paints only every VALUE_REPAINT seconds
        let mut paints = 0;
        let mut t = 2.0;
        for w in 11..=44 {
            t += 0.03;
            if s.step(&at(w), t) == Step::Paint {
                s.painted(&at(w), t);
                paints += 1;
            }
        }
        assert!(paints <= 5, "{paints} paints in {:.2} s", t - 2.0);
        // let go at 44: a later frame paints the value the sheet doesn't show yet
        let last = at(44);
        if s.shown.as_deref() != Some(last.as_slice()) {
            assert_eq!(s.step(&last, t + VALUE_REPAINT), Step::Paint);
            s.painted(&last, t + VALUE_REPAINT);
        }
        assert_eq!(s.shown.as_deref(), Some(last.as_slice()));
    }

    #[test]
    fn only_custom_values_count_as_a_step() {
        let a = texts(&["‹ Width 9 ›", "Start"]);
        assert!(only_values_differ(&a, &texts(&["‹ Width 10 ›", "Start"])));
        assert!(!only_values_differ(&a, &texts(&["‹ Width 9 ›", "Back"])));
        assert!(!only_values_differ(&a, &texts(&["‹ Width 9 ›"])));
        assert!(!only_values_differ(&texts(&["Play"]), &texts(&["Records"])));
    }

    /// Every menu's labels fit their rows once laid out by Rune.
    #[test]
    fn laid_out_labels_fit_their_rows() {
        let mut sheet = LabelSheet::default();
        let long = vec!["Intermediate  ·  16×16  ·  40".to_string(), "Intermediate   ·   no record yet".to_string(), "Play".to_string()];
        let size = sheet.lay_out(&long);
        assert!(size < BASE_SIZE && size > 18.0, "size {size}");
        for k in 0..long.len() {
            let w = sheet.rt.presented(&format!("text{k}")).expect("laid out").0[2];
            assert!(w <= STRIP.0 * ROOM + 0.5, "label {k} is {w} pt wide");
        }
        assert_eq!(sheet.lay_out(&["Play".to_string(), "Records".to_string()]), BASE_SIZE);
        // a sheet at rest lays out the same again: the once-a-second check repaints nothing
        let painted = digest(&sheet.rt.instances);
        for _ in 0..3 {
            sheet.rt.frame(0.0);
            assert_eq!(digest(&sheet.rt.instances), painted, "the sheet changed at rest");
        }
    }
}
