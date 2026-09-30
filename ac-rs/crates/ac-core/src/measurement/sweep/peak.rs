//! Which sample is "the arrival" for a deconvolved impulse response —
//! shared by every caller that has to difference two arrivals (#351).
//!
//! Both halves of `ir_arrival_distance()`'s `arrival_s − τ_s` — the IR's
//! own peak ([`crate::measurement::report::MeasurementReport::ir_stats`])
//! and calibrate's τ leg (`analyse_tau_leg` in `ac-daemon`) — call
//! [`ir_peak`] rather than each picking their own maximum, so the two
//! halves cannot drift onto different tie-break or NaN rules the way they
//! had before this issue (one kept the earliest index on a tie and
//! skipped NaN, the other kept the latest and panicked on NaN).
//!
//! Why the pairing cancels across different sweep bands: a Farina ESS
//! deconvolution of a pure delay is an approximately linear-phase
//! band-pass compressed pulse centred on the delay. Its magnitude peak
//! falls on the delay whatever `f1_hz`/`f2_hz`/window are — the band only
//! changes the *width* of the pulse's skirt, not where its centre sits —
//! so calibrate's short-ESS τ sweep and a `plot_ir` capture with a
//! different band and window agree on the peak index. A threshold or
//! change-point read off that skirt would not: its width scales with
//! `1/bandwidth` (#351 measured −0.093 m, about 13 samples, on a zero-path
//! fake loopback with mismatched bands). That is one reason the #346 onset
//! estimator was removed (#734): the arrival has one definition, a peak.
//! This file's band-invariance test measures the peak's half of the claim
//! on a real Farina deconvolution.
//!
//! A zero-phase band-limited peak ([`crate::measurement::sweep::band_limited_peak`],
//! #537) pairs like a peak: high-passing a centred pulse with zero phase
//! narrows its skirt but does not move its centre, so the band-limited peak
//! of a pure delay lands on the same sample as [`ir_peak`] in any band this
//! file's test uses, and may be differenced against a stored `calibrate` τ.
//! ISO 3382-1:2009 §A.3.4 allows a start "from the broadband or high
//! frequency impulse responses and the measured delay of the filters"; the
//! zero-phase filter makes that delay zero. A threshold or onset read off
//! the band-limited IR would not pair — it sits on the skirt, exactly as
//! above.

/// Index and magnitude of the largest-magnitude sample of a linear IR.
///
/// Ties keep the earliest index. A NaN sample is never selected — not
/// only "never overtakes the running best" but never becomes the
/// returned index either, including a leading NaN with nothing but ties
/// after it (`[NaN, 0.0]` returns `(1, 0.0)`, not the NaN at index 0).
/// The fold tracks "no candidate yet" as `None` rather than seeding on
/// `(0, 0.0)`, so a NaN can never be adopted by tying against a fake
/// zero-magnitude seed. `(0, 0.0)` is returned only when no sample was
/// ever a real candidate — an empty slice, or a slice that is entirely
/// NaN; this is not a real peak, and a caller with an empty-IR case must
/// refuse before calling this rather than let `(0, 0.0)` read as a
/// legitimate zero-index arrival.
///
/// One definition, shared by
/// [`crate::measurement::report::MeasurementReport::ir_stats`] and
/// `ac-daemon`'s `analyse_tau_leg` (#351) — see the module doc for why
/// they must agree.
pub fn ir_peak(linear_ir: &[f64]) -> (usize, f64) {
    linear_ir
        .iter()
        .enumerate()
        .fold(None, |acc: Option<(usize, f64)>, (i, &v)| {
            let m = v.abs();
            let replace = match acc {
                Some((_, best)) => m > best,
                None => !m.is_nan(),
            };
            if replace {
                Some((i, m))
            } else {
                acc
            }
        })
        .unwrap_or((0, 0.0))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::measurement::sweep::{
        deconvolve_full, extract_irs, inverse_sweep, log_sweep, SweepParams,
    };

    #[test]
    fn ir_peak_keeps_the_earlier_index_on_a_tie() {
        let ir = [0.5, 1.0, -1.0, 0.2];
        assert_eq!(ir_peak(&ir), (1, 1.0));
    }

    #[test]
    fn ir_peak_never_selects_a_nan_sample() {
        let ir = [0.1, f64::NAN, 0.9, f64::NAN];
        let (idx, mag) = ir_peak(&ir);
        assert_eq!(idx, 2);
        assert_eq!(mag, 0.9);
    }

    #[test]
    fn ir_peak_is_zero_zero_on_an_empty_slice() {
        assert_eq!(ir_peak(&[]), (0, 0.0));
    }

    /// codex-qa on #479: a leading NaN tying against the old `(0, 0.0)`
    /// seed returned index 0 — naming the NaN sample as the peak even
    /// though NaN is documented as never selected. `ir_peak` must not
    /// seed the fold on a fake zero-magnitude candidate.
    #[test]
    fn ir_peak_does_not_select_a_leading_nan_that_ties_the_seed() {
        assert_eq!(ir_peak(&[f64::NAN, 0.0]), (1, 0.0));
    }

    /// An all-NaN slice has no real candidate at all, same as an empty
    /// slice — `(0, 0.0)` here is the documented no-valid-sample
    /// fallback, not a claim that index 0 is a peak.
    #[test]
    fn ir_peak_is_zero_zero_on_an_all_nan_slice() {
        assert_eq!(ir_peak(&[f64::NAN, f64::NAN]), (0, 0.0));
    }

    /// #351 acceptance (AC6's record): the shared picker's offset from the
    /// gate centre does not depend on the sweep's band or window, measured
    /// on a real `log_sweep` → integer-sample delay → `deconvolve_full` →
    /// `extract_irs` capture rather than a hand-built fixture.
    #[test]
    fn ir_peak_offset_is_band_invariant() {
        let sr = 48_000u32;
        let delay = 1_000usize;
        let window_len = 2_048usize;

        // Configuration A: calibrate's own τ-sweep shape
        // (`tau_sweep_params` in
        // `ac-daemon/src/handlers/calibrate/tau/measure.rs`).
        let p_a = SweepParams {
            f1_hz: 100.0,
            f2_hz: 20_000.0,
            duration_s: 0.2,
            sample_rate: sr,
        };
        // Configuration B: a `plot_ir`-like band and window, deliberately
        // narrower in band and longer in duration than A.
        let p_b = SweepParams {
            f1_hz: 200.0,
            f2_hz: 8_000.0,
            duration_s: 0.5,
            sample_rate: sr,
        };

        let peak_offset_a = peak_offset(&p_a, delay, window_len);
        let peak_offset_b = peak_offset(&p_b, delay, window_len);

        assert_eq!(
            peak_offset_a, delay as i64,
            "config A: peak must land exactly on the modelled delay"
        );
        assert_eq!(
            peak_offset_b, delay as i64,
            "config B: peak must land exactly on the modelled delay"
        );
        assert_eq!(
            peak_offset_a, peak_offset_b,
            "the shared peak picker must agree across different sweep bands \
             — this is why calibrate's τ and an IR's arrival cancel in a \
             subtraction"
        );
    }

    /// #537: the claim that licenses picking the arrival off a zero-phase
    /// high-passed IR. On the same pure-delay chain as the band-invariance
    /// test above, the band-limited peak lands exactly on the modelled delay
    /// in both configurations — the same sample [`ir_peak`] picks — so it
    /// cancels against a peak-picked τ the way [`ir_peak`] does.
    #[test]
    fn band_limited_peak_offset_is_band_invariant_and_equals_ir_peak() {
        use crate::measurement::sweep::{band_limited_peak, ARRIVAL_HIGH_PASS_CORNER_HZ};
        let sr = 48_000u32;
        let delay = 1_000usize;
        let window_len = 2_048usize;
        for p in [band_a(sr), band_b(sr)] {
            let x = log_sweep(&p).unwrap();
            let mut y = vec![0.0_f32; x.len() + delay];
            y[delay..].copy_from_slice(&x);
            let full = deconvolve_full(&y, &inverse_sweep(&p).unwrap());
            let irs = extract_irs(&full, &p, 1, window_len).unwrap();
            let centre = window_len / 2;
            let (peak, _) = ir_peak(&irs.linear);
            let (band_limited, _) =
                band_limited_peak(&irs.linear, sr, p.f2_hz, ARRIVAL_HIGH_PASS_CORNER_HZ)
                    .expect("both bands reach two octaves above the corner");
            assert_eq!(
                band_limited as i64 - centre as i64,
                delay as i64,
                "{p:?}: the band-limited peak must land exactly on the modelled delay"
            );
            assert_eq!(
                band_limited, peak,
                "{p:?}: band-limited peak equals ir_peak"
            );
        }
    }

    /// Build a pure-delay capture at `params` (`y(n) = x(n − delay)`),
    /// deconvolve it, and return the peak's signed offset from the gate
    /// centre.
    fn peak_offset(params: &SweepParams, delay: usize, window_len: usize) -> i64 {
        let x = log_sweep(params).unwrap();
        let mut y = vec![0.0_f32; x.len() + delay];
        y[delay..].copy_from_slice(&x);
        let xi = inverse_sweep(params).unwrap();
        let full = deconvolve_full(&y, &xi);
        let irs = extract_irs(&full, params, 1, window_len).unwrap();
        let (peak_index, _) = ir_peak(&irs.linear);
        peak_index as i64 - (window_len / 2) as i64
    }

    /// #537 architect revision 3, item 5: #346's two-way DUT, in both bands
    /// and at both group delays, never *produces* a band-limited arrival more
    /// than 5 samples from the HF component's delay `t0` — the property the
    /// fixture exists for. Revision 2 required `Agrees` in all four; the
    /// Rust gave `ArrivalAmbiguous` in three (developer stop, 2026-09-18),
    /// and revision 3 replaced the requirement with this one, stating each
    /// standing as a documented fact:
    /// Re-pinned on the corrected inverse (#733, 2026-09-30):
    /// - narrow and rig-like with the loud low component (A and B):
    ///   `ArrivalAmbiguous`, correct refusals — the pick lands on the low
    ///   component (+5 to +59) with a comparable lobe beside it.
    /// - rig-like at the original gain (A and B): `Agrees` on `t0`, the
    ///   broadband peak on `t0` too. Before #733 band B refused this case
    ///   as ambiguous (the recorded false refusal); the tilted kernel made
    ///   the low component look loud.
    #[test]
    fn two_way_dut_band_limited_arrival_is_never_produced_off_t0() {
        use crate::measurement::report::{band_limited_arrival, ArrivalCrossCheck};
        const BUDGET: i64 = 5;
        for (name, band, sr, shape, ambiguous, pick_on_t0) in [
            (
                "A narrow",
                band_a as fn(u32) -> SweepParams,
                48_000,
                &TWO_WAY_NARROW,
                true,
                false,
            ),
            ("B narrow", band_b, 48_000, &TWO_WAY_NARROW, true, false),
            (
                "A rig-like loud",
                band_a,
                96_000,
                &TWO_WAY_RIG_LIKE_LOUD,
                true,
                false,
            ),
            (
                "B rig-like loud",
                band_b,
                96_000,
                &TWO_WAY_RIG_LIKE_LOUD,
                true,
                false,
            ),
            ("A rig-like", band_a, 96_000, &TWO_WAY_RIG_LIKE, false, true),
            ("B rig-like", band_b, 96_000, &TWO_WAY_RIG_LIKE, false, true),
        ] {
            let p = band(sr);
            let r = two_way(&p, shape);
            let peak = (r.centre as i64 + r.peak) as usize;
            let a = band_limited_arrival(&r.ir, p.sample_rate, p.f2_hz, peak);
            let arrival = a.arrival_index as i64 - r.centre as i64 - TWO_WAY_T0;
            let context = format!(
                "{name}: {:?}, arrival t0 {arrival:+}, margin {:?}, SNR {:?}",
                a.cross_check, a.lobe_margin_db, a.band_limited_snr_db
            );
            if !a.cross_check.disputes_the_arrival() {
                assert!(arrival.abs() <= BUDGET, "produced off t0 — {context}");
            }
            assert_eq!(
                matches!(a.cross_check, ArrivalCrossCheck::ArrivalAmbiguous { .. }),
                ambiguous,
                "{context}"
            );
            if !ambiguous {
                assert!(
                    matches!(a.cross_check, ArrivalCrossCheck::Agrees { .. }),
                    "{context}"
                );
            }
            assert_eq!(arrival.abs() <= 1, pick_on_t0, "{context}");
        }
    }

    /// `t0` of [`two_way`]'s full-band component, as an offset
    /// from the gate centre.
    pub(crate) const TWO_WAY_T0: i64 = 1_000;

    fn band_a(sample_rate: u32) -> SweepParams {
        SweepParams {
            f1_hz: 100.0,
            f2_hz: 20_000.0,
            duration_s: 0.2,
            sample_rate,
        }
    }

    fn band_b(sample_rate: u32) -> SweepParams {
        SweepParams {
            f1_hz: 200.0,
            f2_hz: 8_000.0,
            duration_s: 0.5,
            sample_rate,
        }
    }

    /// Shape of the two-way DUT's low component.
    pub(crate) struct TwoWayShape {
        /// Extra delay of the low component past `t0`, samples.
        g: usize,
        /// Boxcar low-pass length, samples.
        taps: usize,
        /// The low component's per-sample peak re the full-band arrival's
        /// (`0.3`). `None`: the original fixture's flat gain of 1.0, a
        /// `1/taps` per-sample peak, under the arrival.
        low_over_high: Option<f32>,
    }

    /// The branch's original fixture: small group delay.
    ///
    /// #733: with the original gain, the low component outweighed the
    /// arrival only through the inverse filter's reversed envelope (~40 dB
    /// of low-band boost). These shapes set it to twice the arrival's
    /// per-sample peak, so the broadband peak lands late on its own merit —
    /// what the arrival test below needs to refuse.
    const TWO_WAY_NARROW: TwoWayShape = TwoWayShape {
        g: 8,
        taps: 9,
        low_over_high: Some(2.0),
    };
    /// Rig-like group delay at 96 kHz (G = 24 samples),
    /// loud low component as [`TWO_WAY_NARROW`].
    const TWO_WAY_RIG_LIKE_LOUD: TwoWayShape = TwoWayShape {
        g: 24,
        taps: 33,
        low_over_high: Some(2.0),
    };
    /// Rig-like group delay with the original gain: the arrival suite's
    /// speaker kernel. With the corrected inverse its broadband peak lands
    /// on `t0`, as pupu's 1083 did on 2026-09-30.
    pub(crate) const TWO_WAY_RIG_LIKE: TwoWayShape = TwoWayShape {
        g: 24,
        taps: 33,
        low_over_high: None,
    };

    pub(crate) struct TwoWay {
        /// The windowed linear IR.
        pub(crate) ir: Vec<f64>,
        /// Gate centre index in `ir`.
        pub(crate) centre: usize,
        /// The broadband peak's offset from the gate centre, signed samples.
        peak: i64,
    }

    /// Capture a two-way DUT at `params`: `0.3·x(n − t0)` plus
    /// `LP(x)(n − t0 − G)`, where `LP` is a `taps`-long causal boxcar
    /// (linear phase, `(taps − 1)/2` samples of its own delay), and return
    /// its windowed linear IR.
    pub(crate) fn two_way(params: &SweepParams, shape: &TwoWayShape) -> TwoWay {
        const HIGH_GAIN: f32 = 0.3;
        let window_len = 4_096usize;
        let TwoWayShape {
            g,
            taps,
            low_over_high,
        } = *shape;
        let low_gain = low_over_high.map_or(1.0, |r| r * HIGH_GAIN * taps as f32);
        let t0 = TWO_WAY_T0 as usize;
        let x = log_sweep(params).unwrap();
        let lp: Vec<f32> = (0..x.len())
            .map(|n| {
                let from = n.saturating_sub(taps - 1);
                x[from..=n].iter().sum::<f32>() / taps as f32
            })
            .collect();
        let mut y = vec![0.0_f32; x.len() + t0 + g + taps];
        for (n, &v) in x.iter().enumerate() {
            y[n + t0] += HIGH_GAIN * v;
        }
        // The boxcar above is causal, so its own delay is already in `lp`;
        // the low component's total delay is t0 + G + (taps − 1)/2.
        for (n, &v) in lp.iter().enumerate() {
            y[n + t0 + g] += low_gain * v;
        }
        let xi = inverse_sweep(params).unwrap();
        let full = deconvolve_full(&y, &xi);
        let irs = extract_irs(&full, params, 1, window_len).unwrap();
        let (peak_index, _) = ir_peak(&irs.linear);
        let centre = window_len / 2;
        TwoWay {
            peak: peak_index as i64 - centre as i64,
            ir: irs.linear,
            centre,
        }
    }
}
