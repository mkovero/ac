//! Wire assembly: the `transfer_stream` frame, the settling frame that
//! precedes it, and the `visualize/ir` sidecar.
//!
//! Nothing here computes an estimate. Everything expensive happened in
//! [`super::analysis`]; what is left is building JSON from a held estimate
//! plus this tick's live scalars, which is what lets a frame ship on every
//! capture tick while the estimate behind it advances at the ring's rate.

use serde_json::{json, Value};

use ac_core::shared::calibration::Calibration;

use crate::handlers::mic;

use super::analysis::PairAnalysis;
use super::pair::{PairCtx, PairState};

/// `20·log10(max|sample|)` over one capture block, or `None` for
/// digital silence (which would be `-inf`, unrepresentable in JSON).
///
/// Takes raw capture samples. There is no calibrated variant of this on
/// purpose: a voltage-calibrated frame must not move the input meters.
pub(super) fn raw_peak_dbfs(block: &[f32]) -> Option<f64> {
    let peak = block.iter().fold(0.0f32, |acc, &s| acc.max(s.abs()));
    if peak <= 0.0 {
        return None;
    }
    Some(20.0 * (peak as f64).log10())
}

/// Frame inputs that are fixed for the worker's whole life.
///
/// Split from [`TickInputs`] on exactly that axis: anything here is
/// derived from `sr` or from a launch parameter, so a test can build one
/// and reuse it, and a reviewer can see at a glance that nothing in it
/// can drift between ticks.
pub(super) struct FrameStatics {
    pub(super) sr: u32,
    /// Fixed log-column grid for `meas_spectrum`/`ref_spectrum` (D18).
    pub(super) spec_f_min: f64,
    pub(super) spec_f_max: f64,
    pub(super) spec_n_columns: usize,
    pub(super) weighting: ac_core::visualize::weighting_curves::WeightingCurve,
    pub(super) integration_tag: String,
    pub(super) mtw_ppo: f64,
    pub(super) mtw_n_blocks: usize,
    /// Ladder description, shipped whole with every frame so a consumer
    /// can interpret a column's `stage` without knowing the layout rules,
    /// and so a saved frame stays interpretable if those rules change.
    pub(super) mtw_stages: Value,
}

/// Frame inputs that change every capture tick.
///
/// The rings are deliberately absent: assembly reads the cached
/// [`PairAnalysis`] instead, which is what lets a frame ship on every tick
/// while the estimate behind it advances only when the ring does.
pub(super) struct TickInputs<'a> {
    /// Raw pre-calibration peaks (§4.2) from THIS tick's blocks, indexed
    /// by `PairCtx::mi`/`ri`.
    pub(super) tick_peaks_dbfs: &'a [Option<f64>],
    /// Global mic-correction toggle, sampled once per tick so every pair in
    /// a frame agrees about it.
    pub(super) mc_enabled: bool,
    /// Observed drive state (#228), identical for every pair in the tick.
    pub(super) drive_msg: &'a Value,
    /// Ladder columns and settled-rung flags, indexed by `PairCtx::pos`.
    /// Recomputed every tick — the ladder is a push pipeline.
    pub(super) mtw_columns: &'a [Option<Vec<ac_core::visualize::mtw::splice::Column>>],
    pub(super) mtw_settled: &'a [Vec<bool>],
    /// The held H1 estimate per pair, indexed by `PairCtx::pos`. `None`
    /// means no segment yet, which publishes a settling frame.
    pub(super) analysis: &'a [Option<PairAnalysis>],
    /// Capture channels the session has rings for. A pair naming a channel
    /// outside that is dropped from the frame entirely rather than
    /// publishing a partial one (#254) — which is a different thing from
    /// having no estimate yet, and must not be reported as one.
    pub(super) n_channels: usize,
}

/// Per-channel provenance tags (tier-framing labelled-tag rules, #97/#98
/// vocabulary): `"on"`/`"none"` for voltage and SPL — neither has a
/// daemon-side enable toggle, unlike mic-curve — and `"on"`/`"off"`/
/// `"none"` for `mic_curve` via `mic_correction_tag`.
///
/// The reference leg's `mic_curve` is structurally almost always
/// `"none"`: a ref-channel mic curve is refused at request time.
///
/// Shared by the settling frame and the analysis frame so the two cannot
/// disagree about a session constant.
pub(super) fn cal_tags_value(
    meas_cal: Option<&Calibration>,
    ref_cal: Option<&Calibration>,
    meas_mic_tag: &str,
    mc_enabled: bool,
) -> Value {
    let ref_curve_loaded = ref_cal.is_some_and(|c| c.mic_response.is_some());
    json!({
        "meas": {
            "voltage": if meas_cal.and_then(|c| c.vrms_at_0dbfs_in).is_some() { "on" } else { "none" },
            "spl":     if meas_cal.and_then(Calibration::spl_offset_db).is_some() { "on" } else { "none" },
            "mic_curve": meas_mic_tag,
        },
        "ref": {
            "voltage": if ref_cal.and_then(|c| c.vrms_at_0dbfs_in).is_some() { "on" } else { "none" },
            "spl":     if ref_cal.and_then(Calibration::spl_offset_db).is_some() { "on" } else { "none" },
            "mic_curve": mic::mic_correction_tag(ref_curve_loaded, mc_enabled),
        },
    })
}

/// The frame a pair publishes before its ring holds a whole Welch segment.
///
/// Same key set as the analysis frame, with every H1-derived field empty
/// or null and `n_averages: 0` saying so. What it does carry is everything
/// that never depended on the analysis window: the observed drive state,
/// the raw capture peaks, the attempt count, and the calibration tags.
///
/// The alternative — the loop `continue`ing until the window fills —
/// suppressed those too, so for the first second of a session a client
/// could not tell a daemon that had not started from one whose drive had
/// already dead-manned, and `ac-scene::fault` had no frame to read. The
/// analysis window and time-to-first-frame are different quantities and
/// this is what stops one setting the other.
///
/// `spec_freqs` is empty here rather than carrying the session's fixed
/// grid, so the three spectrum arrays agree in length on this frame as
/// they do on every other.
#[allow(clippy::too_many_arguments)]
pub(super) fn settling_frame(
    ctx: &PairCtx,
    st: &PairState,
    statics: &FrameStatics,
    meas_peak: Value,
    ref_peak: Value,
    mc_tag: &str,
    mc_enabled: bool,
    drive_msg: &Value,
) -> Value {
    json!({
        "type":            "transfer_stream",
        "cmd":             "transfer_stream",
        "mtw":             Value::Null,
        "freqs":           Vec::<f64>::new(),
        "magnitude_db":    Vec::<f64>::new(),
        "phase_deg":       Vec::<f64>::new(),
        "coherence":       Vec::<f64>::new(),
        "delay_samples":   0,
        "delay_ms":        0.0,
        "delay_locked":    false,
        "delay_attempts":  st.attempts,
        "delay_evidence":  st.prominence,
        "meas_peak_dbfs":  meas_peak,
        "ref_peak_dbfs":   ref_peak,
        "ref_channel":     ctx.ref_ch,
        "meas_channel":    ctx.meas_ch,
        "sr":              statics.sr,
        // Zero blocks: this frame carries no Welch estimate at all, which
        // is a different statement from the `1` a first-segment frame
        // makes. A consumer reading coherence's `1/N` bias needs the
        // difference, and so does anyone deciding whether an empty
        // magnitude array is a fault or a start.
        "n_averages":      0,
        // No estimate exists to number. `null` rather than 0, so the
        // first real estimate's `0` cannot be mistaken for a repeat of
        // something that was never sent.
        "analysis_seq":    Value::Null,
        "mic_correction":  mc_tag,
        "spec_freqs":      Vec::<f64>::new(),
        "meas_spectrum":   Vec::<f64>::new(),
        "ref_spectrum":    Vec::<f64>::new(),
        "spl":             Value::Null,
        "spl_weighting":   statics.weighting.tag(),
        "spl_integration": statics.integration_tag.as_str(),
        "cal_tags":        cal_tags_value(ctx.meas_cal.as_ref(), ctx.ref_cal.as_ref(), mc_tag, mc_enabled),
        "drive":           drive_msg.clone(),
    })
}

/// Build one pair's wire messages for this tick: the `transfer_stream`
/// frame, plus a Phase 4b `visualize/ir` sidecar when there is an
/// estimate to derive one from. Returns the pair's launch position
/// alongside them, and the **un-integrated** broadband SPL — integration
/// holds `&mut` per-pair state and so happens on the worker thread, after
/// the fan-out.
///
/// Everything expensive already happened in [`analyse_pair`]; what is
/// left is assembly from that plus this tick's live scalars, which is why
/// a frame can ship every tick while the estimate behind it advances at
/// the ring's own rate.
pub(super) fn build_pair_messages(
    ctx: &PairCtx,
    st: &PairState,
    statics: &FrameStatics,
    tick: &TickInputs<'_>,
) -> Option<(usize, Vec<Value>, Option<f64>)> {
    let &PairCtx {
        pos,
        meas_ch,
        ref_ch,
        mi,
        ri,
        ..
    } = ctx;
    let &TickInputs {
        tick_peaks_dbfs,
        mc_enabled,
        drive_msg,
        mtw_columns,
        mtw_settled,
        analysis,
        n_channels,
    } = tick;
    if mi >= n_channels || ri >= n_channels {
        return None;
    }
    let &FrameStatics {
        sr,
        mtw_ppo,
        mtw_n_blocks,
        ..
    } = statics;
    // `-inf` (digital silence) travels as JSON null: serde_json cannot
    // serialise a non-finite float, so the conversion is explicit here
    // rather than left to the `json!` site.
    let meas_peak: Value = tick_peaks_dbfs
        .get(mi)
        .copied()
        .flatten()
        .map(Value::from)
        .unwrap_or(Value::Null);
    let ref_peak: Value = tick_peaks_dbfs
        .get(ri)
        .copied()
        .flatten()
        .map(Value::from)
        .unwrap_or(Value::Null);
    let mc_tag = mic::mic_correction_tag(ctx.meas_curve.is_some(), mc_enabled);

    let Some(Some(a)) = analysis.get(pos) else {
        // No estimate yet — the ring does not hold a whole Welch segment.
        // Publish anyway; see `settling_frame`.
        return Some((
            pos,
            vec![settling_frame(
                ctx, st, statics, meas_peak, ref_peak, mc_tag, mc_enabled, drive_msg,
            )],
            None,
        ));
    };

    let cal_tags = cal_tags_value(
        ctx.meas_cal.as_ref(),
        ctx.ref_cal.as_ref(),
        mc_tag,
        mc_enabled,
    );

    // Multi-time-window columns (additive; `null` until every rung holds a
    // full N blocks — 2.56 s at the bottom, the design's stated settling
    // time. Gating on the full N is what makes the reported N
    // unambiguous: every column is the mean of the same number of
    // blocks). Unlike the Welch arrays above, these are recomputed every
    // tick: the ladder is a push pipeline fed the fresh capture buffers,
    // so its columns really do move at the frame rate.
    //
    // Every column ships the Δf, window and N that produced it. That is
    // not decoration: neighbouring columns can come from windows 12x
    // apart, and coherence from uncorrelated inputs floats near 1/N, so
    // without those a screenshot of this display is not interpretable.
    // `bins` is criterion 1 made observable — it is never zero.
    //
    // dB is applied here, daemon-side, per the display-truth rule:
    // `ac-view` plots what it is given and does no `log10` of its own.
    let mtw_msg = match mtw_columns.get(pos).and_then(|c| c.as_ref()) {
        None => Value::Null,
        Some(cols) => json!({
            "freqs":        cols.iter().map(|c| c.freq).collect::<Vec<_>>(),
            "f_lo":         cols.iter().map(|c| c.lo).collect::<Vec<_>>(),
            "f_hi":         cols.iter().map(|c| c.hi).collect::<Vec<_>>(),
            "magnitude_db": cols.iter()
                .map(|c| 20.0 * c.h1.norm().max(1e-6).log10())
                .collect::<Vec<_>>(),
            "phase_deg":    cols.iter()
                .map(|c| c.h1.arg().to_degrees())
                .collect::<Vec<_>>(),
            "coherence":    cols.iter().map(|c| c.coherence).collect::<Vec<_>>(),
            "df":           cols.iter().map(|c| c.df).collect::<Vec<_>>(),
            "window_s":     cols.iter().map(|c| c.window_s).collect::<Vec<_>>(),
            "n":            cols.iter().map(|c| c.n).collect::<Vec<_>>(),
            "stage":        cols.iter().map(|c| c.stage).collect::<Vec<_>>(),
            "blend":        cols.iter().map(|c| c.blend).collect::<Vec<_>>(),
            "bins":         cols.iter().map(|c| c.bins).collect::<Vec<_>>(),
            "ppo":          mtw_ppo,
            "n_blocks":     mtw_n_blocks,
            // Which rungs have settled, shallowest first. Shipped so a
            // consumer can distinguish "still warming, more band coming"
            // from "this is all there is" — a short column list looks the
            // same either way, and the difference decides whether a blank
            // low end is a fault.
            "settled_stages": mtw_settled
                .get(pos)
                .cloned()
                .unwrap_or_default(),
            "stages":       &statics.mtw_stages,
        }),
    };

    let transfer_msg = json!({
        "type":            "transfer_stream",
        "cmd":             "transfer_stream",
        "mtw":             mtw_msg,
        "freqs":           a.freqs,
        "magnitude_db":    a.magnitude_db,
        "phase_deg":       a.phase_deg,
        "coherence":       a.coherence,
        "delay_samples":   a.delay_samples,
        "delay_ms":        a.delay_ms,
        "delay_locked":    st.delay.is_some(),
        "delay_attempts":  st.attempts,
        "delay_evidence":  st.prominence,
        "meas_peak_dbfs":  meas_peak,
        "ref_peak_dbfs":   ref_peak,
        "ref_channel":     ref_ch,
        "meas_channel":    meas_ch,
        "sr":              sr,
        // Welch blocks actually averaged into THIS frame (#208) — 1 while
        // the window fills, then `n_averages` for the rest of the session,
        // and 0 on a settling frame. Shipped because coherence carries a
        // `1/N` bias, so a coherence figure without N is not
        // interpretable: a consumer that saw N move silently could not
        // tell a settling display from a DUT that changed.
        "n_averages":      a.n_blocks,
        // Which estimate these arrays are. Increments when the analysis is
        // recomputed, which is once per Welch hop — slower than the frame
        // rate, so consecutive frames repeat the same arrays by design.
        // Without this the repetition is invisible and a stalled estimator
        // looks exactly like a stationary DUT.
        "analysis_seq":    a.seq,
        "mic_correction":  mc_tag,
        "spec_freqs":      a.spec_freqs,
        "meas_spectrum":   a.meas_spectrum,
        "ref_spectrum":    a.ref_spectrum,
        "spl":             Value::Null,
        "spl_weighting":   statics.weighting.tag(),
        "spl_integration": statics.integration_tag.as_str(),
        "cal_tags":        cal_tags,
        "drive":           drive_msg.clone(),
    });

    let mut out = vec![transfer_msg];
    if let Some(ir) = a.ir.as_ref() {
        out.push(json!({
            "type":          "visualize/ir",
            "cmd":           "transfer_stream",
            "samples":       ir.samples,
            "sr":            sr,
            "stride":        ir.stride,
            "dt_ms":         ir.dt_ms,
            "t_origin_ms":   ir.t_origin_ms,
            "ref_channel":   ref_ch,
            "meas_channel":  meas_ch,
            "delay_samples": a.delay_samples,
            "delay_ms":      a.delay_ms,
            "delay_locked":  st.delay.is_some(),
            "analysis_seq":  a.seq,
        }));
    }
    Some((pos, out, a.spl_raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::transfer::analysis::{analyse_pair, AnalysisKey};
    use crate::handlers::transfer::pair::Lock;

    // ---- build_pair_messages -----------------------------------------
    //
    // Same reason: the frame contract used to be reachable only through a
    // daemon, a socket and an audio backend.

    const TEST_SR: u32 = 8000;

    fn test_statics() -> FrameStatics {
        let spec_f_min = 20.0_f64;
        let spec_f_max = TEST_SR as f64 / 2.0;
        FrameStatics {
            sr: TEST_SR,
            spec_f_min,
            spec_f_max,
            spec_n_columns: ac_core::visualize::aggregate::transfer_spectrum_n_columns(
                spec_f_min, spec_f_max,
            ),
            weighting: ac_core::visualize::weighting_curves::WeightingCurve::Z,
            integration_tag: "fast".to_string(),
            mtw_ppo: ac_core::visualize::mtw::ladder::P_REF,
            mtw_n_blocks: ac_core::visualize::mtw::average::DEFAULT_N_BLOCKS,
            mtw_stages: Value::Null,
        }
    }

    fn test_ctx() -> PairCtx {
        PairCtx {
            pos: 0,
            meas_ch: 2,
            ref_ch: 5,
            mi: 0,
            ri: 1,
            meas_cal: None,
            ref_cal: None,
            meas_curve: None,
        }
    }

    /// One Welch segment of a correlated pair: ref is a tone, meas is the
    /// same tone at half amplitude. Enough for H1 to produce a frame.
    fn test_rings() -> Vec<Vec<f32>> {
        let n = TEST_SR as usize;
        let refb: Vec<f32> = (0..n)
            .map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 100.0 / TEST_SR as f32).sin())
            .collect();
        let meas: Vec<f32> = refb.iter().map(|s| s * 0.5).collect();
        vec![meas, refb]
    }

    fn call(
        ctx: &PairCtx,
        st: &PairState,
        statics: &FrameStatics,
        rings: &[Vec<f32>],
        peaks: &[Option<f64>],
    ) -> Option<(usize, Vec<Value>, Option<f64>)> {
        let drive_msg = json!({"on": false, "level_dbfs": Value::Null, "drivable": false});
        let cols: Vec<Option<Vec<ac_core::visualize::mtw::splice::Column>>> = vec![None];
        let settled: Vec<Vec<bool>> = vec![Vec::new()];
        // Assembly reads a held estimate, so the estimate is computed
        // here — the same call the session makes when a ring crosses a
        // block boundary.
        let key = AnalysisKey {
            dropped: 0,
            n_blocks: 4,
            delay: st.delay.map(|l| l.samples).unwrap_or(0),
            mc_enabled: false,
        };
        let analysis = vec![analyse_pair(ctx, st, statics, rings, key, 0)];
        let tick = TickInputs {
            tick_peaks_dbfs: peaks,
            mc_enabled: false,
            drive_msg: &drive_msg,
            mtw_columns: &cols,
            mtw_settled: &settled,
            analysis: &analysis,
            n_channels: rings.len(),
        };
        build_pair_messages(ctx, st, statics, &tick)
    }

    #[test]
    fn frame_carries_channels_and_position() {
        let rings = test_rings();
        let (pos, batch, _) = call(
            &test_ctx(),
            &PairState::new(None),
            &test_statics(),
            &rings,
            &[Some(-6.0), Some(-3.0)],
        )
        .expect("full rings must produce a frame");
        assert_eq!(pos, 0);
        let f = &batch[0];
        assert_eq!(f["type"], "transfer_stream");
        assert_eq!(f["meas_channel"], 2);
        assert_eq!(f["ref_channel"], 5);
        assert_eq!(f["sr"], TEST_SR);
        assert_eq!(f["meas_peak_dbfs"], -6.0);
        assert_eq!(f["ref_peak_dbfs"], -3.0);
    }

    // `delay_locked` is the field that keeps a refused pair distinguishable
    // from a genuine 0-sample digital loopback (#216, #227) — both report
    // `delay_ms` 0.0, so the number alone cannot carry the difference.
    #[test]
    fn unlocked_pair_reports_delay_locked_false() {
        let rings = test_rings();
        let (_, batch, _) = call(
            &test_ctx(),
            &PairState::new(None),
            &test_statics(),
            &rings,
            &[None, None],
        )
        .unwrap();
        assert_eq!(batch[0]["delay_locked"], false);
    }

    #[test]
    fn locked_pair_reports_delay_locked_true() {
        let rings = test_rings();
        let mut st = PairState::new(None);
        st.delay = Some(Lock {
            samples: 0,
            driving: true,
        });
        let (_, batch, _) = call(&test_ctx(), &st, &test_statics(), &rings, &[None, None]).unwrap();
        assert_eq!(batch[0]["delay_locked"], true);
    }

    // The count that separates "warming up" from "refusing" for the fault
    // indicator (#238). It is read straight off `PairState`, so a frame
    // must echo whatever the estimator has recorded.
    #[test]
    fn frame_echoes_attempt_count() {
        let rings = test_rings();
        let mut st = PairState::new(None);
        st.attempts = 7;
        let (_, batch, _) = call(&test_ctx(), &st, &test_statics(), &rings, &[None, None]).unwrap();
        assert_eq!(batch[0]["delay_attempts"], 7);
    }

    // Digital silence is `-inf` dBFS, which serde_json cannot serialise.
    // It has to reach the wire as null, not as a substituted number.
    #[test]
    fn silent_channel_peak_is_null_not_a_number() {
        let rings = test_rings();
        let (_, batch, _) = call(
            &test_ctx(),
            &PairState::new(None),
            &test_statics(),
            &rings,
            &[None, Some(-3.0)],
        )
        .unwrap();
        assert!(batch[0]["meas_peak_dbfs"].is_null());
    }

    // An uncalibrated pair must say so on both legs, and publish no SPL —
    // not a zero, which would read as a real 0 dB SPL measurement.
    #[test]
    fn uncalibrated_pair_tags_none_and_publishes_no_spl() {
        let rings = test_rings();
        let (_, batch, spl_raw) = call(
            &test_ctx(),
            &PairState::new(None),
            &test_statics(),
            &rings,
            &[None, None],
        )
        .unwrap();
        assert!(spl_raw.is_none());
        assert!(batch[0]["spl"].is_null());
        for leg in ["meas", "ref"] {
            let t = &batch[0]["cal_tags"][leg];
            assert_eq!(t["voltage"], "none", "{leg}");
            assert_eq!(t["spl"], "none", "{leg}");
            assert_eq!(t["mic_curve"], "none", "{leg}");
        }
    }

    // The ladder is additive: a pair with no columns yet publishes a frame
    // with `mtw` null, never a frame withheld or a partial ladder.
    #[test]
    fn absent_ladder_publishes_null_not_a_withheld_frame() {
        let rings = test_rings();
        let (_, batch, _) = call(
            &test_ctx(),
            &PairState::new(None),
            &test_statics(),
            &rings,
            &[None, None],
        )
        .unwrap();
        assert!(batch[0]["mtw"].is_null());
    }

    // Phase 4b sidecar rides along with the frame, from the same H1 result.
    #[test]
    fn ir_sidecar_accompanies_the_frame() {
        let rings = test_rings();
        let (_, batch, _) = call(
            &test_ctx(),
            &PairState::new(None),
            &test_statics(),
            &rings,
            &[None, None],
        )
        .unwrap();
        assert_eq!(batch.len(), 2, "frame + IR sidecar");
        assert_eq!(batch[1]["type"], "visualize/ir");
        assert_eq!(batch[1]["meas_channel"], 2);
        assert!(batch[1]["samples"]
            .as_array()
            .is_some_and(|a| !a.is_empty()));
    }

    // A pair whose channels are not in this tick's rings is dropped, not
    // published half-built.
    #[test]
    fn missing_channel_buffer_drops_the_pair() {
        let rings = test_rings();
        let mut ctx = test_ctx();
        ctx.ri = 9;
        assert!(call(
            &ctx,
            &PairState::new(None),
            &test_statics(),
            &rings,
            &[None, None]
        )
        .is_none());
    }
}
