//! `monitor_spectrum` — live per-channel spectrum/CWT + drum-tuner feed.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::calibration::Calibration;

use crate::audio::make_engine;
use crate::server::{MonitorParams, ServerState};

use super::super::{busy_guard, resolve_input, send_pub, spawn_worker};

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
    let cwt_sigma_shared = state.cwt_sigma.clone();
    let cwt_n_scales_shared = state.cwt_n_scales.clone();
    let tuner_range_locks_shared = state.tuner_range_locks.clone();
    let tuner_config_shared = state.tuner_config.clone();

    let worker = spawn_worker(state, "monitor_spectrum", move |stop| {
        let cals: Vec<Option<Calibration>> = channels_worker.iter()
            .map(|&ch| Calibration::load(out_ch, ch, None).ok().flatten())
            .collect();
        let mut eng = make_engine(fake);
        let start_port = in_ports_worker.first().map(String::as_str);
        if let Err(e) = eng.start(&[], start_port) {
            send_pub(&pub_tx, "error", &json!({"cmd":"monitor_spectrum","message":format!("{e}")}));
            return;
        }
        let sr = eng.sample_rate();
        let mut current_freqs: Vec<f64> = vec![freq_hz; channels_worker.len()];
        let mut xruns_total = 0u32;

        // CWT state: recomputed when sigma/n_scales change.
        let mut cwt_sigma = *cwt_sigma_shared.lock().unwrap();
        let mut cwt_n_scales = *cwt_n_scales_shared.lock().unwrap();
        let (mut cwt_scales, mut cwt_freqs) = ac_core::cwt::log_scales(
            ac_core::cwt::DEFAULT_F_MIN,
            ac_core::cwt::default_f_max(sr),
            cwt_n_scales,
            sr,
            cwt_sigma,
        );

        // Sliding ring buffer for CWT: holds ~0.5 s of audio per channel so
        // low-frequency wavelets (20 Hz @ sigma=12 ≈ 0.6 s support) see
        // enough data. Short 50 ms captures feed the ring; the CWT runs on
        // the full ring each tick giving ~20 Hz update rate.
        let ring_cap = (sr as f64 * 0.15).ceil() as usize; // 0.15 s — enough for 20 Hz
        let cwt_tick = 0.02_f64; // 20 ms capture per CWT tick
        let mut cwt_rings: Vec<std::collections::VecDeque<f32>> =
            channels_worker.iter().map(|_| std::collections::VecDeque::with_capacity(ring_cap)).collect();
        let mut cwt_log_counter = 0u32;
        // Reused across every CWT tick so morlet_cwt_into doesn't allocate
        // a fresh Vec each call (prev ~3.5% of CPU in madvise / allocator).
        let mut cwt_mags: Vec<f32> = Vec::with_capacity(cwt_n_scales);

        // Sliding ring buffer for single-channel FFT path so refresh cadence
        // (`cur_interval`) can run faster than capture-window duration
        // (`cur_fft_n / sr`). Each tick pulls just the new samples that
        // arrived since the last tick, appends them, trims to the current
        // FFT-N, and analyses the full ring.
        let single_channel = channels_worker.len() == 1;
        let mut fft_rings: Vec<std::collections::VecDeque<f32>> =
            channels_worker.iter().map(|_| std::collections::VecDeque::with_capacity(131_072)).collect();

        // Per-channel TunerState — drum-head fundamental identifier. Runs
        // inside the FFT path on the raw half-spectrum (better resolution
        // than the log-aggregated wire columns); publishes on `tuner` topic
        // only when the level-trigger fires (sparse).
        let mut tuner_states: Vec<ac_core::tuner::TunerState> = channels_worker.iter()
            .map(|_| ac_core::tuner::TunerState::new(ac_core::tuner::TunerConfig::default()))
            .collect();
        let mut tuner_last_tick = std::time::Instant::now();
        let mut tuner_status_last_log = std::time::Instant::now();

        while !stop.load(Ordering::Relaxed) {
            let tick_start = std::time::Instant::now();
            let tuner_dt_s = tick_start.duration_since(tuner_last_tick).as_secs_f32();
            tuner_last_tick = tick_start;
            let tuner_range_snapshot: std::collections::HashMap<u32, (f64, f64)> =
                tuner_range_locks_shared.lock().unwrap().clone();
            let tuner_cfg_snapshot = *tuner_config_shared.lock().unwrap();
            for st in tuner_states.iter_mut() {
                st.set_config(tuner_cfg_snapshot);
            }
            let (cur_interval, cur_fft_n) = {
                let mp = monitor_params_shared.lock().unwrap();
                (mp.interval, mp.fft_n)
            };
            let mode = analysis_mode.lock().unwrap().clone();
            let is_cwt = mode == "cwt";

            // Check for live CWT param changes.
            if is_cwt {
                let new_sigma = *cwt_sigma_shared.lock().unwrap();
                let new_n = *cwt_n_scales_shared.lock().unwrap();
                if (new_sigma - cwt_sigma).abs() > 0.01 || new_n != cwt_n_scales {
                    cwt_sigma = new_sigma;
                    cwt_n_scales = new_n;
                    let (s, f) = ac_core::cwt::log_scales(
                        ac_core::cwt::DEFAULT_F_MIN,
                        ac_core::cwt::default_f_max(sr),
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
                        send_pub(&pub_tx, "error", &json!({
                            "cmd":     "monitor_spectrum",
                            "message": format!("reconnect ch{channel}: {e}"),
                        }));
                        continue;
                    }
                    eng.flush_capture();
                }
                if is_cwt {
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
                    ac_core::cwt::morlet_cwt_into(
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
                    let frame = json!({
                        "type":        "cwt",
                        "cmd":         "monitor_spectrum",
                        "channel":     channel,
                        "n_channels":  n_channels,
                        "sr":          sr,
                        "magnitudes":  &cwt_mags,
                        "frequencies": cwt_freqs,
                        "timestamp":   ts_ns,
                        "xruns":       xruns_total,
                    });
                    send_pub(&pub_tx, "data", &frame);
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
                    let analyze_result = ac_core::analysis::analyze(samples, sr, current_freqs[idx], 10);
                    // Feed tuner on every frame (both analyze-Ok and -Err paths) so
                    // baseline tracks silence and the internal peak-hold decays
                    // idle. Tuner expects dBFS; daemon spectra are linear amplitude,
                    // so convert here (matches the UI's receiver-side conversion).
                    let (tuner_spec, tuner_freqs): (Vec<f32>, Vec<f32>) = match &analyze_result {
                        Ok(r) => (
                            r.spectrum.iter()
                                .map(|&v| 20.0 * (v as f32).max(1e-12).log10())
                                .collect(),
                            r.freqs.iter().map(|&v| v as f32).collect(),
                        ),
                        Err(_) => {
                            let (s, f) = ac_core::analysis::spectrum_only(samples, sr);
                            (
                                s.iter()
                                    .map(|&v| 20.0 * (v as f32).max(1e-12).log10())
                                    .collect(),
                                f.iter().map(|&v| v as f32).collect(),
                            )
                        }
                    };
                    tuner_states[idx].set_range_lock(tuner_range_snapshot.get(&channel).copied());
                    let trig = tuner_states[idx].feed(&tuner_spec, &tuner_freqs, tuner_dt_s);
                    // Log every trigger attempt (both confident and non-confident)
                    // so users can see why tracking fails — non-confident fires
                    // don't get published but are still load-bearing diagnostic
                    // signal. Throttled level status at 1 Hz separately below.
                    // Gated by AC_TUNER_DEBUG=1 to keep stderr quiet otherwise.
                    let tuner_debug = std::env::var_os("AC_TUNER_DEBUG").is_some();
                    if tuner_debug {
                        if let ac_core::tuner::Triggered::Fired { candidate, confident } = &trig {
                            let st = tuner_states[idx].status();
                            match candidate {
                                Some(c) => eprintln!(
                                    "[tuner ch{channel}] FIRE conf={} f0={:.1}Hz confidence={:.2} \
                                     partials={} current={:.1}dB baseline={:.1}dB delta={:.1}dB \
                                     floor={:.1}dB peaks={}",
                                    if *confident { "YES" } else { "no " },
                                    c.freq_hz, c.confidence, c.partials.len(),
                                    st.current_db, st.baseline_db, st.delta_db,
                                    st.floor_db, st.peak_count,
                                ),
                                None => eprintln!(
                                    "[tuner ch{channel}] FIRE no-candidate \
                                     current={:.1}dB baseline={:.1}dB delta={:.1}dB \
                                     floor={:.1}dB peaks={}",
                                    st.current_db, st.baseline_db, st.delta_db,
                                    st.floor_db, st.peak_count,
                                ),
                            }

                            // Re-run the identifier to dump every candidate that
                            // survived the physicality gates — lets us see why
                            // the winner won (or lost) vs. the runners-up. Runs
                            // only when AC_TUNER_DEBUG=1 fires, so the extra
                            // allocation isn't on the hot path.
                            let peak_hold = tuner_states[idx].peak_hold();
                            let range = tuner_states[idx]
                                .range_lock()
                                .unwrap_or(tuner_states[idx].config().search_range_hz);
                            if peak_hold.len() == tuner_freqs.len() && st.floor_db.is_finite() {
                                let (_, diags) = ac_core::tuner::identify_fundamental_with_candidates(
                                    peak_hold, &tuner_freqs, st.floor_db, range,
                                );
                                let mut sorted = diags;
                                sorted.sort_by(|a, b| {
                                    b.score.partial_cmp(&a.score)
                                        .unwrap_or(std::cmp::Ordering::Equal)
                                });
                                for d in &sorted {
                                    let mut parts = String::new();
                                    for p in &d.matched {
                                        use std::fmt::Write;
                                        let _ = write!(
                                            parts,
                                            " ({},{})/{:.1}/{:.0}",
                                            p.mode.0, p.mode.1,
                                            p.measured_hz, p.magnitude_db,
                                        );
                                    }
                                    eprintln!(
                                        "[tuner ch{channel}]   cand f0={:.1} has_peak={} \
                                         score={:.1} matched={} loudest_ratio={:.3} \
                                         low_mode_max={:.1} partials={}",
                                        d.f0,
                                        if d.has_peak { 1 } else { 0 },
                                        d.score, d.matched.len(),
                                        d.loudest_ratio, d.low_mode_max_db,
                                        parts.trim_start(),
                                    );
                                }
                            }

                            // Top-5 raw FFT bins inside the search range, from
                            // the pre-peak-hold spectrum. Isolates whether the
                            // "228 Hz at N>=32k" drift comes from a real bin or
                            // from the peak-hold / aggregator pipeline.
                            let (fmin, fmax) = range;
                            let mut bins: Vec<(f32, f32)> = tuner_freqs
                                .iter()
                                .zip(tuner_spec.iter())
                                .filter(|(f, m)| {
                                    let fh = **f as f64;
                                    fh >= fmin && fh <= fmax && m.is_finite()
                                })
                                .map(|(f, m)| (*f, *m))
                                .collect();
                            bins.sort_by(|a, b| {
                                b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
                            });
                            let top: Vec<String> = bins
                                .iter()
                                .take(5)
                                .map(|(f, m)| format!("{:.1}Hz/{:.0}", f, m))
                                .collect();
                            if !top.is_empty() {
                                eprintln!(
                                    "[tuner ch{channel}]   top5bins: {}",
                                    top.join(" "),
                                );
                            }
                        }
                    }
                    if let ac_core::tuner::Triggered::Fired {
                        candidate: Some(c), confident: true,
                    } = trig {
                        let partials_json: Vec<Value> = c.partials.iter().map(|p| json!({
                            "mode":           [p.mode.0, p.mode.1],
                            "ideal_ratio":    p.ideal_ratio,
                            "measured_hz":    p.measured_hz,
                            "measured_ratio": p.measured_ratio,
                            "deviation_pct":  p.deviation_pct,
                            "magnitude_db":   p.magnitude_db,
                        })).collect();
                        let ts_ns = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_nanos() as u64)
                            .unwrap_or(0);
                        let tframe = json!({
                            "type":        "tuner",
                            "cmd":         "monitor_spectrum",
                            "channel":     channel,
                            "freq_hz":     c.freq_hz,
                            "confidence":  c.confidence,
                            "partials":    partials_json,
                            "baseline_db": tuner_states[idx].baseline_db(),
                            "range_lock":  tuner_states[idx].range_lock()
                                .map(|(l, h)| json!([l, h])),
                            "timestamp":   ts_ns,
                        });
                        send_pub(&pub_tx, "tuner", &tframe);
                    }
                    let frame = match analyze_result {
                        Ok(r) => {
                            current_freqs[idx] = r.fundamental_hz;
                            let cal = cals[idx].as_ref();
                            let in_dbu = cal
                                .and_then(|c| c.in_vrms(r.linear_rms))
                                .map(ac_core::conversions::vrms_to_dbu);
                            let sr_f = sr as f64;
                            let (spec, freqs) = ac_core::aggregate::spectrum_to_columns_wire(
                                &r.spectrum,
                                sr_f,
                                20.0,
                                (sr_f / 2.0).max(21.0),
                                ac_core::aggregate::DEFAULT_WIRE_COLUMNS,
                            );
                            json!({
                                "type":             "spectrum",
                                "cmd":              "monitor_spectrum",
                                "channel":          channel,
                                "n_channels":       n_channels,
                                "freq_hz":          r.fundamental_hz,
                                "sr":               sr,
                                "freqs":            freqs,
                                "spectrum":         spec,
                                "fundamental_dbfs": r.fundamental_dbfs,
                                "thd_pct":          r.thd_pct,
                                "thdn_pct":         r.thdn_pct,
                                "in_dbu":           in_dbu,
                                "clipping":         r.clipping,
                                "xruns":            xruns_total,
                            })
                        }
                        Err(_) => {
                            let (spec, _) = ac_core::analysis::spectrum_only(samples, sr);
                            let sr_f = sr as f64;
                            let (spec, freqs) = ac_core::aggregate::spectrum_to_columns_wire(
                                &spec,
                                sr_f,
                                20.0,
                                (sr_f / 2.0).max(21.0),
                                ac_core::aggregate::DEFAULT_WIRE_COLUMNS,
                            );
                            json!({
                                "type":             "spectrum",
                                "cmd":              "monitor_spectrum",
                                "channel":          channel,
                                "n_channels":       n_channels,
                                "sr":               sr,
                                "freqs":            freqs,
                                "spectrum":         spec,
                                "xruns":            xruns_total,
                            })
                        }
                    };
                    send_pub(&pub_tx, "data", &frame);
                }
            }
            // 1 Hz tuner level status log. Gated by AC_TUNER_DEBUG=1; lets
            // users see live current/baseline/delta/armed/floor even when no
            // trigger is firing, to diagnose why tracking fails.
            if std::env::var_os("AC_TUNER_DEBUG").is_some()
                && tuner_status_last_log.elapsed().as_secs_f64() >= 1.0
            {
                for (idx, &channel) in channels_worker.iter().enumerate() {
                    let st = tuner_states[idx].status();
                    eprintln!(
                        "[tuner ch{channel}] status current={:.1}dB baseline={:.1}dB \
                         delta={:.1}dB armed={} floor={:.1}dB peaks={} \
                         last_f0={:.1}Hz last_conf={:.2}",
                        st.current_db, st.baseline_db, st.delta_db,
                        st.armed, st.floor_db, st.peak_count,
                        st.last_candidate_hz, st.last_confidence,
                    );
                }
                tuner_status_last_log = std::time::Instant::now();
            }
            // Pace FFT mode to requested interval. CWT has its own cadence
            // (short tick + sliding ring — see `cwt_tick`) and paces itself.
            if !is_cwt {
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
