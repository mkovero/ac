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
    decay:     f32,
    scroll_dx: f32,
    _pad:      [f32; 2],
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

        // Vertex format: (x, y, w) — xy in cell-local [0,1], w is the per-vertex
        // confidence weight (γ²^k for coherence-aware transfer views, 1.0 for
        // views without a per-bin confidence signal).
        let point_attrs = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32,
                offset: 8,
                shader_location: 1,
            },
        ];
        let point_layout = [wgpu::VertexBufferLayout {
            array_stride: 12,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &point_attrs,
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
            size:  (POINT_CAPACITY * 12) as u64,
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

            // Tunable per-view via setters; defaults here suit a strip-chart
            // scope feeding ~800 line pairs/frame into a thin band. For a
            // spectrum view (sparse static polyline, no scroll) bump tau_p
            // up via `set_tau_p` so successive measurements fade-blend.
            tau_p:       0.6,
            intensity:   0.002,
            gamma:       0.6,
            gain:        0.5,
            palette_row: 0,
        }
    }

    pub fn set_tau_p(&mut self, tau_p: f32) { self.tau_p = tau_p.max(1e-3); }
    pub fn set_intensity(&mut self, v: f32) { self.intensity = v.max(0.0); }
    pub fn set_tone(&mut self, gamma: f32, gain: f32) {
        self.gamma = gamma.max(0.05);
        self.gain  = gain.max(0.0);
    }

    pub fn set_palette(&mut self, idx: u32) {
        self.palette_row = idx % LUT_PALETTES;
    }

    pub fn active_palette(&self) -> u32 { self.palette_row }

    /// Wipe the substrate to black on the next `advance()` call. Used by
    /// the Z key to give the user a clean slate when they switch test
    /// signals — otherwise prior content fades naturally over τ_p, which
    /// can confuse A/B comparisons that change input within ~1 s.
    pub fn request_clear(&mut self) {
        self.cleared = false;
    }

    /// Run decay + deposit. Must be called outside any surface render pass.
    ///
    /// `viewport` selects where `draw()` will land on the surface (in
    /// surface-normalised [0,1] coords; `(x,y)` = bottom-left, `(w,h)` = size).
    /// `line_pairs` is a flat LineList: every two consecutive vertices form
    /// one line segment. Each vertex is `[x, y, w]` where `w ∈ [0, 1]` is
    /// the per-vertex confidence weight applied multiplicatively to the
    /// global intensity (γ²^k for coherence-aware transfer views, 1.0 for
    /// views without a per-bin confidence signal). The caller controls
    /// connectivity — emit a pair for every connected segment, omit pairs
    /// where the polyline should break (e.g. spectrum bins below the dB
    /// floor).
    /// `scroll_dx_norm` ∈ [0,1] shifts the existing substrate leftward by
    /// that fraction of its width before depositing — pass 0.0 for a static
    /// view (e.g. spectrum), `dt / window_s` for a strip-chart scope.
    pub fn advance(
        &mut self,
        device: &wgpu::Device,
        queue:  &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        viewport: [f32; 4],
        line_pairs: &[[f32; 3]],
        scroll_dx_norm: f32,
        dt: f32,
    ) {
        let dt = dt.clamp(0.0, 0.25);

        if !self.cleared {
            clear_substrate(encoder, &self.front_view);
            clear_substrate(encoder, &self.back_view);
            self.cleared = true;
        }

        let decay = (-dt / self.tau_p).exp();
        let scroll_dx = scroll_dx_norm.clamp(0.0, 1.0);
        queue.write_buffer(
            &self.decay_uniform,
            0,
            bytemuck::bytes_of(&DecayU { decay, scroll_dx, _pad: [0.0; 2] }),
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

        let (src_view_for_decay_bg, dst_view) = if self.front_is_latest {
            (&self.decay_bg_from_front, &self.back_view)
        } else {
            (&self.decay_bg_from_back, &self.front_view)
        };

        // Caller built a LineList already — clamp count and upload as-is.
        // Each two consecutive vertices form one segment; broken polylines
        // (e.g. spectrum bins below the dB floor) skip the pair instead of
        // pinning a vertex at the floor.
        let n_pairs = (line_pairs.len() / 2).min(POINT_CAPACITY / 2);
        let used = n_pairs * 2;
        if used > 0 {
            queue.write_buffer(
                &self.point_vbuf,
                0,
                bytemuck::cast_slice(&line_pairs[..used]),
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
        if used > 0 {
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
            pass.draw(0..used as u32, 0..1);
        }

        self.front_is_latest = !self.front_is_latest;
        let _ = device;  // not used yet — kept for symmetry with set_palette / future on-the-fly resize
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
