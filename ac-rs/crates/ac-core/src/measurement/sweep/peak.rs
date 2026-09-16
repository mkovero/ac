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
mod tests {
    use super::*;
    use crate::measurement::sweep::{
        deconvolve_full, estimate_onset, extract_irs, inverse_sweep, log_sweep, CausalBound,
        MissingBoundInput, SweepParams,
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
}
