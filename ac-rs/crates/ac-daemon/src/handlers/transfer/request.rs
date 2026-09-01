//! The `transfer_stream` launch contract: what a request may say, what it
//! means, and what it is refused for.
//!
//! Everything here is a pure function of the request `Value`. No
//! `ServerState`, no config, no audio backend — see [`parse_params`] for why
//! that is a constraint rather than a coincidence.

use serde_json::Value;

/// Parse the `pairs` and legacy `meas_channel`/`ref_channel` shapes of
/// `transfer_stream` into a canonical pair list. Returns an Err message
/// suitable for `{"ok": false, "error": ...}` on malformed input.
pub(super) fn parse_transfer_pairs(cmd: &Value) -> Result<Vec<(u32, u32)>, String> {
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

/// Every `transfer_stream` launch parameter that comes from the request
/// alone, validated.
///
/// Deliberately a pure function of `cmd`: it reads no `ServerState` and
/// no config, so the whole of the request contract — defaults, ranges,
/// and the three rejections — is decidable without a daemon, a socket, or
/// an audio backend. That is why `level_dbfs` is left UNCLAMPED here; the
/// ceiling is `cfg.drive_max_dbfs`, and folding it in would drag the
/// config in and make the reachable-value tests need one.
#[derive(Debug)]
pub(super) struct TransferParams {
    pub(super) drive: bool,
    pub(super) drivable: bool,
    /// As requested. Still to be clamped to `cfg.drive_max_dbfs` by the
    /// caller (#360) before it reaches `DriveState` or a loudspeaker.
    pub(super) level_dbfs: f64,
    pub(super) fake_correlated_pair: Option<(f64, usize)>,
    pub(super) fake_ring_process_secs: Option<f64>,
    pub(super) fake_ring_period: usize,
    pub(super) mtw_ppo: f64,
    pub(super) mtw_n_blocks: usize,
    pub(super) pairs: Vec<(u32, u32)>,
    pub(super) weighting: ac_core::visualize::weighting_curves::WeightingCurve,
    /// Normalised (lower-cased) tag, echoed on every frame as
    /// `spl_integration`.
    pub(super) integration_tag: String,
    pub(super) integration_tau_s: f64,
}

/// Parse and validate `transfer_stream`'s launch parameters. `Err` is a
/// message suitable for `{"ok": false, "error": ...}`.
///
/// Out-of-range numeric knobs (`mtw_ppo`, `mtw_n_blocks`) fall back to
/// their defaults rather than erroring — that is the pre-existing
/// contract, preserved here, and `filter` is what implements it.
pub(super) fn parse_params(cmd: &Value) -> Result<TransferParams, String> {
    // `drive` controls whether the daemon plays pink noise on the output
    // while capturing. Default `false` — the UI wants a purely passive H1
    // estimate against whatever the user is already driving into the inputs.
    // Set `true` to restore the old self-driving behavior (with `level_dbfs`
    // controlling amplitude).
    let drive = cmd.get("drive").and_then(Value::as_bool).unwrap_or(false);
    // `drivable` opens and connects the output ports at launch while staying
    // silent — the shape `ac transfer` needs, where drive arrives later via
    // `set_drive`. Legacy `drive=true` implies drivable. Neither means fully
    // passive: no output ports at all (external-DUT workflow).
    let drivable = drive
        || cmd
            .get("drivable")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let level_dbfs = cmd
        .get("level_dbfs")
        .and_then(Value::as_f64)
        .unwrap_or(-10.0);

    // Fake-audio-only stimulus knob (handoff: parity-completion M1.5),
    // same pattern as `monitor_spectrum`'s `fake_tones`/`fake_noise_dbfs`
    // (`handlers/audio/monitor.rs`): read unconditionally here, applied
    // only inside `if fake { ... }` in the worker — real backends never
    // see it.
    let fake_correlated_pair: Option<(f64, usize)> =
        cmd.get("fake_correlated_pair").and_then(|v| {
            let gain = v.get("gain").and_then(Value::as_f64)?;
            let delay_samples = v.get("delay_samples").and_then(Value::as_u64)?;
            Some((gain, delay_samples as usize))
        });

    // Fake-audio-only capture knob (handoff-capture-contiguity D1): route
    // fake capture through a real ring so `clear()`-before-wait has the same
    // observable consequence it has on JACK. Presence of the key selects the
    // mode; absence leaves the on-demand generator in place, unchanged.
    //
    // `process_secs` defaults to the transfer worker's own measured compute
    // on the tick that matters — the one where the ring crosses a block
    // boundary and the analysis is recomputed: ~5 ms per pair in release,
    // against ~1 ms on the nine ticks in ten that reuse the held estimate.
    // Modelling the worst tick is the point: a splice is a function of the
    // longest gap, not the average one.
    let fake_ring_process_secs: Option<f64> = cmd.get("fake_ring").map(|v| {
        v.get("process_secs")
            .and_then(Value::as_f64)
            .unwrap_or(0.005)
    });
    // Producer granularity. A real backend hands the ring one whole period at
    // a time, and that quantisation — not the gap length — is what decides
    // which stimulus frequencies expose the splice at all: a tone at an exact
    // multiple of `sr/period` survives a discarded period with zero phase
    // error. 1024 matches the verified rig (RME Babyface Pro at 96 kHz).
    let fake_ring_period: usize = cmd
        .get("fake_ring")
        .and_then(|v| v.get("period"))
        .and_then(Value::as_u64)
        .unwrap_or(1024) as usize;

    // Multi-time-window ladder parameters. Display density is a **parameter**,
    // not a constant — where it exceeds what a stage can resolve the column
    // grid widens rather than interpolating, so asking for more here buys
    // resolution only where the ladder can back it. It does not move the
    // ladder's crossovers, which are anchored to `mtw::ladder::P_REF`.
    let mtw_ppo: f64 = cmd
        .get("mtw_ppo")
        .and_then(Value::as_f64)
        .filter(|v| *v > 0.0 && *v <= 384.0)
        .unwrap_or(ac_core::visualize::mtw::ladder::P_REF);
    // Blocks averaged per stage, held *uniform across stages* so the coherence
    // bias `1/N` is the same either side of every crossover. A uniform
    // wall-clock time constant would instead give ~47 averages at stage 0
    // against ~1.5 at stage 2, putting a fixed-frequency step in the trust
    // indicator that reads as a property of the DUT.
    //
    // Lowering it speeds the bottom stage up but raises the coherence floor
    // across the whole display, since N is uniform: 0.33 at N = 3, 0.50 at
    // N = 2, and at 0.50 a coherence reading has stopped meaning anything.
    let mtw_n_blocks: usize = cmd
        .get("mtw_n_blocks")
        .and_then(Value::as_u64)
        .filter(|v| *v >= 1 && *v <= 64)
        .map(|v| v as usize)
        .unwrap_or(ac_core::visualize::mtw::average::DEFAULT_N_BLOCKS);

    let pairs = parse_transfer_pairs(cmd)?;

    // Per-meas-channel SPL session params (D10 — static for the session,
    // set once here, not live-toggleable). `weighting`'s wire contract
    // (handoff: transfer-frame-v2 M0) is a strict 3-way A/C/Z — no
    // "off" — so `WeightingCurve::from_tag`'s existing rejection of
    // anything else (including "off") is exactly the validation wanted.
    let weighting_tag = cmd.get("weighting").and_then(Value::as_str).unwrap_or("Z");
    let weighting =
        ac_core::visualize::weighting_curves::WeightingCurve::from_tag(weighting_tag)
            .ok_or_else(|| format!("weighting must be one of A, C, Z, got '{weighting_tag}'"))?;
    let integration_tag = cmd
        .get("integration")
        .and_then(Value::as_str)
        .unwrap_or("fast")
        .to_ascii_lowercase();
    let integration_tau_s = match integration_tag.as_str() {
        "fast" => ac_core::visualize::time_integration::TAU_FAST_S,
        "slow" => ac_core::visualize::time_integration::TAU_SLOW_S,
        _ => {
            return Err(format!(
                "integration must be 'fast' or 'slow', got '{integration_tag}'"
            ))
        }
    };

    Ok(TransferParams {
        drive,
        drivable,
        level_dbfs,
        fake_correlated_pair,
        fake_ring_process_secs,
        fake_ring_period,
        mtw_ppo,
        mtw_n_blocks,
        pairs,
        weighting,
        integration_tag,
        integration_tau_s,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    // ---- parse_params -------------------------------------------------
    //
    // These exist because `parse_params` reads no `ServerState` and no
    // config. Every one of them used to require a live daemon to reach.

    fn params(v: Value) -> Result<TransferParams, String> {
        parse_params(&v)
    }

    #[test]
    fn params_defaults_are_passive_and_z_weighted() {
        let p = params(json!({"pairs": [[0, 1]]})).unwrap();
        assert!(!p.drive, "default session must not drive");
        assert!(!p.drivable, "default session must open no output ports");
        assert_eq!(p.level_dbfs, -10.0);
        assert_eq!(p.weighting.tag(), "Z");
        assert_eq!(p.integration_tag, "fast");
        assert_eq!(
            p.integration_tau_s,
            ac_core::visualize::time_integration::TAU_FAST_S
        );
        assert!(p.fake_correlated_pair.is_none());
        assert!(p.fake_ring_process_secs.is_none());
    }

    // Legacy `drive: true` must still imply drivable, or the generator
    // plays onto a port that was never opened.
    #[test]
    fn params_drive_implies_drivable() {
        let p = params(json!({"pairs": [[0, 1]], "drive": true})).unwrap();
        assert!(p.drivable);
    }

    #[test]
    fn params_drivable_alone_does_not_drive() {
        let p = params(json!({"pairs": [[0, 1]], "drivable": true})).unwrap();
        assert!(p.drivable);
        assert!(!p.drive);
    }

    // #360: the ceiling is `cfg.drive_max_dbfs`, which this fn cannot see.
    // It must therefore hand back exactly what was asked for — a clamp
    // appearing here would be a second, config-blind ceiling.
    #[test]
    fn params_do_not_clamp_level() {
        let p = params(json!({"pairs": [[0, 1]], "level_dbfs": 0.0})).unwrap();
        assert_eq!(p.level_dbfs, 0.0);
    }

    // The wire contract is a strict 3-way A/C/Z. "off" is the specific
    // value worth pinning: it is accepted by other weighting knobs in this
    // daemon and must be refused here.
    #[test]
    fn params_reject_off_weighting() {
        assert!(params(json!({"pairs": [[0, 1]], "weighting": "off"})).is_err());
    }

    #[test]
    fn params_reject_unknown_weighting() {
        let e = params(json!({"pairs": [[0, 1]], "weighting": "B"})).unwrap_err();
        assert!(e.contains("A, C, Z"), "{e}");
    }

    #[test]
    fn params_accept_lowercase_weighting() {
        assert_eq!(
            params(json!({"pairs": [[0, 1]], "weighting": "a"}))
                .unwrap()
                .weighting
                .tag(),
            "A"
        );
    }

    #[test]
    fn params_integration_slow_and_case_insensitive() {
        let p = params(json!({"pairs": [[0, 1]], "integration": "SLOW"})).unwrap();
        assert_eq!(p.integration_tag, "slow", "tag is normalised for the wire");
        assert_eq!(
            p.integration_tau_s,
            ac_core::visualize::time_integration::TAU_SLOW_S
        );
    }

    #[test]
    fn params_reject_unknown_integration() {
        assert!(params(json!({"pairs": [[0, 1]], "integration": "medium"})).is_err());
    }

    // Out-of-range ladder knobs fall back to the default rather than
    // erroring. That is the pre-existing contract; this test is here so a
    // future tightening to a rejection is a visible decision rather than a
    // silent one.
    #[test]
    fn params_out_of_range_ladder_knobs_fall_back() {
        let p = params(json!({
            "pairs": [[0, 1]],
            "mtw_ppo": 10_000.0,
            "mtw_n_blocks": 0,
        }))
        .unwrap();
        assert_eq!(p.mtw_ppo, ac_core::visualize::mtw::ladder::P_REF);
        assert_eq!(
            p.mtw_n_blocks,
            ac_core::visualize::mtw::average::DEFAULT_N_BLOCKS
        );
    }

    #[test]
    fn params_in_range_ladder_knobs_are_taken() {
        let p = params(json!({"pairs": [[0, 1]], "mtw_ppo": 24.0, "mtw_n_blocks": 8})).unwrap();
        assert_eq!(p.mtw_ppo, 24.0);
        assert_eq!(p.mtw_n_blocks, 8);
    }

    // Presence of the key selects ring mode; its absence leaves the
    // on-demand generator in place. An empty object is still presence.
    #[test]
    fn params_fake_ring_presence_selects_mode() {
        let p = params(json!({"pairs": [[0, 1]], "fake_ring": {}})).unwrap();
        assert_eq!(p.fake_ring_process_secs, Some(0.005));
        assert_eq!(p.fake_ring_period, 1024);
    }

    #[test]
    fn params_pair_error_propagates() {
        assert!(params(json!({"pairs": []})).is_err());
    }
}
