//! Wavefront onset in a deconvolved impulse response (#346, #378).
//!
//! The peak of `|h|` is not the arrival: on a multi-way loudspeaker it
//! sits a fixed group delay past the wavefront that actually left the
//! baffle first. Neither is a level crossing, which is shape-dependent on
//! a rising edge. What lives here is the AIC change-point pick that
//! replaced both, plus the search window it runs over and the rule string
//! that records which of them produced a given index.

/// Result of [`estimate_onset`]: the onset sample index plus the rule
/// that produced it, so a persisted onset can be told apart from a bare
/// peak read a year later (#346, acceptance criterion 4).
///
/// Reported, not used as the arrival: `MeasurementReport::ir_stats`
/// derives `delay_samples` / `arrival_s` from the magnitude peak and
/// carries this beside it as a diagnostic, bounded or not. #378's
/// contingency, triggered by its AC6 rig run (pupu, 2026-09-15): between
/// 1.000 m and 2.000 m the unbounded onset's increment missed
/// `transfer_stream`'s by 143.75 samples while the peak's missed by 8.62.
/// The bounded onset's pre-registered rig check (#346, pupu, 2026-09-16)
/// refused to conclude, and the operator kept the peak.
///
/// Pairing rule (#351): an onset-derived arrival may only be differenced
/// against a τ picked by the *same* onset rule from the *same* capture's
/// reference leg (#460) — never against a stored `calibrate` τ. A stored
/// τ is measured under a different sweep, so nothing guarantees its
/// bandlimited skirt matches this onset's; [`crate::measurement::sweep::ir_peak`]
/// is the one picker whose result may be differenced against another
/// `ir_peak` result from any capture — see that function's module doc for
/// why.
#[derive(Debug, Clone, PartialEq)]
pub struct OnsetEstimate {
    pub index: usize,
    pub rule: String,
    /// What produced `index`, as a typed value: a caller decides from
    /// this, never by parsing `rule` (#346 architect revision 2).
    pub pick: OnsetPick,
}

/// How an [`OnsetEstimate`] came about.
#[derive(Debug, Clone, PartialEq)]
pub enum OnsetPick {
    /// The picker declined; `index` is the peak, not an onset. `rule`
    /// names the case.
    Declined,
    /// The picker ran over `[window_start, peak]`.
    Picked {
        window_start: usize,
        /// Which limit set `window_start`.
        limit: WindowLimit,
        /// The pick is `window_start` itself — no split beat the null
        /// model, so the true onset may lie earlier.
        pinned: bool,
        /// The edge-following check on a clear pick in a window the
        /// causal bound started. `None` when it did not run: the bound did
        /// not set the window start (`limit` is `SearchSpan`), or the pick
        /// is pinned.
        edge_guard: Option<EdgeGuard>,
    },
}

/// Which limit set the onset search window's start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowLimit {
    /// The enforced causal bound: it is at or after the span start.
    CausalBound,
    /// [`ONSET_SEARCH_WINDOW_S`] back from the peak — either no bound was
    /// enforced, or the bound lies earlier than the span start.
    SearchSpan,
}

/// Outcome of the edge-following check (#346 architect revision 3).
///
/// With the causal bound setting the window start, the search window can
/// be short (40 samples at 2 m on the rig), and on such a window the pick
/// can follow the window start rather than the IR: moving the bound
/// earlier moves the pick earlier, and the pick sits between the bound and
/// the peak. The check re-picks over `[window_start − m, peak]`, where `m`
/// is [`EDGE_GUARD_EXTENSION_M`] of flight at the capture's rate and the
/// bound's own speed of sound. A pick the IR set does not move when the
/// window starts 5 cm earlier; a pick the window edge set does.
///
/// The check extends rather than trims the window: on a noise-free
/// deconvolution the samples before the onset are band-limited skirt, not
/// stationary noise, so cutting them changes the leading segment's
/// variance and moves a correct pick (the rejected revision-2 guard,
/// tested against in `peak.rs`).
///
/// Known limits. The check refuses some correct picks, and a pick that
/// follows the bound on a noisy capture can still pass. A pick that passes
/// lies in `[bound, peak)`, so its error is bounded on both sides: it is never later than the peak, and it is
/// earlier than the true onset by at most the bound's own error. The bound
/// comes from a taped distance, so that earlier error is at most the tape
/// uncertainty (≤ 5 cm); every other known error points late.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeGuard {
    /// The re-pick is within [`EDGE_GUARD_TOLERANCE_SAMPLES`] of the pick.
    Passed,
    /// The check failed closed. `repick: Some(r)`: the re-pick over the
    /// window started [`EDGE_GUARD_EXTENSION_M`] earlier moved to `r` —
    /// the pick follows the window edge, so the bound rather than the IR
    /// set the answer. `repick: None`: no re-pick ran, because the window
    /// could not start that much earlier (the window start is closer to
    /// sample 0 than the margin, or the margin is 0 samples), or the
    /// extended window had no variance to pick from.
    Failed { repick: Option<usize> },
}

/// How much earlier, in metres of flight, the edge-following check starts
/// its re-pick window (#346 architect revision 3). Provenance: derived —
/// the ±5 cm hand-tape error, the dominant uncertainty in the causal
/// bound's distance input. Converted to samples as
/// `round(EDGE_GUARD_EXTENSION_M / c × sample_rate)` with the enforced
/// bound's own speed of sound (14 samples at 96 kHz and 343 m/s). A
/// distance rather than a sample count because what it stands in for is a
/// tape error.
pub const EDGE_GUARD_EXTENSION_M: f64 = 0.05;

/// How far, in samples, the edge-following re-pick may move before
/// [`EdgeGuard::Failed`]. Allows for tie resolution between neighbouring
/// splits — provenance: assumed (#346 architect revision 2).
pub const EDGE_GUARD_TOLERANCE_SAMPLES: usize = 1;

/// The earliest sample an onset may occupy, or why no such limit applies
/// (#460). The onset search's lower window edge when enforced.
///
/// Built by [`crate::measurement::report::MeasurementReport::ir_stats`]
/// from a same-capture reference latency and an operator-entered distance.
/// A stored τ never feeds it: it re-picks by a multiple of the FireWire SYT
/// interval on every device enumeration (#461), which is larger than the
/// bound's margin.
#[derive(Debug, Clone, PartialEq)]
pub enum CausalBound {
    /// Pure flight time (reference τ + distance / c) as a sample index,
    /// with the inputs it was built from so a read-out can print them
    /// rather than re-derive them (#460 UX).
    Enforced { index: usize, inputs: BoundInputs },
    /// The bound could not be built. Names the missing input(s) (#460 AC3).
    Unavailable(MissingBoundInput),
}

/// What an enforced [`CausalBound`] was computed from.
#[derive(Debug, Clone, PartialEq)]
pub struct BoundInputs {
    /// Same-capture reference latency, seconds: τ of the reference pair.
    pub reference_tau_s: f64,
    /// Operator-entered source-to-receiver distance, metres.
    pub distance_m: f64,
    /// Speed of sound the distance was converted with, m/s.
    pub speed_of_sound_m_s: f64,
    /// Temperature that set `speed_of_sound_m_s`. `None` means none was
    /// configured and the default was assumed.
    pub temperature_c: Option<f64>,
}

/// Which input(s) an unavailable [`CausalBound`] lacked.
#[derive(Debug, Clone, PartialEq)]
pub enum MissingBoundInput {
    /// A valid same-capture reference exists; no distance was given.
    Distance,
    /// A distance was given; no valid same-capture reference. `reason` is
    /// the reference reading's own unavailable reason.
    ReferenceLatency { reason: String },
    /// Neither. `reference_reason` as above.
    Both { reference_reason: String },
}

impl CausalBound {
    /// The enforced index, or `None` when the bound is unavailable.
    pub fn min_admissible_index(&self) -> Option<usize> {
        match self {
            CausalBound::Enforced { index, .. } => Some(*index),
            CausalBound::Unavailable(_) => None,
        }
    }
}

impl MissingBoundInput {
    /// The rule-string clause naming what is missing. Stable text: the
    /// CLI read-out and tests match on it.
    pub fn clause(&self) -> &'static str {
        match self {
            MissingBoundInput::Distance => "distance not given",
            MissingBoundInput::ReferenceLatency { .. } => "reference latency unavailable",
            MissingBoundInput::Both { .. } => "no distance, reference latency unavailable",
        }
    }

    /// The reference reading's unavailable reason, when that input is
    /// among the missing ones.
    pub fn reference_reason(&self) -> Option<&str> {
        match self {
            MissingBoundInput::Distance => None,
            MissingBoundInput::ReferenceLatency { reason } => Some(reason),
            MissingBoundInput::Both { reference_reason } => Some(reference_reason),
        }
    }
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
/// [`crate::measurement::report::MeasurementReport::ir_stats`]) and the
/// capture's `causal_bound`.
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
/// `causal_bound`, when [`CausalBound::Enforced`], is the earliest sample
/// the capture's own geometry allows an onset to occupy: same-capture
/// reference τ plus distance over c, as a sample index (#460). It is the
/// search window's lower limit, so a bandlimited pre-ring that a
/// floor-relative scan would return non-causally is outside the picker's
/// reach entirely rather than being clamped after the fact. A bound at or
/// after the peak leaves nothing to search and is a named decline. When it
/// is [`CausalBound::Unavailable`] the bound cannot be enforced and `rule`
/// names the missing input, so a reader can tell a geometry-checked onset
/// from a best-effort one.
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
///   sound can have arrived. Only an enforced `causal_bound` can reject
///   that, and it exists only when both a valid same-capture reference and a
///   recorded distance are present. Where geometry is known this is the tighter
///   limit and the case does not arise.
/// - *A window with no pre-onset noise in it is uninformative.* If the
///   causal bound truncates the window past the true onset, the window
///   is close to homogeneous and no split is much better than any
///   other. The null model's AIC penalty catches the exactly-tied case
///   and reports the window start (flagged in `rule`), but a near-tie
///   still resolves to some index, and on a short bounded window that
///   index can follow the window start. Under an enforced bound the
///   [`EdgeGuard`] re-picks with the window start moved
///   [`EDGE_GUARD_EXTENSION_M`] earlier and reports [`EdgeGuard::Failed`]
///   (also named in `rule`) when that index moves, so a caller can refuse
///   it. The reported index stays the pick, never a sample below the
///   bound. Nothing here can recover an onset that geometry says is
///   inadmissible.
pub fn estimate_onset(
    ir: &[f64],
    peak_index: usize,
    sample_rate_hz: u32,
    gate_floor: f64,
    causal_bound: &CausalBound,
) -> OnsetEstimate {
    let declined = |reason: &str| OnsetEstimate {
        index: peak_index.min(ir.len().saturating_sub(1)),
        rule: format!("onset picker declined ({reason}) — index is the peak, not an onset"),
        pick: OnsetPick::Declined,
    };
    if ir.is_empty() {
        return OnsetEstimate {
            index: peak_index,
            rule: "onset picker declined (search window shorter than 2 samples) — index is \
                   the peak, not an onset"
                .to_string(),
            pick: OnsetPick::Declined,
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
    let bound = causal_bound.min_admissible_index();
    // A bound at or after the peak leaves no admissible sample before it.
    // Named here rather than left to collapse the window to one sample and
    // decline as "search window shorter than 2 samples", which reads as a
    // gate or peak-position fault when it is the bound's own inputs
    // (distance, reference latency) that put it there (#460).
    if bound.is_some_and(|b| b >= end) {
        return declined("causal bound at or after the peak");
    }
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

    let Some(best_k) = aic_change_point(window) else {
        return declined("zero variance in the search window");
    };
    let onset = window_start + best_k;
    if onset >= end {
        return declined("no change point earlier than the peak in the window");
    }

    let window_ms = ONSET_SEARCH_WINDOW_S * 1000.0;
    let binding = if bound.is_some_and(|b| b >= span_start) {
        WindowLimit::CausalBound
    } else {
        WindowLimit::SearchSpan
    };
    let limit = match causal_bound {
        // Which limit actually set the window start is the operator's
        // next question when a pick sits on it, so the clause names the
        // binding one rather than only whether geometry was known.
        CausalBound::Enforced { .. } if binding == WindowLimit::CausalBound => {
            "causal bound enforced".to_string()
        }
        CausalBound::Enforced { index: b, .. } => {
            format!("causal bound enforced at sample {b}, search span is the tighter limit")
        }
        // Names the missing input (#460 AC3): "geometry not known" named
        // neither input, so it told the operator nothing they could fix.
        CausalBound::Unavailable(missing) => format!("no causal bound ({})", missing.clause()),
    };
    let mut rule = format!(
        "AIC change-point pick over a {window_ms:.1} ms window; window start at sample \
         {window_start}, {limit}"
    );
    let pinned = onset == window_start;
    if pinned {
        rule.push_str("; pick landed on the window start — the true onset may lie earlier");
    }
    // Bounded path only — a window the bound started. The unbounded
    // pick's behaviour and rule text stay exactly as #378 left them; a bound earlier than the span leaves a full-length
    // window, which is not the short-window case this checks for.
    //
    // The extended window reaches below the causal bound on purpose: a
    // re-pick that lands there has moved, so the guard fails. `index`
    // stays the pick either way.
    let edge_guard = (binding == WindowLimit::CausalBound && !pinned).then(|| {
        let margin = match causal_bound {
            CausalBound::Enforced { inputs, .. } => {
                edge_guard_margin_samples(inputs.speed_of_sound_m_s, sample_rate_hz)
            }
            CausalBound::Unavailable(_) => 0,
        };
        let extended_start = match window_start.checked_sub(margin) {
            Some(s) if margin > 0 => s,
            _ => return EdgeGuard::Failed { repick: None },
        };
        let repick = aic_change_point(&ir[extended_start..=end]).map(|k| extended_start + k);
        match repick {
            Some(r) if r.abs_diff(onset) <= EDGE_GUARD_TOLERANCE_SAMPLES => EdgeGuard::Passed,
            repick => EdgeGuard::Failed { repick },
        }
    });
    if let Some(EdgeGuard::Failed { repick }) = edge_guard {
        let cm = EDGE_GUARD_EXTENSION_M * 100.0;
        match repick {
            Some(r) => rule.push_str(&format!(
                "; re-pick with the window start {cm:.0} cm earlier went to sample {r} — the \
                 pick follows the window edge"
            )),
            None => rule.push_str(&format!(
                "; no re-pick — the window cannot start {cm:.0} cm earlier, so the pick could \
                 not be checked"
            )),
        }
    }
    OnsetEstimate {
        index: onset,
        rule,
        pick: OnsetPick::Picked {
            window_start,
            limit: binding,
            pinned,
            edge_guard,
        },
    }
}

/// [`EDGE_GUARD_EXTENSION_M`] as a sample count at `sample_rate_hz`, using
/// the enforced bound's `speed_of_sound_m_s`. `0` when the conversion has
/// no finite, positive result, which the guard treats as "cannot extend".
fn edge_guard_margin_samples(speed_of_sound_m_s: f64, sample_rate_hz: u32) -> usize {
    let m = (EDGE_GUARD_EXTENSION_M / speed_of_sound_m_s * sample_rate_hz as f64).round();
    if m.is_finite() && m > 0.0 {
        m as usize
    } else {
        0
    }
}

/// The AIC change point of `window`, as the count of samples in its
/// leading segment: `0` when no split beats the null model by Akaike's
/// penalty. `None` when the window has no finite, nonzero variance.
pub(super) fn aic_change_point(window: &[f64]) -> Option<usize> {
    let n = window.len();
    let mut prefix_sum = vec![0.0_f64; n + 1];
    let mut prefix_sq = vec![0.0_f64; n + 1];
    for (i, &v) in window.iter().enumerate() {
        prefix_sum[i + 1] = prefix_sum[i] + v;
        prefix_sq[i + 1] = prefix_sq[i] + v * v;
    }
    let total_var = segment_variance(&prefix_sum, &prefix_sq, 0, n);
    if total_var <= 0.0 || !total_var.is_finite() {
        return None;
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
    Some(if best_aic + AIC_PENALTY < null_aic {
        best_k
    } else {
        0
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unbounded search: neither a reference nor a distance.
    fn unbounded() -> CausalBound {
        CausalBound::Unavailable(MissingBoundInput::Both {
            reference_reason: "no reference configured".into(),
        })
    }

    /// A bound enforced at `index`. The inputs are nominal:
    /// `estimate_onset` reads the index and, for the edge guard's margin,
    /// the speed of sound (14 samples at 96 kHz).
    fn bounded(index: usize) -> CausalBound {
        CausalBound::Enforced {
            index,
            inputs: BoundInputs {
                reference_tau_s: 0.0,
                distance_m: 1.0,
                speed_of_sound_m_s: 343.0,
                temperature_c: None,
            },
        }
    }

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
        let est = estimate_onset(&ir, peak_index, 48_000, floor, &unbounded());
        assert_eq!(est.index, onset_true);
        assert_ne!(
            est.index, peak_index,
            "must not fall back to argmax|h| — #346"
        );
    }

    /// #378's window against the unbounded AIC pick: a bandlimited
    /// pre-ring sits below the sample pure flight time allows, with the
    /// real wavefront above it. The pre-ring here peaks at 2 % of the peak,
    /// so a 10 %-of-peak threshold is causal on this fixture; #346 AC3's
    /// threshold comparison is
    /// `bounded_onset_is_causal_where_a_ten_percent_threshold_is_not`.
    /// An unbounded search lands on the pre-ring (computed here directly,
    /// per "test against the rejected implementation"); supplying the
    /// causal bound as the search window's lower limit puts the non-causal candidate outside
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

        let unbounded = estimate_onset(&ir, peak_index, 48_000, sigma_n, &unbounded());
        assert!(
            unbounded.index < bound,
            "test setup: unbounded pick must land non-causally, got {}",
            unbounded.index
        );

        let bounded = estimate_onset(&ir, peak_index, 48_000, sigma_n, &bounded(bound));
        assert!(
            bounded.index >= bound,
            "causal bound must exclude the non-causal pre-ring, got {}",
            bounded.index
        );
        assert_ne!(bounded.index, unbounded.index);
        assert!(bounded.rule.contains("causal bound enforced"));
    }

    /// #346 acceptance criterion 3, against the rejected rule it names: a
    /// band-limited pre-ring above 10 % of the peak, entirely before the
    /// sample pure flight time allows. The 10 %-of-peak threshold is
    /// computed inline (it is not an estimator this crate returns) and
    /// asserted non-causal as test setup, so a fixture on which the
    /// threshold happens to be causal fails here instead of passing.
    #[test]
    fn bounded_onset_is_causal_where_a_ten_percent_threshold_is_not() {
        let sigma_n = 1e-4;
        let peak_index = 300usize;
        let bound = 280usize; // earliest sample pure flight time allows
        let wavefront = 285usize;
        let mut ir = onset_noise(512, sigma_n, 0xABCD_EF01);
        for (i, v) in ir.iter_mut().enumerate() {
            if (245..bound).contains(&i) {
                *v += 0.15 * ((i - 245) as f64 * 0.7).sin();
            } else if (wavefront..peak_index).contains(&i) {
                *v += 0.3 * (i - wavefront + 1) as f64 / 16.0;
            }
        }
        ir[peak_index] = 1.0;

        let threshold = 0.1 * ir[peak_index].abs();
        let naive = ir
            .iter()
            .position(|v| v.abs() > threshold)
            .expect("test setup: something crosses 10 % of peak");
        assert!(
            naive < bound,
            "test setup: the 10 % crossing must be non-causal, got {naive}"
        );

        let est = estimate_onset(&ir, peak_index, 48_000, sigma_n, &bounded(bound));
        assert!(
            est.index >= bound,
            "bounded onset went non-causal: {}",
            est.index
        );
        assert_ne!(est.index, naive);
        assert_ne!(est.index, peak_index, "must not fall back to argmax|h|");
    }

    /// #346 acceptance criterion 4: the rule string must say whether a
    /// causal bound was actually enforced, so a persisted number can be
    /// told apart from a best-effort one.
    #[test]
    fn onset_rule_states_whether_a_causal_bound_was_enforced() {
        let sigma_n = 1e-4;
        let (ir, _, peak_index) = drr_fixture(1.0, sigma_n, 0.05);
        let with_bound = estimate_onset(&ir, peak_index, 96_000, sigma_n, &bounded(1500));
        let without_bound = estimate_onset(&ir, peak_index, 96_000, sigma_n, &unbounded());
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

        let spanned = estimate_onset(&ir, peak_index, 96_000, sigma_n, &unbounded());
        assert!(spanned
            .rule
            .contains(&format!("window start at sample {span_start}")));
        assert!(spanned
            .rule
            .contains("AIC change-point pick over a 10.0 ms window"));

        // A causal bound looser than the search span does not move the
        // window start, and the rule must not imply that it did.
        let loose = estimate_onset(&ir, peak_index, 96_000, sigma_n, &bounded(10));
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
            let p = estimate_onset(&ir, peak_index, 96_000, sigma_n, &unbounded()).index;
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
        let base = estimate_onset(&ir, peak_index, 96_000, sigma_n, &unbounded()).index;
        let base_rejected = rejected_level_crossing_rule(&ir, peak_index, sigma_n);

        let mut moved = false;
        for scale in [0.001, 0.5, 2.0, 1000.0] {
            let scaled: Vec<f64> = ir.iter().map(|v| v * scale).collect();
            let est = estimate_onset(&scaled, peak_index, 96_000, sigma_n * scale, &unbounded());
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
        let reference = estimate_onset(&ir, peak_index, 96_000, sigma_n, &unbounded()).index;

        // Span varies 4.8x (480 → 2304 samples); every value brackets the
        // onset, which sits 110 samples before the peak.
        for sr in [48_000u32, 96_000, 192_000, 230_400] {
            let span = (ONSET_SEARCH_WINDOW_S * sr as f64).round() as usize;
            assert!(
                span > peak_index - onset_true,
                "test setup: span {span} must bracket the onset"
            );
            let est = estimate_onset(&ir, peak_index, sr, sigma_n, &unbounded());
            assert_eq!(
                est.index, reference,
                "pick must not depend on the window span (sample rate {sr}, span {span})"
            );
        }

        // Gate floor varies 1000x; every value leaves the gate open,
        // because the picker takes no threshold from it.
        for floor in [sigma_n * 0.01, sigma_n, sigma_n * 10.0] {
            let est = estimate_onset(&ir, peak_index, 96_000, floor, &unbounded());
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
                ir: vec![],
                peak_index: 0,
                floor: 1e-6,
                bound: None,
            },
            Case {
                reason: "causal bound at or after the peak",
                ir: {
                    let mut v = onset_noise(200, 1e-3, 5);
                    v[150] = 1.0;
                    v
                },
                peak_index: 150,
                floor: 1e-4,
                bound: Some(150),
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
            let est = estimate_onset(
                &ir,
                peak_index,
                48_000,
                floor,
                &bound.map_or_else(unbounded, bounded),
            );
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
            let est = estimate_onset(&ir, peak_index, 96_000, sigma_n, &bounded(bound));
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
        let est = estimate_onset(&ir, 150, 48_000, floor, &unbounded());
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
        let est = estimate_onset(&ir, 150, 48_000, floor, &unbounded());
        assert_eq!(est.index, 150);
        assert!(
            est.rule.contains("onset picker declined")
                && est.rule.contains("— index is the peak, not an onset"),
            "rule string missing the breakdown admission: {}",
            est.rule
        );
    }

    /// #460: a causal bound at or after the peak leaves the answer at the
    /// peak, and the decline must name the bound rather than the collapsed
    /// window it produced. The pre-#460 text ("search window shorter than
    /// 2 samples") pointed the operator at gate length and peak position,
    /// when what put the bound there is the distance and reference latency.
    /// A real distance input makes this reachable: on the fake loopback the
    /// reference τ equals the peak offset, so any distance > 0 lands here.
    #[test]
    fn onset_rule_names_a_bound_at_the_peak_not_a_collapsed_window() {
        let sigma_n = 1e-4;
        let (ir, _, peak_index) = drr_fixture(1.0, sigma_n, 0.05);
        for bound in [peak_index, peak_index + 40] {
            let est = estimate_onset(&ir, peak_index, 96_000, sigma_n, &bounded(bound));
            assert_eq!(est.index, peak_index);
            assert!(
                est.rule.contains("causal bound at or after the peak"),
                "bound {bound}: decline does not name the bound: {}",
                est.rule
            );
            assert!(
                !est.rule.contains("search window shorter than 2 samples"),
                "bound {bound}: the pre-#460 collapsed-window text survives: {}",
                est.rule
            );
        }
    }

    /// #460 AC3: an unavailable bound names which input was missing, and
    /// the three cases read differently. "geometry not known" named neither.
    #[test]
    fn onset_rule_names_the_missing_bound_input() {
        let sigma_n = 1e-4;
        let (ir, _, peak_index) = drr_fixture(1.0, sigma_n, 0.05);
        let cases = [
            (
                MissingBoundInput::Distance,
                "no causal bound (distance not given)",
            ),
            (
                MissingBoundInput::ReferenceLatency {
                    reason: "no reference configured".into(),
                },
                "no causal bound (reference latency unavailable)",
            ),
            (
                MissingBoundInput::Both {
                    reference_reason: "no reference configured".into(),
                },
                "no causal bound (no distance, reference latency unavailable)",
            ),
        ];
        let mut rules = Vec::new();
        for (missing, want) in cases {
            let est = estimate_onset(
                &ir,
                peak_index,
                96_000,
                sigma_n,
                &CausalBound::Unavailable(missing),
            );
            assert!(est.rule.contains(want), "{want:?} not in {:?}", est.rule);
            assert!(
                !est.rule.contains("geometry not known"),
                "pre-#460 clause survives: {}",
                est.rule
            );
            rules.push(est.rule);
        }
        assert_ne!(rules[0], rules[1]);
        assert_ne!(rules[1], rules[2]);
        assert_ne!(rules[0], rules[2]);
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
        let est = estimate_onset(&ir, peak_index, 96_000, sigma_n, &bounded(bound));
        assert_eq!(est.index, bound);
        assert!(
            est.rule
                .contains("pick landed on the window start — the true onset may lie earlier"),
            "pinned pick not flagged: {}",
            est.rule
        );
        // A pinned pick already has its own onset standing, and the edge
        // guard does not run on it: a pick on the window start is already flagged.
        assert_eq!(
            est.pick,
            OnsetPick::Picked {
                window_start: bound,
                limit: WindowLimit::CausalBound,
                pinned: true,
                edge_guard: None,
            }
        );
    }

    /// The pick the guard is not applied to, computed inline from the same
    /// AIC core: `window_start + k` over `[window_start, peak]`.
    fn unguarded_pick(ir: &[f64], window_start: usize, peak_index: usize) -> usize {
        window_start + aic_change_point(&ir[window_start..=peak_index]).unwrap()
    }

    /// #346 architect revision 3, edge-following guard, firing direction.
    /// The bound sits inside the wavefront's rise, after the true onset, so
    /// the window holds no pre-onset noise — the homogeneous case the
    /// picker's doc names. Starting the window 5 cm earlier moves the
    /// re-pick. Tested against the rejected implementation: the
    /// unguarded bounded pick is computed inline and is clear of the window
    /// start under a binding bound, i.e. it passes every other onset
    /// condition and *would* have stood as `Unscored`.
    #[test]
    fn edge_guard_fires_when_the_window_starts_after_the_true_onset() {
        let sigma_n = 1e-4;
        let (ir, onset_true, peak_index) = drr_fixture(1.0, sigma_n, 0.05);
        let bound = onset_true + 50;

        let unguarded = unguarded_pick(&ir, bound, peak_index);
        assert!(
            unguarded > bound && unguarded < peak_index,
            "test setup: the unguarded pick must be clear of the window start, got {unguarded}"
        );

        let est = estimate_onset(&ir, peak_index, 96_000, sigma_n, &bounded(bound));
        assert_eq!(
            est.index, unguarded,
            "the guard reports, it does not move the pick"
        );
        match est.pick {
            OnsetPick::Picked {
                window_start,
                limit,
                pinned,
                edge_guard,
            } => {
                assert_eq!(window_start, bound);
                assert_eq!(limit, WindowLimit::CausalBound);
                assert!(!pinned);
                match edge_guard {
                    Some(EdgeGuard::Failed { repick: Some(r) }) => assert!(
                        r.abs_diff(unguarded) > EDGE_GUARD_TOLERANCE_SAMPLES,
                        "a failed guard must name a re-pick that moved: {r} vs {unguarded}"
                    ),
                    other => panic!("guard must fire on a homogeneous window, got {other:?}"),
                }
            }
            OnsetPick::Declined => panic!("picker declined: {}", est.rule),
        }
        assert!(
            est.rule.contains("the pick follows the window edge"),
            "rule must say the pick follows the window edge: {}",
            est.rule
        );
    }

    /// Edge-following guard, non-firing direction: a clean change point
    /// with pre-onset noise inside the bounded window does not move when
    /// the window starts 5 cm (14 samples at 96 kHz) earlier, adding more
    /// of that noise to the leading segment.
    #[test]
    fn edge_guard_passes_a_clean_step_with_pre_onset_noise() {
        let sigma_n = 1e-4;
        let floor = sigma_n;
        let onset_true = 400usize;
        let peak_index = 440usize;
        let bound = 380usize;
        let mut ir = onset_noise(1024, sigma_n, 0x5151_2020);
        for v in ir.iter_mut().take(peak_index).skip(onset_true) {
            *v += 0.3;
        }
        ir[peak_index] = 1.0;

        let est = estimate_onset(&ir, peak_index, 96_000, floor, &bounded(bound));
        assert_eq!(est.index, onset_true);
        assert_eq!(
            est.pick,
            OnsetPick::Picked {
                window_start: bound,
                limit: WindowLimit::CausalBound,
                pinned: false,
                edge_guard: Some(EdgeGuard::Passed),
            }
        );
        assert!(!est.rule.contains("window edge"), "{}", est.rule);
    }

    /// The guard fails closed, with no re-pick, when the window cannot
    /// start 5 cm earlier: here the bound sits 10 samples into the IR and
    /// the margin is 14. The fixture is the passing clean step above,
    /// shifted so its bound is that close to sample 0 — the same pick
    /// that passes there is refused here, because nothing checked it.
    #[test]
    fn edge_guard_fails_closed_when_the_window_cannot_be_extended() {
        let sigma_n = 1e-4;
        let onset_true = 30usize;
        let peak_index = 70usize;
        let bound = 10usize;
        assert!(bound < edge_guard_margin_samples(343.0, 96_000));
        let mut ir = onset_noise(1024, sigma_n, 0x5151_2020);
        for v in ir.iter_mut().take(peak_index).skip(onset_true) {
            *v += 0.3;
        }
        ir[peak_index] = 1.0;

        let est = estimate_onset(&ir, peak_index, 96_000, sigma_n, &bounded(bound));
        assert_eq!(est.index, onset_true);
        assert_eq!(
            est.pick,
            OnsetPick::Picked {
                window_start: bound,
                limit: WindowLimit::CausalBound,
                pinned: false,
                edge_guard: Some(EdgeGuard::Failed { repick: None }),
            }
        );
        assert!(
            est.rule
                .contains("no re-pick — the window cannot start 5 cm earlier"),
            "{}",
            est.rule
        );
        assert!(!est.rule.contains("window edge"), "{}", est.rule);
    }

    /// The margin is the 5 cm tape error at the bound's own speed of
    /// sound, not a fixed sample count.
    #[test]
    fn edge_guard_margin_is_five_cm_at_the_bounds_speed_of_sound() {
        assert_eq!(edge_guard_margin_samples(343.0, 96_000), 14);
        assert_eq!(edge_guard_margin_samples(343.0, 48_000), 7);
        assert_eq!(edge_guard_margin_samples(200.0, 96_000), 24);
        assert_eq!(edge_guard_margin_samples(343.0, 0), 0);
        assert_eq!(edge_guard_margin_samples(0.0, 96_000), 0);
    }

    /// The guard is bounded-path only: an unbounded pick carries no guard
    /// verdict, and its rule text is what #378 left.
    #[test]
    fn edge_guard_does_not_run_without_a_causal_bound() {
        let sigma_n = 1e-4;
        let (ir, _, peak_index) = drr_fixture(1.0, sigma_n, 0.05);
        let est = estimate_onset(&ir, peak_index, 96_000, sigma_n, &unbounded());
        match est.pick {
            OnsetPick::Picked {
                limit, edge_guard, ..
            } => {
                assert_eq!(limit, WindowLimit::SearchSpan);
                assert_eq!(edge_guard, None);
            }
            OnsetPick::Declined => panic!("picker declined: {}", est.rule),
        }
        assert!(!est.rule.contains("window edge"), "{}", est.rule);
    }

    /// The binding-limit value is typed, and agrees with the rule text the
    /// CLI already prints: a bound earlier than the span start is enforced
    /// but does not set the window.
    #[test]
    fn onset_pick_reports_which_limit_set_the_window() {
        let sigma_n = 1e-4;
        let (ir, _, peak_index) = drr_fixture(1.0, sigma_n, 0.05);
        let loose = estimate_onset(&ir, peak_index, 96_000, sigma_n, &bounded(10));
        assert!(matches!(
            loose.pick,
            OnsetPick::Picked {
                limit: WindowLimit::SearchSpan,
                ..
            }
        ));
        assert!(loose.rule.contains("search span is the tighter limit"));
        let tight = estimate_onset(&ir, peak_index, 96_000, sigma_n, &bounded(1500));
        assert!(matches!(
            tight.pick,
            OnsetPick::Picked {
                limit: WindowLimit::CausalBound,
                ..
            }
        ));
        let declined = estimate_onset(&ir, peak_index, 96_000, sigma_n, &bounded(peak_index));
        assert_eq!(declined.pick, OnsetPick::Declined);
    }
}
