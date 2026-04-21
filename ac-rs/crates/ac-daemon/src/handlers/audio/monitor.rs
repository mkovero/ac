//! `monitor_spectrum` — live per-channel spectrum/CWT feed.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::shared::calibration::Calibration;

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
    let ioct_bpo_shared = state.ioct_bpo.clone();

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
        let (mut cwt_scales, mut cwt_freqs) = ac_core::visualize::cwt::log_scales(
            ac_core::visualize::cwt::DEFAULT_F_MIN,
            ac_core::visualize::cwt::default_f_max(sr),
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

        while !stop.load(Ordering::Relaxed) {
            let tick_start = std::time::Instant::now();
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
                    // Optional fractional-octave aggregation of the same
                    // CWT column: reuses `cwt_mags` / `cwt_freqs` — zero
                    // extra DSP cost when enabled.
                    if let Some(bpo) = *ioct_bpo_shared.lock().unwrap() {
                        let (band_centres, band_levels) =
                            ac_core::visualize::fractional_octave::cwt_to_fractional_octave(
                                &cwt_mags,
                                &cwt_freqs,
                                bpo as usize,
                                ac_core::visualize::cwt::DEFAULT_F_MIN,
                                ac_core::visualize::cwt::default_f_max(sr),
                            );
                        let frac_frame = json!({
                            "type":       "fractional_octave",
                            "cmd":        "monitor_spectrum",
                            "channel":    channel,
                            "n_channels": n_channels,
                            "sr":         sr,
                            "bpo":        bpo,
                            "freqs":      band_centres,
                            "spectrum":   band_levels,
                            "timestamp":  ts_ns,
                            "xruns":      xruns_total,
                        });
                        send_pub(&pub_tx, "data", &frac_frame);
                    }
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
                    let analyze_result = ac_core::measurement::thd::analyze(samples, sr, current_freqs[idx], 10);
                    let frame = match analyze_result {
                        Ok(r) => {
                            current_freqs[idx] = r.fundamental_hz;
                            let cal = cals[idx].as_ref();
                            let in_dbu = cal
                                .and_then(|c| c.in_vrms(r.linear_rms))
                                .map(ac_core::shared::conversions::vrms_to_dbu);
                            let sr_f = sr as f64;
                            let (spec, freqs) = ac_core::visualize::aggregate::spectrum_to_columns_wire(
                                &r.spectrum,
                                sr_f,
                                20.0,
                                (sr_f / 2.0).max(21.0),
                                ac_core::visualize::aggregate::DEFAULT_WIRE_COLUMNS,
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
                            let (spec, _) = ac_core::visualize::spectrum::spectrum_only(samples, sr);
                            let sr_f = sr as f64;
                            let (spec, freqs) = ac_core::visualize::aggregate::spectrum_to_columns_wire(
                                &spec,
                                sr_f,
                                20.0,
                                (sr_f / 2.0).max(21.0),
                                ac_core::visualize::aggregate::DEFAULT_WIRE_COLUMNS,
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
