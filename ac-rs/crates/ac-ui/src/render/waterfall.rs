//! Waterfall view — scrolling-history GPU renderer.
//!
//! Each channel owns one layer inside an `R32Float` 2D texture array of size
//! `max_bins × ROWS_PER_CHANNEL × n_channels`. A 2D array (rather than one
//! tall packed 2D) keeps us under the 8192 max-texture-dimension limit on
//! integrated GPUs once the channel count grows past a few dozen. New rows
//! from `DisplayFrame::new_row` get written into the next ring slot for the
//! owning layer via `queue.write_texture` (origin.z = layer), and the WGSL
//! fragment shader looks them back up through a 256-entry inferno LUT baked
//! at build time (`build.rs` → `colormap.bin`).
//!
//! The renderer is allocation-aware: it grows `max_bins` (bin axis) and the
//! layer count on demand, the same way `SpectrumRenderer` does.

use bytemuck::{Pod, Zeroable};

pub const COLORMAP_LUT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/colormap.bin"));
pub const ROWS_PER_CHANNEL: u32 = 256;

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct WaterfallCellMeta {
    pub viewport:     [f32; 4],
    pub db_min:       f32,
    pub db_max:       f32,
    pub freq_log_min: f32,
    pub freq_log_max: f32,
    pub n_bins:       u32,
    pub n_rows:       u32,
    pub write_row:    u32,
    pub layer:        u32,
    /// Frequency of bin 0 in Hz (what the frame's `freqs[0]` held).
    pub freq_first:   f32,
    /// Frequency of the last bin in Hz.
    pub freq_last:    f32,
    /// 1 if the frame's `freqs` are log-spaced (synthetic), 0 if linear (real
    /// FFT output). Selects the bin-remap function in the shader so scroll
    /// zoom works on both.
    pub log_spaced:   u32,
    /// Number of newest rows stretched across the cell height. Must be
    /// `> 0 && <= n_rows`. Ctrl+scroll in waterfall mode shrinks this to zoom
    /// time; default is `n_rows` (show all history).
    pub rows_visible: u32,
}

pub struct CellUpload<'a> {
    pub channel:  usize,
    pub viewport: [f32; 4],
    pub db_min:   f32,
    pub db_max:   f32,
    pub freq_log_min: f32,
    pub freq_log_max: f32,
    pub n_bins:   u32,
    pub freq_first: f32,
    pub freq_last:  f32,
    pub log_spaced: bool,
    /// How many of the newest ring rows to stretch across the cell (time zoom).
    /// Clamped into `1..=ROWS_PER_CHANNEL` by the renderer.
    pub rows_visible: u32,
    /// Latest row to push into this channel's ring, if a fresh frame arrived
    /// since the previous redraw. `None` means re-use the existing texture
    /// contents (the ring keeps scrolling at the producer's rate, not ours).
    pub new_row:  Option<&'a [f32]>,
}

pub struct WaterfallRenderer {
    bind_group_layout: wgpu::BindGroupLayout,
    pipeline:          wgpu::RenderPipeline,
    cells_buf:         wgpu::Buffer,
    history_tex:       wgpu::Texture,
    history_view:      wgpu::TextureView,
    lut_view:          wgpu::TextureView,
    bind_group:        wgpu::BindGroup,
    cells_capacity:    usize,
    history_bins:      u32,
    history_layers:    u32,
    write_row:         Vec<u32>,
    active_cells:      u32,
}

impl WaterfallRenderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label:  Some("waterfall.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/waterfall.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("waterfall bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("waterfall pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let targets = [Some(wgpu::ColorTargetState {
            format,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        })];

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("waterfall pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &targets,
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let cells_capacity = 8_usize;
        let cells_buf = create_cells_buffer(device, cells_capacity);

        let history_bins = 1024_u32;
        let history_layers = 8_u32;
        let history_tex = create_history_texture(device, history_bins, history_layers);
        clear_history(queue, &history_tex, history_bins, history_layers);
        let history_view = history_tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });

        let lut_view = upload_lut(device, queue);

        let bind_group = make_bind_group(
            device,
            &bind_group_layout,
            &cells_buf,
            &history_view,
            &lut_view,
        );

        Self {
            bind_group_layout,
            pipeline,
            cells_buf,
            history_tex,
            history_view,
            lut_view,
            bind_group,
            cells_capacity,
            history_bins,
            history_layers,
            write_row: vec![0; history_layers as usize],
            active_cells: 0,
        }
    }

    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        n_total_channels: usize,
        uploads: &[CellUpload<'_>],
    ) {
        if uploads.is_empty() {
            self.active_cells = 0;
            return;
        }

        let max_bin_seen = uploads.iter().map(|u| u.n_bins).max().unwrap_or(0).max(1);
        let need_bins = max_bin_seen;
        let need_layers = n_total_channels.max(1) as u32;

        let mut realloc = false;
        if need_bins > self.history_bins || need_layers > self.history_layers {
            let new_bins = need_bins.next_power_of_two().max(self.history_bins);
            let new_layers = need_layers.max(self.history_layers);
            self.history_tex = create_history_texture(device, new_bins, new_layers);
            clear_history(queue, &self.history_tex, new_bins, new_layers);
            self.history_view = self.history_tex.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            });
            self.history_bins = new_bins;
            self.history_layers = new_layers;
            self.write_row = vec![0; new_layers as usize];
            realloc = true;
        }

        if uploads.len() > self.cells_capacity {
            let new_cap = uploads.len().next_power_of_two().max(8);
            self.cells_buf = create_cells_buffer(device, new_cap);
            self.cells_capacity = new_cap;
            realloc = true;
        }

        if realloc {
            self.bind_group = make_bind_group(
                device,
                &self.bind_group_layout,
                &self.cells_buf,
                &self.history_view,
                &self.lut_view,
            );
        }

        let mut metas: Vec<WaterfallCellMeta> = Vec::with_capacity(uploads.len());
        for u in uploads {
            if let Some(row) = u.new_row {
                if !row.is_empty() && u.channel < self.write_row.len() {
                    let n = row.len().min(self.history_bins as usize);
                    let mut padded;
                    let row_slice: &[f32] = if (n as u32) == self.history_bins {
                        &row[..n]
                    } else {
                        padded = vec![-200.0_f32; self.history_bins as usize];
                        padded[..n].copy_from_slice(&row[..n]);
                        &padded[..]
                    };
                    let row_idx = self.write_row[u.channel];
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &self.history_tex,
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: 0,
                                y: row_idx,
                                z: u.channel as u32,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        bytemuck::cast_slice(row_slice),
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(self.history_bins * 4),
                            rows_per_image: Some(1),
                        },
                        wgpu::Extent3d {
                            width: self.history_bins,
                            height: 1,
                            depth_or_array_layers: 1,
                        },
                    );
                    self.write_row[u.channel] = (row_idx + 1) % ROWS_PER_CHANNEL;
                }
            }

            let write_row = if u.channel < self.write_row.len() {
                self.write_row[u.channel]
            } else {
                0
            };

            metas.push(WaterfallCellMeta {
                viewport: u.viewport,
                db_min: u.db_min,
                db_max: u.db_max,
                freq_log_min: u.freq_log_min,
                freq_log_max: u.freq_log_max,
                n_bins: u.n_bins.min(self.history_bins),
                n_rows: ROWS_PER_CHANNEL,
                write_row,
                layer: u.channel as u32,
                freq_first: u.freq_first,
                freq_last:  u.freq_last,
                log_spaced: u.log_spaced as u32,
                rows_visible: u.rows_visible.clamp(1, ROWS_PER_CHANNEL),
            });
        }

        queue.write_buffer(&self.cells_buf, 0, bytemuck::cast_slice(&metas));
        self.active_cells = uploads.len() as u32;
    }

    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if self.active_cells == 0 {
            return;
        }
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_pipeline(&self.pipeline);
        pass.draw(0..4, 0..self.active_cells);
    }
}

fn create_cells_buffer(device: &wgpu::Device, count: usize) -> wgpu::Buffer {
    let size = (count * std::mem::size_of::<WaterfallCellMeta>()).max(64) as u64;
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("waterfall_cells"),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Fill every texel with -200.0 dBFS so unfilled rows render black instead of
/// the wgpu-default 0.0 (which maps to the hottest colormap color).
fn clear_history(queue: &wgpu::Queue, tex: &wgpu::Texture, bins: u32, layers: u32) {
    let row: Vec<f32> = vec![-200.0_f32; bins as usize];
    let row_bytes = bytemuck::cast_slice(&row);
    for layer in 0..layers {
        for row_idx in 0..ROWS_PER_CHANNEL {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: 0, y: row_idx, z: layer },
                    aspect: wgpu::TextureAspect::All,
                },
                row_bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bins * 4),
                    rows_per_image: Some(1),
                },
                wgpu::Extent3d { width: bins, height: 1, depth_or_array_layers: 1 },
            );
        }
    }
}

fn create_history_texture(device: &wgpu::Device, bins: u32, layers: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("waterfall_history"),
        size: wgpu::Extent3d {
            width: bins.max(1),
            height: ROWS_PER_CHANNEL,
            depth_or_array_layers: layers.max(1),
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

fn upload_lut(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    debug_assert_eq!(COLORMAP_LUT.len(), 256 * 4);
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("waterfall_lut"),
        size: wgpu::Extent3d { width: 256, height: 1, depth_or_array_layers: 1 },
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
        COLORMAP_LUT,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(256 * 4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d { width: 256, height: 1, depth_or_array_layers: 1 },
    );
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

fn make_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    cells: &wgpu::Buffer,
    history: &wgpu::TextureView,
    lut: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("waterfall bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: cells.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(history) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(lut) },
        ],
    })
}
