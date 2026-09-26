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
        mtw_stages: Vec::new(),
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
        meas_voltage_check: None,
        ref_voltage_check: None,
        meas_curve: None,
    }
}

/// #466 (R4-4): `voltage` names what is applied, and `voltage_check` rides
/// every leg whose stored calibration had a scale.
#[test]
fn cal_tags_carry_the_voltage_verdict_under_the_presence_rule() {
    use ac_core::shared::calibration::session::{
        CheckSource, Evidence, UnverifiedCause, VerdictUnit,
    };
    use ac_core::shared::calibration::LayerVerdict;
    let mut stored = Calibration::new(0, 2);
    stored.vrms_at_0dbfs_in = Some(1.5);
    let refused = LayerVerdict::Refused {
        evidence: Evidence {
            measured: 2.42,
            stored: -0.6,
            delta: 3.02,
            tolerance: 0.1,
            unit: VerdictUnit::Db,
            stored_at: "2026-09-15T23:43:04Z".into(),
            checked_at: "2026-09-16T14:02:11.000Z".into(),
            source: CheckSource::Probe,
        },
        via: None,
        delta_bound: None,
    };
    let unverified = LayerVerdict::unverified(
        UnverifiedCause::NoLoopback,
        "no reference loopback configured",
    );

    // (i) refused: the scale is withheld and the verdict says why.
    let gated = stored.without_voltage();
    let tags = cal_tags_value(Some(&gated), None, Some(&refused), None, "none", false);
    assert_eq!(tags["meas"]["voltage"], "none");
    assert_eq!(tags["meas"]["voltage_check"]["state"], "refused");

    // (ii) no stored scale: no verdict key at all.
    let tags = cal_tags_value(None, None, None, None, "none", false);
    assert!(tags["meas"].get("voltage_check").is_none(), "{tags}");
    assert!(tags["ref"].get("voltage_check").is_none(), "{tags}");

    // (iii) applied but unverified: `on`, carrying the verdict.
    let tags = cal_tags_value(Some(&stored), None, Some(&unverified), None, "none", false);
    assert_eq!(tags["meas"]["voltage"], "on");
    assert_eq!(tags["meas"]["voltage_check"]["cause"], "no_loopback");
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

/// The published form of one pair's tick: the frame, then the sidecar when
/// there is one — the order `Session::tick` publishes them in.
fn published(
    built: Option<(usize, TransferFrame, Option<IrFrame>, Option<f64>)>,
) -> Option<(usize, Vec<Value>, Option<f64>)> {
    built.map(|(pos, frame, ir, spl_raw)| {
        let mut batch = vec![serde_json::to_value(&frame).expect("frame serialises")];
        batch.extend(ir.map(|ir| serde_json::to_value(&ir).expect("sidecar serialises")));
        (pos, batch, spl_raw)
    })
}

fn call(
    ctx: &PairCtx,
    st: &PairState,
    statics: &FrameStatics,
    rings: &[Vec<f32>],
    peaks: &[Option<f64>],
) -> Option<(usize, Vec<Value>, Option<f64>)> {
    let drive_msg = WireDrive::default();
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
    published(build_pair_messages(ctx, st, statics, &tick))
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
        operator: false,
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

// ---- key-set characterisation (#112 D4.1) ----
//
// The exact key set, nested keys included, of each frame these builders
// emit. Recorded before the builders moved onto `ac_core::wire` types so
// that move is provably key-preserving: a key the shared type forgets
// disappears from the wire, and nothing else in this file would notice.

/// Every key path in `v`: `a`, `a.b`, and `a[].b` for objects inside
/// arrays. Array elements that are not objects contribute nothing, so a
/// numeric array is one path however long it is.
fn key_paths(v: &Value) -> std::collections::BTreeSet<String> {
    fn walk(v: &Value, prefix: &str, out: &mut std::collections::BTreeSet<String>) {
        match v {
            Value::Object(m) => {
                for (k, child) in m {
                    let path = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    out.insert(path.clone());
                    walk(child, &path, out);
                }
            }
            Value::Array(a) => {
                for child in a {
                    walk(child, &format!("{prefix}[]"), out);
                }
            }
            _ => {}
        }
    }
    let mut out = std::collections::BTreeSet::new();
    walk(v, "", &mut out);
    out
}

fn paths(list: &[&str]) -> std::collections::BTreeSet<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// Keys every `transfer_stream` frame carries, settling or analysing, with
/// `mtw` null and no voltage verdict in `cal_tags`.
const TRANSFER_KEYS: &[&str] = &[
    "analysis_seq",
    "backend",
    "cal_tags",
    "cal_tags.meas",
    "cal_tags.meas.mic_curve",
    "cal_tags.meas.spl",
    "cal_tags.meas.voltage",
    "cal_tags.ref",
    "cal_tags.ref.mic_curve",
    "cal_tags.ref.spl",
    "cal_tags.ref.voltage",
    "cmd",
    "coherence",
    "delay_attempts",
    "delay_locked",
    "delay_ms",
    "delay_operator",
    "delay_residual",
    "delay_samples",
    "drive",
    "drive.drivable",
    "drive.level_dbfs",
    "drive.on",
    "freqs",
    "magnitude_db",
    "meas_channel",
    "meas_peak_dbfs",
    "meas_spectrum",
    "mic_correction",
    "mtw",
    "n_averages",
    "phase_deg",
    "ref_channel",
    "ref_peak_dbfs",
    "ref_spectrum",
    "spec_freqs",
    "spl",
    "spl_integration",
    "spl_weighting",
    "sr",
    "type",
];

/// The nested keys a present `mtw` adds.
const MTW_KEYS: &[&str] = &[
    "mtw.bins",
    "mtw.blend",
    "mtw.coherence",
    "mtw.df",
    "mtw.f_hi",
    "mtw.f_lo",
    "mtw.freqs",
    "mtw.magnitude_db",
    "mtw.n",
    "mtw.n_blocks",
    "mtw.phase_deg",
    "mtw.ppo",
    "mtw.settled_stages",
    "mtw.stage",
    "mtw.stages",
    "mtw.stages[].blend_top",
    "mtw.stages[].decim",
    "mtw.stages[].df",
    "mtw.stages[].f_top",
    "mtw.stages[].f_valid",
    "mtw.stages[].hop_s",
    "mtw.stages[].rate",
    "mtw.stages[].settling_s",
    "mtw.stages[].window_s",
    "mtw.window_s",
];

const IR_KEYS: &[&str] = &[
    "analysis_seq",
    "backend",
    "cmd",
    "delay_locked",
    "delay_ms",
    "delay_samples",
    "dt_ms",
    "meas_channel",
    "ref_channel",
    "samples",
    "sr",
    "stride",
    "t_origin_ms",
    "type",
];

fn test_column() -> ac_core::visualize::mtw::splice::Column {
    ac_core::visualize::mtw::splice::Column {
        freq: 100.0,
        lo: 90.0,
        hi: 110.0,
        h1: Default::default(),
        coherence: 0.5,
        df: 1.0,
        window_s: 1.0,
        n: 4,
        stage: 0,
        blend: 0.0,
        bins: 3,
    }
}

/// Build one tick with a ladder present, from the same held estimate
/// [`call`] computes.
fn call_with_mtw(statics: &FrameStatics) -> (usize, Vec<Value>, Option<f64>) {
    published(Some(build_with_mtw(statics).0)).expect("frame")
}

type Built = (usize, TransferFrame, Option<IrFrame>, Option<f64>);

/// [`call_with_mtw`]'s typed build, plus the held estimate it read.
fn build_with_mtw(statics: &FrameStatics) -> (Built, PairAnalysis) {
    let rings = test_rings();
    let ctx = test_ctx();
    let st = PairState::new(None);
    let drive_msg = WireDrive {
        on: true,
        level_dbfs: Some(-30.0),
        drivable: true,
    };
    let cols = vec![Some(vec![test_column(), test_column()])];
    let settled = vec![vec![true, false]];
    let key = AnalysisKey {
        dropped: 0,
        n_blocks: 4,
        delay: 0,
        mc_enabled: false,
    };
    let mut analysis = vec![analyse_pair(&ctx, &st, statics, &rings, key, 0)];
    let tick = TickInputs {
        tick_peaks_dbfs: &[Some(-6.0), Some(-3.0)],
        mc_enabled: false,
        drive_msg: &drive_msg,
        mtw_columns: &cols,
        mtw_settled: &settled,
        analysis: &analysis,
        n_channels: rings.len(),
    };
    let built = build_pair_messages(&ctx, &st, statics, &tick).expect("frame");
    (built, analysis.remove(0).expect("estimate"))
}

/// Statics with the ladder description `plan.rs` builds for `sr`.
fn statics_with_stages() -> FrameStatics {
    let mut statics = test_statics();
    let n_blocks = statics.mtw_n_blocks;
    statics.mtw_stages = ac_core::visualize::mtw::ladder::layout(TEST_SR)
        .expect("ladder layout")
        .stages
        .iter()
        .map(|s| MtwStage {
            settling_s: ac_core::visualize::mtw::settling_seconds(s, n_blocks),
            decim: s.decim,
            rate: s.rate,
            df: s.df,
            window_s: s.window_s,
            hop_s: s.hop_s,
            f_valid: s.f_valid,
            f_top: s.f_top,
            blend_top: s.blend_top,
        })
        .collect();
    statics
}

#[test]
fn analysis_frame_key_set_is_characterised() {
    let rings = test_rings();
    let (_, batch, _) = call(
        &test_ctx(),
        &PairState::new(None),
        &test_statics(),
        &rings,
        &[None, None],
    )
    .unwrap();
    assert_eq!(key_paths(&batch[0]), paths(TRANSFER_KEYS));
}

#[test]
fn analysis_frame_with_ladder_key_set_is_characterised() {
    let (_, batch, _) = call_with_mtw(&statics_with_stages());
    let mut want = paths(TRANSFER_KEYS);
    want.extend(paths(MTW_KEYS));
    assert_eq!(key_paths(&batch[0]), want);
}

#[test]
fn settling_frame_key_set_is_characterised() {
    let rings = test_rings();
    let statics = test_statics();
    let drive_msg = WireDrive::default();
    let cols: Vec<Option<Vec<ac_core::visualize::mtw::splice::Column>>> = vec![None];
    let settled: Vec<Vec<bool>> = vec![Vec::new()];
    let analysis: Vec<Option<PairAnalysis>> = vec![None];
    let tick = TickInputs {
        tick_peaks_dbfs: &[None, None],
        mc_enabled: false,
        drive_msg: &drive_msg,
        mtw_columns: &cols,
        mtw_settled: &settled,
        analysis: &analysis,
        n_channels: rings.len(),
    };
    let (_, batch, _) = published(build_pair_messages(
        &test_ctx(),
        &PairState::new(None),
        &statics,
        &tick,
    ))
    .unwrap();
    assert_eq!(batch.len(), 1, "a settling tick publishes no IR sidecar");
    assert_eq!(key_paths(&batch[0]), paths(TRANSFER_KEYS));
}

#[test]
fn ir_sidecar_key_set_is_characterised() {
    let (_, batch, _) = call_with_mtw(&statics_with_stages());
    assert_eq!(key_paths(&batch[1]), paths(IR_KEYS));
}

// ---- round trip through the shared type and the consumers (#112) ----

/// `to_value(from_value::<T>(v)) == v` for a builder output `v` (D4.2): the
/// shared type reads back everything it wrote, so skip-versus-null and
/// integer-versus-float survive the trip.
fn assert_lossless<T: serde::Serialize + serde::de::DeserializeOwned>(v: &Value) {
    let typed: T = serde_json::from_value(v.clone()).expect("parses as the shared type");
    assert_eq!(&serde_json::to_value(&typed).unwrap(), v);
}

#[test]
fn builder_outputs_round_trip_losslessly() {
    let (_, batch, _) = call_with_mtw(&statics_with_stages());
    assert_lossless::<TransferFrame>(&batch[0]);
    assert_lossless::<IrFrame>(&batch[1]);

    let rings = test_rings();
    let drive_msg = WireDrive::default();
    let cols: Vec<Option<Vec<ac_core::visualize::mtw::splice::Column>>> = vec![None];
    let settled: Vec<Vec<bool>> = vec![Vec::new()];
    let analysis: Vec<Option<PairAnalysis>> = vec![None];
    let tick = TickInputs {
        tick_peaks_dbfs: &[None, None],
        mc_enabled: false,
        drive_msg: &drive_msg,
        mtw_columns: &cols,
        mtw_settled: &settled,
        analysis: &analysis,
        n_channels: rings.len(),
    };
    let (_, settling, _) = published(build_pair_messages(
        &test_ctx(),
        &PairState::new(None),
        &test_statics(),
        &tick,
    ))
    .unwrap();
    assert_lossless::<TransferFrame>(&settling[0]);
}

/// D4.3: the published frame, read through `ac-scene`'s own adapters, gives
/// back what the builder was handed.
#[test]
fn consumers_read_back_what_the_builder_was_given() {
    let statics = statics_with_stages();
    let (built, estimate) = build_with_mtw(&statics);
    let (_, batch, _) = published(Some(built)).unwrap();
    let frame: TransferFrame = serde_json::from_value(batch[0].clone()).unwrap();
    let ir: IrFrame = serde_json::from_value(batch[1].clone()).unwrap();

    let input = ac_scene::TransferInput::from_wire_frame(&frame);
    assert_eq!((input.meas_channel, input.ref_channel), (2, 5));
    assert_eq!(input.sr, TEST_SR);
    assert_eq!(input.meas_peak_dbfs, Some(-6.0));
    assert_eq!(input.ref_peak_dbfs, Some(-3.0));
    assert_eq!(input.delay_ms, estimate.delay_ms);
    assert_eq!(input.delay_locked, Some(false));
    let col = test_column();
    assert_eq!(input.freqs, vec![col.freq; 2]);
    assert_eq!(input.coherence, vec![col.coherence; 2]);
    assert_eq!(input.column_df, vec![col.df; 2]);
    assert_eq!(input.column_window_s, vec![col.window_s; 2]);
    assert_eq!(input.column_n, vec![col.n as f64; 2]);
    assert_eq!(input.column_bins, vec![col.bins; 2]);
    assert_eq!(input.stages, statics.mtw_stages);
    let fault = input.fault.as_ref().expect("drive state present");
    assert!(fault.drive.on && fault.drive.drivable);
    assert_eq!(fault.delay_locked, Some(false));

    let fault_input = ac_scene::FaultInput::from_wire_frame(&frame);
    assert_eq!(fault_input.coherence, [col.coherence; 2].as_slice());
    assert_eq!(fault_input.meas_peak_dbfs, Some(-6.0));

    let payload = estimate.ir.as_ref().expect("IR payload");
    let ir_input = ac_scene::IrInput::from_wire_frame(&ir);
    assert_eq!(ir_input.samples, payload.samples);
    assert_eq!(ir_input.dt_ms, payload.dt_ms);
    assert_eq!(ir_input.t_origin_ms, payload.t_origin_ms);
    assert_eq!(ir_input.delay_ms, estimate.delay_ms);
    assert_eq!(ir_input.delay_locked, Some(false));
    assert_eq!(ir_input.sr, TEST_SR);
    assert_eq!(ir_input.channel_role, "meas_2");
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
