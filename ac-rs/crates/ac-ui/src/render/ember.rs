//! Ember substrate — phosphor-persistence renderer.
//!
//! Single-cell prototype (unified.md Phase 0a). Three GPU passes per frame:
//!
//! 1. **Decay** — fullscreen quad reads the previous luminance buffer,
//!    multiplies by `exp(-Δt/τ_p)`, and writes to the ping-pong target.
//! 2. **Deposit** — point-list of `(x, y)` per audio sample, additive-
//!    blended into the same target.
//! 3. **Display** — fullscreen quad samples the target, applies a CRT
//!    phosphor tone curve, looks up a palette LUT, and writes RGB to the
//!    surface.
//!
//! Two `R16Float` textures alternate as src/dst so a render pass never
//! reads and writes the same texture, which wgpu forbids.
//!
//! Phase 0a drives the deposit stream from a synthetic 1 kHz sine generated
//! on the render thread. Real audio sources land in Phase 0b once the wire
//! protocol question (unified.md OQ7) is decided.

use std::time::Instant;

use bytemuck::{Pod, Zeroable};

const TEX_W: u32 = 1024;
const TEX_H: u32 = 512;
/// LineList pairs: each emitted line segment costs two vertices, and we want
/// headroom for 192 kHz × 30 fps worst case (~12 800 line endpoints).
const POINT_CAPACITY: usize = 32768;
const LUT_WIDTH: u32 = 256;
const LUT_PALETTES: u32 = 2;

pub const PALETTE_NAMES: &[&str] = &["blackbody", "warm"];

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct DecayU {
    decay: f32,
    _pad: [f32; 3],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct DepositU {
    intensity: f32,
    _pad: [f32; 3],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct DisplayU {
    viewport:    [f32; 4],
    gamma:       f32,
    gain:        f32,
    palette_row: u32,
    _pad:        f32,
}

pub struct EmberRenderer {
    decay_pipeline:   wgpu::RenderPipeline,
    deposit_pipeline: wgpu::RenderPipeline,
    display_pipeline: wgpu::RenderPipeline,

    front_tex: wgpu::Texture,
    back_tex:  wgpu::Texture,
    front_view: wgpu::TextureView,
    back_view:  wgpu::TextureView,

    decay_bg_from_front:   wgpu::BindGroup,
    decay_bg_from_back:    wgpu::BindGroup,
    display_bg_from_front: wgpu::BindGroup,
    display_bg_from_back:  wgpu::BindGroup,
    deposit_bg:            wgpu::BindGroup,

    decay_uniform:   wgpu::Buffer,
    deposit_uniform: wgpu::Buffer,
    display_uniform: wgpu::Buffer,

    point_vbuf: wgpu::Buffer,

    front_is_latest: bool,
    cleared:         bool,

    tau_p:       f32,
    intensity:   f32,
    gamma:       f32,
    gain:        f32,
    palette_row: u32,

    sample_rate:    f32,
    sweep_period_s: f32,
    sine_freq_hz:   f32,
    sine_phase:     f32,
    sample_counter: u64,
    last_tick:      Option<Instant>,

    point_scratch: Vec<[f32; 2]>,
}

impl EmberRenderer {
    pub fn new(
        device: &wgpu::Device,
        queue:  &wgpu::Queue,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let decay_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label:  Some("ember_decay.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/ember_decay.wgsl").into()),
        });
        let deposit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label:  Some("ember_deposit.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/ember_deposit.wgsl").into()),
        });
        let display_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label:  Some("ember_display.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/ember_display.wgsl").into()),
        });

        let decay_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ember decay bgl"),
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
        let deposit_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ember deposit bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let display_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ember display bgl"),
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
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let decay_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ember decay pl"),
            bind_group_layouts: &[&decay_bgl],
            push_constant_ranges: &[],
        });
        let deposit_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ember deposit pl"),
            bind_group_layouts: &[&deposit_bgl],
            push_constant_ranges: &[],
        });
        let display_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ember display pl"),
            bind_group_layouts: &[&display_bgl],
            push_constant_ranges: &[],
        });

        let r16f_target = [Some(wgpu::ColorTargetState {
            format: wgpu::TextureFormat::R16Float,
            blend: None,
            write_mask: wgpu::ColorWrites::RED,
        })];
        let r16f_target_add = [Some(wgpu::ColorTargetState {
            format: wgpu::TextureFormat::R16Float,
            blend: Some(wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation:  wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent::REPLACE,
            }),
            write_mask: wgpu::ColorWrites::RED,
        })];
        let surface_target = [Some(wgpu::ColorTargetState {
            format: surface_format,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        })];

        let decay_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ember decay"),
            layout: Some(&decay_layout),
            vertex: wgpu::VertexState {
                module: &decay_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &decay_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &r16f_target,
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let point_attr = wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: 0,
            shader_location: 0,
        };
        let point_layout = [wgpu::VertexBufferLayout {
            array_stride: 8,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: std::slice::from_ref(&point_attr),
        }];

        let deposit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ember deposit"),
            layout: Some(&deposit_layout),
            vertex: wgpu::VertexState {
                module: &deposit_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &point_layout,
            },
            fragment: Some(wgpu::FragmentState {
                module: &deposit_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &r16f_target_add,
            }),
            primitive: wgpu::PrimitiveState {
                // LineList so consecutive sample pairs draw a continuous
                // 1 px line. PointList leaves visible gaps when adjacent
                // samples are >1 px apart in screen space (768 samples /
                // frame across a 1024 px buffer leaves 4 px gaps).
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let display_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ember display"),
            layout: Some(&display_layout),
            vertex: wgpu::VertexState {
                module: &display_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &display_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &surface_target,
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let (front_tex, front_view) = create_substrate_texture(device, "ember front");
        let (back_tex,  back_view)  = create_substrate_texture(device, "ember back");

        let lut_view = create_palette_lut(device, queue);

        let decay_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ember decay uniform"),
            size:  std::mem::size_of::<DecayU>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let deposit_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ember deposit uniform"),
            size:  std::mem::size_of::<DepositU>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let display_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ember display uniform"),
            size:  std::mem::size_of::<DisplayU>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let decay_bg_from_front = make_decay_bg(device, &decay_bgl, &front_view, &decay_uniform);
        let decay_bg_from_back  = make_decay_bg(device, &decay_bgl, &back_view,  &decay_uniform);
        let display_bg_from_front =
            make_display_bg(device, &display_bgl, &front_view, &lut_view, &display_uniform);
        let display_bg_from_back  =
            make_display_bg(device, &display_bgl, &back_view,  &lut_view, &display_uniform);
        let deposit_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ember deposit bg"),
            layout: &deposit_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: deposit_uniform.as_entire_binding(),
            }],
        });

        let point_vbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ember points"),
            size:  (POINT_CAPACITY * 8) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            decay_pipeline,
            deposit_pipeline,
            display_pipeline,
            front_tex,
            back_tex,
            front_view,
            back_view,
            decay_bg_from_front,
            decay_bg_from_back,
            display_bg_from_front,
            display_bg_from_back,
            deposit_bg,
            decay_uniform,
            deposit_uniform,
            display_uniform,
            point_vbuf,
            front_is_latest: false,
            cleared: false,

            // Tuned for "ember on pure black": short-ish persistence so the
            // trace feels alive, gain × gamma curve such that a sustained
            // 1 kHz sine saturates the LUT to white-hot at peaks while
            // off-trace pixels stay at L=0 (black after the sqrt(t) floor
            // applied in the display shader).
            tau_p:       0.8,
            intensity:   0.12,
            gamma:       0.6,
            gain:        0.4,
            palette_row: 0,

            sample_rate:    48_000.0,
            sweep_period_s: 0.005,
            sine_freq_hz:   1_000.0,
            sine_phase:     0.0,
            sample_counter: 0,
            last_tick:      None,

            point_scratch: Vec::with_capacity(POINT_CAPACITY),
        }
    }

    pub fn set_palette(&mut self, idx: u32) {
        self.palette_row = idx % LUT_PALETTES;
    }

    pub fn active_palette(&self) -> u32 { self.palette_row }

    /// Run decay + deposit. Must be called outside any surface render pass.
    /// `viewport` selects where `draw()` will land on the surface (in
    /// surface-normalised [0,1] coords; (x,y) is the bottom-left and (w,h)
    /// the size).
    pub fn advance(
        &mut self,
        device: &wgpu::Device,
        queue:  &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        viewport: [f32; 4],
    ) {
        let now = Instant::now();
        let dt = self
            .last_tick
            .map(|t| now.saturating_duration_since(t).as_secs_f32())
            .unwrap_or(1.0 / 60.0)
            .min(0.25);
        self.last_tick = Some(now);

        if !self.cleared {
            clear_substrate(encoder, &self.front_view);
            clear_substrate(encoder, &self.back_view);
            self.cleared = true;
        }

        let decay = (-dt / self.tau_p.max(1e-3)).exp();
        queue.write_buffer(
            &self.decay_uniform,
            0,
            bytemuck::bytes_of(&DecayU { decay, _pad: [0.0; 3] }),
        );
        queue.write_buffer(
            &self.deposit_uniform,
            0,
            bytemuck::bytes_of(&DepositU { intensity: self.intensity, _pad: [0.0; 3] }),
        );
        queue.write_buffer(
            &self.display_uniform,
            0,
            bytemuck::bytes_of(&DisplayU {
                viewport,
                gamma:       self.gamma,
                gain:        self.gain,
                palette_row: self.palette_row,
                _pad:        0.0,
            }),
        );

        // Pick src/dst for this frame's ping-pong step.
        let (src_view_for_decay_bg, dst_view) = if self.front_is_latest {
            (&self.decay_bg_from_front, &self.back_view)
        } else {
            (&self.decay_bg_from_back, &self.front_view)
        };

        // Generate synthetic samples (Phase 0a).
        // LineList → emit each consecutive sample pair as two vertices, skip
        // pairs that straddle a wraparound (where x decreases) to avoid a
        // ghost line dragged across the cell at every sweep boundary.
        self.point_scratch.clear();
        let n_samples = ((dt * self.sample_rate) as usize).min(POINT_CAPACITY / 2);
        let two_pi = std::f32::consts::TAU;
        let phase_step = two_pi * self.sine_freq_hz / self.sample_rate;
        let sweep_samples = (self.sweep_period_s * self.sample_rate).max(1.0);
        let mut prev: Option<[f32; 2]> = None;
        for _ in 0..n_samples {
            let s = self.sine_phase.sin();
            self.sine_phase = (self.sine_phase + phase_step) % two_pi;
            let counter = self.sample_counter;
            self.sample_counter = self.sample_counter.wrapping_add(1);
            let x = (counter as f32 % sweep_samples) / sweep_samples;
            let y = 0.5 + 0.45 * s;
            let cur = [x, y];
            if let Some(p) = prev {
                if cur[0] >= p[0] {
                    self.point_scratch.push(p);
                    self.point_scratch.push(cur);
                }
            }
            prev = Some(cur);
        }
        if !self.point_scratch.is_empty() {
            queue.write_buffer(
                &self.point_vbuf,
                0,
                bytemuck::cast_slice(&self.point_scratch),
            );
        }

        // Decay pass: src → dst.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ember decay pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dst_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load:  wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.decay_pipeline);
            pass.set_bind_group(0, src_view_for_decay_bg, &[]);
            pass.draw(0..4, 0..1);
        }

        // Deposit pass: additive into dst (which now holds decayed values).
        if !self.point_scratch.is_empty() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ember deposit pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dst_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load:  wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.deposit_pipeline);
            pass.set_bind_group(0, &self.deposit_bg, &[]);
            pass.set_vertex_buffer(0, self.point_vbuf.slice(..));
            pass.draw(0..self.point_scratch.len() as u32, 0..1);
        }

        self.front_is_latest = !self.front_is_latest;
        let _ = device;
    }

    /// Render the latest substrate to the surface.
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        let bg = if self.front_is_latest {
            &self.display_bg_from_front
        } else {
            &self.display_bg_from_back
        };
        pass.set_pipeline(&self.display_pipeline);
        pass.set_bind_group(0, bg, &[]);
        pass.draw(0..4, 0..1);
    }
}

fn create_substrate_texture(
    device: &wgpu::Device,
    label: &'static str,
) -> (wgpu::Texture, wgpu::TextureView) {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: TEX_W, height: TEX_H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R16Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    (tex, view)
}

fn clear_substrate(encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
    let _ = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("ember clear"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load:  wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
    });
}

fn make_decay_bg(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    src_view: &wgpu::TextureView,
    uniform: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ember decay bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(src_view) },
            wgpu::BindGroupEntry { binding: 1, resource: uniform.as_entire_binding() },
        ],
    })
}

fn make_display_bg(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    src_view: &wgpu::TextureView,
    lut_view: &wgpu::TextureView,
    uniform: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ember display bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(src_view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(lut_view) },
            wgpu::BindGroupEntry { binding: 2, resource: uniform.as_entire_binding() },
        ],
    })
}

/// Bake two 256-entry RGBA8 palettes into a `2 × 256` LUT texture.
///
/// Row 0 — blackbody: Tanner-Helland Planck approximation, mapped from
/// 1000 K (deep red) up to 10 000 K (white-blue).
/// Row 1 — warm: hand-tuned interpolation black → red → orange → yellow →
/// white. More perceptually uniform than blackbody at the low end (the
/// readability concern recorded as unified.md OQ3).
fn create_palette_lut(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    let n = LUT_WIDTH as usize;
    let mut data = vec![0u8; (LUT_WIDTH * LUT_PALETTES * 4) as usize];

    for i in 0..n {
        let t = i as f32 / (n - 1) as f32;
        let temp_k = 1000.0 + t * 9000.0;
        let (r, g, b) = blackbody_rgb(temp_k);
        let off = i * 4;
        data[off]     = (r * 255.0) as u8;
        data[off + 1] = (g * 255.0) as u8;
        data[off + 2] = (b * 255.0) as u8;
        data[off + 3] = 255;
    }

    let warm_stops: [(f32, [f32; 3]); 5] = [
        (0.00, [0.00, 0.00, 0.00]),
        (0.25, [0.40, 0.05, 0.00]),
        (0.55, [1.00, 0.35, 0.00]),
        (0.80, [1.00, 0.85, 0.20]),
        (1.00, [1.00, 1.00, 0.95]),
    ];
    for i in 0..n {
        let t = i as f32 / (n - 1) as f32;
        let mut rgb = warm_stops[0].1;
        for w in warm_stops.windows(2) {
            let (t0, c0) = w[0];
            let (t1, c1) = w[1];
            if t >= t0 && t <= t1 {
                let f = (t - t0) / (t1 - t0).max(1e-6);
                rgb = [
                    c0[0] + (c1[0] - c0[0]) * f,
                    c0[1] + (c1[1] - c0[1]) * f,
                    c0[2] + (c1[2] - c0[2]) * f,
                ];
                break;
            }
        }
        let off = (LUT_WIDTH as usize + i) * 4;
        data[off]     = (rgb[0] * 255.0) as u8;
        data[off + 1] = (rgb[1] * 255.0) as u8;
        data[off + 2] = (rgb[2] * 255.0) as u8;
        data[off + 3] = 255;
    }

    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ember palette lut"),
        size: wgpu::Extent3d {
            width: LUT_WIDTH,
            height: LUT_PALETTES,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(LUT_WIDTH * 4),
            rows_per_image: Some(LUT_PALETTES),
        },
        wgpu::Extent3d {
            width: LUT_WIDTH,
            height: LUT_PALETTES,
            depth_or_array_layers: 1,
        },
    );
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

fn blackbody_rgb(temp_k: f32) -> (f32, f32, f32) {
    // Tanner Helland's piecewise approximation; output normalised to [0,1].
    let t = temp_k / 100.0;
    let r = if t <= 66.0 {
        1.0
    } else {
        (329.698_73 * (t - 60.0).powf(-0.133_205_1) / 255.0).clamp(0.0, 1.0)
    };
    let g = if t <= 66.0 {
        (99.470_802 * t.ln() - 161.119_57) / 255.0
    } else {
        288.122_2 * (t - 60.0).powf(-0.075_514_85) / 255.0
    }.clamp(0.0, 1.0);
    let b = if t >= 66.0 {
        1.0
    } else if t <= 19.0 {
        0.0
    } else {
        ((138.517_73 * (t - 10.0).ln() - 305.044_8) / 255.0).clamp(0.0, 1.0)
    };
    (r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blackbody_endpoints_are_in_range() {
        for k in (1000..=10_000).step_by(500) {
            let (r, g, b) = blackbody_rgb(k as f32);
            assert!((0.0..=1.0).contains(&r), "r out of range at {k}K: {r}");
            assert!((0.0..=1.0).contains(&g), "g out of range at {k}K: {g}");
            assert!((0.0..=1.0).contains(&b), "b out of range at {k}K: {b}");
        }
    }

    #[test]
    fn blackbody_low_temp_is_red_dominant() {
        let (r, g, b) = blackbody_rgb(1500.0);
        assert!(r > g, "expected r > g at 1500K, got r={r} g={g}");
        assert!(r > b, "expected r > b at 1500K, got r={r} b={b}");
    }

    #[test]
    fn blackbody_high_temp_is_blue_dominant() {
        let (r, g, b) = blackbody_rgb(9500.0);
        assert!(b >= 0.95, "expected b≈1 at 9500K, got b={b}");
        let _ = r;
        let _ = g;
    }
}
