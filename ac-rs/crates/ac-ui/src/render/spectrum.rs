use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct ChannelMeta {
    pub color: [f32; 4],
    pub viewport: [f32; 4],
    pub db_min: f32,
    pub db_max: f32,
    pub freq_log_min: f32,
    pub freq_log_max: f32,
    pub n_bins: u32,
    pub offset: u32,
    pub fill_alpha: f32,
    pub line_width: f32,
}

pub struct ChannelUpload {
    pub spectrum: Vec<f32>,
    pub meta: ChannelMeta,
}

pub struct SpectrumRenderer {
    bind_group_layout: wgpu::BindGroupLayout,
    pipeline_line: wgpu::RenderPipeline,
    pipeline_fill: wgpu::RenderPipeline,
    spectrum_buf: wgpu::Buffer,
    channel_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    capacity_floats: usize,
    capacity_channels: usize,
    active_channels: u32,
    max_bins: u32,
    /// Reused per-frame staging for the packed spectrum upload. Pre-#109
    /// we allocated `Vec::with_capacity(n_channels × max_bins)` on every
    /// `upload()` call — at 8 ch × 4096 bins × 60 fps that's ~7.7 MB/s
    /// of allocation thrash plus the same in NVIDIA staging-buffer churn
    /// from `queue.write_buffer`. Hold the buffer across frames; only
    /// the inner `resize` triggers heap work, and only when the channel
    /// count or bin count grows.
    spectrum_packed: Vec<f32>,
    metas: Vec<ChannelMeta>,
}

impl SpectrumRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("spectrum.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/spectrum.wgsl").into()),
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("spectrum bgl"),
                entries: &[storage_entry(0), storage_entry(1)],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("spectrum pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let blend = Some(wgpu::BlendState::ALPHA_BLENDING);
        let targets = [Some(wgpu::ColorTargetState {
            format,
            blend,
            write_mask: wgpu::ColorWrites::ALL,
        })];

        let make_pipeline = |label: &str, vs: &str, fs: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
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
            })
        };

        let pipeline_fill = make_pipeline("spectrum fill", "vs_fill", "fs_fill");
        let pipeline_line = make_pipeline("spectrum line", "vs_line", "fs_line");

        let capacity_floats = 4096_usize;
        let capacity_channels = 8_usize;

        let spectrum_buf = create_storage::<f32>(device, "spectrum_data", capacity_floats);
        let channel_buf = create_channel_buf(device, capacity_channels);
        let bind_group = make_bind_group(
            device,
            &bind_group_layout,
            &spectrum_buf,
            &channel_buf,
        );

        Self {
            bind_group_layout,
            pipeline_line,
            pipeline_fill,
            spectrum_buf,
            channel_buf,
            bind_group,
            capacity_floats,
            capacity_channels,
            active_channels: 0,
            max_bins: 0,
            spectrum_packed: Vec::new(),
            metas: Vec::new(),
        }
    }

    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        uploads: &[ChannelUpload],
    ) {
        if uploads.is_empty() {
            self.active_channels = 0;
            self.max_bins = 0;
            return;
        }
        let max_bins = uploads.iter().map(|u| u.spectrum.len()).max().unwrap_or(0);
        let total_floats = max_bins * uploads.len();

        let mut reallocated = false;
        if total_floats > self.capacity_floats {
            let new_cap = total_floats.next_power_of_two().max(4096);
            self.spectrum_buf = create_storage::<f32>(device, "spectrum_data", new_cap);
            self.capacity_floats = new_cap;
            reallocated = true;
        }
        if uploads.len() > self.capacity_channels {
            let new_cap = uploads.len().next_power_of_two().max(8);
            self.channel_buf = create_channel_buf(device, new_cap);
            self.capacity_channels = new_cap;
            reallocated = true;
        }
        if reallocated {
            self.bind_group = make_bind_group(
                device,
                &self.bind_group_layout,
                &self.spectrum_buf,
                &self.channel_buf,
            );
        }

        // Reuse the persistent scratch buffers; `resize` is a no-op once
        // they reach steady-state capacity. Avoids ~7.7 MB/s of heap
        // alloc + free on a typical 8 ch × 4096 bin × 60 fps workload.
        self.spectrum_packed.clear();
        self.spectrum_packed.resize(total_floats, 0.0_f32);
        self.metas.clear();
        self.metas.reserve(uploads.len());
        for (idx, u) in uploads.iter().enumerate() {
            let off = idx * max_bins;
            let n = u.spectrum.len();
            self.spectrum_packed[off..off + n].copy_from_slice(&u.spectrum);
            if n < max_bins {
                let last_s = *u.spectrum.last().unwrap_or(&-140.0);
                for j in n..max_bins {
                    self.spectrum_packed[off + j] = last_s;
                }
            }
            let mut meta = u.meta;
            meta.offset = off as u32;
            meta.n_bins = n as u32;
            self.metas.push(meta);
        }

        queue.write_buffer(
            &self.spectrum_buf,
            0,
            bytemuck::cast_slice(&self.spectrum_packed),
        );
        queue.write_buffer(&self.channel_buf, 0, bytemuck::cast_slice(&self.metas));

        self.active_channels = uploads.len() as u32;
        self.max_bins = max_bins as u32;
    }

    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if self.active_channels == 0 || self.max_bins == 0 {
            return;
        }
        let verts = self.max_bins * 2;
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_pipeline(&self.pipeline_fill);
        pass.draw(0..verts, 0..self.active_channels);
        pass.set_pipeline(&self.pipeline_line);
        pass.draw(0..verts, 0..self.active_channels);
    }
}

fn storage_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn create_storage<T: bytemuck::Pod>(
    device: &wgpu::Device,
    label: &str,
    count: usize,
) -> wgpu::Buffer {
    let size = (count * std::mem::size_of::<T>()).max(16) as u64;
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_channel_buf(device: &wgpu::Device, count: usize) -> wgpu::Buffer {
    let size = (count * std::mem::size_of::<ChannelMeta>()).max(64) as u64;
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("channel_meta"),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn make_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    spectrum: &wgpu::Buffer,
    channels: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("spectrum bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: spectrum.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: channels.as_entire_binding(),
            },
        ],
    })
}

