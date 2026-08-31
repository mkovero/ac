//! Splitting a Farina deconvolution into the linear impulse response and
//! the pre-impulse harmonic-order impulse responses, including the gate
//! arithmetic that keeps adjacent orders from overlapping.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use super::SweepParams;

/// A single harmonic-order impulse response extracted from a Farina
/// deconvolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarmonicIr {
    pub order: u32,
    pub samples: Vec<f64>,
}

/// Outcome of `extract_irs`: the linear IR plus per-order harmonic IRs.
#[derive(Debug, Clone, PartialEq)]
pub struct DeconvolvedIrs {
    pub linear: Vec<f64>,
    pub harmonics: Vec<HarmonicIr>,
    /// The `window_len` the caller asked `extract_irs` for, before any
    /// clamping.
    pub window_len_requested: usize,
    /// Gate length actually used per order: index `i` is order `i + 1`, so
    /// index 0 is the linear IR. Each entry is `window_len_requested`
    /// clamped down to the sample distance to that order's nearest
    /// neighbouring order — see [`extract_irs`]. This is the gate-length
    /// decision, so it stays populated for an order whose gate fell off
    /// the front of `full` and therefore has empty `samples`.
    pub window_len_used: Vec<usize>,
    /// The sweep parameters `extract_irs` was called with. These set the
    /// adjacent-harmonic-order gap (`Δt_k = duration_s · ln(k) /
    /// ln(f2_hz/f1_hz)`) that [`Self::clamp_note`] names as the reason for
    /// any clamp — kept here rather than re-threaded through the call site
    /// so the note can be produced from `self` alone.
    pub params: SweepParams,
}

impl DeconvolvedIrs {
    /// Gate length used for harmonic `order` (order 1 = the linear IR),
    /// or `None` if that order was not extracted.
    pub fn window_len_for(&self, order: u32) -> Option<usize> {
        let idx = (order as usize).checked_sub(1)?;
        self.window_len_used.get(idx).copied()
    }

    /// One-line operator-facing note naming every order whose gate was
    /// clamped below the requested length, or `None` when the request was
    /// honoured for every order. Meant for `MeasurementReport.notes`: a
    /// shortened gate changes what the harmonic IRs mean, so it must not
    /// reach the operator as a silent substitution.
    ///
    /// Names the reason (the adjacent-harmonic-order gap) and the sweep
    /// parameters that set it (#342) — a caller who only sees "clamped to
    /// N samples" has no knob to turn; one who sees `duration_s`/`f1_hz`/
    /// `f2_hz` does.
    pub fn clamp_note(&self) -> Option<String> {
        let clamped: Vec<String> = self
            .window_len_used
            .iter()
            .enumerate()
            .filter(|(_, &w)| w < self.window_len_requested)
            .map(|(i, &w)| format!("order {} \u{2192} {} samples", i + 1, w))
            .collect();
        if clamped.is_empty() {
            return None;
        }
        Some(format!(
            "harmonic gate window clamped below the requested {} samples so \
             adjacent orders do not overlap: {}. Orders not listed used the \
             requested length. The adjacent-harmonic-order gap is set by \
             f1_hz={} Hz, f2_hz={} Hz, duration_s={} s (sample_rate={} Hz) \
             — widen it by increasing duration_s or narrowing the \
             f2_hz/f1_hz span.",
            self.window_len_requested,
            clamped.join(", "),
            self.params.f1_hz,
            self.params.f2_hz,
            self.params.duration_s,
            self.params.sample_rate,
        ))
    }
}

/// Sample offsets of the harmonic-order gate centres, orders `1..=n`,
/// measured back from the linear IR (so index 0 is always 0 and the
/// sequence is non-decreasing).
fn harmonic_offsets_samples(p: &SweepParams, n_harmonics: usize) -> Vec<i64> {
    let fs = p.sample_rate as f64;
    (1..=n_harmonics as u32)
        .map(|k| (p.harmonic_time_offset_s(k) * fs).round() as i64)
        .collect()
}

/// Per-order gate lengths for `window_len`, each clamped down to the
/// sample distance to that order's nearest neighbouring order.
///
/// `offsets` is [`harmonic_offsets_samples`] for the same orders — passed
/// in rather than recomputed so the gaps that size the gates and the
/// centres those gates are placed at come from one rounding of
/// `Δt_k = L·ln(k)`, not two.
///
/// Clamping per order rather than globally is what keeps the linear IR
/// usable: order 1's only neighbour is order 2, which for a 1 s 20 Hz–
/// 20 kHz sweep sits ~4816 samples away at 48 kHz, while the narrowest
/// gap in the set (order 4 → 5) is ~1551. A single global clamp would cut
/// the linear IR — the measurement's primary output — to the spacing of
/// the highest orders, which constrain nothing about it.
///
/// Because neighbouring orders `k` and `k+1` are each clamped to at most
/// their shared gap `g`, their facing half-windows sum to at most `g`, so
/// the gates cannot overlap. Errors if a gap rounds to zero or below, at
/// which point no non-overlapping gate exists at all.
fn per_order_window_lens(
    p: &SweepParams,
    offsets: &[i64],
    window_len: usize,
) -> Result<Vec<usize>> {
    let n_harmonics = offsets.len();
    let mut used = vec![window_len; n_harmonics];
    for i in 0..n_harmonics.saturating_sub(1) {
        let gap = offsets[i + 1] - offsets[i];
        if gap <= 0 {
            bail!(
                "adjacent-harmonic spacing between orders {} and {} rounds to \
                 {} samples (f1={} Hz, f2={} Hz, duration_s={}, \
                 sample_rate={}); no non-overlapping gate exists — reduce \
                 n_harmonics or increase duration_s",
                i + 1,
                i + 2,
                gap,
                p.f1_hz,
                p.f2_hz,
                p.duration_s,
                p.sample_rate
            );
        }
        let gap = gap as usize;
        used[i] = used[i].min(gap);
        used[i + 1] = used[i + 1].min(gap);
    }
    Ok(used)
}

/// Split the full deconvolution output `full` into the linear IR plus
/// `n_harmonics - 1` pre-impulse harmonic IRs.
///
/// `full` is the output of [`super::deconvolve_full`] on a recording of a
/// sweep generated by [`super::log_sweep`] on `p`. `window_len` is the gate
/// length (samples) requested for each IR. Gates for adjacent orders must
/// not overlap, or the orders cross-contaminate, so each order's gate is
/// clamped down to the sample distance to its nearest neighbouring order.
/// The lengths actually used are reported back as
/// [`DeconvolvedIrs::window_len_used`], with
/// [`DeconvolvedIrs::clamp_note`] rendering them for an operator. With
/// `n_harmonics == 1` there is no neighbour and `window_len` is used as
/// asked.
///
/// The linear IR is centred at the sweep endpoint (sample `N−1` of the
/// forward sweep). Each harmonic IR is centred at
/// `linear_centre − round(Δt_k · fs)`.
pub fn extract_irs(
    full: &[f64],
    p: &SweepParams,
    n_harmonics: usize,
    window_len: usize,
) -> Result<DeconvolvedIrs> {
    p.validate()?;
    if n_harmonics == 0 {
        bail!("n_harmonics must be ≥ 1");
    }
    if window_len == 0 {
        bail!("window_len must be ≥ 1");
    }
    let n_sweep = p.n_samples();
    if full.len() < n_sweep {
        bail!(
            "convolution output too short: got {} samples, need at least {}",
            full.len(),
            n_sweep
        );
    }
    let offsets = harmonic_offsets_samples(p, n_harmonics);
    let window_len_used = per_order_window_lens(p, &offsets, window_len)?;

    let linear_centre = n_sweep - 1;
    let linear = gate(full, linear_centre, window_len_used[0]);

    let mut harmonics = Vec::with_capacity(n_harmonics.saturating_sub(1));
    for k in 2..=(n_harmonics as u32) {
        let idx = k as usize - 1;
        let centre = linear_centre as i64 - offsets[idx];
        if centre < 0 {
            harmonics.push(HarmonicIr {
                order: k,
                samples: Vec::new(),
            });
            continue;
        }
        let samples = gate(full, centre as usize, window_len_used[idx]);
        harmonics.push(HarmonicIr { order: k, samples });
    }
    Ok(DeconvolvedIrs {
        linear,
        harmonics,
        window_len_requested: window_len,
        window_len_used,
        params: *p,
    })
}

/// Copy `len` samples of `buf` starting at signed index `start`, padding
/// with zeros wherever the request falls outside the buffer, and scale
/// sample `i` by `weight(i)`.
///
/// The single definition of "gate a buffer" in this module: both
/// [`gate`] (rectangular, centred) and [`super::gated_frequency_response`]
/// (Tukey-tapered, offset from the peak reference) are this with a
/// different weight and start.
pub(super) fn gate_weighted(
    buf: &[f64],
    start: i64,
    len: usize,
    weight: impl Fn(usize) -> f64,
) -> Vec<f64> {
    (0..len)
        .map(|i| {
            let idx = start + i as i64;
            if idx < 0 || (idx as usize) >= buf.len() {
                0.0
            } else {
                buf[idx as usize] * weight(i)
            }
        })
        .collect()
}

/// Sample index within a gate at which the gated event sits: the same
/// `len / 2` convention for the gate [`extract_irs`] cuts and for the
/// peak reference [`super::gated_frequency_response`] measures its own gate
/// start from. Both must agree or a gate offset means two different
/// things on the two sides of the module — see
/// `extracted_linear_ir_is_centred_where_gated_response_expects_it`.
pub(super) fn gate_centre_index(len: usize) -> usize {
    len / 2
}

/// Return `window_len` samples centred on `centre` within `buf`, padding
/// with zeros outside the buffer. The IR peak is placed at
/// `gate_centre_index(window_len)`.
fn gate(buf: &[f64], centre: usize, window_len: usize) -> Vec<f64> {
    let start = centre as i64 - gate_centre_index(window_len) as i64;
    gate_weighted(buf, start, window_len, |_| 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement::sweep::testkit::*;
    use crate::measurement::sweep::{deconvolve_full, inverse_sweep, log_sweep};

    #[test]
    fn identity_system_produces_unit_linear_ir_peak() {
        let p = p_default();
        let x = log_sweep(&p).unwrap();
        let xi = inverse_sweep(&p).unwrap();
        let full = deconvolve_full(&x, &xi);
        let irs = extract_irs(&full, &p, 1, 128).unwrap();
        let peak = irs
            .linear
            .iter()
            .cloned()
            .fold(0.0_f64, |m, v| m.max(v.abs()));
        // Normalisation inside inverse_sweep should bring this to 1.
        assert!(
            (peak - 1.0).abs() < 0.05,
            "identity IR peak = {peak}, expected ~1"
        );
        // Peak should be at the window centre.
        let peak_idx = irs
            .linear
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap()
            .0;
        assert_eq!(
            peak_idx, 64,
            "expected peak at window centre, got {peak_idx}"
        );
    }

    #[test]
    fn delayed_impulse_shifts_linear_ir() {
        // Model a pure delay: y(n) = x(n - d). Linear IR from Farina
        // should be a spike at (window_centre + d).
        let p = p_default();
        let x = log_sweep(&p).unwrap();
        let d = 17_usize;
        let mut y = vec![0.0_f32; x.len() + d];
        y[d..].copy_from_slice(&x);
        let xi = inverse_sweep(&p).unwrap();
        let full = deconvolve_full(&y, &xi);
        let irs = extract_irs(&full, &p, 1, 128).unwrap();
        let peak_idx = irs
            .linear
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap()
            .0;
        assert_eq!(peak_idx, 64 + d);
    }

    #[test]
    fn scaled_delay_recovers_magnitude_and_sign() {
        // A pure inverting half-gain channel y(n) = -0.5 · x(n − d). The
        // Farina-deconvolved IR should be a negative spike at
        // (window_centre + d) with peak magnitude ≈ 0.5.
        let p = p_default();
        let x = log_sweep(&p).unwrap();
        let d = 9_usize;
        let mut y = vec![0.0_f32; x.len() + d];
        for (i, &v) in x.iter().enumerate() {
            y[i + d] = -0.5 * v;
        }
        let xi = inverse_sweep(&p).unwrap();
        let full = deconvolve_full(&y, &xi);
        let irs = extract_irs(&full, &p, 1, 128).unwrap();
        let (peak_idx, peak_val) = irs
            .linear
            .iter()
            .enumerate()
            .map(|(i, v)| (i, *v))
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap();
        assert_eq!(peak_idx, 64 + d, "peak at wrong offset");
        assert!(peak_val < 0.0, "peak should be negative: {peak_val}");
        assert!(
            (peak_val.abs() - 0.5).abs() < 0.05,
            "|peak| {} should be ~0.5",
            peak_val.abs()
        );
    }

    #[test]
    fn cubic_nonlinearity_produces_third_harmonic_ir() {
        // y = a·x + b·x³ has a 3rd-harmonic component at scale |b|/4
        // relative to the fundamental's scale (a + 3b/4). The extracted
        // 3rd-harmonic IR is carried by the Farina inverse filter with a
        // frequency-dependent gain, so we don't pin the absolute ratio —
        // we just verify the 3rd-harmonic IR has meaningful energy when
        // the input is clearly nonlinear, and is essentially zero when
        // the input is linear.
        let p = p_default();
        let x = log_sweep(&p).unwrap();
        let xi = inverse_sweep(&p).unwrap();
        let window = 128;

        // Linear baseline: no 3rd-harmonic energy.
        let full_lin = deconvolve_full(&x, &xi);
        let irs_lin = extract_irs(&full_lin, &p, 3, window).unwrap();
        let lin_only_h3_peak = irs_lin
            .harmonics
            .iter()
            .find(|h| h.order == 3)
            .unwrap()
            .samples
            .iter()
            .cloned()
            .fold(0.0_f64, |m, v| m.max(v.abs()));

        // Cubic nonlinearity: substantial 3rd-harmonic energy.
        let b = 0.3_f32;
        let y: Vec<f32> = x.iter().map(|&v| v + b * v * v * v).collect();
        let full_nl = deconvolve_full(&y, &xi);
        let irs_nl = extract_irs(&full_nl, &p, 3, window).unwrap();
        let nl_h3_peak = irs_nl
            .harmonics
            .iter()
            .find(|h| h.order == 3)
            .unwrap()
            .samples
            .iter()
            .cloned()
            .fold(0.0_f64, |m, v| m.max(v.abs()));

        assert!(
            nl_h3_peak > 3.0 * lin_only_h3_peak,
            "expected nonlinear 3rd-harmonic peak ({nl_h3_peak:.5}) to clearly exceed the linear baseline ({lin_only_h3_peak:.5})"
        );
        assert!(
            nl_h3_peak > 0.001,
            "nonlinear 3rd-harmonic peak too small: {nl_h3_peak}"
        );
    }

    /// Gate-centre offsets in samples for orders `1..=n`, derived from
    /// Farina's Δt_k = L·ln(k) directly rather than through
    /// `harmonic_offsets_samples`, so the window tests below measure the
    /// implementation instead of restating it.
    fn hand_derived_offsets(p: &SweepParams, n: usize) -> Vec<i64> {
        let l = p.duration_s / (p.f2_hz / p.f1_hz).ln();
        let fs = p.sample_rate as f64;
        (1..=n as u32)
            .map(|k| (l * (k as f64).ln() * fs).round() as i64)
            .collect()
    }

    fn hand_derived_gaps(p: &SweepParams, n: usize) -> Vec<i64> {
        hand_derived_offsets(p, n)
            .windows(2)
            .map(|w| w[1] - w[0])
            .collect()
    }

    fn deconvolved_default() -> (SweepParams, Vec<f64>) {
        let p = p_default();
        let x = log_sweep(&p).unwrap();
        let xi = inverse_sweep(&p).unwrap();
        let full = deconvolve_full(&x, &xi);
        (p, full)
    }

    #[test]
    fn hand_derived_gaps_match_issue_278_table() {
        // Anchors every window test below to numbers computed off the
        // spec, not off this module: 1 s, 20 Hz–20 kHz, 48 kHz, orders 1..5
        // give L = 144.76 ms and gaps of 4816 / 2818 / 1999 / 1551 samples.
        // The 1999 and 1551 figures are issue #278's own H3→H4 and H4→H5
        // rows.
        assert_eq!(
            hand_derived_gaps(&p_default(), 5),
            vec![4816, 2818, 1999, 1551]
        );
    }

    #[test]
    fn each_order_is_clamped_to_its_own_neighbour_spacing() {
        let (p, full) = deconvolved_default();
        let gaps = hand_derived_gaps(&p, 5);
        // Order i's gate is bounded by the gap below it and the gap above
        // it — not by the narrowest gap anywhere in the set.
        let expect = vec![
            4096usize,        // order 1: only neighbour is order 2 (4816)
            gaps[1] as usize, // order 2: min(4816, 2818)
            gaps[2] as usize, // order 3: min(2818, 1999)
            gaps[3] as usize, // order 4: min(1999, 1551)
            gaps[3] as usize, // order 5: only neighbour is order 4 (1551)
        ];

        let irs = extract_irs(&full, &p, 5, 4096).unwrap();
        assert_eq!(irs.window_len_requested, 4096);
        assert_eq!(irs.window_len_used, expect);
        assert_eq!(irs.linear.len(), expect[0]);
        for h in &irs.harmonics {
            assert_eq!(
                h.samples.len(),
                expect[h.order as usize - 1],
                "order {} gate length",
                h.order
            );
        }
        for (i, w) in expect.iter().enumerate() {
            assert_eq!(irs.window_len_for(i as u32 + 1), Some(*w));
        }
        assert_eq!(irs.window_len_for(6), None);
    }

    #[test]
    fn linear_gate_is_not_cut_to_the_global_minimum_spacing() {
        // The rejected alternative: one global clamp to the narrowest gap
        // in the set. Compute it here so the test fails if the code ever
        // reverts to it — under that rule the linear IR, whose nearest
        // neighbour is ~3x further away than the narrowest gap, would lose
        // most of its length for no contamination reason.
        let (p, full) = deconvolved_default();
        let global_min = *hand_derived_gaps(&p, 5).iter().min().unwrap() as usize;
        let irs = extract_irs(&full, &p, 5, 4096).unwrap();
        assert!(
            irs.linear.len() > global_min,
            "linear gate {} was cut to the global minimum spacing {}",
            irs.linear.len(),
            global_min
        );
        assert_eq!(irs.linear.len(), 4096, "linear gate should be unclamped");
    }

    #[test]
    fn adjacent_order_gates_never_overlap() {
        let (p, full) = deconvolved_default();
        let n = 5usize;
        let irs = extract_irs(&full, &p, n, 4096).unwrap();
        let centres: Vec<i64> = hand_derived_offsets(&p, n)
            .iter()
            .map(|o| (p.n_samples() as i64 - 1) - o)
            .collect();
        // `gate` puts the centre at index window/2, so order i covers
        // [centre - w/2, centre - w/2 + w). Orders run backwards in time,
        // so order i+1's window must end at or before order i's starts.
        for i in 0..n - 1 {
            let (w_hi, w_lo) = (irs.window_len_used[i], irs.window_len_used[i + 1]);
            let start_hi = centres[i] - (w_hi / 2) as i64;
            let end_lo = centres[i + 1] - (w_lo / 2) as i64 + w_lo as i64;
            assert!(
                end_lo <= start_hi,
                "order {} gate ends at {} but order {} starts at {}",
                i + 2,
                end_lo,
                i + 1,
                start_hi
            );
        }
    }

    #[test]
    fn window_is_untouched_when_it_fits_every_gap() {
        let (p, full) = deconvolved_default();
        // 128 is under the narrowest gap (1551) for orders 1..5.
        let irs = extract_irs(&full, &p, 5, 128).unwrap();
        assert_eq!(irs.window_len_used, vec![128; 5]);
        assert_eq!(irs.linear.len(), 128);
        assert_eq!(irs.clamp_note(), None);
    }

    #[test]
    fn single_order_is_never_clamped() {
        // Guards the τ measurement in ac-daemon's `calibrate`, which asks
        // for n_harmonics=1 and a long window because half of it is the
        // largest round-trip delay it can report. With no second order
        // there is no adjacent-order constraint to clamp against.
        let (p, full) = deconvolved_default();
        let irs = extract_irs(&full, &p, 1, 8192).unwrap();
        assert_eq!(irs.window_len_used, vec![8192]);
        assert_eq!(irs.linear.len(), 8192);
        assert!(irs.harmonics.is_empty());
        assert_eq!(irs.clamp_note(), None);
    }

    #[test]
    fn collapsed_spacing_is_an_error_not_a_zero_length_gate() {
        // At 0.3 ms the orders round onto each other: offsets are
        // [0, 1, 2, 3, 3] samples, so orders 4 and 5 share a centre and no
        // non-overlapping gate exists for them at any length.
        let p = SweepParams {
            duration_s: 0.0003,
            ..p_default()
        };
        assert!(p.validate().is_ok(), "params must be otherwise valid");
        assert_eq!(hand_derived_offsets(&p, 5), vec![0, 1, 2, 3, 3]);
        let full = vec![0.0_f64; 4096];
        let err = extract_irs(&full, &p, 5, 128).unwrap_err().to_string();
        assert!(
            err.contains("orders 4 and 5"),
            "error should name the collapsed pair: {err}"
        );
        // Orders 1..4 are still separated, so this must stay an error about
        // the requested set rather than silently dropping order 5.
        assert!(extract_irs(&full, &p, 4, 128).is_ok());
    }

    #[test]
    fn clamp_note_names_every_shortened_order() {
        let (p, full) = deconvolved_default();
        let irs = extract_irs(&full, &p, 5, 4096).unwrap();
        let note = irs.clamp_note().expect("orders 2..5 are clamped at 4096");
        assert!(
            note.contains("4096"),
            "note should state the request: {note}"
        );
        for order in 2..=5 {
            assert!(
                note.contains(&format!("order {order} \u{2192}")),
                "note should name order {order}: {note}"
            );
        }
        assert!(
            !note.contains("order 1 \u{2192}"),
            "order 1 was not clamped and must not be listed: {note}"
        );
    }

    #[test]
    fn clamp_note_carries_the_gap_reason_and_its_parameters() {
        // #342: "clamped to N samples" alone gives the operator no knob to
        // turn. The note must also say *why* — the adjacent-harmonic-order
        // gap — and the sweep parameters that set it, so the operator knows
        // duration_s/f1_hz/f2_hz is what to change.
        let (p, full) = deconvolved_default();
        let irs = extract_irs(&full, &p, 5, 4096).unwrap();
        let note = irs.clamp_note().expect("orders 2..5 are clamped at 4096");
        assert!(
            note.to_lowercase().contains("gap"),
            "note should name the adjacent-harmonic-order gap as the reason: {note}"
        );
        for needle in [
            format!("f1_hz={}", p.f1_hz),
            format!("f2_hz={}", p.f2_hz),
            format!("duration_s={}", p.duration_s),
        ] {
            assert!(
                note.contains(&needle),
                "note should carry `{needle}` so the operator knows which \
                 knob widens the gap: {note}"
            );
        }
    }
}
