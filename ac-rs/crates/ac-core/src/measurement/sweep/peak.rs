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
//! different band and window agree on the peak index. An onset
//! ([`crate::measurement::sweep::estimate_onset`]) is a threshold/change-
//! point read off that same skirt, and the skirt's width scales with
//! `1/bandwidth`, so an onset does *not* cancel across bands the way the
//! peak does (issue #351; measured as −0.093 m, about 13 samples, on a
//! zero-path fake loopback with mismatched bands, before this module
//! existed). See this file's band-invariance test, which measures both
//! claims on a real Farina deconvolution rather than asserting them.
//!
//! Pairing rule, so a later change does not reopen this: an onset-derived
//! arrival may only be differenced against a τ picked by the *same*
//! onset rule from the *same* capture's reference leg (#460) — never
//! against a stored `calibrate` τ, which was measured under a different
//! sweep and cannot be guaranteed to share the onset's skirt width. A
//! [`ir_peak`] result may always be differenced against another
//! [`ir_peak`] result, from any capture, because the band-invariance
//! above is what makes that pairing cancel.
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
    use crate::measurement::sweep::onset::aic_change_point;
    use crate::measurement::sweep::{
        deconvolve_full, estimate_onset, extract_irs, inverse_sweep, log_sweep, BoundInputs,
        CausalBound, EdgeGuard, MissingBoundInput, OnsetEstimate, OnsetPick, SweepParams,
        WindowLimit, EDGE_GUARD_TOLERANCE_SAMPLES,
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
    /// gate centre does not depend on the sweep's band or window, while
    /// `estimate_onset`'s does — the "test against the rejected
    /// implementation" for this module's pairing rule, measured on a real
    /// `log_sweep` → integer-sample delay → `deconvolve_full` →
    /// `extract_irs` capture rather than a hand-built fixture.
    ///
    /// If the onset offsets stop differing here, the #351 rationale
    /// (matching estimators alone does not make τ and an onset-derived
    /// arrival cancel) no longer holds on this fixture — report that back
    /// rather than loosening the assertion.
    #[test]
    fn ir_peak_offset_is_band_invariant_while_onset_offset_is_not() {
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

        let (peak_offset_a, onset_offset_a) = peak_and_onset_offset(&p_a, delay, window_len);
        let (peak_offset_b, onset_offset_b) = peak_and_onset_offset(&p_b, delay, window_len);

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

        assert_ne!(
            onset_offset_a, onset_offset_b,
            "the onset picker's offset must differ across sweep bands, or \
             this fixture no longer demonstrates why matching estimators \
             alone would not have fixed #351"
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
    /// deconvolve it, and return `(peak_offset, onset_offset)`, each the
    /// signed sample offset from the gate centre.
    fn peak_and_onset_offset(params: &SweepParams, delay: usize, window_len: usize) -> (i64, i64) {
        let x = log_sweep(params).unwrap();
        let mut y = vec![0.0_f32; x.len() + delay];
        y[delay..].copy_from_slice(&x);
        let xi = inverse_sweep(params).unwrap();
        let full = deconvolve_full(&y, &xi);
        let irs = extract_irs(&full, params, 1, window_len).unwrap();
        let (peak_index, _) = ir_peak(&irs.linear);
        let centre = window_len / 2;

        let unbounded = CausalBound::Unavailable(MissingBoundInput::Both {
            reference_reason: String::new(),
        });
        let onset = estimate_onset(
            &irs.linear,
            peak_index,
            params.sample_rate,
            1e-9,
            &unbounded,
        );
        (
            peak_index as i64 - centre as i64,
            onset.index as i64 - centre as i64,
        )
    }

    /// #346 architect revisions 2 to 4: estimator and guard properties of
    /// the bounded onset. A two-way DUT — a smaller full-band component at `t0`
    /// plus a larger low-passed one at `t0 + G` — whose magnitude peak
    /// lands late, captured at both of the band-invariance test's sweep
    /// bands, with the causal bound one hand-tape error (5 cm) before `t0`.
    ///
    /// (i) Against the rejected rule: in both bands the bounded onset is
    /// closer to `t0` than the peak is.
    /// (ii) The bounded onset's offset differs between the two bands by at
    /// most one sample — the same integer-rounding budget as #351's
    /// hardware budget. Any future design that differences an onset
    /// against a peak-picked τ would need this; no such pairing is licensed
    /// today (this module's pairing rule). If it fails, report it back, do
    /// not loosen it.
    /// (iii) The edge guard passes band A's pick. Band B is deliberately
    /// not asserted: its pick is right (`t0 + 1`) but the re-pick over the
    /// window started 5 cm earlier lands at `t0 − 1`, a 2-sample move, so
    /// the guard refuses it and the onset standing is `EdgeFollowing`. That is
    /// a known false refusal (architect revision 3 probe), not a reason to
    /// widen [`crate::measurement::sweep::EDGE_GUARD_TOLERANCE_SAMPLES`]:
    /// a tolerance of 2 also passes a 12-samples-late pick.
    #[test]
    fn bounded_onset_on_a_two_way_dut_beats_the_peak_and_is_band_invariant() {
        let a = two_way_bounded(&band_a(48_000), &TWO_WAY_NARROW);
        let b = two_way_bounded(&band_b(48_000), &TWO_WAY_NARROW);
        for (name, r) in [("A", &a), ("B", &b)] {
            assert!(
                r.peak - TWO_WAY_T0 >= 10,
                "test setup, config {name}: the peak must land at least 10 samples \
                 after t0, got {}",
                r.peak - TWO_WAY_T0
            );
            assert!(
                (r.onset - TWO_WAY_T0).abs() < (r.peak - TWO_WAY_T0).abs(),
                "config {name}: bounded onset {} must be closer to t0 {TWO_WAY_T0} than \
                 the peak {}",
                r.onset,
                r.peak
            );
            assert!(
                r.bound_binds(),
                "config {name}: the bound must set the window start"
            );
        }
        assert!(
            (a.onset - b.onset).abs() <= 1,
            "bounded onset offset moved {} samples between bands (A {}, B {}) — the \
             bounded onset is not band-invariant to one sample",
            (a.onset - b.onset).abs(),
            a.onset,
            b.onset
        );
        assert_eq!(
            a.edge_guard(),
            Some(EdgeGuard::Passed),
            "band A's pick {} is right and must pass the edge guard",
            a.onset
        );
    }

    /// #537 architect revision 3, item 5: #346's two-way DUT, in both bands
    /// and at both group delays, never *produces* a band-limited arrival more
    /// than 5 samples from the HF component's delay `t0` — the property the
    /// fixture exists for. Revision 2 required `Agrees` in all four; the
    /// Rust gave `ArrivalAmbiguous` in three (developer stop, 2026-09-18),
    /// and revision 3 replaced the requirement with this one, stating each
    /// standing as a documented fact:
    /// - narrow (A and B): `ArrivalAmbiguous`, a correct refusal. At 48 kHz
    ///   the 9-tap boxcar passes 2–5 kHz at gain 1.0 against the HF
    ///   component's 0.3, so the pick lands on the low component (+13), with
    ///   a half-cycle within about 1 dB of it.
    /// - A rig-like: `Agrees`, on `t0`.
    /// - B rig-like: `ArrivalAmbiguous`, margin about 2.6 dB, with the pick
    ///   on `t0`. **A known false refusal of a correct pick**, recorded here
    ///   as the rule's cost. It is not a reason to retune
    ///   [`crate::measurement::report::ARRIVAL_LOBE_MARGIN_MIN_DB`].
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
            ("A rig-like", band_a, 96_000, &TWO_WAY_RIG_LIKE, false, true),
            ("B rig-like", band_b, 96_000, &TWO_WAY_RIG_LIKE, true, true),
        ] {
            let p = band(sr);
            let r = two_way_bounded(&p, shape);
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

    /// #346 architect revision 3: the guard fires on a rig-like two-way
    /// edge-follower. 96 kHz, band A, `G = 24`, 33-tap boxcar, bound
    /// `t0 − 14` (5 cm). The unguarded bounded pick lands well after `t0`,
    /// between the bound and the peak — the pattern of the rig's 2 m
    /// capture (bound +0, onset +20, peak +40). It is clear of the window
    /// start and would pass every other onset condition, so without the
    /// guard its standing would be `Unscored`.
    #[test]
    fn edge_guard_fires_on_a_rig_like_two_way_edge_follower() {
        let r = two_way_bounded(&band_a(96_000), &TWO_WAY_RIG_LIKE);

        // Computed inline from the IR, not taken from the estimate.
        let (peak_inline, _) = ir_peak(&r.ir);
        assert_eq!(peak_inline as i64 - r.centre as i64, r.peak);
        let unguarded = r.bound_index
            + aic_change_point(&r.ir[r.bound_index..=peak_inline]).expect("window has variance");
        let unguarded = unguarded as i64 - r.centre as i64;
        assert_eq!(
            r.onset, unguarded,
            "the guard reports, it does not move the pick"
        );

        assert!(
            r.onset - TWO_WAY_T0 >= 10,
            "test setup: the pick must follow the bound, landing at least 10 samples \
             after t0; got {} (bound {}, peak {})",
            r.onset - TWO_WAY_T0,
            r.bound_index as i64 - r.centre as i64 - TWO_WAY_T0,
            r.peak - TWO_WAY_T0
        );
        assert!(r.onset < r.peak, "test setup: the pick is before the peak");
        assert!(
            r.bound_binds(),
            "test setup: the bound sets the window start"
        );
        match r.estimate.pick {
            OnsetPick::Picked { pinned, .. } => {
                assert!(!pinned, "test setup: the pick is clear of the window start")
            }
            OnsetPick::Declined => panic!("picker declined: {}", r.estimate.rule),
        }
        assert!(
            matches!(r.edge_guard(), Some(EdgeGuard::Failed { repick: Some(_) })),
            "the guard must refuse an edge-following pick, got {:?}",
            r.edge_guard()
        );
    }

    /// #346 architect revision 3, tested against the rejected
    /// implementation: revision 2's guard re-picked over a window whose
    /// start was moved *later*, by half the lead segment. On a noise-free
    /// deconvolution the pre-onset samples are band-limited skirt, so the
    /// trim moves a correct pick. 48 kHz, band A, `G = 8`, 9-tap boxcar,
    /// bound `t0 − 7`: the trim re-pick is computed inline and must move
    /// by more than the tolerance, while the shipped extension guard
    /// passes the same pick.
    #[test]
    fn extension_guard_passes_a_right_pick_the_trim_guard_refused() {
        let r = two_way_bounded(&band_a(48_000), &TWO_WAY_NARROW);
        let onset_abs = r.estimate.index;
        assert!(
            (r.onset - TWO_WAY_T0).abs() <= 1,
            "test setup: the pick must be right, got t0 {:+}",
            r.onset - TWO_WAY_T0
        );

        let (peak_index, _) = ir_peak(&r.ir);
        let trimmed_start = r.bound_index + (onset_abs - r.bound_index).div_ceil(2);
        let trim_repick = trimmed_start
            + aic_change_point(&r.ir[trimmed_start..=peak_index]).expect("window has variance");
        assert!(
            trim_repick.abs_diff(onset_abs) > EDGE_GUARD_TOLERANCE_SAMPLES,
            "test setup: the rejected trim guard must refuse this right pick; its re-pick \
             went to t0 {:+}",
            trim_repick as i64 - r.centre as i64 - TWO_WAY_T0
        );

        assert_eq!(r.edge_guard(), Some(EdgeGuard::Passed));
    }

    /// `t0` of [`two_way_bounded`]'s full-band component, as an offset
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
    }

    /// The branch's original fixture: small group delay.
    const TWO_WAY_NARROW: TwoWayShape = TwoWayShape { g: 8, taps: 9 };
    /// Rig-like group delay at 96 kHz (onset-to-peak gap ≥ 23 samples).
    pub(crate) const TWO_WAY_RIG_LIKE: TwoWayShape = TwoWayShape { g: 24, taps: 33 };

    pub(crate) struct TwoWay {
        /// The windowed linear IR.
        pub(crate) ir: Vec<f64>,
        /// Gate centre index in `ir`.
        pub(crate) centre: usize,
        /// Enforced causal bound, absolute index in `ir`.
        bound_index: usize,
        estimate: OnsetEstimate,
        /// Offsets from the gate centre, signed samples.
        peak: i64,
        onset: i64,
    }

    impl TwoWay {
        fn bound_binds(&self) -> bool {
            matches!(
                self.estimate.pick,
                OnsetPick::Picked {
                    limit: WindowLimit::CausalBound,
                    ..
                }
            )
        }

        fn edge_guard(&self) -> Option<EdgeGuard> {
            match self.estimate.pick {
                OnsetPick::Picked { edge_guard, .. } => edge_guard,
                OnsetPick::Declined => None,
            }
        }
    }

    /// Capture a two-way DUT at `params`: `0.3·x(n − t0)` plus
    /// `LP(x)(n − t0 − G)`, where `LP` is a `taps`-long causal boxcar
    /// (linear phase, `(taps − 1)/2` samples of its own delay), then run the
    /// bounded onset picker with the bound 5 cm of flight at 343 m/s before
    /// `t0` (7 samples at 48 kHz, 14 at 96 kHz).
    pub(crate) fn two_way_bounded(params: &SweepParams, shape: &TwoWayShape) -> TwoWay {
        const LOW_GAIN: f32 = 1.0;
        const HIGH_GAIN: f32 = 0.3;
        const C: f64 = 343.0;
        let window_len = 4_096usize;
        let TwoWayShape { g, taps } = *shape;
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
            y[n + t0 + g] += LOW_GAIN * v;
        }
        let xi = inverse_sweep(params).unwrap();
        let full = deconvolve_full(&y, &xi);
        let irs = extract_irs(&full, params, 1, window_len).unwrap();
        let (peak_index, _) = ir_peak(&irs.linear);
        let centre = window_len / 2;

        // ±5 cm hand tape (#346 architect revision 2).
        let tape_samples = (0.05 / C * params.sample_rate as f64).round() as usize;
        let bound_index = centre + t0 - tape_samples;
        let bound = CausalBound::Enforced {
            index: bound_index,
            inputs: BoundInputs {
                reference_tau_s: 0.0,
                distance_m: 1.0,
                speed_of_sound_m_s: C,
                temperature_c: None,
            },
        };
        let estimate = estimate_onset(&irs.linear, peak_index, params.sample_rate, 1e-9, &bound);
        TwoWay {
            peak: peak_index as i64 - centre as i64,
            onset: estimate.index as i64 - centre as i64,
            ir: irs.linear,
            centre,
            bound_index,
            estimate,
        }
    }
}
