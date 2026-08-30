//! `monitor_spectrum` — live per-channel spectrum/CWT feed.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::measurement::loudness::LoudnessState;
use ac_core::shared::calibration::Calibration;
use ac_core::shared::mic_curve_filter::{MicCurveFir, DEFAULT_N_TAPS};
use ac_core::visualize::time_integration::{EmaIntegrator, LeqIntegrator, TAU_FAST_S, TAU_SLOW_S};
use ac_core::visualize::weighting_curves::WeightingCurve;

use crate::audio::make_engine;
use crate::server::{MonitorParams, ServerState};

use super::super::{busy_guard, cfg_guard, resolve_input, send_pub, spawn_worker};

/// Emit a `measurement/loudness` sidecar frame for one channel. Kept
/// out of the worker body so the FFT / CWT / CQT / reassigned analysis
/// paths can share it. `spl_offset_db` mirrors the offset stamped on
/// the spectrum frame for the same channel; `mic_correction` reflects
/// whether the LKFS values were computed on samples that had already
/// passed through the per-channel mic-curve FIR (#104) — `"on"` means
/// LKFS / LRA / dBTP report the *corrected* (true acoustic) levels.
#[allow(clippy::too_many_arguments)]
fn emit_loudness_frame(
    pub_tx: &crossbeam_channel::Sender<Vec<u8>>,
    channel: u32,
    n_channels: u32,
    sr: u32,
    loudness: &LoudnessState,
    spl_offset_db: Option<f64>,
    mic_correction: &str,
    ts_ns: u64,
    xruns: u32,
) {
    let frame = json!({
        "type":             "measurement/loudness",
        "cmd":              "monitor_spectrum",
        "channel":          channel,
        "n_channels":       n_channels,
        "sr":               sr,
        "momentary_lkfs":   json_finite(loudness.momentary()),
        "short_term_lkfs":  json_finite(loudness.short_term()),
        "integrated_lkfs":  json_finite(loudness.integrated()),
        "lra_lu":           loudness.loudness_range(),
        "true_peak_dbtp":   json_finite(loudness.true_peak_dbtp()),
        "gated_duration_s": loudness.gated_duration_s(),
        "spl_offset_db":    spl_offset_db,
        "mic_correction":   mic_correction,
        "timestamp":        ts_ns,
        "xruns":            xruns,
    });
    send_pub(pub_tx, "data", &frame);
}

/// Cap on `samples` per scope frame so the wire payload stays bounded
/// regardless of sample rate / tick budget. 2048 f32 = 8 KB per channel
/// per tick; at 192 kHz × 200 ms the per-tick capture is ~38 k samples,
/// so we truncate to the newest 2048 (≈10 ms @ 192 kHz, plenty for
/// trajectory rendering at 60 fps). Visible aliasing is the failure mode
/// to watch for and would prompt a v2 decimator.
const SCOPE_MAX_SAMPLES: usize = 2048;

/// Emit a `visualize/scope` sidecar frame for one channel — raw f32
/// samples (no voltage / SPL / mic-curve calibration applied), used by
/// the UI's Goniometer / PhaseScope3D trajectory views (`unified.md`
/// Phase 0b / OQ7). `frame_idx` is the per-tick monotonic counter
/// shared across both channels of a stereo pair; the UI uses it to
/// confirm L and R came from the same capture before pairing them.
#[allow(clippy::too_many_arguments)]
fn emit_scope_frame(
    pub_tx: &crossbeam_channel::Sender<Vec<u8>>,
    channel: u32,
    n_channels: u32,
    sr: u32,
    samples: &[f32],
    frame_idx: u64,
    ts_ns: u64,
    xruns: u32,
) {
    let tail = if samples.len() > SCOPE_MAX_SAMPLES {
        &samples[samples.len() - SCOPE_MAX_SAMPLES..]
    } else {
        samples
    };
    let frame = json!({
        "type":       "visualize/scope",
        "cmd":        "monitor_spectrum",
        "channel":    channel,
        "n_channels": n_channels,
        "sr":         sr,
        "frame_idx":  frame_idx,
        "samples":    tail,
        "timestamp":  ts_ns,
        "xruns":      xruns,
    });
    send_pub(pub_tx, "data", &frame);
}

/// Samples to pull from the capture ring per tick, per channel.
///
/// **This must never be less than what arrives in the same wall-clock
/// interval.** The ring fills at `sr` samples/second regardless of what the
/// analysis does with them; if a tick drains less than `per_ch_budget · sr`,
/// the shortfall accumulates in the JACK ring on every single tick and the
/// displayed spectrum falls progressively further behind realtime — for as
/// long as the monitor runs.
///
/// That is exactly what issue #208 was. The budget used to be
/// `clamp(128, fft_n)`, and the upper clamp bit whenever
/// `per_ch_budget · sr > fft_n` — which is the *default* configuration:
/// `interval` 0.2 s at 96 kHz is 19 200 samples arriving per tick against an
/// `fft_n` of 8192 drained, so the monitor consumed audio at 42.7% of
/// realtime. Measured on hardware: 500 ms bursts emitted 10 s apart appeared
/// in the published frames 23.2 s apart, at lags of 12.3 s, 25.3 s and
/// 38.8 s — growing without bound. The reported symptom (a stimulus
/// reappearing seconds later, then repeating with nothing happening in the
/// room) was the daemon playing out that backlog.
///
/// It bit at 48 kHz too — 9600 arriving against 8192 drained, a lag growing
/// at 0.147 s per second, so 3–5 s of lag after 20–34 s of monitoring. That
/// matches the originally reported "three to five seconds" and is why the
/// symptom survived the `ac-ui` → `ac-view` rewrite: it was never in either
/// UI.
///
/// There is deliberately **no upper bound**. Reading more than `fft_n` is
/// harmless — the sliding ring pops the excess straight back off — whereas
/// any ceiling below the arrival rate reintroduces the defect. The 128-sample
/// floor stays so a very short `interval` still gives JACK something to hand
/// back.
fn capture_budget_samples(per_ch_budget_secs: f64, sr: u32) -> usize {
    ((per_ch_budget_secs * sr as f64) as usize).max(128)
}

/// Push captured samples to the loudness state, optionally filtering
/// through the per-channel mic-curve FIR first (#104). When the FIR
/// is bypassed (toggle off, or no curve loaded), pushes the raw
/// samples — preserves the existing dBTP / LKFS path so a channel
/// without a curve sees no behavioural change.
///
/// The FIR's delay-line state persists across calls so block boundaries
/// are seamless. Toggling the global enable flag mid-stream causes a
/// brief discontinuity (one FIR-length of stale history); document'd
/// in the wire frame's `mic_correction` field flipping `"on"` → `"off"`.
fn push_loudness_with_optional_fir(
    loudness: &mut LoudnessState,
    fir: &mut Option<MicCurveFir>,
    mic_corr_enabled: bool,
    samples: &[f32],
) {
    if let (true, Some(fir)) = (mic_corr_enabled, fir.as_mut()) {
        let mut filtered = samples.to_vec();
        fir.process_inplace(&mut filtered);
        let _ = loudness.push(&[&filtered]);
    } else {
        let _ = loudness.push(&[samples]);
    }
}

/// LF-band FFT overlap fraction (#173). The previous un-overlapped block
/// cadence (~0.7-1.4 s depending on `lf_fft_n`) recomputed the LF spectrum
/// from entirely fresh samples each time; under broadband material each
/// recompute is an independent chi-squared(2) draw per bin (~5.6 dB sigma,
/// matching #173's measurement), so the LF half of the display lurched
/// while the HF band (refreshed every tick) glided smoothly. 90% overlap
/// keeps the 65536-point FFT's frequency resolution (needed to split
/// closely-spaced LF tones, #142) while recomputing every
/// `(1 - LF_OVERLAP) * lf_fft_n / sr` seconds instead of the full block —
/// ~136 ms at the N=65536/48kHz default, under the ~170 ms cadence target.
const LF_OVERLAP: f64 = 0.9;

/// Power-domain EMA time constant applied to each newly-recomputed LF
/// spectrum (#173), reusing `EmaIntegrator`'s existing fast/slow/Leq
/// convention at per-bin scale. Tuned in
/// `ac_core::visualize::time_integration::tests::lf_ema_brings_variance_within_2x_of_hf_target`
/// to bring the raw ~5.6 dB chi-squared sigma down to ~2.2-2.4 dB, within
/// 2x of the HF band's measured 0.7-2.4 dB range, without smearing tone
/// levels (EMA is power-domain, unbiased at steady state).
const LF_AVG_TAU_S: f64 = 0.25;

/// Wall-clock nanoseconds since the epoch, for a frame's `timestamp`.
/// Returns 0 if the clock is before the epoch — a frame with a bogus
/// timestamp still beats dropping the frame.
fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Unwrap a capture result, publishing a `capture error on chN` frame and
/// returning `None` when the engine failed.
///
/// A capture failure is terminal for the worker — there is no partial
/// buffer to fall back on — so every caller must `return` on `None`. The
/// `let ... else { return; }` is left at the call site rather than hidden
/// in here, so the control flow stays visible where it happens.
fn capture_or_report<T, E: std::fmt::Display>(
    result: Result<T, E>,
    pub_tx: &crossbeam_channel::Sender<Vec<u8>>,
    channel: u32,
) -> Option<T> {
    match result {
        Ok(v) => Some(v),
        Err(e) => {
            send_pub(
                pub_tx,
                "error",
                &json!({
                    "cmd":     "monitor_spectrum",
                    "message": format!("capture error on ch{channel}: {e}"),
                }),
            );
            None
        }
    }
}

/// Wire identity and per-tick values that every frame in one channel
/// iteration shares. Bundled so the ring-path helpers below stay under a
/// readable argument count.
struct TickCtx<'a> {
    pub_tx: &'a crossbeam_channel::Sender<Vec<u8>>,
    n_channels: u32,
    sr: u32,
    /// Per-tick monotonic counter; identical for every channel of a tick
    /// so the UI can pair L and R scope frames.
    frame_idx: u64,
    /// Tick-wide capture timestamp, for scope frames only.
    tick_ts_ns: u64,
    /// Snapshot of the global mic-correction toggle, read once per tick.
    mic_corr_enabled: bool,
    /// Capture-block duration for the ring-buffered modes, already
    /// clamped to [16 ms, 100 ms].
    tick_secs: f64,
}

/// Minimum CWT ring fill before a column is emitted. `morlet_cwt_into`
/// asserts on fewer than 256 samples.
const CWT_MIN_FILL: usize = 256;

/// Which of a channel's rings a ring-buffered mode fills.
#[derive(Clone, Copy)]
enum RingKind {
    Cwt,
    Cqt,
    Reassigned,
}

/// Outcome of one ring-buffered capture.
enum RingTick {
    /// The ring holds enough samples for a valid column. Carries the
    /// engine's cumulative xrun count, read once here so the frames built
    /// from this capture all quote the same number.
    Ready { xruns: u32 },
    /// Not enough samples yet — the caller should skip this channel.
    NotReady,
    /// Capture failed and has been reported; the caller must return.
    Failed,
}

/// Capture one paced block for `ch`, push it through the loudness meter,
/// emit its scope frame, and append it to the mode's ring, trimmed to
/// `ring_cap` from the front.
///
/// This is the half of a CWT / CQT / reassigned tick that does not depend
/// on which transform runs: the three modes differ only in which ring
/// they fill, how full it must be, and what they then compute from it.
fn capture_into_ring(
    eng: &mut dyn crate::audio::AudioEngine,
    ch: &mut ChannelState,
    ctx: &TickCtx,
    kind: RingKind,
    ring_cap: usize,
    min_fill: usize,
) -> RingTick {
    // Pace the capture tick to the UI's requested interval, clamped to
    // [16 ms, 100 ms] by the caller. Pre-#109 this was hardcoded 20 ms
    // regardless of `--max-fps`, so CWT emitted at 50 fps even when the
    // UI was capped at 30 — wasted work on both sides.
    let tick_secs = ctx.tick_secs;
    let Some(samples) = capture_or_report(eng.capture_block(tick_secs), ctx.pub_tx, ch.channel)
    else {
        return RingTick::Failed;
    };
    // `eng.xruns()` is already a cumulative count for this engine session
    // (see `jack_backend.rs`'s `SharedState::xruns`), so this assigns
    // rather than accumulates — summing it across per-tick, per-channel
    // reads would multiply a handful of real xruns into thousands over a
    // long monitor session.
    let xruns = eng.xruns();
    // Feed the raw capture into the loudness meter before any downstream
    // consumer touches it.
    push_loudness_with_optional_fir(
        &mut ch.loudness,
        &mut ch.loudness_fir,
        ctx.mic_corr_enabled,
        &samples,
    );
    emit_scope_frame(
        ctx.pub_tx,
        ch.channel,
        ctx.n_channels,
        ctx.sr,
        &samples,
        ctx.frame_idx,
        ctx.tick_ts_ns,
        xruns,
    );
    let ring = ch.ring_mut(kind);
    ring.extend(samples.iter());
    while ring.len() > ring_cap {
        ring.pop_front();
    }
    if ring.len() < min_fill {
        return RingTick::NotReady;
    }
    RingTick::Ready { xruns }
}

/// Periodic timing line for a ring-buffered transform — one line every
/// 50 ticks so a slow transform is visible without flooding the log.
fn log_transform_time(
    counter: &mut u32,
    label: &str,
    channel: u32,
    t0: std::time::Instant,
    ring_len: usize,
    n_out: usize,
) {
    *counter += 1;
    if *counter % 50 == 1 {
        eprintln!(
            "{label} ch{channel}: {:.1}ms, ring={ring_len}, out={n_out}",
            t0.elapsed().as_secs_f64() * 1000.0,
        );
    }
}

/// Mic-correct `mags` in place, then emit the mode's `visualize/*` frame
/// and the channel's `measurement/loudness` sidecar.
///
/// Returns the frame timestamp and mic-correction tag, which the CWT
/// path reuses for its fractional-octave frames so every frame built from
/// one column agrees.
fn emit_ring_frames(
    ch: &ChannelState,
    ctx: &TickCtx,
    frame_type: &str,
    freqs: &[f32],
    mags: &mut [f32],
    extra: &[(&str, Value)],
    xruns: u32,
) -> (u64, &'static str) {
    if ctx.mic_corr_enabled {
        if let Some(curve) = &ch.mic_curve {
            apply_mic_curve_inplace_f32(curve, freqs, mags);
        }
    }
    let mc_tag = mic_correction_tag(ch.mic_curve.is_some(), ctx.mic_corr_enabled);
    let ts_ns = now_ns();
    let mags: &[f32] = mags;
    let mut frame = json!({
        "type":           frame_type,
        "cmd":            "monitor_spectrum",
        "channel":        ch.channel,
        "n_channels":     ctx.n_channels,
        "sr":             ctx.sr,
        "magnitudes":     mags,
        "frequencies":    freqs,
        "spl_offset_db":  ch.spl_offset,
        "mic_correction": mc_tag,
        "timestamp":      ts_ns,
        "xruns":          xruns,
    });
    if let Some(obj) = frame.as_object_mut() {
        for (k, v) in extra {
            obj.insert((*k).to_string(), v.clone());
        }
    }
    send_pub(ctx.pub_tx, "data", &frame);
    emit_loudness_frame(
        ctx.pub_tx,
        ch.channel,
        ctx.n_channels,
        ctx.sr,
        &ch.loudness,
        ch.spl_offset,
        mc_tag,
        ts_ns,
        xruns,
    );
    (ts_ns, mc_tag)
}

/// Per-bin dBFS → dBu conversion offset:
///   analog_vrms = sample_peak × cal_in / sqrt(2)   (sine assumption)
///   dBu = dbfs_peak + 20·log10(cal_in / (sqrt(2)·dbu_ref))
///
/// The UI overlays this on hover readouts so any cursor position shows
/// dBFS / dBu / dBV without a round-trip. `None` when the channel has no
/// input voltage calibration.
fn dbu_offset_db(cal: Option<&Calibration>) -> Option<f64> {
    cal.and_then(|c| c.vrms_at_0dbfs_in).map(|v| {
        20.0 * (v / (std::f64::consts::SQRT_2 * ac_core::shared::conversions::get_dbu_ref()))
            .log10()
    })
}

/// Aggregate a linear half-spectrum onto the wire's log-spaced columns,
/// splicing the long-N LF spectrum in below `crossover_hz` when one is
/// cached (#142), then apply the channel's mic curve.
///
/// Returns `(columns, column centre frequencies)`. Both the THD path and
/// the fallback path below feed this; they differ only in where their
/// input spectrum came from.
fn spectrum_columns(
    spec: &[f64],
    lf: Option<&[f64]>,
    sr: u32,
    crossover_hz: f32,
    ch: &ChannelState,
    mic_corr_enabled: bool,
) -> (Vec<f64>, Vec<f64>) {
    let sr_f = sr as f64;
    let (mut columns, freqs) = match lf {
        Some(lf) => ac_core::visualize::aggregate::spectrum_to_columns_multiband_wire(
            lf,
            spec,
            sr_f,
            crossover_hz as f64,
            20.0,
            (sr_f / 2.0).max(21.0),
            ac_core::visualize::aggregate::DEFAULT_WIRE_COLUMNS,
        ),
        None => ac_core::visualize::aggregate::spectrum_to_columns_wire(
            spec,
            sr_f,
            20.0,
            (sr_f / 2.0).max(21.0),
            ac_core::visualize::aggregate::DEFAULT_WIRE_COLUMNS,
        ),
    };
    if mic_corr_enabled {
        if let Some(curve) = &ch.mic_curve {
            apply_mic_curve_inplace_f64(curve, &freqs, &mut columns);
        }
    }
    (columns, freqs)
}

/// Convert a possibly-infinite `f64` to JSON — `null` when not finite,
/// real number otherwise. Keeps the sidecar frame JSON-parseable; `-inf`
/// would otherwise fail `serde_json`'s finite-value check.
fn json_finite(v: f64) -> Value {
    if v.is_finite() {
        json!(v)
    } else {
        Value::Null
    }
}

// mic-curve helpers live in `super::super::mic` (handlers/mic.rs) since
// the Tier 1 handlers also need them; see #97 / #98.
use crate::handlers::mic::{
    apply_mic_curve_inplace_f32, apply_mic_curve_inplace_f64, mic_correction_tag,
};

/// Per-channel time-integrator state for the `fractional_octave_leq`
/// sidecar frame. Re-initialised when the mode changes or when the band
/// count changes (ioct_bpo toggle).
enum Integrator {
    Ema(EmaIntegrator),
    Leq(LeqIntegrator),
}

impl Integrator {
    fn for_mode(mode: &str, n_bands: usize) -> Option<Self> {
        match mode {
            "fast" => Some(Self::Ema(EmaIntegrator::new(TAU_FAST_S, n_bands))),
            "slow" => Some(Self::Ema(EmaIntegrator::new(TAU_SLOW_S, n_bands))),
            "leq" => Some(Self::Leq(LeqIntegrator::new(n_bands))),
            _ => None,
        }
    }

    fn n_bands(&self) -> usize {
        match self {
            Self::Ema(e) => e.state_len(),
            Self::Leq(l) => l.state_len(),
        }
    }

    fn update(&mut self, levels_dbfs: &[f64], dt_s: f64) -> Vec<f64> {
        match self {
            Self::Ema(e) => e.update(levels_dbfs, dt_s),
            Self::Leq(l) => l.update(levels_dbfs, dt_s),
        }
    }

    fn duration_s(&self) -> f64 {
        match self {
            Self::Ema(_) => f64::NAN,
            Self::Leq(l) => l.duration_s(),
        }
    }

    fn reset_if_leq(&mut self) {
        if let Self::Leq(l) = self {
            l.reset();
        }
    }
}

/// Per-channel state for the multi-channel monitor's `eng.reconnect_input()`
/// path. Tracks consecutive failures so the worker can rate-limit error
/// frames, back off between retries, and give up on a sustained outage.
/// (#93 fix — without this, a permanently-disconnected port would
/// re-enter the reconnect path on every tick, flooding both the JACK
/// syscall and the PUB socket.)
struct ReconnectState {
    consecutive_failures: u32,
    first_failure_at: Option<std::time::Instant>,
    last_error_pub_at: Option<std::time::Instant>,
}

const RECONNECT_GIVE_UP: std::time::Duration = std::time::Duration::from_secs(30);
const RECONNECT_ERR_RATE_LIMIT: std::time::Duration = std::time::Duration::from_secs(1);

impl ReconnectState {
    fn new() -> Self {
        Self {
            consecutive_failures: 0,
            first_failure_at: None,
            last_error_pub_at: None,
        }
    }

    fn note_success(&mut self) {
        self.consecutive_failures = 0;
        self.first_failure_at = None;
        self.last_error_pub_at = None;
    }

    fn note_failure(&mut self, now: std::time::Instant) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.first_failure_at.is_none() {
            self.first_failure_at = Some(now);
        }
    }

    /// Back-off before the next retry. 1st failure = no sleep; ramps to a
    /// 1 s cap so a permanently-disconnected channel doesn't busy-loop.
    fn backoff(&self) -> std::time::Duration {
        std::time::Duration::from_millis(match self.consecutive_failures {
            0 | 1 => 0,
            2..=4 => 100,
            5..=9 => 500,
            _ => 1000,
        })
    }

    /// True when the first failure was ≥ `RECONNECT_GIVE_UP` ago — caller
    /// should emit a terminal error and `return` from the worker.
    fn should_give_up(&self, now: std::time::Instant) -> bool {
        self.first_failure_at
            .is_some_and(|t0| now.duration_since(t0) >= RECONNECT_GIVE_UP)
    }

    /// True when the current error PUB should be emitted (≥ 1 s since the
    /// last one, or first error of this outage). Updates the timestamp as
    /// a side effect.
    fn should_emit_error(&mut self, now: std::time::Instant) -> bool {
        let emit = self
            .last_error_pub_at
            .is_none_or(|t| now.duration_since(t) >= RECONNECT_ERR_RATE_LIMIT);
        if emit {
            self.last_error_pub_at = Some(now);
        }
        emit
    }
}

/// One tone in a `fake_tones` stimulus request — see `monitor_spectrum`.
struct FakeTone {
    freq_hz: f64,
    level_dbfs: f64,
}

/// Full-scale dBFS → linear peak amplitude (0 dBFS = 1.0). Used only for
/// the `--fake-audio` stimulus knobs below (#170 display-truth harness);
/// real backends never see this.
fn dbfs_to_amplitude(dbfs: f64) -> f64 {
    10f64.powf(dbfs / 20.0)
}

/// Dual-resolution low-frequency FFT state for one channel (#142 / #173).
///
/// A second, longer ring feeds an `lf_fft_n`-point FFT used only below
/// `crossover_hz`; the channel's short `fft_ring` keeps driving the high
/// band at the live refresh rate. The LF spectrum is recomputed every
/// `LF_OVERLAP`-hop (`(1 - LF_OVERLAP) * lf_fft_n / sr`, #173) rather
/// than once per full block, and each new raw recompute is smoothed
/// through a power-domain EMA (`ema`) before caching — see `LF_OVERLAP`
/// / `LF_AVG_TAU_S` above.
///
/// The five members are only ever read and written together — the long
/// ring, the smoothed spectrum it produces, the overlap-hop counter, and
/// the EMA plus the timestamp its `dt` is measured from.
struct LfState {
    ring: std::collections::VecDeque<f32>,
    /// Most recent smoothed LF linear half-spectrum; `None` until the
    /// ring first fills.
    spec_cache: Option<Vec<f64>>,
    ticks_since_recompute: u32,
    ema: Option<EmaIntegrator>,
    ema_last_ts: Option<std::time::Instant>,
}

impl LfState {
    fn new() -> Self {
        Self {
            ring: std::collections::VecDeque::with_capacity(131_072),
            spec_cache: None,
            ticks_since_recompute: u32::MAX,
            ema: None,
            ema_last_ts: None,
        }
    }

    /// True when this channel still holds LF state that a disabled LF
    /// band would leave stale.
    fn is_stale(&self) -> bool {
        !self.ring.is_empty() || self.spec_cache.is_some()
    }

    /// Drop everything so a later re-enable rebuilds from fresh capture.
    fn clear(&mut self) {
        self.ring.clear();
        self.spec_cache = None;
        self.ticks_since_recompute = u32::MAX;
        self.ema = None;
        self.ema_last_ts = None;
    }
}

/// Everything the monitor worker tracks for one monitored channel.
///
/// This used to be eighteen `Vec`s walked by a shared `idx`. Holding one
/// struct per channel means a channel cannot go half-initialised, a new
/// piece of per-channel state cannot be added at one construction site
/// and forgotten at another, and `idx` can no longer address two
/// different channels within one tick.
struct ChannelState {
    channel: u32,
    /// Resolved input port; used only by the multi-channel
    /// `reconnect_input` path.
    in_port: String,
    cal: Option<Calibration>,
    /// Per-channel SPL offset (= 94 - mic_sens_dbfs); `None` when the
    /// channel hasn't been pistonphone-calibrated. Cached once at start
    /// — re-running `calibrate_spl` requires a `monitor` restart, same
    /// as voltage cal changes need today.
    spl_offset: Option<f64>,
    /// Mic frequency-response curve, cloned out of `cal` for cheap
    /// per-tick lookup. Same staleness caveat as `spl_offset`.
    mic_curve: Option<ac_core::shared::calibration::MicResponse>,
    /// Mic-curve FIR for the loudness path (#104), built once at start
    /// when the curve is loaded, bypassed when no curve or when the
    /// global toggle is off. Runs *before* K-weighting / dBTP so LKFS
    /// reflects the mic-corrected acoustic level.
    loudness_fir: Option<MicCurveFir>,
    current_freq: f64,
    /// #93: reconnect-failure state for the multi-channel path.
    /// Single-channel never touches `eng.reconnect_input()` and this
    /// stays zeroed.
    reconnect: ReconnectState,
    cwt_ring: std::collections::VecDeque<f32>,
    cqt_ring: std::collections::VecDeque<f32>,
    reass_ring: std::collections::VecDeque<f32>,
    /// Sliding ring for the FFT path so refresh cadence (`cur_interval`)
    /// is decoupled from capture-window duration (`cur_fft_n / sr`).
    fft_ring: std::collections::VecDeque<f32>,
    lf: LfState,
    /// Time-integration state for the `fractional_octave_leq` sidecar
    /// frame. `None` until the first fractional_octave frame at the
    /// current mode + band count arrives.
    integrator: Option<Integrator>,
    last_frac_ts: Option<std::time::Instant>,
    /// BS.1770-5 / R128 mono-weighted loudness, emitted as a
    /// `measurement/loudness` sidecar frame each tick.
    loudness: LoudnessState,
}

/// Ring capacities for one channel. They come from the worker rather
/// than being recomputed here because the same values also drive the
/// per-tick trim conditions, and each is derived from the engine's `sr`,
/// which is only known after `eng.start()`.
struct RingCaps {
    cwt: usize,
    cqt: usize,
    reass: usize,
}

impl ChannelState {
    /// The ring a given ring-buffered mode fills.
    fn ring_mut(&mut self, kind: RingKind) -> &mut std::collections::VecDeque<f32> {
        match kind {
            RingKind::Cwt => &mut self.cwt_ring,
            RingKind::Cqt => &mut self.cqt_ring,
            RingKind::Reassigned => &mut self.reass_ring,
        }
    }

    fn new(
        channel: u32,
        in_port: String,
        out_ch: u32,
        sr: u32,
        freq_hz: f64,
        caps: &RingCaps,
    ) -> Self {
        let cal = Calibration::load(out_ch, channel, None).ok().flatten();
        let spl_offset = cal.as_ref().and_then(Calibration::spl_offset_db);
        let mic_curve = cal.as_ref().and_then(|c| c.mic_response.clone());
        let loudness_fir = mic_curve
            .as_ref()
            .map(|curve| MicCurveFir::new(curve, sr, DEFAULT_N_TAPS));
        Self {
            channel,
            in_port,
            cal,
            spl_offset,
            mic_curve,
            loudness_fir,
            current_freq: freq_hz,
            reconnect: ReconnectState::new(),
            cwt_ring: std::collections::VecDeque::with_capacity(caps.cwt),
            cqt_ring: std::collections::VecDeque::with_capacity(caps.cqt),
            reass_ring: std::collections::VecDeque::with_capacity(caps.reass),
            fft_ring: std::collections::VecDeque::with_capacity(131_072),
            lf: LfState::new(),
            integrator: None,
            last_frac_ts: None,
            loudness: LoudnessState::new_mono(sr)
                .expect("sample_rate > 0 guaranteed by engine.sample_rate()"),
        }
    }
}

pub fn monitor_spectrum(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "monitor_spectrum");
    cfg_guard!(state);
    let freq_hz = cmd.get("freq_hz").and_then(Value::as_f64).unwrap_or(1000.0);
    // Fake-audio-only stimulus knobs for the display-truth harness (#170,
    // `ac test software`). Ignored entirely on real backends — the harness
    // is required to never touch physical hardware, so these are only
    // read/applied below when `state.fake_audio` is true.
    let amplitude = cmd.get("amplitude").and_then(Value::as_f64).unwrap_or(0.0);
    let fake_tones: Option<Vec<FakeTone>> =
        cmd.get("fake_tones").and_then(Value::as_array).map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let freq_hz = t.get("freq_hz").and_then(Value::as_f64)?;
                    let level_dbfs = t.get("level_dbfs").and_then(Value::as_f64)?;
                    Some(FakeTone {
                        freq_hz,
                        level_dbfs,
                    })
                })
                .collect()
        });
    let fake_noise_dbfs = cmd.get("fake_noise_dbfs").and_then(Value::as_f64);

    let defaults = MonitorParams::default();
    let interval = cmd
        .get("interval")
        .and_then(Value::as_f64)
        .unwrap_or(defaults.interval);
    let fft_n = cmd
        .get("fft_n")
        .and_then(Value::as_u64)
        .unwrap_or(defaults.fft_n as u64) as u32;

    if !(interval > 0.0 && interval <= 60.0) {
        return json!({"ok": false, "error": "interval must be > 0 and <= 60"});
    }
    if !fft_n.is_power_of_two() || !(256..=131_072).contains(&fft_n) {
        return json!({"ok": false, "error": "fft_n must be power of 2 in [256, 131072]"});
    }

    let lf_fft_n = defaults.lf_fft_n;
    let crossover_hz = defaults.crossover_hz;
    {
        let mut mp = state.monitor_params.lock().unwrap();
        *mp = MonitorParams {
            interval,
            fft_n,
            lf_fft_n,
            crossover_hz,
            active: true,
        };
    }
    let monitor_params_shared = state.monitor_params.clone();

    let cfg = state.cfg.lock().unwrap().clone();

    let channels: Vec<u32> = cmd
        .get("channels")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_u64)
                .map(|v| v as u32)
                .collect()
        })
        .filter(|v: &Vec<u32>| !v.is_empty())
        .unwrap_or_else(|| vec![cfg.input_channel]);

    // One bad channel fails the whole request rather than monitoring a
    // fabricated port alongside the good ones (#206).
    let in_ports: Vec<String> = match channels
        .iter()
        .map(|&ch| {
            let mut cfg_override = cfg.clone();
            cfg_override.input_channel = ch;
            cfg_override.input_port = None; // force index-based resolution
            resolve_input(&cfg_override, state)
        })
        .collect::<Result<Vec<String>, String>>()
    {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let primary_in_port = in_ports.first().cloned().unwrap_or_default();

    let pub_tx = state.pub_tx.clone();
    let fake = state.fake_audio;
    let out_ch = cfg.output_channel;
    let n_channels = channels.len() as u32;
    let channels_worker = channels.clone();
    let in_ports_worker = in_ports.clone();
    let analysis_mode = state.analysis_mode.clone();
    let mic_corr_enabled = state.mic_correction_enabled.clone();
    let cwt_sigma_shared = state.cwt_sigma.clone();
    let cwt_n_scales_shared = state.cwt_n_scales.clone();
    let ioct_bpo_shared = state.ioct_bpo.clone();
    let time_integration_shared = state.time_integration_mode.clone();
    let leq_reset_shared = state.leq_reset_request.clone();
    let loudness_reset_shared = state.loudness_reset_request.clone();
    let band_weighting_shared = state.band_weighting.clone();

    let worker = spawn_worker(state, "monitor_spectrum", move |stop| {
        let mut eng = make_engine(fake);
        let start_port = in_ports_worker.first().map(String::as_str);
        if let Err(e) = eng.start(&[], start_port) {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"monitor_spectrum","message":format!("{e}")}),
            );
            return;
        }
        // Apply the display-truth harness's stimulus knobs (#170) — fake
        // backend only, real hardware is never touched by this command.
        // Precedence when more than one is given: fake_tones > fake_noise_dbfs
        // > freq_hz/amplitude single-tone (the pre-#170 default path).
        if fake {
            if let Some(tones) = &fake_tones {
                let pairs: Vec<(f64, f64)> = tones
                    .iter()
                    .map(|t| (t.freq_hz, dbfs_to_amplitude(t.level_dbfs)))
                    .collect();
                eng.set_tone_pair(&pairs);
            } else if let Some(noise_dbfs) = fake_noise_dbfs {
                eng.set_broadband_noise(dbfs_to_amplitude(noise_dbfs));
            } else {
                eng.set_tone(freq_hz, amplitude);
            }
        }
        let sr = eng.sample_rate();
        // Per-tick monotonic counter shared across all channels in the
        // tick. Phase 0b: the UI's Goniometer / PhaseScope3D pair L and
        // R scope frames by `frame_idx`, so it MUST increment exactly
        // once per tick — not once per (tick, channel). Wraps on u64
        // overflow (~600 years at 1 kHz tick rate; not a real concern).
        let mut frame_idx: u64 = 0;

        // CWT state: recomputed when sigma/n_scales change.
        let mut cwt_sigma = *cwt_sigma_shared.lock().unwrap();
        let mut cwt_n_scales = *cwt_n_scales_shared.lock().unwrap();
        let (mut cwt_scales, mut cwt_freqs) = ac_core::visualize::cwt::log_scales(
            ac_core::visualize::cwt::DEFAULT_F_MIN,
            ac_core::visualize::cwt::default_f_max(sr),
            cwt_n_scales,
            sr,
            cwt_sigma,
        );

        // Sliding ring buffer for CWT: holds ~0.5 s of audio per channel so
        // low-frequency wavelets (20 Hz @ sigma=12 ≈ 0.6 s support) see
        // enough data. The capture-per-tick window matches the UI's
        // monitor_interval (read live from `monitor_params_shared` below)
        // so the daemon doesn't emit faster than the UI can paint —
        // pre-#109 CWT was hardcoded to 20 ms (50 Hz) regardless of
        // `--max-fps`, so a UI capped at 30 fps still received 50
        // frames/sec, with the extras dropped by skip-when-unchanged.
        // Floor at 16 ms (display refresh) and ceil at 100 ms so a wild
        // user override doesn't break the sliding-ring assumption.
        let ring_cap = (sr as f64 * 0.15).ceil() as usize; // 0.15 s — enough for 20 Hz
        let mut cwt_log_counter = 0u32;
        // Reused across every CWT tick so morlet_cwt_into doesn't allocate
        // a fresh Vec each call (prev ~3.5% of CPU in madvise / allocator).
        let mut cwt_mags: Vec<f32> = Vec::with_capacity(cwt_n_scales);

        // CQT state: separate from CWT because the lowest CQT bin needs
        // ~Q · sr / f_min samples in the ring to keep Q constant. With
        // bpo=24 (Q ≈ 34.1), 1 s of audio gives a usable f_min of ~34 Hz.
        // Kernels are built once per (sr, bpo, freqs) — fixed for the
        // worker's lifetime; live tunables can come later.
        let cqt_bpo = ac_core::visualize::cqt::DEFAULT_BPO;
        let cqt_ring_cap = sr as usize; // 1.0 s
                                        // CQT tick paced from `monitor_params.interval` like CWT (#109).
        let cqt_f_min = ac_core::visualize::cqt::DEFAULT_F_MIN.max(
            ac_core::visualize::cqt::min_supported_f(cqt_ring_cap, sr, cqt_bpo),
        );
        let cqt_freqs = ac_core::visualize::cqt::log_freqs(
            cqt_f_min,
            ac_core::visualize::cqt::default_f_max(sr),
            cqt_bpo,
        );
        let cqt_kernels =
            ac_core::visualize::cqt::build_kernels(&cqt_freqs, sr, cqt_bpo, cqt_ring_cap);
        let mut cqt_mags: Vec<f32> = Vec::with_capacity(cqt_freqs.len());
        let mut cqt_log_counter = 0u32;

        // Reassigned-spectrogram state. One forward FFT plan + Hann
        // window plus its time-weighted and derivative variants are
        // pre-built; the live tick reuses them across frames. The output
        // grid is log-spaced (so the existing waterfall renders it
        // unchanged), with more bins than the FFT length so reassignment
        // can split closely-spaced peaks the FFT would smear together.
        let reass_n = ac_core::visualize::reassigned::DEFAULT_N;
        let reass_n_out = ac_core::visualize::reassigned::DEFAULT_N_OUT_BINS;
        // Reassigned tick paced from `monitor_params.interval` (#109).
        let reass_kernels = ac_core::visualize::reassigned::build_kernels(
            reass_n,
            sr,
            reass_n_out,
            ac_core::visualize::reassigned::DEFAULT_F_MIN,
            ac_core::visualize::reassigned::default_f_max(sr),
        );
        let reass_freqs_out: Vec<f32> = reass_kernels.freqs_out.clone();
        let mut reass_mags: Vec<f32> = Vec::with_capacity(reass_n_out);
        let mut reass_log_counter = 0u32;

        // Integrator state is reset on mode/band-count change; Leq also
        // resets on the `leq_reset_request` flag, loudness on
        // `loudness_reset_request`.
        let mut cur_ti_mode: String = time_integration_shared.lock().unwrap().clone();

        // One state record per monitored channel — every per-channel ring,
        // calibration and integrator lives here. Built after `eng.start()`
        // because each ring capacity is derived from the engine's `sr`.
        let ring_caps = RingCaps {
            cwt: ring_cap,
            cqt: cqt_ring_cap,
            reass: reass_n,
        };
        let mut channel_states: Vec<ChannelState> = channels_worker
            .iter()
            .zip(in_ports_worker.iter())
            .map(|(&channel, in_port)| {
                ChannelState::new(channel, in_port.clone(), out_ch, sr, freq_hz, &ring_caps)
            })
            .collect();
        let single_channel = channel_states.len() == 1;

        while !stop.load(Ordering::Relaxed) {
            let tick_start = std::time::Instant::now();
            // Bump the per-tick counter and snapshot a tick-wide
            // timestamp BEFORE the per-channel loop so every scope
            // frame in this tick carries the same `frame_idx` /
            // `tick_ts_ns`. The existing per-channel `ts_ns` calls in
            // the loudness/spectrum branches stay as-is; only scope
            // frames need tick-wide alignment.
            frame_idx = frame_idx.wrapping_add(1);
            let tick_ts_ns = now_ns();
            let (cur_interval, cur_fft_n, cur_lf_fft_n, cur_crossover_hz) = {
                let mp = monitor_params_shared.lock().unwrap();
                (mp.interval, mp.fft_n, mp.lf_fft_n, mp.crossover_hz)
            };
            // LF band is only worth running when its FFT is genuinely longer
            // than the live one — otherwise the live spectrum already has
            // equal-or-finer Δf everywhere.
            let lf_band_enabled = cur_lf_fft_n > cur_fft_n;
            // Recompute the LF FFT every overlap-hop instead of once per
            // full block (#173) — see `LF_OVERLAP`.
            let lf_recompute_every = ((cur_lf_fft_n as f64 * (1.0 - LF_OVERLAP) / sr as f64)
                / cur_interval.max(1e-6))
            .round()
            .clamp(1.0, 4096.0) as u32;
            let mode = analysis_mode.lock().unwrap().clone();
            let is_cwt = mode == "cwt";
            let is_cqt = mode == "cqt";
            let is_reassigned = mode == "reassigned";

            // Time-integration bookkeeping — run once per tick.
            let new_ti_mode = time_integration_shared.lock().unwrap().clone();
            if new_ti_mode != cur_ti_mode {
                for ch in channel_states.iter_mut() {
                    ch.integrator = None;
                    ch.last_frac_ts = None;
                }
                cur_ti_mode = new_ti_mode;
            }
            if leq_reset_shared.swap(false, Ordering::Relaxed) {
                for ch in channel_states.iter_mut() {
                    if let Some(i) = ch.integrator.as_mut() {
                        i.reset_if_leq();
                    }
                    ch.last_frac_ts = None;
                }
            }
            if loudness_reset_shared.swap(false, Ordering::Relaxed) {
                for ch in channel_states.iter_mut() {
                    ch.loudness.reset();
                }
            }

            // Check for live CWT param changes.
            if is_cwt {
                let new_sigma = *cwt_sigma_shared.lock().unwrap();
                let new_n = *cwt_n_scales_shared.lock().unwrap();
                if (new_sigma - cwt_sigma).abs() > 0.01 || new_n != cwt_n_scales {
                    cwt_sigma = new_sigma;
                    cwt_n_scales = new_n;
                    let (s, f) = ac_core::visualize::cwt::log_scales(
                        ac_core::visualize::cwt::DEFAULT_F_MIN,
                        ac_core::visualize::cwt::default_f_max(sr),
                        cwt_n_scales,
                        sr,
                        cwt_sigma,
                    );
                    cwt_scales = s;
                    cwt_freqs = f;
                }
            }

            // Wire identity + per-tick values every channel shares. The
            // mic-correction toggle is sampled once per tick so all of a
            // tick's frames agree on it; before, each branch re-read the
            // atomic and one channel could disagree with the next.
            let ctx = TickCtx {
                pub_tx: &pub_tx,
                n_channels,
                sr,
                frame_idx,
                tick_ts_ns,
                mic_corr_enabled: mic_corr_enabled.load(Ordering::Relaxed),
                // Pace the ring-buffered modes to the UI's requested
                // interval, clamped to [16 ms, 100 ms]. Pre-#109 this was
                // hardcoded 20 ms regardless of `--max-fps`, so CWT
                // emitted at 50 fps even when the UI was capped at 30 —
                // wasted work on both sides.
                tick_secs: cur_interval.clamp(0.016, 0.100),
            };

            for ch in channel_states.iter_mut() {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let channel = ch.channel;
                if !single_channel {
                    if let Err(e) = eng.reconnect_input(&ch.in_port) {
                        let now = std::time::Instant::now();
                        let st = &mut ch.reconnect;
                        st.note_failure(now);
                        if st.should_give_up(now) {
                            let outage_s = st
                                .first_failure_at
                                .map(|t0| now.duration_since(t0).as_secs())
                                .unwrap_or(0);
                            send_pub(
                                &pub_tx,
                                "error",
                                &json!({
                                    "cmd":     "monitor_spectrum",
                                    "message": format!(
                                        "ch{channel} gave up after {outage_s}s of reconnect failures: {e}",
                                    ),
                                }),
                            );
                            return;
                        }
                        if st.should_emit_error(now) {
                            send_pub(
                                &pub_tx,
                                "error",
                                &json!({
                                    "cmd":     "monitor_spectrum",
                                    "message": format!(
                                        "reconnect ch{channel} (failures: {}): {e}",
                                        st.consecutive_failures,
                                    ),
                                }),
                            );
                        }
                        let backoff = st.backoff();
                        if !backoff.is_zero() {
                            std::thread::sleep(backoff);
                        }
                        continue;
                    }
                    ch.reconnect.note_success();
                    eng.flush_capture();
                }
                if is_cwt {
                    let xruns_total = match capture_into_ring(
                        eng.as_mut(),
                        ch,
                        &ctx,
                        RingKind::Cwt,
                        ring_cap,
                        CWT_MIN_FILL,
                    ) {
                        RingTick::Ready { xruns } => xruns,
                        RingTick::NotReady => continue,
                        RingTick::Failed => return,
                    };
                    let t0 = std::time::Instant::now();
                    let buf = ch.cwt_ring.make_contiguous();
                    ac_core::visualize::cwt::morlet_cwt_into(
                        buf,
                        sr,
                        &cwt_scales,
                        cwt_sigma,
                        &mut cwt_mags,
                    );
                    log_transform_time(
                        &mut cwt_log_counter,
                        "cwt",
                        channel,
                        t0,
                        buf.len(),
                        cwt_scales.len(),
                    );
                    let (ts_ns, mc_tag) = emit_ring_frames(
                        ch,
                        &ctx,
                        "visualize/cwt",
                        &cwt_freqs,
                        &mut cwt_mags,
                        &[],
                        xruns_total,
                    );
                    // Optional fractional-octave aggregation of the same
                    // CWT column: reuses `cwt_mags` / `cwt_freqs` — zero
                    // extra DSP cost when enabled.
                    if let Some(bpo) = *ioct_bpo_shared.lock().unwrap() {
                        let (band_centres, mut band_levels) =
                            ac_core::visualize::fractional_octave::cwt_to_fractional_octave(
                                &cwt_mags,
                                &cwt_freqs,
                                bpo as usize,
                                ac_core::visualize::cwt::DEFAULT_F_MIN,
                                ac_core::visualize::cwt::default_f_max(sr),
                            );
                        // Per-band frequency weighting (off/A/C/Z). Off
                        // and Z share the identity curve; applying is a
                        // no-op then, but we still tag the frame so the
                        // UI can distinguish "weighting explicitly Z"
                        // from "no weighting picked".
                        let weighting_tag = band_weighting_shared.lock().unwrap().clone();
                        let weighting_curve = WeightingCurve::from_tag(&weighting_tag);
                        if let Some(curve) = weighting_curve {
                            if !matches!(curve, WeightingCurve::Z) {
                                for (level, &fc) in band_levels.iter_mut().zip(band_centres.iter())
                                {
                                    *level += curve.db_offset(fc as f64) as f32;
                                }
                            }
                        }
                        let frac_frame = json!({
                            "type":           "visualize/fractional_octave",
                            "cmd":            "monitor_spectrum",
                            "channel":        channel,
                            "n_channels":     n_channels,
                            "sr":             sr,
                            "bpo":            bpo,
                            "weighting":      weighting_tag,
                            "freqs":          band_centres,
                            "spectrum":       band_levels.clone(),
                            "spl_offset_db":  ch.spl_offset,
                            "mic_correction": mc_tag,
                            "timestamp":      ts_ns,
                            "xruns":          xruns_total,
                        });
                        send_pub(&pub_tx, "data", &frac_frame);

                        if cur_ti_mode != "off" {
                            let n_bands = band_levels.len();
                            let slot = &mut ch.integrator;
                            // Re-init if the band count changed (e.g. live
                            // ioct_bpo toggle) or if this channel hasn't
                            // been primed yet.
                            if slot
                                .as_ref()
                                .map(|i| i.n_bands() != n_bands)
                                .unwrap_or(true)
                            {
                                *slot = Integrator::for_mode(&cur_ti_mode, n_bands);
                                ch.last_frac_ts = None;
                            }
                            if let Some(integ) = slot.as_mut() {
                                let now = std::time::Instant::now();
                                let dt = ch
                                    .last_frac_ts
                                    .map(|t| now.duration_since(t).as_secs_f64())
                                    .unwrap_or(cur_interval)
                                    .max(1e-6);
                                ch.last_frac_ts = Some(now);
                                let levels_f64: Vec<f64> =
                                    band_levels.iter().map(|&v| v as f64).collect();
                                let integrated = integ.update(&levels_f64, dt);
                                let tau_s: Option<f64> = match cur_ti_mode.as_str() {
                                    "fast" => Some(TAU_FAST_S),
                                    "slow" => Some(TAU_SLOW_S),
                                    _ => None,
                                };
                                let dur_s = integ.duration_s();
                                let leq_frame = json!({
                                    "type":           "visualize/fractional_octave_leq",
                                    "cmd":            "monitor_spectrum",
                                    "channel":        channel,
                                    "n_channels":     n_channels,
                                    "sr":             sr,
                                    "bpo":            bpo,
                                    "weighting":      weighting_tag,
                                    "mode":           cur_ti_mode,
                                    "tau_s":          tau_s,
                                    "duration_s":     if dur_s.is_finite() { json!(dur_s) } else { Value::Null },
                                    "freqs":          band_centres,
                                    "spectrum":       integrated,
                                    "spl_offset_db":  ch.spl_offset,
                                    "mic_correction": mc_tag,
                                    "timestamp":      ts_ns,
                                    "xruns":          xruns_total,
                                });
                                send_pub(&pub_tx, "data", &leq_frame);
                            }
                        }
                    }
                    continue;
                }
                if is_cqt {
                    // The kernel for the lowest bin needs the full ring, so
                    // ticks are skipped until it has filled that far; the
                    // bins above it produce earlier, but a partial column
                    // would confuse the waterfall.
                    let xruns_total = match capture_into_ring(
                        eng.as_mut(),
                        ch,
                        &ctx,
                        RingKind::Cqt,
                        cqt_ring_cap,
                        cqt_kernels.max_kernel_len(),
                    ) {
                        RingTick::Ready { xruns } => xruns,
                        RingTick::NotReady => continue,
                        RingTick::Failed => return,
                    };
                    let t0 = std::time::Instant::now();
                    let buf = ch.cqt_ring.make_contiguous();
                    ac_core::visualize::cqt::cqt_into(buf, &cqt_kernels, &mut cqt_mags);
                    log_transform_time(
                        &mut cqt_log_counter,
                        "cqt",
                        channel,
                        t0,
                        buf.len(),
                        cqt_freqs.len(),
                    );
                    emit_ring_frames(
                        ch,
                        &ctx,
                        "visualize/cqt",
                        &cqt_freqs,
                        &mut cqt_mags,
                        &[("bpo", json!(cqt_bpo))],
                        xruns_total,
                    );
                    continue;
                }
                if is_reassigned {
                    let xruns_total = match capture_into_ring(
                        eng.as_mut(),
                        ch,
                        &ctx,
                        RingKind::Reassigned,
                        reass_n,
                        reass_n,
                    ) {
                        RingTick::Ready { xruns } => xruns,
                        RingTick::NotReady => continue,
                        RingTick::Failed => return,
                    };
                    let t0 = std::time::Instant::now();
                    let buf = ch.reass_ring.make_contiguous();
                    ac_core::visualize::reassigned::reassigned_into(
                        buf,
                        &reass_kernels,
                        &mut reass_mags,
                    );
                    log_transform_time(
                        &mut reass_log_counter,
                        "reassigned",
                        channel,
                        t0,
                        buf.len(),
                        reass_freqs_out.len(),
                    );
                    emit_ring_frames(
                        ch,
                        &ctx,
                        "visualize/reassigned",
                        &reass_freqs_out,
                        &mut reass_mags,
                        &[],
                        xruns_total,
                    );
                    continue;
                }

                // FFT path. Each channel has its own sliding ring so refresh
                // cadence (`cur_interval`) is decoupled from FFT window length
                // (`cur_fft_n`). Single-channel uses `capture_available` (non-
                // clearing drain on JACK, falls back to capture_block
                // elsewhere); multi-channel must use block capture because
                // `reconnect_input` clears the ring on every switch.
                let per_ch_budget = (cur_interval / n_channels as f64).max(0.002);
                let budget_samples = capture_budget_samples(per_ch_budget, sr);
                let captured = if single_channel {
                    eng.capture_available(budget_samples)
                } else {
                    eng.capture_block(budget_samples as f64 / sr as f64)
                };
                let Some(new) = capture_or_report(captured, &pub_tx, channel) else {
                    return;
                };
                // `eng.xruns()` is already a cumulative count for this
                // engine session (see `jack_backend.rs`'s
                // `SharedState::xruns`), so this assigns rather than
                // accumulates — summing it across per-tick, per-channel
                // reads would multiply a handful of real xruns into
                // thousands over a long monitor session.
                let xruns_total = eng.xruns();
                // Loudness runs on the raw capture, independent of the
                // FFT-N sliding ring.
                push_loudness_with_optional_fir(
                    &mut ch.loudness,
                    &mut ch.loudness_fir,
                    ctx.mic_corr_enabled,
                    &new,
                );
                emit_scope_frame(
                    &pub_tx,
                    channel,
                    n_channels,
                    sr,
                    &new,
                    frame_idx,
                    tick_ts_ns,
                    xruns_total,
                );
                let ring = &mut ch.fft_ring;
                ring.extend(new.iter());
                while ring.len() > cur_fft_n as usize {
                    ring.pop_front();
                }
                if ring.len() < 256 {
                    continue;
                }
                let samples = ring.make_contiguous();

                // Dual-resolution LF path (#142): keep a longer ring fed by the
                // same capture and recompute its long-N spectrum on the LF
                // cadence. The cached LF half-spectrum is merged below the
                // crossover; above it the live `r.spectrum` is untouched.
                if lf_band_enabled {
                    let lf_ring = &mut ch.lf.ring;
                    lf_ring.extend(new.iter());
                    while lf_ring.len() > cur_lf_fft_n as usize {
                        lf_ring.pop_front();
                    }
                    if lf_ring.len() >= cur_lf_fft_n as usize {
                        let counter = &mut ch.lf.ticks_since_recompute;
                        if *counter >= lf_recompute_every {
                            let lf_buf = lf_ring.make_contiguous();
                            let (lf_spec, _) =
                                ac_core::visualize::spectrum::spectrum_only(lf_buf, sr);
                            // Power-domain EMA smoothing (#173): re-init
                            // whenever the bin count changes (lf_fft_n
                            // change) so stale state never gets fed a
                            // mismatched vector length.
                            let n_bins = lf_spec.len();
                            let ema_slot = &mut ch.lf.ema;
                            if ema_slot
                                .as_ref()
                                .map(|e| e.state_len() != n_bins)
                                .unwrap_or(true)
                            {
                                *ema_slot = Some(EmaIntegrator::new(LF_AVG_TAU_S, n_bins));
                                ch.lf.ema_last_ts = None;
                            }
                            let now = std::time::Instant::now();
                            let dt = ch
                                .lf
                                .ema_last_ts
                                .map(|t| now.duration_since(t).as_secs_f64())
                                .unwrap_or(cur_lf_fft_n as f64 * (1.0 - LF_OVERLAP) / sr as f64)
                                .max(1e-6);
                            ch.lf.ema_last_ts = Some(now);
                            let raw_db: Vec<f64> =
                                lf_spec.iter().map(|&a| 20.0 * a.log10()).collect();
                            let smoothed_db = ema_slot.as_mut().unwrap().update(&raw_db, dt);
                            let smoothed_amp: Vec<f64> = smoothed_db
                                .iter()
                                .map(|&db| 10f64.powf(db / 20.0))
                                .collect();
                            ch.lf.spec_cache = Some(smoothed_amp);
                            *counter = 0;
                        } else {
                            *counter = counter.saturating_add(1);
                        }
                    }
                } else if ch.lf.is_stale() {
                    // LF band disabled (live N caught up to LF N) — drop stale
                    // state so a later re-enable rebuilds from fresh capture.
                    ch.lf.clear();
                }
                let lf_spec_for_merge: Option<&[f64]> = if lf_band_enabled {
                    ch.lf.spec_cache.as_deref()
                } else {
                    None
                };

                {
                    let analyze_result =
                        ac_core::measurement::thd::analyze(samples, sr, ch.current_freq, 10);
                    let mc_enabled = ctx.mic_corr_enabled;
                    let mc_tag = mic_correction_tag(ch.mic_curve.is_some(), mc_enabled);
                    let mut frame = match analyze_result {
                        Ok(r) => {
                            ch.current_freq = r.fundamental_hz;
                            let in_dbu = ch
                                .cal
                                .as_ref()
                                .and_then(|c| c.in_vrms(r.linear_rms))
                                .map(ac_core::shared::conversions::vrms_to_dbu);
                            // Parabolic-interpolated peaks on the linear FFT
                            // (before column aggregation), so the cursor can
                            // show scallop-corrected dBFS on hover. Threshold
                            // 80 dB below the strongest bin keeps noise-floor
                            // bumps out; n_max=64 covers a busy harmonic
                            // spectrum without bloating the wire frame.
                            let raw_n = r.spectrum.len();
                            let raw_freqs: Vec<f64> = (0..raw_n)
                                .map(|k| k as f64 * sr as f64 / (2.0 * (raw_n - 1).max(1) as f64))
                                .collect();
                            let peak_thr = r.fundamental_dbfs as f32 - 80.0;
                            let mut peaks = ac_core::visualize::spectrum::find_interpolated_peaks(
                                &r.spectrum,
                                &raw_freqs,
                                64,
                                peak_thr,
                            );
                            // Below the crossover the long-N LF spectrum gives
                            // finer peak positions; splice LF peaks under the
                            // crossover with HF peaks above it (#142).
                            if let Some(lf) = lf_spec_for_merge {
                                let cx = cur_crossover_hz;
                                let lf_n = lf.len();
                                let lf_freqs: Vec<f64> = (0..lf_n)
                                    .map(|k| {
                                        k as f64 * sr as f64 / (2.0 * (lf_n - 1).max(1) as f64)
                                    })
                                    .collect();
                                let mut lf_peaks =
                                    ac_core::visualize::spectrum::find_interpolated_peaks(
                                        lf, &lf_freqs, 64, peak_thr,
                                    );
                                peaks.retain(|p| p.freq_hz >= cx);
                                lf_peaks.retain(|p| p.freq_hz < cx);
                                peaks.append(&mut lf_peaks);
                                peaks.sort_by(|a, b| {
                                    b.dbfs
                                        .partial_cmp(&a.dbfs)
                                        .unwrap_or(std::cmp::Ordering::Equal)
                                });
                                peaks.truncate(64);
                            }
                            let peaks_json: Vec<serde_json::Value> =
                                peaks.iter().map(|p| json!([p.freq_hz, p.dbfs])).collect();
                            let (spec, freqs) = spectrum_columns(
                                &r.spectrum,
                                lf_spec_for_merge,
                                sr,
                                cur_crossover_hz,
                                ch,
                                mc_enabled,
                            );
                            // THD analysis succeeded, so the frame carries
                            // the tone readouts on top of the common
                            // envelope built below.
                            json!({
                                "freqs":            freqs,
                                "spectrum":         spec,
                                "freq_hz":          r.fundamental_hz,
                                "peaks":            peaks_json,
                                "fundamental_dbfs": r.fundamental_dbfs,
                                "thd_pct":          r.thd_pct,
                                "thdn_pct":         r.thdn_pct,
                                "in_dbu":           in_dbu,
                                "clipping":         r.clipping,
                            })
                        }
                        // No resolvable fundamental — emit the plain
                        // spectrum with none of the tone readouts.
                        Err(_) => {
                            let (raw, _) = ac_core::visualize::spectrum::spectrum_only(samples, sr);
                            let (spec, freqs) = spectrum_columns(
                                &raw,
                                lf_spec_for_merge,
                                sr,
                                cur_crossover_hz,
                                ch,
                                mc_enabled,
                            );
                            json!({
                                "freqs":    freqs,
                                "spectrum": spec,
                            })
                        }
                    };
                    // Envelope common to both paths. `serde_json`'s Map is
                    // a BTreeMap here (no `preserve_order` feature), so the
                    // wire key order is sorted either way and merging the
                    // two halves cannot reorder the frame.
                    if let Some(obj) = frame.as_object_mut() {
                        for (k, v) in [
                            ("type", json!("visualize/spectrum")),
                            ("cmd", json!("monitor_spectrum")),
                            ("channel", json!(channel)),
                            ("n_channels", json!(n_channels)),
                            ("sr", json!(sr)),
                            ("dbu_offset_db", json!(dbu_offset_db(ch.cal.as_ref()))),
                            ("spl_offset_db", json!(ch.spl_offset)),
                            ("mic_correction", json!(mc_tag)),
                            ("xruns", json!(xruns_total)),
                        ] {
                            obj.insert(k.to_string(), v);
                        }
                    }
                    send_pub(&pub_tx, "data", &frame);
                    let ts_ns = now_ns();
                    emit_loudness_frame(
                        &pub_tx,
                        channel,
                        n_channels,
                        sr,
                        &ch.loudness,
                        ch.spl_offset,
                        mc_tag,
                        ts_ns,
                        xruns_total,
                    );
                }
            }
            // Pace FFT mode to requested interval. CWT/CQT/reassigned have
            // their own cadence (short tick + sliding ring) and pace
            // themselves.
            if !is_cwt && !is_cqt && !is_reassigned {
                let elapsed = tick_start.elapsed().as_secs_f64();
                if elapsed < cur_interval {
                    std::thread::sleep(std::time::Duration::from_secs_f64(cur_interval - elapsed));
                }
            }
        }
        eng.stop();
        {
            let mut mp = monitor_params_shared.lock().unwrap();
            mp.active = false;
        }
        send_pub(&pub_tx, "done", &json!({"cmd":"monitor_spectrum"}));
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("monitor_spectrum".to_string(), worker);
    }
    json!({
        "ok": true,
        "in_port":         primary_in_port,
        "in_ports":        in_ports,
        "channels":        channels,
        "lf_fft_n":        lf_fft_n,
        "crossover_hz":    crossover_hz,
        "lf_avg_tau_ms":   LF_AVG_TAU_S * 1000.0,
        "lf_overlap_pct":  LF_OVERLAP * 100.0,
    })
}

#[cfg(test)]
mod reconnect_state_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn backoff_ramps_then_caps() {
        let mut st = ReconnectState::new();
        let now = Instant::now();

        assert_eq!(st.backoff(), Duration::ZERO);
        st.note_failure(now);
        assert_eq!(st.backoff(), Duration::ZERO, "1st failure: no sleep yet");

        st.note_failure(now);
        assert_eq!(st.backoff(), Duration::from_millis(100), "2nd failure");
        st.note_failure(now);
        st.note_failure(now);
        assert_eq!(st.backoff(), Duration::from_millis(100), "4th failure");

        st.note_failure(now);
        assert_eq!(st.backoff(), Duration::from_millis(500), "5th failure");
        for _ in 0..4 {
            st.note_failure(now);
        }
        assert_eq!(st.backoff(), Duration::from_millis(500), "9th failure");

        st.note_failure(now);
        assert_eq!(
            st.backoff(),
            Duration::from_millis(1000),
            "10th failure caps at 1s"
        );
        for _ in 0..50 {
            st.note_failure(now);
        }
        assert_eq!(st.backoff(), Duration::from_millis(1000), "stays capped");
    }

    #[test]
    fn note_success_resets_state() {
        let mut st = ReconnectState::new();
        let now = Instant::now();
        for _ in 0..7 {
            st.note_failure(now);
        }
        let _ = st.should_emit_error(now);
        assert!(st.first_failure_at.is_some());
        assert!(st.last_error_pub_at.is_some());

        st.note_success();
        assert_eq!(st.consecutive_failures, 0);
        assert!(st.first_failure_at.is_none());
        assert!(st.last_error_pub_at.is_none());
        assert_eq!(st.backoff(), Duration::ZERO);
    }

    #[test]
    fn should_emit_error_rate_limits() {
        let mut st = ReconnectState::new();
        let t0 = Instant::now();

        st.note_failure(t0);
        assert!(st.should_emit_error(t0), "first error always emits");

        let t_half = t0 + Duration::from_millis(500);
        st.note_failure(t_half);
        assert!(!st.should_emit_error(t_half), "0.5 s later: suppressed");

        let t_2 = t0 + Duration::from_millis(1100);
        st.note_failure(t_2);
        assert!(st.should_emit_error(t_2), "1.1 s later: emit again");

        let t_3 = t_2 + Duration::from_millis(900);
        st.note_failure(t_3);
        assert!(
            !st.should_emit_error(t_3),
            "0.9 s after last emit: suppressed"
        );
    }

    #[test]
    fn should_give_up_only_after_30s_of_failures() {
        let mut st = ReconnectState::new();
        let t0 = Instant::now();

        assert!(!st.should_give_up(t0), "no failures yet — never give up");

        st.note_failure(t0);
        assert!(!st.should_give_up(t0));
        assert!(!st.should_give_up(t0 + Duration::from_secs(29)));
        assert!(st.should_give_up(t0 + Duration::from_secs(30)));
        assert!(st.should_give_up(t0 + Duration::from_secs(60)));
    }

    #[test]
    fn first_failure_at_is_sticky_until_success() {
        let mut st = ReconnectState::new();
        let t0 = Instant::now();
        st.note_failure(t0);
        let initial = st.first_failure_at;
        assert!(initial.is_some());

        for n in 1..5 {
            st.note_failure(t0 + Duration::from_millis(n * 200));
            assert_eq!(st.first_failure_at, initial, "anchor fixed across failures");
        }

        st.note_success();
        assert!(st.first_failure_at.is_none());
    }
}

#[cfg(test)]
mod drain_budget_tests {
    use super::capture_budget_samples;

    /// **The invariant that issue #208 violated.** A tick must drain at least
    /// as many samples as arrive during the same interval. Anything less
    /// accumulates in the JACK ring every tick, so the spectrum falls
    /// progressively behind realtime and the daemon ends up replaying old
    /// audio — the reported "response reappears with no stimulus present".
    ///
    /// Swept across every sample rate and interval the daemon accepts, and
    /// across the full legal `fft_n` range, because the old bug was precisely
    /// a ceiling that depended on `fft_n`.
    #[test]
    fn budget_never_drains_slower_than_the_ring_fills() {
        for sr in [44_100u32, 48_000, 88_200, 96_000, 176_400, 192_000] {
            for interval in [0.002f64, 0.01, 0.05, 0.1, 0.2, 0.5, 1.0] {
                for n_ch in [1usize, 2, 4, 8] {
                    let per_ch = (interval / n_ch as f64).max(0.002);
                    let arriving = (per_ch * sr as f64) as usize;
                    let budget = capture_budget_samples(per_ch, sr);
                    assert!(
                        budget >= arriving,
                        "sr={sr} interval={interval} n_ch={n_ch}: drains {budget}                          but {arriving} arrive — backlog grows {} samples/tick",
                        arriving - budget
                    );
                }
            }
        }
    }

    /// The exact configuration that shipped the defect: daemon defaults
    /// (`interval` 0.2, `fft_n` 8192), single channel, at both rig rates.
    /// The old `clamp(128, fft_n)` returned 8192 in both rows.
    #[test]
    fn default_monitor_config_keeps_up_at_both_rig_rates() {
        // 96 kHz: 19 200 arrive per 0.2 s tick. Old budget 8192 = 42.7%.
        assert_eq!(capture_budget_samples(0.2, 96_000), 19_200);
        // 48 kHz: 9600 arrive. Old budget 8192 = 85.3%, a lag growing at
        // 0.147 s/s — 3-5 s behind after 20-34 s.
        assert_eq!(capture_budget_samples(0.2, 48_000), 9_600);
    }

    /// The floor survives: a very short interval must still ask JACK for
    /// enough to be worth a round trip.
    #[test]
    fn short_interval_keeps_the_floor() {
        assert_eq!(capture_budget_samples(0.002, 44_100), 128);
        assert!(capture_budget_samples(0.0, 96_000) >= 128);
    }
}
