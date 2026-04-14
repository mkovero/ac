use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::Color32;
use triple_buffer::Input;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::data::receiver::{ReceiverHandle, ReceiverStatus};
use crate::data::store::ChannelStore;
use crate::data::synthetic::SyntheticHandle;
use crate::data::types::{DisplayConfig, DisplayFrame, LayoutMode, SpectrumFrame};
use crate::render::context::RenderContext;
use crate::render::grid;
use crate::render::spectrum::{ChannelMeta, ChannelUpload, SpectrumRenderer};
use crate::theme;
use crate::ui::export::{self, ScreenshotRequest};
use crate::ui::layout;
use crate::ui::overlay::{self, OverlayInput};

pub enum DataSource {
    Synthetic(#[allow(dead_code)] SyntheticHandle),
    Receiver(ReceiverHandle),
}

impl DataSource {
    fn connected(&self) -> bool {
        match self {
            DataSource::Synthetic(_) => true,
            DataSource::Receiver(h) => h.status.connected.load(std::sync::atomic::Ordering::Relaxed),
        }
    }
    #[allow(dead_code)]
    fn status(&self) -> Option<&ReceiverStatus> {
        match self {
            DataSource::Receiver(h) => Some(&h.status),
            _ => None,
        }
    }
}

pub struct AppInit {
    pub store: ChannelStore,
    pub inputs: Vec<Input<SpectrumFrame>>,
    pub source_kind: SourceKind,
    pub output_dir: PathBuf,
    pub endpoint: String,
    pub synthetic_params: Option<(usize, usize, f32)>,
}

pub enum SourceKind {
    Synthetic,
    Daemon,
}

pub struct App {
    init: Option<AppInit>,
    source: Option<DataSource>,
    store: Option<ChannelStore>,
    config: DisplayConfig,
    render_ctx: Option<RenderContext>,
    spectrum: Option<SpectrumRenderer>,
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,
    last_frames: Vec<Option<DisplayFrame>>,
    pending_screenshot: bool,
    output_dir: PathBuf,
    notification: Option<(String, Instant)>,
    modifiers: ModifiersState,
    last_render: Instant,
    cursor_pos: Option<PhysicalPosition<f64>>,
    drag: Option<DragState>,
}

#[derive(Clone)]
struct DragState {
    start: PhysicalPosition<f64>,
    start_log_min: f32,
    start_log_max: f32,
    start_db_min: f32,
    start_db_max: f32,
    cell_w_px: f32,
    cell_h_px: f32,
}

impl App {
    pub fn new(init: AppInit) -> Self {
        let output_dir = init.output_dir.clone();
        Self {
            init: Some(init),
            source: None,
            store: None,
            config: DisplayConfig::default(),
            render_ctx: None,
            spectrum: None,
            egui_ctx: egui::Context::default(),
            egui_state: None,
            egui_renderer: None,
            last_frames: Vec::new(),
            pending_screenshot: false,
            output_dir,
            notification: None,
            modifiers: ModifiersState::empty(),
            last_render: Instant::now(),
            cursor_pos: None,
            drag: None,
        }
    }

    fn cell_at(&self, pos: PhysicalPosition<f64>) -> Option<(f32, f32, f32, f32)> {
        let ctx = self.render_ctx.as_ref()?;
        let w = ctx.config.width as f32;
        let h = ctx.config.height as f32;
        let n = self.store.as_ref().map(|s| s.len()).unwrap_or(0);
        if n == 0 {
            return None;
        }
        let cells = layout::compute(self.config.layout, n, self.config.active_channel);
        for c in &cells {
            let r = layout::to_pixel_rect(c, w, h);
            let x = pos.x as f32;
            let y = pos.y as f32;
            if x >= r.left() && x <= r.right() && y >= r.top() && y <= r.bottom() {
                let nx = (x - r.left()) / r.width().max(1.0);
                let ny = 1.0 - (y - r.top()) / r.height().max(1.0);
                return Some((nx, ny, r.width(), r.height()));
            }
        }
        None
    }

    fn apply_zoom(&mut self, scroll_y: f32) {
        let pos = match self.cursor_pos {
            Some(p) => p,
            None => return,
        };
        let (nx, ny, _, _) = match self.cell_at(pos) {
            Some(v) => v,
            None => return,
        };
        let factor = 0.85_f32.powf(scroll_y);
        let shift = self.modifiers.shift_key();
        let ctrl = self.modifiers.control_key();

        if !shift {
            let log_min = self.config.freq_min.max(1.0).log10();
            let log_max = self.config.freq_max.max(log_min.exp().max(10.0)).log10();
            let anchor = log_min + nx * (log_max - log_min);
            let new_span = ((log_max - log_min) * factor).clamp(0.15, 4.5);
            let new_min = (anchor - nx * new_span).clamp(0.0, 4.5);
            let new_max = (new_min + new_span).min(4.8);
            self.config.freq_min = 10.0_f32.powf(new_min).max(1.0);
            self.config.freq_max = 10.0_f32.powf(new_max);
        }
        if !ctrl {
            let db_min = self.config.db_min;
            let db_max = self.config.db_max;
            let anchor = db_min + ny * (db_max - db_min);
            let new_span = ((db_max - db_min) * factor).clamp(10.0, 240.0);
            let new_min = (anchor - ny * new_span).max(-240.0);
            let new_max = (new_min + new_span).min(20.0);
            self.config.db_min = new_min;
            self.config.db_max = new_max;
        }
    }

    fn begin_drag(&mut self) {
        let pos = match self.cursor_pos {
            Some(p) => p,
            None => return,
        };
        let cell = match self.cell_at(pos) {
            Some(v) => v,
            None => return,
        };
        let log_min = self.config.freq_min.max(1.0).log10();
        let log_max = self.config.freq_max.max(10.0).log10();
        self.drag = Some(DragState {
            start: pos,
            start_log_min: log_min,
            start_log_max: log_max,
            start_db_min: self.config.db_min,
            start_db_max: self.config.db_max,
            cell_w_px: cell.2,
            cell_h_px: cell.3,
        });
    }

    fn update_drag(&mut self, pos: PhysicalPosition<f64>) {
        let drag = match self.drag.clone() {
            Some(d) => d,
            None => return,
        };
        let dx_px = (pos.x - drag.start.x) as f32;
        let dy_px = (pos.y - drag.start.y) as f32;
        let log_span = drag.start_log_max - drag.start_log_min;
        let db_span = drag.start_db_max - drag.start_db_min;
        let d_log = -(dx_px / drag.cell_w_px.max(1.0)) * log_span;
        let d_db = -(dy_px / drag.cell_h_px.max(1.0)) * db_span;
        let new_log_min = (drag.start_log_min + d_log).clamp(0.0, 4.8 - log_span.min(4.8));
        let new_log_max = new_log_min + log_span;
        let new_db_min = (drag.start_db_min + d_db).max(-240.0);
        let new_db_max = (new_db_min + db_span).min(20.0);
        self.config.freq_min = 10.0_f32.powf(new_log_min).max(1.0);
        self.config.freq_max = 10.0_f32.powf(new_log_max);
        self.config.db_min = new_db_min;
        self.config.db_max = new_db_max;
    }

    fn reset_view(&mut self) {
        self.config.freq_min = theme::DEFAULT_FREQ_MIN;
        self.config.freq_max = theme::DEFAULT_FREQ_MAX;
        self.config.db_min = theme::DEFAULT_DB_MIN;
        self.config.db_max = theme::DEFAULT_DB_MAX;
        self.notify("view reset");
    }

    fn start_data_source(&mut self) {
        let init = match self.init.take() {
            Some(i) => i,
            None => return,
        };
        self.store = Some(init.store);
        match init.source_kind {
            SourceKind::Synthetic => {
                let (n, bins, rate) = init.synthetic_params.unwrap_or((1, 1000, 10.0));
                let src = crate::data::synthetic::SyntheticSource {
                    n_channels: n,
                    n_bins: bins,
                    update_hz: rate,
                };
                let handle = src.spawn(init.inputs);
                self.source = Some(DataSource::Synthetic(handle));
            }
            SourceKind::Daemon => {
                let mut inputs = init.inputs;
                let input = inputs.remove(0);
                let handle = crate::data::receiver::spawn(init.endpoint, input);
                self.source = Some(DataSource::Receiver(handle));
            }
        }
    }

    fn init_graphics(&mut self, window: Arc<Window>) {
        let ctx = pollster::block_on(RenderContext::new(window.clone())).expect("wgpu init");
        let format = ctx.surface_format();
        let spectrum = SpectrumRenderer::new(&ctx.device, format);
        let egui_renderer = egui_wgpu::Renderer::new(&ctx.device, format, None, 1, false);
        self.egui_ctx.set_visuals(dark_visuals());
        let viewport_id = self.egui_ctx.viewport_id();
        let egui_state =
            egui_winit::State::new(self.egui_ctx.clone(), viewport_id, &window, None, None, None);
        self.render_ctx = Some(ctx);
        self.spectrum = Some(spectrum);
        self.egui_renderer = Some(egui_renderer);
        self.egui_state = Some(egui_state);
    }

    fn handle_key(&mut self, elwt: &ActiveEventLoop, code: KeyCode) {
        match code {
            KeyCode::Escape | KeyCode::KeyQ => elwt.exit(),
            KeyCode::Enter => {
                self.config.frozen = !self.config.frozen;
                self.notify(if self.config.frozen { "FROZEN" } else { "live" });
            }
            KeyCode::Space => {
                self.config.peak_hold = !self.config.peak_hold;
                self.notify(if self.config.peak_hold {
                    "peak hold on"
                } else {
                    "peak hold off"
                });
            }
            KeyCode::KeyS => {
                self.pending_screenshot = true;
            }
            KeyCode::KeyL => {
                self.config.layout = self.config.layout.next();
                self.notify(match self.config.layout {
                    LayoutMode::Grid => "layout: grid",
                    LayoutMode::Overlay => "layout: overlay",
                    LayoutMode::Single => "layout: single",
                });
            }
            KeyCode::KeyF => {
                if let Some(ctx) = self.render_ctx.as_ref() {
                    let is_full = ctx.window.fullscreen().is_some();
                    ctx.window.set_fullscreen(if is_full {
                        None
                    } else {
                        Some(winit::window::Fullscreen::Borderless(None))
                    });
                }
            }
            KeyCode::Equal | KeyCode::NumpadAdd => {
                let span = (self.config.db_max - self.config.db_min).max(20.0) - 20.0;
                self.config.db_min = self.config.db_max - span.max(20.0);
            }
            KeyCode::Minus | KeyCode::NumpadSubtract => {
                let span = (self.config.db_max - self.config.db_min) + 20.0;
                self.config.db_min = (self.config.db_max - span).max(-240.0);
            }
            KeyCode::Tab => {
                let n = self.store.as_ref().map(|s| s.len()).unwrap_or(1).max(1);
                if self.modifiers.control_key() {
                    let delta = if self.modifiers.shift_key() { n - 1 } else { 1 };
                    self.config.active_channel = (self.config.active_channel + delta) % n;
                    self.notify(&format!("CH{}", self.config.active_channel));
                }
            }
            _ => {}
        }
    }

    fn notify(&mut self, msg: &str) {
        self.notification = Some((msg.to_string(), Instant::now()));
    }

    fn redraw(&mut self) {
        let ctx = match self.render_ctx.as_mut() {
            Some(c) => c,
            None => return,
        };
        let spectrum = self.spectrum.as_mut().unwrap();
        let egui_renderer = self.egui_renderer.as_mut().unwrap();
        let egui_state = self.egui_state.as_mut().unwrap();

        let frames = {
            let store = self.store.as_mut();
            if let Some(store) = store {
                if !self.config.frozen {
                    self.last_frames = store.read_all(&self.config);
                } else {
                    let _ = store.read_all(&self.config);
                }
            }
            self.last_frames.clone()
        };

        let n_channels = frames.len();
        let cells = layout::compute(self.config.layout, n_channels, self.config.active_channel);

        let mut uploads: Vec<ChannelUpload<'_>> = Vec::with_capacity(cells.len());
        let cells_vec: Vec<_> = cells.clone();
        for cell in &cells_vec {
            let frame = match frames.get(cell.channel).and_then(|f| f.as_ref()) {
                Some(f) if !f.spectrum.is_empty() => f,
                _ => continue,
            };
            let freq_log_min = self.config.freq_min.max(1.0).log10();
            let freq_log_max = self.config.freq_max.max(20.0).log10();
            let meta = ChannelMeta {
                color: theme::channel_color(cell.channel),
                viewport: [cell.x, cell.y, cell.w, cell.h],
                db_min: self.config.db_min,
                db_max: self.config.db_max,
                freq_log_min,
                freq_log_max,
                n_bins: frame.spectrum.len() as u32,
                offset: 0,
                _pad0: 0,
                _pad1: 0,
            };
            uploads.push(ChannelUpload {
                spectrum: &frame.spectrum,
                freqs: &frame.freqs,
                meta,
            });
        }

        spectrum.upload(&ctx.device, &ctx.queue, &uploads);

        let raw_input = egui_state.take_egui_input(&ctx.window);
        let show_labels = self.config.layout != LayoutMode::Grid || n_channels <= 8;
        let connected = self
            .source
            .as_ref()
            .map(|s| s.connected())
            .unwrap_or(false);
        let config_snap = self.config.clone();
        let width_px = ctx.config.width as f32;
        let height_px = ctx.config.height as f32;
        let notification = self
            .notification
            .as_ref()
            .filter(|(_, t)| t.elapsed() < Duration::from_millis(1200))
            .map(|(s, _)| s.clone());

        let full_output = self.egui_ctx.run(raw_input, |ui_ctx| {
            let painter = ui_ctx.layer_painter(egui::LayerId::new(
                egui::Order::Background,
                egui::Id::new("ac-ui-grid"),
            ));
            for cell in &cells_vec {
                let rect = layout::to_pixel_rect(cell, width_px, height_px);
                grid::draw_grid(&painter, rect, &config_snap, show_labels);
            }
            overlay::draw(
                ui_ctx,
                OverlayInput {
                    config: &config_snap,
                    frames: &frames,
                    connected,
                    notification: notification.as_deref(),
                },
            );
        });

        let pixels_per_point = self.egui_ctx.pixels_per_point();
        let paint_jobs = self
            .egui_ctx
            .tessellate(full_output.shapes, pixels_per_point);
        let screen_desc = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [ctx.config.width, ctx.config.height],
            pixels_per_point,
        };

        for (id, delta) in &full_output.textures_delta.set {
            egui_renderer.update_texture(&ctx.device, &ctx.queue, *id, delta);
        }

        let surface_tex = match ctx.surface.get_current_texture() {
            Ok(t) => t,
            Err(wgpu::SurfaceError::Lost) | Err(wgpu::SurfaceError::Outdated) => {
                ctx.surface.configure(&ctx.device, &ctx.config);
                return;
            }
            Err(e) => {
                log::error!("surface acquire: {e:?}");
                return;
            }
        };
        let view = surface_tex
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("ac-ui frame"),
        });

        egui_renderer.update_buffers(
            &ctx.device,
            &ctx.queue,
            &mut encoder,
            &paint_jobs,
            &screen_desc,
        );

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("spectrum pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: theme::BG[0] as f64,
                            g: theme::BG[1] as f64,
                            b: theme::BG[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            spectrum.draw(&mut pass);
        }

        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            let mut pass = pass.forget_lifetime();
            egui_renderer.render(&mut pass, &paint_jobs, &screen_desc);
        }

        let capture = if self.pending_screenshot {
            self.pending_screenshot = false;
            Some(prepare_capture(ctx, &mut encoder, &surface_tex))
        } else {
            None
        };

        ctx.queue.submit(Some(encoder.finish()));
        surface_tex.present();

        for id in &full_output.textures_delta.free {
            egui_renderer.free_texture(id);
        }

        if let Some(cap) = capture {
            finalize_capture(ctx, cap, &self.output_dir, &frames);
            self.notify("saved");
        }

        self.last_render = Instant::now();
    }
}

fn dark_visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.window_fill = Color32::from_rgba_unmultiplied(10, 10, 15, 0);
    v.panel_fill = Color32::from_rgba_unmultiplied(10, 10, 15, 0);
    v
}

struct CaptureJob {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    bytes_per_row: u32,
    format: wgpu::TextureFormat,
}

fn prepare_capture(
    ctx: &RenderContext,
    encoder: &mut wgpu::CommandEncoder,
    surface_tex: &wgpu::SurfaceTexture,
) -> CaptureJob {
    let width = ctx.config.width;
    let height = ctx.config.height;
    let bytes_per_row = export::bytes_per_row_aligned(width);
    let size = (bytes_per_row as u64) * (height as u64);
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("screenshot buf"),
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &surface_tex.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    CaptureJob {
        buffer,
        width,
        height,
        bytes_per_row,
        format: ctx.config.format,
    }
}

fn finalize_capture(
    ctx: &RenderContext,
    job: CaptureJob,
    output_dir: &std::path::Path,
    frames: &[Option<DisplayFrame>],
) {
    let slice = job.buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    let _ = ctx.device.poll(wgpu::Maintain::Wait);
    match rx.recv() {
        Ok(Ok(())) => {
            let data = slice.get_mapped_range();
            let pixels = data.to_vec();
            drop(data);
            job.buffer.unmap();
            export::spawn_save(ScreenshotRequest {
                output_dir: output_dir.to_path_buf(),
                width: job.width,
                height: job.height,
                bytes_per_row: job.bytes_per_row,
                pixels,
                format: job.format,
                frames: frames.to_vec(),
            });
        }
        _ => log::error!("screenshot map failed"),
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, elwt: &ActiveEventLoop) {
        if self.render_ctx.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("ac-ui — spectrum")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0));
        let window = Arc::new(elwt.create_window(attrs).expect("window create"));
        self.init_graphics(window);
        self.start_data_source();
    }

    fn window_event(
        &mut self,
        elwt: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        if let Some(state) = self.egui_state.as_mut() {
            if let Some(ctx) = self.render_ctx.as_ref() {
                let resp = state.on_window_event(&ctx.window, &event);
                if resp.consumed {
                    return;
                }
            }
        }
        match event {
            WindowEvent::CloseRequested => elwt.exit(),
            WindowEvent::Resized(size) => {
                if let Some(ctx) = self.render_ctx.as_mut() {
                    ctx.resize(size);
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                self.modifiers = m.state();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = Some(position);
                if self.drag.is_some() {
                    self.update_drag(position);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.begin_drag();
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.drag = None;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                self.reset_view();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let scroll = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => (p.y / 50.0) as f32,
                };
                if scroll != 0.0 {
                    self.apply_zoom(scroll);
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                self.handle_key(elwt, code);
            }
            WindowEvent::RedrawRequested => {
                self.redraw();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _elwt: &ActiveEventLoop) {
        if let Some(ctx) = self.render_ctx.as_ref() {
            ctx.window.request_redraw();
        }
    }
}
