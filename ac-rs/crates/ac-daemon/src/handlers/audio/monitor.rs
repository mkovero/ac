//! `monitor_spectrum` — live per-channel spectrum/CWT feed.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::measurement::loudness::LoudnessState;
use ac_core::shared::calibration::Calibration;
use ac_core::shared::mic_curve_filter::{MicCurveFir, DEFAULT_N_TAPS};
use ac_core::visualize::time_integration::{
    EmaIntegrator, LeqIntegrator, TAU_FAST_S, TAU_SLOW_S,
};
use ac_core::visualize::weighting_curves::WeightingCurve;

use crate::audio::make_engine;
use crate::server::{MonitorParams, ServerState};

use super::super::{busy_guard, resolve_input, send_pub, spawn_worker};

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
    loudness:         &mut LoudnessState,
    fir:              &mut Option<MicCurveFir>,
    mic_corr_enabled: bool,
    samples:          &[f32],
) {
    if let (true, Some(fir)) = (mic_corr_enabled, fir.as_mut()) {
        let mut filtered = samples.to_vec();
        fir.process_inplace(&mut filtered);
        let _ = loudness.push(&[&filtered]);
    } else {
        let _ = loudness.push(&[samples]);
    }
}

/// Convert a possibly-infinite `f64` to JSON — `null` when not finite,
/// real number otherwise. Keeps the sidecar frame JSON-parseable; `-inf`
/// would otherwise fail `serde_json`'s finite-value check.
fn json_finite(v: f64) -> Value {
    if v.is_finite() { json!(v) } else { Value::Null }
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
            "leq"  => Some(Self::Leq(LeqIntegrator::new(n_bands))),
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
    first_failure_at:     Option<std::time::Instant>,
    last_error_pub_at:    Option<std::time::Instant>,
}

const RECONNECT_GIVE_UP: std::time::Duration = std::time::Duration::from_secs(30);
const RECONNECT_ERR_RATE_LIMIT: std::time::Duration = std::time::Duration::from_secs(1);

impl ReconnectState {
    fn new() -> Self {
        Self {
            consecutive_failures: 0,
            first_failure_at:     None,
            last_error_pub_at:    None,
        }
    }

    fn note_success(&mut self) {
        self.consecutive_failures = 0;
        self.first_failure_at     = None;
        self.last_error_pub_at    = None;
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
            _     => 1000,
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

pub fn monitor_spectrum(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "monitor_spectrum");
    let freq_hz = cmd.get("freq_hz").and_then(Value::as_f64).unwrap_or(1000.0);

    let defaults = MonitorParams::default();
    let interval = cmd.get("interval").and_then(Value::as_f64).unwrap_or(defaults.interval);
    let fft_n = cmd.get("fft_n").and_then(Value::as_u64).unwrap_or(defaults.fft_n as u64) as u32;

    if !(interval > 0.0 && interval <= 60.0) {
        return json!({"ok": false, "error": "interval must be > 0 and <= 60"});
    }
    if !fft_n.is_power_of_two() || fft_n < 256 || fft_n > 131_072 {
        return json!({"ok": false, "error": "fft_n must be power of 2 in [256, 131072]"});
    }

    {
        let mut mp = state.monitor_params.lock().unwrap();
        *mp = MonitorParams { interval, fft_n, active: true };
    }
    let monitor_params_shared = state.monitor_params.clone();

    let cfg = state.cfg.lock().unwrap().clone();

    let channels: Vec<u32> = cmd.get("channels")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_u64).map(|v| v as u32).collect())
        .filter(|v: &Vec<u32>| !v.is_empty())
        .unwrap_or_else(|| vec![cfg.input_channel]);

    let in_ports: Vec<String> = channels.iter()
        .map(|&ch| {
            let mut cfg_override = cfg.clone();
            cfg_override.input_channel = ch;
            cfg_override.input_port = None; // force index-based resolution
            resolve_input(&cfg_override, state)
        })
        .collect();
    let primary_in_port = in_ports.first().cloned().unwrap_or_default();

    let pub_tx = state.pub_tx.clone();
    let fake   = state.fake_audio;
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
        let cals: Vec<Option<Calibration>> = channels_worker.iter()
            .map(|&ch| Calibration::load(out_ch, ch, None).ok().flatten())
            .collect();
        // Per-channel SPL offset (= 94 - mic_sens_dbfs); `None` when the
        // channel hasn't been pistonphone-calibrated. Cached once at start
        // — re-running `calibrate_spl` requires a `monitor` restart, same
        // as voltage cal changes need today.
        let spl_offsets: Vec<Option<f64>> =
            cals.iter().map(|c| c.as_ref().and_then(Calibration::spl_offset_db)).collect();
        // Per-channel mic frequency-response curves (cloned out of `cals`
        // for cheap per-tick lookup). Same staleness caveat as above.
        let mic_curves: Vec<Option<ac_core::shared::calibration::MicResponse>> =
            cals.iter().map(|c| c.as_ref().and_then(|c| c.mic_response.clone())).collect();
        let mut eng = make_engine(fake);
        let start_port = in_ports_worker.first().map(String::as_str);
        if let Err(e) = eng.start(&[], start_port) {
            send_pub(&pub_tx, "error", &json!({"cmd":"monitor_spectrum","message":format!("{e}")}));
            return;
        }
        let sr = eng.sample_rate();
        // Per-channel mic-curve FIRs for the loudness path (#104). One
        // FIR per channel, built once at start when the curve is loaded,
        // bypassed when no curve or when the global toggle is off. The
        // FIR runs *before* K-weighting / dBTP so LKFS reflects the
        // mic-corrected acoustic level.
        let mut loudness_firs: Vec<Option<MicCurveFir>> = mic_curves
            .iter()
            .map(|c| c.as_ref().map(|curve| MicCurveFir::new(curve, sr, DEFAULT_N_TAPS)))
            .collect();
        let mut current_freqs: Vec<f64> = vec![freq_hz; channels_worker.len()];
        let mut xruns_total = 0u32;
        // Per-tick monotonic counter shared across all channels in the
        // tick. Phase 0b: the UI's Goniometer / PhaseScope3D pair L and
        // R scope frames by `frame_idx`, so it MUST increment exactly
        // once per tick — not once per (tick, channel). Wraps on u64
        // overflow (~600 years at 1 kHz tick rate; not a real concern).
        let mut frame_idx: u64 = 0;

        // #93: per-channel reconnect-failure state for the multi-channel
        // path. Single-channel never touches `eng.reconnect_input()` and
        // these slots stay zeroed.
        let mut reconnect_states: Vec<ReconnectState> =
            channels_worker.iter().map(|_| ReconnectState::new()).collect();

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
        let mut cwt_rings: Vec<std::collections::VecDeque<f32>> =
            channels_worker.iter().map(|_| std::collections::VecDeque::with_capacity(ring_cap)).collect();
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
        let cqt_f_min = ac_core::visualize::cqt::DEFAULT_F_MIN
            .max(ac_core::visualize::cqt::min_supported_f(cqt_ring_cap, sr, cqt_bpo));
        let cqt_freqs = ac_core::visualize::cqt::log_freqs(
            cqt_f_min,
            ac_core::visualize::cqt::default_f_max(sr),
            cqt_bpo,
        );
        let cqt_kernels = ac_core::visualize::cqt::build_kernels(
            &cqt_freqs, sr, cqt_bpo, cqt_ring_cap,
        );
        let mut cqt_rings: Vec<std::collections::VecDeque<f32>> = channels_worker
            .iter()
            .map(|_| std::collections::VecDeque::with_capacity(cqt_ring_cap))
            .collect();
        let mut cqt_mags: Vec<f32> = Vec::with_capacity(cqt_freqs.len());
        let mut cqt_log_counter = 0u32;

        // Reassigned-spectrogram state. One forward FFT plan + Hann
        // window plus its time-weighted and derivative variants are
        // pre-built; the live tick reuses them across frames. The output
        // grid is log-spaced (so the existing waterfall renders it
        // unchanged), with more bins than the FFT length so reassignment
        // can split closely-spaced peaks the FFT would smear together.
        let reass_n        = ac_core::visualize::reassigned::DEFAULT_N;
        let reass_n_out    = ac_core::visualize::reassigned::DEFAULT_N_OUT_BINS;
        // Reassigned tick paced from `monitor_params.interval` (#109).
        let reass_kernels  = ac_core::visualize::reassigned::build_kernels(
            reass_n, sr, reass_n_out,
            ac_core::visualize::reassigned::DEFAULT_F_MIN,
            ac_core::visualize::reassigned::default_f_max(sr),
        );
        let reass_freqs_out: Vec<f32> = reass_kernels.freqs_out.clone();
        let mut reass_rings: Vec<std::collections::VecDeque<f32>> = channels_worker
            .iter()
            .map(|_| std::collections::VecDeque::with_capacity(reass_n))
            .collect();
        let mut reass_mags: Vec<f32> = Vec::with_capacity(reass_n_out);
        let mut reass_log_counter = 0u32;

        // Sliding ring buffer for single-channel FFT path so refresh cadence
        // (`cur_interval`) can run faster than capture-window duration
        // (`cur_fft_n / sr`). Each tick pulls just the new samples that
        // arrived since the last tick, appends them, trims to the current
        // FFT-N, and analyses the full ring.
        let single_channel = channels_worker.len() == 1;
        let mut fft_rings: Vec<std::collections::VecDeque<f32>> =
            channels_worker.iter().map(|_| std::collections::VecDeque::with_capacity(131_072)).collect();

        // Per-channel time-integration state for the `fractional_octave_leq`
        // sidecar frame. `None` until the first fractional_octave frame at
        // the current mode + band count arrives. Reset on mode/band-count
        // change; Leq also reset on the `leq_reset_request` flag.
        let mut integrators: Vec<Option<Integrator>> =
            (0..channels_worker.len()).map(|_| None).collect();
        let mut last_frac_ts: Vec<Option<std::time::Instant>> =
            vec![None; channels_worker.len()];
        let mut cur_ti_mode: String =
            time_integration_shared.lock().unwrap().clone();

        // Per-channel BS.1770-5 / R128 loudness state — one mono-weighted
        // LoudnessState per monitored channel. Emits a `measurement/loudness`
        // sidecar frame each tick. Reset on `loudness_reset_request`.
        let mut loudness: Vec<LoudnessState> = channels_worker
            .iter()
            .map(|_| {
                LoudnessState::new_mono(sr)
                    .expect("sample_rate > 0 guaranteed by engine.sample_rate()")
            })
            .collect();

        while !stop.load(Ordering::Relaxed) {
            let tick_start = std::time::Instant::now();
            // Bump the per-tick counter and snapshot a tick-wide
            // timestamp BEFORE the per-channel loop so every scope
            // frame in this tick carries the same `frame_idx` /
            // `tick_ts_ns`. The existing per-channel `ts_ns` calls in
            // the loudness/spectrum branches stay as-is; only scope
            // frames need tick-wide alignment.
            frame_idx = frame_idx.wrapping_add(1);
            let tick_ts_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            let (cur_interval, cur_fft_n) = {
                let mp = monitor_params_shared.lock().unwrap();
                (mp.interval, mp.fft_n)
            };
            let mode = analysis_mode.lock().unwrap().clone();
            let is_cwt        = mode == "cwt";
            let is_cqt        = mode == "cqt";
            let is_reassigned = mode == "reassigned";

            // Time-integration bookkeeping — run once per tick.
            let new_ti_mode = time_integration_shared.lock().unwrap().clone();
            if new_ti_mode != cur_ti_mode {
                for slot in integrators.iter_mut() { *slot = None; }
                for slot in last_frac_ts.iter_mut() { *slot = None; }
                cur_ti_mode = new_ti_mode;
            }
            if leq_reset_shared.swap(false, Ordering::Relaxed) {
                for slot in integrators.iter_mut() {
                    if let Some(i) = slot { i.reset_if_leq(); }
                }
                for slot in last_frac_ts.iter_mut() { *slot = None; }
            }
            if loudness_reset_shared.swap(false, Ordering::Relaxed) {
                for l in loudness.iter_mut() {
                    l.reset();
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

            for (idx, &channel) in channels_worker.iter().enumerate() {
                if stop.load(Ordering::Relaxed) { break; }
                if channels_worker.len() > 1 {
                    if let Err(e) = eng.reconnect_input(&in_ports_worker[idx]) {
                        let now = std::time::Instant::now();
                        let st = &mut reconnect_states[idx];
                        st.note_failure(now);
                        if st.should_give_up(now) {
                            let outage_s = st
                                .first_failure_at
                                .map(|t0| now.duration_since(t0).as_secs())
                                .unwrap_or(0);
                            send_pub(&pub_tx, "error", &json!({
                                "cmd":     "monitor_spectrum",
                                "message": format!(
                                    "ch{channel} gave up after {outage_s}s of reconnect failures: {e}",
                                ),
                            }));
                            return;
                        }
                        if st.should_emit_error(now) {
                            send_pub(&pub_tx, "error", &json!({
                                "cmd":     "monitor_spectrum",
                                "message": format!(
                                    "reconnect ch{channel} (failures: {}): {e}",
                                    st.consecutive_failures,
                                ),
                            }));
                        }
                        let backoff = st.backoff();
                        if !backoff.is_zero() {
                            std::thread::sleep(backoff);
                        }
                        continue;
                    }
                    reconnect_states[idx].note_success();
                    eng.flush_capture();
                }
                if is_cwt {
                    // Pace the capture tick to the UI's requested
                    // interval, clamped to [16 ms, 100 ms]. Pre-#109 this
                    // was hardcoded 20 ms regardless of `--max-fps`,
                    // so CWT emitted at 50 fps even when the UI was
                    // capped at 30 — wasted work on both sides.
                    let cwt_tick = cur_interval.clamp(0.016, 0.100);
                    let samples = match eng.capture_block(cwt_tick) {
                        Ok(s) => s,
                        Err(e) => {
                            send_pub(&pub_tx, "error", &json!({
                                "cmd":     "monitor_spectrum",
                                "message": format!("capture error on ch{channel}: {e}"),
                            }));
                            return;
                        }
                    };
                    xruns_total += eng.xruns();
                    // Feed the raw capture into the loudness meter before
                    // any downstream consumers touch it.
                    push_loudness_with_optional_fir(
                        &mut loudness[idx], &mut loudness_firs[idx],
                        mic_corr_enabled.load(Ordering::Relaxed), &samples,
                    );
                    emit_scope_frame(
                        &pub_tx, channel, n_channels, sr, &samples,
                        frame_idx, tick_ts_ns, xruns_total,
                    );
                    let ring = &mut cwt_rings[idx];
                    ring.extend(samples.iter());
                    while ring.len() > ring_cap {
                        ring.pop_front();
                    }
                    if ring.len() < 256 {
                        continue;
                    }
                    let t0 = std::time::Instant::now();
                    let buf = ring.make_contiguous();
                    ac_core::visualize::cwt::morlet_cwt_into(
                        buf,
                        sr,
                        &cwt_scales,
                        cwt_sigma,
                        &mut cwt_mags,
                    );
                    cwt_log_counter += 1;
                    if cwt_log_counter % 50 == 1 {
                        eprintln!(
                            "cwt ch{channel}: {:.1}ms, ring={}, scales={}",
                            t0.elapsed().as_secs_f64() * 1000.0,
                            buf.len(),
                            cwt_scales.len(),
                        );
                    }
                    let ts_ns = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0);
                    let mc_enabled = mic_corr_enabled.load(Ordering::Relaxed);
                    if mc_enabled {
                        if let Some(curve) = &mic_curves[idx] {
                            apply_mic_curve_inplace_f32(curve, &cwt_freqs, &mut cwt_mags);
                        }
                    }
                    let mc_tag = mic_correction_tag(mic_curves[idx].is_some(), mc_enabled);
                    let frame = json!({
                        "type":            "visualize/cwt",
                        "cmd":             "monitor_spectrum",
                        "channel":         channel,
                        "n_channels":      n_channels,
                        "sr":              sr,
                        "magnitudes":      &cwt_mags,
                        "frequencies":     cwt_freqs,
                        "spl_offset_db":   spl_offsets[idx],
                        "mic_correction":  mc_tag,
                        "timestamp":       ts_ns,
                        "xruns":           xruns_total,
                    });
                    send_pub(&pub_tx, "data", &frame);
                    emit_loudness_frame(
                        &pub_tx, channel, n_channels, sr,
                        &loudness[idx], spl_offsets[idx], mc_tag,
                        ts_ns, xruns_total,
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
                                for (level, &fc) in band_levels.iter_mut().zip(band_centres.iter()) {
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
                            "spl_offset_db":  spl_offsets[idx],
                            "mic_correction": mc_tag,
                            "timestamp":      ts_ns,
                            "xruns":          xruns_total,
                        });
                        send_pub(&pub_tx, "data", &frac_frame);

                        if cur_ti_mode != "off" {
                            let n_bands = band_levels.len();
                            let slot = &mut integrators[idx];
                            // Re-init if the band count changed (e.g. live
                            // ioct_bpo toggle) or if this channel hasn't
                            // been primed yet.
                            if slot.as_ref().map(|i| i.n_bands() != n_bands).unwrap_or(true) {
                                *slot = Integrator::for_mode(&cur_ti_mode, n_bands);
                                last_frac_ts[idx] = None;
                            }
                            if let Some(integ) = slot.as_mut() {
                                let now = std::time::Instant::now();
                                let dt = last_frac_ts[idx]
                                    .map(|t| now.duration_since(t).as_secs_f64())
                                    .unwrap_or(cur_interval)
                                    .max(1e-6);
                                last_frac_ts[idx] = Some(now);
                                let levels_f64: Vec<f64> = band_levels.iter().map(|&v| v as f64).collect();
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
                                    "spl_offset_db":  spl_offsets[idx],
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
                    let cqt_tick = cur_interval.clamp(0.016, 0.100);
                    let samples = match eng.capture_block(cqt_tick) {
                        Ok(s) => s,
                        Err(e) => {
                            send_pub(&pub_tx, "error", &json!({
                                "cmd":     "monitor_spectrum",
                                "message": format!("capture error on ch{channel}: {e}"),
                            }));
                            return;
                        }
                    };
                    xruns_total += eng.xruns();
                    push_loudness_with_optional_fir(
                        &mut loudness[idx], &mut loudness_firs[idx],
                        mic_corr_enabled.load(Ordering::Relaxed), &samples,
                    );
                    emit_scope_frame(
                        &pub_tx, channel, n_channels, sr, &samples,
                        frame_idx, tick_ts_ns, xruns_total,
                    );
                    let ring = &mut cqt_rings[idx];
                    ring.extend(samples.iter());
                    while ring.len() > cqt_ring_cap {
                        ring.pop_front();
                    }
                    // The kernel for the lowest bin needs the full ring.
                    // Skip ticks until the ring has filled enough that the
                    // lowest kernel produces a finite reading; the bins
                    // above it produce earlier but a partial column would
                    // confuse the waterfall.
                    if ring.len() < cqt_kernels.max_kernel_len() {
                        continue;
                    }
                    let t0 = std::time::Instant::now();
                    let buf = ring.make_contiguous();
                    ac_core::visualize::cqt::cqt_into(buf, &cqt_kernels, &mut cqt_mags);
                    cqt_log_counter += 1;
                    if cqt_log_counter % 50 == 1 {
                        eprintln!(
                            "cqt ch{channel}: {:.1}ms, ring={}, bins={}",
                            t0.elapsed().as_secs_f64() * 1000.0,
                            buf.len(),
                            cqt_freqs.len(),
                        );
                    }
                    let ts_ns = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0);
                    let mc_enabled = mic_corr_enabled.load(Ordering::Relaxed);
                    if mc_enabled {
                        if let Some(curve) = &mic_curves[idx] {
                            apply_mic_curve_inplace_f32(curve, &cqt_freqs, &mut cqt_mags);
                        }
                    }
                    let mc_tag = mic_correction_tag(mic_curves[idx].is_some(), mc_enabled);
                    let frame = json!({
                        "type":           "visualize/cqt",
                        "cmd":            "monitor_spectrum",
                        "channel":        channel,
                        "n_channels":     n_channels,
                        "sr":             sr,
                        "bpo":            cqt_bpo,
                        "magnitudes":     &cqt_mags,
                        "frequencies":    cqt_freqs,
                        "spl_offset_db":  spl_offsets[idx],
                        "mic_correction": mc_tag,
                        "timestamp":      ts_ns,
                        "xruns":          xruns_total,
                    });
                    send_pub(&pub_tx, "data", &frame);
                    emit_loudness_frame(
                        &pub_tx, channel, n_channels, sr,
                        &loudness[idx], spl_offsets[idx], mc_tag,
                        ts_ns, xruns_total,
                    );
                    continue;
                }
                if is_reassigned {
                    let reass_tick = cur_interval.clamp(0.016, 0.100);
                    let samples = match eng.capture_block(reass_tick) {
                        Ok(s) => s,
                        Err(e) => {
                            send_pub(&pub_tx, "error", &json!({
                                "cmd":     "monitor_spectrum",
                                "message": format!("capture error on ch{channel}: {e}"),
                            }));
                            return;
                        }
                    };
                    xruns_total += eng.xruns();
                    push_loudness_with_optional_fir(
                        &mut loudness[idx], &mut loudness_firs[idx],
                        mic_corr_enabled.load(Ordering::Relaxed), &samples,
                    );
                    emit_scope_frame(
                        &pub_tx, channel, n_channels, sr, &samples,
                        frame_idx, tick_ts_ns, xruns_total,
                    );
                    let ring = &mut reass_rings[idx];
                    ring.extend(samples.iter());
                    while ring.len() > reass_n {
                        ring.pop_front();
                    }
                    if ring.len() < reass_n {
                        continue;
                    }
                    let t0 = std::time::Instant::now();
                    let buf = ring.make_contiguous();
                    ac_core::visualize::reassigned::reassigned_into(
                        buf, &reass_kernels, &mut reass_mags,
                    );
                    reass_log_counter += 1;
                    if reass_log_counter % 50 == 1 {
                        eprintln!(
                            "reassigned ch{channel}: {:.1}ms, n={}, bins={}",
                            t0.elapsed().as_secs_f64() * 1000.0,
                            buf.len(),
                            reass_freqs_out.len(),
                        );
                    }
                    let ts_ns = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0);
                    let mc_enabled = mic_corr_enabled.load(Ordering::Relaxed);
                    if mc_enabled {
                        if let Some(curve) = &mic_curves[idx] {
                            apply_mic_curve_inplace_f32(curve, &reass_freqs_out, &mut reass_mags);
                        }
                    }
                    let mc_tag = mic_correction_tag(mic_curves[idx].is_some(), mc_enabled);
                    let frame = json!({
                        "type":           "visualize/reassigned",
                        "cmd":            "monitor_spectrum",
                        "channel":        channel,
                        "n_channels":     n_channels,
                        "sr":             sr,
                        "magnitudes":     &reass_mags,
                        "frequencies":    reass_freqs_out,
                        "spl_offset_db":  spl_offsets[idx],
                        "mic_correction": mc_tag,
                        "timestamp":      ts_ns,
                        "xruns":          xruns_total,
                    });
                    send_pub(&pub_tx, "data", &frame);
                    emit_loudness_frame(
                        &pub_tx, channel, n_channels, sr,
                        &loudness[idx], spl_offsets[idx], mc_tag,
                        ts_ns, xruns_total,
                    );
                    continue;
                }

                // FFT path. Each channel has its own sliding ring so refresh
                // cadence (`cur_interval`) is decoupled from FFT window length
                // (`cur_fft_n`). Per-tick per-channel budget = interval / n_ch,
                // clamped to a sensible floor so JACK always has something to
                // hand back. Single-channel uses `capture_available` (non-
                // clearing drain on JACK, falls back to capture_block
                // elsewhere); multi-channel must use block capture because
                // `reconnect_input` clears the ring on every switch.
                let per_ch_budget = (cur_interval / channels_worker.len() as f64)
                    .max(0.002);
                let budget_samples = ((per_ch_budget * sr as f64) as usize)
                    .clamp(128, cur_fft_n as usize);
                let new = if single_channel {
                    match eng.capture_available(budget_samples) {
                        Ok(s) => s,
                        Err(e) => {
                            send_pub(&pub_tx, "error", &json!({
                                "cmd":     "monitor_spectrum",
                                "message": format!("capture error on ch{channel}: {e}"),
                            }));
                            return;
                        }
                    }
                } else {
                    match eng.capture_block(budget_samples as f64 / sr as f64) {
                        Ok(s) => s,
                        Err(e) => {
                            send_pub(&pub_tx, "error", &json!({
                                "cmd":     "monitor_spectrum",
                                "message": format!("capture error on ch{channel}: {e}"),
                            }));
                            return;
                        }
                    }
                };
                xruns_total += eng.xruns();
                // Loudness runs on the raw capture, independent of the
                // FFT-N sliding ring.
                push_loudness_with_optional_fir(
                    &mut loudness[idx], &mut loudness_firs[idx],
                    mic_corr_enabled.load(Ordering::Relaxed), &new,
                );
                emit_scope_frame(
                    &pub_tx, channel, n_channels, sr, &new,
                    frame_idx, tick_ts_ns, xruns_total,
                );
                let ring = &mut fft_rings[idx];
                ring.extend(new.iter());
                while ring.len() > cur_fft_n as usize {
                    ring.pop_front();
                }
                if ring.len() < 256 {
                    continue;
                }
                let samples = ring.make_contiguous();

                {
                    let analyze_result = ac_core::measurement::thd::analyze(samples, sr, current_freqs[idx], 10);
                    let mc_enabled = mic_corr_enabled.load(Ordering::Relaxed);
                    let mc_tag = mic_correction_tag(mic_curves[idx].is_some(), mc_enabled);
                    let frame = match analyze_result {
                        Ok(r) => {
                            current_freqs[idx] = r.fundamental_hz;
                            let cal = cals[idx].as_ref();
                            let in_dbu = cal
                                .and_then(|c| c.in_vrms(r.linear_rms))
                                .map(ac_core::shared::conversions::vrms_to_dbu);
                            // Per-bin dBFS → dBu conversion offset:
                            //   analog_vrms = sample_peak × cal_in / sqrt(2)   (sine assumption)
                            //   dBu = dbfs_peak + 20·log10(cal_in / (sqrt(2)·dbu_ref))
                            // UI overlays this on hover readouts so any cursor
                            // position shows dBFS / dBu / dBV without a round-trip.
                            let dbu_offset_db = cal.and_then(|c| c.vrms_at_0dbfs_in).map(|v| {
                                20.0 * (v / (std::f64::consts::SQRT_2
                                    * ac_core::shared::conversions::get_dbu_ref())).log10()
                            });
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
                            let peaks = ac_core::visualize::spectrum::find_interpolated_peaks(
                                &r.spectrum, &raw_freqs, 64,
                                r.fundamental_dbfs as f32 - 80.0,
                            );
                            let peaks_json: Vec<serde_json::Value> = peaks
                                .iter()
                                .map(|p| json!([p.freq_hz, p.dbfs]))
                                .collect();
                            let sr_f = sr as f64;
                            let (mut spec, freqs) = ac_core::visualize::aggregate::spectrum_to_columns_wire(
                                &r.spectrum,
                                sr_f,
                                20.0,
                                (sr_f / 2.0).max(21.0),
                                ac_core::visualize::aggregate::DEFAULT_WIRE_COLUMNS,
                            );
                            if mc_enabled {
                                if let Some(curve) = &mic_curves[idx] {
                                    apply_mic_curve_inplace_f64(curve, &freqs, &mut spec);
                                }
                            }
                            json!({
                                "type":             "visualize/spectrum",
                                "cmd":              "monitor_spectrum",
                                "channel":          channel,
                                "n_channels":       n_channels,
                                "freq_hz":          r.fundamental_hz,
                                "sr":               sr,
                                "freqs":            freqs,
                                "spectrum":         spec,
                                "peaks":            peaks_json,
                                "fundamental_dbfs": r.fundamental_dbfs,
                                "thd_pct":          r.thd_pct,
                                "thdn_pct":         r.thdn_pct,
                                "in_dbu":           in_dbu,
                                "dbu_offset_db":    dbu_offset_db,
                                "spl_offset_db":    spl_offsets[idx],
                                "mic_correction":   mc_tag,
                                "clipping":         r.clipping,
                                "xruns":            xruns_total,
                            })
                        }
                        Err(_) => {
                            let cal = cals[idx].as_ref();
                            let dbu_offset_db = cal.and_then(|c| c.vrms_at_0dbfs_in).map(|v| {
                                20.0 * (v / (std::f64::consts::SQRT_2
                                    * ac_core::shared::conversions::get_dbu_ref())).log10()
                            });
                            let (spec, _) = ac_core::visualize::spectrum::spectrum_only(samples, sr);
                            let sr_f = sr as f64;
                            let (mut spec, freqs) = ac_core::visualize::aggregate::spectrum_to_columns_wire(
                                &spec,
                                sr_f,
                                20.0,
                                (sr_f / 2.0).max(21.0),
                                ac_core::visualize::aggregate::DEFAULT_WIRE_COLUMNS,
                            );
                            if mc_enabled {
                                if let Some(curve) = &mic_curves[idx] {
                                    apply_mic_curve_inplace_f64(curve, &freqs, &mut spec);
                                }
                            }
                            json!({
                                "type":             "visualize/spectrum",
                                "cmd":              "monitor_spectrum",
                                "channel":          channel,
                                "n_channels":       n_channels,
                                "sr":               sr,
                                "freqs":            freqs,
                                "spectrum":         spec,
                                "dbu_offset_db":    dbu_offset_db,
                                "spl_offset_db":    spl_offsets[idx],
                                "mic_correction":   mc_tag,
                                "xruns":            xruns_total,
                            })
                        }
                    };
                    send_pub(&pub_tx, "data", &frame);
                    let ts_ns = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0);
                    emit_loudness_frame(
                        &pub_tx, channel, n_channels, sr,
                        &loudness[idx], spl_offsets[idx], mc_tag,
                        ts_ns, xruns_total,
                    );
                }
            }
            // Pace FFT mode to requested interval. CWT/CQT/reassigned have
            // their own cadence (short tick + sliding ring) and pace
            // themselves.
            if !is_cwt && !is_cqt && !is_reassigned {
                let elapsed = tick_start.elapsed().as_secs_f64();
                if elapsed < cur_interval {
                    std::thread::sleep(std::time::Duration::from_secs_f64(
                        cur_interval - elapsed,
                    ));
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
        "in_port":   primary_in_port,
        "in_ports":  in_ports,
        "channels":  channels,
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
        for _ in 0..4 { st.note_failure(now); }
        assert_eq!(st.backoff(), Duration::from_millis(500), "9th failure");

        st.note_failure(now);
        assert_eq!(st.backoff(), Duration::from_millis(1000), "10th failure caps at 1s");
        for _ in 0..50 { st.note_failure(now); }
        assert_eq!(st.backoff(), Duration::from_millis(1000), "stays capped");
    }

    #[test]
    fn note_success_resets_state() {
        let mut st = ReconnectState::new();
        let now = Instant::now();
        for _ in 0..7 { st.note_failure(now); }
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
        assert!(!st.should_emit_error(t_3), "0.9 s after last emit: suppressed");
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
