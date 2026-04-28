use std::sync::Arc;
use winit::window::Window;

use super::timing::GpuTiming;

pub struct RenderContext {
    pub window: Arc<Window>,
    // Retained: wgpu resources held for Drop ordering; surface/device depend on them.
    #[allow(dead_code)]
    pub instance: wgpu::Instance,
    pub surface: wgpu::Surface<'static>,
    #[allow(dead_code)]
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
    pub size: winit::dpi::PhysicalSize<u32>,
    pub timing: Option<GpuTiming>,
}

impl RenderContext {
    pub async fn new(
        window: Arc<Window>,
        requested_present_mode: wgpu::PresentMode,
    ) -> anyhow::Result<Self> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        let surface = instance.create_surface(window.clone())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| anyhow::anyhow!("no wgpu adapter"))?;
        let adapter_features = adapter.features();
        let want_timing = adapter_features.contains(wgpu::Features::TIMESTAMP_QUERY);
        let required_features = if want_timing {
            wgpu::Features::TIMESTAMP_QUERY
        } else {
            wgpu::Features::empty()
        };
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("ac-ui device"),
                    required_features,
                    required_limits: wgpu::Limits::default(),
                    memory_hints: wgpu::MemoryHints::default(),
                },
                None,
            )
            .await?;
        let timing = want_timing.then(|| GpuTiming::new(&device, &queue));

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        // Pick the present mode the user asked for if the surface supports
        // it; otherwise fall back to the surface's first advertised mode and
        // log so the actual choice is auditable. `AutoVsync` / `AutoNoVsync`
        // are wgpu virtual modes that always resolve, so the fallback path
        // only fires for explicit `Mailbox` / `Immediate` / `FifoRelaxed`
        // requests on surfaces that don't expose them (#110).
        let present_mode =
            if caps.present_modes.contains(&requested_present_mode)
                || matches!(
                    requested_present_mode,
                    wgpu::PresentMode::AutoVsync | wgpu::PresentMode::AutoNoVsync
                )
            {
                requested_present_mode
            } else {
                let fallback = *caps
                    .present_modes
                    .first()
                    .unwrap_or(&wgpu::PresentMode::Fifo);
                log::warn!(
                    "present mode {:?} not supported by surface (have {:?}); using {:?}",
                    requested_present_mode, caps.present_modes, fallback,
                );
                fallback
            };
        let info = adapter.get_info();
        log::info!(
            "wgpu: backend={:?} adapter={:?} driver={:?} driver_info={:?} present_mode={:?} max_latency=3",
            info.backend, info.name, info.driver, info.driver_info, present_mode,
        );
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            // 3 = true triple-buffering on Fifo: acquire returns immediately
            // while the previously-presented frame is still on-screen, so the
            // CPU isn't blocked waiting for vsync. Bumped from 2 alongside
            // #110's runtime present-mode selection.
            desired_maximum_frame_latency: 3,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        Ok(Self {
            window,
            instance,
            surface,
            adapter,
            device,
            queue,
            config,
            size,
            timing,
        })
    }

    pub fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.size = new_size;
        self.config.width = new_size.width;
        self.config.height = new_size.height;
        self.surface.configure(&self.device, &self.config);
    }

    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.config.format
    }
}
