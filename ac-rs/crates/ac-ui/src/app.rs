use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::Color32;
use triple_buffer::Input;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::data::control::CtrlClient;
use crate::data::receiver::{ReceiverHandle, ReceiverStatus};
use crate::data::smoothing;
use crate::data::store::{
    ChannelStore, SweepState, SweepStore, TransferStore, TunerStore, VirtualChannelStore,
};
use crate::data::synthetic::SyntheticHandle;
use crate::data::types::{
    CellView, DisplayConfig, DisplayFrame, FrameMeta, LayoutMode, SpectrumFrame, SweepKind,
    TransferFrame, TransferPair, TunerMode, ViewMode,
};
use crate::render::context::RenderContext;
use crate::render::grid;
use crate::render::spectrum::{ChannelMeta, ChannelUpload, SpectrumRenderer};
use crate::render::waterfall::{CellUpload as WaterfallCellUpload, WaterfallRenderer};
use crate::theme;
use crate::ui::export::{self, ScreenshotRequest};
use crate::ui::layout::{self, GridParams};
use crate::ui::overlay::{self, HoverInfo, HoverReadout, MonitorParamsInfo, OverlayInput};
use crate::ui::stats::{StatsSnapshot, TimingStats};

/// How long a notification string stays visible in the overlay. Also gates
/// the continuous-repaint window: while a notification is live we repaint at
/// ~60 Hz so the fade / pop-in feels right; after it expires we drop back to
/// event-driven idle. Was previously a 1200 ms magic literal at the single
/// overlay-display site; lifted so `about_to_wait` can clear `self.notification`
/// at the same boundary instead of leaking state forever.
pub const NOTIFICATION_TTL: Duration = Duration::from_millis(1200);

/// Frame cap for continuous repaint windows (notification fade, benchmark).
pub const CONTINUOUS_REPAINT_INTERVAL: Duration = Duration::from_millis(16);

/// Left/Right arrow tunes FFT monitor refresh rate in 1 ms steps (Left =
/// slower, Right = faster). Clamped to [`MONITOR_INTERVAL_MIN_MS`,
/// `MONITOR_INTERVAL_MAX_MS`]. The FLOOR/CEIL below bracket what the eye
/// perceives as "live": below 33 ms (30 Hz) we're wasting CPU past the
/// display refresh, above 50 ms (20 Hz) the motion starts to step.
pub const MONITOR_INTERVAL_MIN_MS: u32 = 1;
pub const MONITOR_INTERVAL_MAX_MS: u32 = 1000;
pub const MONITOR_INTERVAL_FLOOR_MS: u32 = 33;
pub const MONITOR_INTERVAL_CEIL_MS: u32 = 50;

/// Pick a smooth monitor tick for a given FFT size + sample rate. Targets
/// ~window/8 (87.5% overlap) so consecutive frames share most of their
/// input and motion reads as continuous even when N is large; floored at
/// 33 ms so tiny N doesn't burn cycles past the display refresh, ceilinged
/// at 50 ms so huge N still feels alive rather than stepped. Arrow keys
/// still let the user push outside this band manually.
pub fn auto_monitor_interval_ms(fft_n: u32, sr_hz: u32) -> u32 {
    let sr = sr_hz.max(1) as f32;
    let window_ms = (fft_n as f32 * 1000.0) / sr;
    let target = (window_ms / 8.0).round().max(1.0) as u32;
    target.clamp(MONITOR_INTERVAL_FLOOR_MS, MONITOR_INTERVAL_CEIL_MS)
}

/// Rolling window for the waterfall row-period estimator. Median over the
/// last N row-to-row dt samples; bigger → more stable axis labels, smaller
/// → faster tracking if the producer cadence genuinely changes. 16 at 10 Hz
/// means the axis responds to real cadence shifts within ~1.6 s while
/// single-frame jitter is absorbed by the median.
pub const WATERFALL_ROW_DT_WINDOW: usize = 16;

/// Peak-hold release: how long the held value sits unchanged before it
/// starts falling toward the live trace. A standard audio-meter "attack-0,
/// release-after-hold" behaviour — a transient pins the trace for 3 s, then
/// the line glides back down at a bounded rate instead of snapping.
pub const PEAK_HOLD_DECAY: Duration = Duration::from_secs(1);

/// Fall rate once release kicks in. 20 dB/s matches the perceived cadence of
/// analogue peak-program meters — fast enough to track genuine level drops,
/// slow enough that the user can still read the number on the way down. Also
/// drives min-hold's symmetric rise toward live.
pub const PEAK_RELEASE_DB_PER_SEC: f32 = 20.0;

/// Drum-tuner search window for the (0,1) fundamental. 40 Hz is below any
/// reasonable kick resonance; 2 kHz sits above the tightest piccolo snare.
/// Overtone partials can fall anywhere above this ceiling — only the
/// fundamental candidate itself is gated.
pub const TUNER_SEARCH_MIN_HZ: f64 = 40.0;
pub const TUNER_SEARCH_MAX_HZ: f64 = 2000.0;
/// Below this confidence score we render nothing — a wrong-but-labelled
/// peak is worse than no peak at all when the user is chasing cents.
pub const TUNER_MIN_CONFIDENCE: f64 = 0.25;
/// Half-width (as fraction of f0) of the Space-to-lock search range
/// around the current fundamental. 0.2 = ±20% covers a couple of
/// semitones of detune without admitting neighbouring octaves.
pub const TUNER_RANGE_LOCK_FRAC: f64 = 0.20;
/// Min-level step (dBFS) applied by `+`/`-` while tuner mode is active.
pub const TUNER_MIN_LEVEL_STEP_DB: f32 = 1.0;
pub const TUNER_MIN_LEVEL_FLOOR_DBFS: f32 = -120.0;
pub const TUNER_MIN_LEVEL_CEIL_DBFS: f32 = -10.0;

/// Detector sensitivity preset cycled by `Shift+U`. Each preset maps to a
/// `(trigger_delta_db, min_confidence)` pair; the absolute-level gate is
/// controlled separately by `+`/`-` so the user can dial it independently
/// of the edge/confidence pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TunerSensitivity {
    /// Lenient — fires on quieter/closer-spaced overtone stacks. Room
    /// ambience can false-fire.
    Low,
    /// Default balanced preset.
    #[default]
    Mid,
    /// Strict — rejects all but clearly-voiced hits.
    High,
}

impl TunerSensitivity {
    pub fn label(self) -> &'static str {
        match self {
            TunerSensitivity::Low => "Low",
            TunerSensitivity::Mid => "Mid",
            TunerSensitivity::High => "High",
        }
    }
    /// `(trigger_delta_db, min_confidence)`.
    pub fn params(self) -> (f32, f64) {
        match self {
            TunerSensitivity::Low  => (6.0,  0.25),
            TunerSensitivity::Mid  => (10.0, 0.35),
            TunerSensitivity::High => (15.0, 0.45),
        }
    }
    pub fn next(self) -> Self {
        match self {
            TunerSensitivity::Low  => TunerSensitivity::Mid,
            TunerSensitivity::Mid  => TunerSensitivity::High,
            TunerSensitivity::High => TunerSensitivity::Low,
        }
    }
}
/// Need at least this many dt samples in the window before we trust the
/// median enough to replace the 0.1 s default. Below this we keep the
/// default so the first couple of frames don't set a wildly wrong period.
pub const WATERFALL_ROW_DT_MIN: usize = 5;
/// Relative-change gate: only repaint the row-period when the new median
/// differs by more than this fraction from the current value. Kills label
/// flipping caused by micro-jitter in the median without blocking real
/// cadence shifts.
pub const WATERFALL_ROW_DT_HYSTERESIS: f32 = 0.03;

/// Median of an f32 slice, ignoring NaN. Returns `None` if empty.
pub fn median_f32(samples: &[f32]) -> Option<f32> {
    let mut v: Vec<f32> = samples.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    })
}

/// Up/Down arrow tunes FFT size (bin count) through this ladder. Up → larger
/// N (finer resolution), Down → smaller N (coarser but faster capture).
/// Protocol rejects anything outside [256, 131072] or non-pow2.
pub const MONITOR_FFT_N_LADDER: &[u32] =
    &[1024, 2048, 4096, 8192, 16384, 32768, 65536, 131072];

/// Step a ladder: find `current`'s index, move by `delta`, clamp to bounds.
/// Returns the new value, or `current` if it wasn't on the ladder (keeps the
/// UI coherent when the daemon default drifts from the UI default).
pub fn step_ladder(ladder: &[u32], current: u32, delta: i32) -> u32 {
    let Some(idx) = ladder.iter().position(|&v| v == current) else {
        return current;
    };
    let new_idx = (idx as i32 + delta).clamp(0, ladder.len() as i32 - 1) as usize;
    ladder[new_idx]
}

/// Pure-state result of the `about_to_wait` decision, extracted so the same
/// logic can be unit-tested without a winit event loop. Translating this to
/// winit calls is the only thing `about_to_wait` does on top of calling
/// `App::loop_directive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopDirective {
    /// Redraw now, then keep the loop running at ~60 Hz until next tick
    /// (notification fade / benchmark).
    RedrawContinuous,
    /// Redraw now, then block on events (data wake-ups, OS input).
    RedrawIdle,
    /// Don't redraw, wait indefinitely for the next event.
    Idle,
}

pub enum DataSource {
    // Retained: handle owns the synthetic worker thread; dropping it stops the thread.
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
    pub transfer_store: TransferStore,
    pub virtual_channels: VirtualChannelStore,
    pub sweep_store: SweepStore,
    pub tuner_store: TunerStore,
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

pub enum SourceKind {
    Synthetic,
    Daemon,
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
    tuner_store: Option<TunerStore>,
    sweep_kind: Option<SweepKind>,
    sweep_last: SweepState,
    sweep_selected_idx: Option<usize>,
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
    /// Live FFT monitor knobs (interval 1 ms steps in [1, 1000] ms;
    /// `MONITOR_FFT_N_LADDER` for N). Mutated by plain arrow keys in FFT mode
    /// and pushed to the daemon via `set_monitor_params`.
    monitor_interval_ms: u32,
    monitor_fft_n: u32,
    /// Insertion-order view of `selected`. In Transfer layout the convention
    /// is: the **last** entry is REF, every preceding entry is a meas channel
    /// the user would like to H1-compare against that ref. Only one meas
    /// stream runs at a time (daemon worker + display), selected via
    /// `active_meas_idx`; Tab cycles through the meas list.
    selection_order: Vec<usize>,
    /// Index into `selection_order[..len-1]` picking which meas channel is
    /// currently streamed/displayed in Transfer layout. Clamped on every
    /// consumer read; Tab/Shift+Tab bump it.
    active_meas_idx: usize,
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
    /// Drum tuner tri-state. Off → Live → Locked → Off. Cycled by `U`. In
    /// Live mode the identifier runs every frame on each channel's peak-hold
    /// buffer; in Locked mode it keeps running so the readout updates when
    /// the user retunes a lug, but the target stays pinned to whatever
    /// `freq_hz` was current at the moment of the 2nd press.
    tuner_mode: TunerMode,
    /// Per-channel tuning target set on 2nd `U` press. Independent per
    /// channel so a two-drum setup (kick CH0 + snare CH1) can carry two
    /// locks at once.
    tuner_locks: Vec<Option<f64>>,
    /// Global narrow search range (Hz) sent to the daemon via `tuner_range`
    /// REQ when the user presses Space in Live mode. Mirrored locally so
    /// the overlay can label the lock without round-tripping to the daemon.
    tuner_range_lock: Option<(f64, f64)>,
    /// Detector sensitivity preset cycled by `Shift+U`. Maps to
    /// `(trigger_delta_db, min_confidence)` pushed to the daemon via
    /// `tuner_config`.
    tuner_sensitivity: TunerSensitivity,
    /// Absolute-level gate in dBFS — fires only when the in-band peak is
    /// this loud or louder. `None` disables the gate. Stepped by `+`/`-`
    /// while tuner mode is active; falls through to the default dB-span
    /// control otherwise.
    tuner_min_level_dbfs: Option<f32>,
    /// Fractional-octave smoothing mode. `None` = raw spectrum; `Some(n)`
    /// smooths each bin with its neighbours inside ±f/2^(1/2n) so the
    /// linearly-spaced FFT output reads as a log-spaced curve. Typical
    /// audio values: 24, 12, 6, 3. Cycles via `O`.
    smoothing_frac: Option<u32>,
    /// Cached window index lists for `smoothing_frac`, keyed by
    /// (n, n_bins, last-freq-seen). Rebuilt when any of those change; saves
    /// a per-bin log range recomputation every frame.
    smoothing_cache: Option<smoothing::OctaveWindows>,
    /// Accumulates fractional scroll ticks while Alt+Scroll is cycling the
    /// waterfall palette, so trackpad pixel-deltas don't step the palette on
    /// every frame. One palette step per full unit of scroll.
    alt_scroll_accum: f32,
    output_dir: PathBuf,
    notification: Option<(String, Instant)>,
    modifiers: ModifiersState,
    last_render: Instant,
    cursor_pos: Option<PhysicalPosition<f64>>,
    drag: Option<DragState>,
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

#[derive(Clone)]
struct DragState {
    start: PhysicalPosition<f64>,
    targets: Vec<usize>,
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
            tuner_store: None,
            sweep_kind,
            sweep_last: SweepState::default(),
            sweep_selected_idx: None,
            monitor_spectrum_active: false,
            monitor_channels,
            analysis_mode: "fft".to_string(),
            cwt_sigma: 12.0,
            cwt_n_scales: 512,
            // Auto-scaled on every N change (arrow Up/Down) and at the
            // first frame (once sr is known). Seeded from the default N
            // assuming 48 kHz so the very first tick doesn't overshoot.
            monitor_interval_ms: auto_monitor_interval_ms(8192, 48_000),
            monitor_fft_n: 8192,
            selection_order: Vec::new(),
            active_meas_idx: 0,
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
            tuner_mode: TunerMode::Off,
            tuner_locks: Vec::new(),
            tuner_range_lock: None,
            tuner_sensitivity: TunerSensitivity::Mid,
            tuner_min_level_dbfs: Some(-60.0),
            // Default to 1/6 octave: gentle enough to preserve resonance
            // detail, heavy enough to calm the FFT grass. Users can cycle or
            // disable via `O`.
            smoothing_frac: Some(6),
            smoothing_cache: None,
            alt_scroll_accum: 0.0,
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

    /// Scroll-to-resize handler, only active in Grid layout when the cursor
    /// sits outside any cell (the empty band around / between cells). Seeds
    /// from the current auto layout so the first tick is a continuous step
    /// from wherever the user currently sees, then pins `grid_page` into the
    /// new page range so the visible content doesn't jump off-screen.
    fn adjust_grid_size(&mut self, scroll_y: f32) {
        let n = self.cell_views.len();
        if n == 0 {
            return;
        }
        let current = self.grid_cell_size.unwrap_or_else(|| {
            let cols = (n as f32).sqrt().ceil().max(1.0);
            1.0 / cols
        });
        // Scroll up (positive) = larger cells (fewer per page). Clamped so we
        // never produce zero-width cells or fewer than 1 col.
        let factor = 1.15_f32.powf(scroll_y);
        let new_size = (current * factor).clamp(1.0 / 8.0, 1.0);
        self.grid_cell_size = Some(new_size);
        let (cols, rows, _page_size, pages) =
            layout::grid_dims(n, self.grid_params());
        self.grid_page = self.grid_page.min(pages.saturating_sub(1));
        self.notify(&format!(
            "grid {}×{} · page {}/{}",
            cols,
            rows,
            self.grid_page + 1,
            pages,
        ));
    }

    /// Identify the cell the cursor is in. Returns `(channel, nx, ny, w_px, h_px)`
    /// where `(nx, ny)` are normalized cell-local coords (y up) and `channel` is
    /// the cell's primary channel. In Overlay mode every cell shares the same
    /// rect so this returns the first hit; call [`targets_for_channel`] to
    /// resolve the full set of cell_views to mutate.
    fn cell_at(&self, pos: PhysicalPosition<f64>) -> Option<(usize, f32, f32, f32, f32)> {
        let ctx = self.render_ctx.as_ref()?;
        let w = ctx.config.width as f32;
        let h = ctx.config.height as f32;
        let n_real = self.store.as_ref().map(|s| s.len()).unwrap_or(0);
        if n_real == 0 {
            return None;
        }
        // Include virtual transfer channels so hover / scroll / drag work on
        // their cells just like real ones.
        let n = n_real + self.virtual_render_pairs.len();
        let cells = layout::compute(
            self.config.layout,
            n,
            self.config.active_channel,
            &self.selected,
            &self.selection_order,
            self.active_meas_idx,
            self.grid_params(),
        );
        for c in &cells {
            let r = layout::to_pixel_rect(c, w, h);
            let x = pos.x as f32;
            let y = pos.y as f32;
            if x >= r.left() && x <= r.right() && y >= r.top() && y <= r.bottom() {
                let nx = (x - r.left()) / r.width().max(1.0);
                let ny = 1.0 - (y - r.top()) / r.height().max(1.0);
                return Some((c.channel, nx, ny, r.width(), r.height()));
            }
        }
        None
    }

    /// Which `cell_views` indices should a mouse/key interaction under the
    /// given hovered channel mutate. Grid → just that cell. Overlay → all
    /// cells (their rects are stacked). Single → whichever channel is active.
    fn targets_for_channel(&self, hovered: usize) -> Vec<usize> {
        let n = self.cell_views.len();
        if n == 0 {
            return Vec::new();
        }
        match self.config.layout {
            LayoutMode::Grid => vec![hovered.min(n - 1)],
            LayoutMode::Single => vec![self.config.active_channel.min(n - 1)],
            // Compare stacks the selected set in one rect, so zoom/pan should
            // move every selected channel together to keep the overlay coherent.
            LayoutMode::Compare => self
                .selected
                .iter()
                .enumerate()
                .filter_map(|(i, sel)| sel.then_some(i))
                .collect(),
            // Transfer cell carries the active meas channel slot; zoom/pan
            // acts on that meas' CellView (phase/coh sub-panels inherit the
            // frequency axis from it). Last-selected is REF, everything
            // before it is a meas — `active_meas_idx` picks the current one.
            LayoutMode::Transfer => {
                if let Some(meas) = self.transfer_active_meas() {
                    vec![meas.min(n - 1)]
                } else {
                    Vec::new()
                }
            }
            LayoutMode::Sweep => vec![0],
        }
    }

    fn apply_zoom(&mut self, scroll_y: f32) {
        // Alt+Scroll cycles the waterfall colormap palette (inferno → viridis
        // → magma → plasma → inferno). Alt is otherwise unused in scroll
        // handling so this is non-breaking; Shift keeps dB-gain zoom semantics.
        // Spectrum mode ignores the cycle — palette only affects the LUT.
        if self.modifiers.alt_key()
            && matches!(self.config.view_mode, ViewMode::Waterfall)
            && scroll_y != 0.0
        {
            self.alt_scroll_accum += scroll_y;
            let steps = self.alt_scroll_accum.trunc() as i32;
            if steps != 0 {
                self.alt_scroll_accum -= steps as f32;
                let new_idx = self.waterfall.as_mut().map(|wf| {
                    let n = crate::render::waterfall::N_PALETTES as i32;
                    let cur = wf.active_palette() as i32;
                    let next = ((cur + steps).rem_euclid(n)) as u32;
                    wf.set_palette(next);
                    next as usize
                });
                if let Some(idx) = new_idx {
                    let name = crate::render::waterfall::PALETTE_NAMES
                        .get(idx)
                        .copied()
                        .unwrap_or("?");
                    self.notify(&format!("palette: {name}"));
                    self.needs_redraw = true;
                }
            }
            return;
        }
        // Alt released (or not held) — drop any leftover fractional scroll so
        // the next Alt+Scroll session starts from zero instead of firing on
        // the first tick.
        if !self.modifiers.alt_key() && self.alt_scroll_accum != 0.0 {
            self.alt_scroll_accum = 0.0;
        }

        let pos = match self.cursor_pos {
            Some(p) => p,
            None => return,
        };
        let (hovered, nx, ny, _, _) = match self.cell_at(pos) {
            Some(v) => v,
            None => return,
        };
        let targets = self.targets_for_channel(hovered);
        let factor = 0.85_f32.powf(scroll_y);
        let shift = self.modifiers.shift_key();
        let ctrl = self.modifiers.control_key();
        let waterfall = matches!(self.config.view_mode, ViewMode::Waterfall);

        // Hard floor/ceiling on the visible freq window: the spectrum data
        // only covers ~20 Hz..Nyquist, so letting the user zoom out past the
        // data just shows empty space. Ceiling grows to match the largest
        // `freqs.last()` we've seen from the producer (96 kHz sessions etc.).
        let data_log_min = theme::DEFAULT_FREQ_MIN.log10();
        let data_log_max = self.data_freq_ceiling.max(theme::DEFAULT_FREQ_MAX).log10();
        let data_ceiling = 10_f32.powf(data_log_max);
        let data_span = (data_log_max - data_log_min).max(0.001);
        // In waterfall mode: plain scroll = freq, Ctrl+scroll = time (rows
        // shown), Shift+scroll = gain (colormap dB). Spectrum mode keeps the
        // "plain scroll zooms both axes at once" feel.
        let (zoom_freq, zoom_db, zoom_time) = if waterfall {
            (!shift && !ctrl, shift, ctrl)
        } else {
            (!shift, !ctrl, false)
        };

        for idx in targets {
            let view = match self.cell_views.get_mut(idx) {
                Some(v) => v,
                None => continue,
            };
            if zoom_freq {
                let log_min = view.freq_min.max(1.0).log10();
                let log_max = view.freq_max.max(log_min.exp().max(10.0)).log10();
                let anchor = log_min + nx * (log_max - log_min);
                let new_span = ((log_max - log_min) * factor).clamp(0.15, data_span);
                let mut new_min = anchor - nx * new_span;
                let mut new_max = new_min + new_span;
                if new_min < data_log_min {
                    new_min = data_log_min;
                    new_max = (new_min + new_span).min(data_log_max);
                }
                if new_max > data_log_max {
                    new_max = data_log_max;
                    new_min = (new_max - new_span).max(data_log_min);
                }
                view.freq_min = 10.0_f32.powf(new_min).max(theme::DEFAULT_FREQ_MIN);
                view.freq_max = 10.0_f32.powf(new_max).min(data_ceiling);
            }
            if zoom_db {
                let db_min = view.db_min;
                let db_max = view.db_max;
                let anchor = db_min + ny * (db_max - db_min);
                let new_span = ((db_max - db_min) * factor).clamp(10.0, 240.0);
                let new_min = (anchor - ny * new_span).max(-240.0);
                let new_max = (new_min + new_span).min(20.0);
                view.db_min = new_min;
                view.db_max = new_max;
            }
            if zoom_time {
                // Fractional zoom: the f32 is authoritative so consecutive
                // scroll ticks don't lose precision to integer rounding,
                // giving a smoothly growing/shrinking time window instead of
                // stepped jumps. The u32 copy tracks round(f32) for the
                // shader and label consumers.
                let current = view.rows_visible_f.max(1.0);
                let max_rows = crate::render::waterfall::ROWS_PER_CHANNEL as f32;
                let new_rows = (current * factor).clamp(2.0, max_rows);
                view.rows_visible_f = new_rows;
                view.rows_visible = new_rows.round().clamp(2.0, max_rows) as u32;
            }
        }
    }

    fn begin_drag(&mut self) {
        let pos = match self.cursor_pos {
            Some(p) => p,
            None => return,
        };
        if matches!(self.config.layout, LayoutMode::Sweep) {
            self.handle_sweep_click(pos);
            return;
        }
        let (hovered, _nx, _ny, cell_w, cell_h) = match self.cell_at(pos) {
            Some(v) => v,
            None => return,
        };
        let targets = self.targets_for_channel(hovered);
        // Capture the seed view from the first target so every cell in the
        // set pans by the same amount regardless of where they started.
        let seed = match targets.first().and_then(|&i| self.cell_views.get(i)) {
            Some(v) => *v,
            None => return,
        };
        let log_min = seed.freq_min.max(1.0).log10();
        let log_max = seed.freq_max.max(10.0).log10();
        self.drag = Some(DragState {
            start: pos,
            targets,
            start_log_min: log_min,
            start_log_max: log_max,
            start_db_min: seed.db_min,
            start_db_max: seed.db_max,
            cell_w_px: cell_w,
            cell_h_px: cell_h,
        });
    }

    fn handle_sweep_click(&mut self, pos: PhysicalPosition<f64>) {
        let kind = match self.sweep_kind {
            Some(k) => k,
            None => return,
        };
        let cells = layout::compute(
            self.config.layout,
            1,
            0,
            &self.selected,
            &self.selection_order,
            self.active_meas_idx,
            self.grid_params(),
        );
        let Some(cell) = cells.first() else { return };
        let ctx = self.render_ctx.as_ref().unwrap();
        let w = ctx.config.width as f32;
        let h = ctx.config.height as f32;
        let rect = layout::to_pixel_rect(cell, w, h);
        let cursor = egui::pos2(pos.x as f32, pos.y as f32);
        if let Some(idx) = crate::render::sweep::nearest_point(rect, kind, &self.sweep_last, cursor) {
            self.sweep_selected_idx = Some(idx);
            self.needs_redraw = true;
        }
    }

    fn update_drag(&mut self, pos: PhysicalPosition<f64>) {
        let drag = match self.drag.clone() {
            Some(d) => d,
            None => return,
        };
        let waterfall = matches!(self.config.view_mode, ViewMode::Waterfall);
        let data_log_min = theme::DEFAULT_FREQ_MIN.log10();
        let data_log_max = self.data_freq_ceiling.max(theme::DEFAULT_FREQ_MAX).log10();
        let data_ceiling = 10_f32.powf(data_log_max);
        let dx_px = (pos.x - drag.start.x) as f32;
        let dy_px = (pos.y - drag.start.y) as f32;
        let log_span = drag.start_log_max - drag.start_log_min;
        let db_span = drag.start_db_max - drag.start_db_min;
        let d_log = -(dx_px / drag.cell_w_px.max(1.0)) * log_span;
        let d_db = -(dy_px / drag.cell_h_px.max(1.0)) * db_span;
        let new_log_min = (drag.start_log_min + d_log)
            .clamp(data_log_min, (data_log_max - log_span).max(data_log_min));
        let new_log_max = (new_log_min + log_span).min(data_log_max);
        let new_db_min = (drag.start_db_min + d_db).max(-240.0);
        let new_db_max = (new_db_min + db_span).min(20.0);
        for &idx in &drag.targets {
            if let Some(view) = self.cell_views.get_mut(idx) {
                view.freq_min = 10.0_f32.powf(new_log_min).max(theme::DEFAULT_FREQ_MIN);
                view.freq_max = 10.0_f32.powf(new_log_max).min(data_ceiling);
                if !waterfall {
                    view.db_min = new_db_min;
                    view.db_max = new_db_max;
                }
            }
        }
    }

    fn reset_hovered_view(&mut self) {
        let pos = match self.cursor_pos {
            Some(p) => p,
            None => {
                self.reset_all_views();
                return;
            }
        };
        let hovered = match self.cell_at(pos) {
            Some((ch, _, _, _, _)) => ch,
            None => {
                self.reset_all_views();
                return;
            }
        };
        for idx in self.targets_for_channel(hovered) {
            if let Some(view) = self.cell_views.get_mut(idx) {
                *view = CellView::default();
            }
        }
        self.notify("view reset");
    }

    fn reset_all_views(&mut self) {
        for view in &mut self.cell_views {
            *view = CellView::default();
        }
        for init in &mut self.waterfall_inited {
            *init = false;
        }
        self.grid_cell_size = None;
        self.grid_page = 0;
        self.reset_peak_holds();
        self.notify("all views reset");
    }

    /// Clear every channel's peak-hold buffer. Leaves `peak_hold_enabled`
    /// alone — reset triggers (Enter, Ctrl+R, FFT-N / analysis-mode change)
    /// just drop the stale accumulator; the next fresh frame re-seeds it.
    fn reset_peak_holds(&mut self) {
        for slot in &mut self.peak_holds {
            *slot = None;
        }
        for slot in &mut self.peak_last_update {
            *slot = None;
        }
        for slot in &mut self.peak_last_tick {
            *slot = None;
        }
        for slot in &mut self.min_holds {
            *slot = None;
        }
        for slot in &mut self.min_last_update {
            *slot = None;
        }
        for slot in &mut self.min_last_tick {
            *slot = None;
        }
    }

    /// Which channel does Space act on. Single mode → the active channel (the
    /// one visible). Any other layout → the hovered cell, or the active
    /// channel as a fallback when the cursor sits outside the plot area.
    /// Clamps to the real channel count — Space over a virtual transfer
    /// cell is a no-op, because virtual channels can't themselves be used
    /// as MEAS/REF for a nested transfer.
    fn selection_target(&self) -> Option<usize> {
        let n_real = self.store.as_ref().map(|s| s.len()).unwrap_or(0);
        if n_real == 0 {
            return None;
        }
        let idx = match self.config.layout {
            LayoutMode::Single => self.config.active_channel,
            _ => self
                .cursor_pos
                .and_then(|p| self.cell_at(p))
                .map(|(ch, _, _, _, _)| ch)
                .unwrap_or(self.config.active_channel),
        };
        if idx >= n_real {
            return None;
        }
        Some(idx)
    }

    fn toggle_selection(&mut self) {
        let target = match self.selection_target() {
            Some(t) => t,
            None => return,
        };
        let in_transfer = matches!(self.config.layout, LayoutMode::Transfer);
        let now_selected = {
            let slot = &mut self.selected[target];
            *slot = !*slot;
            *slot
        };
        if now_selected {
            if !self.selection_order.contains(&target) {
                self.selection_order.push(target);
            }
        } else {
            self.selection_order.retain(|&i| i != target);
        }
        let count = self.selected.iter().filter(|s| **s).count();
        self.notify(&format!(
            "CH{} {} ({} selected)",
            target,
            if now_selected { "selected" } else { "unselected" },
            count,
        ));
        if in_transfer {
            // Selection change while live: clamp the active meas into the
            // (possibly shrunk) meas list, then hot-swap the running
            // transfer_stream to match the new pair.
            let meas_count = self.selection_order.len().saturating_sub(1);
            if meas_count == 0 {
                self.active_meas_idx = 0;
            } else if self.active_meas_idx >= meas_count {
                self.active_meas_idx = meas_count - 1;
            }
            self.restart_transfer_stream();
        }
    }

    /// Active meas channel under the current Transfer convention. `None`
    /// means the selection is too small (< 2) or the resolved index is
    /// out-of-range; the overlay hint shows up in that case.
    fn transfer_active_meas(&self) -> Option<usize> {
        let n = self.selection_order.len();
        if n < 2 {
            return None;
        }
        let meas_count = n - 1;
        let idx = self.active_meas_idx.min(meas_count - 1);
        Some(self.selection_order[idx])
    }

    fn transfer_ref_channel(&self) -> Option<usize> {
        if self.selection_order.len() < 2 {
            return None;
        }
        self.selection_order.last().copied()
    }

    /// Stop any currently running `transfer_stream` worker and restart it
    /// with the current union of virtual-channel pairs plus (if in L-transfer
    /// layout) the active meas/ref pair. No-op when the union is empty —
    /// stopping the worker is enough.
    fn restart_transfer_stream(&mut self) {
        self.send_transfer_stream_stop();
        let pairs = self.collect_transfer_pairs();
        if pairs.is_empty() {
            return;
        }
        // Args are unused in the new pairs-based implementation; kept for
        // call-site compatibility in case we ever revert.
        self.send_transfer_stream_start(0, 0);
    }

    /// Called after `config.layout` has been advanced by the `l` key. Starts
    /// the transfer_stream worker when entering Transfer (if the pair is
    /// ready) and stops it when leaving — *unless* virtual channels are
    /// registered, in which case the worker stays live to keep feeding them
    /// across layout changes.
    fn on_layout_changed(&mut self, prev: LayoutMode, next: LayoutMode) {
        let entering = !matches!(prev, LayoutMode::Transfer)
            && matches!(next, LayoutMode::Transfer);
        let leaving = matches!(prev, LayoutMode::Transfer)
            && !matches!(next, LayoutMode::Transfer);
        if entering {
            // Start from the first meas on fresh entry so the user doesn't
            // inherit stale Tab state from a previous Transfer session.
            self.active_meas_idx = 0;
            if self.transfer_active_meas().is_some() {
                self.restart_transfer_stream();
            } else if self.virtual_channels.is_empty() {
                self.notify("transfer: pick ≥ 2 channels (last = REF)");
            } else {
                // Worker already live serving virtual channels — nothing to
                // restart, the layout just has no legacy pair to display.
            }
        } else if leaving {
            // Virtual channels keep the worker alive across layout changes;
            // only fully stop if there's nothing left to stream.
            if self.virtual_channels.is_empty() {
                self.send_transfer_stream_stop();
            } else {
                // Drop the L-layout pair from the worker's set.
                self.restart_transfer_stream();
            }
            // Resume spectrum publishing that was paused when we entered
            // Transfer. No-op if it's already running (e.g. the user never
            // had a valid pair so we never actually stopped it).
            self.send_monitor_spectrum_start();
        }
    }

    fn start_data_source(&mut self) {
        let init = match self.init.take() {
            Some(i) => i,
            None => return,
        };
        self.cell_views = vec![CellView::default(); init.store.len()];
        self.selected = vec![false; init.store.len()];
        self.waterfall_inited = vec![false; init.store.len()];
        self.store = Some(init.store);
        let transfer_store = init.transfer_store.clone();
        self.transfer_store = Some(transfer_store.clone());
        self.virtual_channels = init.virtual_channels.clone();
        let virtual_channels = init.virtual_channels.clone();
        let sweep_store = init.sweep_store.clone();
        self.sweep_store = Some(sweep_store.clone());
        let tuner_store = init.tuner_store.clone();
        self.tuner_store = Some(tuner_store.clone());
        match init.source_kind {
            SourceKind::Synthetic => {
                let (n, bins, rate) = init.synthetic_params.unwrap_or((1, 1000, 10.0));
                let src = crate::data::synthetic::SyntheticSource {
                    n_channels: n,
                    n_bins: bins,
                    update_hz: rate,
                    transfer: transfer_store,
                    virtual_channels,
                };
                let handle = src.spawn(init.inputs, self.wake.clone());
                self.source = Some(DataSource::Synthetic(handle));
            }
            SourceKind::Daemon => {
                let handle = crate::data::receiver::spawn(
                    init.endpoint,
                    init.inputs,
                    transfer_store,
                    virtual_channels,
                    sweep_store,
                    tuner_store,
                    self.wake.clone(),
                );
                self.source = Some(DataSource::Receiver(handle));
                if !matches!(self.config.layout, LayoutMode::Sweep) {
                    self.send_monitor_spectrum_start();
                }
            }
        }
    }

    /// Lazy-connect the CTRL REQ socket on first use. Called from the
    /// transfer-stream start/stop path. If the daemon isn't up the socket
    /// connect will still succeed (ZMQ is async) but `send` will time out.
    fn ensure_ctrl(&mut self) -> Option<&CtrlClient> {
        if self.ctrl.is_none() {
            match CtrlClient::connect(&self.ctrl_endpoint) {
                Ok(c) => self.ctrl = Some(c),
                Err(e) => {
                    log::warn!("ctrl client connect failed: {e}");
                    return None;
                }
            }
        }
        self.ctrl.as_ref()
    }

    /// Tell the daemon to switch `monitor_spectrum`'s analysis path between
    /// FFT and Morlet CWT. No-op on the synthetic backend (no daemon). On
    /// success the local `analysis_mode` is updated so the W cycle can pick
    /// the next state; on failure we leave it unchanged and notify.
    fn send_set_analysis_mode(&mut self, mode: &str) -> bool {
        if matches!(self.source.as_ref(), Some(DataSource::Synthetic(_))) {
            self.analysis_mode = mode.to_string();
            return true;
        }
        let sigma = self.cwt_sigma;
        let n_scales = self.cwt_n_scales;
        let Some(ctrl) = self.ensure_ctrl() else {
            self.notify("analysis_mode: no ctrl");
            return false;
        };
        let cmd = serde_json::json!({
            "cmd":      "set_analysis_mode",
            "mode":     mode,
            "sigma":    sigma,
            "n_scales": n_scales,
        });
        match ctrl.send(&cmd) {
            Ok(reply) => {
                if reply.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                    self.analysis_mode = mode.to_string();
                    true
                } else {
                    let err = reply.get("error").and_then(|v| v.as_str()).unwrap_or("?");
                    self.notify(&format!("analysis_mode: {err}"));
                    false
                }
            }
            Err(e) => {
                log::warn!("set_analysis_mode failed: {e}");
                self.notify("analysis_mode: ctrl error");
                false
            }
        }
    }

    /// Set or clear the daemon-side tuner search-range lock for a channel.
    /// Synthetic backend has no daemon; silently no-op.
    fn send_tuner_range(&mut self, channel: u32, range: Option<(f64, f64)>) {
        if matches!(self.source.as_ref(), Some(DataSource::Synthetic(_))) {
            return;
        }
        let Some(ctrl) = self.ensure_ctrl() else { return };
        let cmd = if let Some((lo, hi)) = range {
            serde_json::json!({
                "cmd":     "tuner_range",
                "channel": channel,
                "lo_hz":   lo,
                "hi_hz":   hi,
            })
        } else {
            serde_json::json!({
                "cmd":     "tuner_range",
                "channel": channel,
                "clear":   true,
            })
        };
        if let Err(e) = ctrl.send(&cmd) {
            log::warn!("tuner_range failed: {e}");
        }
    }

    /// Step `tuner_min_level_dbfs` by the given dB delta, clamped to the
    /// configured floor/ceiling. Crossing the floor clears the gate
    /// (None); starting from None assumes the floor.
    fn step_tuner_min_level(&mut self, delta_db: f32) {
        let cur = self.tuner_min_level_dbfs.unwrap_or(TUNER_MIN_LEVEL_FLOOR_DBFS);
        let next = cur + delta_db;
        if next < TUNER_MIN_LEVEL_FLOOR_DBFS {
            self.tuner_min_level_dbfs = None;
            self.notify("tuner min level: off");
        } else {
            let clamped = next.min(TUNER_MIN_LEVEL_CEIL_DBFS);
            self.tuner_min_level_dbfs = Some(clamped);
            self.notify(&format!("tuner min level: {:.0} dBFS", clamped));
        }
        self.send_tuner_config();
        self.needs_redraw = true;
    }

    /// Push the current sensitivity preset + min-level gate to the daemon
    /// via the `tuner_config` REQ. Synthetic backend has no daemon; no-op.
    fn send_tuner_config(&mut self) {
        if matches!(self.source.as_ref(), Some(DataSource::Synthetic(_))) {
            return;
        }
        let (trigger_delta_db, min_confidence) = self.tuner_sensitivity.params();
        let min_level = match self.tuner_min_level_dbfs {
            Some(v) => serde_json::Value::from(v as f64),
            None => serde_json::Value::Null,
        };
        let Some(ctrl) = self.ensure_ctrl() else { return };
        let cmd = serde_json::json!({
            "cmd":              "tuner_config",
            "trigger_delta_db": trigger_delta_db,
            "min_confidence":   min_confidence,
            "min_level_dbfs":   min_level,
        });
        if let Err(e) = ctrl.send(&cmd) {
            log::warn!("tuner_config failed: {e}");
        }
    }

    fn send_cwt_params(&mut self) {
        if self.analysis_mode != "cwt" {
            return;
        }
        self.send_set_analysis_mode("cwt");
    }

    /// Sample rate of the most recent real-channel frame, or 48 kHz if no
    /// frame has arrived yet. Used by `auto_monitor_interval_ms` to pick a
    /// tick that matches the actual capture rate rather than assuming one.
    fn current_sr(&self) -> u32 {
        self.last_frames
            .iter()
            .flatten()
            .map(|f| f.meta.sr)
            .find(|&sr| sr > 0)
            .unwrap_or(48_000)
    }

    /// Push the current `monitor_interval_ms` + `monitor_fft_n` to the daemon
    /// via `set_monitor_params`. Silent no-op on the synthetic backend.
    fn send_monitor_params(&mut self) {
        if matches!(self.source.as_ref(), Some(DataSource::Synthetic(_))) {
            return;
        }
        if !self.monitor_spectrum_active {
            return;
        }
        let interval = self.monitor_interval_ms as f64 / 1000.0;
        let fft_n = self.monitor_fft_n;
        let Some(ctrl) = self.ensure_ctrl() else { return };
        let cmd = serde_json::json!({
            "cmd":      "set_monitor_params",
            "interval": interval,
            "fft_n":    fft_n,
        });
        if let Err(e) = ctrl.send(&cmd) {
            log::warn!("set_monitor_params failed: {e}");
        }
    }

    /// Union of every pair the worker needs to service: every registered
    /// virtual channel plus, if the user is currently in the legacy
    /// L-transfer layout, the (active_meas, ref) pair that view points at —
    /// dedup'd so the worker doesn't compute the same H1 twice.
    fn collect_transfer_pairs(&self) -> Vec<TransferPair> {
        let mut pairs = self.virtual_channels.pairs();
        if matches!(self.config.layout, LayoutMode::Transfer) {
            if let (Some(meas), Some(refc)) =
                (self.transfer_active_meas(), self.transfer_ref_channel())
            {
                let layout_pair = TransferPair { meas: meas as u32, ref_ch: refc as u32 };
                if !pairs.iter().any(|p| *p == layout_pair) {
                    pairs.push(layout_pair);
                }
            }
        }
        pairs
    }

    fn send_transfer_stream_start(&mut self, _meas_ch: usize, _ref_ch: usize) {
        let pairs = self.collect_transfer_pairs();
        if pairs.is_empty() {
            return;
        }
        // Synthetic mode: no daemon involved — the synthetic worker reads
        // `virtual_channels.pairs()` directly on each tick, so we just flip
        // the active flag. The renderer and overlay don't care where the
        // frame came from.
        if matches!(self.source.as_ref(), Some(DataSource::Synthetic(_))) {
            self.transfer_stream_active = true;
            self.notify(&format!("transfer_stream: {} pair(s) (synthetic)", pairs.len()));
            return;
        }
        // `transfer_stream` is in the `Transfer` group — coexists with the
        // running `monitor_spectrum` (`Input`) because each worker owns its
        // own JACK client, so no need to pause monitor here.
        let Some(ctrl) = self.ensure_ctrl() else { return };
        // Passive mode: don't ask the daemon to drive the output. The user
        // wires their own stimulus (pink, sweep, speech, music) into the
        // meas/ref inputs externally and we just compute H1 against it.
        let pairs_json: Vec<[u32; 2]> =
            pairs.iter().map(|p| [p.meas, p.ref_ch]).collect();
        let cmd = serde_json::json!({
            "cmd":   "transfer_stream",
            "pairs": pairs_json,
        });
        log::info!("transfer_stream: sending start pairs={pairs_json:?}");
        match ctrl.send(&cmd) {
            Ok(reply) => {
                log::info!("transfer_stream reply: {reply}");
                if reply.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                    self.transfer_stream_active = true;
                    self.notify(&format!("transfer_stream: {} pair(s) live", pairs.len()));
                } else {
                    let err = reply.get("error").and_then(|v| v.as_str()).unwrap_or("?");
                    self.notify(&format!("transfer_stream: {err}"));
                }
            }
            Err(e) => {
                log::warn!("transfer_stream start failed: {e}");
                self.notify("transfer_stream: ctrl error");
            }
        }
    }

    fn send_transfer_stream_stop(&mut self) {
        if !self.transfer_stream_active {
            return;
        }
        if !matches!(self.source.as_ref(), Some(DataSource::Synthetic(_))) {
            if let Some(ctrl) = self.ensure_ctrl() {
                let cmd = serde_json::json!({ "cmd": "stop", "name": "transfer_stream" });
                let _ = ctrl.send(&cmd);
            }
        }
        self.transfer_stream_active = false;
        if let Some(ts) = self.transfer_store.as_ref() {
            ts.clear();
        }
        self.transfer_last = None;
    }

    /// Ask the daemon to start publishing spectrum frames. `ac-ui` is a
    /// passive SUB otherwise — without this call every view stays blank.
    /// Requests one slot per preallocated channel so the grid / overlay
    /// layouts can display every input the daemon exposes.
    fn send_monitor_spectrum_start(&mut self) {
        if self.monitor_spectrum_active {
            return;
        }
        if matches!(self.source, Some(DataSource::Synthetic(_))) {
            return;
        }
        let n = self.store.as_ref().map(|s| s.len()).unwrap_or(0);
        if n == 0 {
            return;
        }
        let channels: Vec<u32> = self.monitor_channels.clone()
            .unwrap_or_else(|| (0..n as u32).collect());
        let interval = self.monitor_interval_ms as f64 / 1000.0;
        let fft_n = self.monitor_fft_n;
        let Some(ctrl) = self.ensure_ctrl() else { return };
        let cmd = serde_json::json!({
            "cmd":      "monitor_spectrum",
            "interval": interval,
            "fft_n":    fft_n,
            "channels": channels,
        });
        log::info!("monitor_spectrum: sending start channels={channels:?}");
        match ctrl.send(&cmd) {
            Ok(reply) => {
                log::info!("monitor_spectrum reply: {reply}");
                if reply.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                    self.monitor_spectrum_active = true;
                } else {
                    let err = reply.get("error").and_then(|v| v.as_str()).unwrap_or("?");
                    self.notify(&format!("monitor_spectrum: {err}"));
                }
            }
            Err(e) => {
                log::warn!("monitor_spectrum start failed: {e}");
                self.notify("monitor_spectrum: ctrl error");
            }
        }
    }

    fn send_monitor_spectrum_stop(&mut self) {
        if !self.monitor_spectrum_active {
            return;
        }
        if let Some(ctrl) = self.ensure_ctrl() {
            let cmd = serde_json::json!({ "cmd": "stop", "name": "monitor_spectrum" });
            let _ = ctrl.send(&cmd);
        }
        self.monitor_spectrum_active = false;
    }

    fn init_graphics(&mut self, window: Arc<Window>) {
        let ctx = pollster::block_on(RenderContext::new(window.clone())).expect("wgpu init");
        let format = ctx.surface_format();
        let spectrum = SpectrumRenderer::new(&ctx.device, format);
        let waterfall = WaterfallRenderer::new(&ctx.device, &ctx.queue, format);
        let egui_renderer = egui_wgpu::Renderer::new(&ctx.device, format, None, 1, false);
        self.egui_ctx.set_visuals(dark_visuals());
        let viewport_id = self.egui_ctx.viewport_id();
        let egui_state =
            egui_winit::State::new(self.egui_ctx.clone(), viewport_id, &window, None, None, None);
        self.render_ctx = Some(ctx);
        self.spectrum = Some(spectrum);
        self.waterfall = Some(waterfall);
        self.egui_renderer = Some(egui_renderer);
        self.egui_state = Some(egui_state);
    }

    /// Pick the next layout in the cycle given current selection state.
    /// Compare and Transfer are only visited when the user has selected
    /// enough channels (Compare: any; Transfer: >= 2).
    fn next_layout(&self, from: LayoutMode) -> LayoutMode {
        let any_selected = self.selected.iter().any(|s| *s);
        // Transfer is reachable when either a fresh L-layout meas/ref pair is
        // available (≥ 2 selected) or virtual channels are already registered
        // — the layout still has content to render from the virtual pairs
        // even if the user has since cleared their selection.
        let transfer_ready =
            self.selection_order.len() >= 2 || !self.virtual_channels.is_empty();
        let raw_cycle = [
            LayoutMode::Grid,
            LayoutMode::Single,
            LayoutMode::Compare,
            LayoutMode::Transfer,
        ];
        let start = raw_cycle
            .iter()
            .position(|m| *m == from)
            .map(|i| (i + 1) % raw_cycle.len())
            .unwrap_or(0);
        for offset in 0..raw_cycle.len() {
            let candidate = raw_cycle[(start + offset) % raw_cycle.len()];
            let allowed = match candidate {
                LayoutMode::Compare => any_selected,
                LayoutMode::Transfer => transfer_ready,
                _ => true,
            };
            if allowed {
                return candidate;
            }
        }
        LayoutMode::Grid
    }

    fn handle_key(&mut self, elwt: &ActiveEventLoop, code: KeyCode) {
        match code {
            KeyCode::Escape | KeyCode::KeyQ => elwt.exit(),
            KeyCode::Enter => {
                self.config.frozen = !self.config.frozen;
                self.reset_peak_holds();
                self.notify(if self.config.frozen { "FROZEN" } else { "live" });
            }
            KeyCode::KeyP => {
                self.peak_hold_enabled = !self.peak_hold_enabled;
                self.reset_peak_holds();
                self.notify(if self.peak_hold_enabled {
                    "peak hold: on"
                } else {
                    "peak hold: off"
                });
            }
            KeyCode::KeyM => {
                self.min_hold_enabled = !self.min_hold_enabled;
                self.reset_peak_holds();
                self.notify(if self.min_hold_enabled {
                    "min hold: on"
                } else {
                    "min hold: off"
                });
            }
            KeyCode::KeyU if self.modifiers.shift_key() => {
                // Shift+U cycles detector sensitivity (Low → Mid → High) without
                // touching mode. The preset pair (trigger_delta_db,
                // min_confidence) is pushed to the daemon immediately so the
                // next tick applies it. Works from Off too — pressing now so
                // the next Live-mode entry starts at the user's preferred
                // sensitivity.
                self.tuner_sensitivity = self.tuner_sensitivity.next();
                let (dd, mc) = self.tuner_sensitivity.params();
                self.send_tuner_config();
                self.notify(&format!(
                    "tuner sensitivity: {} (Δ {:.0} dB, conf ≥ {:.2})",
                    self.tuner_sensitivity.label(), dd, mc
                ));
                self.needs_redraw = true;
            }
            KeyCode::KeyU => {
                // Tuner tri-state cycle. The identifier reads from the
                // peak-hold buffer (a live drum hit decays in <100 ms — no
                // hold, no signal to analyse), so the first press also turns
                // peak hold on if it wasn't already. Locking requires a
                // currently-valid candidate on the active channel; if there
                // isn't one the press just turns the tuner off again with a
                // different toast so the user knows the lock failed.
                self.tuner_mode = match self.tuner_mode {
                    TunerMode::Off => {
                        if !self.peak_hold_enabled {
                            self.peak_hold_enabled = true;
                            self.reset_peak_holds();
                            self.notify("tuner: on (peak hold auto-enabled)");
                        } else {
                            self.notify("tuner: on");
                        }
                        // First enable: push current preset + min-level to the
                        // daemon so its detector matches what the UI advertises
                        // before any `+`/`-` or `Shift+U` tweaks.
                        self.send_tuner_config();
                        TunerMode::Live
                    }
                    TunerMode::Live => {
                        let ch = self.config.active_channel;
                        let cand = self
                            .tuner_store
                            .as_ref()
                            .and_then(|s| s.snapshot(ch + 1).0.into_iter().nth(ch).flatten())
                            .filter(|c| c.confidence >= TUNER_MIN_CONFIDENCE);
                        if let Some(cand) = cand {
                            if self.tuner_locks.len() <= ch {
                                self.tuner_locks.resize(ch + 1, None);
                            }
                            self.tuner_locks[ch] = Some(cand.freq_hz);
                            self.notify(&format!("tuner: locked {:.1} Hz", cand.freq_hz));
                            TunerMode::Locked
                        } else {
                            self.notify("tuner: off (no confident fundamental)");
                            for slot in self.tuner_locks.iter_mut() {
                                *slot = None;
                            }
                            TunerMode::Off
                        }
                    }
                    TunerMode::Locked => {
                        self.notify("tuner: off");
                        for slot in self.tuner_locks.iter_mut() {
                            *slot = None;
                        }
                        TunerMode::Off
                    }
                };
                self.needs_redraw = true;
            }
            KeyCode::KeyO => {
                self.smoothing_frac = smoothing::next(self.smoothing_frac);
                // Rebuilds on next frame — drop the cache so the new window
                // factor takes effect immediately even if n_bins/sr haven't
                // changed.
                self.smoothing_cache = None;
                // Stale peak/min buffers were taken over the old smoothing;
                // clear them so the user immediately sees traces matching
                // the new resolution.
                self.reset_peak_holds();
                self.notify(&format!(
                    "smoothing: {}",
                    smoothing::label(self.smoothing_frac),
                ));
            }
            KeyCode::Space => {
                // In Live tuner mode Space narrows the search range on the
                // active channel so repeated hits can't alias to a sub-
                // harmonic outside the area of interest. Range lock lives
                // on the daemon (per-channel); the UI sends `tuner_range`
                // REQ and mirrors the value locally for the overlay label.
                if self.tuner_mode == TunerMode::Live {
                    let ch = self.config.active_channel as u32;
                    if self.tuner_range_lock.is_some() {
                        self.tuner_range_lock = None;
                        self.send_tuner_range(ch, None);
                        self.notify("tuner: range unlocked");
                    } else {
                        let ch_usize = self.config.active_channel;
                        let f0 = self
                            .tuner_store
                            .as_ref()
                            .and_then(|s| s.snapshot(ch_usize + 1).0.into_iter().nth(ch_usize).flatten())
                            .filter(|c| c.confidence >= TUNER_MIN_CONFIDENCE)
                            .map(|c| c.freq_hz);
                        if let Some(f0) = f0 {
                            let lo = (f0 * (1.0 - TUNER_RANGE_LOCK_FRAC))
                                .max(TUNER_SEARCH_MIN_HZ);
                            let hi = (f0 * (1.0 + TUNER_RANGE_LOCK_FRAC))
                                .min(TUNER_SEARCH_MAX_HZ);
                            self.tuner_range_lock = Some((lo, hi));
                            self.send_tuner_range(ch, Some((lo, hi)));
                            self.notify(&format!(
                                "tuner: range locked {:.0}-{:.0} Hz",
                                lo, hi
                            ));
                        } else {
                            self.notify("tuner: no confident f0 to lock range on");
                        }
                    }
                    self.needs_redraw = true;
                } else {
                    self.toggle_selection();
                }
            }
            KeyCode::KeyH => {
                self.show_help = !self.show_help;
            }
            KeyCode::KeyS => {
                self.pending_screenshot = true;
            }
            KeyCode::KeyL => {
                let prev = self.config.layout;
                self.config.layout = self.next_layout(prev);
                self.on_layout_changed(prev, self.config.layout);
                self.notify(match self.config.layout {
                    LayoutMode::Grid => "layout: grid",
                    LayoutMode::Single => "layout: single",
                    LayoutMode::Compare => "layout: compare",
                    LayoutMode::Transfer => "layout: transfer",
                    LayoutMode::Sweep => "layout: sweep",
                });
            }
            KeyCode::KeyT => {
                if self.selection_order.len() < 2 {
                    self.notify("T: select ≥ 2 channels first (last = REF)");
                    return;
                }
                let meas = self.selection_order[0] as u32;
                let ref_ch = *self.selection_order.last().unwrap() as u32;
                let pair = TransferPair { meas, ref_ch };
                if self.virtual_channels.remove(pair) {
                    self.notify(&format!(
                        "T: removed transfer (CH{meas}←CH{ref_ch})"
                    ));
                } else {
                    self.virtual_channels.add(pair);
                    let idx = self.virtual_channels.len().saturating_sub(1);
                    self.notify(&format!(
                        "T: added transfer{idx} (CH{meas}←CH{ref_ch})"
                    ));
                }
                self.restart_transfer_stream();
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
                if self.tuner_mode != TunerMode::Off {
                    self.step_tuner_min_level(TUNER_MIN_LEVEL_STEP_DB);
                } else {
                    self.adjust_hovered_db_span(-20.0);
                }
            }
            KeyCode::Minus | KeyCode::NumpadSubtract => {
                if self.tuner_mode != TunerMode::Off {
                    self.step_tuner_min_level(-TUNER_MIN_LEVEL_STEP_DB);
                } else {
                    self.adjust_hovered_db_span(20.0);
                }
            }
            KeyCode::KeyD => {
                self.show_timing = !self.show_timing;
                self.notify(if self.show_timing { "timing on" } else { "timing off" });
            }
            KeyCode::KeyW => {
                // W cycles three states so the waterfall view can toggle
                // between the linear FFT and Morlet CWT analysis paths
                // without needing a second hotkey:
                //   Spectrum(fft) → Waterfall(fft) → Waterfall(cwt) → Spectrum(fft)
                let (next_view, next_mode, label): (ViewMode, &str, &str) =
                    match (self.config.view_mode, self.analysis_mode.as_str()) {
                        (ViewMode::Spectrum, _) => (ViewMode::Waterfall, "fft", "view: waterfall (fft)"),
                        (ViewMode::Waterfall, "fft") => (ViewMode::Waterfall, "cwt", "view: waterfall (cwt)"),
                        (ViewMode::Waterfall, _) => (ViewMode::Spectrum, "fft", "view: spectrum"),
                    };
                if self.analysis_mode != next_mode && !self.send_set_analysis_mode(next_mode) {
                    // Mode change refused — don't advance the view so the
                    // key keeps meaning "next state" on the next press.
                    return;
                }
                self.config.view_mode = next_view;
                // Re-arm waterfall auto-init on every switch into waterfall
                // (or between FFT ↔ CWT where the dB distribution shifts) so
                // a fresh dB window gets picked from the current signal.
                if matches!(self.config.view_mode, ViewMode::Waterfall) {
                    for init in &mut self.waterfall_inited {
                        *init = false;
                    }
                }
                // FFT ↔ CWT changes the bin grid; a stale peak buffer would
                // mis-align with the new frames. Spectrum ↔ waterfall keeps
                // the grid but a peak marker is meaningless in waterfall, so
                // resetting in all W cycles keeps the state simple.
                self.reset_peak_holds();
                self.notify(label);
            }
            KeyCode::ArrowUp if self.modifiers.shift_key() && self.analysis_mode == "cwt" => {
                self.cwt_sigma = (self.cwt_sigma + 1.0).min(24.0);
                self.send_cwt_params();
                self.notify(&format!("cwt sigma: {:.0}", self.cwt_sigma));
            }
            KeyCode::ArrowDown if self.modifiers.shift_key() && self.analysis_mode == "cwt" => {
                self.cwt_sigma = (self.cwt_sigma - 1.0).max(5.0);
                self.send_cwt_params();
                self.notify(&format!("cwt sigma: {:.0}", self.cwt_sigma));
            }
            KeyCode::ArrowRight if self.modifiers.shift_key() && self.analysis_mode == "cwt" => {
                self.cwt_n_scales = (self.cwt_n_scales * 2).min(2048);
                self.send_cwt_params();
                self.notify(&format!("cwt scales: {}", self.cwt_n_scales));
            }
            KeyCode::ArrowLeft if self.modifiers.shift_key() && self.analysis_mode == "cwt" => {
                self.cwt_n_scales = (self.cwt_n_scales / 2).max(64);
                self.send_cwt_params();
                self.notify(&format!("cwt scales: {}", self.cwt_n_scales));
            }
            KeyCode::ArrowLeft
                if !self.modifiers.shift_key() && self.analysis_mode == "fft" =>
            {
                self.monitor_interval_ms =
                    (self.monitor_interval_ms + 1).clamp(MONITOR_INTERVAL_MIN_MS, MONITOR_INTERVAL_MAX_MS);
                self.send_monitor_params();
                self.notify(&format!("interval: {} ms", self.monitor_interval_ms));
            }
            KeyCode::ArrowRight
                if !self.modifiers.shift_key() && self.analysis_mode == "fft" =>
            {
                self.monitor_interval_ms =
                    self.monitor_interval_ms.saturating_sub(1).max(MONITOR_INTERVAL_MIN_MS);
                self.send_monitor_params();
                self.notify(&format!("interval: {} ms", self.monitor_interval_ms));
            }
            KeyCode::ArrowUp
                if !self.modifiers.shift_key() && self.analysis_mode == "fft" =>
            {
                self.monitor_fft_n = step_ladder(MONITOR_FFT_N_LADDER, self.monitor_fft_n, 1);
                self.monitor_interval_ms =
                    auto_monitor_interval_ms(self.monitor_fft_n, self.current_sr());
                self.send_monitor_params();
                self.reset_peak_holds();
                self.notify(&format!(
                    "fft N: {} @ {} ms",
                    self.monitor_fft_n, self.monitor_interval_ms
                ));
            }
            KeyCode::ArrowDown
                if !self.modifiers.shift_key() && self.analysis_mode == "fft" =>
            {
                self.monitor_fft_n = step_ladder(MONITOR_FFT_N_LADDER, self.monitor_fft_n, -1);
                self.monitor_interval_ms =
                    auto_monitor_interval_ms(self.monitor_fft_n, self.current_sr());
                self.send_monitor_params();
                self.reset_peak_holds();
                self.notify(&format!(
                    "fft N: {} @ {} ms",
                    self.monitor_fft_n, self.monitor_interval_ms
                ));
            }
            KeyCode::BracketLeft => {
                self.shift_hovered_db_floor(-5.0);
            }
            KeyCode::BracketRight => {
                self.shift_hovered_db_floor(5.0);
            }
            KeyCode::KeyR if self.modifiers.control_key() => {
                self.reset_all_views();
            }
            KeyCode::Tab => {
                let n_real = self.store.as_ref().map(|s| s.len()).unwrap_or(0);
                let n_virt = self.virtual_channels.len();
                // Virtual transfer channels participate in Tab cycling so the
                // user can drop into Single / Compare for any `transfer{n}`
                // without first having to re-select the pair.
                let n = (n_real + n_virt).max(1);
                // In Grid layout Tab pages through the grid (when more than
                // one page exists). Other layouts still cycle the active
                // channel for Single / overlay channel-of-interest.
                if matches!(self.config.layout, LayoutMode::Grid) {
                    let (_, _, _, pages) = layout::grid_dims(n, self.grid_params());
                    if pages > 1 {
                        let delta = if self.modifiers.shift_key() {
                            pages - 1
                        } else {
                            1
                        };
                        self.grid_page = (self.grid_page + delta) % pages;
                        self.notify(&format!("page {}/{}", self.grid_page + 1, pages));
                        return;
                    }
                }
                // Transfer layout: Tab/Shift+Tab rotates the active meas
                // channel and hot-swaps the running transfer_stream worker.
                // With only one meas selected this is a no-op.
                if matches!(self.config.layout, LayoutMode::Transfer) {
                    let meas_count = self.selection_order.len().saturating_sub(1);
                    if meas_count > 1 {
                        let delta = if self.modifiers.shift_key() {
                            meas_count - 1
                        } else {
                            1
                        };
                        self.active_meas_idx =
                            (self.active_meas_idx + delta) % meas_count;
                        let meas = self
                            .transfer_active_meas()
                            .unwrap_or(self.config.active_channel);
                        self.notify(&format!(
                            "MEAS CH{} ({}/{})",
                            meas,
                            self.active_meas_idx + 1,
                            meas_count,
                        ));
                        self.restart_transfer_stream();
                    }
                    return;
                }
                let delta = if self.modifiers.shift_key() { n - 1 } else { 1 };
                self.config.active_channel = (self.config.active_channel + delta) % n;
                let label = if self.config.active_channel < n_real {
                    format!("CH{}", self.config.active_channel)
                } else {
                    format!("transfer{}", self.config.active_channel - n_real)
                };
                self.notify(&label);
            }
            _ => {}
        }
    }

    /// Resolve the set of cell_views a non-mouse key interaction targets:
    /// hovered cell when the cursor is over one, otherwise every cell so the
    /// keybind still does *something* useful when the mouse is outside.
    fn key_targets(&self) -> Vec<usize> {
        match self.cursor_pos.and_then(|p| self.cell_at(p)) {
            Some((ch, _, _, _, _)) => self.targets_for_channel(ch),
            None => (0..self.cell_views.len()).collect(),
        }
    }

    fn adjust_hovered_db_span(&mut self, delta: f32) {
        for idx in self.key_targets() {
            if let Some(view) = self.cell_views.get_mut(idx) {
                let span = (view.db_max - view.db_min + delta).clamp(20.0, 240.0);
                view.db_min = (view.db_max - span).max(-240.0);
            }
        }
    }

    fn shift_hovered_db_floor(&mut self, delta: f32) {
        let mut last = 0.0_f32;
        for idx in self.key_targets() {
            if let Some(view) = self.cell_views.get_mut(idx) {
                view.db_min = (view.db_min + delta).clamp(-240.0, view.db_max - 10.0);
                last = view.db_min;
            }
        }
        self.notify(&format!("db min {}", last));
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

    fn redraw(&mut self) {
        let frame_start = Instant::now();
        let grid_params_snap = self.grid_params();
        // Drain any worker-error message the receiver picked up on the
        // `error` PUB topic BEFORE we take any long-lived &mut borrows on
        // self — notify() is &mut self and the render_ctx borrow below spans
        // the whole draw body.
        let pending_error = self.source.as_ref().and_then(|src| src.status()).and_then(|s| s.take_error());
        if let Some(err) = pending_error {
            if err.contains("transfer_stream") {
                self.transfer_stream_active = false;
            }
            self.notify(&err);
        }
        let ctx = match self.render_ctx.as_mut() {
            Some(c) => c,
            None => return,
        };
        let spectrum = self.spectrum.as_mut().unwrap();
        let waterfall = self.waterfall.as_mut().unwrap();
        let egui_renderer = self.egui_renderer.as_mut().unwrap();
        let egui_state = self.egui_state.as_mut().unwrap();

        let mut frames = {
            let store = self.store.as_mut();
            if let Some(store) = store {
                if !self.config.frozen {
                    // Drop the previous tick's DisplayFrames *before* reading so
                    // ChannelSlot::averaged has refcount 1 and `Arc::make_mut`
                    // can mutate in place instead of copy-on-write.
                    self.last_frames.clear();
                    self.last_frames = store.read_all(&self.config);
                } else {
                    let _ = store.read_all(&self.config);
                }
            }
            self.last_frames.clone()
        };

        if let Some(ts) = self.transfer_store.as_ref() {
            if !self.config.frozen {
                if let Some(frame) = ts.read() {
                    self.transfer_last = Some(frame);
                }
            }
        }

        // Virtual transfer channels get appended as extra cells so the grid /
        // single / compare layouts render `|H(ω)|` alongside real captures. A
        // virtual cell with no frame yet shows as empty (same as a real
        // channel before its first packet). Phase + coherence rendering is
        // deferred to a follow-up; this commit only wires magnitude-dB through
        // the spectrum/waterfall renderers.
        let n_real = frames.len();
        let virtual_snapshots = self.virtual_channels.read_all_with_serial();
        self.virtual_render_pairs = virtual_snapshots.iter().map(|(p, _, _)| *p).collect();
        {
            let live: std::collections::HashSet<_> =
                virtual_snapshots.iter().map(|(p, _, _)| *p).collect();
            self.virtual_seen_serial.retain(|p, _| live.contains(p));
        }
        for (pair, serial, maybe_tf) in &virtual_snapshots {
            let is_fresh = *serial != 0
                && self.virtual_seen_serial.get(pair).copied().unwrap_or(0) != *serial;
            if is_fresh {
                self.virtual_seen_serial.insert(*pair, *serial);
            }
            let frame = maybe_tf.as_ref().map(|tf| {
                let spectrum = Arc::new(tf.magnitude_db.clone());
                DisplayFrame {
                    spectrum: spectrum.clone(),
                    freqs:    Arc::new(tf.freqs.clone()),
                    meta: FrameMeta {
                        freq_hz:          0.0,
                        fundamental_dbfs: -140.0,
                        thd_pct:          0.0,
                        thdn_pct:         0.0,
                        in_dbu:           None,
                        sr:               tf.sr,
                        clipping:         false,
                        xruns:            0,
                    },
                    new_row: if is_fresh { Some(spectrum) } else { None },
                }
            });
            frames.push(frame);
        }

        // Fractional-octave smoothing. Runs before peak-hold so the held max
        // is taken over the smoothed trace the user is actually looking at;
        // it also keeps the frame-level `spectrum` consistent with what the
        // overlay reads for hover labels. Window indices are cached per
        // `(n_frac, n_bins, last_freq)` to avoid a log-range recompute per
        // frame.
        if let Some(n_frac) = self.smoothing_frac {
            for frame in frames.iter_mut().flatten() {
                if frame.freqs.is_empty() || frame.spectrum.is_empty() {
                    continue;
                }
                let last_f = *frame.freqs.last().unwrap();
                let needs_rebuild = self
                    .smoothing_cache
                    .as_ref()
                    .map_or(true, |w| !w.matches(n_frac, frame.freqs.len(), last_f));
                if needs_rebuild {
                    self.smoothing_cache = Some(smoothing::OctaveWindows::build(
                        n_frac,
                        frame.freqs.as_ref(),
                    ));
                }
                let windows = self.smoothing_cache.as_ref().unwrap();
                let smoothed = smoothing::smooth_db(
                    frame.spectrum.as_slice(),
                    windows,
                );
                frame.spectrum = Arc::new(smoothed);
            }
        }

        // Grow per-channel state arrays so the render path can index by the
        // virtual channel index the same way it does for real channels.
        // Shrinking never happens: real channel count is fixed for the
        // session, and removing a virtual channel leaves harmless empty
        // trailing slots (they'll be reused the next time the user presses T).
        let n_total = frames.len();
        if self.cell_views.len() < n_total {
            self.cell_views.resize(n_total, CellView::default());
        }
        if self.selected.len() < n_total {
            self.selected.resize(n_total, false);
        }
        if self.peak_holds.len() < n_real {
            self.peak_holds.resize(n_real, None);
        }
        if self.peak_last_update.len() < n_real {
            self.peak_last_update.resize(n_real, None);
        }
        if self.peak_last_tick.len() < n_real {
            self.peak_last_tick.resize(n_real, None);
        }
        if self.min_holds.len() < n_real {
            self.min_holds.resize(n_real, None);
        }
        if self.min_last_update.len() < n_real {
            self.min_last_update.resize(n_real, None);
        }
        if self.min_last_tick.len() < n_real {
            self.min_last_tick.resize(n_real, None);
        }

        // Peak-hold accumulator: fold every fresh spectrum bin-wise against
        // the held max. Virtual (transfer) channels are skipped — peak-hold
        // is a spectrum-only concept. A bin-count mismatch (FFT-N change we
        // missed, or a late first frame at a different N) re-seeds the
        // buffer instead of panicking or silently clipping.
        if self.peak_hold_enabled {
            let now = Instant::now();
            for (i, slot) in frames.iter().enumerate().take(n_real) {
                let Some(frame) = slot.as_ref() else { continue };
                if frame.new_row.is_none() || frame.spectrum.is_empty() {
                    continue;
                }
                let buf = match self.peak_holds.get_mut(i) {
                    Some(b) => b,
                    None => continue,
                };
                let stamp = self
                    .peak_last_update
                    .get_mut(i)
                    .expect("resized above");
                let tick = self
                    .peak_last_tick
                    .get_mut(i)
                    .expect("resized above");
                // Seconds since the previous frame we processed for this
                // channel — used below to scale the release drop. Clamped
                // into a sane range so a stall (tab hidden, debugger pause)
                // can't produce a single enormous drop on resume.
                let dt = tick
                    .map(|t| now.duration_since(t).as_secs_f32())
                    .unwrap_or(0.0)
                    .clamp(0.0, 0.25);
                *tick = Some(now);
                match buf.as_mut() {
                    Some(existing) if existing.len() == frame.spectrum.len() => {
                        let mut any_updated = false;
                        for (held, fresh) in existing.iter_mut().zip(frame.spectrum.iter()) {
                            if fresh.is_finite() && *fresh > *held {
                                *held = *fresh;
                                any_updated = true;
                            }
                        }
                        if any_updated {
                            *stamp = Some(now);
                        } else if let Some(last) = *stamp {
                            // Hold window has elapsed — glide down toward the
                            // live trace at a bounded dB/s so the peak fades
                            // out instead of blinking away. Clamped to `fresh`
                            // so a bin that's already below the current
                            // spectrum stops falling.
                            if now.duration_since(last) >= PEAK_HOLD_DECAY {
                                let drop = PEAK_RELEASE_DB_PER_SEC * dt;
                                for (held, fresh) in
                                    existing.iter_mut().zip(frame.spectrum.iter())
                                {
                                    if fresh.is_finite() {
                                        *held = (*held - drop).max(*fresh);
                                    }
                                }
                            }
                        } else {
                            *stamp = Some(now);
                        }
                    }
                    _ => {
                        *buf = Some(frame.spectrum.as_ref().clone());
                        *stamp = Some(now);
                    }
                }
            }
        }

        // Drum tuner candidates are pushed by the daemon on the `tuner`
        // PUB topic and cached in `tuner_store`. Off mode drops the cache so
        // a stale pre-Off readout doesn't reappear on re-enter.
        if self.tuner_mode == TunerMode::Off {
            if let Some(s) = self.tuner_store.as_ref() {
                s.clear();
            }
            self.tuner_range_lock = None;
        }
        if self.tuner_locks.len() < n_real {
            self.tuner_locks.resize(n_real, None);
        }

        // Min-hold accumulator: mirror of the peak loop with the comparator
        // flipped. Same decay rule so a brief gap in the signal doesn't pin
        // the trace down forever at whatever accidental silence the buffer
        // captured.
        if self.min_hold_enabled {
            let now = Instant::now();
            for (i, slot) in frames.iter().enumerate().take(n_real) {
                let Some(frame) = slot.as_ref() else { continue };
                if frame.new_row.is_none() || frame.spectrum.is_empty() {
                    continue;
                }
                let buf = match self.min_holds.get_mut(i) {
                    Some(b) => b,
                    None => continue,
                };
                let stamp = self
                    .min_last_update
                    .get_mut(i)
                    .expect("resized above");
                let tick = self
                    .min_last_tick
                    .get_mut(i)
                    .expect("resized above");
                let dt = tick
                    .map(|t| now.duration_since(t).as_secs_f32())
                    .unwrap_or(0.0)
                    .clamp(0.0, 0.25);
                *tick = Some(now);
                match buf.as_mut() {
                    Some(existing) if existing.len() == frame.spectrum.len() => {
                        let mut any_updated = false;
                        for (held, fresh) in existing.iter_mut().zip(frame.spectrum.iter()) {
                            if fresh.is_finite() && *fresh < *held {
                                *held = *fresh;
                                any_updated = true;
                            }
                        }
                        if any_updated {
                            *stamp = Some(now);
                        } else if let Some(last) = *stamp {
                            // Symmetric release — rise toward live so a quiet
                            // moment doesn't pin the noise-floor trace at a
                            // fluke dropout forever.
                            if now.duration_since(last) >= PEAK_HOLD_DECAY {
                                let rise = PEAK_RELEASE_DB_PER_SEC * dt;
                                for (held, fresh) in
                                    existing.iter_mut().zip(frame.spectrum.iter())
                                {
                                    if fresh.is_finite() {
                                        *held = (*held + rise).min(*fresh);
                                    }
                                }
                            }
                        } else {
                            *stamp = Some(now);
                        }
                    }
                    _ => {
                        *buf = Some(frame.spectrum.as_ref().clone());
                        *stamp = Some(now);
                    }
                }
            }
        }

        if self.waterfall_inited.len() < n_total {
            self.waterfall_inited.resize(n_total, false);
        }

        let n_channels = frames.len();
        let cells = layout::compute(
            self.config.layout,
            n_channels,
            self.config.active_channel,
            &self.selected,
            &self.selection_order,
            self.active_meas_idx,
            grid_params_snap,
        );
        let in_transfer_layout = matches!(self.config.layout, LayoutMode::Transfer);
        let in_sweep_layout = matches!(self.config.layout, LayoutMode::Sweep);
        if let Some(ss) = self.sweep_store.as_ref() {
            if !self.config.frozen {
                self.sweep_last = ss.read();
            }
        }

        // Track producer cadence from channel-0 new_row arrivals. Rolling
        // median over the last WATERFALL_ROW_DT_WINDOW samples so a single
        // hiccup or brief stall can't drag the axis; guarded to a sane band
        // (1 ms..5 s) to reject clock jumps and first-frame deltas. A small
        // hysteresis gate suppresses label flipping from median micro-churn
        // while leaving real cadence shifts free to propagate.
        if let Some(Some(f0)) = frames.first() {
            if f0.new_row.is_some() {
                let now = Instant::now();
                if let Some(prev) = self.waterfall_last_row_at {
                    let dt = now.duration_since(prev).as_secs_f32();
                    if dt > 0.001 && dt < 5.0 {
                        if self.waterfall_row_dts.len() == WATERFALL_ROW_DT_WINDOW {
                            self.waterfall_row_dts.pop_front();
                        }
                        self.waterfall_row_dts.push_back(dt);
                        if self.waterfall_row_dts.len() >= WATERFALL_ROW_DT_MIN {
                            let slice: Vec<f32> =
                                self.waterfall_row_dts.iter().copied().collect();
                            if let Some(med) = median_f32(&slice) {
                                let cur = self.waterfall_row_period_s.max(1e-6);
                                if ((med - cur) / cur).abs() > WATERFALL_ROW_DT_HYSTERESIS {
                                    self.waterfall_row_period_s = med;
                                }
                            }
                        }
                    }
                }
                self.waterfall_last_row_at = Some(now);
            }
        }
        // Stretch the freq clamp to whatever Nyquist the producer is running
        // at: fake-audio daemon is typically 48 kHz → 24 kHz, but a 96 kHz
        // session will hand us freqs up to ~48 kHz and the clamp must follow.
        // Daemon owns the aggregation and publishes bins spanning f_min..f_max
        // (20 Hz .. sr/2). The GPU shader maps bin index linearly across the
        // viewport, so the on-screen axis is correct only if view.freq_min /
        // freq_max match the data range. Lock both to the data range — pan/zoom
        // on the freq axis was explicitly traded away for this.
        let mut data_max_seen = self.data_freq_ceiling;
        let mut data_min_seen = theme::DEFAULT_FREQ_MIN;
        for slot in frames.iter().flatten() {
            if let Some(&last) = slot.freqs.last() {
                if last.is_finite() && last > data_max_seen {
                    data_max_seen = last;
                }
            }
            if let Some(&first) = slot.freqs.first() {
                if first.is_finite() && first > 0.0 {
                    data_min_seen = first;
                }
            }
        }
        if data_max_seen > self.data_freq_ceiling {
            self.data_freq_ceiling = data_max_seen;
        }
        for cv in self.cell_views.iter_mut() {
            cv.freq_min = data_min_seen;
            cv.freq_max = self.data_freq_ceiling;
        }

        let view_mode = self.config.view_mode;
        // First waterfall frame per channel picks a dB window derived from
        // the actual signal: ceiling just above the observed peak (clamped
        // to [-20, 0]), 80 dB span below. A fixed [-60, 0] window renders
        // black for line-in mic levels ~-100 dBFS, forcing the user to mash
        // `+` / `[` to see anything.
        if matches!(view_mode, ViewMode::Waterfall) {
            for (i, slot) in frames.iter().enumerate() {
                let Some(frame) = slot.as_ref() else { continue };
                if frame.spectrum.is_empty() {
                    continue;
                }
                let already = self.waterfall_inited.get(i).copied().unwrap_or(true);
                if already {
                    continue;
                }
                let peak = frame
                    .spectrum
                    .iter()
                    .copied()
                    .filter(|v| v.is_finite())
                    .fold(f32::NEG_INFINITY, f32::max);
                let (db_min, db_max) = if peak.is_finite() {
                    let top = (peak + 5.0).clamp(-20.0, 0.0);
                    (top - 80.0, top)
                } else {
                    (-60.0, 0.0)
                };
                if let Some(view) = self.cell_views.get_mut(i) {
                    view.db_min = db_min;
                    view.db_max = db_max;
                }
                if let Some(flag) = self.waterfall_inited.get_mut(i) {
                    *flag = true;
                }
            }
        }
        let mut spectrum_uploads: Vec<ChannelUpload> = Vec::new();
        let mut waterfall_uploads: Vec<WaterfallCellUpload<'_>> = Vec::new();
        if !in_transfer_layout && !in_sweep_layout {
            match view_mode {
                ViewMode::Spectrum => spectrum_uploads.reserve(cells.len()),
                ViewMode::Waterfall => waterfall_uploads.reserve(cells.len()),
            }
        }

        for cell in &cells {
            if in_transfer_layout || in_sweep_layout {
                break;
            }
            let frame = match frames.get(cell.channel).and_then(|f| f.as_ref()) {
                Some(f) if !f.spectrum.is_empty() => f,
                _ => continue,
            };
            let view = self
                .cell_views
                .get(cell.channel)
                .copied()
                .unwrap_or_default();
            let freq_log_min = view.freq_min.max(1.0).log10();
            let freq_log_max = view.freq_max.max(20.0).log10();
            match view_mode {
                ViewMode::Spectrum => {
                    // Single view + virtual transfer channel splits the
                    // cell into spectrum (top) + phase subplot (bottom).
                    // GPU viewport uses y=0 at bottom, so shift origin up
                    // by (1 - FRACTION) * cell.h and shrink height.
                    let single_virtual = matches!(self.config.layout, LayoutMode::Single)
                        && cell.channel >= n_real;
                    let (vp_y, vp_h) = if single_virtual {
                        let frac = crate::render::virtual_overlay::SPECTRUM_FRACTION_SINGLE;
                        (cell.y + cell.h * (1.0 - frac), cell.h * frac)
                    } else {
                        (cell.y, cell.h)
                    };
                    // Daemon now publishes a log-aggregated spectrum (see
                    // ac_core::aggregate::spectrum_to_columns_wire). Upload as-is.
                    let live_cols: Vec<f32> = frame.spectrum.as_ref().clone();
                    let meta = ChannelMeta {
                        color: theme::channel_color(cell.channel),
                        viewport: [cell.x, vp_y, cell.w, vp_h],
                        db_min: view.db_min,
                        db_max: view.db_max,
                        freq_log_min,
                        freq_log_max,
                        n_bins: live_cols.len() as u32,
                        offset: 0,
                        fill_alpha: 0.0,
                        line_width: 0.0,
                    };
                    spectrum_uploads.push(ChannelUpload {
                        spectrum: live_cols,
                        meta,
                    });
                    // Peak-hold trace. Reuses the spectrum pipeline as a second
                    // upload with the same viewport/axes but a brighter,
                    // channel-tinted colour and a thicker line so the frozen
                    // max stands out above the live trace. Using the channel
                    // hue (brightened) instead of a single cyan means the
                    // peak line in Compare layout is visually paired with
                    // its parent trace — with N peaks overlapping in one
                    // rect, a shared cyan made them indistinguishable. Only
                    // real channels get a peak; virtual transfer cells are
                    // excluded (peak of a transfer magnitude is not a useful
                    // measurement).
                    if self.peak_hold_enabled && cell.channel < n_real {
                        if let Some(Some(peak)) = self.peak_holds.get(cell.channel) {
                            if peak.len() == frame.spectrum.len() {
                                let mut peak_color = theme::channel_color(cell.channel);
                                // Brighten so peak sits visibly above the live
                                // trace in the same hue. Clamp to 1.0 so
                                // already-bright hues (pale gold) don't wrap.
                                for c in peak_color.iter_mut().take(3) {
                                    *c = (*c * 1.35).min(1.0);
                                }
                                let peak_cols: Vec<f32> = peak.clone();
                                let n_cols = peak_cols.len() as u32;
                                spectrum_uploads.push(ChannelUpload {
                                    spectrum: peak_cols,
                                    meta: ChannelMeta {
                                        color: peak_color,
                                        viewport: [cell.x, vp_y, cell.w, vp_h],
                                        db_min: view.db_min,
                                        db_max: view.db_max,
                                        freq_log_min,
                                        freq_log_max,
                                        n_bins: n_cols,
                                        offset: 0,
                                        fill_alpha: 0.0001,
                                        // line_width is a HALF-WIDTH in 0..1 screen
                                        // space; ~1.6× default reads as a
                                        // distinctly thicker trace.
                                        line_width: 0.003,
                                    },
                                });
                            }
                        }
                    }
                    // Min-hold trace: darker channel tint, same thickness as
                    // the live trace. Sits underneath the live line in dB;
                    // a darker hue keeps it from competing with peak + live
                    // for the eye.
                    if self.min_hold_enabled && cell.channel < n_real {
                        if let Some(Some(min)) = self.min_holds.get(cell.channel) {
                            if min.len() == frame.spectrum.len() {
                                let base = theme::channel_color(cell.channel);
                                let min_color = [
                                    base[0] * 0.55,
                                    base[1] * 0.55,
                                    base[2] * 0.55,
                                    1.0,
                                ];
                                let min_cols: Vec<f32> = min.clone();
                                let n_cols = min_cols.len() as u32;
                                spectrum_uploads.push(ChannelUpload {
                                    spectrum: min_cols,
                                    meta: ChannelMeta {
                                        color: min_color,
                                        viewport: [cell.x, vp_y, cell.w, vp_h],
                                        db_min: view.db_min,
                                        db_max: view.db_max,
                                        freq_log_min,
                                        freq_log_max,
                                        n_bins: n_cols,
                                        offset: 0,
                                        fill_alpha: 0.0001,
                                        line_width: 0.0022,
                                    },
                                });
                            }
                        }
                    }
                }
                ViewMode::Waterfall => {
                    // Detect log vs linear bin spacing from step growth at
                    // the two ends of freqs. Synthetic log-spaced grows by
                    // ~×1.01 per bin at the top vs bottom; real FFT linear
                    // bins have constant step.
                    let n = frame.freqs.len();
                    let (freq_first, freq_last, log_spaced) = if n >= 4 {
                        let lo_step = frame.freqs[1] - frame.freqs[0];
                        let hi_step = frame.freqs[n - 1] - frame.freqs[n - 2];
                        let is_log = hi_step > lo_step * 3.0;
                        (frame.freqs[0], frame.freqs[n - 1], is_log)
                    } else {
                        (
                            frame.freqs.first().copied().unwrap_or(1.0),
                            frame.freqs.last().copied().unwrap_or(24000.0),
                            false,
                        )
                    };
                    waterfall_uploads.push(WaterfallCellUpload {
                        channel: cell.channel,
                        viewport: [cell.x, cell.y, cell.w, cell.h],
                        db_min: view.db_min,
                        db_max: view.db_max,
                        freq_log_min,
                        freq_log_max,
                        n_bins: frame.spectrum.len() as u32,
                        freq_first,
                        freq_last,
                        log_spaced,
                        rows_visible: view.rows_visible_f,
                        new_row: frame.new_row.as_deref().map(|v| v.as_slice()),
                    });
                }
            }
        }

        match view_mode {
            ViewMode::Spectrum => {
                spectrum.upload(&ctx.device, &ctx.queue, &spectrum_uploads);
            }
            ViewMode::Waterfall => {
                waterfall.upload(&ctx.device, &ctx.queue, n_channels, &waterfall_uploads);
            }
        }

        let raw_input = egui_state.take_egui_input(&ctx.window);
        let show_labels = self.config.layout != LayoutMode::Grid || n_channels <= 8;
        let connected = self
            .source
            .as_ref()
            .map(|s| s.connected())
            .unwrap_or(false);
        let config_snap = self.config.clone();
        let cell_views_snap = self.cell_views.clone();
        let selected_snap = self.selected.clone();
        let virtual_pairs_snap = self.virtual_render_pairs.clone();
        let peak_hold_enabled_snap = self.peak_hold_enabled;
        let active_palette_snap = waterfall.active_palette();
        let smoothing_snap = self.smoothing_frac;
        let peak_holds_snap = if self.peak_hold_enabled {
            self.peak_holds.clone()
        } else {
            Vec::new()
        };
        let tuner_mode_snap = self.tuner_mode;
        let (tuner_last_snap, tuner_history_snap): (
            Vec<Option<ac_core::tuner::FundamentalCandidate>>,
            Vec<Vec<f64>>,
        ) = if self.tuner_mode != TunerMode::Off {
            self.tuner_store
                .as_ref()
                .map(|s| s.snapshot(n_real))
                .unwrap_or_else(|| (Vec::new(), Vec::new()))
        } else {
            (Vec::new(), Vec::new())
        };
        let tuner_locks_snap = if self.tuner_mode == TunerMode::Locked {
            self.tuner_locks.clone()
        } else {
            Vec::new()
        };
        let tuner_range_lock_snap = if self.tuner_mode != TunerMode::Off {
            self.tuner_range_lock
        } else {
            None
        };
        let tuner_sens_snap = self.tuner_sensitivity;
        let tuner_min_level_snap = self.tuner_min_level_dbfs;
        let virtual_tf_snap: Vec<Option<TransferFrame>> = virtual_snapshots
            .iter()
            .map(|(_, _, tf)| tf.clone())
            .collect();
        let n_real_snap = n_real;
        let show_help_snap = self.show_help;
        let monitor_params_snap = (self.analysis_mode == "fft").then_some(MonitorParamsInfo {
            interval_ms: self.monitor_interval_ms,
            fft_n: self.monitor_fft_n,
        });
        let mut transfer_snap: Option<TransferFrame> = if in_transfer_layout {
            self.transfer_last.clone()
        } else {
            None
        };
        // Apply the same fractional-octave smoothing to the full Transfer-layout
        // magnitude trace so it matches the grid-view virtual cells. Only
        // magnitude-dB is smoothed — phase has 2π wraps that would average to
        // meaningless midpoints, and coherence is already a windowed stat.
        if let (Some(n_frac), Some(tf)) = (self.smoothing_frac, transfer_snap.as_mut()) {
            if !tf.freqs.is_empty() && !tf.magnitude_db.is_empty() {
                let windows = smoothing::OctaveWindows::build(n_frac, &tf.freqs);
                tf.magnitude_db = smoothing::smooth_db(&tf.magnitude_db, &windows);
            }
        }
        let sweep_snap = if in_sweep_layout {
            Some(self.sweep_last.clone())
        } else {
            None
        };
        let sweep_kind_snap = self.sweep_kind;
        let sweep_sel_snap = self.sweep_selected_idx;
        let selection_order_snap = self.selection_order.clone();
        let active_meas_idx_snap = self.active_meas_idx;
        let active_meas_snap = {
            // Inline to dodge an otherwise-mutable borrow of `self` held by
            // `render_ctx.as_mut()` above.
            let n = selection_order_snap.len();
            if n >= 2 {
                let meas_count = n - 1;
                let idx = active_meas_idx_snap.min(meas_count - 1);
                Some(selection_order_snap[idx])
            } else {
                None
            }
        };
        let width_px = ctx.config.width as f32;
        let height_px = ctx.config.height as f32;
        let notification = self
            .notification
            .as_ref()
            .filter(|(_, t)| t.elapsed() < NOTIFICATION_TTL)
            .map(|(s, _)| s.clone());
        let timing_for_overlay: Option<StatsSnapshot> =
            self.show_timing.then(|| self.timing_stats.snapshot());
        let gpu_supported = ctx.timing.is_some();

        // Resolve the hovered cell inline off local snapshots so we don't
        // borrow `self` across the egui-closure lifetime.
        let hover_info = self.cursor_pos.and_then(|pos| {
            let cx = pos.x as f32;
            let cy = pos.y as f32;
            let mut hit: Option<(usize, egui::Rect, f32, f32)> = None;
            for c in &cells {
                let r = layout::to_pixel_rect(c, width_px, height_px);
                if cx >= r.left() && cx <= r.right() && cy >= r.top() && cy <= r.bottom() {
                    let nx = (cx - r.left()) / r.width().max(1.0);
                    let ny = 1.0 - (cy - r.top()) / r.height().max(1.0);
                    hit = Some((c.channel, r, nx, ny));
                    break;
                }
            }
            let (channel, rect, nx, ny) = hit?;
            let view = cell_views_snap
                .get(channel)
                .copied()
                .unwrap_or_default();
            let log_min = view.freq_min.max(1.0).log10();
            let log_max = view.freq_max.max(log_min.exp().max(1.1)).log10();
            let freq_hz = 10_f32.powf(log_min + nx * (log_max - log_min));
            // In Transfer layout the y-axis meaning depends on which
            // sub-panel the cursor is in — mag shows dB, phase shows degrees,
            // coh shows 0..1. Outside all three panels (the inter-panel gap)
            // we fall back to mag dB so the crosshair label stays populated.
            let readout = if matches!(config_snap.layout, LayoutMode::Sweep) {
                let cursor = egui::pos2(cx, cy);
                let kind = sweep_kind_snap.unwrap_or(SweepKind::Frequency);
                match crate::render::sweep::hit_test(rect, cursor, kind) {
                    Some((crate::render::sweep::SweepHitPanel::Thd, v)) => {
                        HoverReadout::Thd(v)
                    }
                    Some((crate::render::sweep::SweepHitPanel::Gain, v)) => {
                        HoverReadout::Gain(v)
                    }
                    Some((crate::render::sweep::SweepHitPanel::SpectrumDetail, v)) => {
                        HoverReadout::Db(v)
                    }
                    None => HoverReadout::Db(0.0),
                }
            } else if matches!(config_snap.layout, LayoutMode::Transfer) {
                let cursor = egui::pos2(cx, cy);
                match crate::render::transfer::hit_test(rect, cursor) {
                    Some((crate::render::transfer::HitPanel::Phase, v)) => {
                        HoverReadout::Phase(v)
                    }
                    Some((crate::render::transfer::HitPanel::Coherence, v)) => {
                        HoverReadout::Coherence(v)
                    }
                    Some((crate::render::transfer::HitPanel::Magnitude, v)) => {
                        HoverReadout::Db(v)
                    }
                    None => {
                        let db = view.db_min + ny * (view.db_max - view.db_min);
                        HoverReadout::Db(db)
                    }
                }
            } else {
                let db = view.db_min + ny * (view.db_max - view.db_min);
                HoverReadout::Db(db)
            };
            Some(HoverInfo {
                channel,
                rect,
                cursor: egui::pos2(cx, cy),
                freq_hz,
                readout,
            })
        });

        let row_period_s = self.waterfall_row_period_s;
        let full_output = self.egui_ctx.run(raw_input, |ui_ctx| {
            let painter = ui_ctx.layer_painter(egui::LayerId::new(
                egui::Order::Background,
                egui::Id::new("ac-ui-grid"),
            ));
            let sel_border = egui::Color32::from_rgb(
                theme::SELECT_BORDER[0],
                theme::SELECT_BORDER[1],
                theme::SELECT_BORDER[2],
            );
            // Counter for how many peak readouts have been placed in the
            // top-right corner so far this frame. Only advances in Compare
            // layout (overlapping cells); other layouts each have their own
            // rect so every readout can sit at slot 0.
            let mut peak_corner_slot: usize = 0;
            for cell in &cells {
                let rect = layout::to_pixel_rect(cell, width_px, height_px);
                let view = cell_views_snap
                    .get(cell.channel)
                    .copied()
                    .unwrap_or_default();
                if matches!(config_snap.layout, LayoutMode::Sweep) {
                    if let Some(ref ss) = sweep_snap {
                        let kind = sweep_kind_snap.unwrap_or(SweepKind::Frequency);
                        crate::render::sweep::draw(&painter, rect, kind, ss, sweep_sel_snap);
                    }
                    continue;
                }
                if matches!(config_snap.layout, LayoutMode::Transfer) {
                    let color = theme::channel_color(cell.channel);
                    crate::render::transfer::draw(
                        &painter,
                        rect,
                        &view,
                        transfer_snap.as_ref(),
                        color,
                    );
                    continue;
                }
                let time_axis = matches!(config_snap.view_mode, ViewMode::Waterfall)
                    .then(|| grid::WaterfallTimeAxis {
                        row_period_s,
                        rows_visible: view.rows_visible_f,
                    });
                // Single view + virtual transfer channel → split cell:
                // spectrum on top, standalone phase subplot below. In all
                // other cases the grid fills the full cell and the phase
                // data (if any) overlays the spectrum.
                let single_virtual = matches!(config_snap.layout, LayoutMode::Single)
                    && matches!(config_snap.view_mode, ViewMode::Spectrum)
                    && cell.channel >= n_real_snap;
                let (grid_rect, phase_rect) = if single_virtual {
                    let frac = crate::render::virtual_overlay::SPECTRUM_FRACTION_SINGLE;
                    let split_y = rect.top() + rect.height() * frac;
                    let top = egui::Rect::from_min_max(
                        rect.min,
                        egui::pos2(rect.max.x, split_y),
                    );
                    let bot = egui::Rect::from_min_max(
                        egui::pos2(rect.min.x, split_y),
                        rect.max,
                    );
                    (top, Some(bot))
                } else {
                    (rect, None)
                };
                // Freq x-axis labels sit at the real cell bottom: the phase
                // subplot when split, the grid rect otherwise.
                let grid_freq_labels = show_labels && phase_rect.is_none();
                grid::draw_grid(
                    &painter,
                    grid_rect,
                    &view,
                    config_snap.view_mode,
                    show_labels,
                    grid_freq_labels,
                    time_axis,
                );
                let is_selected = selected_snap
                    .get(cell.channel)
                    .copied()
                    .unwrap_or(false);
                // Highlight selected cells in the non-Compare layouts. In
                // Compare the cells are already filtered to the selection set,
                // so a per-cell border just adds noise on top of the legend.
                if is_selected && !matches!(config_snap.layout, LayoutMode::Compare) {
                    painter.rect_stroke(
                        rect,
                        egui::CornerRadius::same(2),
                        egui::Stroke::new(1.5, sel_border),
                        egui::StrokeKind::Inside,
                    );
                }
                // Peak-hold overlay: fundamental marker + 2×–5× harmonic
                // ticks + corner readout. Spectrum view only; virtual
                // channels excluded (peak-of-transfer magnitude is not a
                // useful reading). Drawn after the grid so it stacks above
                // the axes.
                if peak_hold_enabled_snap
                    && matches!(config_snap.view_mode, ViewMode::Spectrum)
                    && cell.channel < n_real_snap
                {
                    if let (Some(Some(peak)), Some(Some(frame))) = (
                        peak_holds_snap.get(cell.channel),
                        frames.get(cell.channel),
                    ) {
                        draw_peak_overlay(
                            &painter,
                            grid_rect,
                            cell.channel,
                            peak,
                            &frame.freqs,
                            &view,
                            peak_corner_slot,
                        );
                        // Only Compare stacks readouts in one shared rect;
                        // Grid/Single cells have their own top-right so reset
                        // each cell to slot 0.
                        if matches!(config_snap.layout, LayoutMode::Compare) {
                            peak_corner_slot += 1;
                        }
                    }
                }
                // Drum-tuner overlay: f0 marker + membrane-mode partial
                // markers + corner readout. Spectrum view only, real channels
                // only. Sits above the peak-hold glyphs so the tuner markers
                // aren't obscured by them on a loud drum hit where both
                // features co-exist.
                if tuner_mode_snap != TunerMode::Off
                    && matches!(config_snap.view_mode, ViewMode::Spectrum)
                    && cell.channel < n_real_snap
                {
                    let raw_cand = tuner_last_snap
                        .get(cell.channel)
                        .and_then(|o| o.as_ref());
                    let cand_opt = raw_cand
                        .filter(|c| c.confidence >= TUNER_MIN_CONFIDENCE);
                    let hist_empty: Vec<f64> = Vec::new();
                    let hist = tuner_history_snap
                        .get(cell.channel)
                        .unwrap_or(&hist_empty);
                    if std::env::var_os("AC_TUNER_DEBUG").is_some() {
                        eprintln!(
                            "[ui draw] ch{} mode={:?} raw={} cand_pass={} hist_len={} freqs_len={}",
                            cell.channel, tuner_mode_snap,
                            raw_cand.map(|c| format!("f0={:.1}Hz conf={:.2}", c.freq_hz, c.confidence))
                                .unwrap_or_else(|| "None".into()),
                            cand_opt.is_some(), hist.len(),
                            frames.get(cell.channel).and_then(|f| f.as_ref()).map(|f| f.freqs.len()).unwrap_or(0),
                        );
                    }
                    if cand_opt.is_some()
                        || !hist.is_empty()
                        || tuner_range_lock_snap.is_some()
                    {
                        let lock = tuner_locks_snap
                            .get(cell.channel)
                            .copied()
                            .flatten();
                        let freqs: &[f32] = frames
                            .get(cell.channel)
                            .and_then(|f| f.as_ref())
                            .map(|f| f.freqs.as_slice())
                            .unwrap_or(&[]);
                        draw_tuner_overlay(
                            &painter,
                            grid_rect,
                            cell.channel,
                            cand_opt,
                            freqs,
                            hist,
                            &view,
                            lock,
                            tuner_mode_snap,
                            tuner_range_lock_snap,
                            tuner_sens_snap,
                            tuner_min_level_snap,
                        );
                    }
                }
                // Virtual transfer channels get a standalone phase subplot
                // in Single view (split cell, per issue #49). Grid/Compare
                // show magnitude only — the phase overlay was visually
                // intrusive at grid cell size. Waterfall view is also a
                // no-op since the row image can't host a static polyline.
                if matches!(config_snap.view_mode, ViewMode::Spectrum)
                    && cell.channel >= n_real_snap
                {
                    if let Some(bot) = phase_rect {
                        let vi = cell.channel - n_real_snap;
                        if let Some(Some(tf)) = virtual_tf_snap.get(vi) {
                            painter.line_segment(
                                [
                                    egui::pos2(rect.left(), bot.top()),
                                    egui::pos2(rect.right(), bot.top()),
                                ],
                                egui::Stroke::new(
                                    1.0,
                                    egui::Color32::from_rgba_unmultiplied(180, 180, 180, 40),
                                ),
                            );
                            crate::render::virtual_overlay::draw_phase_subplot(
                                &painter, bot, &view, tf, show_labels,
                            );
                        }
                    }
                }
            }
            overlay::draw(
                ui_ctx,
                OverlayInput {
                    config: &config_snap,
                    frames: &frames,
                    cell_views: &cell_views_snap,
                    selected: &selected_snap,
                    selection_order: &selection_order_snap,
                    transfer: transfer_snap.as_ref(),
                    active_meas: active_meas_snap,
                    active_meas_idx: active_meas_idx_snap,
                    connected,
                    notification: notification.as_deref(),
                    timing: timing_for_overlay,
                    gpu_supported,
                    hover: hover_info.clone(),
                    show_help: show_help_snap,
                    monitor_params: monitor_params_snap,
                    n_real: n_real_snap,
                    virtual_pairs: &virtual_pairs_snap,
                    active_palette: active_palette_snap,
                    smoothing_frac: smoothing_snap,
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

        let acquire_start = Instant::now();
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
        let acquire_wait = acquire_start.elapsed();
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

        let spectrum_writes = ctx.timing.as_ref().map(|t| t.spectrum_writes());
        let egui_writes     = ctx.timing.as_ref().map(|t| t.egui_writes());

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
                timestamp_writes: spectrum_writes,
                occlusion_query_set: None,
            });
            match view_mode {
                ViewMode::Spectrum => spectrum.draw(&mut pass),
                ViewMode::Waterfall => waterfall.draw(&mut pass),
            }
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
                timestamp_writes: egui_writes,
                occlusion_query_set: None,
            });
            let mut pass = pass.forget_lifetime();
            egui_renderer.render(&mut pass, &paint_jobs, &screen_desc);
        }

        if let Some(timing) = ctx.timing.as_mut() {
            timing.resolve(&mut encoder);
        }

        let capture = if self.pending_screenshot {
            self.pending_screenshot = false;
            Some(prepare_capture(ctx, &mut encoder, &surface_tex))
        } else {
            None
        };

        ctx.queue.submit(Some(encoder.finish()));
        surface_tex.present();

        let gpu_pass = if let Some(timing) = ctx.timing.as_mut() {
            timing.after_submit();
            let _ = ctx.device.poll(wgpu::Maintain::Poll);
            timing.poll();
            timing.last()
        } else {
            crate::render::timing::PassTimings::default()
        };

        for id in &full_output.textures_delta.free {
            egui_renderer.free_texture(id);
        }

        if let Some(cap) = capture {
            let transfer_for_capture = if in_transfer_layout {
                self.transfer_last.clone()
            } else {
                None
            };
            finalize_capture(ctx, cap, &self.output_dir, &frames, transfer_for_capture);
            self.notify("saved");
        }

        let now = Instant::now();
        let frame_dt = now.saturating_duration_since(self.last_render);
        // Subtract the surface-acquire wait so the cpu metric reflects actual
        // CPU work, not vsync block time. With Fifo present mode the acquire
        // call sleeps until the next vblank; counting that as cpu time would
        // pin the metric to the frame budget regardless of how light the
        // workload is.
        let cpu_dt = now
            .saturating_duration_since(frame_start)
            .saturating_sub(acquire_wait);
        self.timing_stats.push(cpu_dt, frame_dt, gpu_pass);
        self.last_render = now;
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
    transfer: Option<TransferFrame>,
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
                transfer,
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
                self.drag = None;
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

/// Per-cell peak-hold overlay: hottest-peak triangle + label, 2×–5× harmonic
/// ticks, and a corner "PEAK CHn: f Hz A dB" readout. Called inside the egui
/// closure (Spectrum view, real channels only). Skips DC/sub-audio bins so
/// a spurious low-frequency excursion can't lock the marker.
fn draw_peak_overlay(
    painter: &egui::Painter,
    rect: egui::Rect,
    channel: usize,
    peak: &[f32],
    freqs: &[f32],
    view: &CellView,
    // Which row in the top-right corner this readout occupies. 0 = topmost.
    // Compare layout stacks N overlapping cells into the same rect; without a
    // per-channel slot the readouts would all overdraw the same pixel and
    // only the last one would survive.
    corner_slot: usize,
) {
    if peak.is_empty() || freqs.len() != peak.len() {
        return;
    }
    let log_min = view.freq_min.max(1.0).log10();
    let log_max = view.freq_max.max(log_min.exp().max(1.1)).log10();
    let log_span = (log_max - log_min).max(0.0001);
    let db_span = (view.db_max - view.db_min).max(0.0001);

    // Argmax across only the visible freq window, clamped to ≥ 20 Hz so the
    // marker can't latch onto DC or sub-audio noise.
    let mut best_idx: Option<usize> = None;
    let mut best_amp = f32::NEG_INFINITY;
    for (i, (&f, &amp)) in freqs.iter().zip(peak.iter()).enumerate() {
        if !f.is_finite() || !amp.is_finite() {
            continue;
        }
        if f < view.freq_min.max(theme::DEFAULT_FREQ_MIN) || f > view.freq_max {
            continue;
        }
        if amp > best_amp {
            best_amp = amp;
            best_idx = Some(i);
        }
    }
    let Some(argmax) = best_idx else { return };
    let f0 = freqs[argmax];
    let a0 = peak[argmax];

    // Colour the markers with the channel's own palette entry so Compare
    // layout — where every selected channel's peak traces stack into the
    // same rect — lets the eye pair each triangle/label/readout with its
    // underlying spectrum trace. `PEAK_MARKER` (cyan) was fine for a single
    // channel but ambiguous once N peaks share one cell.
    let ch_rgba = theme::channel_color(channel);
    let marker_color = Color32::from_rgb(
        (ch_rgba[0] * 255.0) as u8,
        (ch_rgba[1] * 255.0) as u8,
        (ch_rgba[2] * 255.0) as u8,
    );
    let tick_color = Color32::from_rgba_unmultiplied(
        (ch_rgba[0] * 255.0) as u8,
        (ch_rgba[1] * 255.0) as u8,
        (ch_rgba[2] * 255.0) as u8,
        160,
    );

    let freq_amp_to_px = |f: f32, amp: f32| -> egui::Pos2 {
        let tx = (f.max(1.0).log10() - log_min) / log_span;
        let ty = (amp - view.db_min) / db_span;
        let x = rect.left() + tx.clamp(0.0, 1.0) * rect.width();
        let y = rect.top() + (1.0 - ty.clamp(0.0, 1.0)) * rect.height();
        egui::pos2(x, y)
    };

    // Collect the 2×..=5× harmonic samples up front so the same data drives
    // both the on-plot markers and the corner text block. A harmonic is kept
    // regardless of whether its frequency is inside the current view window —
    // the corner readout still wants to show its dB — but on-plot drawing
    // gates on the visible range.
    struct Harmonic {
        k: u32,
        f_hz: f32,
        amp_db: f32,
        in_view: bool,
    }
    let mut harmonics: Vec<Harmonic> = Vec::with_capacity(4);
    for k in 2u32..=5 {
        let f_k = f0 * k as f32;
        if freqs.last().map_or(true, |&f| f_k > f) {
            break;
        }
        // Nearest-bin search; freqs are monotonic so a simple partition_point
        // lands on the right neighbour. Clamp to the buffer so the amp lookup
        // never panics.
        let near = match freqs.partition_point(|&f| f < f_k) {
            0 => 0,
            p if p >= freqs.len() => freqs.len() - 1,
            p => {
                if (freqs[p] - f_k).abs() < (f_k - freqs[p - 1]).abs() {
                    p
                } else {
                    p - 1
                }
            }
        };
        let a_k = peak[near];
        if !a_k.is_finite() {
            continue;
        }
        harmonics.push(Harmonic {
            k,
            f_hz: f_k,
            amp_db: a_k,
            in_view: f_k >= view.freq_min && f_k <= view.freq_max,
        });
    }

    // Harmonic markers: small downward triangle + "k×" label at each
    // in-view harmonic's amplitude. A 5-px vertical tick was too thin to
    // read against the spectrum trace; the triangle matches the fundamental
    // glyph at half size so the family resemblance is obvious.
    for h in harmonics.iter().filter(|h| h.in_view) {
        let p = freq_amp_to_px(h.f_hz, h.amp_db);
        let tri = [
            egui::pos2(p.x - 3.0, p.y - 8.0),
            egui::pos2(p.x + 3.0, p.y - 8.0),
            egui::pos2(p.x,       p.y - 2.0),
        ];
        painter.add(egui::Shape::convex_polygon(
            tri.to_vec(),
            tick_color,
            egui::Stroke::new(1.0, tick_color),
        ));
        painter.text(
            egui::pos2(p.x, p.y - 10.0),
            egui::Align2::CENTER_BOTTOM,
            format!("{}×", h.k),
            egui::FontId::monospace(theme::GRID_LABEL_PX),
            tick_color,
        );
    }

    // Fundamental marker: downward triangle above the peak point + label.
    let p0 = freq_amp_to_px(f0, a0);
    let tri = [
        egui::pos2(p0.x - 5.0, p0.y - 10.0),
        egui::pos2(p0.x + 5.0, p0.y - 10.0),
        egui::pos2(p0.x,       p0.y - 2.0),
    ];
    painter.add(egui::Shape::convex_polygon(
        tri.to_vec(),
        marker_color,
        egui::Stroke::new(1.0, marker_color),
    ));
    let label = format!("{} {:.1} dB", format_freq_compact(f0), a0);
    painter.text(
        egui::pos2(p0.x, p0.y - 12.0),
        egui::Align2::CENTER_BOTTOM,
        label,
        egui::FontId::monospace(theme::GRID_LABEL_PX),
        marker_color,
    );

    // Top-right corner readout — visible even when the peak is off-screen
    // (e.g. user zoomed below the fundamental). The header gives the
    // fundamental; one indented line per 2×..=5× harmonic follows with its
    // frequency and dB. `corner_slot` stacks one full block per channel so
    // Compare layout's overlapping cells don't overdraw a single pixel;
    // Grid/Single always pass 0 and render at the top.
    let row_h = theme::GRID_LABEL_PX + 2.0;
    let block_rows = 1 + harmonics.len();
    let block_top = rect.top() + 2.0 + corner_slot as f32 * block_rows as f32 * row_h;
    let corner = crate::ui::fmt::peak_corner_label(channel, f0, a0);
    painter.text(
        egui::pos2(rect.right() - 4.0, block_top),
        egui::Align2::RIGHT_TOP,
        corner,
        egui::FontId::monospace(theme::GRID_LABEL_PX),
        marker_color,
    );
    for (i, h) in harmonics.iter().enumerate() {
        let line = crate::ui::fmt::peak_harmonic_line(h.k, h.f_hz, h.amp_db);
        painter.text(
            egui::pos2(
                rect.right() - 4.0,
                block_top + (i + 1) as f32 * row_h,
            ),
            egui::Align2::RIGHT_TOP,
            line,
            egui::FontId::monospace(theme::GRID_LABEL_PX),
            tick_color,
        );
    }
}

// Compact frequency formatter for the peak overlay lives in `ui::fmt`;
// re-export here so existing call sites in this file don't have to fully
// qualify the path.
use crate::ui::fmt::format_freq_compact;

/// Per-cell drum-tuner overlay. Draws a full-height vertical line at the
/// identified (0,1) fundamental, shorter ticks at each matched overtone
/// partial, and a corner readout stack with Hz/note/cents/confidence and
/// (when locked) the target + deviation with traffic-light colouring.
///
/// Coordinate system matches `draw_peak_overlay`: log-frequency x-axis
/// clamped to `view.freq_min..freq_max`; any marker whose Hz falls outside
/// the visible window is skipped for the on-plot glyph but still appears
/// in the corner text so the reader sees it exists.
#[allow(clippy::too_many_arguments)]
fn draw_tuner_overlay(
    painter: &egui::Painter,
    rect: egui::Rect,
    channel: usize,
    cand: Option<&ac_core::tuner::FundamentalCandidate>,
    freqs: &[f32],
    history: &[f64],
    view: &CellView,
    lock_target_hz: Option<f64>,
    mode: TunerMode,
    range_lock: Option<(f64, f64)>,
    sensitivity: TunerSensitivity,
    min_level_dbfs: Option<f32>,
) {
    let log_min = view.freq_min.max(1.0).log10();
    let log_max = view.freq_max.max(log_min.exp().max(1.1)).log10();
    let log_span = (log_max - log_min).max(0.0001);
    // Daemon aggregates raw FFT bins into log-spaced display columns, so
    // the parabolic-interp Hz sitting between two column centers lands off
    // the visible peak. Snap marker-x to the nearest column center so the
    // vertical line always sits on a plotted sample.
    let snap_to_column = |f: f32| -> f32 {
        if freqs.len() < 2 {
            return f;
        }
        let idx = freqs.partition_point(|&v| v < f);
        if idx == 0 {
            freqs[0]
        } else if idx >= freqs.len() {
            *freqs.last().unwrap()
        } else {
            let lo = freqs[idx - 1];
            let hi = freqs[idx];
            if (f - lo).abs() <= (hi - f).abs() {
                lo
            } else {
                hi
            }
        }
    };
    let freq_to_x = |f: f32| -> Option<f32> {
        let fs = snap_to_column(f);
        if !fs.is_finite() || fs < view.freq_min || fs > view.freq_max {
            return None;
        }
        let tx = (fs.max(1.0).log10() - log_min) / log_span;
        Some(rect.left() + tx.clamp(0.0, 1.0) * rect.width())
    };

    let ch_rgba = theme::channel_color(channel);
    let base = Color32::from_rgba_unmultiplied(
        (ch_rgba[0] * 255.0) as u8,
        (ch_rgba[1] * 255.0) as u8,
        (ch_rgba[2] * 255.0) as u8,
        220,
    );
    let partial_color = Color32::from_rgba_unmultiplied(
        (ch_rgba[0] * 255.0) as u8,
        (ch_rgba[1] * 255.0) as u8,
        (ch_rgba[2] * 255.0) as u8,
        140,
    );
    let text_color = Color32::from_rgb(theme::TEXT[0], theme::TEXT[1], theme::TEXT[2]);

    // Fundamental marker: solid vertical line through the whole cell so
    // it's visible even when the (0,1) peak itself is below the dB floor.
    if let Some(cand) = cand {
        if let Some(x) = freq_to_x(cand.freq_hz as f32) {
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                egui::Stroke::new(2.0, base),
            );
            let (note, cents_off) = crate::ui::fmt::hz_to_note(cand.freq_hz);
            let label = format!("f0 {:.1} Hz  {} {:+.0}¢", cand.freq_hz, note, cents_off);
            painter.text(
                egui::pos2(x + 4.0, rect.top() + 2.0),
                egui::Align2::LEFT_TOP,
                label,
                egui::FontId::monospace(theme::GRID_LABEL_PX),
                base,
            );
        }

        // Overtone partial ticks: short verticals in the lower half of the
        // cell so they don't collide with the fundamental label at the top.
        // Partial ticks only — no per-tick text labels. At 8–11 matched
        // partials squeezed into a narrow log-scaled Hz band the labels
        // stack on top of each other and become unreadable; the corner
        // readout already enumerates the same (m,n)/Δ% data with the
        // partials sorted by ideal ratio so the user has a clean table to
        // read instead of a smear on the plot.
        for p in cand.partials.iter().skip(1) {
            if let Some(x) = freq_to_x(p.measured_hz as f32) {
                let y_top = rect.top() + rect.height() * 0.65;
                painter.line_segment(
                    [egui::pos2(x, y_top), egui::pos2(x, rect.bottom())],
                    egui::Stroke::new(1.0, partial_color),
                );
            }
        }
    }

    // Lock target: dashed vertical at the remembered target Hz plus a
    // traffic-light-coloured deviation readout so the tuner reads at a
    // glance while the user retunes a lug. Green within ±5¢ is the
    // standard acceptance band for "in tune"; yellow/red escalate from there.
    if matches!(mode, TunerMode::Locked) {
        if let (Some(target), Some(cand)) = (lock_target_hz, cand) {
            let delta_cents = crate::ui::fmt::cents(cand.freq_hz, target);
            let lock_color = if delta_cents.abs() <= 5.0 {
                Color32::from_rgb(80, 220, 120)
            } else if delta_cents.abs() <= 20.0 {
                Color32::from_rgb(230, 200, 60)
            } else {
                Color32::from_rgb(230, 80, 60)
            };
            if let Some(x) = freq_to_x(target as f32) {
                // Dashed line: paint 4 px segments with 4 px gaps so the
                // lock marker is visually distinct from the solid f0 line.
                let mut y = rect.top();
                while y < rect.bottom() {
                    let y1 = (y + 4.0).min(rect.bottom());
                    painter.line_segment(
                        [egui::pos2(x, y), egui::pos2(x, y1)],
                        egui::Stroke::new(1.0, lock_color),
                    );
                    y += 8.0;
                }
            }
            let delta_hz = cand.freq_hz - target;
            let lock_line = format!(
                "target {:.1} Hz  Δ {:+.2} Hz  {:+.1}¢",
                target, delta_hz, delta_cents,
            );
            painter.text(
                egui::pos2(rect.right() - 4.0, rect.bottom() - 4.0),
                egui::Align2::RIGHT_BOTTOM,
                lock_line,
                egui::FontId::monospace(theme::GRID_LABEL_PX),
                lock_color,
            );
        }
    }

    // Corner readout (top-right of this cell). Stacks below the peak-hold
    // corner block when peak hold is also on: the peak block uses slot 0
    // at `rect.top() + 2.0`, so anchor the tuner block further down. The
    // height offset is a fixed constant; the two overlays are each short
    // enough that an exact measurement would be overkill.
    let row_h = theme::GRID_LABEL_PX + 2.0;
    let peak_block_rows = 6.0;
    let mut y = rect.top() + 2.0 + peak_block_rows * row_h;
    if let Some(cand) = cand {
        let corner_text = crate::ui::fmt::tuner_corner_label(cand);
        let n_lines = corner_text.lines().count().max(1) as f32;
        painter.text(
            egui::pos2(rect.right() - 4.0, y),
            egui::Align2::RIGHT_TOP,
            corner_text,
            egui::FontId::monospace(theme::GRID_LABEL_PX),
            text_color,
        );
        y += n_lines * row_h;
    }
    // Range-lock indicator: one line showing the clamped search window
    // when active, so the user can tell at a glance why the tuner is
    // ignoring sub-harmonic candidates outside the band.
    if let Some((lo, hi)) = range_lock {
        painter.text(
            egui::pos2(rect.right() - 4.0, y),
            egui::Align2::RIGHT_TOP,
            format!("range-lock {:.0}-{:.0} Hz", lo, hi),
            egui::FontId::monospace(theme::GRID_LABEL_PX),
            text_color,
        );
        y += row_h;
    }
    // History block: newest last, so the list reads top-down as oldest →
    // latest. Label prefix lets the user parse it even when no live f0 is
    // showing (between triggers or after a low-confidence hit).
    if !history.is_empty() {
        painter.text(
            egui::pos2(rect.right() - 4.0, y),
            egui::Align2::RIGHT_TOP,
            "last hits:",
            egui::FontId::monospace(theme::GRID_LABEL_PX),
            text_color,
        );
        y += row_h;
        for &hz in history {
            let (note, cents_off) = crate::ui::fmt::hz_to_note(hz);
            painter.text(
                egui::pos2(rect.right() - 4.0, y),
                egui::Align2::RIGHT_TOP,
                format!("{:.1} Hz  {} {:+.0}¢", hz, note, cents_off),
                egui::FontId::monospace(theme::GRID_LABEL_PX),
                text_color,
            );
            y += row_h;
        }
    }
    // Sensitivity + min-level readout — always shown while the tuner is
    // visible so the user can see what detector settings produced the
    // candidate above (or what's blocking one).
    let level_str = match min_level_dbfs {
        Some(v) => format!("{:.0} dBFS", v),
        None => "off".to_string(),
    };
    painter.text(
        egui::pos2(rect.right() - 4.0, y),
        egui::Align2::RIGHT_TOP,
        format!("sens {}  min {}", sensitivity.label(), level_str),
        egui::FontId::monospace(theme::GRID_LABEL_PX),
        text_color,
    );
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

    use crate::data::store::{
        ChannelStore, SweepStore, TransferStore, TunerStore, VirtualChannelStore,
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
            tuner_store: TunerStore::new(),
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

#[cfg(test)]
mod ladder_tests {
    use super::*;

    #[test]
    fn step_ladder_walks_within_bounds() {
        assert_eq!(step_ladder(MONITOR_FFT_N_LADDER, 8192, 0), 8192);
        assert_eq!(step_ladder(MONITOR_FFT_N_LADDER, 8192, -1), 4096);
        assert_eq!(step_ladder(MONITOR_FFT_N_LADDER, 8192, 1), 16384);
    }

    #[test]
    fn step_ladder_clamps_at_edges() {
        assert_eq!(step_ladder(MONITOR_FFT_N_LADDER, 1024, -5), 1024);
        assert_eq!(step_ladder(MONITOR_FFT_N_LADDER, 131072, 5), 131072);
    }

    #[test]
    fn step_ladder_leaves_off_ladder_value_unchanged() {
        assert_eq!(step_ladder(MONITOR_FFT_N_LADDER, 12345, 1), 12345);
    }

    #[test]
    fn fft_n_ladder_entries_are_pow2_in_protocol_range() {
        for &n in MONITOR_FFT_N_LADDER {
            assert!(n.is_power_of_two(), "ladder entry {n} not pow2");
            assert!((256..=131_072).contains(&n), "ladder entry {n} out of protocol range");
        }
    }

    #[test]
    fn auto_interval_floors_for_small_n() {
        // Tiny windows never drop below the display-refresh floor — no
        // reason to tick faster than the eye can track.
        assert_eq!(auto_monitor_interval_ms(1024, 48_000), MONITOR_INTERVAL_FLOOR_MS);
        assert_eq!(auto_monitor_interval_ms(4096, 48_000), MONITOR_INTERVAL_FLOOR_MS);
    }

    #[test]
    fn auto_interval_ceils_for_huge_n() {
        // Huge windows cap at the "still feels live" ceiling even though
        // window/8 would suggest much slower ticks.
        assert_eq!(auto_monitor_interval_ms(32768, 48_000), MONITOR_INTERVAL_CEIL_MS);
        assert_eq!(auto_monitor_interval_ms(65536, 48_000), MONITOR_INTERVAL_CEIL_MS);
    }

    #[test]
    fn auto_interval_scales_with_sample_rate() {
        // Double the sample rate halves the window duration → tick shrinks
        // (until it hits the floor).
        let at_48k = auto_monitor_interval_ms(16384, 48_000);
        let at_96k = auto_monitor_interval_ms(16384, 96_000);
        assert!(at_96k <= at_48k, "{at_96k} ms should be ≤ {at_48k} ms");
    }

    #[test]
    fn auto_interval_stays_within_clamp_band() {
        for &n in MONITOR_FFT_N_LADDER {
            for &sr in &[44_100u32, 48_000, 88_200, 96_000, 192_000] {
                let tick = auto_monitor_interval_ms(n, sr);
                assert!(
                    (MONITOR_INTERVAL_FLOOR_MS..=MONITOR_INTERVAL_CEIL_MS).contains(&tick),
                    "tick {tick} outside band for N={n} sr={sr}"
                );
            }
        }
    }

    #[test]
    fn median_of_odd_slice_picks_middle() {
        assert_eq!(median_f32(&[0.09, 0.11, 0.10]), Some(0.10));
    }

    #[test]
    fn median_of_even_slice_averages_middle_pair() {
        let m = median_f32(&[0.1, 0.2, 0.3, 0.4]).unwrap();
        assert!((m - 0.25).abs() < 1e-6);
    }

    #[test]
    fn median_rejects_nan_samples() {
        // A single spurious NaN in the ring shouldn't poison the estimate.
        let m = median_f32(&[0.10, f32::NAN, 0.11, 0.10]).unwrap();
        assert!((m - 0.10).abs() < 1e-6);
    }

    #[test]
    fn median_empty_returns_none() {
        assert_eq!(median_f32(&[]), None);
    }

    #[test]
    fn median_absorbs_single_stall() {
        // Producer running at ~10 Hz with one 500 ms stall — the median
        // should stay near 0.1 s instead of jumping halfway to 0.5 s the way
        // a 15% EMA would.
        let mut samples = vec![0.10_f32; 15];
        samples.push(0.50);
        let m = median_f32(&samples).unwrap();
        assert!((m - 0.10).abs() < 0.01, "median {m} pulled too far by stall");
    }
}

