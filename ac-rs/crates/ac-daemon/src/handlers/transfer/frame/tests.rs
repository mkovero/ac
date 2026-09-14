//! The frame contract, asserted against the builders directly.
//!
//! Every one of these used to need a daemon, a socket and an audio
//! backend to reach: the frame was assembled inside the worker closure,
//! so the only way to see what it carried was to subscribe to it.
//!
//! Assembly reads a held estimate, so each test computes one first —
//! the same [`analyse_pair`] call the session makes when a ring crosses
//! a block boundary.

use super::*;
use crate::handlers::transfer::analysis::{analyse_pair, AnalysisKey};
use crate::handlers::transfer::pair::Lock;

const TEST_SR: u32 = 8000;

fn test_statics() -> FrameStatics {
    let spec_f_min = 20.0_f64;
    let spec_f_max = TEST_SR as f64 / 2.0;
    FrameStatics {
        sr: TEST_SR,
        backend: "fake".to_string(),
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
