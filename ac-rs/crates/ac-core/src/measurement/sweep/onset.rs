//! Wavefront onset in a deconvolved impulse response (#346, #378).
//!
//! The peak of `|h|` is not the arrival: on a multi-way loudspeaker it
//! sits a fixed group delay past the wavefront that actually left the
//! baffle first. Neither is a level crossing, which is shape-dependent on
//! a rising edge. What lives here is the AIC change-point pick that
//! replaced both, plus the search window it runs over and the rule string
//! that records which of them produced a given index.

/// Result of [`estimate_onset`]: the onset sample index plus the rule
/// that produced it, so a persisted arrival can be told apart from a
/// bare peak read a year later (#346, acceptance criterion 4).
#[derive(Debug, Clone, PartialEq)]
pub struct OnsetEstimate {
    pub index: usize,
    pub rule: String,
}

/// Length of the onset picker's search window, in seconds.
///
/// The window ends at the magnitude peak; its start is the later of this
/// span back from the peak and the capture's own causal bound. It exists
/// only so the picker has a bounded window when no geometry is known —
/// where a causal bound is available that bound is always the tighter
/// limit and this constant never binds.
///
/// Derivation: the onset-to-peak distances measured on the rig are 110.4
/// samples at 1.000 m and 92.2 at 3.000 m, n = 12 each at 96 kHz
/// (`work/rig/rig-2026-08-23-onset-353-results.md`) — 1.15 ms and
/// 0.96 ms. 10 ms is 8.7x the larger of the two, so the window brackets
/// the onset with headroom and the picker's leading (pre-onset) segment
/// is never starved by its trailing one: at the measured geometry the
/// pre-onset part of the window is roughly 8x the post-onset part.
/// Expressed as a duration rather than a sample count because the
/// quantity it has to exceed is a flight-time difference; the sample
/// count follows from the capture's own rate.
pub const ONSET_SEARCH_WINDOW_S: f64 = 10.0e-3;

/// Sample variance of `xs[from..to]`, from prefix sums. Zero for
/// segments shorter than two samples.
fn segment_variance(prefix_sum: &[f64], prefix_sq: &[f64], from: usize, to: usize) -> f64 {
    if to <= from + 1 {
        return 0.0;
    }
    let n = (to - from) as f64;
    let s = prefix_sum[to] - prefix_sum[from];
    let q = prefix_sq[to] - prefix_sq[from];
    (q / n - (s / n) * (s / n)).max(0.0)
}

/// Estimate the wavefront onset in a deconvolved impulse response `ir`,
/// given its magnitude peak at `peak_index`, the capture's
/// `sample_rate_hz`, a `gate_floor` (the pre-impulse median floor, see
/// [`crate::measurement::report::MeasurementReport::ir_stats`]) and,
/// where geometry is known, `min_admissible_index`.
///
/// Not `argmax|h|` (issue #346): on a multi-way loudspeaker the sample of
/// largest magnitude sits at a fixed group-delay offset past the
/// wavefront that actually left the baffle first — LF and crossover
/// phase pull the peak later, by an amount that does not shrink with
/// distance (it cancels in an increment between two positions but
/// persists in the absolute, per the issue's rig table).
///
/// Not a level crossing either (issue #378). A threshold referenced to
/// the pre-impulse floor cannot see energy arriving *after* the
/// pre-impulse window, and the rig showed the resulting onset moving
/// 18.2 samples toward the peak between 1.000 m and 3.000 m while the
/// pre-impulse SNR moved 0.83 dB — 2.8 samples' worth at the measured
/// within-position slope. Expressing the same threshold relative to the
/// peak buys back only those same 2.8 samples with the sign flipped, so
/// the residual is not a threshold-level error at all: at a constant
/// level re peak the IR reaches that level 110.4 samples before the peak
/// at 1 m and 92.2 at 3 m. It is the edge's shape, and a level crossing
/// on a rising edge is a shape-dependent estimator by construction.
///
/// Rule: an AIC change-point pick (Maeda 1985, the standard
/// single-parameter onset picker; literature, not a standard — it is
/// deliberately absent from [`crate::measurement::report::StandardsCitation`]) over the search
/// window. For each candidate split `k` the window is cut into a leading
/// and a trailing segment and
///
/// ```text
/// AIC(k) = n_lead * ln var(lead) + n_trail * ln var(trail)
/// ```
///
/// is minimised; the onset is the first sample of the trailing segment.
/// Scaling the whole IR by `c` adds `(n_lead + n_trail) * ln c²`, which
/// is constant in `k`, so the pick is *exactly* invariant to a uniform
/// rescale — the direct level dropping with distance cannot move it.
/// There is no threshold and no margin constant in the rule.
///
/// `min_admissible_index`, when supplied, is the earliest sample the
/// measurement's own known geometry allows an onset to occupy (pure
/// flight time converted to a sample index). It is the search window's
/// lower limit, so a bandlimited pre-ring that a floor-relative scan
/// would return non-causally is outside the picker's reach entirely
/// rather than being clamped after the fact. When it is `None` the bound
/// cannot be enforced (no geometry known for this capture) and `rule`
/// says so, so a reader can tell a geometry-checked onset from a
/// best-effort one.
///
/// Breakdown (#378 acceptance criterion 3, extending #353's): unlike the
/// backward walk the picker cannot fail to move, so the degenerate cases
/// are named explicitly and gated. On any of them the returned index is
/// `peak_index` — today's answer, never earlier and never non-causal —
/// and `rule` names which case fired. `gate_floor` is #377's
/// contamination-robust median floor, retained but demoted from the
/// threshold's input to this gate: it decides only whether the window
/// holds anything at all, never where the onset is.
///
/// Two limits, stated so they are not discovered as surprises:
///
/// - *Without geometry the pre-ring is unguarded.* The picker keys on
///   where the IR's variance changes, not on amplitude, so on a
///   band-limited deconvolution it reads the leading skirt of the main
///   lobe — earlier than the old level crossing did, and earlier than
///   sound can have arrived. Only `min_admissible_index` can reject
///   that, and it exists only when both a measured τ and a recorded
///   distance are present. Where geometry is known this is the tighter
///   limit and the case does not arise.
/// - *A window with no pre-onset noise in it is uninformative.* If the
///   causal bound truncates the window past the true onset, the window
///   is close to homogeneous and no split is much better than any
///   other. The null model's AIC penalty catches the exactly-tied case
///   and reports the window start (flagged in `rule`), but a near-tie
///   still resolves to some index. Nothing here can recover an onset
///   that geometry says is inadmissible.
pub fn estimate_onset(
    ir: &[f64],
    peak_index: usize,
    sample_rate_hz: u32,
    gate_floor: f64,
    min_admissible_index: Option<usize>,
) -> OnsetEstimate {
    let declined = |reason: &str| OnsetEstimate {
        index: peak_index.min(ir.len().saturating_sub(1)),
        rule: format!("onset picker declined ({reason}) — index is the peak, not an onset"),
    };
    if ir.is_empty() {
        return OnsetEstimate {
            index: peak_index,
            rule: "onset picker declined (search window shorter than 2 samples) — index is \
                   the peak, not an onset"
                .to_string(),
        };
    }
    let end = peak_index.min(ir.len() - 1);
    if end == 0 {
        return declined("peak at sample 0");
    }

    let span = if sample_rate_hz == 0 {
        end
    } else {
        (ONSET_SEARCH_WINDOW_S * sample_rate_hz as f64)
            .round()
            .max(0.0) as usize
    };
    let span_start = end.saturating_sub(span);
    let bound = min_admissible_index.map(|b| b.min(end));
    let window_start = bound.unwrap_or(0).max(span_start);

    let window = &ir[window_start..=end];
    if window.len() < 2 {
        return declined("search window shorter than 2 samples");
    }
    // The validity gate (#377's median floor, demoted): with nothing in
    // the window above the pre-impulse floor there is no wavefront to
    // find, only floor. Compared bare — no margin, no dB — because the
    // floor no longer sets an operating point, it only answers whether
    // the window holds anything at all.
    if !window.iter().any(|v| v.abs() > gate_floor) {
        return declined("nothing in the search window above the pre-impulse floor");
    }

    let n = window.len();
    let mut prefix_sum = vec![0.0_f64; n + 1];
    let mut prefix_sq = vec![0.0_f64; n + 1];
    for (i, &v) in window.iter().enumerate() {
        prefix_sum[i + 1] = prefix_sum[i] + v;
        prefix_sq[i + 1] = prefix_sq[i] + v * v;
    }
    let total_var = segment_variance(&prefix_sum, &prefix_sq, 0, n);
    if total_var <= 0.0 || !total_var.is_finite() {
        return declined("zero variance in the search window");
    }
    // A segment that is exactly constant would otherwise give ln(0).
    // The guard is a fraction of the window's own variance, not an
    // absolute epsilon, so it scales with the IR exactly as the two
    // segment variances do — the pick stays invariant to a uniform
    // rescale even on the samples where it binds.
    let var_floor = total_var * 1e-12;
    // A segment of fewer than two samples has no variance to estimate,
    // so it carries no likelihood information and contributes nothing —
    // the same convention that lets `k = 0` (an empty leading segment)
    // stand for "no change point inside this window". Without it a lone
    // sample's exact-zero variance would be clamped and then rewarded,
    // and every window ending in an isolated spike would pick its own
    // last sample.
    let term = |from: usize, to: usize| {
        if to <= from + 1 {
            return 0.0;
        }
        let n = (to - from) as f64;
        n * segment_variance(&prefix_sum, &prefix_sq, from, to)
            .max(var_floor)
            .ln()
    };

    // `k` is the count of samples in the leading segment. `k = 0` is the
    // null model — one variance over the whole window, i.e. no change
    // point inside it — and it is scored against the best split rather
    // than competing with it, because the two models do not have the
    // same number of parameters.
    let null_aic = term(0, n);
    let mut best_k = 0usize;
    let mut best_aic = f64::INFINITY;
    for k in 1..n {
        let aic = term(0, k) + term(k, n);
        if aic < best_aic {
            best_aic = aic;
            best_k = k;
        }
    }
    // Akaike's penalty, not a tuned threshold: the split model carries
    // two parameters the null does not — a second variance and the
    // change point's own location — and AIC charges 2 per parameter. A
    // homogeneous window is a near-tie between the null and whatever
    // split the noise happens to favour, so without this the picker
    // returns an arbitrary index there with full confidence. Below the
    // penalty the window supports no change point at all and the answer
    // is its own start, flagged as such in `rule`.
    const SPLIT_MODEL_EXTRA_PARAMETERS: f64 = 2.0;
    const AIC_PENALTY: f64 = 2.0 * SPLIT_MODEL_EXTRA_PARAMETERS;
    let onset = if best_aic + AIC_PENALTY < null_aic {
        window_start + best_k
    } else {
        window_start
    };
    if onset >= end {
        return declined("no change point earlier than the peak in the window");
    }

    let window_ms = ONSET_SEARCH_WINDOW_S * 1000.0;
    let limit = match bound {
        // Which limit actually set the window start is the operator's
        // next question when a pick sits on it, so the clause names the
        // binding one rather than only whether geometry was known.
        Some(b) if b >= span_start => "causal bound enforced".to_string(),
        Some(b) => format!("causal bound enforced at sample {b}, search span is the tighter limit"),
        None => "no causal bound (geometry not known for this capture)".to_string(),
    };
    let mut rule = format!(
        "AIC change-point pick over a {window_ms:.1} ms window; window start at sample \
         {window_start}, {limit}"
    );
    if onset == window_start {
        rule.push_str("; pick landed on the window start — the true onset may lie earlier");
    }
    OnsetEstimate { index: onset, rule }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── estimate_onset (#346, #378) ───────────────────────────────────

    /// Deterministic pseudo-noise, uniform in ±√3·`sigma` so its
    /// variance is exactly `sigma²`. A fixed LCG rather than a real RNG
    /// so every assertion below is reproducible byte for byte.
    fn onset_noise(n: usize, sigma: f64, seed: u64) -> Vec<f64> {
        let mut s = seed | 1;
        (0..n)
            .map(|_| {
                s = s
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let u = ((s >> 11) as f64) / ((1u64 << 53) as f64);
                (u - 0.5) * 2.0 * 3.0f64.sqrt() * sigma
            })
            .collect()
    }

    /// The rule #378 rejects, computed inline so the tests below measure
    /// it rather than asserting "the new answer is closer to truth":
    /// a threshold `ONSET_FLOOR_MARGIN_DB` (12 dB) above the pre-impulse
    /// floor, walked backward from the peak. This is the whole of the
    /// pre-#378 estimator; nothing in `sweep.rs` implements it any more.
    fn rejected_level_crossing_rule(ir: &[f64], peak_index: usize, floor: f64) -> usize {
        let threshold = floor * 10f64.powf(12.0 / 20.0);
        let mut onset = peak_index;
        while onset > 0 && ir[onset - 1].abs() > threshold {
            onset -= 1;
        }
        onset
    }

    /// A capture whose direct-to-reverberant ratio is a parameter and
    /// whose true onset, noise floor and geometry are not: a fixed noise
    /// floor, a direct wavefront of amplitude `direct` rising from a
    /// fixed `onset_true` to a fixed `peak_index`, and an exponentially
    /// decaying reverberant tail of fixed level after the peak.
    ///
    /// DRR is varied through the direct's level rather than the tail's,
    /// deliberately. The tail arrives *after* the peak, so it cannot move
    /// a backward walk that starts at the peak — a fixture that varied
    /// only the tail would leave the rejected rule motionless and would
    /// therefore be unable to fail. Varying the direct is also the rig's
    /// own mechanism between 1.000 m and 3.000 m: the direct drops with
    /// 1/r while room noise at the capsule and the reverberant level do
    /// not.
    fn drr_fixture(direct: f64, sigma_n: f64, tail: f64) -> (Vec<f64>, usize, usize) {
        const N: usize = 4096;
        const ONSET_TRUE: usize = 1800;
        const PEAK_INDEX: usize = 1910;
        let mut ir = onset_noise(N, sigma_n, 0x9E37_79B9);
        let rise = (PEAK_INDEX - ONSET_TRUE) as f64;
        for (i, v) in ir.iter_mut().enumerate() {
            if (ONSET_TRUE..=PEAK_INDEX).contains(&i) {
                let x = (i - ONSET_TRUE) as f64 / rise;
                *v += direct * x * x;
            } else if i > PEAK_INDEX {
                let t = (i - PEAK_INDEX) as f64;
                let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
                *v += sign * tail * (-t / 400.0).exp();
            }
        }
        (ir, ONSET_TRUE, PEAK_INDEX)
    }

    /// Test against the rejected implementation (#346 acceptance
    /// criterion 2): a synthetic IR with realistic multi-way group delay
    /// — sustained energy from the true onset through to a magnitude
    /// peak that sits well after it, standing in for LF/crossover group
    /// delay pulling the largest sample later than the wavefront. The
    /// estimator must not fall back to `argmax|h|`.
    #[test]
    fn onset_estimate_rejects_the_peak_the_way_346_names_as_wrong() {
        let floor = 0.001;
        let onset_true = 200usize;
        let peak_index = 260usize;
        let mut ir = onset_noise(512, floor, 0x1234_5678);
        for v in ir.iter_mut().take(peak_index + 1).skip(onset_true) {
            *v += 0.3;
        }
        ir[peak_index] = 1.0; // the actual magnitude maximum
        let est = estimate_onset(&ir, peak_index, 48_000, floor, None);
        assert_eq!(est.index, onset_true);
        assert_ne!(
            est.index, peak_index,
            "must not fall back to argmax|h| — #346"
        );
    }

    /// #346 acceptance criterion 3, restated for #378's window: a
    /// bandlimited pre-ring sits below the sample pure flight time
    /// allows, with the real wavefront above it. An unbounded search
    /// lands on the pre-ring (computed here directly, per "test against
    /// the rejected implementation"); supplying the causal bound as the
    /// search window's lower limit puts the non-causal candidate outside
    /// the picker's reach entirely rather than clamping it after the
    /// fact, which is what changed under #378.
    #[test]
    fn onset_estimate_rejects_bandlimited_preringing_below_the_causal_bound() {
        let sigma_n = 1e-4;
        let peak_index = 300usize;
        let bound = 280usize; // earliest sample pure flight time allows
        let wavefront = 285usize;
        let mut ir = onset_noise(512, sigma_n, 0xABCD_EF01);
        for (i, v) in ir.iter_mut().enumerate() {
            if (245..wavefront).contains(&i) {
                *v += 0.02 * ((i - 245) as f64 * 0.7).sin();
            } else if (wavefront..=peak_index).contains(&i) {
                *v += 0.3 * (i - wavefront + 1) as f64 / 16.0;
            }
        }
        ir[peak_index] = 1.0;

        let unbounded = estimate_onset(&ir, peak_index, 48_000, sigma_n, None);
        assert!(
            unbounded.index < bound,
            "test setup: unbounded pick must land non-causally, got {}",
            unbounded.index
        );

        let bounded = estimate_onset(&ir, peak_index, 48_000, sigma_n, Some(bound));
        assert!(
            bounded.index >= bound,
            "causal bound must exclude the non-causal pre-ring, got {}",
            bounded.index
        );
        assert_ne!(bounded.index, unbounded.index);
        assert!(bounded.rule.contains("causal bound enforced"));
    }

    /// #346 acceptance criterion 4: the rule string must say whether a
    /// causal bound was actually enforced, so a persisted number can be
    /// told apart from a best-effort one.
    #[test]
    fn onset_rule_states_whether_a_causal_bound_was_enforced() {
        let sigma_n = 1e-4;
        let (ir, _, peak_index) = drr_fixture(1.0, sigma_n, 0.05);
        let with_bound = estimate_onset(&ir, peak_index, 96_000, sigma_n, Some(1500));
        let without_bound = estimate_onset(&ir, peak_index, 96_000, sigma_n, None);
        assert_ne!(with_bound.rule, without_bound.rule);
        assert!(with_bound.rule.contains("causal bound enforced"));
        assert!(without_bound.rule.contains("no causal bound"));
        assert!(with_bound.rule.contains("window start at sample 1500"));
    }

    /// #378: the rule names the search window and where it started, and
    /// says which limit set it — the causal bound or the search span.
    #[test]
    fn onset_rule_names_the_window_and_which_limit_started_it() {
        let sigma_n = 1e-4;
        let (ir, _, peak_index) = drr_fixture(1.0, sigma_n, 0.05);
        let span_start = peak_index - (ONSET_SEARCH_WINDOW_S * 96_000.0).round() as usize;

        let spanned = estimate_onset(&ir, peak_index, 96_000, sigma_n, None);
        assert!(spanned
            .rule
            .contains(&format!("window start at sample {span_start}")));
        assert!(spanned
            .rule
            .contains("AIC change-point pick over a 10.0 ms window"));

        // A causal bound looser than the search span does not move the
        // window start, and the rule must not imply that it did.
        let loose = estimate_onset(&ir, peak_index, 96_000, sigma_n, Some(10));
        assert!(loose
            .rule
            .contains(&format!("window start at sample {span_start}")));
        assert!(
            loose.rule.contains("search span is the tighter limit"),
            "a non-binding causal bound must not read as the window's start: {}",
            loose.rule
        );
    }

    /// #378 acceptance criterion 2, the load-bearing one. The rejected
    /// level-crossing rule is computed inline on the same captures. As
    /// DRR falls, *its* answer must move toward the peak — the direction
    /// the rig measured (110.4 → 92.2 samples onset-to-peak between
    /// 1.000 m and 3.000 m) — while the picker's must move less, on
    /// every capture and across the sweep as a whole.
    ///
    /// The direction is asserted, not just a difference: if the fixture
    /// ever stops reproducing the defect, this test fails rather than
    /// passing for the wrong reason.
    #[test]
    fn onset_pick_tracks_drr_less_than_the_rejected_level_rule_does() {
        let sigma_n = 1e-4;
        let tail = 0.05;
        let directs = [1.0, 0.3, 0.1, 0.03, 0.01];

        let mut rejected = Vec::new();
        let mut picked = Vec::new();
        for &d in &directs {
            let (ir, onset_true, peak_index) = drr_fixture(d, sigma_n, tail);
            let r = rejected_level_crossing_rule(&ir, peak_index, sigma_n);
            let p = estimate_onset(&ir, peak_index, 96_000, sigma_n, None).index;
            assert!(
                r >= onset_true && p >= onset_true,
                "test setup: neither rule may read before the true onset \
                 (direct {d}): rejected {r}, picked {p}, true {onset_true}"
            );
            rejected.push(r - onset_true);
            picked.push(p - onset_true);
        }

        // Direction: the rejected rule's answer moves monotonically
        // toward the peak as the direct level drops.
        for w in rejected.windows(2) {
            assert!(
                w[1] > w[0],
                "rejected rule must move toward the peak as DRR falls: {rejected:?}"
            );
        }
        let rejected_spread = rejected.last().unwrap() - rejected.first().unwrap();
        let picked_spread = picked.iter().max().unwrap() - picked.iter().min().unwrap();
        assert!(
            picked_spread < rejected_spread,
            "picker must track DRR less than the rejected rule: picker spread \
             {picked_spread} ({picked:?}), rejected spread {rejected_spread} \
             ({rejected:?})"
        );
        for (i, (&r, &p)) in rejected.iter().zip(picked.iter()).enumerate() {
            assert!(
                p <= r,
                "capture {i} (direct {}): picker residual {p} must not exceed \
                 the rejected rule's {r}",
                directs[i]
            );
        }
        // At the highest DRR the two rules agree — that is the case #378
        // is not about. The claim is about the low-DRR end, where the
        // rejected rule has walked away from the onset and the picker has
        // not.
        assert!(
            picked.last().unwrap() < rejected.last().unwrap(),
            "at the lowest DRR the picker must be strictly closer to the \
             true onset: picker {picked:?}, rejected {rejected:?}"
        );
    }

    /// #378: the property the pick is chosen for. Scaling the whole IR by
    /// any constant adds a term constant in `k` to every AIC value, so the
    /// pick is *exactly* invariant — not approximately. The rejected rule
    /// is computed inline on the same scaled captures with the floor held
    /// at the unscaled value (which is what a pre-impulse-referenced
    /// threshold does when the direct level drops and the room noise does
    /// not) and must move.
    #[test]
    fn onset_pick_is_exactly_invariant_to_a_uniform_rescale() {
        let sigma_n = 1e-4;
        let (ir, _, peak_index) = drr_fixture(1.0, sigma_n, 0.05);
        let base = estimate_onset(&ir, peak_index, 96_000, sigma_n, None).index;
        let base_rejected = rejected_level_crossing_rule(&ir, peak_index, sigma_n);

        let mut moved = false;
        for scale in [0.001, 0.5, 2.0, 1000.0] {
            let scaled: Vec<f64> = ir.iter().map(|v| v * scale).collect();
            let est = estimate_onset(&scaled, peak_index, 96_000, sigma_n * scale, None);
            assert_eq!(
                est.index, base,
                "pick must be exactly invariant to a uniform rescale by {scale}"
            );
            if rejected_level_crossing_rule(&scaled, peak_index, sigma_n) != base_rejected {
                moved = true;
            }
        }
        assert!(
            moved,
            "test setup: the rejected rule must move under a rescale its floor \
             does not follow, or this test proves nothing"
        );
    }

    /// #378 acceptance criterion 4. The rule introduces exactly one new
    /// constant — [`ONSET_SEARCH_WINDOW_S`] — and no gate level: the
    /// validity gate compares against the caller's pre-impulse floor
    /// bare, with no margin. This pins that the two are independent:
    /// the pick does not change with the window span (over the range that
    /// brackets the onset, exercised through the sample rate the span is
    /// derived from), and it does not change with the gate floor (over
    /// the range that passes the gate).
    #[test]
    fn onset_window_span_and_validity_gate_are_independent() {
        let sigma_n = 1e-4;
        let (ir, onset_true, peak_index) = drr_fixture(0.3, sigma_n, 0.05);
        let reference = estimate_onset(&ir, peak_index, 96_000, sigma_n, None).index;

        // Span varies 4.8x (480 → 2304 samples); every value brackets the
        // onset, which sits 110 samples before the peak.
        for sr in [48_000u32, 96_000, 192_000, 230_400] {
            let span = (ONSET_SEARCH_WINDOW_S * sr as f64).round() as usize;
            assert!(
                span > peak_index - onset_true,
                "test setup: span {span} must bracket the onset"
            );
            let est = estimate_onset(&ir, peak_index, sr, sigma_n, None);
            assert_eq!(
                est.index, reference,
                "pick must not depend on the window span (sample rate {sr}, span {span})"
            );
        }

        // Gate floor varies 1000x; every value leaves the gate open,
        // because the picker takes no threshold from it.
        for floor in [sigma_n * 0.01, sigma_n, sigma_n * 10.0] {
            let est = estimate_onset(&ir, peak_index, 96_000, floor, None);
            assert_eq!(
                est.index, reference,
                "pick must not depend on the validity gate's floor ({floor})"
            );
        }
    }

    /// #378 acceptance criterion 3: the failure direction is bounded.
    /// Every named degenerate case returns `peak_index` — today's answer,
    /// never earlier and never non-causal — and says which case fired.
    /// The list is the whole of the picker's decline surface; a case that
    /// is not here cannot make the picker decline.
    #[test]
    fn onset_picker_declines_to_the_peak_on_every_degenerate_case() {
        struct Case {
            reason: &'static str,
            ir: Vec<f64>,
            peak_index: usize,
            floor: f64,
            bound: Option<usize>,
        }
        let cases = vec![
            Case {
                reason: "peak at sample 0",
                ir: vec![1.0, 0.1, 0.1],
                peak_index: 0,
                floor: 1e-6,
                bound: None,
            },
            Case {
                reason: "search window shorter than 2 samples",
                ir: vec![0.01, 1.0, 0.01],
                peak_index: 1,
                floor: 1e-6,
                bound: Some(1),
            },
            Case {
                reason: "zero variance in the search window",
                ir: vec![0.5; 64],
                peak_index: 40,
                floor: 0.1,
                bound: None,
            },
            Case {
                reason: "nothing in the search window above the pre-impulse floor",
                ir: onset_noise(200, 1e-3, 7),
                peak_index: 150,
                floor: 1.0,
                bound: None,
            },
            Case {
                reason: "no change point earlier than the peak in the window",
                ir: {
                    let mut v = onset_noise(200, 1e-3, 11);
                    v[150] = 1.0;
                    v
                },
                peak_index: 150,
                floor: 1e-4,
                bound: None,
            },
        ];
        for Case {
            reason,
            ir,
            peak_index,
            floor,
            bound,
        } in cases
        {
            let est = estimate_onset(&ir, peak_index, 48_000, floor, bound);
            assert_eq!(
                est.index, peak_index,
                "{reason}: breakdown must degrade to the peak, not earlier"
            );
            assert!(
                est.rule.contains("onset picker declined")
                    && est.rule.contains(reason)
                    && est.rule.contains("index is the peak, not an onset"),
                "{reason}: rule string does not name the case: {}",
                est.rule
            );
        }
    }

    /// #378: the pick can only ever land inside the admissible window, so
    /// a bounded capture can never return a non-causal answer — the
    /// property the pre-#378 clamp had to enforce after the fact.
    #[test]
    fn onset_pick_never_lands_below_the_causal_bound() {
        let sigma_n = 1e-4;
        let (ir, _, peak_index) = drr_fixture(1.0, sigma_n, 0.05);
        for bound in [1500usize, 1750, 1850, 1900] {
            let est = estimate_onset(&ir, peak_index, 96_000, sigma_n, Some(bound));
            assert!(
                est.index >= bound,
                "pick {} fell below the causal bound {bound}",
                est.index
            );
        }
    }

    #[test]
    fn onset_estimate_stays_at_peak_when_nothing_precedes_it_above_floor() {
        // No change point behind the peak: the earliest admissible onset
        // is the peak itself, and the picker must say so rather than
        // returning a confident index it did not find.
        let floor = 0.01;
        let mut ir = vec![floor; 200];
        ir[150] = 1.0;
        let est = estimate_onset(&ir, 150, 48_000, floor, None);
        assert_eq!(est.index, 150);
        assert!(est.rule.contains("onset picker declined"));
    }

    /// QA (PR #377), carried forward to #378's picker: the breakdown
    /// admission is the load-bearing addition per the UX design comment
    /// ("has to reach the line the operator reads, or the failure stays
    /// silent where it costs something"). Pins the exact substring
    /// `short_onset_rule` (ac-cli) matches on, so a typo in either place
    /// breaks a test instead of silently dropping the warning at the
    /// terminal.
    #[test]
    fn onset_rule_names_the_breakdown_when_the_picker_declines() {
        let floor = 1.0; // nothing in the window clears it
        let mut ir = onset_noise(200, 1e-3, 3);
        ir[150] = 0.5;
        let est = estimate_onset(&ir, 150, 48_000, floor, None);
        assert_eq!(est.index, 150);
        assert!(
            est.rule.contains("onset picker declined")
                && est.rule.contains("— index is the peak, not an onset"),
            "rule string missing the breakdown admission: {}",
            est.rule
        );
    }

    /// QA (PR #377), restated for #378: a causal bound sitting at the
    /// peak also leaves the answer at the peak, and the decline text must
    /// name the window that collapsed rather than implying the capture
    /// carried nothing above the floor.
    #[test]
    fn onset_rule_names_the_collapsed_window_not_a_missing_signal() {
        let sigma_n = 1e-4;
        let (ir, _, peak_index) = drr_fixture(1.0, sigma_n, 0.05);
        let est = estimate_onset(&ir, peak_index, 96_000, sigma_n, Some(peak_index));
        assert_eq!(est.index, peak_index);
        assert!(
            est.rule.contains("search window shorter than 2 samples"),
            "collapsed-window decline mislabeled: {}",
            est.rule
        );
    }

    /// #378 / UX: a pick sitting on the window start is a stable,
    /// repeatable, possibly wrong number, and the rule must say so — the
    /// operator's question is whether the answer came from the signal or
    /// from the bracket. Reached here by a causal bound that leaves the
    /// picker two admissible samples: no split beats the null model over
    /// a window that short, so the answer is the window start and the
    /// true onset may lie earlier.
    #[test]
    fn onset_rule_flags_a_pick_pinned_to_the_window_start() {
        let sigma_n = 1e-4;
        let (ir, _, peak_index) = drr_fixture(1.0, sigma_n, 0.05);
        let bound = peak_index - 1;
        let est = estimate_onset(&ir, peak_index, 96_000, sigma_n, Some(bound));
        assert_eq!(est.index, bound);
        assert!(
            est.rule
                .contains("pick landed on the window start — the true onset may lie earlier"),
            "pinned pick not flagged: {}",
            est.rule
        );
    }
}
