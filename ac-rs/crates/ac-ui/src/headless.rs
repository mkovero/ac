//! Headless T2/T3 display-truth harness — `ac-ui --headless-test` (#170).
//!
//! Extends `ac test software` with checks between "the numbers are right"
//! (T2: the post-receiver `DisplayFrame` buffer, exactly what `ChannelStore`
//! hands the renderer) and "the screen is right" (T3: pixels read back from
//! an offscreen wgpu texture rendered with the *real* `SpectrumRenderer` /
//! `EmberRenderer` production paint code, under software Vulkan/lavapipe or
//! `WGPU_BACKEND=gl`). See `handoff.md` at the repo root for the full I1-I4
//! invariant rationale this module implements.
//!
//! Never touches real hardware itself — it only knows how to talk to
//! whatever daemon endpoint it's given. `ac-cli`'s `run_software` is
//! responsible for pointing it at an isolated `ac-daemon --fake-audio`
//! subprocess (see `ac-cli/src/commands/test.rs`), never the user's
//! possibly-real daemon.
//!
//! Runs no winit event loop, opens no window, creates no `wgpu::Surface` —
//! `HeadlessGpu` requests a device directly from an adapter with
//! `compatible_surface: None`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::render_pipeline::{build_ember_spectrum_trace, ember_pack_cell};
use crate::data::control::CtrlClient;
use crate::data::receiver;
use crate::data::store::{
    ChannelStore, LoudnessStore, ScopeStore, SweepStore, TransferStore, VirtualChannelStore,
};
use crate::data::types::{CellView, DisplayConfig, DisplayFrame};
use crate::render::ember::EmberRenderer;
use crate::render::spectrum::{ChannelMeta, ChannelUpload, SpectrumRenderer};
use crate::ui::layout::CellRect;

struct Check {
    name: String,
    pass: bool,
    detail: String,
}

/// LF/HF crossover — reuse the same constant the real aggregator uses
/// rather than hardcoding a second copy (I1 requires "derive... from
/// current config, do not hardcode").
const LF_HF_CROSSOVER_HZ: f64 = ac_core::visualize::aggregate::DEFAULT_LF_CROSSOVER_HZ as f64;

/// FFT length requested for every harness capture. Fixed so the
/// interpolation→aggregation crossover derived from the returned `freqs`
/// grid (see `find_interp_aggregation_crossover`) is reproducible run to
/// run; matches the size used in the original HF-garbage field capture
/// (`tests/fixtures/fixtures-spectrum-hf-garbage/README.md`).
const HARNESS_FFT_N: u64 = 8192;

/// T2 tolerance for a single injected tone read back from the post-receiver
/// buffer. Rationale: Hann-window scalloping loss between bin centres is
/// ≤1.42 dB worst case (see `monitor_spectrum_wire_values_match_fake_tone`
/// in ac-daemon's it_protocol.rs, which already carries this exact number);
/// 1.5 dB gives a hair of margin without hiding a real multi-dB regression.
const I1_BUFFER_TOLERANCE_DB: f64 = 1.5;

/// T2 tolerance for "no value should exceed 0 dBFS" on bounded input.
/// Rationale: broadband noise can read a few hundredths of a dB above
/// nominal in a single bin from window-leakage constructive summation of
/// random phase (benign); 1.0 dB is well clear of that noise floor but
/// far below the +19 dB-class violation the HF-garbage fixture corpus
/// demonstrates (`tests/fixtures/fixtures-spectrum-hf-garbage/README.md`).
const I4_TOLERANCE_DB: f64 = 1.0;

/// T3 pixel tolerance, in rows, for locating a trace feature at its
/// expected position. Rationale: the issue's own risk note (architect
/// design comment, #170) flags that lavapipe's software rasterizer can
/// differ subtly from a real GPU at anti-aliased trace edges — assert on
/// the trace *feature* within a small pixel band, not an exact pixel.
const I3_PIXEL_TOLERANCE_PX: f32 = 2.0;

/// Fixed offscreen render target size for every T3 check. Resolution only
/// needs to be large enough that ±1-2 px maps to a meaningfully small dB
/// span; matches the ember substrate's own internal resolution
/// (`render/ember.rs::TEX_W/TEX_H`) so both views are checked at
/// comparable pixel density.
const T3_WIDTH: u32 = 1024;
const T3_HEIGHT: u32 = 512;
const T3_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Fixed dB/freq calibration window every T3 render uses. Not read from
/// live UI zoom/pan state (there is none here) — these are the harness's
/// own known-good axis calibration, independent of whatever the shader
/// actually draws, which is exactly what lets a T3 check catch an
/// orientation or scale bug in the shader rather than agreeing with it.
const WINDOW_DB_MIN: f32 = -90.0;
const WINDOW_DB_MAX: f32 = 0.0;
const WINDOW_FREQ_MIN: f32 = 20.0;
const WINDOW_FREQ_MAX: f32 = 20_000.0;

/// Entry point for `ac-ui --headless-test <ctrl_endpoint> <data_endpoint>`.
/// Returns the same `{ok, results: [{name, pass, detail}], all_pass}` shape
/// as `ac-daemon`'s `test_software`, so `ac-cli`'s `run_software` can print
/// both result sets through one loop.
pub fn run(ctrl_endpoint: &str, data_endpoint: &str) -> Value {
    let mut checks = Vec::new();
    match run_checked(ctrl_endpoint, data_endpoint, &mut checks) {
        Ok(()) => {}
        Err(e) => checks.push(Check {
            name: "display-truth harness".to_string(),
            pass: false,
            detail: format!("harness error: {e}"),
        }),
    }
    let all_pass = !checks.is_empty() && checks.iter().all(|c| c.pass);
    let results: Vec<Value> = checks
        .iter()
        .map(|c| json!({"name": c.name, "pass": c.pass, "detail": c.detail}))
        .collect();
    json!({"ok": true, "results": results, "all_pass": all_pass})
}

fn run_checked(
    ctrl_endpoint: &str,
    data_endpoint: &str,
    checks: &mut Vec<Check>,
) -> anyhow::Result<()> {
    let ctrl = CtrlClient::connect(ctrl_endpoint)?;
    let (inputs, mut store) = ChannelStore::new(1);
    let _receiver = receiver::spawn(
        data_endpoint.to_string(),
        inputs,
        TransferStore::new(),
        VirtualChannelStore::new(),
        SweepStore::new(),
        LoudnessStore::new(),
        ScopeStore::new(),
        None,
    );
    // Raw injected value, no EMA smoothing bias — I1/I4 compare against
    // the exact value the daemon put on the wire.
    let cfg = DisplayConfig {
        averaging_alpha: 1.0,
        ..Default::default()
    };

    // ── Capture 1: two tones straddling the LF/HF crossover (750 Hz) ──
    let f_lo = LF_HF_CROSSOVER_HZ * 0.8; // ~600 Hz
    let f_hi = LF_HF_CROSSOVER_HZ * 1.2; // ~900 Hz
    let frame1 = capture(
        &ctrl,
        &mut store,
        &cfg,
        json!({
            "cmd": "monitor_spectrum",
            "channels": [0],
            "fft_n": HARNESS_FFT_N,
            "fake_tones": [
                {"freq_hz": f_lo, "level_dbfs": -6.0},
                {"freq_hz": f_hi, "level_dbfs": -18.0},
            ],
        }),
        true,
    )?;
    check_i1_buffer(
        checks,
        &frame1,
        f_lo,
        -6.0,
        "I1 buffer @ LF/HF-crossover low side",
    );
    check_i1_buffer(
        checks,
        &frame1,
        f_hi,
        -18.0,
        "I1 buffer @ LF/HF-crossover high side",
    );
    check_i4_bounded(
        checks,
        &frame1,
        "I4 bounded output (two-tone LF/HF capture)",
    );

    // ── T3: render capture 1 through the real Spectrum + SpectrumEmber
    //    paint code and check pixel-level I1 apex + I3 orientation ──
    // Kept around (rather than dropped after this block) so the I5 soak
    // below can reuse the same adapter for its periodic T3 sampling
    // instead of standing up a second wgpu instance.
    let gpu = match HeadlessGpu::new() {
        Ok(gpu) => Some(gpu),
        Err(e) => {
            checks.push(Check {
                name: "T3 GPU adapter".to_string(),
                pass: false,
                detail: format!(
                    "no wgpu adapter available ({e}) — T3 checks require a software (lavapipe) \
                     or real Vulkan/GL adapter; see runbook, this is expected on a host with no \
                     GPU/driver stack"
                ),
            });
            None
        }
    };
    if let Some(gpu) = &gpu {
        run_t3_checks(checks, gpu, &frame1, f_lo, -6.0, f_hi, -18.0);
    }

    // ── Capture 2: two tones straddling the interpolation→aggregation
    //    crossover, derived from capture 1's own freq grid + HARNESS_FFT_N
    //    (not hardcoded — I1 requires deriving it from current config) ──
    let sr = frame1.meta.sr as f64;
    let interp_crossover =
        find_interp_aggregation_crossover(&frame1.freqs, sr, HARNESS_FFT_N).unwrap_or(6533.0); // fallback: documented field value, only used if the grid is too coarse to detect the transition
    let f_lo2 = interp_crossover * 0.9;
    let f_hi2 = interp_crossover * 1.1;
    let frame2 = capture(
        &ctrl,
        &mut store,
        &cfg,
        json!({
            "cmd": "monitor_spectrum",
            "channels": [0],
            "fft_n": HARNESS_FFT_N,
            "fake_tones": [
                {"freq_hz": f_lo2, "level_dbfs": -6.0},
                {"freq_hz": f_hi2, "level_dbfs": -12.0},
            ],
        }),
        false,
    )?;
    check_i1_buffer(
        checks,
        &frame2,
        f_lo2,
        -6.0,
        "I1 buffer @ interp/aggregation-crossover low side",
    );
    check_i1_buffer(
        checks,
        &frame2,
        f_hi2,
        -12.0,
        "I1 buffer @ interp/aggregation-crossover high side",
    );
    check_i4_bounded(
        checks,
        &frame2,
        "I4 bounded output (interp/aggregation capture)",
    );

    // ── Capture 3: calibrated broadband noise — I2 (reported, not
    //    asserted, per Decision 2) + I4 bounded ──
    let frame3 = capture(
        &ctrl,
        &mut store,
        &cfg,
        json!({
            "cmd": "monitor_spectrum",
            "channels": [0],
            "fft_n": HARNESS_FFT_N,
            "fake_noise_dbfs": -20.0,
        }),
        false,
    )?;
    check_i4_bounded(
        checks,
        &frame3,
        "I4 bounded output (broadband noise capture)",
    );
    check_i2_variance_report(checks, &frame3);

    // ── I5 soak: the temporal invariant (handoff.md) — snapshot checks
    // above settle, read one frame, judge. Any bug with onset delay
    // (ring-buffer wrap, EMA poisoning, cadence-boundary mishandling) is
    // structurally invisible to them. Run seeded broadband noise for
    // long enough to exceed every internal buffer period and assert on
    // every published frame, not just one.
    run_i5_soak(checks, &ctrl, &mut store, &cfg, gpu.as_ref())?;

    let _ = ctrl.send(&json!({"cmd": "stop"}));
    Ok(())
}

// ── I5 soak — temporal display-truth invariant (handoff.md) ──

/// Explicit floor the handoff calls out directly ("Run ≥ 15 s").
const I5_SOAK_MIN_FLOOR_S: f64 = 15.0;

/// The soak must exceed the LF window period by this multiple (handoff.md:
/// "must exceed every internal buffer period in the system by a margin").
/// At current defaults (65536/48000 ~= 1.365 s) this term (~13.6 s) is
/// under `I5_SOAK_MIN_FLOOR_S`, which is the binding floor today; this
/// term takes over automatically if `lf_fft_n` ever grows enough to need
/// more than 15 s to see a wrap with margin.
const I5_SOAK_WINDOW_MARGIN: f64 = 10.0;

/// Injected broadband-noise level for the soak stimulus. Clear of 0 dBFS
/// (I4-t headroom) and clear of the noise floor so collapse/garbage/drift
/// is unambiguous against this reference.
const I5_NOISE_DBFS: f64 = -20.0;

/// Number of post-settle frames averaged into the I5b plausibility
/// baseline before later frames are compared against it.
const I5_BASELINE_FRAMES: usize = 5;

/// I5a: LF content frozen for more than this many expected hops is a
/// liveness violation (handoff.md: "frozen LF beyond 2x expected hop").
const I5_LIVENESS_HOP_MULTIPLE: f64 = 2.0;

/// I5b tolerance: LF band mean (power-domain) vs. its own post-settle
/// baseline. Generous relative to the ~2.2-2.4 dB post-EMA sigma the
/// `#173` tuning test (`lf_ema_brings_variance_within_2x_of_hf_target`)
/// targets, so this catches collapse/garbage/drift, not EMA's own
/// expected residual variance.
const I5_PLAUSIBILITY_TOLERANCE_DB: f64 = 6.0;

/// I2-t tolerance for the LF/HF splice step on every frame. Generous
/// relative to the documented per-band sigma (HF ~0.7-2.4 dB, LF
/// post-EMA target ~2x that) since the two bands are independent
/// estimates that need not agree bin-for-bin, only avoid a gross jump.
const I5_CONTINUITY_TOLERANCE_DB: f64 = 8.0;

/// Consecutive out-of-tolerance frames required before continuity
/// declares a violation. A single-frame crossing of a statistical
/// tolerance is expected occasionally from broadband-noise chi-squared
/// tails even with no bug present (observed empirically: a lone 8.16 dB
/// splice step out of 327 frames in a 69 s run, against an 8 dB
/// tolerance); the handoff's own failure-mode taxonomy is about
/// *sustained* deviation (frozen / permanent-until-restart / cyclic at a
/// period), not a lone outlier, so requiring a short run confirms a real
/// break rather than flagging normal tail variance.
const I5_SUSTAINED_STREAK_FRAMES: u32 = 3;

/// I5b uses a rolling window rather than a strict consecutive streak: a
/// bug that keeps updating but with wrong information doesn't necessarily
/// stay out of tolerance on every single frame (a partially-correct or
/// oscillating readout can dip back into tolerance between bad samples),
/// which would reset a strict streak counter to zero and never fire. A
/// majority-of-window vote catches that pattern as well as a sustained
/// one, while a single blip in a `I5_PLAUSIBILITY_WINDOW`-sized window
/// still can't cross the majority threshold.
const I5_PLAUSIBILITY_WINDOW: usize = 10;
const I5_PLAUSIBILITY_WINDOW_MIN_VIOLATIONS: usize = 5;

/// Minimum number of observed LF-content-change intervals before I5c
/// judges the update rate — too few samples make a mean interval noisy.
const I5_RATE_MIN_SAMPLES: u32 = 5;

/// I5c: the LF band's actual update cadence (mean interval between
/// genuine content changes) must stay within this factor of the expected
/// hop in either direction. Loose relative to `I5_LIVENESS_HOP_MULTIPLE`
/// (2x, for "stopped changing entirely") because this check instead
/// catches a band that *keeps* updating but at the wrong rate — e.g.
/// recomputing every tick instead of every overlap-hop (losing the EMA
/// smoothing that makes the recompute cadence meaningful) or, in the
/// other direction, silently under-refreshing. Reported in the same
/// dump as a "wrong information" failure, not just "frozen" — a bug can
/// keep visibly updating and still be wrong (issue feedback: LF "continues
/// to update with wrong rate ... and wrong information").
const I5_RATE_TOLERANCE_FACTOR: f64 = 3.0;

/// Poll cadence for reading the store while soaking. Well under the
/// fastest internal cadence (LF hop ~136 ms at defaults) so no published
/// frame is skipped between polls.
const I5_POLL_INTERVAL_MS: u64 = 15;

/// Fixed LF column T3 samples each ~1 Hz tick. Safely mid-band (well clear
/// of the crossover and the plot's low-frequency edge) so a line-strip
/// renderer always has a preceding point to draw from.
const I5_T3_SAMPLE_FREQ_HZ: f64 = 100.0;

/// Cadence between T3 samples. Looser than 1 Hz since each sample re-runs
/// `render_ember_pixels`'s ~90-tick phosphor warm-up; periodic per the
/// handoff's own wording, not a hard 1 Hz requirement.
const I5_T3_SAMPLE_PERIOD_S: f64 = 3.0;

#[derive(Clone)]
struct SoakFrame {
    elapsed_s: f64,
    ts_ns: u64,
    freqs: Arc<Vec<f32>>,
    spectrum: Arc<Vec<f32>>,
}

struct SoakViolation {
    /// Which invariant tripped — used to route pass/fail to the right
    /// `Check` entry. One of "bounded" | "continuity" | "liveness" |
    /// "plausibility".
    source: &'static str,
    /// Descriptive failure-mode word for the dump/report — one of the
    /// four classes handoff.md asks QA to record (frozen / garbage /
    /// level-jump / drift-as-cyclic-recovering-proxy).
    class: &'static str,
    detail: String,
}

/// Split index: LF = `freqs[..split]`, HF = `freqs[split..]`.
fn lf_split(freqs: &[f32], crossover_hz: f64) -> usize {
    freqs
        .iter()
        .position(|&f| f as f64 >= crossover_hz)
        .unwrap_or(freqs.len())
}

/// Power-domain mean of a dBFS slice: `10*log10(mean(10^(v/10)))`, not a
/// bare mean of dB values — consistent with `EmaIntegrator`'s own
/// power-domain convention (`time_integration.rs`).
fn power_mean_db(vals: &[f32]) -> f64 {
    if vals.is_empty() {
        return f64::NEG_INFINITY;
    }
    let mean_pow = vals
        .iter()
        .map(|&v| 10f64.powf(v as f64 / 10.0))
        .sum::<f64>()
        / vals.len() as f64;
    10.0 * mean_pow.log10()
}

/// Incremental per-frame checker for the I5 soak — O(1) state per frame
/// rather than re-scanning full history, so the soak can run indefinitely
/// without unbounded memory. Checks run in a fixed priority order per
/// frame (bounded > continuity > liveness > plausibility); only the first
/// violation across the whole run is reported (handoff.md: "on first
/// violation").
struct SoakState {
    crossover_hz: f64,
    expected_hop_s: f64,
    settle_s: f64,
    bounded_tol_db: f64,
    continuity_tol_db: f64,
    liveness_hop_multiple: f64,
    plausibility_tol_db: f64,

    last_lf: Option<Vec<f32>>,
    last_change_elapsed_s: f64,
    baseline_samples: Vec<f64>,
    baseline_mean_db: Option<f64>,
    continuity_streak: u32,
    plausibility_window: std::collections::VecDeque<bool>,
    rate_interval_count: u32,
    rate_interval_sum_s: f64,

    checked_bounded: bool,
    checked_continuity: bool,
    checked_liveness: bool,
    checked_plausibility: bool,
    checked_rate: bool,
}

impl SoakState {
    fn new(crossover_hz: f64, expected_hop_s: f64, settle_s: f64) -> Self {
        Self {
            crossover_hz,
            expected_hop_s,
            settle_s,
            bounded_tol_db: I4_TOLERANCE_DB,
            continuity_tol_db: I5_CONTINUITY_TOLERANCE_DB,
            liveness_hop_multiple: I5_LIVENESS_HOP_MULTIPLE,
            plausibility_tol_db: I5_PLAUSIBILITY_TOLERANCE_DB,
            last_lf: None,
            last_change_elapsed_s: 0.0,
            baseline_samples: Vec::with_capacity(I5_BASELINE_FRAMES),
            baseline_mean_db: None,
            continuity_streak: 0,
            plausibility_window: std::collections::VecDeque::with_capacity(I5_PLAUSIBILITY_WINDOW),
            rate_interval_count: 0,
            rate_interval_sum_s: 0.0,
            checked_bounded: false,
            checked_continuity: false,
            checked_liveness: false,
            checked_plausibility: false,
            checked_rate: false,
        }
    }

    /// Returns the first violation found in `sf`, if any. Runs every
    /// sub-check that has enough state, marking each as "exercised" so the
    /// caller can report an honest "never triggered" vs. "never ran" if
    /// the soak stops early.
    fn check_frame(&mut self, sf: &SoakFrame) -> Option<SoakViolation> {
        // I4-t bounded: no value > 0 dBFS + tolerance, no NaN/Inf, ever.
        self.checked_bounded = true;
        for &v in sf.spectrum.iter() {
            if !v.is_finite() {
                return Some(SoakViolation {
                    source: "bounded",
                    class: "garbage",
                    detail: format!(
                        "non-finite value {v:?} in published frame at t={:.3}s",
                        sf.elapsed_s
                    ),
                });
            }
            if v as f64 > self.bounded_tol_db {
                return Some(SoakViolation {
                    source: "bounded",
                    class: "level-jump",
                    detail: format!(
                        "value {v:.3} dBFS exceeds bound (0 dBFS + {} dB tol) at t={:.3}s",
                        self.bounded_tol_db, sf.elapsed_s
                    ),
                });
            }
        }

        // I2-t continuity: LF/HF splice step bounded on every frame.
        let split = lf_split(&sf.freqs, self.crossover_hz);
        if split > 0 && split < sf.spectrum.len() {
            self.checked_continuity = true;
            let step = (sf.spectrum[split] - sf.spectrum[split - 1]).abs() as f64;
            if step > self.continuity_tol_db {
                self.continuity_streak += 1;
                if self.continuity_streak >= I5_SUSTAINED_STREAK_FRAMES {
                    return Some(SoakViolation {
                        source: "continuity",
                        class: "level-jump",
                        detail: format!(
                            "LF/HF splice step {step:.2} dB at t={t:.3}s exceeds tolerance \
                             ({tol} dB) for {streak} consecutive frames — LF col \
                             {lf_hz:.1}Hz={lf_db:.2}dBFS, HF col {hf_hz:.1}Hz={hf_db:.2}dBFS",
                            t = sf.elapsed_s,
                            tol = self.continuity_tol_db,
                            streak = self.continuity_streak,
                            lf_hz = sf.freqs[split - 1],
                            lf_db = sf.spectrum[split - 1],
                            hf_hz = sf.freqs[split],
                            hf_db = sf.spectrum[split],
                        ),
                    });
                }
            } else {
                self.continuity_streak = 0;
            }
        }

        let lf = &sf.spectrum[..split];
        if !lf.is_empty() {
            // I5a liveness: LF content must change within 2x expected hop.
            // Catches "stopped updating entirely" — a distinct failure
            // mode from I5c below, which catches "keeps updating, but at
            // the wrong rate."
            self.checked_liveness = true;
            let changed = match &self.last_lf {
                Some(prev) => {
                    prev.len() != lf.len() || prev.iter().zip(lf).any(|(a, b)| (a - b).abs() > 1e-4)
                }
                None => true,
            };
            if changed {
                let prev_change_elapsed_s = self.last_change_elapsed_s;
                let is_first_change = self.last_lf.is_none();
                self.last_change_elapsed_s = sf.elapsed_s;
                self.last_lf = Some(lf.to_vec());

                // I5c update rate: track the interval between genuine LF
                // content changes (not just whether it changed) once
                // settled. A bug that keeps visibly updating but at the
                // wrong cadence — e.g. recomputing every tick instead of
                // every overlap-hop, losing the EMA smoothing that makes
                // the cadence meaningful, or the opposite,
                // under-refreshing — is invisible to I5a (which only
                // fires on a full stop) but shows up here as a mean
                // interval far from `expected_hop_s`.
                if !is_first_change && sf.elapsed_s >= self.settle_s {
                    self.checked_rate = true;
                    let interval = sf.elapsed_s - prev_change_elapsed_s;
                    self.rate_interval_sum_s += interval;
                    self.rate_interval_count += 1;
                    if self.rate_interval_count >= I5_RATE_MIN_SAMPLES {
                        let mean_interval =
                            self.rate_interval_sum_s / self.rate_interval_count as f64;
                        let ratio = mean_interval / self.expected_hop_s;
                        if !(1.0 / I5_RATE_TOLERANCE_FACTOR..=I5_RATE_TOLERANCE_FACTOR)
                            .contains(&ratio)
                        {
                            return Some(SoakViolation {
                                source: "rate",
                                class: "wrong-rate",
                                detail: format!(
                                    "LF band update rate wrong at t={:.3}s: mean interval \
                                     between changes {mean_interval:.3}s over {} samples vs. \
                                     expected hop {:.3}s (ratio {ratio:.2}x, tolerance \
                                     {I5_RATE_TOLERANCE_FACTOR}x either way) — still updating, \
                                     just not at the right cadence",
                                    sf.elapsed_s, self.rate_interval_count, self.expected_hop_s,
                                ),
                            });
                        }
                    }
                }
            } else {
                let stale_for = sf.elapsed_s - self.last_change_elapsed_s;
                let bound = self.liveness_hop_multiple * self.expected_hop_s;
                if stale_for > bound {
                    return Some(SoakViolation {
                        source: "liveness",
                        class: "frozen",
                        detail: format!(
                            "LF band unchanged for {stale_for:.3}s (> {bound:.3}s = \
                             {}x expected hop {:.3}s) as of t={:.3}s",
                            self.liveness_hop_multiple, self.expected_hop_s, sf.elapsed_s
                        ),
                    });
                }
            }

            // I5b plausibility: LF band mean within tolerance of its own
            // post-settle baseline (the "known level" — the injected
            // noise's actual spectral-magnitude relationship isn't a
            // closed form independent of window/aggregation, so the
            // baseline is measured from this same run rather than
            // assumed; a bug that garbles/collapses/drifts the LF band
            // shows up as a departure from that self-measured reference
            // just the same). Uses a rolling-window majority vote, not a
            // strict consecutive streak: a band that keeps updating with
            // wrong information doesn't necessarily stay out of tolerance
            // on every single frame — an oscillating or partially-correct
            // readout can dip back into tolerance between bad samples,
            // which would reset a strict streak to zero and never fire.
            if sf.elapsed_s >= self.settle_s {
                self.checked_plausibility = true;
                let cur = power_mean_db(lf);
                match self.baseline_mean_db {
                    None => {
                        self.baseline_samples.push(cur);
                        if self.baseline_samples.len() >= I5_BASELINE_FRAMES {
                            let mean = self.baseline_samples.iter().sum::<f64>()
                                / self.baseline_samples.len() as f64;
                            self.baseline_mean_db = Some(mean);
                        }
                    }
                    Some(baseline) => {
                        let err = (cur - baseline).abs();
                        let out_of_tolerance = err > self.plausibility_tol_db;
                        self.plausibility_window.push_back(out_of_tolerance);
                        while self.plausibility_window.len() > I5_PLAUSIBILITY_WINDOW {
                            self.plausibility_window.pop_front();
                        }
                        let violations_in_window =
                            self.plausibility_window.iter().filter(|&&v| v).count();
                        if self.plausibility_window.len() >= I5_PLAUSIBILITY_WINDOW
                            && violations_in_window >= I5_PLAUSIBILITY_WINDOW_MIN_VIOLATIONS
                        {
                            return Some(SoakViolation {
                                source: "plausibility",
                                class: "drift",
                                detail: format!(
                                    "LF band mean {cur:.2} dBFS at t={:.3}s departs from \
                                     post-settle baseline {baseline:.2} dBFS by {err:.2} dB \
                                     (tol {} dB) — {violations_in_window}/{} recent frames out \
                                     of tolerance",
                                    sf.elapsed_s,
                                    self.plausibility_tol_db,
                                    self.plausibility_window.len(),
                                ),
                            });
                        }
                    }
                }
            }
        }
        None
    }
}

/// Dump frames N-1, N, N+1 (whatever is available) as CSVs alongside the
/// elapsed-time-to-violation, per handoff.md's "this artifact is the
/// debugging input for the fix issue" requirement. Returns the directory
/// written, logged by the caller.
fn dump_soak_violation(
    frames: &std::collections::VecDeque<SoakFrame>,
    violation: &SoakViolation,
) -> std::io::Result<std::path::PathBuf> {
    let base = std::env::var("AC_I5_DUMP_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("ac-i5-soak-dump"));
    let dir = base.join(format!("run-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;

    for (i, f) in frames.iter().enumerate() {
        let path = dir.join(format!("frame_{i}_t{:.3}s.csv", f.elapsed_s));
        let mut body = String::new();
        body.push_str(&format!("# elapsed_s={:.6}\n", f.elapsed_s));
        body.push_str(&format!("# daemon_ts_ns={}\n", f.ts_ns));
        body.push_str(&format!("# violation_class={}\n", violation.class));
        body.push_str(&format!("# violation_detail={}\n", violation.detail));
        body.push_str("freq_hz,dbfs\n");
        for (freq, db) in f.freqs.iter().zip(f.spectrum.iter()) {
            body.push_str(&format!("{freq},{db}\n"));
        }
        std::fs::write(&path, body)?;
    }
    Ok(dir)
}

/// Every-published-frame soak: seeded broadband noise run long enough to
/// exceed every internal buffer period (LF window, LF recompute hop, ring
/// capacity) with margin, asserting I4-t/I2-t/I5a/I5b on each frame as it
/// arrives. On the first violation, dumps N-1/N/N+1 and stops early
/// (still reports elapsed time run); otherwise runs the full floor
/// duration and reports each check green.
fn run_i5_soak(
    checks: &mut Vec<Check>,
    ctrl: &CtrlClient,
    store: &mut ChannelStore,
    cfg: &DisplayConfig,
    gpu: Option<&HeadlessGpu>,
) -> anyhow::Result<()> {
    let _ = ctrl.send(&json!({"cmd": "stop"}));
    std::thread::sleep(Duration::from_millis(200));
    let ack = ctrl.send(&json!({
        "cmd": "monitor_spectrum",
        "channels": [0],
        "fft_n": HARNESS_FFT_N,
        "fake_noise_dbfs": I5_NOISE_DBFS,
    }))?;
    if ack.get("ok").and_then(Value::as_bool) != Some(true) {
        anyhow::bail!("I5 soak: monitor_spectrum ack not ok: {ack}");
    }
    // Derived from the daemon's own ack, not hardcoded, so a future
    // config change (e.g. a larger lf_fft_n) is picked up automatically.
    let lf_fft_n = ack
        .get("lf_fft_n")
        .and_then(Value::as_f64)
        .unwrap_or(65536.0);
    let crossover_hz = ack
        .get("crossover_hz")
        .and_then(Value::as_f64)
        .unwrap_or(750.0);
    let lf_overlap_pct = ack
        .get("lf_overlap_pct")
        .and_then(Value::as_f64)
        .unwrap_or(90.0);
    let lf_avg_tau_ms = ack
        .get("lf_avg_tau_ms")
        .and_then(Value::as_f64)
        .unwrap_or(250.0);

    let poll_start = Instant::now();
    let init_deadline = poll_start + Duration::from_secs(10);
    let sr: f64;
    loop {
        if Instant::now() > init_deadline {
            anyhow::bail!("I5 soak: no initial frame within 10s");
        }
        std::thread::sleep(Duration::from_millis(20));
        if let Some(Some(f)) = store.read_all(cfg).into_iter().next() {
            if !f.spectrum.is_empty() {
                sr = f.meta.sr as f64;
                break;
            }
        }
    }

    let lf_window_s = lf_fft_n / sr;
    let expected_hop_s = lf_window_s * (1.0 - lf_overlap_pct / 100.0);
    let tau_s = lf_avg_tau_ms / 1000.0;
    let settle_s = (5.0 * tau_s).max(3.0 * expected_hop_s);
    let floor_s = I5_SOAK_MIN_FLOOR_S.max(I5_SOAK_WINDOW_MARGIN * lf_window_s);

    checks.push(Check {
        name: "I5 soak config".to_string(),
        pass: true,
        detail: format!(
            "sr={sr:.0}Hz lf_fft_n={lf_fft_n:.0} (window {lf_window_s:.3}s) \
             lf_overlap={lf_overlap_pct:.0}% (expected hop {expected_hop_s:.3}s) \
             lf_avg_tau={tau_s:.3}s crossover={crossover_hz:.0}Hz floor={floor_s:.1}s \
             settle={settle_s:.2}s noise={I5_NOISE_DBFS}dBFS (deterministic per-channel \
             LCG seed, see FakeEngine::noise_state — same seed replays identically)"
        ),
    });

    let mut state = SoakState::new(crossover_hz, expected_hop_s, settle_s);
    let mut ring: std::collections::VecDeque<SoakFrame> =
        std::collections::VecDeque::with_capacity(3);
    let mut violation: Option<SoakViolation> = None;
    let mut extra_needed: u32 = 0;
    let mut frame_count: usize = 0;
    let mut last_t3_sample_s = -10.0;
    let mut t3_worst_err_px: f32 = 0.0;
    let mut t3_samples: u32 = 0;
    let mut t3_failed = false;

    loop {
        let elapsed = poll_start.elapsed().as_secs_f64();
        if violation.is_some() && extra_needed == 0 {
            break;
        }
        if violation.is_none() && elapsed > floor_s {
            break;
        }
        std::thread::sleep(Duration::from_millis(I5_POLL_INTERVAL_MS));
        let Some(Some(f)) = store.read_all(cfg).into_iter().next() else {
            continue;
        };
        if f.new_row.is_none() || f.spectrum.is_empty() {
            continue;
        }
        let ts_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let sf = SoakFrame {
            elapsed_s: elapsed,
            ts_ns,
            freqs: f.freqs.clone(),
            spectrum: f.spectrum.clone(),
        };
        frame_count += 1;
        ring.push_back(sf.clone());
        while ring.len() > 3 {
            ring.pop_front();
        }

        if violation.is_none() {
            if let Some(v) = state.check_frame(&sf) {
                violation = Some(v);
                extra_needed = 1; // still want frame N+1 for the dump
            }
        } else {
            extra_needed = extra_needed.saturating_sub(1);
        }

        // T3: sample the real paint path periodically, best-effort — a
        // missing adapter must not fail the (GPU-independent) T2 soak
        // above it. Compares the rendered pixel row at a fixed, safely
        // mid-band LF column against the *buffer's own* reported value
        // for that same column (not an assumed absolute level) — this is
        // what "pixels track the buffer over time" means.
        //
        // Uses SpectrumEmber, not the plain Spectrum view: on real GPU
        // hardware (RADV, not lavapipe) `render_spectrum_pixels`'s I1/I3
        // checks independently fail (`find_trace_row` locks onto a row-0
        // border/gridline artifact rather than the data trace — a
        // pre-existing bug in that renderer's T3 path, unrelated to LF
        // temporal averaging, reproduced by the existing non-soak I1/I3
        // checks above and out of scope for this handoff). SpectrumEmber
        // renders correctly on the same hardware, so it's the reliable
        // choice for the soak's own T3 signal.
        //
        // Sampled every `I5_T3_SAMPLE_PERIOD_S` (looser than 1 Hz) because
        // each sample re-settles the phosphor substrate
        // (`render_ember_pixels`'s ~90-tick warm-up) — periodic per the
        // handoff's own wording ("e.g., 1 Hz"), not a hard requirement.
        if let Some(gpu) = gpu {
            if elapsed - last_t3_sample_s >= I5_T3_SAMPLE_PERIOD_S {
                last_t3_sample_s = elapsed;
                if let Some((_, buf_db)) = nearest_dbfs(&f, I5_T3_SAMPLE_FREQ_HZ) {
                    let pixels = render_ember_pixels(gpu, &f);
                    let col = freq_to_col(I5_T3_SAMPLE_FREQ_HZ);
                    match find_trace_row(&pixels, T3_WIDTH, T3_HEIGHT, col, [0, 0, 0]) {
                        Some(row) => {
                            let expected = expected_row_for_db(buf_db);
                            let err = (row as f32 - expected).abs();
                            t3_worst_err_px = t3_worst_err_px.max(err);
                            t3_samples += 1;
                        }
                        None => t3_failed = true,
                    }
                }
            }
        }
    }

    let elapsed_total = poll_start.elapsed().as_secs_f64();

    if let Some(v) = &violation {
        let dump_dir = dump_soak_violation(&ring, v);
        let dump_detail = match &dump_dir {
            Ok(dir) => format!("dumped frames N-1/N/N+1 to {}", dir.display()),
            Err(e) => format!("violation frame dump FAILED: {e}"),
        };
        checks.push(Check {
            name: "I5 soak — first violation".to_string(),
            pass: false,
            detail: format!(
                "class={} time-to-first-violation={:.3}s frames_observed={frame_count} — {} \
                 — {dump_detail}",
                v.class, elapsed_total, v.detail
            ),
        });
    }

    let early_stop_note = |name: &str, exercised: bool| -> String {
        if violation.is_some() {
            if exercised {
                format!(
                    "{name}: no violation seen before soak stopped early at t={elapsed_total:.3}s \
                     ({frame_count} frames) — see 'I5 soak — first violation' for what did trip"
                )
            } else {
                format!(
                    "{name}: never exercised — soak stopped at t={elapsed_total:.3}s before this \
                     check had enough state to run"
                )
            }
        } else {
            format!(
                "{name}: no violation across full soak, t={elapsed_total:.3}s, \
                 {frame_count} published frames observed"
            )
        }
    };

    checks.push(Check {
        name: "I4-t bounded output (soak, every frame)".to_string(),
        pass: violation.as_ref().is_none_or(|v| v.source != "bounded"),
        detail: early_stop_note("I4-t bounded", state.checked_bounded),
    });
    checks.push(Check {
        name: "I2-t continuity (soak, every frame)".to_string(),
        pass: violation.as_ref().is_none_or(|v| v.source != "continuity"),
        detail: early_stop_note("I2-t continuity", state.checked_continuity),
    });
    checks.push(Check {
        name: "I5a LF liveness".to_string(),
        pass: violation.as_ref().is_none_or(|v| v.source != "liveness"),
        detail: early_stop_note("I5a LF liveness", state.checked_liveness),
    });
    checks.push(Check {
        name: "I5b LF plausibility".to_string(),
        pass: violation
            .as_ref()
            .is_none_or(|v| v.source != "plausibility"),
        detail: early_stop_note("I5b LF plausibility", state.checked_plausibility),
    });
    checks.push(Check {
        name: "I5c LF update rate".to_string(),
        pass: violation.as_ref().is_none_or(|v| v.source != "rate"),
        detail: early_stop_note("I5c LF update rate", state.checked_rate),
    });

    // Gated only on an outright failure to locate any trace at all (a
    // render/crash-adjacent problem T3's other checks already cover) —
    // not on pixel-position precision. Unlike the clean two-tone T1/T3
    // captures above, a continuously-varying broadband-noise column
    // spreads the ember phosphor's brightest-deviation pixel across
    // several rows (blur from a genuinely busy, ever-changing trace, not
    // an isolated apex), so a tight px tolerance here would flag harmless
    // blur, not a regression. Reported for visibility per the same
    // "reported, not asserted" precedent as `check_i2_variance_report`.
    checks.push(Check {
        name: format!("I5 soak T3 sampling (every {I5_T3_SAMPLE_PERIOD_S:.0}s, SpectrumEmber)"),
        pass: !t3_failed,
        detail: if gpu.is_none() {
            "skipped — no wgpu adapter available (see 'T3 GPU adapter' check above)".to_string()
        } else {
            format!(
                "{t3_samples} samples @ {I5_T3_SAMPLE_FREQ_HZ:.0}Hz, worst pixel drift from \
                 buffer {t3_worst_err_px:.2}px (reported, not asserted — see rationale above), \
                 failed_to_locate_trace={t3_failed}"
            )
        },
    });

    Ok(())
}

/// Send `monitor_spectrum` with `cmd_extra` merged in, then poll the
/// receiver until a settled, non-empty frame arrives. `first` controls
/// whether a leading `stop` is sent first (skip on the very first capture
/// — nothing to stop yet).
fn capture(
    ctrl: &CtrlClient,
    store: &mut ChannelStore,
    cfg: &DisplayConfig,
    cmd: Value,
    first: bool,
) -> anyhow::Result<DisplayFrame> {
    if !first {
        let _ = ctrl.send(&json!({"cmd": "stop"}));
        std::thread::sleep(Duration::from_millis(200));
    }
    let reply = ctrl.send(&cmd)?;
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        anyhow::bail!("monitor_spectrum ack not ok: {reply}");
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    // Skip the first couple of ticks — the FFT ring may still be filling
    // (same convention as ac-daemon's it_protocol.rs wire tests).
    let mut seen = 0;
    loop {
        if Instant::now() > deadline {
            anyhow::bail!("no usable spectrum frame within 8 s");
        }
        std::thread::sleep(Duration::from_millis(100));
        let frames = store.read_all(cfg);
        if let Some(Some(f)) = frames.into_iter().next() {
            if !f.spectrum.is_empty() {
                seen += 1;
                if seen >= 3 {
                    return Ok(f);
                }
            }
        }
    }
}

/// Nearest-bin dBFS lookup — the harness's own independent readout, not a
/// call into any daemon/UI aggregation code, so a bug in that aggregation
/// can't hide from the comparison.
fn nearest_dbfs(frame: &DisplayFrame, target_hz: f64) -> Option<(f64, f64)> {
    let mut best: Option<(f64, f64, f64)> = None; // (freq, dbfs, |diff|)
    for (i, &f) in frame.freqs.iter().enumerate() {
        let diff = (f as f64 - target_hz).abs();
        let dbfs = *frame.spectrum.get(i)? as f64;
        if best.is_none_or(|(_, _, bd)| diff < bd) {
            best = Some((f as f64, dbfs, diff));
        }
    }
    best.map(|(f, db, _)| (f, db))
}

fn check_i1_buffer(
    checks: &mut Vec<Check>,
    frame: &DisplayFrame,
    target_hz: f64,
    level_dbfs: f64,
    name: &str,
) {
    let (name, pass, detail) = match nearest_dbfs(frame, target_hz) {
        Some((found_hz, found_db)) => {
            let err = (found_db - level_dbfs).abs();
            (
                name.to_string(),
                err <= I1_BUFFER_TOLERANCE_DB,
                format!(
                    "injected {level_dbfs:.1} dBFS @ {target_hz:.1} Hz, buffer column @ {found_hz:.1} \
                     Hz = {found_db:.2} dBFS (err {err:.2} dB, tol {I1_BUFFER_TOLERANCE_DB} dB)"
                ),
            )
        }
        None => (
            name.to_string(),
            false,
            format!("no buffer column found near {target_hz:.1} Hz"),
        ),
    };
    checks.push(Check { name, pass, detail });
}

fn check_i4_bounded(checks: &mut Vec<Check>, frame: &DisplayFrame, name: &str) {
    let max = frame.spectrum.iter().copied().fold(f32::MIN, f32::max) as f64;
    let pass = max <= I4_TOLERANCE_DB;
    checks.push(Check {
        name: name.to_string(),
        pass,
        detail: format!(
            "max buffer value = {max:.3} dBFS (bound: 0 dBFS + {I4_TOLERANCE_DB} dB tolerance)"
        ),
    });
}

/// I2 flat-noise continuity: per-band (LF < 750 Hz, HF ≥ 750 Hz)
/// column-to-column variance, reported only — Decision 2 (handoff.md)
/// defers the pass/fail threshold until LF temporal averaging lands and
/// the equalized target is known. This check therefore always reports
/// `pass: true`; its value is the number in `detail`, which becomes a
/// diff once that threshold exists.
fn check_i2_variance_report(checks: &mut Vec<Check>, frame: &DisplayFrame) {
    let mut lf_diffs = Vec::new();
    let mut hf_diffs = Vec::new();
    for w in frame.freqs.windows(2).zip(frame.spectrum.windows(2)) {
        let (fw, dw) = w;
        let d = (dw[1] - dw[0]).abs() as f64;
        if (fw[0] as f64) < LF_HF_CROSSOVER_HZ {
            lf_diffs.push(d);
        } else {
            hf_diffs.push(d);
        }
    }
    let variance = |v: &[f64]| -> f64 {
        if v.is_empty() {
            return 0.0;
        }
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len() as f64
    };
    let lf_var = variance(&lf_diffs);
    let hf_var = variance(&hf_diffs);
    checks.push(Check {
        name: "I2 flat-noise column-to-column variance (reported, not asserted)".to_string(),
        pass: true,
        detail: format!(
            "LF band (<{LF_HF_CROSSOVER_HZ:.0} Hz, n={}) step variance = {lf_var:.4} dB²; \
             HF band (n={}) step variance = {hf_var:.4} dB² — threshold deferred to LF \
             temporal averaging (handoff.md Decision 2)",
            lf_diffs.len(),
            hf_diffs.len(),
        ),
    });
}

/// Find the column index where the aggregator's source-bin count first
/// goes from 1 (interpolation) to ≥2 (aggregation) — i.e. where adjacent
/// output columns stop being spaced one raw FFT bin apart. Mirrors the
/// mechanism documented in `audit/spectrum-hf-garbage-report.md` without
/// depending on any internal aggregator function: it only reads the
/// `freqs` grid the daemon already put on the wire plus the requested
/// `sr`/`fft_n`, so a bug in the aggregator's own crossover math can't
/// mask itself from this derivation.
fn find_interp_aggregation_crossover(freqs: &[f32], sr: f64, fft_n: u64) -> Option<f64> {
    let bin_width = sr / fft_n as f64;
    for w in freqs.windows(2) {
        let spacing = (w[1] - w[0]) as f64;
        if spacing >= bin_width * 1.5 {
            return Some(w[0] as f64);
        }
    }
    None
}

struct HeadlessGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl HeadlessGpu {
    fn new() -> anyhow::Result<Self> {
        pollster::block_on(Self::new_async())
    }

    async fn new_async() -> anyhow::Result<Self> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY | wgpu::Backends::GL,
            ..Default::default()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| anyhow::anyhow!("no wgpu adapter"))?;
        let info = adapter.get_info();
        log::info!(
            "ac-ui --headless-test: wgpu backend={:?} adapter={:?} driver={:?}",
            info.backend,
            info.name,
            info.driver,
        );
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("ac-ui headless-test device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: wgpu::MemoryHints::default(),
                },
                None,
            )
            .await?;
        Ok(Self { device, queue })
    }
}

fn make_offscreen_texture(device: &wgpu::Device, label: &str) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: T3_WIDTH,
            height: T3_HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: T3_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// Read an offscreen texture back to unpadded, channel-order-corrected
/// RGBA bytes. Reuses the exact same de-pad / channel-swap helpers the
/// interactive 'S' screenshot path uses (`ui/export.rs`), so this isn't a
/// second, possibly-diverging implementation of that bookkeeping.
fn readback(gpu: &HeadlessGpu, tex: &wgpu::Texture, mut encoder: wgpu::CommandEncoder) -> Vec<u8> {
    let bytes_per_row = crate::ui::export::bytes_per_row_aligned(T3_WIDTH);
    let size = (bytes_per_row as u64) * (T3_HEIGHT as u64);
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("t3 readback"),
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(T3_HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: T3_WIDTH,
            height: T3_HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(Some(encoder.finish()));
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    // This is where a driver crash (rather than a wgpu validation error)
    // would surface — the CPU-side recording above is always accepted;
    // `poll` is what actually asks the adapter to execute the command
    // buffer. If `ac-ui --headless-test` dies with no further log output
    // past a "rendering ... view" line, look here first.
    let _ = gpu.device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .expect("map_async callback channel closed")
        .expect("map_async failed");
    let data = slice.get_mapped_range().to_vec();
    buffer.unmap();
    let rgba = crate::ui::export::unpad(&data, T3_WIDTH, T3_HEIGHT, bytes_per_row);
    crate::ui::export::channel_swap_if_needed(rgba, T3_FORMAT)
}

fn render_spectrum_pixels(gpu: &HeadlessGpu, frame: &DisplayFrame) -> Vec<u8> {
    log::info!("t3: rendering Spectrum view offscreen");
    let mut renderer = SpectrumRenderer::new(&gpu.device, T3_FORMAT);
    let meta = ChannelMeta {
        color: [1.0, 1.0, 1.0, 1.0],
        viewport: [0.0, 0.0, 1.0, 1.0],
        db_min: WINDOW_DB_MIN,
        db_max: WINDOW_DB_MAX,
        freq_log_min: WINDOW_FREQ_MIN.max(1.0).log10(),
        freq_log_max: WINDOW_FREQ_MAX.max(WINDOW_FREQ_MIN * 1.001).log10(),
        n_bins: 0,
        offset: 0,
        fill_alpha: 0.0,
        line_width: 3.0,
    };
    renderer.upload(
        &gpu.device,
        &gpu.queue,
        &[ChannelUpload {
            spectrum: (*frame.spectrum).clone(),
            meta,
        }],
    );
    let tex = make_offscreen_texture(&gpu.device, "t3 spectrum");
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("t3 spectrum encoder"),
        });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("t3 spectrum pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        renderer.draw(&mut pass);
    }
    readback(gpu, &tex, encoder)
}

/// Render `frame` through the real SpectrumEmber paint path
/// (`build_ember_spectrum_trace` + `ember_pack_cell`, the exact functions
/// `render_pipeline.rs` calls for a Single-cell SpectrumEmber view) and
/// let the phosphor substrate settle to steady state before reading back —
/// a single deposit at production intensity is too dim to threshold
/// reliably (the intensity constant is tuned for multi-tick steady state,
/// see the comment at its call site in `render_pipeline.rs`).
fn render_ember_pixels(gpu: &HeadlessGpu, frame: &DisplayFrame) -> Vec<u8> {
    log::info!("t3: rendering SpectrumEmber view offscreen");
    let view = CellView {
        freq_min: WINDOW_FREQ_MIN,
        freq_max: WINDOW_FREQ_MAX,
        db_min: WINDOW_DB_MIN,
        db_max: WINDOW_DB_MAX,
        ..Default::default()
    };
    let raw = build_ember_spectrum_trace(&frame.freqs, &frame.spectrum, &view, 1.0);
    let full_cell = CellRect {
        channel: 0,
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };
    let mut polyline = Vec::with_capacity(raw.len());
    ember_pack_cell(&mut polyline, &raw, &full_cell, &view, 1.0);

    let mut ember = EmberRenderer::new(&gpu.device, &gpu.queue, T3_FORMAT);
    ember.set_tau_p(1.2);
    ember.set_intensity(0.006);
    ember.set_tone(0.6, 1.5);

    let tex = make_offscreen_texture(&gpu.device, "t3 ember");
    let view_tex = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("t3 ember encoder"),
        });
    // Same static polyline every tick so the substrate reaches the same
    // steady-state luminance a continuously-running UI would settle to.
    const TICKS: usize = 90; // ~1.5 s at 60 fps
    for _ in 0..TICKS {
        ember.advance(
            &gpu.device,
            &gpu.queue,
            &mut encoder,
            [0.0, 0.0, 1.0, 1.0],
            &polyline,
            0.0,
            1.0 / 60.0,
            &[],
        );
    }
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("t3 ember display pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view_tex,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        ember.draw(&mut pass);
    }
    readback(gpu, &tex, encoder)
}

fn freq_to_col(freq_hz: f64) -> u32 {
    let log_min = (WINDOW_FREQ_MIN as f64).max(1.0).log10();
    let log_max = (WINDOW_FREQ_MAX as f64)
        .max(WINDOW_FREQ_MIN as f64 * 1.001)
        .log10();
    let xn = ((freq_hz.max(1.0).log10() - log_min) / (log_max - log_min).max(1e-6)).clamp(0.0, 1.0);
    ((xn * T3_WIDTH as f64) as u32).min(T3_WIDTH - 1)
}

fn expected_row_for_db(db: f64) -> f32 {
    let n = ((db - WINDOW_DB_MIN as f64) / (WINDOW_DB_MAX as f64 - WINDOW_DB_MIN as f64).max(1e-3))
        .clamp(0.0, 1.0);
    ((1.0 - n) * (T3_HEIGHT as f64 - 1.0)) as f32
}

/// Scan column `col` for the row with the greatest colour deviation from
/// `bg` — the trace/phosphor "apex" for that column. Pure pixel-buffer
/// analysis, independent of how the buffer was produced (GPU readback or
/// a hand-built test fixture), which is what makes it unit-testable
/// without a GPU below.
fn find_trace_row(pixels: &[u8], width: u32, height: u32, col: u32, bg: [u8; 3]) -> Option<u32> {
    let mut best_row = None;
    let mut best_dist = 24u32; // ignore near-background noise / AA fringing
    for row in 0..height {
        let idx = ((row * width + col) * 4) as usize;
        if idx + 2 >= pixels.len() {
            continue;
        }
        let dist = (pixels[idx] as i32 - bg[0] as i32).unsigned_abs()
            + (pixels[idx + 1] as i32 - bg[1] as i32).unsigned_abs()
            + (pixels[idx + 2] as i32 - bg[2] as i32).unsigned_abs();
        if dist > best_dist {
            best_dist = dist;
            best_row = Some(row);
        }
    }
    best_row
}

#[allow(clippy::too_many_arguments)]
fn run_t3_checks(
    checks: &mut Vec<Check>,
    gpu: &HeadlessGpu,
    frame: &DisplayFrame,
    f_lo: f64,
    db_lo: f64,
    f_hi: f64,
    db_hi: f64,
) {
    let spectrum_px = render_spectrum_pixels(gpu, frame);
    let ember_px = render_ember_pixels(gpu, frame);

    for (view_name, pixels, bg) in [
        ("Spectrum", &spectrum_px, [0u8, 0, 0]),
        ("SpectrumEmber", &ember_px, [0u8, 0, 0]),
    ] {
        let col_lo = freq_to_col(f_lo);
        let col_hi = freq_to_col(f_hi);
        let row_lo = find_trace_row(pixels, T3_WIDTH, T3_HEIGHT, col_lo, bg);
        let row_hi = find_trace_row(pixels, T3_WIDTH, T3_HEIGHT, col_hi, bg);

        match (row_lo, row_hi) {
            (Some(rl), Some(rh)) => {
                // db_lo is louder than db_hi in every capture 1 uses this
                // helper for, so its trace must land at the smaller row
                // (higher on screen) — I3, no exemptions.
                let pass = if db_lo > db_hi { rl < rh } else { rh < rl };
                checks.push(Check {
                    name: format!("I3 orientation ({view_name})"),
                    pass,
                    detail: format!(
                        "{f_lo:.0} Hz @ {db_lo} dBFS -> row {rl}; {f_hi:.0} Hz @ {db_hi} dBFS -> row {rh} \
                         (smaller row = higher on screen; louder tone must be smaller)"
                    ),
                });

                if view_name == "Spectrum" {
                    let exp_lo = expected_row_for_db(db_lo);
                    let exp_hi = expected_row_for_db(db_hi);
                    let err_lo = (rl as f32 - exp_lo).abs();
                    let err_hi = (rh as f32 - exp_hi).abs();
                    checks.push(Check {
                        name: "I1 pixel apex (Spectrum)".to_string(),
                        pass: err_lo <= I3_PIXEL_TOLERANCE_PX && err_hi <= I3_PIXEL_TOLERANCE_PX,
                        detail: format!(
                            "{f_lo:.0} Hz: expected row {exp_lo:.1}, found {rl} (err {err_lo:.1} px); \
                             {f_hi:.0} Hz: expected row {exp_hi:.1}, found {rh} (err {err_hi:.1} px); \
                             tol {I3_PIXEL_TOLERANCE_PX} px"
                        ),
                    });
                }
            }
            _ => {
                checks.push(Check {
                    name: format!("I3 orientation ({view_name})"),
                    pass: false,
                    detail: format!(
                        "could not locate a trace feature at one or both columns \
                         (row_lo={row_lo:?}, row_hi={row_hi:?})"
                    ),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_bg(width: u32, height: u32) -> Vec<u8> {
        vec![0u8; (width * height * 4) as usize]
    }

    fn set_px(pixels: &mut [u8], width: u32, col: u32, row: u32, rgb: [u8; 3]) {
        let idx = ((row * width + col) * 4) as usize;
        pixels[idx] = rgb[0];
        pixels[idx + 1] = rgb[1];
        pixels[idx + 2] = rgb[2];
        pixels[idx + 3] = 255;
    }

    #[test]
    fn find_trace_row_locates_bright_pixel_against_background() {
        let (w, h) = (16, 16);
        let mut px = solid_bg(w, h);
        set_px(&mut px, w, 4, 9, [255, 255, 255]);
        let row = find_trace_row(&px, w, h, 4, [0, 0, 0]);
        assert_eq!(row, Some(9));
    }

    #[test]
    fn find_trace_row_none_when_column_is_all_background() {
        let (w, h) = (16, 16);
        let px = solid_bg(w, h);
        assert_eq!(find_trace_row(&px, w, h, 4, [0, 0, 0]), None);
    }

    /// Harness self-test (#170 acceptance criterion): a deliberately
    /// introduced Y-flip must be caught. This directly exercises the same
    /// orientation logic `run_t3_checks` uses, on hand-built pixel data
    /// standing in for a GPU readback — no adapter required.
    #[test]
    fn self_test_y_flip_is_caught_by_orientation_check() {
        let (w, h) = (16, 16);
        let col = 4;
        let bg = [0u8, 0, 0];

        // Correct orientation: louder (db_lo=-6) trace at smaller row than
        // quieter (db_hi=-18) trace.
        let mut correct = solid_bg(w, h);
        set_px(&mut correct, w, col, 3, [255, 255, 255]); // louder -> near top
        let row_louder = find_trace_row(&correct, w, h, col, bg).unwrap();
        assert!(
            row_louder < h / 2,
            "sanity: louder trace should be in upper half"
        );

        // Deliberately flipped: same data, mirrored vertically — this is
        // exactly the class of bug I3/T3 exists to catch (the ember Y
        // inversion, #170 handoff.md).
        let mut flipped = solid_bg(w, h);
        set_px(&mut flipped, w, col, h - 1 - 3, [255, 255, 255]);
        let row_flipped = find_trace_row(&flipped, w, h, col, bg).unwrap();

        let pass_correct = row_louder < h / 2;
        let pass_flipped = row_flipped < h / 2;
        assert!(
            pass_correct,
            "correct orientation must pass the 'louder = higher' check"
        );
        assert!(
            !pass_flipped,
            "flipped orientation must fail the 'louder = higher' check"
        );
    }

    /// Harness self-test (#170 acceptance criterion): a deliberately
    /// introduced +6 dB offset must be caught at T2 (buffer). Fabricates a
    /// `DisplayFrame` with a known error and confirms `check_i1_buffer`
    /// flags it — no daemon required.
    #[test]
    fn self_test_plus_6db_offset_is_caught_at_t2() {
        use crate::data::types::FrameMeta;
        use std::sync::Arc;

        let frame = DisplayFrame {
            freqs: Arc::new(vec![1000.0]),
            // Injected level was -20 dBFS; buffer reports -14 dBFS — a +6
            // dB offset bug (the class handoff.md cites: aggregate.rs
            // double-dB-conversion).
            spectrum: Arc::new(vec![-14.0]),
            meta: FrameMeta {
                freq_hz: 1000.0,
                fundamental_dbfs: -14.0,
                thd_pct: 0.0,
                thdn_pct: 0.0,
                in_dbu: None,
                dbu_offset_db: None,
                peaks: Arc::new(Vec::new()),
                spl_offset_db: None,
                mic_correction: None,
                sr: 48_000,
                clipping: false,
                xruns: 0,
                leq_duration_s: None,
            },
            new_row: None,
        };
        let mut checks = Vec::new();
        check_i1_buffer(&mut checks, &frame, 1000.0, -20.0, "self-test +6dB");
        assert_eq!(checks.len(), 1);
        assert!(
            !checks[0].pass,
            "a +6 dB offset ({:.1} dB error) must fail I1's {I1_BUFFER_TOLERANCE_DB} dB tolerance: {}",
            6.0,
            checks[0].detail,
        );
    }

    #[test]
    fn nearest_dbfs_picks_closest_bin() {
        use crate::data::types::FrameMeta;
        use std::sync::Arc;
        let frame = DisplayFrame {
            freqs: Arc::new(vec![100.0, 500.0, 1000.0, 2000.0]),
            spectrum: Arc::new(vec![-40.0, -30.0, -20.0, -10.0]),
            meta: FrameMeta {
                freq_hz: 1000.0,
                fundamental_dbfs: -20.0,
                thd_pct: 0.0,
                thdn_pct: 0.0,
                in_dbu: None,
                dbu_offset_db: None,
                peaks: Arc::new(Vec::new()),
                spl_offset_db: None,
                mic_correction: None,
                sr: 48_000,
                clipping: false,
                xruns: 0,
                leq_duration_s: None,
            },
            new_row: None,
        };
        let (f, db) = nearest_dbfs(&frame, 950.0).unwrap();
        assert_eq!(f, 1000.0);
        assert_eq!(db, -20.0);
    }

    #[test]
    fn find_interp_aggregation_crossover_detects_spacing_jump() {
        // Synthetic grid: one-bin-wide spacing (0.5 Hz) up to col 100, then
        // a jump to 5x spacing — the aggregation branch's signature.
        let bin_width = 0.5_f64;
        let mut freqs = Vec::new();
        for i in 0..100 {
            freqs.push((i as f64 * bin_width) as f32);
        }
        let last = freqs.last().copied().unwrap() as f64;
        for i in 0..20 {
            freqs.push((last + i as f64 * bin_width * 5.0) as f32);
        }
        let sr = bin_width * 8192.0; // so sr/fft_n == bin_width
        let crossover = find_interp_aggregation_crossover(&freqs, sr, 8192).unwrap();
        assert!((crossover as f32 - freqs[99]).abs() < 0.01);
    }

    /// I4 must-fail regression corpus (#170 handoff.md / Decision 3
    /// validation gate): `ac monitor`'s own CSV export format, parsed with
    /// no daemon or GPU involved, must trip the same bounded-output check
    /// `check_i4_bounded` uses on a live capture. These five fixtures are
    /// the documented aggregate.rs double-dB-conversion bug's field
    /// signature (`tests/fixtures/fixtures-spectrum-hf-garbage/README.md`)
    /// — 876 violations per file above ~6533 Hz, up to +19.115 dBFS.
    #[test]
    fn i4_fixture_corpus_known_bad_csvs_fail_bounded_output() {
        // CARGO_MANIFEST_DIR = .../ac-rs/crates/ac-ui; fixtures live at the
        // outer repo root's tests/ (a sibling of ac-rs/, not inside it —
        // see ../../CLAUDE.md's "Repo structure").
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tests/fixtures/fixtures-spectrum-hf-garbage");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("could not read fixture dir {}: {e}", dir.display()))
        {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("csv") {
                continue;
            }
            let body = std::fs::read_to_string(&path).unwrap();
            let max = max_dbfs_in_csv(&body);
            assert!(
                max > 0.0,
                "{}: expected a known-bad fixture (max > 0 dBFS), got max={max:.3} — \
                 if the underlying bug is now fixed, this fixture should move to a \
                 known-good reference set instead of silently passing here",
                path.display(),
            );
            checked += 1;
        }
        assert_eq!(
            checked,
            5,
            "expected exactly 5 known-bad fixtures in {}",
            dir.display()
        );
    }

    /// Minimal reader for the `ac monitor` CSV export shape (`# `-prefixed
    /// metadata, then `freq_hz,ch0_dbfs,ch0_mic_corrected,...`). Only
    /// extracts the max dBFS across every `*_dbfs` column — enough for the
    /// I4 bounded-output check, not a general-purpose CSV reader.
    fn max_dbfs_in_csv(body: &str) -> f64 {
        let mut lines = body.lines().filter(|l| !l.starts_with('#'));
        let header = lines.next().unwrap_or("");
        let dbfs_cols: Vec<usize> = header
            .split(',')
            .enumerate()
            .filter(|(_, name)| name.ends_with("_dbfs"))
            .map(|(i, _)| i)
            .collect();
        let mut max = f64::MIN;
        for line in lines {
            let cols: Vec<&str> = line.split(',').collect();
            for &i in &dbfs_cols {
                if let Some(v) = cols.get(i).and_then(|s| s.parse::<f64>().ok()) {
                    max = max.max(v);
                }
            }
        }
        max
    }

    // ── I5 soak self-test (handoff.md acceptance criterion) ──
    //
    // "A deliberately introduced delayed-onset fault ... is caught by I5
    // and NOT by I1-I4" — exercised here as a pure unit test against
    // `SoakState` directly (no daemon, no GPU), the same style as the
    // T3 self-tests above. Builds a synthetic run where the LF band
    // behaves correctly for a while (varies frame to frame, like real
    // noise-driven content) and then freezes solid — a ring-wrap /
    // state-poisoning class of bug with onset delay, structurally
    // invisible to a single-frame snapshot check.

    fn synthetic_soak_frame(elapsed_s: f64, lf: &[f32], hf: &[f32]) -> SoakFrame {
        let mut freqs = Vec::new();
        let mut spectrum = Vec::new();
        for (i, &v) in lf.iter().enumerate() {
            freqs.push(100.0 + i as f32 * 100.0); // 100..700 Hz, below crossover
            spectrum.push(v);
        }
        for (i, &v) in hf.iter().enumerate() {
            freqs.push(800.0 + i as f32 * 100.0); // 800..1400 Hz, above crossover
            spectrum.push(v);
        }
        SoakFrame {
            elapsed_s,
            ts_ns: (elapsed_s * 1e9) as u64,
            freqs: Arc::new(freqs),
            spectrum: Arc::new(spectrum),
        }
    }

    #[test]
    fn self_test_delayed_onset_freeze_caught_by_i5_not_by_a_snapshot_check() {
        const CROSSOVER_HZ: f64 = 750.0;
        const EXPECTED_HOP_S: f64 = 0.137;
        const DT_S: f64 = 0.05;
        const FREEZE_AT_FRAME: usize = 10;
        const TOTAL_FRAMES: usize = 30;

        // Close to the LF baseline so the LF/HF splice step (I2-t) never
        // trips — this test isolates the liveness (I5a) invariant.
        let hf = vec![-40.0f32; 7];
        let mut state = SoakState::new(CROSSOVER_HZ, EXPECTED_HOP_S, /* settle_s */ 1000.0);
        let mut frozen_lf: Option<Vec<f32>> = None;
        let mut violation_frame: Option<(usize, SoakViolation)> = None;
        let mut last_frame: Option<SoakFrame> = None;

        for i in 0..TOTAL_FRAMES {
            let elapsed = i as f64 * DT_S;
            let lf: Vec<f32> = if i < FREEZE_AT_FRAME {
                // Varies frame to frame, like real noise-driven LF content.
                (0..7)
                    .map(|k| -40.0 + (i as f32 * 0.7 + k as f32).sin() * 3.0)
                    .collect()
            } else {
                // Ring-wrap / state-poisoning stand-in: identical to
                // whatever the last pre-freeze frame was, forever after.
                frozen_lf
                    .get_or_insert_with(|| {
                        (0..7)
                            .map(|k| {
                                -40.0 + ((FREEZE_AT_FRAME - 1) as f32 * 0.7 + k as f32).sin() * 3.0
                            })
                            .collect()
                    })
                    .clone()
            };
            let sf = synthetic_soak_frame(elapsed, &lf, &hf);
            if violation_frame.is_none() {
                if let Some(v) = state.check_frame(&sf) {
                    violation_frame = Some((i, v));
                }
            }
            last_frame = Some(sf);
        }

        let (frame_idx, violation) = violation_frame
            .unwrap_or_else(|| panic!("I5 should have caught the delayed-onset freeze"));
        assert_eq!(violation.source, "liveness");
        assert_eq!(violation.class, "frozen");
        // Must not fire before the freeze actually starts, and must fire
        // with real delay (this is the "onset delay" I1-I4 can't see) —
        // not on the very first post-freeze frame either, since a single
        // stale reading is expected while the ring is still draining the
        // last good hop.
        assert!(
            frame_idx > FREEZE_AT_FRAME,
            "violation at frame {frame_idx} fired suspiciously early (freeze starts at \
             frame {FREEZE_AT_FRAME})"
        );

        // The I1/I4-style snapshot check: one frame, judged in isolation.
        // The last (frozen) frame's values are individually finite and
        // bounded — there is nothing in a single frame that reveals the
        // freeze, which is exactly why I1-I4 passed on the real bug this
        // harness was built to catch (handoff.md).
        let last = last_frame.unwrap();
        let snapshot_looks_fine = last
            .spectrum
            .iter()
            .all(|&v| v.is_finite() && v as f64 <= I4_TOLERANCE_DB);
        assert!(
            snapshot_looks_fine,
            "sanity: the frozen frame must look individually valid to a snapshot check — \
             otherwise this isn't testing what I5 adds over I1-I4"
        );
    }

    /// Field feedback on this harness: the real bug doesn't necessarily
    /// freeze — it can "continue to update with wrong rate ... and wrong
    /// information as well." That pattern is invisible to I5a (which only
    /// fires when the LF band *stops* changing) and could slip past a
    /// naive per-frame plausibility check too if the wrong values
    /// oscillate rather than sitting still. Builds a synthetic run where
    /// the LF band changes on every single frame (never frozen) at ~7x
    /// the expected hop rate, and where the post-settle values are
    /// consistently offset from the pre-settle baseline — proving I5c
    /// (rate) and I5b (plausibility) catch it while confirming I5a would
    /// not have (the violation source must not be "liveness").
    #[test]
    fn self_test_updating_but_wrong_rate_and_wrong_info_not_caught_by_liveness_alone() {
        const CROSSOVER_HZ: f64 = 750.0;
        const EXPECTED_HOP_S: f64 = 0.137;
        const DT_S: f64 = 0.02; // ~7x faster than the expected hop
        const SETTLE_S: f64 = 1.0;
        const TOTAL_FRAMES: usize = 200;

        let mut state = SoakState::new(CROSSOVER_HZ, EXPECTED_HOP_S, SETTLE_S);
        let mut violation: Option<SoakViolation> = None;

        for i in 0..TOTAL_FRAMES {
            let elapsed = i as f64 * DT_S;
            // Before settle: correct-ish level, jittering normally.
            // After settle: "wrong information" — offset hard from the
            // pre-settle baseline, but still changing every frame (never
            // frozen), and always at DT_S cadence (the wrong rate). HF
            // tracks the same base as LF throughout so I2-t continuity
            // (a different invariant) never trips — this test isolates
            // I5c/I5b.
            let base = if elapsed < SETTLE_S { -40.0 } else { -15.0 };
            let hf = vec![base; 7];
            let lf: Vec<f32> = (0..7)
                .map(|k| base + (i as f32 * 1.3 + k as f32).sin() * 2.0)
                .collect();
            let sf = synthetic_soak_frame(elapsed, &lf, &hf);
            if violation.is_none() {
                violation = state.check_frame(&sf);
            }
        }

        let violation = violation
            .unwrap_or_else(|| panic!("I5 should have caught the wrong-rate/wrong-info run"));
        assert_ne!(
            violation.source, "liveness",
            "I5a (frozen-only) must not be the one catching this — the band never stopped \
             changing, which is exactly the gap this test guards against"
        );
        assert!(
            violation.source == "rate" || violation.source == "plausibility",
            "expected I5c (rate) or I5b (plausibility) to catch it, got source={:?}: {}",
            violation.source,
            violation.detail
        );
    }
}
