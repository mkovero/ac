//! Reference-plane commands: H1 transfer estimation and channel probe.
//! Both depend on engine port routing and the configured reference channel.

use std::sync::atomic::Ordering;

use rayon::prelude::*;
use serde_json::{json, Value};

use ac_core::shared::calibration::Calibration;

use crate::audio::make_engine;
use crate::handlers::mic;
use crate::server::ServerState;

use super::{
    busy_guard, read_dmm_vrms, resolve_output, resolve_ref_output, send_pub, spawn_worker,
};

/// Parse the `pairs` and legacy `meas_channel`/`ref_channel` shapes of
/// `transfer_stream` into a canonical pair list. Returns an Err message
/// suitable for `{"ok": false, "error": ...}` on malformed input.
fn parse_transfer_pairs(cmd: &Value) -> Result<Vec<(u32, u32)>, String> {
    if let Some(arr) = cmd.get("pairs").and_then(Value::as_array) {
        if arr.is_empty() {
            return Err("pairs is empty".into());
        }
        let mut out = Vec::with_capacity(arr.len());
        for (i, p) in arr.iter().enumerate() {
            let tuple = p
                .as_array()
                .ok_or_else(|| format!("pairs[{i}] must be [meas, ref]"))?;
            if tuple.len() != 2 {
                return Err(format!(
                    "pairs[{i}] must have exactly 2 elements, got {}",
                    tuple.len()
                ));
            }
            let m = tuple[0]
                .as_u64()
                .ok_or_else(|| format!("pairs[{i}][0] must be unsigned int"))?;
            let r = tuple[1]
                .as_u64()
                .ok_or_else(|| format!("pairs[{i}][1] must be unsigned int"))?;
            out.push((m as u32, r as u32));
        }
        // De-dup identical pairs — harmless but wasteful to publish twice.
        out.sort_unstable();
        out.dedup();
        return Ok(out);
    }
    // Legacy single-pair form.
    let m = cmd
        .get("meas_channel")
        .and_then(Value::as_u64)
        .ok_or_else(|| "meas_channel required (or use pairs=[[m,r], ...])".to_string())?;
    let r = cmd
        .get("ref_channel")
        .and_then(Value::as_u64)
        .ok_or_else(|| "ref_channel required (or use pairs=[[m,r], ...])".to_string())?;
    Ok(vec![(m as u32, r as u32)])
}

pub fn transfer_stream(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "transfer_stream");

    // `drive` controls whether the daemon plays pink noise on the output
    // while capturing. Default `false` — the UI wants a purely passive H1
    // estimate against whatever the user is already driving into the inputs.
    // Set `true` to restore the old self-driving behavior (with `level_dbfs`
    // controlling amplitude).
    let drive = cmd.get("drive").and_then(Value::as_bool).unwrap_or(false);
    let level_dbfs = cmd
        .get("level_dbfs")
        .and_then(Value::as_f64)
        .unwrap_or(-10.0);

    let pairs = match parse_transfer_pairs(cmd) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };

    // Per-meas-channel SPL session params (D10 — static for the session,
    // set once here, not live-toggleable). `weighting`'s wire contract
    // (handoff: transfer-frame-v2 M0) is a strict 3-way A/C/Z — no
    // "off" — so `WeightingCurve::from_tag`'s existing rejection of
    // anything else (including "off") is exactly the validation wanted.
    let weighting_tag = cmd.get("weighting").and_then(Value::as_str).unwrap_or("Z");
    let weighting = match ac_core::visualize::weighting_curves::WeightingCurve::from_tag(
        weighting_tag,
    ) {
        Some(w) => w,
        None => {
            return json!({"ok": false, "error": format!("weighting must be one of A, C, Z, got '{weighting_tag}'")})
        }
    };
    let integration_tag = cmd
        .get("integration")
        .and_then(Value::as_str)
        .unwrap_or("fast")
        .to_ascii_lowercase();
    let integration_tau_s = match integration_tag.as_str() {
        "fast" => ac_core::visualize::time_integration::TAU_FAST_S,
        "slow" => ac_core::visualize::time_integration::TAU_SLOW_S,
        _ => {
            return json!({"ok": false, "error": format!("integration must be 'fast' or 'slow', got '{integration_tag}'")})
        }
    };

    let cfg = state.cfg.lock().unwrap().clone();
    let capture_ports = super::cached_capture_ports(state);

    // Resolve each unique capture channel to a port name once. `unique_ports`
    // drives the JACK port-registration order; each pair indexes into it.
    let mut unique_chans: Vec<u32> = Vec::new();
    for &(m, r) in &pairs {
        for c in [m, r] {
            if !unique_chans.contains(&c) {
                unique_chans.push(c);
            }
        }
    }
    let mut unique_ports = Vec::with_capacity(unique_chans.len());
    for &ch in &unique_chans {
        match capture_ports.get(ch as usize) {
            Some(p) => unique_ports.push(p.clone()),
            None => {
                return json!({"ok": false,
                "error": format!("channel {ch} out of range (n_capture={})", capture_ports.len())})
            }
        }
    }

    // Per-pair buffer indices into the `Vec<Vec<f32>>` that `capture_multi`
    // returns. Precomputed so the worker loop doesn't re-scan per iteration.
    let pair_idx: Vec<(usize, usize)> = pairs
        .iter()
        .map(|&(m, r)| {
            let mi = unique_chans.iter().position(|&c| c == m).unwrap();
            let ri = unique_chans.iter().position(|&c| c == r).unwrap();
            (mi, ri)
        })
        .collect();

    let out_port = resolve_output(&cfg, state);
    let ref_out_port = resolve_ref_output(&cfg, state);

    // Sync routing-capability check so CPAL-only environments get an
    // immediate REP error instead of a silent worker exit that the UI never
    // sees (the async `send_pub("error", …)` path below is still needed for
    // per-capture failures once the worker is live).
    {
        let probe_eng = make_engine(state.fake_audio);
        if !probe_eng.supports_routing() {
            return json!({
                "ok": false,
                "error": format!("{} backend does not support port routing", probe_eng.backend_name()),
            });
        }
    }

    // Calibration discipline (#101): a mic-curve on the **reference**
    // channel is a misconfiguration — H1 = Y/X is a ratio, applying a
    // mic correction to the reference leg cancels against the meas-leg
    // correction (or worse, biases it the wrong way when only one leg
    // has a curve). Refuse the request synchronously with a clear
    // message so the user knows to clear the curve or swap channels.
    let out_ch = cfg.output_channel;
    for &(_, ref_ch) in &pairs {
        if let Ok(Some(cal)) = Calibration::load(out_ch, ref_ch, None) {
            if cal.mic_response.is_some() {
                return json!({
                    "ok": false,
                    "error": format!(
                        "transfer ref channel {ref_ch} has a mic-curve loaded — \
                         transfer is a ratio, applying a curve to the reference leg \
                         is wrong. Run `ac calibrate mic-curve clear input {ref_ch}` \
                         or swap meas/ref."
                    ),
                });
            }
        }
    }

    let pub_tx = state.pub_tx.clone();
    let fake = state.fake_audio;
    let mic_corr_enabled = state.mic_correction_enabled.clone();

    let out_port_r = out_port.clone();
    let meas_port_r = unique_ports.first().cloned().unwrap_or_default();
    let ref_port_r = unique_ports
        .get(1)
        .cloned()
        .unwrap_or_else(|| meas_port_r.clone());
    let pairs_r = pairs.clone();

    // Per-pair measurement-channel mic-curves. Loaded once at worker
    // start; ref-channel curves were already refused above.
    let pair_meas_curves: Vec<Option<ac_core::shared::calibration::MicResponse>> = pairs
        .iter()
        .map(|&(meas, _)| {
            Calibration::load(out_ch, meas, None)
                .ok()
                .flatten()
                .and_then(|c| c.mic_response.clone())
        })
        .collect();

    // Full per-pair calibration (voltage / SPL / mic-curve) for
    // `meas_spectrum`/`ref_spectrum`/`spl`/`cal_tags` (handoff:
    // transfer-frame-v2 M0). Kept side by side with `pair_meas_curves`
    // above (mic_response only, feeds the existing mag/phase/re/im
    // correction) rather than merged, so that path stays untouched
    // (additive-only discipline).
    let pair_meas_cals: Vec<Option<Calibration>> = pairs
        .iter()
        .map(|&(meas, _)| Calibration::load(out_ch, meas, None).ok().flatten())
        .collect();
    let pair_ref_cals: Vec<Option<Calibration>> = pairs
        .iter()
        .map(|&(_, r)| Calibration::load(out_ch, r, None).ok().flatten())
        .collect();

    let worker = spawn_worker(state, "transfer_stream", move |stop| {
        let amplitude = ac_core::shared::generator::dbfs_to_amplitude(level_dbfs);

        // Passive mode (default): open no output ports at all so the daemon
        // doesn't need exclusive access to the playback side — the user is
        // driving the DUT externally. `drive=true` restores the old
        // pink-noise self-stimulus path for the loopback workflow.
        let out_ports: Vec<String> = if !drive {
            Vec::new()
        } else if ref_out_port != out_port {
            vec![out_port.clone(), ref_out_port.clone()]
        } else {
            vec![out_port.clone()]
        };

        let mut eng = make_engine(fake);
        let main_port = unique_ports[0].clone();
        if let Err(e) = eng.start(&out_ports, Some(&main_port)) {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"transfer_stream","message":format!("{e}")}),
            );
            return;
        }
        for p in &unique_ports[1..] {
            if let Err(e) = eng.add_ref_input(p) {
                eprintln!("transfer_stream: warning — ref input {p}: {e}");
            }
        }

        let sr = eng.sample_rate();
        // Fixed log-column grid for `meas_spectrum`/`ref_spectrum` (D18) —
        // computed once since it's constant for the worker's lifetime
        // (sr never changes mid-session); `spectrum_to_columns_wire`'s
        // returned freqs are then identical on every tick by construction
        // (AC #1).
        let spec_f_min = 20.0_f64;
        let spec_f_max = sr as f64 / 2.0;
        let spec_n_columns =
            ac_core::visualize::aggregate::transfer_spectrum_n_columns(spec_f_min, spec_f_max);
        // Sliding window: keep the last `target_total` samples per unique
        // channel and recompute H1 every `chunk_secs`. nperseg/step mirror
        // h1_estimate's internal Welch settings. chunk_secs matches the
        // monitor tick default (50 ms → 20 Hz refresh) — now that per-tick
        // compute is ~5 ms (delay cached), the loop is capture-bound, not
        // compute-bound, and the view matches the FFT/CWT monitor cadence.
        let nperseg = sr as usize; // 1 Hz bin width
        let step = nperseg / 2; // 50% overlap
        let n_averages = 4;
        let target_total = nperseg + step * (n_averages - 1);
        let chunk_secs = 0.05;
        let mut rings: Vec<Vec<f32>> = (0..unique_ports.len())
            .map(|_| Vec::with_capacity(target_total + step))
            .collect();

        if drive {
            eng.set_pink(amplitude);
        }
        let _ = eng.capture_block(0.2); // warmup flush

        // Delay cache: ref↔meas propagation is constant during a streaming
        // session (fixed hardware path), so we estimate once per pair on
        // warmup and reuse the result. Skipping `estimate_delay` per tick
        // (a 262 k-point FFT+IFFT at 2.5 s ring / 48 kHz) cuts the hot-loop
        // work from ~17 ms → ~3 ms and takes the refresh rate from choppy
        // ~8.5 Hz to the capture-interval-limited ~10 Hz.
        let mut pair_delays: Vec<Option<i64>> = vec![None; pairs.len()];

        // Per-pair `spl` time-integration state (F/S EMA, n_bands=1 —
        // handoff: transfer-frame-v2 M0). `None` for a pair whose meas
        // channel has no SPL calibration layer; `spl` stays `null` for
        // that pair's whole session, matching `spl_offsets` in
        // `monitor.rs`. Session-static per D10, so built once here
        // rather than re-checked per tick.
        let mut spl_integrators: Vec<Option<ac_core::visualize::time_integration::EmaIntegrator>> =
            pair_meas_cals
                .iter()
                .map(|c| {
                    c.as_ref().and_then(Calibration::spl_offset_db).map(|_| {
                        ac_core::visualize::time_integration::EmaIntegrator::new(
                            integration_tau_s,
                            1,
                        )
                    })
                })
                .collect();
        let mut spl_last_ts: Vec<Option<std::time::Instant>> = vec![None; pairs.len()];

        while !stop.load(Ordering::Relaxed) {
            let bufs = match eng.capture_multi(chunk_secs) {
                Ok(b) => b,
                Err(e) => {
                    send_pub(
                        &pub_tx,
                        "error",
                        &json!({"cmd":"transfer_stream","message":format!("{e}")}),
                    );
                    break;
                }
            };
            if stop.load(Ordering::Relaxed) {
                break;
            }

            for (i, buf) in bufs.iter().enumerate() {
                if i >= rings.len() {
                    break;
                }
                let r = &mut rings[i];
                r.extend_from_slice(buf);
                if r.len() > target_total {
                    let drop = r.len() - target_total;
                    r.drain(..drop);
                }
            }

            // Warm-up: wait for the rings to fill to at least one Welch
            // segment so the first H1 has meaningful coherence.
            if rings.iter().any(|r| r.len() < nperseg) {
                continue;
            }

            // Estimate any missing per-pair delays once on first entry (after
            // the rings are full). Subsequent ticks reuse the cached value.
            for i in 0..pairs.len() {
                if pair_delays[i].is_some() {
                    continue;
                }
                let (mi, ri) = pair_idx[i];
                if let (Some(meas), Some(refb)) = (rings.get(mi), rings.get(ri)) {
                    pair_delays[i] = Some(ac_core::visualize::transfer::estimate_delay_samples(
                        refb.as_slice(),
                        meas.as_slice(),
                        sr,
                    ));
                }
            }

            // Pairs are independent H1 estimates — fan out across the rayon
            // pool so multi-pair sessions (e.g. 4 mic positions against one
            // reference) scale linearly with core count. The rings are
            // read-only inside the per-pair closure; JSON is built on the
            // worker thread and published back in original pair order.
            let mc_enabled = mic_corr_enabled.load(Ordering::Relaxed);
            // Each pair can contribute multiple wire messages (the
            // transfer_stream frame + a Phase 4b visualize/ir sidecar
            // computed from the same H₁ result). flat_map flattens to
            // one Vec<Value> per filter_map; overall Vec<Vec<Value>>
            // is then chained into the per-message send loop below.
            let messages: Vec<(usize, Vec<Value>, Option<f64>)> = pairs
                .par_iter()
                .enumerate()
                .zip(pair_idx.par_iter())
                .zip(pair_delays.par_iter())
                .zip(pair_meas_curves.par_iter())
                .zip(pair_meas_cals.par_iter())
                .zip(pair_ref_cals.par_iter())
                .filter_map(
                    |((((((pos, &(meas_ch, ref_ch)), &(mi, ri)), &delay_opt), curve_opt), meas_cal_opt), ref_cal_opt)| {
                        let meas = rings.get(mi)?.as_slice();
                        let refb = rings.get(ri)?.as_slice();
                        let delay = delay_opt.unwrap_or(0);
                        let result = ac_core::visualize::transfer::h1_estimate_with_delay(
                            refb, meas, sr, delay,
                        );

                        let n_pts = result.freqs.len();
                        let indices: Vec<usize> = if n_pts > 2000 {
                            let mut idx: Vec<usize> = (0..2000)
                                .map(|i| (i as f64 * (n_pts - 1) as f64 / 1999.0).round() as usize)
                                .collect();
                            idx.dedup();
                            idx
                        } else {
                            (0..n_pts).collect()
                        };

                        let freqs = indices.iter().map(|&i| result.freqs[i]).collect::<Vec<_>>();
                        let mut mag = indices
                            .iter()
                            .map(|&i| result.magnitude_db[i])
                            .collect::<Vec<_>>();
                        let phase = indices
                            .iter()
                            .map(|&i| result.phase_deg[i])
                            .collect::<Vec<_>>();
                        let coh = indices
                            .iter()
                            .map(|&i| result.coherence[i])
                            .collect::<Vec<_>>();
                        // unified.md Phase 3: complex H downsampled in
                        // lockstep so Tier 2 views (Nyquist, IR-via-IFFT,
                        // group-delay-from-complex) get H(ω) directly
                        // without re-deriving from mag/phase. Same `indices`
                        // → guaranteed consistent with mag/phase/coherence.
                        let mut re = indices.iter().map(|&i| result.re[i]).collect::<Vec<_>>();
                        let mut im = indices.iter().map(|&i| result.im[i]).collect::<Vec<_>>();

                        // Mic-curve correction (#101) on the measurement leg
                        // only — the reference leg was guarded above. H1's
                        // dB magnitude has the mic over-read embedded; subtract
                        // the curve at each downsampled bin to recover truth.
                        // Same correction applied multiplicatively to (re, im)
                        // — the curve is magnitude-only (no phase change), so
                        // scaling both re and im by 10^(-curve_db/20)
                        // preserves arg(H) while shrinking |H| consistently.
                        if mc_enabled {
                            if let Some(curve) = curve_opt.as_ref() {
                                mic::apply_mic_curve_inplace_f64(curve, &freqs, &mut mag);
                                for ((r, i), &f) in
                                    re.iter_mut().zip(im.iter_mut()).zip(freqs.iter())
                                {
                                    let corr_db = curve.correction_at(f as f32) as f64;
                                    let scale = 10.0_f64.powf(-corr_db / 20.0);
                                    *r *= scale;
                                    *i *= scale;
                                }
                            }
                        }
                        let mc_tag = mic::mic_correction_tag(curve_opt.is_some(), mc_enabled);

                        // Calibrated per-channel spectra (D18, handoff:
                        // transfer-frame-v2 M0). Full-resolution
                        // `result.meas_amp`/`result.ref_amp` (NOT the
                        // 2000-pt indices above — same full-res-then-
                        // aggregate split the IR sidecar below already
                        // uses) so the mic-curve's per-freq correction is
                        // applied at native Welch-bin resolution, then
                        // `spectrum_to_columns_wire` band-power-aggregates
                        // to the fixed grid — same aggregator, same tests,
                        // the monitor `spectrum` frame already uses.
                        let mut mc_meas_amp = result.meas_amp.clone();
                        if mc_enabled {
                            if let Some(curve) = curve_opt.as_ref() {
                                for (amp, &f) in
                                    mc_meas_amp.iter_mut().zip(result.freqs.iter())
                                {
                                    let corr_db = curve.correction_at(f as f32) as f64;
                                    *amp *= 10.0_f64.powf(-corr_db / 20.0);
                                }
                            }
                        }
                        // `spl` is computed from the mic-corrected (acoustic
                        // truth) spectrum, before voltage-cal scaling below
                        // — SPL derives from the dBFS + spl_offset_db model
                        // (Calibration::spl_offset_db), independent of the
                        // electrical Vrms voltage-cal layer.
                        let spl_raw: Option<f64> = meas_cal_opt
                            .as_ref()
                            .and_then(Calibration::spl_offset_db)
                            .map(|offset| {
                                ac_core::visualize::spl_level::weighted_broadband_dbfs(
                                    &mc_meas_amp,
                                    &result.freqs,
                                    weighting,
                                ) + offset
                            });

                        // Voltage cal (D3): a constant per-channel scale
                        // factor, so it commutes with column aggregation
                        // (`sqrt(Σ(c·x)²) = c·sqrt(Σx²)`) — applied here,
                        // post mic-curve, pre-aggregation.
                        let meas_amp_wire = {
                            let mut a = mc_meas_amp;
                            if let Some(scale) =
                                meas_cal_opt.as_ref().and_then(|c| c.vrms_at_0dbfs_in)
                            {
                                for v in a.iter_mut() {
                                    *v *= scale;
                                }
                            }
                            a
                        };
                        let ref_amp_wire = {
                            let mut a = result.ref_amp.clone();
                            if let Some(scale) =
                                ref_cal_opt.as_ref().and_then(|c| c.vrms_at_0dbfs_in)
                            {
                                for v in a.iter_mut() {
                                    *v *= scale;
                                }
                            }
                            a
                        };
                        let (meas_spectrum, spec_freqs) =
                            ac_core::visualize::aggregate::spectrum_to_columns_wire(
                                &meas_amp_wire,
                                sr as f64,
                                spec_f_min,
                                spec_f_max,
                                spec_n_columns,
                            );
                        let (ref_spectrum, _) =
                            ac_core::visualize::aggregate::spectrum_to_columns_wire(
                                &ref_amp_wire,
                                sr as f64,
                                spec_f_min,
                                spec_f_max,
                                spec_n_columns,
                            );

                        // Per-channel provenance tags (tier-framing
                        // labelled-tag rules, #97/#98 vocabulary):
                        // "on"/"none" for voltage and SPL (no daemon-side
                        // enable toggle for either, unlike mic-curve);
                        // "on"/"off"/"none" for mic_curve via the existing
                        // `mic_correction_tag`. Ref leg's mic_curve is
                        // structurally always "none" — a ref-channel mic
                        // curve is refused at request time, above.
                        let ref_curve_loaded = ref_cal_opt
                            .as_ref()
                            .is_some_and(|c| c.mic_response.is_some());
                        let cal_tags = json!({
                            "meas": {
                                "voltage": if meas_cal_opt.as_ref().and_then(|c| c.vrms_at_0dbfs_in).is_some() { "on" } else { "none" },
                                "spl":     if meas_cal_opt.as_ref().and_then(Calibration::spl_offset_db).is_some() { "on" } else { "none" },
                                "mic_curve": mc_tag,
                            },
                            "ref": {
                                "voltage": if ref_cal_opt.as_ref().and_then(|c| c.vrms_at_0dbfs_in).is_some() { "on" } else { "none" },
                                "spl":     if ref_cal_opt.as_ref().and_then(Calibration::spl_offset_db).is_some() { "on" } else { "none" },
                                "mic_curve": mic::mic_correction_tag(ref_curve_loaded, mc_enabled),
                            },
                        });

                        let transfer_msg = json!({
                            "type":            "transfer_stream",
                            "cmd":             "transfer_stream",
                            "freqs":           freqs,
                            "magnitude_db":    mag,
                            "phase_deg":       phase,
                            "coherence":       coh,
                            "re":              re,
                            "im":              im,
                            "delay_samples":   result.delay_samples,
                            "delay_ms":        result.delay_ms,
                            "ref_channel":     ref_ch,
                            "meas_channel":    meas_ch,
                            "sr":              sr,
                            "mic_correction":  mc_tag,
                            "spec_freqs":      spec_freqs,
                            "meas_spectrum":   meas_spectrum,
                            "ref_spectrum":    ref_spectrum,
                            "spl":             Value::Null,
                            "spl_weighting":   weighting.tag(),
                            "spl_integration": integration_tag.as_str(),
                            "cal_tags":        cal_tags,
                        });
                        // Phase 4b: IR sidecar from full-resolution
                        // complex H (NOT the downsampled re/im above —
                        // the IFFT needs the raw nperseg/2+1 bins to
                        // recover h(t) correctly). Time-domain output is
                        // 1 s long at sr (matches the 1 Hz Welch
                        // resolution); downsample to ≤2000 samples for
                        // wire economy by stride-picking. Mic-curve
                        // correction is intentionally NOT applied to the
                        // IR — the downsampled re/im version already
                        // would have it, but the full-resolution H here
                        // does not (mic-curve correction in the curve_opt
                        // branch above only touches the downsampled mag /
                        // re / im). For the visualization-only Tier 2 IR
                        // view this is acceptable; if a Tier-1 calibrated
                        // IR is wanted, that path goes through the sweep
                        // measurement, not transfer_stream.
                        let h_full_re = &result.re;
                        let h_full_im = &result.im;
                        let ir_full = ac_core::visualize::transfer::impulse_response_from_h(
                            h_full_re, h_full_im,
                        );
                        let ir_msg = if ir_full.is_empty() {
                            None
                        } else {
                            const IR_MAX_SAMPLES: usize = 2000;
                            let stride = (ir_full.len() / IR_MAX_SAMPLES).max(1);
                            let ir_ds: Vec<f32> = ir_full.iter().step_by(stride).copied().collect();
                            // t_origin_ms = -mid_ms because
                            // impulse_response_from_h centres the IR
                            // peak at the middle of the array (t=0 in
                            // the user's mental model).
                            let dt_ms = 1000.0 / sr as f64 * stride as f64;
                            let t_origin_ms = -((ir_ds.len() / 2) as f64) * dt_ms;
                            Some(json!({
                                "type":         "visualize/ir",
                                "cmd":          "transfer_stream",
                                "samples":      ir_ds,
                                "sr":           sr,
                                "stride":       stride,
                                "dt_ms":        dt_ms,
                                "t_origin_ms":  t_origin_ms,
                                "ref_channel":  ref_ch,
                                "meas_channel": meas_ch,
                                "delay_samples": result.delay_samples,
                                "delay_ms":     result.delay_ms,
                            }))
                        };
                        let mut out = vec![transfer_msg];
                        if let Some(m) = ir_msg {
                            out.push(m);
                        }
                        Some((pos, out, spl_raw))
                    },
                )
                .collect();
            for (pos, mut batch, spl_raw) in messages {
                // Sequential, indexed by original pair position — the
                // EMA integrator holds mutable per-pair state that can't
                // safely live inside the parallel closure above (#pos is
                // preserved through `filter_map` via `enumerate()` at the
                // top of the chain, not the post-filter Vec position).
                if let (Some(raw), Some(integ)) = (spl_raw, spl_integrators[pos].as_mut()) {
                    let now = std::time::Instant::now();
                    let dt = spl_last_ts[pos]
                        .map(|t| now.duration_since(t).as_secs_f64())
                        .unwrap_or(chunk_secs)
                        .max(1e-6);
                    spl_last_ts[pos] = Some(now);
                    let integrated = integ.update(&[raw], dt)[0];
                    if let Some(first) = batch.first_mut() {
                        first["spl"] = json!(integrated);
                    }
                }
                for msg in batch {
                    send_pub(&pub_tx, "data", &msg);
                }
            }
        }

        if drive {
            eng.set_silence();
        }
        eng.stop();
        send_pub(
            &pub_tx,
            "done",
            &json!({"cmd":"transfer_stream","stopped":true}),
        );
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("transfer_stream".to_string(), worker);
    }
    json!({
        "ok":           true,
        "out_port":     out_port_r,
        "meas_port":    meas_port_r,
        "ref_port":     ref_port_r,
        "pairs":        pairs_r,
        // Legacy fields — filled with the first pair so old clients keep working.
        "meas_channel": pairs_r.first().map(|p| p.0).unwrap_or(0),
        "ref_channel":  pairs_r.first().map(|p| p.1).unwrap_or(0),
    })
}

pub fn probe(state: &ServerState, _cmd: &Value) -> Value {
    busy_guard!(state, "probe");

    let fake = state.fake_audio;
    let pub_tx = state.pub_tx.clone();
    let cfg = state.cfg.lock().unwrap().clone();
    let dmm_host = cfg.dmm_host.clone();

    let (playback, capture) = (
        super::cached_playback_ports(state),
        super::cached_capture_ports(state),
    );
    let n_play = playback.len();
    let n_cap = capture.len();

    let worker = spawn_worker(state, "probe", move |stop| {
        let threshold_rms: f64 = 0.010 / (2.0f64.sqrt()); // 10 mVrms ≈ this linear RMS

        let freq = 1000.0;
        let amplitude = ac_core::shared::generator::dbfs_to_amplitude(-10.0);

        let mut eng = make_engine(fake);
        if !eng.supports_routing() {
            send_pub(
                &pub_tx,
                "error",
                &json!({
                    "cmd":     "probe",
                    "message": format!("{} backend does not support port routing", eng.backend_name()),
                }),
            );
            return;
        }
        if playback.is_empty() {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"probe","message":"no playback ports"}),
            );
            return;
        }

        if let Err(e) = eng.start(&[playback[0].clone()], None) {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"probe","message":format!("{e}")}),
            );
            return;
        }
        eng.set_tone(freq, amplitude);
        eng.disconnect_output(&playback[0]);

        // Phase 1: DMM output scan
        let mut analog_channels: Vec<usize> = Vec::new();
        if let Some(ref host) = dmm_host {
            send_pub(
                &pub_tx,
                "data",
                &json!({
                    "cmd": "probe", "phase": "output_start", "n_ports": n_play
                }),
            );
            for (i, port) in playback.iter().enumerate() {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                eng.connect_output(port).ok();
                std::thread::sleep(std::time::Duration::from_millis(400));
                let vrms = read_dmm_vrms(host, 3);
                eng.disconnect_output(port);
                let is_analog = vrms.map(|v| v > threshold_rms).unwrap_or(false);
                if is_analog {
                    analog_channels.push(i);
                }
                send_pub(
                    &pub_tx,
                    "data",
                    &json!({
                        "cmd": "probe", "phase": "output",
                        "channel": i, "port": port,
                        "vrms": vrms, "analog": is_analog,
                    }),
                );
            }
        } else {
            send_pub(
                &pub_tx,
                "data",
                &json!({
                    "cmd": "probe", "phase": "output_skip",
                    "message": "no DMM configured — skipping output scan",
                }),
            );
            analog_channels = (0..n_play).collect();
        }

        // Phase 2: Loopback detection
        if !stop.load(Ordering::Relaxed) {
            send_pub(
                &pub_tx,
                "data",
                &json!({
                    "cmd": "probe", "phase": "loopback_start",
                    "n_outputs": analog_channels.len(), "n_inputs": n_cap,
                }),
            );
        }

        if let Some(cap0) = capture.first() {
            eng.reconnect_input(cap0).ok();
        }

        for &out_idx in &analog_channels {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            eng.connect_output(&playback[out_idx]).ok();
            std::thread::sleep(std::time::Duration::from_millis(150));

            for (j, cap_port) in capture.iter().enumerate() {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                eng.reconnect_input(cap_port).ok();
                eng.flush_capture();
                std::thread::sleep(std::time::Duration::from_millis(50));
                let level_dbfs = match eng.capture_block(0.05) {
                    Ok(data) => {
                        let rms = (data.iter().map(|&x| (x as f64).powi(2)).sum::<f64>()
                            / data.len().max(1) as f64)
                            .sqrt();
                        20.0 * rms.max(1e-12).log10()
                    }
                    Err(_) => -120.0,
                };
                if level_dbfs > -30.0 {
                    send_pub(
                        &pub_tx,
                        "data",
                        &json!({
                            "cmd": "probe", "phase": "loopback",
                            "out_ch": out_idx, "out_port": &playback[out_idx],
                            "in_ch": j, "in_port": cap_port,
                            "level_dbfs": (level_dbfs * 10.0).round() / 10.0,
                        }),
                    );
                }
            }
            eng.disconnect_output(&playback[out_idx]);
        }

        eng.set_silence();
        eng.stop();
        send_pub(
            &pub_tx,
            "done",
            &json!({
                "cmd": "probe",
                "analog_channels": analog_channels,
            }),
        );
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("probe".to_string(), worker);
    }
    json!({ "ok": true, "n_playback": n_play, "n_capture": n_cap })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pairs_multi() {
        let cmd = json!({ "pairs": [[0, 3], [1, 3], [2, 3]] });
        assert_eq!(
            parse_transfer_pairs(&cmd).unwrap(),
            vec![(0, 3), (1, 3), (2, 3)]
        );
    }

    #[test]
    fn parse_pairs_dedups() {
        let cmd = json!({ "pairs": [[0, 3], [1, 3], [0, 3]] });
        assert_eq!(parse_transfer_pairs(&cmd).unwrap(), vec![(0, 3), (1, 3)]);
    }

    #[test]
    fn parse_pairs_legacy_single() {
        let cmd = json!({ "meas_channel": 0, "ref_channel": 3 });
        assert_eq!(parse_transfer_pairs(&cmd).unwrap(), vec![(0, 3)]);
    }

    #[test]
    fn parse_pairs_empty_errors() {
        let cmd = json!({ "pairs": [] });
        assert!(parse_transfer_pairs(&cmd).is_err());
    }

    #[test]
    fn parse_pairs_malformed_element_errors() {
        let cmd = json!({ "pairs": [[0, 3], [1]] });
        assert!(parse_transfer_pairs(&cmd).is_err());
    }

    #[test]
    fn parse_pairs_missing_fields_errors() {
        let cmd = json!({});
        assert!(parse_transfer_pairs(&cmd).is_err());
    }
}
