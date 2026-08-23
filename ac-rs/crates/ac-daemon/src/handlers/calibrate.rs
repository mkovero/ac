//! Calibrate state machine: plays reference tone, prompts for output and
//! input Vrms readings, writes cal.json. Routes the worker's terminal frame
//! to `done` or `error` based on the save outcome.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::measurement::sweep::{
    deconvolve_full, extract_irs, inverse_sweep, log_sweep, SweepParams,
};
use ac_core::shared::calibration::{
    compare_tau_readings, Calibration, MicResponse, TauComparison, TauConditions, TauEntry,
};

use crate::audio::{make_engine, AudioEngine};
use crate::server::ServerState;

use super::{
    apply_drive_ceiling, busy_guard, capture_rms, read_dmm_vrms, resolve_input, resolve_output,
    rms_to_dbfs, send_pub, spawn_worker, wait_cal_reply, CalReply,
};

/// Apply one prompt's reply to one stored voltage field and report which
/// of the three states the field ends up in.
///
/// `"measured"` — a reading was supplied and scaled into the field.
/// `"unchanged"` — nothing was supplied; the previously stored value stands.
/// `"absent"` — the field holds no value, either because it never did or
/// because the reply asked for it to be cleared.
///
/// Skipping must never write. That is the whole of #279: the old code
/// assigned `reading.map(..)` unconditionally, so a skipped prompt wrote
/// `None` over a good calibration and reported it as "not measured".
fn apply_cal_reading(field: &mut Option<f64>, reply: CalReply, scale: f64) -> &'static str {
    match reply {
        CalReply::Value(v) => {
            *field = Some(v * scale);
            "measured"
        }
        CalReply::Clear => {
            *field = None;
            "absent"
        }
        CalReply::Skip => {
            if field.is_some() {
                "unchanged"
            } else {
                "absent"
            }
        }
    }
}

/// Method tag stored on every [`TauEntry`] this handler produces. Bumped
/// to `_v2` by #340: the window-sizing change below means a τ captured
/// under the old, uncapped-in-time window (which pinned wrong past
/// ~13–43 ms depending on sample rate, silently) must not go on matching
/// current conditions via `Calibration::tau_for`'s exact lookup — the
/// window it was measured with was never part of `TauConditions`, so the
/// method tag is the only thing that can invalidate it.
const TAU_METHOD: &str = "farina_short_ess_v2";

/// Short-ESS parameters for the τ measurement (#281). Deliberately much
/// shorter than a `plot_ir` sweep — this only has to locate the linear-IR
/// peak, not resolve harmonics or a decay tail, so `n_harmonics = 1` on
/// the `extract_irs` call below. That matters for the window bound: with
/// `n_harmonics == 1` there is no neighbouring order, so
/// `per_order_window_lens`'s harmonic-gap clamp (`sweep.rs`) never runs —
/// it does not bind here, only on multi-harmonic callers like `plot_ir`.
///
/// The bound that actually fires is the requested window itself, so it
/// must be sized directly to the round trip this needs to measure rather
/// than left as a fixed sample count (#340): `TAU_WINDOW_LEN` used to be a
/// `usize`, so its *time* value shrank as sample rate rose — 85 ms at
/// 48 kHz (looked generous) but only 42.67 ms at 96 kHz, 1.03× short of
/// this rig's measured 43.75 ms τ, and wrong at every rate in between.
/// `TAU_MIN_HALF_WINDOW_S` fixes that: it is a *time* bound, converted to
/// a sample window at measurement time, so the measurable ceiling is the
/// same 50 ms at every sample rate. `TAU_TAIL_S` (150 ms) stays well
/// above it so the capture always holds the whole window regardless of
/// sample rate.
const TAU_F1_HZ: f64 = 100.0;
const TAU_DURATION_S: f64 = 0.2;
const TAU_TAIL_S: f64 = 0.15;
/// Half-width of the τ measurement window, in seconds (AC5 of #340). The
/// largest round trip `measure_tau` can report is just under this value.
const TAU_MIN_HALF_WINDOW_S: f64 = 0.05;
/// Fraction of the half-window treated as "too close to the edge to
/// trust" (AC4 of #340). A peak this close to either edge is
/// indistinguishable from one pinned by an arrival outside the window
/// entirely, so it is refused rather than reported. Not derived from rig
/// data — a threshold newly introduced by this change; may need revisiting
/// once measured against real noise floors.
const TAU_EDGE_MARGIN_FRAC: f64 = 0.10;

/// Rig-instrument overrides for the two τ window constants (#350).
///
/// Compiled in only under the `tau-window-override` feature, which is off
/// by default, so a production daemon cannot be perturbed by its
/// environment. The rig needs them because the only lever hardware has on
/// edge proximity is τ itself, and τ moves in period-sized steps
/// (44.5 %, 33.8 %, 12.5 %, then off the end of the window) — there is no
/// way to sample between 0 % and the shipped 10 % margin that way. Moving
/// the *window* while the round trip stays fixed samples it continuously,
/// inside one JACK client lifetime, which also keeps #347's one-period
/// jump out of the comparison.
#[cfg(feature = "tau-window-override")]
fn tau_env_f64(key: &str, default: f64) -> f64 {
    let raw = match std::env::var(key) {
        Ok(v) => v,
        Err(_) => return default,
    };
    match raw.trim().parse::<f64>() {
        Ok(x) if x.is_finite() && x >= 0.0 => x,
        _ => {
            eprintln!(
                "calibrate: {key}={raw:?} is not a finite non-negative number —                  ignoring it and using {default}"
            );
            default
        }
    }
}

#[cfg(feature = "tau-window-override")]
fn tau_half_window_s() -> f64 {
    tau_env_f64("AC_TAU_HALF_WINDOW_S", TAU_MIN_HALF_WINDOW_S)
}

#[cfg(not(feature = "tau-window-override"))]
fn tau_half_window_s() -> f64 {
    TAU_MIN_HALF_WINDOW_S
}

#[cfg(feature = "tau-window-override")]
fn tau_edge_margin_frac() -> f64 {
    tau_env_f64("AC_TAU_EDGE_MARGIN_FRAC", TAU_EDGE_MARGIN_FRAC)
}

#[cfg(not(feature = "tau-window-override"))]
fn tau_edge_margin_frac() -> f64 {
    TAU_EDGE_MARGIN_FRAC
}

/// Per-reading τ diagnostic (#350). `measure_tau` reports only the peak
/// position, so nothing on this path has ever recorded the SNR the peak
/// was located against — which is the quantity #350 exists to measure.
/// `floor` is defined exactly as `it_loopback_ir` and `ir_probe` define
/// it (max |x| over the leading eighth of the window) so the numbers
/// compare directly against #277's record.
#[cfg(feature = "tau-window-override")]
fn tau_probe_log(
    ir: &[f64],
    peak_idx: usize,
    peak_abs: f64,
    window_len: usize,
    half: usize,
    sr: u32,
) {
    let far_end = (ir.len() / 8).max(1);
    let floor = ir[..far_end]
        .iter()
        .map(|v| v.abs())
        .fold(0.0_f64, f64::max);
    let snr_db = 20.0 * (peak_abs / floor.max(1e-15)).log10();
    let margin_frac = tau_edge_margin_frac();
    let margin = (margin_frac * half as f64).round() as usize;
    let dist_from_end = window_len.saturating_sub(1).saturating_sub(peak_idx);
    let edge_frac = dist_from_end as f64 / half as f64;
    let offset = peak_idx as i64 - half as i64;
    eprintln!("--- tau probe (#350) ---");
    eprintln!("sample_rate:   {sr} Hz");
    eprintln!(
        "half_window:   {half} samples = {:.4} ms",
        half as f64 * 1000.0 / sr as f64
    );
    eprintln!("window_len:    {window_len} samples");
    eprintln!("peak_index:    {peak_idx}");
    eprintln!("peak_abs:      {peak_abs:.6e}");
    eprintln!("floor_abs:     {floor:.6e}  (max |x| over leading {far_end} samples)");
    eprintln!("snr_db:        {snr_db:.2}");
    eprintln!("dist_from_end: {dist_from_end} samples");
    eprintln!("edge_frac:     {edge_frac:.4}  (margin_frac {margin_frac} = {margin} samples)");
    eprintln!(
        "tau:           {offset:+} samples = {:+.4} ms",
        offset as f64 * 1000.0 / sr as f64
    );
    eprintln!("------------------------");
}

/// Refuse a peak sitting within `margin_frac` of the half-window of
/// either edge of a `window_len`-sample gate. Pulled out of `measure_tau`
/// so the edge case can be driven directly in tests without an
/// `AudioEngine` (#340 AC4).
fn check_peak_within_window(
    peak_idx: usize,
    window_len: usize,
    margin_frac: f64,
) -> anyhow::Result<()> {
    let half = window_len / 2;
    let margin = (margin_frac * half as f64).round() as usize;
    let dist_from_start = peak_idx;
    let dist_from_end = window_len.saturating_sub(1).saturating_sub(peak_idx);
    if dist_from_start <= margin || dist_from_end <= margin {
        anyhow::bail!(
            "\u{3c4} peak at sample {peak_idx} of a {window_len}-sample window (half-width \
             {half} samples) sits within {margin} samples of the window edge — the arrival is \
             likely outside the window rather than at this position, so no value is reported"
        );
    }
    Ok(())
}

/// Play a short ESS, deconvolve it, and return the interface round-trip
/// delay in seconds (peak of the linear IR, converted from samples).
///
/// Reuses the Farina machinery from `ac_core::measurement::sweep` exactly
/// as `plot_ir` does — see `handlers/audio/plot.rs` for the longer-form
/// version of the same technique.
fn measure_tau(eng: &mut dyn AudioEngine, amp: f64) -> anyhow::Result<f64> {
    let sr = eng.sample_rate();
    let f2_hz = (sr as f64 * 0.45).min(20_000.0);
    let params = SweepParams {
        f1_hz: TAU_F1_HZ,
        f2_hz,
        duration_s: TAU_DURATION_S,
        sample_rate: sr,
    };
    let sweep = log_sweep(&params)?;
    let amp = amp as f32;
    let scaled: Vec<f32> = sweep.iter().map(|&s| s * amp).collect();
    let captured = eng.play_and_capture(&scaled, TAU_TAIL_S)?;
    let inv = inverse_sweep(&params)?;
    let full = deconvolve_full(&captured, &inv);
    let half_window_s = tau_half_window_s();
    if 2.0 * half_window_s > TAU_TAIL_S {
        anyhow::bail!(
            "\u{3c4} half-window {half_window_s} s needs a {:.4} s gate but the capture tail is \
             only {TAU_TAIL_S} s \u{2014} the window would run off the end of the capture",
            2.0 * half_window_s
        );
    }
    let half = (half_window_s * sr as f64).ceil() as usize;
    let window_len = 2 * half;
    let irs = extract_irs(&full, &params, 1, window_len)?;
    let (peak_idx, peak_val) = irs
        .linear
        .iter()
        .enumerate()
        .map(|(i, v)| (i, *v))
        .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
        .ok_or_else(|| anyhow::anyhow!("empty IR from τ sweep"))?;
    #[cfg(feature = "tau-window-override")]
    tau_probe_log(&irs.linear, peak_idx, peak_val.abs(), window_len, half, sr);
    #[cfg(not(feature = "tau-window-override"))]
    let _ = peak_val;
    check_peak_within_window(peak_idx, window_len, tau_edge_margin_frac())?;
    let offset_samples = peak_idx as i64 - half as i64;
    Ok(offset_samples as f64 / sr as f64)
}

/// Outcome of one independent τ lifecycle attempt (#347): either both
/// readings were taken and compared, or a lifecycle itself failed (engine
/// start / measurement error) before a comparison was possible.
enum TauAttempt {
    Compared {
        conditions: TauConditions,
        reading1_s: f64,
        reading2_s: f64,
        comparison: TauComparison,
    },
    Error {
        conditions: Option<TauConditions>,
        message: String,
    },
}

/// Run `measure_tau` twice, each inside its own fresh engine lifecycle —
/// `make_engine` → `start` → `measure_tau` → `stop` — and compare the two
/// readings (#347). Each call is a genuinely new JACK client registration
/// (`JackEngine::start` calls `Client::new` fresh every time), which is
/// exactly the boundary the one-period bug lives on: within one lifetime
/// both a real `jack_iodelay` client and `ac-daemon`'s own workers are
/// stable to 0.001 frames, so nothing short of a second, separately
/// re-registered client can catch a shift that only shows up between them.
fn measure_tau_twice(
    fake: bool,
    device: u32,
    out_port: &str,
    in_port: &str,
    amp: f64,
) -> TauAttempt {
    let run_once = || -> anyhow::Result<(f64, TauConditions)> {
        let mut eng = make_engine(fake);
        eng.start(std::slice::from_ref(&out_port.to_string()), Some(in_port))?;
        let conditions = TauConditions {
            device,
            backend: eng.backend_name().to_string(),
            sample_rate: eng.sample_rate(),
            period_size: eng.period_size(),
            output_port: out_port.to_string(),
            input_port: in_port.to_string(),
        };
        let reading = measure_tau(&mut *eng, amp);
        eng.set_silence();
        eng.stop();
        reading.map(|t| (t, conditions))
    };

    let (reading1_s, conditions) = match run_once() {
        Ok(r) => r,
        Err(e) => {
            return TauAttempt::Error {
                conditions: None,
                message: format!("\u{3c4} measurement failed (reading 1 of 2): {e}"),
            }
        }
    };
    let (reading2_s, conditions2) = match run_once() {
        Ok(r) => r,
        Err(e) => {
            return TauAttempt::Error {
                conditions: Some(conditions),
                message: format!("\u{3c4} measurement failed (reading 2 of 2): {e}"),
            }
        }
    };
    let comparison = compare_tau_readings(
        reading1_s,
        reading2_s,
        conditions2.sample_rate,
        conditions2.period_size,
    );
    TauAttempt::Compared {
        conditions: conditions2,
        reading1_s,
        reading2_s,
        comparison,
    }
}

/// Everything `calibrate` reports about this run's τ attempt — feeds both
/// the `cal_done` wire frame and, on a corroborated result, the `TauEntry`
/// appended to `tau_history`. One reading is never a storable outcome
/// (#347): `state == "measured"` only when two independent lifecycles
/// agreed, and `tau_s` / `agreement_count` are `Some` only in that case.
struct TauOutcome {
    state: &'static str,
    conditions: Option<TauConditions>,
    tau_s: Option<f64>,
    agreement_count: Option<u32>,
    reading1_s: Option<f64>,
    reading2_s: Option<f64>,
    delta_samples: Option<i64>,
    periods: Option<i64>,
    error: Option<String>,
}

impl TauOutcome {
    fn not_measured_no_loopback() -> Self {
        Self {
            state: "not_measured_no_loopback",
            conditions: None,
            tau_s: None,
            agreement_count: None,
            reading1_s: None,
            reading2_s: None,
            delta_samples: None,
            periods: None,
            error: None,
        }
    }
}

/// Turn the loopback flag established at step 2 into the [`TauOutcome`]
/// `calibrate` reports — the exact decision #281 QA flagged as untestable
/// because it was inlined in the worker closure, reachable only through a
/// full daemon spawn. `attempt` is only called when `is_loopback`, matching
/// the worker's original behaviour of never running the τ sweep on a run
/// with no loopback detected.
fn tau_result(is_loopback: bool, attempt: impl FnOnce() -> TauAttempt) -> TauOutcome {
    if !is_loopback {
        return TauOutcome::not_measured_no_loopback();
    }
    match attempt() {
        TauAttempt::Error {
            conditions,
            message,
        } => TauOutcome {
            state: "error",
            conditions,
            tau_s: None,
            agreement_count: None,
            reading1_s: None,
            reading2_s: None,
            delta_samples: None,
            periods: None,
            error: Some(message),
        },
        TauAttempt::Compared {
            conditions,
            reading1_s,
            reading2_s,
            comparison: TauComparison::Agree,
        } => TauOutcome {
            state: "measured",
            conditions: Some(conditions),
            // Both readings agreed to the whole sample — average them
            // rather than picking one arbitrarily.
            tau_s: Some((reading1_s + reading2_s) / 2.0),
            agreement_count: Some(2),
            reading1_s: Some(reading1_s),
            reading2_s: Some(reading2_s),
            // ZMQ.md: `tau_delta_samples` is present only on disagree_* —
            // a healthy Agree run must not serialize the field at all
            // (QA #348 correctness 1).
            delta_samples: None,
            periods: None,
            error: None,
        },
        TauAttempt::Compared {
            conditions,
            reading1_s,
            reading2_s,
            comparison: TauComparison::Disagree(d),
        } => TauOutcome {
            state: if d.periods.is_some() {
                "disagree_period_shift"
            } else {
                "disagree_other"
            },
            conditions: Some(conditions),
            tau_s: None,
            agreement_count: None,
            reading1_s: Some(reading1_s),
            reading2_s: Some(reading2_s),
            delta_samples: Some(d.delta_samples),
            periods: d.periods,
            error: Some(d.message()),
        },
    }
}

pub fn calibrate(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "calibrate");
    let cfg = state.cfg.lock().unwrap().clone();
    let out_ch = cmd
        .get("output_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.output_channel as u64) as u32;
    let in_ch = cmd
        .get("input_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.input_channel as u64) as u32;
    // #360: an omitted `ref_dbfs` becomes "whatever this session's ceiling
    // is", not a second hardcoded number (-10.0) that has to be kept in
    // sync with `drive_max_dbfs`'s own default by convention. An
    // explicitly-passed value is then clamped the same way — defense in
    // depth, and the single binding every downstream quantity (amp,
    // ref_amp, out_scale, in_scale, cal.ref_dbfs) derives from, so the
    // calibration stays internally consistent (measured-and-scaled-at-the-
    // same-level).
    let ref_dbfs = cmd
        .get("ref_dbfs")
        .and_then(Value::as_f64)
        .unwrap_or(cfg.drive_max_dbfs);
    let ref_dbfs = apply_drive_ceiling(cfg.drive_max_dbfs, ref_dbfs);

    let pub_tx = state.pub_tx.clone();
    let fake = state.fake_audio;
    let out_port = match resolve_output(&cfg, state) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let mut cfg_in = cfg.clone();
    cfg_in.input_channel = in_ch;
    cfg_in.input_port = None;
    let in_port = match resolve_input(&cfg_in, state) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let cal_reply_tx = state.cal_reply_tx.clone();

    let worker = spawn_worker(state, "calibrate", move |stop| {
        let mut eng = make_engine(fake);
        // Open both directions: output for the reference tone, input so
        // step 2 can measure the actual ADC dBFS instead of assuming
        // unity-gain loopback when scaling the user's DMM reading.
        if let Err(e) = eng.start(std::slice::from_ref(&out_port), Some(&in_port)) {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"calibrate","message":format!("{e}")}),
            );
            return;
        }
        let amp = ac_core::shared::generator::dbfs_to_amplitude(ref_dbfs);
        eng.set_tone(1000.0, amp);

        // Step 1 — output voltage. User's DMM reading is the analog
        // Vrms at the DAC output while the tone is playing at `ref_dbfs`,
        // so the projected 0 dBFS Vrms is `reading / amp(ref_dbfs)`.
        let (tx1, rx1) = crossbeam_channel::bounded(1);
        *cal_reply_tx.lock().unwrap() = Some(tx1);
        let dmm_v1 = cfg.dmm_host.as_deref().and_then(|h| read_dmm_vrms(h, 3));
        send_pub(
            &pub_tx,
            "cal_prompt",
            &json!({
                "step":      1,
                "text":      format!(
                    "Output cal — measure DAC output Vrms with DMM (1 kHz @ {ref_dbfs:.1} dBFS). \
                     Enter reading, or press Enter to keep the stored value."),
                "dmm_vrms":  dmm_v1,
                "ref_dbfs":  ref_dbfs,
            }),
        );
        let out_reply = wait_cal_reply(&rx1, &stop, 120);
        *cal_reply_tx.lock().unwrap() = None;
        if stop.load(Ordering::Relaxed) {
            eng.set_silence();
            eng.stop();
            return;
        }

        // Step 2 — input voltage. We need the *captured* input dBFS to
        // convert the user's DMM reading into `vrms_at_0dbfs_in`, so
        // capture briefly before prompting (after a short settle so the
        // fresh tone fills the ring). If the input is silent / unwired,
        // fall back to assuming loopback (`captured_dbfs = ref_dbfs`).
        eng.flush_capture();
        std::thread::sleep(std::time::Duration::from_millis(150));
        let captured_rms = capture_rms(&mut *eng, 0.3);
        let captured_dbfs = rms_to_dbfs(captured_rms);
        let in_dbfs_for_scale = if captured_dbfs > -80.0 {
            captured_dbfs
        } else {
            ref_dbfs
        };

        // Loopback heuristic: a unity-gain DAC→ADC loop puts captured
        // RMS-dBFS at `ref_dbfs - 3.01` (the sine peak/RMS factor). When
        // it lines up within ±2 dB, the user's output reading IS the
        // input reading — pre-fill it so they just hit Enter and the
        // prompt stops feeling redundant.
        let loopback_dbfs = ref_dbfs - 20.0 * 2f64.sqrt().log10();
        let is_loopback = (captured_dbfs - loopback_dbfs).abs() <= 2.0;
        let dmm_v2_real = cfg.dmm_host.as_deref().and_then(|h| read_dmm_vrms(h, 3));
        let dmm_v2 = dmm_v2_real.or(if is_loopback { out_reply.value() } else { None });

        let prompt_text = if is_loopback {
            format!(
                "Input cal — loopback detected ({captured_dbfs:.1} dBFS captured). \
                 Press Enter to reuse the output reading, or override (q to cancel)."
            )
        } else {
            format!(
                "Input cal — measure ADC input Vrms with DMM (captured {captured_dbfs:.1} dBFS). \
                 Enter reading, or press Enter to keep the stored value."
            )
        };
        let (tx2, rx2) = crossbeam_channel::bounded(1);
        *cal_reply_tx.lock().unwrap() = Some(tx2);
        send_pub(
            &pub_tx,
            "cal_prompt",
            &json!({
                "step":          2,
                "text":          prompt_text,
                "dmm_vrms":      dmm_v2,
                "captured_dbfs": captured_dbfs,
                "loopback":      is_loopback,
            }),
        );
        let in_reply = wait_cal_reply(&rx2, &stop, 120);
        *cal_reply_tx.lock().unwrap() = None;
        // Symmetric with the step-1 check above: a cancel here must also
        // abort the run rather than fall through to save. Before this fix
        // a `q` at the second prompt still committed step 1's reading and
        // `ref_dbfs`, while the CLI told the operator "Calibration
        // cancelled." (#294 QA correctness issue 1).
        if stop.load(Ordering::Relaxed) {
            eng.set_silence();
            eng.stop();
            return;
        }

        // Fallback conditions for the `cal_done` wire frame when τ isn't
        // measured this run (no-loopback, or a lifecycle error before any
        // conditions were captured) — ZMQ.md requires `tau_sample_rate` /
        // `tau_period_size` present regardless of `tau_state`.
        let fallback_sample_rate = eng.sample_rate();
        let fallback_period_size = eng.period_size();

        eng.set_silence();
        eng.stop();

        // τ (interface latency, #281/#347) — not prompt-driven, so it
        // piggybacks on the loopback state established above rather than
        // adding a third interactive step. Measured whenever a loopback was
        // detected this run, regardless of whether either voltage prompt
        // was answered or skipped — the cheap-refresh path (#279: both
        // prompts skipped) still refreshes τ. #347: a single reading is not
        // a measurement of τ on this stack, so this now runs two
        // independent client lifecycles (`measure_tau_twice`), decoupled
        // from the voltage-cal `eng` above (already stopped) — see that
        // function's doc for why the lifecycle boundary matters.
        let ref_amp = ac_core::shared::generator::dbfs_to_amplitude(ref_dbfs);
        let tau_outcome = tau_result(is_loopback, || {
            measure_tau_twice(fake, cfg.device, &out_port, &in_port, ref_amp)
        });

        // Convert from "Vrms at the played/captured dBFS" → "Vrms at 0 dBFS".
        let out_scale = 1.0 / ac_core::shared::generator::dbfs_to_amplitude(ref_dbfs);
        let in_scale = 1.0 / ac_core::shared::generator::dbfs_to_amplitude(in_dbfs_for_scale);

        // Load existing entry to preserve unrelated fields (notably the
        // SPL pistonphone reading set by `calibrate_spl`), and to preserve
        // a voltage field whose prompt was skipped — a re-check of one leg
        // must not cost the other.
        let mut cal = Calibration::load_or_new(out_ch, in_ch, None);
        let out_state = apply_cal_reading(&mut cal.vrms_at_0dbfs_out, out_reply, out_scale);
        let in_state = apply_cal_reading(&mut cal.vrms_at_0dbfs_in, in_reply, in_scale);
        // `ref_dbfs` records the level the stored readings were taken at,
        // so it only moves when a reading did. A run that skipped both
        // prompts measured nothing and must leave the entry as it found it.
        if out_state == "measured" || in_state == "measured" {
            cal.ref_dbfs = ref_dbfs;
        }
        if tau_outcome.state == "measured" {
            cal.tau_history.push(TauEntry {
                conditions: tau_outcome
                    .conditions
                    .clone()
                    .expect("conditions is Some when tau_state is \"measured\""),
                tau_s: tau_outcome
                    .tau_s
                    .expect("tau_s is Some when tau_state is \"measured\""),
                measured_at: ac_core::shared::time::now_utc_iso8601(),
                method: TAU_METHOD.to_string(),
                agreement_count: tau_outcome
                    .agreement_count
                    .expect("agreement_count is Some when tau_state is \"measured\""),
            });
        }
        let save_err = cal.save(None).err().map(|e| e.to_string());

        let key = cal.key();
        let (tau_sample_rate, tau_period_size) = match &tau_outcome.conditions {
            Some(c) => (c.sample_rate, c.period_size),
            None => (fallback_sample_rate, fallback_period_size),
        };
        // Values reported are what is now stored, not what this run
        // measured — with `*_state` naming which of the three that is.
        let mut cal_done_frame = json!({
            "key":                  key,
            "vrms_at_0dbfs_out":    cal.vrms_at_0dbfs_out,
            "vrms_at_0dbfs_in":     cal.vrms_at_0dbfs_in,
            "out_state":            out_state,
            "in_state":             in_state,
            "tau_state":            tau_outcome.state,
            "tau_s":                tau_outcome.tau_s,
            "tau_sample_rate":      tau_sample_rate,
            "tau_period_size":      tau_period_size,
            "tau_agreement_count":  tau_outcome.agreement_count.unwrap_or(0),
        });
        if let Some(ref e) = tau_outcome.error {
            cal_done_frame["tau_error"] = json!(e);
        }
        if let Some(r1) = tau_outcome.reading1_s {
            cal_done_frame["tau_reading1_s"] = json!(r1);
        }
        if let Some(r2) = tau_outcome.reading2_s {
            cal_done_frame["tau_reading2_s"] = json!(r2);
        }
        if let Some(d) = tau_outcome.delta_samples {
            cal_done_frame["tau_delta_samples"] = json!(d);
        }
        if let Some(p) = tau_outcome.periods {
            cal_done_frame["tau_periods"] = json!(p);
        }
        if let Some(ref e) = save_err {
            cal_done_frame["error"] = json!(e);
        }
        send_pub(&pub_tx, "cal_done", &cal_done_frame);

        // Route terminal frame on save outcome so the Python client, which
        // treats `done` vs `error` as the authoritative signal, sees failures.
        match save_err {
            Some(e) => send_pub(
                &pub_tx,
                "error",
                &json!({
                    "cmd":     "calibrate",
                    "message": format!("save failed: {e}"),
                }),
            ),
            None => send_pub(
                &pub_tx,
                "done",
                &json!({
                    "cmd": "calibrate",
                    "key": key,
                }),
            ),
        }
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("calibrate".to_string(), worker);
    }
    json!({"ok": true, "ref_dbfs": ref_dbfs})
}

pub fn cal_reply(state: &ServerState, cmd: &Value) -> Value {
    // `clear: true` is the only way to erase a stored value, and it has to
    // be asked for by name — a missing/null `vrms` means "I did not measure
    // this", which is not the same request (#279).
    let reply = if cmd.get("clear").and_then(Value::as_bool).unwrap_or(false) {
        CalReply::Clear
    } else {
        match cmd.get("vrms").and_then(Value::as_f64) {
            Some(v) => CalReply::Value(v),
            None => CalReply::Skip, // JSON null or absent
        }
    };
    let tx = state.cal_reply_tx.lock().unwrap();
    if let Some(ref t) = *tx {
        let _ = t.send(reply);
    }
    json!({"ok": true})
}

/// `calibrate_mic_curve` — attach (or clear) a mic frequency-response
/// correction curve on a channel.
///
/// CLI parses the .frd / .txt file and uploads the validated arrays via
/// this cmd, so the daemon never has to read user-supplied paths and
/// works the same way for local and remote daemons. Two operations:
/// `op = "set"` with `freqs_hz` + `gain_db` arrays, or `op = "clear"`
/// to drop a stored curve. Voltage and SPL fields on the same entry
/// stay untouched (`load_or_new` round-trip).
pub fn calibrate_mic_curve(state: &ServerState, cmd: &Value) -> Value {
    let cfg = state.cfg.lock().unwrap().clone();
    let out_ch = cmd
        .get("output_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.output_channel as u64) as u32;
    let in_ch = cmd
        .get("input_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.input_channel as u64) as u32;
    let op = cmd.get("op").and_then(Value::as_str).unwrap_or("set");

    let mut cal = Calibration::load_or_new(out_ch, in_ch, None);
    match op {
        "clear" => {
            cal.mic_response = None;
        }
        "set" => {
            let freqs: Option<Vec<f32>> = cmd.get("freqs_hz").and_then(Value::as_array).map(|a| {
                a.iter()
                    .filter_map(|v| v.as_f64().map(|v| v as f32))
                    .collect()
            });
            let gains: Option<Vec<f32>> = cmd.get("gain_db").and_then(Value::as_array).map(|a| {
                a.iter()
                    .filter_map(|v| v.as_f64().map(|v| v as f32))
                    .collect()
            });
            let (freqs_hz, gain_db) = match (freqs, gains) {
                (Some(f), Some(g)) if !f.is_empty() && f.len() == g.len() => (f, g),
                _ => {
                    return json!({"ok": false,
                    "error": "calibrate_mic_curve set: missing/mismatched freqs_hz/gain_db"})
                }
            };
            // Re-validate length bounds + monotonicity here too — the CLI
            // already parsed and validated, but a hostile or buggy client
            // shouldn't be able to write a malformed curve to disk.
            if freqs_hz.len() < MicResponse::MIN_POINTS {
                return json!({"ok": false,
                    "error": format!("curve too sparse: {} < {}", freqs_hz.len(), MicResponse::MIN_POINTS)});
            }
            if freqs_hz.len() > MicResponse::MAX_POINTS {
                return json!({"ok": false,
                    "error": format!("curve too dense: {} > {}", freqs_hz.len(), MicResponse::MAX_POINTS)});
            }
            for w in freqs_hz.windows(2) {
                if w[1] <= w[0] || !w[0].is_finite() || !w[1].is_finite() {
                    return json!({"ok": false,
                        "error": "freqs_hz must be strictly increasing and finite"});
                }
            }
            for &g in &gain_db {
                if !g.is_finite() {
                    return json!({"ok": false,
                        "error": "gain_db values must be finite"});
                }
            }
            let source_path = cmd
                .get("source_path")
                .and_then(Value::as_str)
                .map(str::to_string);
            cal.mic_response = Some(MicResponse {
                freqs_hz,
                gain_db,
                source_path,
                imported_at: ac_core::shared::time::now_utc_iso8601(),
            });
        }
        other => {
            return json!({"ok": false,
                "error": format!("calibrate_mic_curve: unknown op {other:?}, expected set/clear")});
        }
    }
    if let Err(e) = cal.save(None) {
        return json!({"ok": false, "error": format!("save failed: {e}")});
    }
    json!({
        "ok":      true,
        "key":     cal.key(),
        "loaded":  cal.mic_response.as_ref().map(|r| r.freqs_hz.len()).unwrap_or(0),
    })
}

/// `set_mic_correction_enabled` — toggle daemon-side mic-curve application.
/// When disabled, monitor frames carry the raw uncorrected magnitudes (a
/// loaded curve does not vanish — just stops being applied). Used by the
/// UI's `Shift+M` keybinding for diagnostics. The flag is process-wide;
/// per-channel curves are still respected, the toggle just gates them.
pub fn set_mic_correction_enabled(state: &ServerState, cmd: &Value) -> Value {
    let enabled = cmd.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    state
        .mic_correction_enabled
        .store(enabled, std::sync::atomic::Ordering::Relaxed);
    json!({"ok": true, "enabled": enabled})
}

/// `calibrate_spl` — pistonphone-reference SPL calibration.
///
/// Captures ~1 s of audio on the input channel, computes its RMS in dBFS,
/// and stores that value as `mic_sensitivity_dbfs_at_94db_spl` so all
/// future dBFS readings on this channel can convert to dB SPL via
/// `dbspl = dbfs - mic_sens_dbfs + 94.0`. Voltage-cal fields on the same
/// entry are preserved.
///
/// Wire flow mirrors `calibrate`:
///   1. emit `cal_prompt` asking the user to apply the pistonphone,
///   2. wait for `cal_reply` (any value — only the act of replying is
///      meaningful; the user has had time to seat the calibrator),
///   3. capture, compute, save,
///   4. emit `cal_done` with the captured dBFS, then `done` / `error`.
pub fn calibrate_spl(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "calibrate_spl");
    let cfg = state.cfg.lock().unwrap().clone();
    let out_ch = cmd
        .get("output_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.output_channel as u64) as u32;
    let in_ch = cmd
        .get("input_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.input_channel as u64) as u32;
    let capture_s = cmd.get("capture_s").and_then(Value::as_f64).unwrap_or(1.0);

    let pub_tx = state.pub_tx.clone();
    let fake = state.fake_audio;
    let mut cfg_in = cfg.clone();
    cfg_in.input_channel = in_ch;
    cfg_in.input_port = None;
    let in_port = match resolve_input(&cfg_in, state) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let cal_reply_tx = state.cal_reply_tx.clone();

    let worker = spawn_worker(state, "calibrate_spl", move |stop| {
        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&[], Some(&in_port)) {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"calibrate_spl","message":format!("{e}")}),
            );
            return;
        }
        // Prompt the user to seat the pistonphone, wait for the green
        // light. The reply value itself is unused — we just need a
        // synchronisation point so the capture sees the reference tone,
        // not silence or seating noise.
        let (tx, rx) = crossbeam_channel::bounded(1);
        *cal_reply_tx.lock().unwrap() = Some(tx);
        send_pub(
            &pub_tx,
            "cal_prompt",
            &json!({
                "step":     1,
                "text":     "Apply 94 dB SPL pistonphone reference, press Enter when ready (q to cancel).",
                "kind":     "spl",
            }),
        );
        let _ = wait_cal_reply(&rx, &stop, 300);
        *cal_reply_tx.lock().unwrap() = None;
        if stop.load(Ordering::Relaxed) {
            eng.stop();
            return;
        }

        // Brief settling period, then a clean capture.
        eng.flush_capture();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let rms = capture_rms(&mut *eng, capture_s);
        let dbfs = rms_to_dbfs(rms);
        eng.stop();

        let mut cal = Calibration::load_or_new(out_ch, in_ch, None);
        cal.mic_sensitivity_dbfs_at_94db_spl = Some(dbfs);
        let save_err = cal.save(None).err().map(|e| e.to_string());

        let key = cal.key();
        let mut cal_done_frame = json!({
            "key":                              key,
            "mic_sensitivity_dbfs_at_94db_spl": dbfs,
            "kind":                             "spl",
        });
        if let Some(ref e) = save_err {
            cal_done_frame["error"] = json!(e);
        }
        send_pub(&pub_tx, "cal_done", &cal_done_frame);

        match save_err {
            Some(e) => send_pub(
                &pub_tx,
                "error",
                &json!({
                    "cmd":     "calibrate_spl",
                    "message": format!("save failed: {e}"),
                }),
            ),
            None => send_pub(
                &pub_tx,
                "done",
                &json!({
                    "cmd": "calibrate_spl",
                    "key": key,
                }),
            ),
        }
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("calibrate_spl".to_string(), worker);
    }
    json!({"ok": true})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_conditions() -> TauConditions {
        TauConditions {
            device: 0,
            backend: "fake".to_string(),
            sample_rate: 48_000,
            period_size: Some(1024),
            output_port: "out".to_string(),
            input_port: "in".to_string(),
        }
    }

    /// #281 QA correctness issue 3: the no-loopback path is hard to drive
    /// end-to-end under `--fake-audio` (the fake backend's step-2 capture
    /// always reads as loopback-shaped), so pin the decision down directly
    /// instead. `attempt` must not run at all when there's no loopback.
    #[test]
    fn tau_result_no_loopback_short_circuits_without_measuring() {
        let mut called = false;
        let outcome = tau_result(false, || {
            called = true;
            TauAttempt::Compared {
                conditions: dummy_conditions(),
                reading1_s: 0.001,
                reading2_s: 0.001,
                comparison: TauComparison::Agree,
            }
        });
        assert_eq!(outcome.state, "not_measured_no_loopback");
        assert_eq!(outcome.tau_s, None);
        assert_eq!(outcome.error, None);
        assert!(
            !called,
            "attempt must not run when no loopback was detected"
        );
    }

    /// #347: two independent readings agreeing is what "measured" means
    /// now — a single reading is no longer a storable outcome, so
    /// `agreement_count` must always be `Some(2)` alongside it.
    #[test]
    fn tau_result_agreeing_readings_reports_measured_with_agreement_count() {
        let outcome = tau_result(true, || TauAttempt::Compared {
            conditions: dummy_conditions(),
            reading1_s: 0.000_667,
            reading2_s: 0.000_667,
            comparison: TauComparison::Agree,
        });
        assert_eq!(outcome.state, "measured");
        assert_eq!(outcome.tau_s, Some(0.000_667));
        assert_eq!(outcome.agreement_count, Some(2));
        assert_eq!(outcome.error, None);
        assert!(outcome.conditions.is_some());
        // ZMQ.md: tau_delta_samples is present only on disagree_* — a
        // healthy Agree run must not carry a stray Some(0) (QA #348
        // correctness 1).
        assert_eq!(outcome.delta_samples, None);
        assert_eq!(outcome.periods, None);
    }

    #[test]
    fn tau_result_averages_two_agreeing_readings() {
        let outcome = tau_result(true, || TauAttempt::Compared {
            conditions: dummy_conditions(),
            reading1_s: 0.001_000_00,
            reading2_s: 0.001_000_02,
            comparison: TauComparison::Agree,
        });
        let tau_s = outcome.tau_s.expect("measured");
        assert!((tau_s - 0.001_000_01).abs() < 1e-9);
    }

    /// #347 acceptance criterion: "two synthetic readings one period_size
    /// apart are refused, with the period named in the message" — rig data
    /// from the issue body (`4262.064 -> 5286.064` at 96 kHz, +1024
    /// exactly).
    #[test]
    fn tau_result_period_shift_disagreement_refuses_and_names_the_period() {
        let comparison =
            compare_tau_readings(4262.064 / 96_000.0, 5286.064 / 96_000.0, 96_000, Some(1024));
        let outcome = tau_result(true, || TauAttempt::Compared {
            conditions: dummy_conditions(),
            reading1_s: 4262.064 / 96_000.0,
            reading2_s: 5286.064 / 96_000.0,
            comparison,
        });
        assert_eq!(outcome.state, "disagree_period_shift");
        assert_eq!(outcome.tau_s, None);
        assert_eq!(outcome.agreement_count, None);
        assert_eq!(outcome.periods, Some(1));
        let msg = outcome.error.expect("disagreement carries a message");
        assert!(msg.contains("1 period"), "got {msg}");
        assert!(msg.contains("1024"), "got {msg}");
    }

    /// #347 acceptance criterion: a disagreement that is *not* a period
    /// multiple is a different fault and must say so, not read as the same
    /// "mismatch" as a period-shift.
    #[test]
    fn tau_result_non_period_disagreement_is_a_different_state() {
        let comparison = compare_tau_readings(0.0, 0.000_5, 48_000, Some(1024));
        let outcome = tau_result(true, || TauAttempt::Compared {
            conditions: dummy_conditions(),
            reading1_s: 0.0,
            reading2_s: 0.000_5,
            comparison,
        });
        assert_eq!(outcome.state, "disagree_other");
        assert_eq!(outcome.tau_s, None);
        assert_eq!(outcome.periods, None);
        let msg = outcome.error.expect("disagreement carries a message");
        assert!(msg.contains("not a period multiple"), "got {msg}");
    }

    #[test]
    fn tau_result_loopback_err_reports_error_state_and_message() {
        let outcome = tau_result(true, || TauAttempt::Error {
            conditions: None,
            message: "\u{3c4} measurement failed (reading 1 of 2): timeout".to_string(),
        });
        assert_eq!(outcome.state, "error");
        assert_eq!(outcome.tau_s, None);
        let msg = outcome.error.expect("error message present on failure");
        assert!(
            msg.contains("timeout"),
            "error message should name the failure: {msg}"
        );
    }

    /// #340 AC4/AC-test: a peak pinned at the window's far edge — exactly
    /// the shape #277 measured — must be refused, not converted into the
    /// plausible-looking offset the old, unguarded code would have
    /// returned. Compute that pinned value here, the way `measure_tau`
    /// used to unconditionally, and confirm the guard fires before it
    /// would ever reach the caller.
    #[test]
    fn check_peak_within_window_refuses_peak_pinned_at_edge() {
        let window_len = 9600; // 2 * 4800, i.e. a 50 ms half-window @ 96 kHz
        let half = window_len / 2;
        let peak_idx = window_len - 1; // pinned against the far edge
        let pinned_offset_samples = peak_idx as i64 - half as i64; // what the old code returned
        assert_eq!(pinned_offset_samples, half as i64 - 1);

        let result = check_peak_within_window(peak_idx, window_len, TAU_EDGE_MARGIN_FRAC);
        assert!(
            result.is_err(),
            "peak pinned at the window edge must be refused, not silently reported as offset {pinned_offset_samples}"
        );
    }

    /// A peak exactly `margin_frac` of the half-window from the start edge
    /// is refused — the margin boundary itself counts as "too close", per
    /// the architect note's instruction to test the boundary directly.
    #[test]
    fn check_peak_within_window_refuses_at_margin_boundary() {
        let window_len = 100;
        let half = window_len / 2;
        let margin = (TAU_EDGE_MARGIN_FRAC * half as f64).round() as usize;

        assert!(check_peak_within_window(margin, window_len, TAU_EDGE_MARGIN_FRAC).is_err());
        assert!(check_peak_within_window(
            window_len - 1 - margin,
            window_len,
            TAU_EDGE_MARGIN_FRAC
        )
        .is_err());
    }

    /// A peak safely inside the window, away from either edge, is
    /// accepted — the guard must not refuse the ordinary, correctly
    /// measured case.
    #[test]
    fn check_peak_within_window_accepts_interior_peak() {
        let window_len = 9600;
        let half = window_len / 2;
        assert!(check_peak_within_window(half, window_len, TAU_EDGE_MARGIN_FRAC).is_ok());
    }

    /// One sample outside the margin boundary must be accepted — pairs
    /// with `check_peak_within_window_refuses_at_margin_boundary` to pin
    /// the guard as off-by-zero (refuses exactly at the boundary, accepts
    /// exactly past it) rather than off-by-one in either direction.
    #[test]
    fn check_peak_within_window_accepts_one_sample_past_margin_boundary() {
        let window_len = 100;
        let half = window_len / 2;
        let margin = (TAU_EDGE_MARGIN_FRAC * half as f64).round() as usize;

        assert!(check_peak_within_window(margin + 1, window_len, TAU_EDGE_MARGIN_FRAC).is_ok());
        assert!(check_peak_within_window(
            window_len - 1 - margin - 1,
            window_len,
            TAU_EDGE_MARGIN_FRAC
        )
        .is_ok());
    }

    /// #340's own motivating number: the rig's measured 43.75 ms round trip
    /// (4200 samples at 96 kHz) must clear the edge guard, not just a
    /// dead-centre synthetic peak. Locks in the margin the fix actually
    /// buys at the operating point that motivated the issue (QA correctness
    /// issue 2).
    #[test]
    fn check_peak_within_window_accepts_the_rigs_measured_round_trip() {
        let window_len = 9600; // 50 ms half-window @ 96 kHz
        let half = window_len / 2;
        let offset_samples = 4200i64; // rig's measured tau, 96 kHz (#340/#277)
        let peak_idx = (half as i64 + offset_samples) as usize;
        assert!(
            check_peak_within_window(peak_idx, window_len, TAU_EDGE_MARGIN_FRAC).is_ok(),
            "the rig's own measured round trip must be inside the accepted window"
        );
    }

    /// Coupled-constant guard (QA correctness issue 1): `TAU_TAIL_S` must
    /// stay ahead of `TAU_MIN_HALF_WINDOW_S` with margin, or the capture
    /// can no longer hold the whole gate at high sample rates. Fails on a
    /// wrong pair (constants inverted) and on an unscored gap (no
    /// headroom) rather than passing silently either way.
    // Both bounds below compare two `const`s, so clippy sees a
    // compile-time-knowable value and suggests a `const {}` assertion
    // block instead. That would turn this into a build-time check instead
    // of a `cargo test` result — deliberately kept as a runtime test (the
    // shape the coupled-constants rule asks for) so it shows up in test
    // output like the rest of the suite, not just as a build failure.
    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn tau_tail_s_clears_tau_min_half_window_s_with_margin() {
        assert!(
            TAU_TAIL_S > TAU_MIN_HALF_WINDOW_S,
            "capture tail ({TAU_TAIL_S}s) must exceed the half-window \
             ({TAU_MIN_HALF_WINDOW_S}s) or the gate can run off the end of the capture"
        );
        // Headroom bound: the doc comment on TAU_TAIL_S claims it "stays
        // well above" the half-window — fail if a future edit erodes that
        // below the 2x this test treats as the floor for "well above" (a
        // future edit to either constant must re-justify the number, not
        // silently drift under it).
        assert!(
            TAU_TAIL_S >= 2.0 * TAU_MIN_HALF_WINDOW_S,
            "tail no longer clears the half-window with the margin the doc comment assumed"
        );
    }

    /// The rig override parser (#350) accepts a plain number and refuses
    /// anything else — a typo must not silently become a different window
    /// than the one the run sheet says was used, which would make the
    /// recorded edge fraction wrong rather than missing. Uses keys no
    /// other test reads so the process-wide environment stays shared-safe.
    #[cfg(feature = "tau-window-override")]
    #[test]
    fn tau_env_override_parses_or_falls_back_to_the_compiled_constant() {
        assert_eq!(
            tau_env_f64("AC_TAU_TEST_UNSET_KEY_350", 0.05),
            0.05,
            "an unset variable must leave the compiled-in constant in place"
        );
        std::env::set_var("AC_TAU_TEST_GOOD_KEY_350", " 0.04862 ");
        assert_eq!(tau_env_f64("AC_TAU_TEST_GOOD_KEY_350", 0.05), 0.04862);
        for bad in ["", "48.62ms", "-0.01", "nan", "inf"] {
            std::env::set_var("AC_TAU_TEST_BAD_KEY_350", bad);
            assert_eq!(
                tau_env_f64("AC_TAU_TEST_BAD_KEY_350", 0.05),
                0.05,
                "{bad:?} is not a usable window and must fall back, not be coerced"
            );
        }
    }
}
