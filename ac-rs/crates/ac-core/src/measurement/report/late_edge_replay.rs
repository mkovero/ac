//! #555 (#539 architect revision 4): a real 1.00 m capture, delayed by N
//! samples, must produce a flight time of F₀ + N at every N and must never
//! be withheld on the late side. #552 deleted the late edge; this pins that
//! against hardware-recorded audio rather than a synthetic spike.
//!
//! A constant processing delay — a DSP monitor's latency, a PA alignment
//! delay — does to an impulse response exactly what this shift does, so the
//! replay is faithful for that part of the device class. It does not model
//! frequency-dependent excess (a passive crossover) or a pulse-shape change
//! (a linear-phase FIR speaker's pre-ringing); only a real second speaker
//! measures those.
//!
//! Source: pupu, 2026-09-21, `d1p0-step2b-01-report.json` from the #541
//! corpus (`$AC_HOME/corpus/d1p0-step2b/`), sha256
//! `54652926706d3b70fc9da2bdf42764ec9baf24acbb453d002e974ba8b52e86dc`.
//! Its `data[0].data.linear_ir` (38400 samples, broadband argmax at 21262)
//! is [`LINEAR_IR_F32LE`], sha256
//! `962ddfd17f689d6db0e13a5d99e11c88bda00d70a8d4b321126a643775661aba`.
//! The corpus `MANIFEST.sha256` covers only the WAVs.
//!
//! The delay prepends `ir[0..N]` reversed in time (`ir[N−1], …, ir[0]`) —
//! the capture's own pre-impulse noise — and drops the last N samples.
//! Zero-fill would inflate the pre-impulse SNR; rotation would put tail
//! content before the arrival. For every N here `ir[0..N]` is pre-arrival
//! noise (N ≤ 9600, arrival at 21262), and at N = 9600 the arrival still
//! sits ≈ 7500 samples inside the gate.
//!
//! A forward copy (`ir[0..N]` then `ir[0..]`) was rejected. Its seam
//! `ir[N−1] | ir[0]` is a value step: this capture's pre-arrival region
//! carries a large low-frequency wander, and at N = 9600 the step is
//! −38.1 dB re the broadband peak, ≈ 20 dB above the largest adjacent-sample
//! step that occurs naturally before the arrival (−58.5 dB). After the
//! high-pass it read as `EarlierComparable` at −19.96 dB, 0.04 dB inside
//! `ARRIVAL_EARLIER_COMPARABLE_DB` — an artefact of the test, not a finding
//! about `ir_stats`. The mirrored junction puts `ir[0]` next to `ir[0]`, so
//! it is continuous in value and only the slope flips; that slope jump,
//! 2·|`ir[1]` − `ir[0]`|, is ≈ −82.5 dB re peak. The prepended block has the
//! N = 0 pre-region's magnitude spectrum and set of sample values, and the
//! high-pass is zero-phase (filtfilt), so away from the junction its output
//! there is the N = 0 output reversed: the pick sees no level that does not
//! already occur before the arrival at N = 0. The block reuses one noise
//! realisation rather than presenting new noise; what is under test is the
//! pick and the check under a constant delay, not noise that drifts.
//!
//! Which way a construction artefact points: it can only add pre-arrival
//! content, which moves the result towards `EarlierComparable` — a
//! withholding, a red test. It cannot turn a real late-side withholding
//! green, because the assertions below require exact equivariance.
//!
//! What makes this test fail:
//! - a pick that is not shift-equivariant on real data (arrival index,
//!   cross-check, lobe offset or lobe margin moving with N);
//! - any late-side withholding: a distance check other than `Consistent`,
//!   or no flight time, at any N;
//! - a produced flight time or excess other than the N = 0 value plus N
//!   samples.
//!
//! Re-introducing the deleted late edge (`TooEarly` when `excess_s >
//! −window.low_s + 1.0e-3`, the pre-#552 `ε + A` at 1 m) fails the excess
//! and flight-time assertions from N = 61.

use rayon::prelude::*;

use super::fixtures::*;
use super::*;

const SR: u32 = 96_000;
/// The capture's gate: 0.4 s at 96 kHz.
const LEN: usize = 38_400;
/// pupu's reference-pair τ on 2026-09-21: 1727 samples at 96 kHz.
const TAU_S: f64 = 1727.0 / 96_000.0;
/// The rig record's values at N = 0 (#539, 2026-09-21), in samples.
const ANCHOR_FLIGHT: i64 = 335;
const ANCHOR_EXCESS: i64 = 55;
/// Tolerance on `arrival_lobe_margin_db` re N = 0: the zero-phase
/// high-pass reaches the shifted arrival from a different filter state, so
/// its last bits differ (#555 architect note 2).
const LOBE_MARGIN_TOL_DB: f64 = 1e-6;

/// `d1p0-step2b-01-report.json`, `data[0].data.linear_ir`, as
/// little-endian f32. Hashes in the module doc.
static LINEAR_IR_F32LE: &[u8] = include_bytes!("testdata/pupu-d1p0-step2b-01-linear-ir.f32le");

/// Every N in `0..=192` (≈ 3× the pre-#552 boundary at N = 61), plus 50 ms
/// and 100 ms at 96 kHz, stand-ins for the "tens to hundreds of ms" of PA
/// alignment delay the operator named on #552 (assumed, not measured).
fn shifts() -> Vec<usize> {
    (0..=192).chain([4_800, 9_600]).collect()
}

fn linear_ir() -> Vec<f64> {
    let ir: Vec<f64> = LINEAR_IR_F32LE
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64)
        .collect();
    assert_eq!(ir.len(), LEN, "testdata length");
    ir
}

/// `ir` delayed by `n`: its own first `n` samples prepended in reverse
/// order (`ir[n−1], …, ir[0]`), its last `n` dropped. The junction puts
/// `ir[0]` next to `ir[0]`; see the module doc for why not a forward copy.
fn delayed(ir: &[f64], n: usize) -> Vec<f64> {
    ir[..n]
        .iter()
        .rev()
        .chain(&ir[..LEN - n])
        .copied()
        .collect()
}

/// The capture's report, with `linear_ir` in place of its IR.
fn report(linear_ir: Vec<f64>) -> MeasurementReport {
    let mut r = ir_report_with_custom_ir_band(linear_ir, SR, 20_000.0);
    r.method = MeasurementMethod::SweptSine {
        f1_hz: 200.0,
        f2_hz: 20_000.0,
        duration_s: 12.0,
    };
    r.stimulus.sample_rate_hz = SR;
    if let MeasurementData::ImpulseResponse {
        f1_hz, duration_s, ..
    } = &mut r.data[0].data
    {
        *f1_hz = 200.0;
        *duration_s = 12.0;
    }
    r.data[0].gate = Some(GateParams {
        gate_start_s: -0.2,
        gate_length_s: 0.4,
        window_kind: "rectangular".into(),
        f_low_hz: 2.5,
    });
    r.position = Some(PositionSnapshot {
        distance_m: Some(1.0),
        ..Default::default()
    });
    // The capture is schema v11 and carries no inter-pair offset: replayed
    // as recorded, `latency_basis` is `PredatesV12`, `distance_check` is
    // `NoLatency`, and nothing below would be tested. `with_live_latency`
    // sets `InterPairOffset::Identity` — the basis the operator used on
    // 2026-09-21 (the 2026-09-18 cable patch measured both analog pairs
    // identically, see #544) — with the capture's measured reference τ.
    with_live_latency(&mut r, TAU_S);
    r
}

/// `(x(n) − x(0)) · fs`, rounded to whole samples.
fn samples_between(x_n: f64, x_0: f64) -> i64 {
    ((x_n - x_0) * SR as f64).round() as i64
}

fn excess_s(s: &IrStats) -> Option<f64> {
    match s.distance_check {
        DistanceCheck::Consistent { excess_s, .. } => Some(excess_s),
        _ => None,
    }
}

#[test]
fn delayed_real_capture_is_never_withheld_late() {
    let ir = linear_ir();
    let runs: Vec<(usize, IrStats)> = shifts()
        .into_par_iter()
        .map(|n| {
            let stats = report(delayed(&ir, n))
                .ir_stats()
                .expect("an impulse response");
            (n, stats)
        })
        .collect();
    let fs = SR as f64;
    let (_, s0) = &runs[0];

    // 1. Anchor: the replay reproduces the rig record.
    let flight0 = s0.flight_time_s.unwrap_or_else(|| {
        panic!(
            "N=0: flight time withheld; cross_check={:?} distance_check={:?}",
            s0.arrival_cross_check, s0.distance_check
        )
    });
    let excess0 = excess_s(s0).unwrap_or_else(|| {
        panic!(
            "N=0: distance check {:?}, want Consistent",
            s0.distance_check
        )
    });
    assert_eq!(
        (flight0 * fs).round() as i64,
        ANCHOR_FLIGHT,
        "N=0 flight time"
    );
    assert_eq!((excess0 * fs).round() as i64, ANCHOR_EXCESS, "N=0 excess");

    for (n, s) in &runs {
        let n = *n;
        println!(
            "N={n} arrival={} {:?} margin={:?} lobe_offset={:?} band_snr={:?} \
             pre_snr={:.2} distance_check={:?} flight={:?}",
            s.arrival_index,
            s.arrival_cross_check,
            s.arrival_lobe_margin_db,
            s.arrival_lobe_offset,
            s.band_limited_snr_db,
            s.pre_impulse_snr_db,
            s.distance_check,
            s.flight_time_s.map(|f| f * fs),
        );

        // 2. The pick and the layers above it do not change with delay.
        assert_eq!(
            s.arrival_index,
            s0.arrival_index + n,
            "N={n}: arrival index"
        );
        assert_eq!(
            s.arrival_cross_check, s0.arrival_cross_check,
            "N={n}: cross-check"
        );
        assert_eq!(
            s.arrival_lobe_offset, s0.arrival_lobe_offset,
            "N={n}: lobe offset"
        );
        match (s.arrival_lobe_margin_db, s0.arrival_lobe_margin_db) {
            (Some(m), Some(m0)) if m.is_finite() || m0.is_finite() => assert!(
                (m - m0).abs() <= LOBE_MARGIN_TOL_DB,
                "N={n}: lobe margin {m} dB, {m0} dB at N=0"
            ),
            (m, m0) => assert_eq!(m, m0, "N={n}: lobe margin"),
        }

        // 3. Consistent, with an excess N samples larger.
        let excess = excess_s(s).unwrap_or_else(|| {
            panic!(
                "N={n}: distance check {:?}, want Consistent; cross_check={:?}",
                s.distance_check, s.arrival_cross_check
            )
        });
        assert_eq!(
            samples_between(excess, excess0),
            n as i64,
            "N={n}: excess {:+.2} samples, {:+.2} at N=0",
            excess * fs,
            excess0 * fs
        );

        // 4. The late side never withholds: F₀ + N, always produced.
        let flight = s.flight_time_s.unwrap_or_else(|| {
            panic!(
                "N={n}: flight time withheld; cross_check={:?} distance_check={:?}",
                s.arrival_cross_check, s.distance_check
            )
        });
        assert_eq!(
            samples_between(flight, flight0),
            n as i64,
            "N={n}: flight time {:+.2} samples, {:+.2} at N=0",
            flight * fs,
            flight0 * fs
        );
    }
}
