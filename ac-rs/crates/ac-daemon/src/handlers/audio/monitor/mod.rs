//! `monitor_spectrum` — live per-channel spectrum/CWT feed.
//!
//! The command is split across sibling modules:
//!
//! | module      | holds                                                       |
//! |-------------|-------------------------------------------------------------|
//! | `channel`   | per-channel state: rings, calibration, LF band, integrators  |
//! | `capture`   | drain budget, ring-buffered capture, capture failures        |
//! | `frames`    | the published wire frames and their shared per-tick values   |
//! | `mode`      | which analysis runs, and which modes pace themselves         |
//! | `reconnect` | multi-channel reconnect backoff (#93)                        |
//!
//! What stays here is the request itself: validation, worker spawn, the
//! tick loop that drives the modules above, and the reply.

mod capture;
mod channel;
mod frames;
mod mode;
mod reconnect;

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::visualize::time_integration::{TAU_FAST_S, TAU_SLOW_S};
use ac_core::visualize::weighting_curves::WeightingCurve;

use crate::server::{MonitorParams, ServerState};

use super::super::{
    busy_guard, cfg_guard, load_calibration_or_refuse, make_engine_for_state, resolve_input,
    selected_backend_is_fake, send_pub, spawn_worker,
};

use self::capture::{
    capture_budget_samples, capture_into_ring, capture_or_report, log_transform_time,
    push_loudness_with_optional_fir, RingTick, CWT_MIN_FILL,
};
use self::channel::{
    lf_band_enabled, lf_recompute_every, ChannelState, Integrator, MicCorrection, RingCaps,
    RingKind, LF_AVG_TAU_S, LF_OVERLAP,
};
use self::frames::{
    dbu_offset_db, emit_loudness_frame, emit_ring_frames, emit_scope_frame, now_ns,
    spectrum_columns, TickCtx,
};
use self::mode::Mode;

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
    let out_ch = cfg.output_channel;

    let mut channel_cals = Vec::with_capacity(channels.len());
    for &channel in &channels {
        match load_calibration_or_refuse(out_ch, channel, "measurement", None) {
            Ok(cal) => channel_cals.push(cal),
            Err(msg) => return json!({"ok": false, "error": msg}),
        }
    }

    let pub_tx = state.pub_tx.clone();
    let mut eng = match make_engine_for_state(state) {
        Ok(eng) => eng,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let fake = selected_backend_is_fake(eng.as_ref());
    let backend = eng.backend_name();
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

    let worker = spawn_worker(state, "monitor_spectrum", move |stop| {
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
            .zip(channel_cals)
            .map(|((&channel, in_port), cal)| {
                ChannelState::new(channel, in_port.clone(), cal, sr, freq_hz, &ring_caps)
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
            let lf_enabled = lf_band_enabled(cur_lf_fft_n, cur_fft_n);
            let lf_every = lf_recompute_every(cur_lf_fft_n, sr, cur_interval);
            let mode = Mode::from_tag(&analysis_mode.lock().unwrap().clone());

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
            if mode == Mode::Cwt {
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
                backend,
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
                if mode == Mode::Cwt {
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
                            "backend":        backend,
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
                                    "backend":        backend,
                                });
                                send_pub(&pub_tx, "data", &leq_frame);
                            }
                        }
                    }
                    continue;
                }
                if mode == Mode::Cqt {
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
                if mode == Mode::Reassigned {
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
                emit_scope_frame(ch, &ctx, &new, xruns_total);
                // Resolved before the ring borrow below: `MicCorrection`
                // borrows only `ch.mic_curve`, so it stays valid while
                // `samples` holds `ch.fft_ring` mutably.
                let mc = MicCorrection::new(ch.mic_curve.as_ref(), &ctx);
                let dbu_offset = dbu_offset_db(ch.cal.as_ref());
                let spl_offset = ch.spl_offset;
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
                if lf_enabled {
                    ch.lf.push_and_maybe_recompute(
                        &new,
                        cur_lf_fft_n,
                        sr,
                        lf_every,
                        std::time::Instant::now(),
                    );
                } else if ch.lf.is_stale() {
                    // LF band disabled (live N caught up to LF N) — drop stale
                    // state so a later re-enable rebuilds from fresh capture.
                    ch.lf.clear();
                }
                let lf_spec_for_merge: Option<&[f64]> = if lf_enabled {
                    ch.lf.spec_cache.as_deref()
                } else {
                    None
                };

                {
                    let analyze_result =
                        ac_core::measurement::thd::analyze(samples, sr, ch.current_freq, 10);
                    let mc_tag = mc.tag();
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
                                cur_crossover_hz,
                                mc,
                                &ctx,
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
                                cur_crossover_hz,
                                mc,
                                &ctx,
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
                            ("dbu_offset_db", json!(dbu_offset)),
                            ("spl_offset_db", json!(spl_offset)),
                            ("mic_correction", json!(mc_tag)),
                            ("xruns", json!(xruns_total)),
                            ("backend", json!(backend)),
                        ] {
                            obj.insert(k.to_string(), v);
                        }
                    }
                    send_pub(&pub_tx, "data", &frame);
                    emit_loudness_frame(ch, &ctx, mc_tag, now_ns(), xruns_total);
                }
            }
            // Pace the FFT mode to the requested interval; the
            // ring-buffered modes already paced themselves.
            if !mode.paces_itself() {
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
        send_pub(
            &pub_tx,
            "done",
            &json!({"cmd":"monitor_spectrum","backend":backend}),
        );
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
        "backend":         backend,
    })
}
