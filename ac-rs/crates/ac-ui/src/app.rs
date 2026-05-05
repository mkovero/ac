use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    ChannelStore, LoudnessStore, ScopeStore, SweepState, SweepStore, TransferStore, VirtualChannelStore,
};
use crate::data::types::{
    CellView, DisplayConfig, DisplayFrame, LayoutMode, SpectrumFrame, SweepKind, TransferFrame,
    TransferPair, ViewMode,
};
use crate::render::context::RenderContext;
use crate::render::ember::EmberRenderer;
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
    pub scope_store: ScopeStore,
    pub source_kind: SourceKind,
    pub output_dir: PathBuf,
    pub endpoint: String,
    pub ctrl_endpoint: String,
    pub synthetic_params: Option<(usize, usize, f32)>,
    pub benchmark_secs: Option<f64>,
    pub initial_view: ViewMode,
    pub initial_sweep_kind: Option<SweepKind>,
    pub monitor_channels: Option<Vec<u32>>,
    /// Surface present mode requested by the caller — usually parsed from
    /// `--present-mode` / `AC_UI_PRESENT_MODE` in main. Default
    /// `AutoVsync` (= `Fifo` on desktop) keeps current behaviour;
    /// `Mailbox` is the headline workaround for the NVIDIA + Vulkan
    /// `present()` busy-spin diagnosed in #109. Falls back gracefully if
    /// the surface doesn't advertise the mode (#110).
    pub present_mode: wgpu::PresentMode,
    /// Sleep between successive `RedrawContinuous` ticks. `--max-fps` /
    /// `AC_UI_MAX_FPS` map to `Duration::from_millis(1000 / hz)`; default
    /// is `CONTINUOUS_REPAINT_INTERVAL_DEFAULT` (33 ms ≈ 30 Hz).
    pub continuous_interval: Duration,
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
    /// Per-channel raw audio ring populated by the receiver from
    /// `visualize/scope` frames; consumed by the Goniometer /
    /// PhaseScope3D dispatch arms (`unified.md` Phase 0b).
    pub(super) scope_store: Option<ScopeStore>,
    /// Computed each render frame at the Goniometer dispatch arm;
    /// read by the overlay so the caption surfaces "ch X + Y" vs
    /// "synthetic — no stereo" vs "synthetic — daemon not streaming
    /// scope yet" to the user.
    pub(super) gonio_real_audio_state: crate::data::types::StereoStatus,
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
    pub(super) ember: Option<EmberRenderer>,
    /// Phase 0a synthetic-sine generator state for `ViewMode::Scope`. Phase
    /// continuity matters: regenerating sine_phase from scratch each frame
    /// would cause audible-rate phase jumps between frames, visible as
    /// stuttering on the strip-chart.
    pub(super) ember_sine_phase: f32,
    /// Timestamp of the previous ember frame, used to derive `dt` for both
    /// the synthetic generator (samples per frame) and the substrate decay.
    /// Separate from `last_render` so the value is fresh at the moment the
    /// substrate runs (last_render is updated at the *end* of redraw).
    pub(super) ember_last_tick: Option<Instant>,
    /// Scope view: vertical amplitude (multiplier on the synthetic sine,
    /// in [0,1] substrate-y units). Mouse-scroll over the cell shrinks /
    /// grows it; default 0.45 = ~90 % of cell height edge-to-edge.
    pub(super) ember_scope_y_gain: f32,
    /// Scope view: strip-chart window in seconds. Ctrl+scroll over the
    /// cell shrinks / grows it. Smaller window → fewer cycles fit, faster
    /// scroll across the cell.
    pub(super) ember_scope_window_s: f32,
    /// Phase 1 trajectory views (unified.md §6) — synthetic stereo source
    /// shared by Goniometer + PhaseScope3D. Same 1 kHz carrier on both
    /// channels, with a slowly-drifting phase offset (0.3 Hz) so the
    /// figure walks through every phase state — in-phase line → ellipse
    /// → circle → ellipse → anti-phase line — in a ~3 s loop. That's
    /// what a goniometer actually visualizes; two incommensurate
    /// frequencies would just draw a meaningless Lissajous.
    pub(super) ember_gonio_carrier_phase: f32,
    pub(super) ember_gonio_phase_offset: f32,
    /// Goniometer rotation: `true` = M/S ((L−R)/√2, (L+R)/√2), `false` = raw
    /// (L, R). Default M/S — matches the analog-meter convention where
    /// in-phase mono draws a vertical line, out-of-phase a horizontal one.
    pub(super) ember_gonio_rotation_ms: bool,
    /// Global intensity multiplier applied to *every* ember view's base
    /// intensity at dispatch time. `,` / `.` adjust it geometrically
    /// (×1.25 per press) so the user can tune deposit brightness live
    /// without rebuilding. Default 1.0 leaves per-view tuning intact.
    pub(super) ember_intensity_scale: f32,
    /// Global τ_p multiplier — ember fade rate. `Shift+,` / `Shift+.`
    /// adjust geometrically. Lower = faster fade (more transient feel);
    /// higher = longer trails (more diff-friendly). Default 1.0.
    pub(super) ember_tau_p_scale: f32,
    /// Running peak of the (L, R) real-audio source for the
    /// Goniometer / PhaseScope3D auto-gain. Updated at dispatch from
    /// each frame's max(|L|, |R|) and decayed slowly so transient
    /// loudness peaks don't permanently shrink the figure. Inverse of
    /// this drives the per-frame display scale so the figure fills
    /// ~90 % of the cell regardless of input level.
    pub(super) ember_stereo_peak: f32,
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
    /// Mirror of the daemon's `mic_correction_enabled` flag. Local copy
    /// so `Shift+M` can toggle without a round-trip; the daemon's reply
    /// to `set_mic_correction_enabled` is fire-and-forget. Defaults `true`
    /// so a freshly loaded curve takes effect immediately.
    pub(super) mic_correction_enabled: bool,
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
    box_zoom: Option<input::BoxZoomState>,
    timing_stats: TimingStats,
    show_timing: bool,
    benchmark_secs: Option<f64>,
    benchmark_started: Option<Instant>,
    benchmark_report: Option<String>,
    /// Last `ReceiverStatus::last_frame_ns` value we saw. Compared in
    /// `about_to_wait` to decide whether new data arrived since the last
    /// render — if not, skip the redraw to save CPU.
    last_seen_frame_ns: u64,
    /// Last `last_frame_ns` we actually painted with. Distinct from
    /// `last_seen_frame_ns` so the skip-when-unchanged check in
    /// `loop_directive` is always a real "frame newer than what's on
    /// screen?" test — not "have we ever seen this frame?". Without
    /// this, `--max-fps 60` against a 30 Hz daemon paints the same
    /// content twice per data tick on the wgpu/NVIDIA stack where
    /// each `present()` is ~16 ms of CPU; with it, identical-content
    /// frames are dropped and the actual paint rate falls back to the
    /// content-change rate (= data rate). The `--max-fps` value
    /// becomes a true upper bound rather than a target. (#109.)
    last_painted_frame_ns: u64,
    /// Wall-clock timestamp of the most recent producer wake (data thread
    /// `EventLoopProxy::send_event(())`). Used by `loop_directive` to
    /// keep `RedrawContinuous` active while data is flowing, so the UI
    /// renders at vsync between daemon ticks (smooth waterfall scroll,
    /// peak-hold decay, hover labels) instead of being slaved to the
    /// measurement interval. Drops back to `Idle` once the value ages
    /// past `DATA_LIVELINESS_WINDOW`.
    last_data_arrival: Option<Instant>,
    /// Surface present mode chosen by the caller (CLI / env / default).
    /// Held here so `init_graphics` can hand it to `RenderContext::new`
    /// when winit creates the window — instance/surface creation happens
    /// after `App::new` so the value has to round-trip through state.
    requested_present_mode: wgpu::PresentMode,
    /// Sleep budget between continuous-repaint frames. Picked from
    /// `--max-fps` / `AC_UI_MAX_FPS` and set in stone for the App's
    /// lifetime; feeds `WaitUntil` in `about_to_wait`. Default 33 ms
    /// (≈ 30 Hz) keeps NVIDIA's expensive `present()` cost from
    /// doubling vs the matched-to-data-rate baseline (#109/#110).
    continuous_interval: Duration,
    /// Wall-clock of the most recent continuous-mode paint. Used as a
    /// min-gap rate limiter: the next paint may happen no sooner than
    /// `last_continuous_paint_at + continuous_interval`. Pre-#109 this
    /// was a *deadline* (fixed grid) which had a phase race with
    /// regular data arrival — daemon ticks at 16 ms, UI deadline at
    /// 16 ms, any thread-scheduling jitter and the deadline tick read
    /// stale `last_frame_ns`, skipped, and the effective paint rate
    /// halved. Min-gap pacing paints on data arrival (or any wake that
    /// brings new content) provided the gap has elapsed, so dropped
    /// frames from phase races can't happen.
    last_continuous_paint_at: Option<Instant>,
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
        let requested_present_mode = init.present_mode;
        let continuous_interval = init.continuous_interval;
        let layout = if sweep_kind.is_some() {
            LayoutMode::Sweep
        } else if matches!(
            init.initial_view,
            ViewMode::Scope
                | ViewMode::SpectrumEmber
                | ViewMode::Goniometer
                | ViewMode::IoTransfer
                | ViewMode::BodeMag
                | ViewMode::Coherence
                | ViewMode::BodePhase
                | ViewMode::GroupDelay
                | ViewMode::Nyquist
        ) {
            LayoutMode::Single
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
            scope_store: None,
            gonio_real_audio_state: crate::data::types::StereoStatus::NoAudio,
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
            ember: None,
            ember_sine_phase: 0.0,
            ember_last_tick: None,
            ember_scope_y_gain: 0.45,
            ember_scope_window_s: 0.1,
            ember_gonio_carrier_phase: 0.0,
            ember_gonio_phase_offset: 0.0,
            ember_gonio_rotation_ms: true,
            ember_intensity_scale: 1.0,
            ember_tau_p_scale: 1.0,
            ember_stereo_peak: 0.5,
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
            mic_correction_enabled: true,
            // Default smoothing OFF — show the raw FFT trace so peaks read
            // at their actual amplitude. Cycle through 1/24..1/3 with `O`
            // when a calmer floor is wanted (single-bin tones drop by
            // ~10·log10(N_bins_in_window) at each setting; that's physics,
            // not a bug). Bottom-strip in_dbu always uses time-domain RMS
            // so the analog-level readout is unaffected by this default.
            smoothing_frac: None,
            smoothing_cache: None,
            palette_scroll_accum: 0.0,
            output_dir,
            notification: None,
            modifiers: ModifiersState::empty(),
            last_render: Instant::now(),
            cursor_pos: None,
            drag: None,
            box_zoom: None,
            timing_stats: TimingStats::new(),
            show_timing,
            benchmark_secs,
            benchmark_started: None,
            benchmark_report: None,
            last_seen_frame_ns: 0,
            last_painted_frame_ns: 0,
            last_data_arrival: None,
            requested_present_mode,
            continuous_interval,
            last_continuous_paint_at: None,
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
        let ctx = pollster::block_on(RenderContext::new(
            window.clone(),
            self.requested_present_mode,
        )).expect("wgpu init");
        let format = ctx.surface_format();
        let spectrum = SpectrumRenderer::new(&ctx.device, format);
        let waterfall = WaterfallRenderer::new(&ctx.device, &ctx.queue, format);
        let ember = EmberRenderer::new(&ctx.device, &ctx.queue, format);
        let egui_renderer = egui_wgpu::Renderer::new(&ctx.device, format, None, 1, false);
        self.egui_ctx.set_visuals(render_pipeline::dark_visuals());
        let viewport_id = self.egui_ctx.viewport_id();
        let egui_state =
            egui_winit::State::new(self.egui_ctx.clone(), viewport_id, &window, None, None, None);
        self.render_ctx = Some(ctx);
        self.spectrum = Some(spectrum);
        self.waterfall = Some(waterfall);
        self.ember = Some(ember);
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

        // Producer threads wake us via `send_event(())`; track the latest
        // frame we've seen so we can dedupe back-to-back wakes for the
        // same frame.
        let current_ns = self
            .source
            .as_ref()
            .and_then(|s| s.status())
            .map(|st| st.last_frame_ns.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(0);
        self.last_seen_frame_ns = current_ns;

        // Used by the non-continuous (input-only) path below to decide
        // whether a fresh frame justifies a one-shot RedrawIdle. In
        // continuous mode, paint timing is purely gap-paced — content
        // freshness doesn't gate paints any more (see "content-blind"
        // comment below).
        let new_data = current_ns != self.last_painted_frame_ns;

        // Continuous-repaint triggers — anything that visibly animates
        // independently of input wakes.
        let data_recent = self
            .last_data_arrival
            .is_some_and(|t| now.saturating_duration_since(t) < DATA_LIVELINESS_WINDOW);
        let hold_active = self.peak_hold_enabled || self.min_hold_enabled;
        let cursor_pinned = self.drag.is_some() || self.box_zoom.is_some();
        let continuous = self.notification.is_some()
            || self.benchmark_secs.is_some()
            || data_recent
            || hold_active
            || cursor_pinned;

        if continuous {
            // Min-gap pacing, content-blind: paint at every gap-eligible
            // wake whether or not anything has changed since the last
            // paint. Earlier we tried to skip "redundant" paints (frames
            // with no new content), but on fixed-refresh Wayland +
            // NVIDIA Vulkan/GL each skipped present is a missed vsync
            // and reads as visible judder — the compositor keeps the
            // previous frame on screen but the eye still picks up the
            // irregular cadence as stutter. Cost of presenting an
            // identical frame is small relative to the perceived loss
            // of smoothness, so accept it. Idle (no data, no animation)
            // still drops out of `continuous` and pays nothing.
            let rate_ok = self
                .last_continuous_paint_at
                .map_or(true, |t| now.saturating_duration_since(t) >= self.continuous_interval);
            if rate_ok {
                self.last_continuous_paint_at = Some(now);
                self.last_painted_frame_ns = current_ns;
                self.needs_redraw = false;
                return LoopDirective::RedrawContinuous;
            }
            // Rate-limited (we just painted within `continuous_interval`).
            // `about_to_wait` re-arms `WaitUntil(next_eligible)` so the
            // very next gap-tick fires regardless of whether a producer
            // event arrives in that window — what gives smooth motion.
            return LoopDirective::Idle;
        }

        // Not in continuous mode — drop any leftover paint timestamp so
        // the next entry into continuous mode paints immediately rather
        // than waiting out a stale gap.
        self.last_continuous_paint_at = None;

        if self.needs_redraw || new_data {
            self.needs_redraw = false;
            self.last_painted_frame_ns = current_ns;
            LoopDirective::RedrawIdle
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
                if self.box_zoom.is_some() {
                    self.update_box_zoom(position);
                    self.needs_redraw = true;
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
                self.begin_box_zoom();
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Right,
                ..
            } => {
                self.end_box_zoom();
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
            LoopDirective::RedrawContinuous | LoopDirective::RedrawIdle => {
                request_redraw();
                // Pure event-driven: producer wakes (data), input events,
                // and the rate-limiter's `WaitUntil(next_eligible)` from
                // the Idle arm below all bring the loop back here when
                // something might want to paint. No pre-scheduled
                // deadline — `loop_directive` rate-limits inline using
                // `last_continuous_paint_at`, so we don't need a fixed
                // grid that races with data arrival.
                elwt.set_control_flow(winit::event_loop::ControlFlow::Wait);
            }
            LoopDirective::Idle => {
                // If continuous mode is active and we just painted, wake
                // at the earliest paint-eligible instant so animations
                // (peak-hold decay, notification fade) keep ticking even
                // if no data arrives in that window. Without this, an
                // animating-only state stalls until the next input event.
                if let Some(t) = self.last_continuous_paint_at {
                    let next_eligible = t + self.continuous_interval;
                    if next_eligible > now {
                        elwt.set_control_flow(
                            winit::event_loop::ControlFlow::WaitUntil(next_eligible),
                        );
                        return;
                    }
                }
                elwt.set_control_flow(winit::event_loop::ControlFlow::Wait);
            }
        }
    }

    fn user_event(&mut self, _elwt: &ActiveEventLoop, _event: ()) {
        // Producer thread signalled a new frame. Stamp `last_data_arrival`
        // so the loop stays in continuous-repaint mode while data flows;
        // do NOT set `needs_redraw` — the continuous-tick deadline is the
        // sole source of truth for paint timing in that mode. The latest
        // frame is pulled from the triple buffer at the next paint.
        // (#109 rate-limiter fix.)
        self.last_data_arrival = Some(Instant::now());
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
        ChannelStore, LoudnessStore, ScopeStore, SweepStore, TransferStore, VirtualChannelStore,
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
            scope_store: ScopeStore::new(),
            source_kind: SourceKind::Synthetic,
            output_dir: PathBuf::new(),
            endpoint: String::new(),
            ctrl_endpoint: String::new(),
            synthetic_params: None,
            benchmark_secs: None,
            initial_view: ViewMode::Spectrum,
            initial_sweep_kind: None,
            monitor_channels: None,
            present_mode: wgpu::PresentMode::AutoVsync,
            continuous_interval: CONTINUOUS_REPAINT_INTERVAL_DEFAULT,
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

    /// Decoupling guard: while data has arrived recently AND something
    /// visibly animates (peak hold here), the loop must stay in
    /// `RedrawContinuous` so the UI renders at vsync. With the
    /// skip-when-unchanged gate added in #109, plain "data arrived"
    /// without new content produces `Idle`; an animating overlay is the
    /// trigger that locks the rate to vsync between data ticks.
    #[test]
    fn data_arrival_starts_continuous() {
        let mut app = fresh_app();
        let t0 = Instant::now();
        let _ = app.loop_directive(t0); // drain the initial paint
        // Simulate a producer wake plus a time-driven animation, so the
        // skip-when-unchanged gate fires `paint` even without new data
        // (the test scaffolding can't easily inject `last_frame_ns`).
        app.last_data_arrival = Some(t0);
        app.peak_hold_enabled = true;
        assert_eq!(app.loop_directive(t0), LoopDirective::RedrawContinuous);
        assert_eq!(
            app.loop_directive(t0 + app.continuous_interval),
            LoopDirective::RedrawContinuous,
            "still animating: paints at the next deadline",
        );
    }

    /// Pins the actual rate-limiter behaviour, which the original
    /// "decouple" PR claimed but didn't deliver. Symptom: producer threads
    /// wake `about_to_wait` via `EventLoopProxy::send_event(())`, which
    /// interrupts `WaitUntil(continuous_interval)` early. Pre-fix every
    /// such wake hit `if continuous { needs_redraw = true; }` and produced
    /// a paint, so a 4-channel monitor at 30 Hz fired 120 paints/sec
    /// instead of 30. With this fix, all wakes between deadline ticks
    /// coalesce to a single paint at-or-after the deadline.
    #[test]
    fn continuous_mode_rate_limits_paints() {
        let mut app = fresh_app();
        let t0 = Instant::now();
        let _ = app.loop_directive(t0);

        app.last_data_arrival = Some(t0);
        // Force a steady time-driven animation so paints aren't dropped
        // by the skip-when-unchanged gate; this test is about *pacing*,
        // not about content-change suppression.
        app.peak_hold_enabled = true;

        // First wake in continuous mode: paint now, stamp the gap.
        assert_eq!(app.loop_directive(t0), LoopDirective::RedrawContinuous);
        assert!(
            app.last_continuous_paint_at.is_some(),
            "RedrawContinuous must stamp last_continuous_paint_at",
        );

        // Subsequent wakes inside the interval: no paint, just keep state
        // moving. This is the regression-guard part — pre-fix every wake
        // returned RedrawContinuous.
        let interval_ms = app.continuous_interval.as_millis() as u64;
        for ms in [1, 5, 10, interval_ms / 2, interval_ms - 1] {
            assert_eq!(
                app.loop_directive(t0 + Duration::from_millis(ms)),
                LoopDirective::Idle,
                "wake at +{ms} ms inside interval ({} ms) must NOT paint",
                interval_ms,
            );
        }

        // At the deadline: paint again, schedule next.
        assert_eq!(
            app.loop_directive(t0 + app.continuous_interval),
            LoopDirective::RedrawContinuous,
            "wake at deadline must paint",
        );
    }

    /// Once data has stopped flowing, the liveness window must expire and
    /// the loop must fall fully idle — no continuous-repaint leak that
    /// would re-create the #108 100 %-CPU symptom whenever a monitor was
    /// stopped.
    #[test]
    fn data_arrival_window_expires() {
        let mut app = fresh_app();
        let t0 = Instant::now();
        let _ = app.loop_directive(t0);
        app.last_data_arrival = Some(t0);
        app.peak_hold_enabled = true;  // animating, so the inside-window
                                       // tick paints rather than skipping.
        // Inside the window: continuous.
        assert_eq!(app.loop_directive(t0), LoopDirective::RedrawContinuous);
        // Stop the animation too — outside the liveness window with no
        // animation and no input, the loop must reach `Idle`.
        app.peak_hold_enabled = false;
        let past = t0 + DATA_LIVELINESS_WINDOW + Duration::from_millis(50);
        let _ = app.loop_directive(past);
        assert_eq!(
            app.loop_directive(past),
            LoopDirective::Idle,
            "past liveness window with no new data, no animation: must idle",
        );
    }

    /// Min-gap pacing: the boundary itself counts as gap-eligible
    /// (`now - last_paint >= interval` is `>=`, not `>`). Pre-fix
    /// this was deadline-based and had a phase race — daemon ticks
    /// aligned with UI deadlines could read stale `last_frame_ns`
    /// and skip the paint, halving effective fps. Min-gap pacing
    /// has no grid to align with: as long as the gap has elapsed,
    /// we paint regardless of content (continuous mode is
    /// content-blind by design — see
    /// `continuous_mode_paints_every_gap_even_without_new_content`).
    #[test]
    fn paint_at_gap_boundary() {
        let mut app = fresh_app();
        let t0 = Instant::now();
        let _ = app.loop_directive(t0);

        app.last_data_arrival = Some(t0);
        assert_eq!(app.loop_directive(t0), LoopDirective::RedrawContinuous);

        // Wake exactly at +continuous_interval: gap met, paint.
        let at_boundary = t0 + app.continuous_interval;
        assert_eq!(
            app.loop_directive(at_boundary),
            LoopDirective::RedrawContinuous,
            "wake at gap boundary must paint (no phase race)",
        );

        // One ns before the boundary: rate-limited, defer.
        let mut app = fresh_app();
        let _ = app.loop_directive(t0);
        app.last_data_arrival = Some(t0);
        assert_eq!(app.loop_directive(t0), LoopDirective::RedrawContinuous);
        let just_before = t0 + app.continuous_interval - Duration::from_nanos(1);
        assert_eq!(
            app.loop_directive(just_before),
            LoopDirective::Idle,
            "wake one ns before gap boundary: rate-limited, no paint",
        );
    }

    /// Continuous mode is content-blind by design (#109 follow-up):
    /// in continuous mode every gap-eligible wake paints, regardless
    /// of whether `last_frame_ns` has advanced. The earlier
    /// "skip-when-unchanged" optimisation traded perceived smoothness
    /// for CPU on Wayland-NVIDIA — each skipped present was a missed
    /// vsync and the eye reads that as judder. Cost of presenting
    /// duplicate content is small; perceived stutter is large; so
    /// we present every vsync while continuous mode holds, and rely
    /// on falling out of `continuous` for the idle savings.
    #[test]
    fn continuous_mode_paints_every_gap_even_without_new_content() {
        let mut app = fresh_app();
        let t0 = Instant::now();
        let _ = app.loop_directive(t0);

        // Enter continuous mode via data_recent only — no animation,
        // no input. Pre-revert this scenario would skip subsequent
        // paints because nothing "looked new"; post-revert each
        // gap-eligible wake paints to keep vsync cadence smooth.
        app.last_data_arrival = Some(t0);
        assert_eq!(app.loop_directive(t0), LoopDirective::RedrawContinuous);

        // No animation, no new last_frame_ns, but past the gap →
        // paint anyway. This is the regression guard against
        // re-introducing skip-when-unchanged.
        let past_gap = t0 + app.continuous_interval + Duration::from_millis(1);
        assert_eq!(
            app.loop_directive(past_gap),
            LoopDirective::RedrawContinuous,
            "continuous mode must present every gap-eligible vsync \
             regardless of content change (#109 follow-up: skip-when-\
             unchanged caused visible stutter on Wayland-NVIDIA)",
        );
        assert_eq!(
            app.last_continuous_paint_at,
            Some(past_gap),
            "successful paint must update last_continuous_paint_at",
        );
    }

    /// Peak hold (and the symmetric min hold) are time-driven animations
    /// — the held line decays toward live at 20 dB/s. While either is
    /// enabled the loop runs continuous so the decay reads as motion;
    /// toggling them off must drop the loop back to idle.
    #[test]
    fn peak_or_min_hold_keeps_continuous() {
        let mut app = fresh_app();
        let t0 = Instant::now();
        let _ = app.loop_directive(t0);

        app.peak_hold_enabled = true;
        assert_eq!(app.loop_directive(t0), LoopDirective::RedrawContinuous);

        // Advance past the freshly-armed continuous deadline so the next
        // call is genuinely "due" again (rate-limiter is now strict).
        let past = t0 + app.continuous_interval + Duration::from_millis(10);
        app.peak_hold_enabled = false;
        app.min_hold_enabled = true;
        assert_eq!(app.loop_directive(past), LoopDirective::RedrawContinuous);

        app.min_hold_enabled = false;
        let _ = app.loop_directive(past);
        assert_eq!(app.loop_directive(past), LoopDirective::Idle);
    }

    /// An active drag or box-zoom keeps the loop continuous so the rubber
    /// band / pan preview tracks vsync even if the user holds the cursor
    /// still mid-gesture. Releasing the mouse must drop the loop back to
    /// idle.
    #[test]
    fn drag_or_box_zoom_keeps_continuous() {
        use crate::app::input::{BoxZoomState, DragState};
        use winit::dpi::PhysicalPosition;

        let mut app = fresh_app();
        let t0 = Instant::now();
        let _ = app.loop_directive(t0);

        app.drag = Some(DragState {
            start: PhysicalPosition::new(0.0, 0.0),
            targets: Vec::new(),
            start_log_min: 0.0,
            start_log_max: 0.0,
            start_db_min: 0.0,
            start_db_max: 0.0,
            cell_w_px: 1.0,
            cell_h_px: 1.0,
        });
        assert_eq!(app.loop_directive(t0), LoopDirective::RedrawContinuous);

        // Advance past the deadline before flipping to box-zoom — otherwise
        // the strict rate-limiter coalesces the second paint into the
        // already-scheduled tick.
        let past = t0 + app.continuous_interval + Duration::from_millis(10);
        app.drag = None;
        app.box_zoom = Some(BoxZoomState {
            start: PhysicalPosition::new(0.0, 0.0),
            current: PhysicalPosition::new(0.0, 0.0),
            targets: Vec::new(),
            cell_left_px: 0.0,
            cell_top_px: 0.0,
            cell_w_px: 1.0,
            cell_h_px: 1.0,
            start_log_min: 0.0,
            start_log_max: 0.0,
            start_db_min: 0.0,
            start_db_max: 0.0,
            start_rows_f: 0.0,
            waterfall: false,
        });
        assert_eq!(app.loop_directive(past), LoopDirective::RedrawContinuous);

        app.box_zoom = None;
        let _ = app.loop_directive(past);
        assert_eq!(app.loop_directive(past), LoopDirective::Idle);
    }

    /// Regression #108: the `Idle` directive must mean "no work pending,
    /// block on events" — the `about_to_wait` arm for Idle MUST NOT call
    /// `request_redraw` or `WaitUntil`. On NVIDIA + Vulkan `AutoVsync`
    /// the proprietary driver busy-waits inside `surface.present()`, so a
    /// 60 Hz forced redraw on a static frame burns one full core (100 %
    /// CPU) regardless of which view is shown. The previous attempt at
    /// fixing 5 Hz "sluggishness" in `04304252` reintroduced exactly
    /// this; reverted in #108 since monitor data now arrives at ~30 Hz
    /// and any genuine continuous-animation case (notification fade,
    /// benchmark FPS) routes through `RedrawContinuous`.
    ///
    /// This test pins the directive-level contract: many sustained idle
    /// ticks stay `Idle`, and no internal state (peak hold, min hold,
    /// per-channel stores) silently flips them back to a redraw arm.
    /// `about_to_wait`'s control-flow side is enforced by code review +
    /// the comment block on the Idle arm.
    #[test]
    fn sustained_idle_never_silently_redraws() {
        let mut app = fresh_app();
        // Drain the seeded first-paint redraw.
        let _ = app.loop_directive(Instant::now());

        // Simulate 200 loop-tick iterations spaced 16 ms apart — what
        // a 60 Hz polling cadence would give us if the bug returned.
        let t0 = Instant::now();
        for i in 0..200 {
            let now = t0 + Duration::from_millis(i * 16);
            assert_eq!(
                app.loop_directive(now),
                LoopDirective::Idle,
                "tick {i} flipped to a redraw with no event — \
                 #108 regression: something added request_redraw to Idle path",
            );
        }
        assert!(!app.needs_redraw, "needs_redraw must stay false across idle ticks");
    }
}

