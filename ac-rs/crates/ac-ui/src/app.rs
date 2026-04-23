use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use triple_buffer::Input;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::data::control::CtrlClient;
use crate::data::smoothing;
use crate::data::store::{
    ChannelStore, LoudnessStore, SweepState, SweepStore, TransferStore, VirtualChannelStore,
};
use crate::data::types::{
    CellView, DisplayConfig, DisplayFrame, LayoutMode, SpectrumFrame, SweepKind, TransferFrame,
    TransferPair, ViewMode,
};
use crate::render::context::RenderContext;
use crate::render::spectrum::SpectrumRenderer;
use crate::render::waterfall::WaterfallRenderer;
use crate::theme;
use crate::ui::layout::GridParams;
use crate::ui::stats::TimingStats;

mod helpers;
mod input;
mod control;
mod render_pipeline;

pub use helpers::*;

/// Per-band time-integration mode toggled by the `T` key and mirrored
/// to the daemon via `set_time_integration`. Matches the string values
/// accepted by the daemon command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeIntegrationMode {
    Off,
    Fast,
    Slow,
    Leq,
}

/// Per-band frequency weighting toggled by the `A` key and mirrored to
/// the daemon via `set_band_weighting`. `Off` means no curve applied;
/// `Z` is the identity curve and is functionally identical to `Off`
/// but distinct in the UI so the user can see "explicitly picked Z"
/// in the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandWeighting {
    Off,
    A,
    C,
    Z,
}

impl BandWeighting {
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::A,
            Self::A   => Self::C,
            Self::C   => Self::Z,
            Self::Z   => Self::Off,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::A   => "a",
            Self::C   => "c",
            Self::Z   => "z",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "weighting: off",
            Self::A   => "weighting: A",
            Self::C   => "weighting: C",
            Self::Z   => "weighting: Z",
        }
    }

    pub fn overlay_tag(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::A   => Some("A"),
            Self::C   => Some("C"),
            Self::Z   => Some("Z"),
        }
    }
}

impl TimeIntegrationMode {
    pub fn next(self) -> Self {
        match self {
            Self::Off  => Self::Fast,
            Self::Fast => Self::Slow,
            Self::Slow => Self::Leq,
            Self::Leq  => Self::Off,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off  => "off",
            Self::Fast => "fast",
            Self::Slow => "slow",
            Self::Leq  => "leq",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off  => "time: off",
            Self::Fast => "time: fast (125 ms)",
            Self::Slow => "time: slow (1 s)",
            Self::Leq  => "time: Leq",
        }
    }
}

pub struct AppInit {
    pub store: ChannelStore,
    pub inputs: Vec<Input<SpectrumFrame>>,
    pub transfer_store: TransferStore,
    pub virtual_channels: VirtualChannelStore,
    pub sweep_store: SweepStore,
    pub loudness_store: LoudnessStore,
    pub source_kind: SourceKind,
    pub output_dir: PathBuf,
    pub endpoint: String,
    pub ctrl_endpoint: String,
    pub synthetic_params: Option<(usize, usize, f32)>,
    pub benchmark_secs: Option<f64>,
    pub initial_view: ViewMode,
    pub initial_sweep_kind: Option<SweepKind>,
    pub monitor_channels: Option<Vec<u32>>,
    /// Proxy handed to background producer threads (receiver / synthetic) so
    /// they can wake the winit event loop the instant a new frame lands.
    /// Without this the UI sits in `ControlFlow::Wait` and won't repaint
    /// until the next OS input event, making streamed data look sluggish.
    pub wake: Option<EventLoopProxy<()>>,
}

pub struct App {
    init: Option<AppInit>,
    source: Option<DataSource>,
    store: Option<ChannelStore>,
    transfer_store: Option<TransferStore>,
    /// Registered virtual transfer channels (Space + T). Multi-pair
    /// `transfer_stream` worker keeps one H1 estimate live per entry, routed
    /// into the store's per-pair slots by the receiver.
    virtual_channels: VirtualChannelStore,
    ctrl_endpoint: String,
    ctrl: Option<CtrlClient>,
    /// Cached latest TransferFrame so the renderer can draw a held view during
    /// freeze. The receiver always writes to `transfer_store` — we snapshot it
    /// once per redraw when not frozen and keep the snapshot while frozen.
    transfer_last: Option<TransferFrame>,
    /// Tracks whether a `transfer_stream` worker is running on the daemon. Set
    /// on successful start, cleared on stop / layout exit. Used to avoid
    /// double-starts and to decide whether to send a stop on layout exit.
    transfer_stream_active: bool,
    sweep_store: Option<SweepStore>,
    sweep_kind: Option<SweepKind>,
    sweep_last: SweepState,
    sweep_selected_idx: Option<usize>,
    /// Per-channel BS.1770 / R128 meter — populated by the receiver from
    /// `measurement/loudness` frames; consumed by the overlay.
    loudness_store: Option<LoudnessStore>,
    /// Tracks whether `ac-ui` has told the daemon to run `monitor_spectrum`.
    /// The UI is a passive SUB by default — without this command the daemon
    /// publishes nothing and every view stays blank ("disconnected"). We
    /// pause/resume around `transfer_stream` since that's the `Exclusive`
    /// group and would otherwise be busy-blocked by the `Input`-group monitor.
    monitor_spectrum_active: bool,
    monitor_channels: Option<Vec<u32>>,
    /// Current daemon analysis mode: "fft" (default) or "cwt". Toggled via
    /// the W waterfall cycle (Spectrum → Waterfall-FFT → Waterfall-CWT). We
    /// track it locally so the cycle key can decide which mode to request
    /// without a round-trip to the daemon.
    analysis_mode: String,
    cwt_sigma: f32,
    cwt_n_scales: usize,
    /// Fractional-octave aggregation bins-per-octave for CWT view.
    /// `None` = disabled, daemon publishes only the raw CWT frame.
    /// `Some(N)` = daemon also publishes a `type: "fractional_octave"`
    /// frame per tick which overwrites the CWT entry in the same triple
    /// buffer slot. Cycled via `Shift+O` (CWT mode only). Distinct from
    /// `smoothing_frac`, which only reshapes the FFT display.
    ioct_bpo: Option<u32>,
    /// Per-band time-integration mode mirrored to the daemon via
    /// `set_time_integration`. Cycled by `T`; the daemon publishes a
    /// `fractional_octave_leq` sidecar frame whenever this is not
    /// [`TimeIntegrationMode::Off`]. Consumed for rendering by the
    /// spectrum view (follow-up); today it is control-surface only.
    time_integration: TimeIntegrationMode,
    /// Per-band frequency-weighting curve mirrored to the daemon via
    /// `set_band_weighting`. Cycled by `A`; the daemon applies the
    /// corresponding dB offset to each band level before emitting the
    /// `fractional_octave` / `fractional_octave_leq` frames. `Off` and
    /// `Z` send the same wire value for the daemon but differ here so
    /// the overlay tag distinguishes the two user intents.
    band_weighting: BandWeighting,
    /// Live FFT monitor knobs (interval 1 ms steps in [1, 1000] ms;
    /// `MONITOR_FFT_N_LADDER` for N). Mutated by plain arrow keys in FFT mode
    /// and pushed to the daemon via `set_monitor_params`.
    monitor_interval_ms: u32,
    monitor_fft_n: u32,
    /// Insertion-order view of `selected`. Compare layout renders cells in
    /// selection order; the T key reads the first and last entries to form
    /// a virtual transfer pair (meas = first, ref = last).
    selection_order: Vec<usize>,
    config: DisplayConfig,
    cell_views: Vec<CellView>,
    selected: Vec<bool>,
    show_help: bool,
    /// Grid layout sizing. `cell_size = None` = auto (sqrt layout, one page);
    /// scrolling outside cells switches to manual mode. `page` is capped to
    /// `grid_dims().3 - 1` after every resize/channel change.
    grid_cell_size: Option<f32>,
    grid_page: usize,
    /// Flipped true the first time a waterfall frame lands for each channel
    /// so we can auto-init dB range from that frame's mean. Cleared on
    /// `Ctrl+R`, on view-mode changes, and when cell_views is reallocated.
    waterfall_inited: Vec<bool>,
    /// Rolling estimate of the producer's frame interval in seconds. Computed
    /// as the median of the last `WATERFALL_ROW_DT_WINDOW` channel-0 `new_row`
    /// inter-arrival times, so the waterfall Y axis stays put under jitter and
    /// a single stall can't drag it. Defaults to 0.1 s (10 Hz) until we have
    /// enough samples.
    waterfall_row_period_s: f32,
    waterfall_last_row_at: Option<Instant>,
    /// Ring of recent row-to-row dt samples (seconds) used to derive the
    /// median above. Bounded at `WATERFALL_ROW_DT_WINDOW` entries.
    waterfall_row_dts: VecDeque<f32>,
    /// Highest `freqs.last()` observed across any frame, used as the freq
    /// clamp ceiling so zoom/pan caps at real Nyquist instead of the 24 kHz
    /// default (48 kHz sr). Seeded from `DEFAULT_FREQ_MAX`, grows monotonically
    /// as daemons at higher sample rates come online.
    data_freq_ceiling: f32,
    render_ctx: Option<RenderContext>,
    spectrum: Option<SpectrumRenderer>,
    waterfall: Option<WaterfallRenderer>,
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,
    last_frames: Vec<Option<DisplayFrame>>,
    /// Virtual transfer channels currently rendered as extra grid cells,
    /// in the order they appear after real channels in `frames`. Refreshed
    /// from `virtual_channels.pairs()` on every redraw so the mapping
    /// `virtual_index_in_frames - n_real → TransferPair` always matches
    /// what the shaders just drew.
    virtual_render_pairs: Vec<TransferPair>,
    /// Per-pair last-seen TransferStore write serial. When the current
    /// serial exceeds the seen value we treat the frame as a fresh row and
    /// scroll the waterfall for that virtual channel.
    virtual_seen_serial: HashMap<TransferPair, u64>,
    pending_screenshot: bool,
    /// Peak-hold state. `enabled` toggles via `P`; when true every fresh
    /// spectrum frame is bin-wise max'd into `holds[channel]` so the UI can
    /// overlay a frozen-max trace on top of the live spectrum. `None` means
    /// the buffer is empty (either peak-hold was just enabled, or a reset
    /// fired because bin count / analysis mode changed).
    peak_hold_enabled: bool,
    peak_holds: Vec<Option<Vec<f32>>>,
    /// Time of the last bin-wise max update for each channel's peak buffer.
    /// If no fresh bin has surpassed the held peak within `PEAK_HOLD_DECAY`,
    /// the buffer re-seeds from the current spectrum so a loud transient
    /// doesn't pin the trace forever when the room has gone quiet again.
    peak_last_update: Vec<Option<Instant>>,
    /// Last frame-tick timestamp per channel, used by the release-rate logic
    /// to compute dt and drop the held trace by `PEAK_RELEASE_DB_PER_SEC*dt`
    /// each frame once the hold window has elapsed.
    peak_last_tick: Vec<Option<Instant>>,
    /// Min-hold: mirror of peak-hold, per-bin rolling minimum. Shows the
    /// noise floor below intermittent signals. Same decay behaviour.
    min_hold_enabled: bool,
    min_holds: Vec<Option<Vec<f32>>>,
    min_last_update: Vec<Option<Instant>>,
    min_last_tick: Vec<Option<Instant>>,
    /// Fractional-octave smoothing mode. `None` = raw spectrum; `Some(n)`
    /// smooths each bin with its neighbours inside ±f/2^(1/2n) so the
    /// linearly-spaced FFT output reads as a log-spaced curve. Typical
    /// audio values: 24, 12, 6, 3. Cycles via `O`.
    smoothing_frac: Option<u32>,
    /// Cached window index lists for `smoothing_frac`, keyed by
    /// (n, n_bins, last-freq-seen). Rebuilt when any of those change; saves
    /// a per-bin log range recomputation every frame.
    smoothing_cache: Option<smoothing::OctaveWindows>,
    /// Accumulates fractional scroll ticks while Shift+Scroll is cycling the
    /// waterfall palette, so trackpad pixel-deltas don't step the palette on
    /// every frame. One palette step per full unit of scroll.
    palette_scroll_accum: f32,
    output_dir: PathBuf,
    notification: Option<(String, Instant)>,
    modifiers: ModifiersState,
    last_render: Instant,
    cursor_pos: Option<PhysicalPosition<f64>>,
    drag: Option<input::DragState>,
    timing_stats: TimingStats,
    show_timing: bool,
    benchmark_secs: Option<f64>,
    benchmark_started: Option<Instant>,
    benchmark_report: Option<String>,
    /// Last `ReceiverStatus::last_frame_ns` value we saw. Compared in
    /// `about_to_wait` to decide whether new data arrived since the last
    /// render — if not, skip the redraw to save CPU.
    last_seen_frame_ns: u64,
    /// Set by input handlers so the next `about_to_wait` requests a redraw
    /// even without new data (e.g. key press changed layout, mouse drag).
    needs_redraw: bool,
    /// Proxy handed to producer threads so they can wake the loop on frame
    /// arrival. Cloned out during `start_data_source`; kept here only so the
    /// clone is retained if we ever need to re-wire a new source.
    wake: Option<EventLoopProxy<()>>,
}

impl App {
    pub fn new(init: AppInit) -> Self {
        let output_dir = init.output_dir.clone();
        let benchmark_secs = init.benchmark_secs;
        let show_timing = benchmark_secs.is_some();
        let ctrl_endpoint = init.ctrl_endpoint.clone();
        let sweep_kind = init.initial_sweep_kind;
        let monitor_channels = init.monitor_channels.clone();
        let wake = init.wake.clone();
        let layout = if sweep_kind.is_some() {
            LayoutMode::Sweep
        } else {
            LayoutMode::Grid
        };
        let config = DisplayConfig {
            view_mode: init.initial_view,
            layout,
            ..DisplayConfig::default()
        };
        Self {
            init: Some(init),
            source: None,
            store: None,
            transfer_store: None,
            virtual_channels: VirtualChannelStore::new(),
            ctrl_endpoint,
            ctrl: None,
            transfer_last: None,
            transfer_stream_active: false,
            sweep_store: None,
            sweep_kind,
            sweep_last: SweepState::default(),
            sweep_selected_idx: None,
            loudness_store: None,
            monitor_spectrum_active: false,
            monitor_channels,
            analysis_mode: "fft".to_string(),
            cwt_sigma: 12.0,
            cwt_n_scales: 512,
            ioct_bpo: None,
            time_integration: TimeIntegrationMode::Off,
            band_weighting: BandWeighting::Off,
            // Auto-scaled on every N change (arrow Up/Down) and at the
            // first frame (once sr is known). Seeded from the default N
            // assuming 48 kHz so the very first tick doesn't overshoot.
            monitor_interval_ms: auto_monitor_interval_ms(8192, 48_000),
            monitor_fft_n: 8192,
            selection_order: Vec::new(),
            config,
            cell_views: Vec::new(),
            selected: Vec::new(),
            show_help: false,
            grid_cell_size: None,
            grid_page: 0,
            waterfall_inited: Vec::new(),
            waterfall_row_period_s: 0.1,
            waterfall_last_row_at: None,
            waterfall_row_dts: VecDeque::with_capacity(WATERFALL_ROW_DT_WINDOW),
            data_freq_ceiling: theme::DEFAULT_FREQ_MAX,
            render_ctx: None,
            spectrum: None,
            waterfall: None,
            egui_ctx: egui::Context::default(),
            egui_state: None,
            egui_renderer: None,
            last_frames: Vec::new(),
            virtual_render_pairs: Vec::new(),
            virtual_seen_serial: HashMap::new(),
            pending_screenshot: false,
            peak_hold_enabled: false,
            peak_holds: Vec::new(),
            peak_last_update: Vec::new(),
            peak_last_tick: Vec::new(),
            min_hold_enabled: false,
            min_holds: Vec::new(),
            min_last_update: Vec::new(),
            min_last_tick: Vec::new(),
            // Default to 1/6 octave: gentle enough to preserve resonance
            // detail, heavy enough to calm the FFT grass. Users can cycle or
            // disable via `O`.
            smoothing_frac: Some(6),
            smoothing_cache: None,
            palette_scroll_accum: 0.0,
            output_dir,
            notification: None,
            modifiers: ModifiersState::empty(),
            last_render: Instant::now(),
            cursor_pos: None,
            drag: None,
            timing_stats: TimingStats::new(),
            show_timing,
            benchmark_secs,
            benchmark_started: None,
            benchmark_report: None,
            last_seen_frame_ns: 0,
            needs_redraw: true,
            wake,
        }
    }

    pub fn benchmark_report(&self) -> Option<&str> {
        self.benchmark_report.as_deref()
    }

    fn benchmark_tick(&mut self, elwt: &ActiveEventLoop) {
        let secs = match self.benchmark_secs {
            Some(s) => s,
            None => return,
        };
        if self.benchmark_started.is_none() {
            self.benchmark_started = Some(Instant::now());
            return;
        }
        let started = self.benchmark_started.unwrap();
        if started.elapsed().as_secs_f64() < secs { return; }

        let snap = self.timing_stats.snapshot();
        let gpu = snap.gpu;
        let report = format!(
            "ac-ui benchmark: {:.1} s, {} frames\n  fps mean {:.2}\n  frame ms mean {:.3}  p50 {:.3}  p95 {:.3}  p99 {:.3}\n  cpu ms mean {:.3}\n  gpu ms last  total {:.3}  spectrum {:.3}  egui {:.3}",
            started.elapsed().as_secs_f64(),
            snap.samples,
            snap.fps,
            snap.frame_mean_ms,
            snap.frame_p50_ms,
            snap.frame_p95_ms,
            snap.frame_p99_ms,
            snap.cpu_mean_ms,
            gpu.gpu_ms,
            gpu.spectrum_ms,
            gpu.egui_ms,
        );
        self.benchmark_report = Some(report);
        elwt.exit();
    }

    fn grid_params(&self) -> GridParams {
        GridParams {
            cell_size: self.grid_cell_size,
            page:      self.grid_page,
        }
    }

    fn init_graphics(&mut self, window: Arc<Window>) {
        let ctx = pollster::block_on(RenderContext::new(window.clone())).expect("wgpu init");
        let format = ctx.surface_format();
        let spectrum = SpectrumRenderer::new(&ctx.device, format);
        let waterfall = WaterfallRenderer::new(&ctx.device, &ctx.queue, format);
        let egui_renderer = egui_wgpu::Renderer::new(&ctx.device, format, None, 1, false);
        self.egui_ctx.set_visuals(render_pipeline::dark_visuals());
        let viewport_id = self.egui_ctx.viewport_id();
        let egui_state =
            egui_winit::State::new(self.egui_ctx.clone(), viewport_id, &window, None, None, None);
        self.render_ctx = Some(ctx);
        self.spectrum = Some(spectrum);
        self.waterfall = Some(waterfall);
        self.egui_renderer = Some(egui_renderer);
        self.egui_state = Some(egui_state);
    }

    fn notify(&mut self, msg: &str) {
        self.notification = Some((msg.to_string(), Instant::now()));
    }

    /// Pure state-machine decision for `about_to_wait`. Separated so it can
    /// be unit-tested without a winit event loop. Mutates `self.needs_redraw`
    /// and `self.notification` (expiry) but otherwise only reads state.
    ///
    /// Behaviour:
    /// - New data frame since last render → `RedrawIdle` (or `RedrawContinuous`
    ///   if a time-driven overlay is also active).
    /// - Live notification or active benchmark → `RedrawContinuous` so the
    ///   ~60 Hz fade / FPS counter ticks.
    /// - Input handler flagged a redraw (key/mouse) → `RedrawIdle`.
    /// - Nothing pending → `Idle`.
    pub fn loop_directive(&mut self, now: Instant) -> LoopDirective {
        // Expire stale notifications eagerly. Without this the `is_some()`
        // check below stays `true` forever after the first `notify()` call
        // — which was the "pressing d makes it permanently smooth" bug.
        if let Some((_, t)) = &self.notification {
            if now.saturating_duration_since(*t) >= NOTIFICATION_TTL {
                self.notification = None;
            }
        }

        // Producer threads wake us via `send_event(())`; we just dedupe on
        // `last_frame_ns` so back-to-back wakes for the same frame don't
        // trigger redundant renders.
        let current_ns = self
            .source
            .as_ref()
            .and_then(|s| s.status())
            .map(|st| st.last_frame_ns.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(0);
        if current_ns != self.last_seen_frame_ns {
            self.last_seen_frame_ns = current_ns;
            self.needs_redraw = true;
        }

        let continuous = self.notification.is_some() || self.benchmark_secs.is_some();
        if continuous {
            // Notification fade / benchmark FPS counter need a steady tick.
            self.needs_redraw = true;
        }

        if self.needs_redraw {
            self.needs_redraw = false;
            if continuous {
                LoopDirective::RedrawContinuous
            } else {
                LoopDirective::RedrawIdle
            }
        } else {
            LoopDirective::Idle
        }
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
        // Any user interaction needs a redraw.
        match &event {
            WindowEvent::RedrawRequested | WindowEvent::Destroyed => {}
            _ => { self.needs_redraw = true; }
        }

        // Tab / Shift+Tab are our channel-cycle keys; egui's default focus
        // handler would otherwise swallow them. We have no text inputs, so
        // short-circuit the egui forward and dispatch straight to handle_key.
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    physical_key: PhysicalKey::Code(KeyCode::Tab),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = &event
        {
            self.handle_key(elwt, KeyCode::Tab);
            return;
        }
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
                self.end_drag();
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                self.reset_hovered_view();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let scroll = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => (p.y / 50.0) as f32,
                };
                if scroll != 0.0 {
                    // Scrolling inside a cell zooms that cell. Scrolling on
                    // the bare background in Grid layout resizes the cells.
                    let over_cell = self
                        .cursor_pos
                        .and_then(|p| self.cell_at(p))
                        .is_some();
                    if over_cell {
                        self.apply_zoom(scroll);
                    } else if matches!(self.config.layout, LayoutMode::Grid) {
                        self.adjust_grid_size(scroll);
                    }
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

    fn about_to_wait(&mut self, elwt: &ActiveEventLoop) {
        self.benchmark_tick(elwt);
        let now = Instant::now();
        let directive = self.loop_directive(now);
        let request_redraw = || {
            if let Some(ctx) = self.render_ctx.as_ref() {
                ctx.window.request_redraw();
            }
        };
        match directive {
            LoopDirective::RedrawContinuous => {
                request_redraw();
                elwt.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                    now + CONTINUOUS_REPAINT_INTERVAL,
                ));
            }
            LoopDirective::RedrawIdle => {
                request_redraw();
                elwt.set_control_flow(winit::event_loop::ControlFlow::Wait);
            }
            LoopDirective::Idle => {
                // Poll + redraw at ~60 Hz even when no explicit redraw is
                // pending. Data arrives at ~5 Hz per channel, so without
                // this the UI only paints 5 fps on single-channel
                // monitoring — which is perceived as sluggishness (hover,
                // cursor, waterfall scroll all look choppy). Rendering a
                // static frame is cheap here (~3 ms CPU + ~3 ms GPU per
                // `ac-ui --synthetic --benchmark`) and wgpu `AutoVsync`
                // caps the actual rate at the display refresh.
                request_redraw();
                elwt.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                    now + CONTINUOUS_REPAINT_INTERVAL,
                ));
            }
        }
    }

    fn user_event(&mut self, _elwt: &ActiveEventLoop, _event: ()) {
        // Producer thread signalled a new frame — schedule a redraw on the
        // next `about_to_wait` cycle. The frame itself is pulled from the
        // triple buffer at render time.
        self.needs_redraw = true;
    }

    fn exiting(&mut self, _elwt: &ActiveEventLoop) {
        // Best-effort: tell the daemon to stop workers we started so it
        // doesn't keep capturing after the UI is gone. Network errors here
        // are fine — the daemon cleans up on its own disconnect timeout.
        self.send_transfer_stream_stop();
        self.send_monitor_spectrum_stop();
    }
}


#[cfg(test)]
mod loop_tests {
    //! State-machine tests for `App::loop_directive`. They exercise the
    //! exact path that turned into the "press `d` once, stay in
    //! continuous-repaint mode forever" regression — a notification
    //! lifecycle bug that neither the overlay paint tests nor the existing
    //! Rust unit tests covered. Each test drives `loop_directive` directly
    //! so it runs without a winit event loop, wgpu surface, or real
    //! ZMQ daemon.
    use super::*;

    use std::time::Duration;

    use crate::data::store::{
        ChannelStore, LoudnessStore, SweepStore, TransferStore, VirtualChannelStore,
    };
    use crate::data::types::ViewMode;

    fn fresh_app() -> App {
        let (inputs, store) = ChannelStore::new(1);
        App::new(AppInit {
            store,
            inputs,
            transfer_store: TransferStore::new(),
            virtual_channels: VirtualChannelStore::new(),
            sweep_store: SweepStore::new(),
            loudness_store: LoudnessStore::new(),
            source_kind: SourceKind::Synthetic,
            output_dir: PathBuf::new(),
            endpoint: String::new(),
            ctrl_endpoint: String::new(),
            synthetic_params: None,
            benchmark_secs: None,
            initial_view: ViewMode::Spectrum,
            initial_sweep_kind: None,
            monitor_channels: None,
            wake: None,
        })
    }

    /// Baseline sanity: after `App::new` the very first directive is a
    /// redraw (so the window actually paints once). The second is idle
    /// — no frame, no notification, no animation pending.
    #[test]
    fn idle_state_waits() {
        let mut app = fresh_app();
        let now = Instant::now();
        // `new` seeds `needs_redraw = true` so the first paint happens.
        assert_eq!(app.loop_directive(now), LoopDirective::RedrawIdle);
        // After that, nothing is pending → full idle.
        assert_eq!(app.loop_directive(now), LoopDirective::Idle);
    }

    /// The regression that motivated this test file: `self.notification`
    /// was never cleared, so `loop_directive` kept returning
    /// `RedrawContinuous` forever. After fix, a notification older than
    /// `NOTIFICATION_TTL` must drop the loop back to idle.
    #[test]
    fn notification_leak_cleared_after_ttl() {
        let mut app = fresh_app();
        let t0 = Instant::now();
        // Drain the initial redraw so we're looking at steady-state.
        let _ = app.loop_directive(t0);
        app.notify("timing on");
        // During the TTL window the loop should be in continuous repaint.
        assert_eq!(
            app.loop_directive(t0 + Duration::from_millis(100)),
            LoopDirective::RedrawContinuous,
            "notification fresh: continuous repaints expected",
        );
        // After the TTL the notification must be evicted and the loop
        // must go fully idle — no lingering continuous-repaint mode.
        let after = t0 + NOTIFICATION_TTL + Duration::from_millis(100);
        // Flush the RedrawContinuous that fires at the TTL boundary.
        let _ = app.loop_directive(after);
        assert_eq!(
            app.loop_directive(after),
            LoopDirective::Idle,
            "notification expired: loop must go idle (regression guard)",
        );
        assert!(app.notification.is_none(), "notification field leaked past TTL");
    }

    /// Benchmark mode drives continuous repaints for its whole duration;
    /// the state-machine must never fall back to Idle while
    /// `benchmark_secs` is active.
    #[test]
    fn benchmark_keeps_continuous() {
        let mut app = fresh_app();
        app.benchmark_secs = Some(5.0);
        let t0 = Instant::now();
        // Both the first and subsequent ticks must request redraws.
        assert_eq!(app.loop_directive(t0), LoopDirective::RedrawContinuous);
        assert_eq!(
            app.loop_directive(t0 + Duration::from_millis(50)),
            LoopDirective::RedrawContinuous,
        );
    }

    /// Explicit input-triggered redraw (key press / mouse) — input
    /// handlers set `needs_redraw = true`. Directive must be
    /// `RedrawIdle`: redraw now, then go fully idle afterwards because
    /// there is nothing time-driven running.
    #[test]
    fn input_redraw_then_idle() {
        let mut app = fresh_app();
        let t0 = Instant::now();
        let _ = app.loop_directive(t0);
        app.needs_redraw = true;
        assert_eq!(app.loop_directive(t0), LoopDirective::RedrawIdle);
        assert_eq!(app.loop_directive(t0), LoopDirective::Idle);
    }
}

