//! The H1 estimate and everything derived from it, plus the key that says
//! when it has to be recomputed.
//!
//! The Welch estimate reads whole segments from the ring's start, and the
//! drain moves that start only in whole `step` units (#208), so the estimate
//! is a function of far less than "the ring right now" — see [`AnalysisKey`].

use serde_json::{json, Value};

use ac_core::shared::calibration::Calibration;

use crate::handlers::mic;

use super::frame::FrameStatics;
use super::pair::{PairCtx, PairState};

/// Scale a linear amplitude spectrum by a channel's voltage
/// calibration, if it has one. No-op for an uncalibrated channel.
///
/// A constant per-channel factor, so it commutes with column
/// aggregation (`sqrt(Σ(c·x)²) = c·sqrt(Σx²)`) — which is what lets it
/// be applied to the full-resolution spectrum before aggregation rather
/// than to the aggregated columns after.
pub(super) fn apply_voltage_cal(amp: &mut [f64], cal: Option<&Calibration>) {
    if let Some(scale) = cal.and_then(|c| c.vrms_at_0dbfs_in) {
        for v in amp.iter_mut() {
            *v *= scale;
        }
    }
}

/// What an H1 estimate is a function of.
///
/// The Welch estimate reads whole segments from the ring's start, so it
/// cannot change while the ring start and the segment count both hold —
/// `drain_to_block_lattice` moves the start only in whole `step` units
/// (#208), and the samples appended past the last complete segment are
/// not read. Everything else the estimate depends on is here: the
/// alignment offset it was computed at, and the mic-correction toggle
/// that scales it.
///
/// Equal keys therefore mean an identical result, which is what makes the
/// cache a cache rather than a staleness policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AnalysisKey {
    /// Samples drained from the ring since session start — the ring
    /// start's absolute position in the stream, and so the absolute
    /// position of every segment boundary.
    pub(super) dropped: usize,
    /// Complete Welch segments in the ring.
    pub(super) n_blocks: usize,
    /// The alignment offset this estimate was computed at. A pair that
    /// locks mid-window must not keep publishing an unaligned estimate
    /// until the next boundary.
    pub(super) delay: i64,
    /// The global mic-correction toggle, which a client can flip between
    /// ticks.
    pub(super) mc_enabled: bool,
}

/// One pair's H1 estimate and everything derived from it, held between
/// block boundaries.
///
/// At 48 kHz the ring advances every 0.5 s while the loop ticks at 20 Hz,
/// so without this the same estimate is recomputed about ten times — a
/// 2.5 s Welch pass and a full-resolution IFFT per pair per tick, all of
/// it producing bytes identical to the previous tick's. #419 named the
/// waste and left it; this is the part of the loop that pays for it.
///
/// Arrays are stored already as `Value` because the only thing left to do
/// with them is serialize them.
pub(super) struct PairAnalysis {
    pub(super) key: AnalysisKey,
    /// Increments once per recomputation, published as `analysis_seq`.
    /// A consumer comparing it across frames can tell an estimate that is
    /// genuinely new from the same one shipped again, which is otherwise
    /// invisible: the arrays are byte-identical either way.
    pub(super) seq: u64,
    pub(super) n_blocks: usize,
    pub(super) delay_samples: i64,
    pub(super) delay_ms: f64,
    pub(super) freqs: Value,
    pub(super) magnitude_db: Value,
    pub(super) phase_deg: Value,
    pub(super) coherence: Value,
    pub(super) spec_freqs: Value,
    pub(super) meas_spectrum: Value,
    pub(super) ref_spectrum: Value,
    /// Broadband weighted level before time integration. The EMA that
    /// consumes it still steps every tick with that tick's `dt`, so
    /// holding the raw value here changes no `spl` number: it was
    /// recomputed identically on every tick before.
    pub(super) spl_raw: Option<f64>,
    /// Phase 4b sidecar payload, absent when the IFFT produced nothing.
    pub(super) ir: Option<IrPayload>,
}

/// The `visualize/ir` sidecar's per-analysis content. The channel and
/// lock fields are added at assembly, from live state.
pub(super) struct IrPayload {
    pub(super) samples: Value,
    pub(super) stride: usize,
    pub(super) dt_ms: f64,
    pub(super) t_origin_ms: f64,
}

/// Compute one pair's H1 estimate and everything derived from it.
///
/// `None` when the pair's channels are not present in the rings, which
/// drops the pair from the frame rather than publishing a partial one.
pub(super) fn analyse_pair(
    ctx: &PairCtx,
    st: &PairState,
    statics: &FrameStatics,
    rings: &[Vec<f32>],
    key: AnalysisKey,
    seq: u64,
) -> Option<PairAnalysis> {
    let &FrameStatics {
        sr,
        spec_f_min,
        spec_f_max,
        spec_n_columns,
        weighting,
        ..
    } = statics;
    let (curve_opt, meas_cal_opt, ref_cal_opt) = (&ctx.meas_curve, &ctx.meas_cal, &ctx.ref_cal);
    let mc_enabled = key.mc_enabled;
    let meas = rings.get(ctx.mi)?.as_slice();
    let refb = rings.get(ctx.ri)?.as_slice();

    // An unlocked pair (still warming up, or refused by the prominence
    // gate — #227) is measured unaligned rather than aligned to a guess.
    // `delay_locked` on the frame is what keeps that distinguishable: a
    // refused pair and a genuine 0-sample digital loopback both report
    // `delay_ms` 0.0, and #216 established that the loopback case is
    // legitimately 0.0, so the number alone cannot carry the difference.
    let result = ac_core::visualize::transfer::h1_estimate_with_delay(refb, meas, sr, key.delay);

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
    // Mic-curve correction (#101) on the measurement leg only — the
    // reference leg was guarded at launch. H1's dB magnitude has the mic
    // over-read embedded; subtract the curve at each downsampled bin to
    // recover truth.
    if mc_enabled {
        if let Some(curve) = curve_opt.as_ref() {
            mic::apply_mic_curve_inplace_f64(curve, &freqs, &mut mag);
        }
    }

    // Calibrated per-channel spectra (D18, handoff: transfer-frame-v2 M0).
    // Full-resolution `result.meas_amp`/`result.ref_amp` — NOT the 2000-pt
    // indices above, the same full-res-then-aggregate split the IR sidecar
    // uses — so the mic-curve's per-freq correction is applied at native
    // Welch-bin resolution, then `spectrum_to_columns_wire` band-power-
    // aggregates to the fixed grid: same aggregator, same tests, as the
    // monitor `spectrum` frame.
    let mut mc_meas_amp = result.meas_amp.clone();
    if mc_enabled {
        if let Some(curve) = curve_opt.as_ref() {
            for (amp, &f) in mc_meas_amp.iter_mut().zip(result.freqs.iter()) {
                *amp *= mic::mic_curve_scale(curve, f);
            }
        }
    }
    // `spl` is computed from the mic-corrected (acoustic truth) spectrum,
    // before voltage-cal scaling below — SPL derives from the dBFS +
    // spl_offset_db model (Calibration::spl_offset_db), independent of the
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

    // Voltage cal (D3), applied here — post mic-curve, pre-aggregation.
    // Both legs go through the same helper: the meas and ref spectra are
    // divided against each other downstream, so a scale applied to one leg
    // under rules that have drifted from the other's is an error that
    // cancels out of every check but the answer.
    let meas_amp_wire = {
        let mut a = mc_meas_amp;
        apply_voltage_cal(&mut a, meas_cal_opt.as_ref());
        a
    };
    let ref_amp_wire = {
        let mut a = result.ref_amp.clone();
        apply_voltage_cal(&mut a, ref_cal_opt.as_ref());
        a
    };
    let (meas_spectrum, spec_freqs) = ac_core::visualize::aggregate::spectrum_to_columns_wire(
        &meas_amp_wire,
        sr as f64,
        spec_f_min,
        spec_f_max,
        spec_n_columns,
    );
    let (ref_spectrum, _) = ac_core::visualize::aggregate::spectrum_to_columns_wire(
        &ref_amp_wire,
        sr as f64,
        spec_f_min,
        spec_f_max,
        spec_n_columns,
    );

    // Phase 4b: IR sidecar from the full-resolution complex H — the IFFT
    // needs the raw nperseg/2+1 bins to recover h(t) correctly, so it
    // reads `result.re`/`result.im` and not the 2000-point display arrays.
    // Time-domain output is 1 s long at sr (matches the 1 Hz Welch
    // resolution); downsampled to ≤2000 samples for wire economy by
    // stride-picking. Mic-curve correction is intentionally NOT applied to
    // the IR: the `curve_opt` branch above corrects only the downsampled
    // `mag`, and the full-resolution H here is uncorrected. For the
    // visualization-only Tier 2 IR view this is acceptable; a Tier-1
    // calibrated IR goes through the sweep measurement, not
    // `transfer_stream`.
    let ir_full = ac_core::visualize::transfer::impulse_response_from_h(&result.re, &result.im);
    let ir = if ir_full.is_empty() {
        None
    } else {
        const IR_MAX_SAMPLES: usize = 2000;
        let stride = (ir_full.len() / IR_MAX_SAMPLES).max(1);
        let ir_ds: Vec<f32> = ir_full.iter().step_by(stride).copied().collect();
        // t_origin_ms = -mid_ms because `impulse_response_from_h` centres
        // the IR peak at the middle of the array (t=0 in the user's
        // mental model).
        let dt_ms = 1000.0 / sr as f64 * stride as f64;
        let t_origin_ms = -((ir_ds.len() / 2) as f64) * dt_ms;
        Some(IrPayload {
            samples: json!(ir_ds),
            stride,
            dt_ms,
            t_origin_ms,
        })
    };

    let _ = st;
    Some(PairAnalysis {
        key,
        seq,
        n_blocks: key.n_blocks,
        delay_samples: result.delay_samples,
        delay_ms: result.delay_ms,
        freqs: json!(freqs),
        magnitude_db: json!(mag),
        phase_deg: json!(phase),
        coherence: json!(coh),
        spec_freqs: json!(spec_freqs),
        meas_spectrum: json!(meas_spectrum),
        ref_spectrum: json!(ref_spectrum),
        spl_raw,
        ir,
    })
}
