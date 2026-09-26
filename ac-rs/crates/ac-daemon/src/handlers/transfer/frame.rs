//! Wire assembly: the `transfer_stream` frame, the settling frame that
//! precedes it, and the `visualize/ir` sidecar.
//!
//! Nothing here computes an estimate. Everything expensive happened in
//! [`super::analysis`]; what is left is building the frame from a held
//! estimate plus this tick's live scalars, which is what lets a frame ship on
//! every capture tick while the estimate behind it advances at the ring's
//! rate.
//!
//! The frames are the shared `ac_core::wire` types the consumers deserialise
//! (#112), so a key this module stops setting is a compile error here rather
//! than a missing readout in `ac-view`.

use serde_json::{json, Value};

use ac_core::shared::calibration::{Calibration, LayerVerdict};
use ac_core::wire::{IrFrame, MtwColumns, MtwStage, TransferFrame, WireDrive};

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
    pub(super) backend: String,
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
    pub(super) mtw_stages: Vec<MtwStage>,
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
    pub(super) drive_msg: &'a WireDrive,
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
///
/// #466: `voltage` means the scale is **applied** (the calibrations here are
/// already gated), and each leg's `voltage_check` carries the session
/// check's verdict whenever that leg's stored calibration had a scale.
pub(super) fn cal_tags_value(
    meas_cal: Option<&Calibration>,
    ref_cal: Option<&Calibration>,
    meas_check: Option<&LayerVerdict>,
    ref_check: Option<&LayerVerdict>,
    meas_mic_tag: &str,
    mc_enabled: bool,
) -> Value {
    let ref_curve_loaded = ref_cal.is_some_and(|c| c.mic_response.is_some());
    let mut tags = json!({
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
    });
    for (leg, check) in [("meas", meas_check), ("ref", ref_check)] {
        if let Some(check) = check {
            tags[leg]["voltage_check"] = json!(check);
        }
    }
    tags
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
    meas_peak: Option<f64>,
    ref_peak: Option<f64>,
    mc_tag: &str,
    mc_enabled: bool,
    drive_msg: &WireDrive,
) -> TransferFrame {
    TransferFrame {
        frame_type: "transfer_stream".to_string(),
        cmd: "transfer_stream".to_string(),
        wire_version: None,
        mtw: None,
        // Set by the session after assembly (#670).
        protection: None,
        freqs: Vec::new(),
        magnitude_db: Vec::new(),
        phase_deg: Vec::new(),
        coherence: Vec::new(),
        delay_samples: 0,
        delay_ms: 0.0,
        delay_locked: Some(false),
        delay_attempts: st.attempts,
        delay_residual: None,
        delay_operator: st.delay.is_some_and(|l| l.operator),
        meas_peak_dbfs: meas_peak,
        ref_peak_dbfs: ref_peak,
        ref_channel: ctx.ref_ch.into(),
        meas_channel: ctx.meas_ch.into(),
        sr: statics.sr,
        // Zero blocks: this frame carries no Welch estimate at all, which
        // is a different statement from the `1` a first-segment frame
        // makes. A consumer reading coherence's `1/N` bias needs the
        // difference, and so does anyone deciding whether an empty
        // magnitude array is a fault or a start.
        n_averages: 0,
        // No estimate exists to number. `null` rather than 0, so the
        // first real estimate's `0` cannot be mistaken for a repeat of
        // something that was never sent.
        analysis_seq: None,
        mic_correction: mc_tag.to_string(),
        spec_freqs: Vec::new(),
        meas_spectrum: Vec::new(),
        ref_spectrum: Vec::new(),
        spl: None,
        spl_weighting: statics.weighting.tag().to_string(),
        spl_integration: statics.integration_tag.clone(),
        cal_tags: Some(cal_tags_value(
            ctx.meas_cal.as_ref(),
            ctx.ref_cal.as_ref(),
            ctx.meas_voltage_check.as_ref(),
            ctx.ref_voltage_check.as_ref(),
            mc_tag,
            mc_enabled,
        )),
        drive: Some(drive_msg.clone()),
        backend: statics.backend.clone(),
    }
}

/// Build one pair's wire messages for this tick: the `transfer_stream`
/// frame, plus a Phase 4b `visualize/ir` sidecar when there is an
/// estimate to derive one from. The frame's `spl` is left `None`; the
/// caller fills it once integrated. Returns the pair's launch position
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
) -> Option<(usize, TransferFrame, Option<IrFrame>, Option<f64>)> {
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
    // `-inf` (digital silence) travels as JSON null: `raw_peak_dbfs`
    // already maps it to `None`, so no non-finite float reaches the
    // serialiser.
    let meas_peak: Option<f64> = tick_peaks_dbfs.get(mi).copied().flatten();
    let ref_peak: Option<f64> = tick_peaks_dbfs.get(ri).copied().flatten();
    let mc_tag = mic::mic_correction_tag(ctx.meas_curve.is_some(), mc_enabled);

    let Some(Some(a)) = analysis.get(pos) else {
        // No estimate yet — the ring does not hold a whole Welch segment.
        // Publish anyway; see `settling_frame`.
        return Some((
            pos,
            settling_frame(
                ctx, st, statics, meas_peak, ref_peak, mc_tag, mc_enabled, drive_msg,
            ),
            None,
            None,
        ));
    };

    let cal_tags = cal_tags_value(
        ctx.meas_cal.as_ref(),
        ctx.ref_cal.as_ref(),
        ctx.meas_voltage_check.as_ref(),
        ctx.ref_voltage_check.as_ref(),
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
    // dB is applied daemon-side, per the display-truth rule: `ac-view`
    // plots what it is given and does no `log10` of its own. The conversion
    // is `ac-core`'s, shared with snapshot replay (#221), so a replayed
    // snapshot and this frame cannot convert the same columns differently.
    let mtw: Option<MtwColumns> = mtw_columns.get(pos).and_then(|c| c.as_ref()).map(|cols| {
        ac_core::visualize::mtw::wire_columns(
            cols,
            mtw_ppo,
            mtw_n_blocks,
            // Which rungs have settled, shallowest first. Shipped so a
            // consumer can distinguish "still warming, more band coming"
            // from "this is all there is" — a short column list looks
            // the same either way, and the difference decides whether a
            // blank low end is a fault.
            mtw_settled.get(pos).cloned().unwrap_or_default(),
            statics.mtw_stages.clone(),
        )
    });

    let transfer = TransferFrame {
        frame_type: "transfer_stream".to_string(),
        cmd: "transfer_stream".to_string(),
        wire_version: None,
        mtw,
        protection: None,
        freqs: a.freqs.clone(),
        magnitude_db: a.magnitude_db.clone(),
        phase_deg: a.phase_deg.clone(),
        coherence: a.coherence.clone(),
        delay_samples: a.delay_samples,
        delay_ms: a.delay_ms,
        delay_locked: Some(st.delay.is_some()),
        delay_attempts: st.attempts,
        delay_residual: a.ir_peak_lag,
        delay_operator: st.delay.is_some_and(|l| l.operator),
        meas_peak_dbfs: meas_peak,
        ref_peak_dbfs: ref_peak,
        ref_channel: ref_ch.into(),
        meas_channel: meas_ch.into(),
        sr,
        // Welch blocks actually averaged into THIS frame (#208) — 1 while
        // the window fills, then `n_averages` for the rest of the session,
        // and 0 on a settling frame. Shipped because coherence carries a
        // `1/N` bias, so a coherence figure without N is not
        // interpretable: a consumer that saw N move silently could not
        // tell a settling display from a DUT that changed.
        n_averages: a.n_blocks,
        // Which estimate these arrays are. Increments when the analysis is
        // recomputed, which is once per Welch hop — slower than the frame
        // rate, so consecutive frames repeat the same arrays by design.
        // Without this the repetition is invisible and a stalled estimator
        // looks exactly like a stationary DUT.
        analysis_seq: Some(a.seq),
        mic_correction: mc_tag.to_string(),
        spec_freqs: a.spec_freqs.clone(),
        meas_spectrum: a.meas_spectrum.clone(),
        ref_spectrum: a.ref_spectrum.clone(),
        spl: None,
        spl_weighting: statics.weighting.tag().to_string(),
        spl_integration: statics.integration_tag.clone(),
        cal_tags: Some(cal_tags),
        drive: Some(drive_msg.clone()),
        backend: statics.backend.clone(),
    };

    let ir = a.ir.as_ref().map(|ir| IrFrame {
        frame_type: "visualize/ir".to_string(),
        cmd: "transfer_stream".to_string(),
        wire_version: None,
        samples: ir.samples.clone(),
        sr,
        stride: ir.stride,
        dt_ms: ir.dt_ms,
        t_origin_ms: ir.t_origin_ms,
        ref_channel: ref_ch.into(),
        meas_channel: meas_ch.into(),
        delay_samples: a.delay_samples,
        delay_ms: a.delay_ms,
        delay_locked: Some(st.delay.is_some()),
        analysis_seq: a.seq,
        backend: statics.backend.clone(),
    });
    Some((pos, transfer, ir, a.spl_raw))
}

#[cfg(test)]
mod tests;
