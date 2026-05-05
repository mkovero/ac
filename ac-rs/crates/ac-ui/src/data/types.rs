use std::sync::Arc;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct SpectrumFrame {
    pub freqs: Vec<f32>,
    pub spectrum: Vec<f32>,
    #[serde(default)]
    pub freq_hz: f32,
    #[serde(default)]
    pub fundamental_dbfs: f32,
    #[serde(default)]
    pub thd_pct: f32,
    #[serde(default)]
    pub thdn_pct: f32,
    pub in_dbu: Option<f32>,
    /// Daemon-supplied additive offset for dBFS → dBu conversion. `None`
    /// when the channel is uncalibrated. `Some(off)` lets the UI compute
    /// `dbu = dbfs + off` for any cursor position without the FFT bin
    /// reading needing a live cal lookup; dBV is then `dbu_to_dbv(dbu)`.
    #[serde(default)]
    pub dbu_offset_db: Option<f32>,
    /// Parabolic-interpolated peaks `[(freq_hz, dbfs)]` from the linear
    /// FFT, ordered strongest-first, up to ~64. Used by the cursor
    /// readout: when the cursor freq is within `±0.5·bin_hz` of a peak
    /// freq, the readout snaps to the interpolated dBFS — accurate to
    /// ≤0.4 dB across the full ±0.5-bin offset range, vs. the raw
    /// scalloped bin which can be off by up to 1.42 dB on a Hann window.
    #[serde(default)]
    pub peaks: Vec<[f32; 2]>,
    /// Daemon-supplied additive offset for dBFS → dB SPL conversion. `None`
    /// keeps the readouts in dBFS; `Some(off)` makes the UI render `dB SPL`
    /// using `dbspl = dbfs + off`.
    #[serde(default)]
    pub spl_offset_db: Option<f32>,
    /// Daemon mic-correction state for the channel that produced this
    /// frame. `"none"` = no curve loaded; `"on"` = curve loaded and
    /// applied; `"off"` = curve loaded but the global toggle is off.
    /// Only used at the type-tag level — the UI doesn't read the curve
    /// itself (correction is already applied by the daemon).
    #[serde(default)]
    pub mic_correction: Option<String>,
    pub sr: u32,
    #[serde(default)]
    pub clipping: bool,
    #[serde(default)]
    pub xruns: u32,
    #[serde(default)]
    pub channel: Option<u32>,
    // Retained: wire-protocol field populated by receiver; no UI consumer yet.
    #[serde(default)]
    #[allow(dead_code)]
    pub n_channels: Option<u32>,
    /// Set by the producer (receiver / synthetic), monotonically increasing per
    /// channel. Lets the consumer detect fresh frames vs. re-reads of the same
    /// data so the waterfall view can advance one row per real new sample. Zero
    /// means "no frame yet"; producers always emit ≥ 1.
    #[serde(default, skip)]
    pub frame_id: u64,
    /// Populated by the receiver when the frame is a `fractional_octave_leq`
    /// sidecar (see ZMQ.md § time-integration). `spectrum` then carries the
    /// integrated per-band dBFS instead of the raw aggregation. `None` for
    /// every other frame type. The value is the Leq accumulator duration
    /// in seconds; `NaN` (not `None`) signals the frame is integrated but
    /// the mode is fast/slow (duration is irrelevant). Overlay reads this
    /// so the user knows the trace is integrated and for how long.
    #[serde(default, skip)]
    pub leq_duration_s: Option<f64>,
}

impl Default for SpectrumFrame {
    fn default() -> Self {
        Self {
            freqs: Vec::new(),
            spectrum: Vec::new(),
            freq_hz: 0.0,
            fundamental_dbfs: -140.0,
            thd_pct: 0.0,
            thdn_pct: 0.0,
            in_dbu: None,
            dbu_offset_db: None,
            peaks: Vec::new(),
            spl_offset_db: None,
            mic_correction: None,
            sr: 48000,
            clipping: false,
            xruns: 0,
            channel: None,
            n_channels: None,
            frame_id: 0,
            leq_duration_s: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DisplayFrame {
    pub spectrum: Arc<Vec<f32>>,
    pub freqs: Arc<Vec<f32>>,
    pub meta: FrameMeta,
    /// Populated by the store on the first read after a fresh producer frame.
    /// `None` on re-reads of the same frame so the waterfall renderer scrolls
    /// at the rate of incoming data, not at the rate of redraws.
    pub new_row: Option<Arc<Vec<f32>>>,
}

#[derive(Debug, Clone)]
pub struct FrameMeta {
    // The single-tone fields (freq_hz, fundamental_dbfs, thd_pct, thdn_pct)
    // are populated by the daemon's `analyze()` path and kept here for
    // future per-frame inspection / export, but the live monitor UI no
    // longer displays them — THD is meaningless on broadband signals and
    // the argmax is already visible via the peak-hold marker and the new
    // broadband readout. See `ui::fmt::broadband_stats`. `xruns` is wire-
    // protocol only (not yet displayed).
    #[allow(dead_code)]
    pub freq_hz: f32,
    #[allow(dead_code)]
    pub fundamental_dbfs: f32,
    #[allow(dead_code)]
    pub thd_pct: f32,
    #[allow(dead_code)]
    pub thdn_pct: f32,
    pub in_dbu: Option<f32>,
    /// Per-channel dBFS → dBu offset (`dbu = dbfs + dbu_offset_db`). `None`
    /// when uncalibrated. Mirrored from the spectrum frame; consumed by the
    /// cursor-driven footer readout to render dBFS / dBu / dBV.
    pub dbu_offset_db: Option<f32>,
    /// Parabolic-interpolated peaks `[(freq_hz, dbfs)]` from the daemon's
    /// linear FFT, strongest-first. Cursor-driven footer snaps to the
    /// nearest peak (within `±0.5·bin_hz`) for scallop-corrected dBFS.
    pub peaks: Arc<Vec<[f32; 2]>>,
    /// Additive offset (`dB SPL = dBFS + spl_offset_db`) populated by the
    /// daemon when the channel has been pistonphone-calibrated. `None`
    /// preserves the dBFS readout convention.
    pub spl_offset_db: Option<f32>,
    /// Daemon mic-correction state. `None` / `Some("none")` → no curve;
    /// `Some("on")` → curve loaded and applied; `Some("off")` → curve
    /// loaded but the global toggle is off. Drives the top-right tag
    /// chip and the bottom-left readout's `[mic-corrected]` suffix.
    pub mic_correction: Option<String>,
    pub sr: u32,
    pub clipping: bool,
    #[allow(dead_code)]
    pub xruns: u32,
    /// Non-`None` on frames where the receiver replaced `spectrum` with
    /// the `fractional_octave_leq` sidecar's integrated bands. `NaN` when
    /// the mode is fast/slow (no unbounded duration); a real value when
    /// the mode is Leq. Overlay uses this to flag the trace as integrated.
    pub leq_duration_s: Option<f64>,
}

/// Morlet CWT waterfall column published by the daemon on the `data` topic
/// with `type == "cwt"`. Shape differs from `SpectrumFrame`: magnitudes are
/// already in dBFS and frequencies are log-spaced, so the receiver converts
/// this into a `SpectrumFrame` without the usual linear→dB step before
/// writing to the display triple-buffer.
#[derive(Debug, Clone, Deserialize)]
pub struct CwtFrame {
    pub magnitudes:  Vec<f32>,
    pub frequencies: Vec<f32>,
    pub sr:          u32,
    #[serde(default)]
    pub channel:     Option<u32>,
    #[serde(default)]
    pub n_channels:  Option<u32>,
    #[serde(default)]
    pub spl_offset_db: Option<f32>,
    #[serde(default)]
    pub mic_correction: Option<String>,
}

/// One H1 transfer function estimate from the daemon. Arrives on the `data`
/// topic with `type == "transfer_stream"` and replaces whatever the UI was
/// displaying — no averaging in the UI layer, the Welch averaging already
/// happens daemon-side.
#[derive(Debug, Clone, Deserialize)]
pub struct TransferFrame {
    pub freqs:         Vec<f32>,
    pub magnitude_db:  Vec<f32>,
    pub phase_deg:     Vec<f32>,
    pub coherence:     Vec<f32>,
    /// Complex H(ω) — real part. `unified.md` Phase 3. `serde(default)`
    /// for backward compatibility — older daemon builds without the
    /// field still produce parseable frames; views that consume re/im
    /// (Nyquist, IR) check for `!is_empty()` before drawing.
    #[serde(default)]
    pub re: Vec<f32>,
    /// Complex H(ω) — imaginary part. Parallel to `re`.
    #[serde(default)]
    pub im: Vec<f32>,
    pub delay_samples: i64,
    pub delay_ms:      f32,
    pub meas_channel:  u32,
    pub ref_channel:   u32,
    pub sr:            u32,
}

/// Identifier for a virtual transfer channel: pair of real channel indices
/// (meas, ref_ch) as they appear in the daemon's capture channel ordering.
/// Lets the UI address virtual channels independently of their position in
/// the virtual-channel list and lets the receiver route incoming
/// `TransferFrame`s to the right slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransferPair {
    pub meas: u32,
    pub ref_ch: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepKind {
    Frequency,
    Level,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SweepPoint {
    pub n: u32,
    pub drive_db: f32,
    pub thd_pct: f32,
    pub thdn_pct: f32,
    pub fundamental_hz: f32,
    pub fundamental_dbfs: f32,
    #[serde(default)]
    pub harmonic_levels: Vec<[f32; 2]>,
    #[serde(default)]
    pub spectrum: Vec<f32>,
    #[serde(default)]
    pub freqs: Vec<f32>,
    #[serde(default)]
    pub clipping: bool,
    pub out_dbu: Option<f32>,
    pub in_dbu: Option<f32>,
    pub gain_db: Option<f32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SweepDone {
    pub cmd: String,
    #[serde(default)]
    pub n_points: u32,
    #[serde(default)]
    pub xruns: u32,
}

/// Per-channel BS.1770-5 / EBU R128 meter readout, derived from the
/// daemon's `measurement/loudness` sidecar frame. Optional fields are
/// `None` before enough audio has been accumulated for a meaningful
/// value (e.g. `momentary_lkfs` needs ≥ 400 ms, `short_term_lkfs` ≥ 3 s,
/// `integrated_lkfs` / `true_peak_dbtp` become finite once the first
/// non-silent audio has passed the gate).
#[derive(Debug, Clone, Copy, Default)]
pub struct LoudnessReadout {
    pub momentary_lkfs: Option<f64>,
    pub short_term_lkfs: Option<f64>,
    pub integrated_lkfs: Option<f64>,
    pub lra_lu: f64,
    pub true_peak_dbtp: Option<f64>,
    pub gated_duration_s: f64,
    /// Mirrors the spectrum frame's offset for the same channel. When
    /// set, M/S/I render as K-weighted dB SPL (`Mk`/`Sk`/`Ik`) and the
    /// true-peak line becomes `Lpk(K) X dB SPL`. The R128 PASS/WARN/FAIL
    /// badge stays anchored on raw integrated LKFS — its target
    /// (`-23 LKFS`) is independent of the absolute SPL reference.
    pub spl_offset_db: Option<f64>,
}

impl From<&SpectrumFrame> for FrameMeta {
    fn from(f: &SpectrumFrame) -> Self {
        Self {
            freq_hz: f.freq_hz,
            fundamental_dbfs: f.fundamental_dbfs,
            thd_pct: f.thd_pct,
            thdn_pct: f.thdn_pct,
            in_dbu: f.in_dbu,
            dbu_offset_db: f.dbu_offset_db,
            peaks: Arc::new(f.peaks.clone()),
            spl_offset_db: f.spl_offset_db,
            mic_correction: f.mic_correction.clone(),
            sr: f.sr,
            clipping: f.clipping,
            xruns: f.xruns,
            leq_duration_s: f.leq_duration_s,
        }
    }
}

/// Goniometer source-state, computed each frame at the dispatch site
/// and surfaced in the overlay caption. `unified.md` Phase 0b — lets
/// the user tell at a glance whether the figure they're looking at is
/// real audio or the synthetic fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StereoStatus {
    /// Real wire-fed audio for both channels of a stereo pair.
    Real { l: u32, r: u32 },
    /// `active_channel` is in the monitor set but `+1` isn't.
    NoSecondChannel { l: u32 },
    /// Both channels are in the set but no recent scope frames have
    /// arrived yet (cold start, or the daemon stopped streaming).
    NotStreamingYet { l: u32, r: u32 },
    /// No daemon source — synthetic mode or pre-connect.
    NoAudio,
}

impl Default for StereoStatus {
    fn default() -> Self {
        Self::NoAudio
    }
}

/// `visualize/scope` wire frame — raw f32 audio samples for one channel
/// per `monitor_spectrum` tick (`unified.md` Phase 0b, resolves §9 OQ7).
/// Consumed by the Goniometer trajectory view; the `frame_idx` field
/// synchronizes L+R channels across the same tick so the UI can pair
/// them without relying on receive order.
#[derive(Debug, Clone, Deserialize)]
pub struct ScopeFrame {
    pub channel: u32,
    pub sr: u32,
    pub frame_idx: u64,
    pub samples: Vec<f32>,
    #[serde(default)]
    pub n_channels: Option<u32>,
}

/// `visualize/ir` wire frame — daemon-side IFFT of H₁(ω) into a
/// time-domain h(t) array. Centred (`t = 0` at the middle of
/// `samples`); `t_origin_ms` is negative and `dt_ms` is the per-
/// sample stride. `unified.md` Phase 4b.
#[derive(Debug, Clone, Deserialize)]
pub struct IrFrame {
    pub samples: Vec<f32>,
    pub sr: u32,
    pub dt_ms: f64,
    pub t_origin_ms: f64,
    pub ref_channel: u32,
    pub meas_channel: u32,
    #[serde(default)]
    pub stride: u32,
    #[serde(default)]
    pub delay_samples: i64,
    #[serde(default)]
    pub delay_ms: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    Grid,
    Single,
    /// Stacks only the user-selected channels in one rect (overlay-style)
    /// with a corner legend. Hidden until the user toggles selections via
    /// Space — the empty case shows a "press Space to select" hint.
    Compare,
    /// Sweep measurement view (THD/THD+N vs freq or level). Passive — the CLI
    /// manages the daemon command; the UI just accumulates `sweep_point` frames.
    /// Only entered via `--mode sweep_frequency|sweep_level`, not the L-key cycle.
    Sweep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Spectrum,
    Waterfall,
    /// Ember substrate driven by a synthetic 1 kHz sine on a strip-chart
    /// scope. Phase 0a validation. See `unified.md`.
    Scope,
    /// Ember substrate driven by the active channel's `SpectrumFrame`s.
    /// Static (no scroll) with long persistence so successive measurements
    /// fade-blend, giving the free diff workflow promised in unified.md §5.
    SpectrumEmber,
    /// 2D stereo phase scope. Same 1 kHz carrier on both with a 0.3 Hz
    /// phase drift on R for the synthetic source; real-audio path uses
    /// `active_channel` as L and `active_channel + 1` as R via Phase 0b
    /// `visualize/scope` frames. Phase 1 of unified.md.
    Goniometer,
    /// Input/Output transfer Lissajous — classic analog-bench
    /// distortion-shape view. X = active_channel reference signal,
    /// Y = active_channel + 1 DUT output (raw, no M/S rotation). A
    /// linear pass-through DUT traces a diagonal line at slope = gain;
    /// nonlinear DUTs deform the line into shapes that map directly
    /// to distortion type (soft compression → S-curve, hard clipping
    /// → flat tops, asymmetric class-A → asymmetric line about
    /// origin, …). Phase 1.5 of unified.md.
    IoTransfer,
    /// Bode magnitude on the ember substrate. Reads
    /// `(active_channel, active_channel + 1)` as a transfer pair from
    /// `VirtualChannelStore`; auto-registers the pair on view-entry
    /// so the daemon's transfer worker starts producing TransferFrames.
    /// Long τ_p (~4 s) so successive measurements fade-blend → free
    /// before/after diff workflow without explicit overlay logic.
    /// Phase 2 of unified.md.
    BodeMag,
    /// Coherence γ²(f) on the ember substrate. Same pair convention
    /// as `BodeMag` (auto-registers `active + active+1`). Y axis is
    /// dimensionless [0, 1] — visually obvious where the FRF is
    /// trustworthy (γ² ≈ 1) vs unreliable (γ² < 0.8). Phase 2.
    Coherence,
    /// Bode phase φ(f) on the ember substrate. Wrapped to [-180°,
    /// +180°] (same convention as the daemon's TransferFrame).
    /// Same auto-pair convention as BodeMag. Phase 2.5 of unified.md.
    BodePhase,
    /// Group delay τ_g(f) = −dφ/dω in milliseconds. Computed from a
    /// finite-difference derivative of the *unwrapped* phase array
    /// — wrapped phase would produce ±360°/Δf spikes wherever the
    /// underlying smooth phase wrapped through ±180°. Same auto-
    /// pair convention as BodeMag. Phase 2.5 of unified.md.
    GroupDelay,
    /// Nyquist locus — parametric (Re H, Im H) curve in the complex
    /// plane, parameterised by frequency. Consumes the re/im fields
    /// added in Phase 3. Auto-gain scales the curve to fit the cell;
    /// a faint unit circle is drawn for visual reference (gain = 1
    /// boundary). Same auto-pair convention as BodeMag. Phase 4 of
    /// unified.md.
    Nyquist,
    /// Impulse response h(t) — daemon-side IFFT of the H₁(ω) estimate
    /// shipped as a `visualize/ir` sidecar to `transfer_stream`. Time
    /// on x (centred so t = 0 is mid-cell), amplitude on y with
    /// auto-gain. Same auto-pair convention as Nyquist / Bode. For
    /// calibrated measurement-grade IR use the Tier 1 sweep path
    /// instead — this view is the live-bench Tier 2 visualisation.
    /// Phase 4b of unified.md.
    Ir,
}

/// Per-cell zoom/pan state. Split out of `DisplayConfig` so mouse interactions
/// can target the hovered cell independently without broadcasting to the rest.
#[derive(Debug, Clone, Copy)]
pub struct CellView {
    pub freq_min: f32,
    pub freq_max: f32,
    pub db_min:   f32,
    pub db_max:   f32,
    /// Waterfall-only: how many history rows from the newest one are stretched
    /// across the cell height. `ROWS_PER_CHANNEL` = show the whole ring (full
    /// time depth); smaller = zoom into the recent past. Ignored in Spectrum.
    pub rows_visible: u32,
    /// Fractional counterpart of `rows_visible`. Scroll zoom steps this by a
    /// small ratio (e.g. ×1.1) so the time axis grows/shrinks continuously
    /// instead of in integer chunks; `rows_visible` is derived via round. Kept
    /// separately so we don't accumulate rounding error across many scroll
    /// ticks. Time-axis labels read this value for smooth interpolation
    /// between tick positions.
    pub rows_visible_f: f32,
}

impl Default for CellView {
    fn default() -> Self {
        Self {
            freq_min: crate::theme::DEFAULT_FREQ_MIN,
            freq_max: crate::theme::DEFAULT_FREQ_MAX,
            db_min:   crate::theme::DEFAULT_DB_MIN,
            db_max:   crate::theme::DEFAULT_DB_MAX,
            rows_visible: crate::render::waterfall::ROWS_PER_CHANNEL,
            rows_visible_f: crate::render::waterfall::ROWS_PER_CHANNEL as f32,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DisplayConfig {
    pub averaging_alpha: f32,
    pub frozen: bool,
    pub layout: LayoutMode,
    pub view_mode: ViewMode,
    pub active_channel: usize,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            averaging_alpha: 0.20,
            frozen: false,
            layout: LayoutMode::Grid,
            view_mode: ViewMode::Spectrum,
            active_channel: 0,
        }
    }
}
