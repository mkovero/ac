//! #550: the pre-impulse gate's floor region, replayed on the real
//! captures it used to refuse. At pupu's speaker ceiling the default-band
//! broadband argmax is a low-frequency room mode ≈ 2300 samples after the
//! arrival, so a floor that ends before the argmax holds the direct sound
//! and the early field. The shipped rule ends the floor before the earlier
//! of the band-limited arrival and the argmax.
//!
//! Sources: pupu, 2026-09-21, Genelec 1083 at 1.00 m, −50 dBFS typed, the
//! default 20 Hz–20 kHz / 4 s sweep, from the `d1p0-onaxis` corpus
//! (`$AC_HOME/corpus/d1p0-onaxis/`, `$AC_HOME` = `/home/mui/src/ac-wt`).
//! Each report's `data[0].data.linear_ir` (38400 samples) is stored as
//! little-endian f32 in `testdata/pupu-d1p0-onaxis-0N-linear-ir.f32le`.
//!
//! | N | report sha256 | `linear_ir` f32le sha256 |
//! |---|---|---|
//! | 01 | `31335faedc51c29cb7725252fd990af39fd1b6b81585823a491687aad1fa7ae4` | `ae0df63018a139e77f28d5e24a5f09fbb0f9a95fe51a69cb467bf8abc21bda73` |
//! | 02 | `b99a7b08c2fa7072291af5d1cf89f4c2a86f154e96379386fa9254ba6f7ccf2b` | `afd08b68372333d63e27415c8c82234d47c88491b80c73db823732c300329539` |
//! | 03 | `e26ff8c74a5891cae8deedd20b677616e1ffa9fb0b1c9f951b8d609c7379aad1` | `4fd47d82ab100372e9c08ec9a5f36bfce725bc5148ef5f3caa21e95ec7841f32` |
//! | 04 | `e7379f7755b190ed96963d0f52be218158c480b40276ac9f92a80287410b32dc` | `38b14a0f914b653cdc1d669c14a3a84ed627f6ba32e05233120bab1f670fd3aa` |
//! | 05 | `8289244935870fae6e1e1ebebfd449f52f5e8a0d6ab37ef9a03466a5cb9ce733` | `8c0a45758b2d31be6be8fc958f31ceb1a36767efb7e512d462791a62dcbf421b` |
//!
//! What makes this test fail:
//! - the rejected rule (floor before the broadband argmax) no longer
//!   refusing at least 4 of the 5 — the replay could then not tell the two
//!   rules apart;
//! - the shipped rule refusing any of the 5 (#550's bar is ≤ 1 of 9; on 5
//!   captures that is 0);
//! - an admitted capture whose flight time is more than ±3 samples from
//!   the 200 Hz set's +335 (#550 architect: 1 sample of observed spread
//!   plus 2 of margin; a half-cycle hop is ≈ 35 samples, the room mode
//!   ≈ 2300);
//! - the #537 arrival gate not being reached and passed on an admitted
//!   capture.

use super::fixtures::*;
use super::*;
use crate::measurement::sweep::pre_impulse_snr_db;

const SR: u32 = 96_000;
/// The capture's gate: 0.4 s at 96 kHz.
const LEN: usize = 38_400;
/// The same-capture reference τ recorded in all five reports: 1711 samples.
const TAU_S: f64 = 0.017822916666666667;
/// The `d1p0-onaxis-hp` (200 Hz–20 kHz) flight time, in samples: +335 on
/// all five captures.
const HP_MEDIAN_FLIGHT: i64 = 335;
/// #550's repeatability bar on the flight time, in samples.
const FLIGHT_BAR: i64 = 3;

/// `(name, linear_ir as f32le, before-argmax dB, before-arrival dB)`: the
/// figures are the #550 architect's replay table, printed `pre-imp SNR` for
/// the first.
static CAPTURES: [(&str, &[u8], f64, f64); 5] = [
    (
        "01",
        include_bytes!("testdata/pupu-d1p0-onaxis-01-linear-ir.f32le"),
        17.4,
        25.1,
    ),
    (
        "02",
        include_bytes!("testdata/pupu-d1p0-onaxis-02-linear-ir.f32le"),
        19.1,
        25.3,
    ),
    (
        "03",
        include_bytes!("testdata/pupu-d1p0-onaxis-03-linear-ir.f32le"),
        16.1,
        21.6,
    ),
    (
        "04",
        include_bytes!("testdata/pupu-d1p0-onaxis-04-linear-ir.f32le"),
        17.4,
        22.2,
    ),
    (
        "05",
        include_bytes!("testdata/pupu-d1p0-onaxis-05-linear-ir.f32le"),
        17.1,
        21.5,
    ),
];

fn linear_ir(bytes: &[u8]) -> Vec<f64> {
    let ir: Vec<f64> = bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64)
        .collect();
    assert_eq!(ir.len(), LEN, "testdata length");
    ir
}

/// The capture's report as recorded: default band and length, a 0.4 s
/// rectangular gate, 1.00 m, with the recorded reference τ on an identity
/// offset (the reports are schema v11 and carry none; see
/// `late_edge_replay.rs` for why `with_live_latency`).
fn report(linear_ir: Vec<f64>) -> MeasurementReport {
    let mut r = ir_report_with_custom_ir_band(linear_ir, SR, 20_000.0);
    r.method = MeasurementMethod::SweptSine {
        f1_hz: 20.0,
        f2_hz: 20_000.0,
        duration_s: 4.0,
    };
    r.stimulus.sample_rate_hz = SR;
    if let MeasurementData::ImpulseResponse {
        f1_hz, duration_s, ..
    } = &mut r.data[0].data
    {
        *f1_hz = 20.0;
        *duration_s = 4.0;
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
    with_live_latency(&mut r, TAU_S);
    r
}

#[test]
fn default_band_captures_at_the_ceiling_pass_with_the_floor_before_the_arrival() {
    let mut old_refused = 0;
    for (name, bytes, old_db, new_db) in CAPTURES {
        let ir = linear_ir(bytes);
        let r = report(ir.clone());
        let stats = r.ir_stats().expect("an impulse response");

        // The report is inside the scored domain: the gate's scope is not
        // what this replay is about.
        assert_eq!(
            pre_impulse_snr_scope(&r),
            Some(PreImpulseSnrScope::Scored),
            "{name}"
        );

        // 1. The rejected rule, computed here: floor before the argmax.
        let old = pre_impulse_snr_db(&ir, stats.peak_index);
        assert!(
            (old - old_db).abs() < 0.1,
            "{name}: before-argmax figure {old:.2} dB, table {old_db}"
        );
        if old < PRE_IMPULSE_SNR_MIN_DB {
            old_refused += 1;
        }

        // 2. The shipped rule: floor before the arrival, which precedes the
        // room-mode argmax here.
        assert!(
            (stats.high_pass_index == 21_245 || stats.high_pass_index == 21_246)
                && stats.peak_index > stats.high_pass_index + 2_000,
            "{name}: arrival {} argmax {}",
            stats.high_pass_index,
            stats.peak_index
        );
        assert_eq!(
            stats.pre_impulse_floor_anchor,
            PreImpulseAnchor::Arrival,
            "{name}"
        );
        assert!(
            (stats.pre_impulse_snr_db - new_db).abs() < 0.1,
            "{name}: before-arrival figure {:.2} dB, table {new_db}",
            stats.pre_impulse_snr_db
        );
        assert_eq!(stats.verdict, IrVerdict::Ok, "{name}");
        assert_eq!(
            stats.pre_impulse_floor_lines(),
            vec![format!(
                "floor ends 1200 samples before high-passed peak, sample {}",
                stats.high_pass_index
            )],
            "{name}"
        );

        // 3. #669: the arrival is the peak — here the room mode — so the
        // flight time is the 200 Hz set's plus the mode's lag, and the
        // advisory names the direct sound the high-pass found.
        let advisory = stats
            .high_pass_advisory
            .expect("the direct sound is advised");
        assert_eq!(
            advisory.earlier_samples,
            stats.peak_index as i64 - stats.high_pass_index as i64,
            "{name}"
        );
        let flight = (stats.flight_time_s.expect("flight time") * SR as f64).round() as i64;
        assert!(
            (flight - advisory.earlier_samples - HP_MEDIAN_FLIGHT).abs() <= FLIGHT_BAR,
            "{name}: flight {flight:+} samples, 200 Hz median {HP_MEDIAN_FLIGHT:+} \
             plus the mode's {} samples",
            advisory.earlier_samples
        );

        // 4. The #537 arrival gate is reached and scored.
        let arrival_snr = stats.band_limited_snr_db.expect("band-limited arrival");
        assert!(
            arrival_snr >= ARRIVAL_SNR_MIN_DB,
            "{name}: arrival SNR {arrival_snr:.1} dB"
        );
        assert!(
            !stats.high_pass_check.disputes_the_pick()
                && !matches!(
                    stats.high_pass_check,
                    HighPassCheck::BandLimitedSnrLow { .. }
                ),
            "{name}: {:?}",
            stats.high_pass_check
        );

        // `ARRIVAL_BROADBAND_COMPARABLE_DB`'s coupling claim, on the floor
        // now gated: the largest pre-arrival sample inside the cross-check
        // tolerance, ahead of the arrival's own lobe, stays below the
        // comparable level.
        let tol = high_pass_check_tolerance_samples(SR) as usize;
        let lobe = crate::measurement::sweep::lobe_window_samples(
            SR,
            crate::measurement::sweep::ARRIVAL_HIGH_PASS_CORNER_HZ,
        );
        let pre = &ir[stats.high_pass_index - tol..stats.high_pass_index - lobe];
        let pre_max = pre.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        let level_db = 20.0 * (pre_max / stats.peak_magnitude).log10();
        assert!(
            level_db < -ARRIVAL_BROADBAND_COMPARABLE_DB,
            "{name}: pre-arrival noise inside the tolerance at {level_db:.1} dB"
        );
    }
    assert!(
        old_refused >= 4,
        "the before-argmax rule refused {old_refused} of 5 — the replay no longer separates \
         the rejected rule from the shipped one"
    );
}
