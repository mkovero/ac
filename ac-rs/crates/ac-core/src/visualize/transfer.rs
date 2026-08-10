//! H1 transfer function estimator via Welch averaging.
//!
//! Direct port of `ac/transfer.py`.  Returns `freqs`, `magnitude_db`,
//! `phase_deg`, `coherence`, `delay_samples`, and `delay_ms`.

use std::f64::consts::PI;

use realfft::num_complex::Complex;
use realfft::RealFftPlanner;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TransferResult {
    pub freqs: Vec<f64>,
    pub magnitude_db: Vec<f64>,
    pub phase_deg: Vec<f64>,
    pub coherence: Vec<f64>,
    /// Complex H(ω) — real part. Parallel to `freqs`. `unified.md`
    /// Phase 3 — needed by Tier 2 views that consume H directly
    /// (Nyquist locus, IR via IFFT, group-delay-from-complex).
    /// Existing magnitude_db / phase_deg are derived from this same
    /// h1 complex value so the three views are guaranteed consistent.
    pub re: Vec<f64>,
    /// Complex H(ω) — imaginary part. Parallel to `re`.
    pub im: Vec<f64>,
    pub delay_samples: i64,
    pub delay_ms: f64,
    /// Reference-channel linear amplitude spectrum, parallel to `freqs` —
    /// `sqrt(Gxx)` normalized to the same peak-amplitude convention as
    /// `visualize::spectrum::spectrum_only` (handoff: transfer-frame-v2
    /// M0). A full-scale on-bin sine reads ≈1.0 here, matching the
    /// monitor path, so the two are cross-comparable (I-C). Uncalibrated
    /// — voltage cal / mic curve are applied by the caller, same as
    /// `magnitude_db` today.
    pub ref_amp: Vec<f64>,
    /// Measurement-channel linear amplitude spectrum, parallel to `freqs`
    /// — `sqrt(Gyy)`, same normalization and calibration state as
    /// `ref_amp`.
    pub meas_amp: Vec<f64>,
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn hann_window(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f64 / (n - 1) as f64).cos()))
        .collect()
}

/// Apply Hann window to `seg` and return the complex spectrum (n/2+1 bins).
fn fft_windowed(
    seg: &[f64],
    window: &[f64],
    planner: &mut RealFftPlanner<f64>,
) -> Vec<Complex<f64>> {
    let n = seg.len();
    let fft = planner.plan_fft_forward(n);
    let mut buf: Vec<f64> = seg
        .iter()
        .zip(window.iter())
        .map(|(&s, &w)| s * w)
        .collect();
    let mut out = fft.make_output_vec();
    fft.process(&mut buf, &mut out).ok();
    out
}

/// Welch joint estimate: returns `(Gxx, Gyy, Gxy)` accumulated from a single
/// per-segment FFT pair. Computing the three quantities together halves the
/// FFT count vs calling separate `welch_psd(x) + welch_psd(y) + welch_csd(x,y)`
/// because each segment's FFTs are reused across all three accumulators.
fn welch_all(
    x: &[f64],
    y: &[f64],
    nperseg: usize,
    noverlap: usize,
    window: &[f64],
    planner: &mut RealFftPlanner<f64>,
) -> (Vec<f64>, Vec<f64>, Vec<Complex<f64>>) {
    let nfft = nperseg / 2 + 1;
    let step = nperseg - noverlap;
    let mut gxx = vec![0.0_f64; nfft];
    let mut gyy = vec![0.0_f64; nfft];
    let mut gxy = vec![Complex::new(0.0, 0.0); nfft];
    let mut n_seg = 0usize;
    let mut pos = 0;
    let len = x.len().min(y.len());
    while pos + nperseg <= len {
        let fx = fft_windowed(&x[pos..pos + nperseg], window, planner);
        let fy = fft_windowed(&y[pos..pos + nperseg], window, planner);
        for k in 0..nfft {
            let cx = fx[k];
            let cy = fy[k];
            gxx[k] += cx.norm_sqr();
            gyy[k] += cy.norm_sqr();
            gxy[k] += cx.conj() * cy;
        }
        n_seg += 1;
        pos += step;
    }
    if n_seg > 0 {
        let inv = 1.0 / n_seg as f64;
        for k in 0..nfft {
            gxx[k] *= inv;
            gyy[k] *= inv;
            gxy[k] *= inv;
        }
    }
    (gxx, gyy, gxy)
}

/// Peak-to-median ratio the strongest causal peak must reach for a lag to be
/// *admitted* at all (#227, #246). Admission only — selection carries no
/// noise term of its own; see point 6.
///
/// For two uncorrelated signals the global maximum of |ρ| over `L` lags sits
/// at roughly `sqrt(2·ln 2L) / 0.6745 ≈ 7×` the median — near-independent of
/// capture length, because peak and median both scale as `1/√N`. Measured
/// over 40 independent uncorrelated pairs at this capture length the worst
/// case was 7.73.
///
/// This constant used to be paired with `MIN_PROMINENCE =
/// NOISE_FLOOR_PROMINENCE / DIRECT_PEAK_FRACTION` (= 24), which gated
/// admission and, by construction, guaranteed the earliest-peak rule 6 dB of
/// reach. **That derivation was sound given what was known**; the coupling
/// really does remove the degenerate zone it describes, and the zone is real.
/// The mistake was the statistic it rested on, not the logic — so the
/// reasoning below replaces it rather than merely deleting it. A future
/// correction should have to beat the data, not re-derive from theory:
///
/// 1. **12 is a full-scan correction.** It is the ceiling of
///    `max|ρ| / median|ρ|` over ~96000 lags — the right correction for
///    admission, which does scan everything.
/// 2. **Selection does not scan.** It chooses among the ~33 peaks already in
///    the candidate list, so reusing the full-scan correction there is
///    over-strict by roughly a factor of two — and that over-strictness is
///    what collapses the window. With the old
///    `max(DIRECT_PEAK_FRACTION x peak, NOISE_FLOOR_PROMINENCE x median)`
///    floor the median term binds below prominence 24, narrowing the window
///    available to the earliest-peak rule to `20·log10(prominence / 12)` dB —
///    zero at the floor. Rig session 3's 3.000 m sessions sit at per-session
///    median prominence 13.3–19.1, i.e. 0.9–4.1 dB of window — and the old
///    gate of 24 refused all 8 of them outright, where the floor of 12 keeps
///    8/8. That is the change: measure the band instead of refusing it.
/// 3. **The full-scan maximum does not govern harm, and does not converge.**
///    Measured silence ceilings: 3.33 / 5.00 / 6.75 over 19 / 66 / 85 silent
///    attempts, plus session 2's 7.73 over 40 pairs. It is a maximum of a
///    noise distribution; it grows with sampling, and every future dataset
///    will exceed it. Designing the gate against it is designing against a
///    number that moves.
/// 4. **The early window governs harm.** To do damage a ripple must clear the
///    cut *and* land earlier than the arrival, so only lags below the arrival
///    qualify — 8.9% of the causal candidate cloud — and the noise ceiling
///    restricted to that window is **4.32**, against admission 12's selection
///    cut of 6.0× median. Measured conjunction rate: **0 in 49 attempts**
///    across five captures, including 24 at a near-wall position.
/// 5. **Why that zero is evidence, not absence of it.** Candidate lists hold
///    everything within 12 dB of the peak, capped at 32 *by rank*, so an
///    unreported early candidate is weaker than the weakest reported —
///    1.0–1.35× median in these captures, nowhere near the cut. The data
///    could have shown a violation and did not.
/// 6. **The interlock, so it is not re-invented as a third constant.**
///    Admission at prominence `P` places the `DIRECT_PEAK_FRACTION x peak`
///    cut at `P/2 × median`. Selection needs no noise term because admission
///    has already put the window above the noise — *provided admission stays
///    at 12*. Raising it is therefore not a free safety improvement: at 16
///    the worst 3 m position needs ~18 s to lock, so #239's persistent-refusal
///    advice fires first and tells the operator to move a correctly placed
///    microphone. See #247.
///
/// Not a correctness test, and recorded here so it is not proposed a third
/// time: **agreement across attempts**. It never fires in silence (0/2), so
/// it is a sound noise filter, but at 3 m a competing reflection sits 93
/// samples from the direct arrival — inside the ±120-sample tolerance — and
/// it fired 8/8 sessions while correct in 2. Agreement measures whether the
/// room is stable, not which peak is the arrival.
///
/// Numbers and replays: `audit/rig-session-3/` — `silence-ceiling.md` is the
/// argument, `early_ripple.py` and `floor_rule.py` are the scoring.
///
/// Public because it decides something outside this crate: how many attempts a
/// correct-but-distant position needs before it locks, and therefore whether
/// `ac-scene`'s persistent-refusal advice fires first. `ac-scene::fault` reads
/// it in `the_admission_constant_leaves_room_before_the_advice_fires`, so
/// changing the number here fails a test there rather than silently moving an
/// operator instruction (#247). Not a tuning knob for callers — nothing may
/// gate on it, see `ZMQ.md` on `delay_prominence`.
pub const NOISE_FLOOR_PROMINENCE: f64 = 12.0;

/// Fraction of the strongest correlation peak that an *earlier* peak must
/// reach to be taken as the direct arrival instead (#227).
///
/// In a live room a reflection can exceed the direct sound, so the global
/// maximum is the wrong pick. 0.5 means "within 6 dB of the strongest peak",
/// which admits a direct sound losing to its own reverberation — the
/// reverberant fixture's direct arrival sits at 0.69 of the reflection that
/// beats it.
///
/// This is the *whole* selection cut: there is no `max(..., N x median)`
/// term beside it, so the window is a fixed 6 dB at prominence 13 as well as
/// at 30. What keeps noise out of it is admission — clearing
/// [`NOISE_FLOOR_PROMINENCE`] puts this cut at `prominence/2 × median`, above
/// the 4.32 early-window noise ceiling. The two constants are interlocked in
/// that direction only; see [`NOISE_FLOOR_PROMINENCE`] points 2 and 6, and
/// `estimate_delay`.
const DIRECT_PEAK_FRACTION: f64 = 0.5;

/// Range, as a fraction of the strongest peak, over which competing
/// correlation peaks are reported in [`DelayEstimate::candidates`].
///
/// 12 dB, deliberately wider than [`DIRECT_PEAK_FRACTION`]'s 6 dB. Reporting
/// only what the current fraction accepts would make the fraction
/// unfalsifiable from captures: the arrivals that say whether 6 dB is too
/// generous are precisely the ones it currently rejects. `handoff-rig-
/// session-2.md` Run C asks for this range by name.
const CANDIDATE_CAPTURE_FRACTION: f64 = 0.251_188_6; // 10^(-12/20)

/// Most candidates reported *by rank*. Three further lags are added
/// unconditionally, so a list may hold `MAX_CANDIDATES + 3`.
///
/// The **strongest** are kept, then reported in lag order. Keeping the
/// earliest instead is the obvious reading of "the direct arrival is first"
/// and it is wrong: at the low SNR where a capture matters most, the 12 dB
/// floor admits thousands of noise ripples, and the earliest of those fill
/// the budget long before any real arrival — leaving a candidate list that
/// contains no arrivals at all. Ranking by strength keeps the arrivals,
/// which outrank the ripple by construction.
///
/// Rank alone is not enough either. Rig session 2 found that at 3 m the
/// direct arrival is weaker than 32 peaks of the reverberant cluster, so the
/// list kept the cluster and dropped the arrival: **in every position-3
/// session the accepted lag was absent from its own evidence**, and replaying
/// the accept rule offline returned a different lag than the daemon chose at
/// every constant. Rank-by-strength *plus* unconditional inclusion of the
/// decision-relevant lags — the accepted one, the strongest causal peak, and
/// the strongest non-causal peak — is the shape that survives both failures.
/// See [`ensure_candidate`], which inserts only what the cut dropped, so the
/// three never appear twice.
const MAX_CANDIDATES: usize = 32;

/// One competing peak in the cross-correlation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DelayCandidate {
    /// Lag in samples, signed the same way as [`DelayEstimate::lag`].
    pub lag: i64,
    /// Normalized correlation magnitude |ρ| at that lag.
    pub value: f64,
}

/// A delay estimate together with the evidence behind it.
///
/// The non-`lag` fields exist so the estimator's thresholds can be set from
/// recorded captures rather than from another physical session — see
/// `handoff-rig-session-2.md` Run C, which calls these the artifacts that
/// cannot be reconstructed afterwards. They are diagnostics: nothing in the
/// measurement path may branch on them.
#[derive(Debug, Clone, PartialEq)]
pub struct DelayEstimate {
    /// The chosen lag, or `None` if no peak qualified.
    pub lag: Option<i64>,
    /// Peak-to-median ratio of the normalized cross-correlation — the
    /// quantity both thresholds are expressed in. Reported whether or not
    /// the estimate was accepted, because a refusal's prominence is what
    /// says how far short it fell.
    ///
    /// This is the estimator's single empirical constant made observable.
    /// [`NOISE_FLOOR_PROMINENCE`] is set 1.55x above a ripple ceiling
    /// measured on synthetic noise; only captures from a real acoustic path
    /// can say whether that is comfortable or lucky, and publishing this
    /// turns any session — including one that refuses at every position —
    /// into the distribution needed to set it from data.
    ///
    /// `0.0` when the correlation could not be formed at all (a silent leg).
    pub prominence: f64,
    /// Lag of the strongest **causal** peak — the answer a global-maximum
    /// rule would give over the range this estimator is allowed to select
    /// from. Differs from [`Self::lag`] exactly when the earliest-peak rule
    /// moved the estimate off a reflection, so the pair is the direct measure
    /// of how often that rule fires in a real room.
    ///
    /// Every threshold is measured against this, not against the strongest
    /// peak overall: see [`Self::noncausal_peak_lag`].
    pub peak_lag: i64,
    /// |ρ| at [`Self::peak_lag`].
    pub peak_value: f64,
    /// Lag of the strongest peak among the **negative** lags, where nothing
    /// physical can arrive — and `0.0` in [`Self::noncausal_peak_value`] when
    /// the search range holds no negative lags.
    ///
    /// Not selectable, and no threshold is measured against it. It is here
    /// because a peak at a negative lag is a real observation about the
    /// capture, and one that has cost this project a session before: #216's
    /// ring skew put every session 0.2 s negative, and a stimulus onset
    /// inside the correlation window throws ripples across the whole lag
    /// range. Both are recognisable from `noncausal_peak_value` standing
    /// clear of [`Self::negative_lag_median`], and neither is recognisable at
    /// all if the estimator reports only what it was willing to select.
    ///
    /// When this exceeds [`Self::peak_value`] the correlation is dominated by
    /// something non-causal. That is *not* by itself a refusal — the arrival
    /// is often still there and still recoverable, which is exactly what rig
    /// session 2's −826 ms lock hid: its own evidence put the true arrival at
    /// +4.52 ms. Whether the causal peak is good enough remains the
    /// prominence gate's question, decided the same way it is decided for
    /// every other capture.
    pub noncausal_peak_lag: i64,
    /// |ρ| at [`Self::noncausal_peak_lag`].
    pub noncausal_peak_value: f64,
    /// Median |ρ| over the searched lags — the noise floor
    /// [`Self::prominence`] is measured against. Published separately so a
    /// capture can be re-thresholded offline without refitting the ratio.
    pub median_value: f64,
    /// Median |ρ| over the **negative** lags only, where a causal path puts
    /// no signal at all — no direct arrival and no reflection, so only noise.
    ///
    /// Diagnostic, and deliberately not used by any decision here.
    ///
    /// It was published to settle a proposal from rig session 2: that
    /// [`Self::prominence`] divides by a median over *all* lags, most of which
    /// hold reverberation on a real path, so an uncontaminated negative-lag
    /// floor would separate valid locks from noise where the all-lag one does
    /// not. **That proposal is closed, measured twice** —
    /// `audit/rig-verify-125/gate-rules-offline.md` §2 (12 captures, ≤8%) and
    /// `audit/rig-session-3/negative-lag-rule.md` (843 attempts, 3.5% against
    /// ±17.5% per-attempt noise). Re-basing prominence on this field widens
    /// nothing and admits three times as many near-wall wrong locks. A median
    /// over a 34 ms scan is not moved by a room, which is the same reason the
    /// floor above is robust in the first place. Do not raise it again.
    ///
    /// **What it is for now** is ring composition, not the room: a ring that
    /// straddles the stimulus onset is mostly silence, so `median_value`
    /// collapses while this holds. Observed `median_value / negative_lag_median`
    /// = 0.364 there, against 0.720 as the lowest of 843 steady-state
    /// attempts — and the inflated all-lag prominence that produced is the
    /// only false-confidence lock either rig session recorded. Reading that
    /// requires both floors on the same frame, which is why this is published
    /// rather than dropped.
    ///
    /// `0.0` when the search range holds no negative lags.
    pub negative_lag_median: f64,
    /// Every local maximum within [`CANDIDATE_CAPTURE_FRACTION`] of the
    /// strongest causal peak, in lag order, ranked to [`MAX_CANDIDATES`] —
    /// plus [`Self::lag`], [`Self::peak_lag`] and
    /// [`Self::noncausal_peak_lag`], which are always present whatever their
    /// rank. Lags are unique and strictly ascending, so a replay may sum or
    /// histogram the list without double-counting.
    ///
    /// This is what makes [`DIRECT_PEAK_FRACTION`] settleable. Prominence
    /// alone fixes the noise floor but says nothing about where the direct
    /// arrival sits relative to the reflection that beats it — that ratio is
    /// only recoverable if the competing peaks are recorded alongside it, and
    /// a list that omits the lag the daemon accepted cannot reproduce the
    /// daemon's own decision offline.
    pub candidates: Vec<DelayCandidate>,
}

impl DelayEstimate {
    /// The degenerate result: no correlation could be formed.
    fn refused_without_evidence() -> Self {
        Self {
            lag: None,
            prominence: 0.0,
            peak_lag: 0,
            peak_value: 0.0,
            noncausal_peak_lag: 0,
            noncausal_peak_value: 0.0,
            median_value: 0.0,
            negative_lag_median: 0.0,
            candidates: Vec::new(),
        }
    }
}

/// Put `lag` in the candidate list if the rank-based cut left it out.
///
/// `candidates` is kept sorted by lag, so this is a binary search plus an
/// insert. Idempotent: a lag already present keeps its recorded value.
fn ensure_candidate(candidates: &mut Vec<DelayCandidate>, lag: i64, value: f64) {
    if let Err(pos) = candidates.binary_search_by_key(&lag, |c| c.lag) {
        candidates.insert(pos, DelayCandidate { lag, value });
    }
}

/// Delay estimation via FFT-based cross-correlation. Exposed so callers that
/// drive `h1_estimate` in a tight loop (e.g. `transfer_stream`) can estimate
/// once on warmup and reuse the result via [`h1_estimate_with_delay`] — the
/// ref↔meas path delay is physically constant during a streaming session.
///
/// Returns `None` when no peak in the correlation is prominent enough to be
/// a path delay — an unpatched reference leg, a dead microphone, or two
/// inputs carrying unrelated sources. Refusing is the point: the previous
/// global-maximum rule returned a confident lag for uncorrelated inputs
/// (#227), and the caller caches the lock for the session.
///
/// Use [`estimate_delay_detailed`] when the reason matters as well as the
/// answer.
pub fn estimate_delay_samples(ref_sig: &[f32], meas: &[f32], sr: u32) -> Option<i64> {
    estimate_delay_detailed(ref_sig, meas, sr).lag
}

/// [`estimate_delay_samples`] plus the prominence the decision was made on.
pub fn estimate_delay_detailed(ref_sig: &[f32], meas: &[f32], sr: u32) -> DelayEstimate {
    let r: Vec<f64> = ref_sig.iter().map(|&x| x as f64).collect();
    let m: Vec<f64> = meas.iter().map(|&x| x as f64).collect();
    estimate_delay(&r, &m, sr)
}

/// Earliest prominent peak of the cross-correlation, or `None` if no peak
/// qualifies.
///
/// Two rules, in order:
///
/// 1. **Prominence** — the strongest causal peak must exceed the median of
///    the correlation magnitude by [`NOISE_FLOOR_PROMINENCE`]. This is what
///    rejects uncorrelated inputs, and it covers both causes seen on the rig:
///    a poor direct-to-reverberant ratio and a low electrical SNR. It is the
///    only noise gate in the function — selection below inherits its margin
///    rather than applying a second one (#246).
/// 2. **Causal** — the search runs over non-negative lags only, and every
///    threshold is measured against the strongest peak in that range. The
///    microphone cannot lead the electrical reference, so a negative lag is a
///    ripple or a skew, not a path (rig session 2: a −826 ms lock at
///    prominence 31.8 where the arrival was +4.52 ms). A peak outside the
///    range is reported, never selected.
/// 3. **Earliest, not largest** — among non-negative peaks within
///    [`DIRECT_PEAK_FRACTION`] of the strongest, the one at the smallest lag
///    wins. The direct sound is by definition the first arrival; a later
///    reflection winning the global maximum is exactly the failure #227
///    measured (22.8 / 30.3 / 30.4 ms locks where 5.9 ms was physical).
///
/// A plausibility window on lag is deliberately *not* used: it would need a
/// source-to-microphone distance the software does not have, and the gain
/// sweep in #227 produced the same failure at fixed geometry.
fn estimate_delay(ref_sig: &[f64], meas: &[f64], sr: u32) -> DelayEstimate {
    let corr_len = ref_sig.len().min(meas.len()).min(4 * sr as usize);
    let r = &ref_sig[..corr_len];
    let m = &meas[..corr_len];
    let max_lag = (sr as usize).min(corr_len / 2);

    // Zero-pad to next power of 2 for efficient FFT
    let fft_len = (2 * corr_len).next_power_of_two();
    let mut rp: Vec<f64> = r.to_vec();
    rp.resize(fft_len, 0.0);
    let mut mp: Vec<f64> = m.to_vec();
    mp.resize(fft_len, 0.0);

    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(fft_len);
    let mut fr = fft.make_output_vec();
    let mut fm = fft.make_output_vec();
    fft.process(&mut rp, &mut fr).ok();
    fft.process(&mut mp, &mut fm).ok();

    // Cross-spectrum: conj(fr) * fm
    let mut cross: Vec<Complex<f64>> = fr
        .iter()
        .zip(fm.iter())
        .map(|(a, b)| a.conj() * b)
        .collect();

    let ifft = planner.plan_fft_inverse(fft_len);
    let mut corr = ifft.make_output_vec();
    ifft.process(&mut cross, &mut corr).ok();
    let norm = fft_len as f64;
    for v in corr.iter_mut() {
        *v /= norm;
    }

    // Normalize to a correlation coefficient so the prominence test is a
    // pure shape test, independent of either leg's absolute level (the two
    // legs differ by 15 dB on a typical acoustic setup).
    let energy_r: f64 = r.iter().map(|v| v * v).sum();
    let energy_m: f64 = m.iter().map(|v| v * v).sum();
    let denom = (energy_r * energy_m).sqrt();
    if !denom.is_finite() || denom <= 0.0 {
        // One leg is silent — there is nothing to correlate against.
        return DelayEstimate::refused_without_evidence();
    }

    // Magnitude over the full ±max_lag search range, in ascending lag order
    // so "earliest" is simply "lowest index".
    let n_lags = 2 * max_lag + 1;
    let mut mag = Vec::<f64>::with_capacity(n_lags);
    for lag in (1..=max_lag).rev() {
        let idx = fft_len - lag;
        mag.push(if idx < corr.len() {
            corr[idx].abs() / denom
        } else {
            0.0
        });
    }
    for &c in corr.iter().take(max_lag + 1) {
        mag.push(c.abs() / denom);
    }

    // Robust noise floor. The median is unmoved by the peak itself and by a
    // reverberant tail, both of which occupy a small fraction of the lags.
    let mut sorted = mag.clone();
    let mid = sorted.len() / 2;
    sorted.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
    let median = sorted[mid];

    // The same statistic over negative lags alone — a noise floor with no
    // reverberation in it, published as evidence only. See
    // `DelayEstimate::negative_lag_median`.
    let negative_lag_median = if max_lag > 0 {
        let mut neg = mag[..max_lag].to_vec();
        let nmid = neg.len() / 2;
        neg.select_nth_unstable_by(nmid, |a, b| a.total_cmp(b));
        neg[nmid]
    } else {
        0.0
    };

    // The searched range for *selection* is the causal half. Nothing can
    // arrive at the microphone before it leaves the electrical reference, so
    // a negative lag is not a path delay, and a correlation peak sitting at
    // one is a fact about the capture rather than an answer: ring skew (#216,
    // fixed in the daemon) or a ripple thrown up by the stimulus onset.
    //
    // Rig session 2 accepted a **−826 ms** lock at prominence 31.8 and
    // painted LOCK ACQUIRED, while its own evidence put the arrival at
    // +4.52 ms. That arrival is recoverable — scanning from lag 0 finds it —
    // so the estimator narrows its search rather than refusing the capture.
    // Every threshold below is therefore measured against the strongest
    // *causal* peak, which is the strongest peak that could be an answer.
    //
    // The negative lags are not discarded. They stay in `mag`, in the median,
    // in `negative_lag_median`, and in the candidate list, and the strongest
    // of them is reported in its own right — see
    // `DelayEstimate::noncausal_peak_lag`. A skew that returns is only
    // diagnosable from a capture if the capture still shows it.
    let zero_idx = max_lag;
    let strongest = |range: std::ops::Range<usize>| {
        range.fold((0usize, f64::NEG_INFINITY), |(bi, bv), i| {
            if mag[i] > bv {
                (i, mag[i])
            } else {
                (bi, bv)
            }
        })
    };
    let (peak_idx, peak_val) = strongest(zero_idx..mag.len());
    let (noncausal_idx, noncausal_val) = if zero_idx > 0 {
        strongest(0..zero_idx)
    } else {
        (0, 0.0)
    };
    if !peak_val.is_finite() || peak_val <= 0.0 {
        // No correlation at any causal lag. In practice this means one leg
        // carries no signal at all -- any live pair correlates *somewhere*
        // after the reference, if only in the ripple -- so there is nothing
        // to report about either half of the range. A skewed pair, which does
        // have something to say, refuses further down on prominence and keeps
        // its evidence.
        return DelayEstimate::refused_without_evidence();
    }
    // A zero median means the correlation is zero almost everywhere — a
    // degenerate input rather than a clean lock, so there is no ratio to
    // report and the peak is not evidence of anything.
    let prominence = if median > 0.0 { peak_val / median } else { 0.0 };

    // Competing peaks, gathered before the accept gate so that a refusal
    // still carries them. A position that never locks is the one whose
    // candidates matter most: they are the evidence for whether the
    // threshold is wrong or the microphone is.
    let capture_floor = peak_val * CANDIDATE_CAPTURE_FRACTION;
    let mut candidates: Vec<DelayCandidate> = (0..mag.len())
        .filter(|&i| {
            mag[i] >= capture_floor
                && (i == 0 || mag[i] >= mag[i - 1])
                && (i + 1 >= mag.len() || mag[i] >= mag[i + 1])
        })
        .map(|i| DelayCandidate {
            lag: i as i64 - max_lag as i64,
            value: mag[i],
        })
        .collect();
    // Rank by strength to survive truncation, then report in lag order —
    // see MAX_CANDIDATES for why keeping the earliest instead loses every
    // real arrival at exactly the SNR where the capture is needed.
    if candidates.len() > MAX_CANDIDATES {
        candidates.select_nth_unstable_by(MAX_CANDIDATES, |a, b| b.value.total_cmp(&a.value));
        candidates.truncate(MAX_CANDIDATES);
    }
    candidates.sort_unstable_by_key(|c| c.lag);
    // Both reported peaks are decision-relevant whatever their rank, and a
    // capture that omits either cannot be replayed. The causal peak is the
    // answer a global-maximum rule would give under the causality rule; the
    // non-causal one is the evidence that says *why* a refusal happened when
    // it is the larger of the two. `ensure_candidate` is a no-op when the
    // rank cut already kept them, so the list holds no duplicates.
    let peak_lag = peak_idx as i64 - max_lag as i64;
    ensure_candidate(&mut candidates, peak_lag, peak_val);
    let noncausal_peak_lag = noncausal_idx as i64 - max_lag as i64;
    if noncausal_val > 0.0 {
        ensure_candidate(&mut candidates, noncausal_peak_lag, noncausal_val);
    }
    let evidence = DelayEstimate {
        lag: None,
        prominence,
        peak_lag,
        peak_value: peak_val,
        noncausal_peak_lag,
        noncausal_peak_value: noncausal_val,
        median_value: median,
        negative_lag_median,
        candidates,
    };

    if median > 0.0 && prominence < NOISE_FLOOR_PROMINENCE {
        return evidence;
    }

    // Earliest local maximum within DIRECT_PEAK_FRACTION of the strongest —
    // the direct arrival when a reflection wins the global maximum, and the
    // strongest peak itself otherwise.
    //
    // One bar, not two: within 6 dB of the strongest peak. The `max(...,
    // NOISE_FLOOR_PROMINENCE x median)` term that used to sit here is a
    // full-scan noise correction, and selection does not scan — it picks
    // among the candidate list. Applying it here bound the threshold above
    // the fraction for every prominence under 24 and shrank this rule's reach
    // to `20·log10(prominence/12)` dB, which is where the rig's 3 m locks
    // live (#246).
    //
    // Noise is kept out by the admission gate above: at prominence `P` this
    // cut sits at `P/2 × median`, i.e. 6.0× at the floor, against a measured
    // 4.32 ceiling for noise landing *earlier* than the arrival. That margin
    // is why no second term is needed, and it is also why the admission
    // constant may not be raised in isolation — see NOISE_FLOOR_PROMINENCE.
    let threshold = peak_val * DIRECT_PEAK_FRACTION;
    let direct_idx = (zero_idx..=peak_idx)
        .find(|&i| {
            mag[i] >= threshold
                && (i == 0 || mag[i] >= mag[i - 1])
                && (i + 1 >= mag.len() || mag[i] >= mag[i + 1])
        })
        .unwrap_or(peak_idx);

    let lag = direct_idx as i64 - max_lag as i64;
    let mut evidence = evidence;
    ensure_candidate(&mut evidence.candidates, lag, mag[direct_idx]);
    DelayEstimate {
        lag: Some(lag),
        ..evidence
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// H1 transfer function estimate.
///
/// * `ref_sig` — reference channel (the stimulus; input to DUT)
/// * `meas`    — measurement channel (the output of DUT)
/// * `sr`      — sample rate in Hz
pub fn h1_estimate(ref_sig: &[f32], meas: &[f32], sr: u32) -> TransferResult {
    let r: Vec<f64> = ref_sig.iter().map(|&x| x as f64).collect();
    let m: Vec<f64> = meas.iter().map(|&x| x as f64).collect();
    // A refused estimate (no prominent correlation peak) leaves the pair
    // unaligned rather than aligned to a guess — 0 is what the one-shot
    // path did before any delay was measurable, and a wrong lag is worse
    // than none. Callers that must know a lock failed use
    // [`estimate_delay_samples`] directly.
    let delay_samples = estimate_delay(&r, &m, sr).lag.unwrap_or(0);
    h1_estimate_core(&r, &m, sr, delay_samples)
}

/// Variant of [`h1_estimate`] that skips the O(N log N) delay estimation and
/// uses a caller-supplied `delay_samples`. The streaming transfer worker
/// estimates the delay once on warmup (the ref↔meas path is physically
/// constant while a session is running) and feeds it in on every tick,
/// cutting ~12–15 ms of per-frame FFT work at 2.5 s ring length and
/// 48 kHz — the difference between 8.5 Hz choppy and 20 Hz smooth
/// transfer-view refresh.
pub fn h1_estimate_with_delay(
    ref_sig: &[f32],
    meas: &[f32],
    sr: u32,
    delay_samples: i64,
) -> TransferResult {
    let r: Vec<f64> = ref_sig.iter().map(|&x| x as f64).collect();
    let m: Vec<f64> = meas.iter().map(|&x| x as f64).collect();
    h1_estimate_core(&r, &m, sr, delay_samples)
}

fn h1_estimate_core(r: &[f64], m: &[f64], sr: u32, delay_samples: i64) -> TransferResult {
    assert_eq!(r.len(), m.len(), "ref and meas must have equal length");

    let nperseg = sr as usize; // 1 Hz resolution
    let noverlap = nperseg / 2;
    let window = hann_window(nperseg);

    let delay_ms = delay_samples as f64 / sr as f64 * 1000.0;

    let mut planner = RealFftPlanner::<f64>::new();
    let (gxx, gyy, gxy) = welch_all(r, m, nperseg, noverlap, &window, &mut planner);

    let nfft = nperseg / 2 + 1;
    let freqs: Vec<f64> = (0..nfft)
        .map(|k| k as f64 * sr as f64 / nperseg as f64)
        .collect();

    // Delay compensation: Gxy_comp = Gxy * exp(j * 2π * f * delay / sr)
    let gxy_comp: Vec<Complex<f64>> = gxy
        .iter()
        .enumerate()
        .map(|(k, &g)| {
            let phase = 2.0 * PI * freqs[k] * delay_samples as f64 / sr as f64;
            g * Complex::new(phase.cos(), phase.sin())
        })
        .collect();

    // H1 = Gxy_comp / Gxx — preserve the complex value so re/im are
    // consistent with magnitude_db / phase_deg (all three derived
    // from the same h1).
    let mut magnitude_db = vec![0.0f64; nfft];
    let mut phase_deg = vec![0.0f64; nfft];
    let mut re = vec![0.0f64; nfft];
    let mut im = vec![0.0f64; nfft];
    for k in 0..nfft {
        let gxx_safe = gxx[k].max(1e-30);
        let h1 = gxy_comp[k] / gxx_safe;
        let mag = h1.norm().max(1e-6); // floor at −120 dB
        magnitude_db[k] = 20.0 * mag.log10();
        phase_deg[k] = h1.arg().to_degrees();
        re[k] = h1.re;
        im[k] = h1.im;
    }

    // Coherence = |Gxy|² / (Gxx × Gyy)
    let coherence: Vec<f64> = (0..nfft)
        .map(|k| {
            let denom = gxx[k] * gyy[k];
            let coh = if denom > 0.0 {
                gxy[k].norm_sqr() / denom
            } else {
                0.0
            };
            coh.clamp(0.0, 1.0)
        })
        .collect();

    // Peak-amplitude normalization matching `spectrum_only`'s convention
    // (handoff: transfer-frame-v2 M0, decision 0): `gxx`/`gyy` are raw
    // `|FFT|²` averaged across Welch segments with no window-compensation.
    // `wc` (Hann coherent gain, mean of the window) and `norm = (nperseg/2)
    // · wc` are the same quantities `with_hann_window`/`spectrum_only` use
    // (`shared/fft_cache.rs`) — recomputed locally here (not imported)
    // because `welch_all` already built its own identical Hann window
    // above and this stays a pure post-processing step with zero risk to
    // the existing (tested) magnitude_db/phase_deg/re/im/coherence outputs.
    let wc = window.iter().sum::<f64>() / nperseg as f64;
    let norm = (nperseg as f64 / 2.0) * wc;
    let ref_amp: Vec<f64> = gxx.iter().map(|&p| p.max(0.0).sqrt() / norm).collect();
    let meas_amp: Vec<f64> = gyy.iter().map(|&p| p.max(0.0).sqrt() / norm).collect();

    TransferResult {
        freqs,
        magnitude_db,
        phase_deg,
        coherence,
        re,
        im,
        delay_samples,
        delay_ms,
        ref_amp,
        meas_amp,
    }
}

/// Inverse FFT of a complex H(ω) (in `re`, `im` parallel arrays from a
/// `TransferResult`) into a time-domain impulse response h(t).
///
/// Returns `Vec<f32>` of length `(re.len() - 1) * 2`. For the
/// `h1_estimate_core` Welch path, that's `nperseg = sr` samples = 1 s
/// of IR — plenty of visual range for typical room / DUT responses.
///
/// h(t) is centred via `fftshift`-style rotation so the dominant peak
/// (DC bin energy + linear-phase pre-roll) sits at `t = 0` in the
/// middle of the array. Caller treats indices `[0, n/2)` as
/// pre-causal taps, `[n/2, n)` as causal. Empty / mismatched / too-
/// short inputs return `Vec::new()`.
///
/// `unified.md` Phase 4b. Daemon-side IFFT — UI gets a downsampled
/// time-series and just plots it (no UI-side FFT plumbing needed).
pub fn impulse_response_from_h(re: &[f64], im: &[f64]) -> Vec<f32> {
    if re.is_empty() || re.len() != im.len() || re.len() < 2 {
        return Vec::new();
    }
    let nfft = re.len();
    let n_time = (nfft - 1) * 2;
    let mut planner = RealFftPlanner::<f64>::new();
    let ifft = planner.plan_fft_inverse(n_time);
    let mut spectrum: Vec<Complex<f64>> = re
        .iter()
        .zip(im.iter())
        .map(|(&r, &i)| Complex::new(r, i))
        .collect();
    // realfft inverse requires DC (bin 0) and Nyquist (bin n-1) to
    // have zero imaginary part — they describe real-valued frequency
    // components in any real-input → complex-output forward FFT, so
    // their inverse must hold the same constraint. Welch H₁ from
    // real signal pairs *should* give real values at these bins
    // (real/real = real), but Welch averaging + float noise leaves
    // tiny non-zero imaginary residue that realfft refuses. Zero
    // them so the IFFT proceeds cleanly. The discarded residue is
    // sub-1e-10 in normal operation and reflects numerical noise,
    // not signal content.
    if let Some(first) = spectrum.first_mut() {
        first.im = 0.0;
    }
    if let Some(last) = spectrum.last_mut() {
        last.im = 0.0;
    }
    let mut time = ifft.make_output_vec();
    if ifft.process(&mut spectrum, &mut time).is_err() {
        return Vec::new();
    }
    // Realfft inverse doesn't normalise — divide by n_time so the
    // recovered impulse magnitude matches the H(ω) amplitudes.
    let norm = n_time as f64;
    // Center via fftshift-style swap so the user sees the IR peak at
    // mid-cell instead of at the array edge (where pre-causal taps
    // wrap around to indices near n_time-1 in the un-shifted output).
    let half = n_time / 2;
    let mut out = Vec::<f32>::with_capacity(n_time);
    for k in 0..n_time {
        // Source index: k = 0 → src n/2 (the t=0 IR peak); k = n-1 →
        // src n/2 - 1 (the wraparound point).
        let src = (k + half) % n_time;
        out.push((time[src] / norm) as f32);
    }
    out
}

/// Number of capture seconds needed for `n_averages` Welch segments at `sr`.
pub fn capture_duration(n_averages: usize, sr: u32) -> f64 {
    let nperseg = sr as usize;
    let noverlap = nperseg / 2;
    let step = nperseg - noverlap;
    let total = nperseg + step * (n_averages - 1);
    total as f64 / sr as f64
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use rand::rngs::StdRng;
    use rand::SeedableRng;
    use rand_distr::{Distribution, Normal};

    const SR: u32 = 48_000;
    const N: usize = 3 * SR as usize; // 3 s → 5 Welch segments

    fn white_noise(n: usize, amplitude: f64, seed: u64) -> Vec<f32> {
        let mut rng = StdRng::seed_from_u64(seed);
        let dist = Normal::new(0.0, amplitude).unwrap();
        (0..n).map(|_| dist.sample(&mut rng) as f32).collect()
    }

    // ---- Amplitude normalization (handoff: transfer-frame-v2 M0, decision 0) ----

    /// A full-scale on-bin sine must read amplitude ≈1.0 in both
    /// `meas_amp` and `ref_amp` — the same peak-amplitude convention
    /// `spectrum_only` uses, so the two paths are cross-comparable (I-C).
    /// Without the missing window-compensation this reads far above 1.0
    /// (raw `|FFT|²` sum, no `÷((nperseg/2)·wc)²`).
    #[test]
    fn meas_and_ref_amp_match_spectrum_only_convention_on_bin_tone() {
        use crate::visualize::spectrum::spectrum_only;
        let f0 = 1_000.0_f64; // exact bin at 1 Hz/bin (nperseg == SR)
        let tone: Vec<f32> = (0..N)
            .map(|i| (2.0 * PI * f0 * i as f64 / SR as f64).sin() as f32)
            .collect();
        let r = h1_estimate(&tone, &tone, SR);
        let bin = f0 as usize;

        assert!(
            (r.meas_amp[bin] - 1.0).abs() < 0.02,
            "meas_amp[{bin}] = {} (expected ~1.0)",
            r.meas_amp[bin]
        );
        assert!(
            (r.ref_amp[bin] - 1.0).abs() < 0.02,
            "ref_amp[{bin}] = {} (expected ~1.0)",
            r.ref_amp[bin]
        );

        // Cross-path parity (I-C): a single-block spectrum_only reading of
        // the same tone lands on the same amplitude scale as the
        // Welch-averaged meas_amp.
        let (spec, _freqs) = spectrum_only(&tone[..SR as usize], SR);
        assert!(
            (spec[bin] - r.meas_amp[bin]).abs() < 0.02,
            "spectrum_only[{bin}]={} vs meas_amp[{bin}]={}",
            spec[bin],
            r.meas_amp[bin]
        );
    }

    #[test]
    fn amp_off_tone_bins_are_near_silence() {
        let f0 = 1_000.0_f64;
        let tone: Vec<f32> = (0..N)
            .map(|i| (2.0 * PI * f0 * i as f64 / SR as f64).sin() as f32)
            .collect();
        let r = h1_estimate(&tone, &tone, SR);
        let bin = f0 as usize;
        for k in [bin - 100, bin + 100, 20, 20_000] {
            assert!(
                r.meas_amp[k] < 0.05,
                "meas_amp[{k}] = {} leaked tone energy",
                r.meas_amp[k]
            );
            assert!(
                r.ref_amp[k] < 0.05,
                "ref_amp[{k}] = {} leaked tone energy",
                r.ref_amp[k]
            );
        }
    }

    #[test]
    fn amp_arrays_parallel_to_freqs() {
        let sig = white_noise(N, 0.5, 7);
        let r = h1_estimate(&sig, &sig, SR);
        assert_eq!(r.ref_amp.len(), r.freqs.len());
        assert_eq!(r.meas_amp.len(), r.freqs.len());
    }

    /// AC #3 (band-power N-independence), the axis actually variable in
    /// this estimator: `nperseg` is pinned to `sr` in `h1_estimate_core`
    /// (1 Hz bins always), so "N" here means **Welch segment count**
    /// (`(len - nperseg) / step + 1`), which varies with capture length.
    /// Same broadband noise (same seed ⇒ same underlying signal, just
    /// truncated to different lengths) fed at K=2 segments (1.5 s, the
    /// minimum above the 1-segment warm-up floor) vs K=8 segments (4.5 s)
    /// must integrate to the same broadband level in a sub-band —
    /// checked as **one integrated level across ~1800 bins**, not
    /// per-column (per-column would be flaky/vacuous: bin→column
    /// assignment doesn't even change with segment count, only the
    /// per-bin averaging noise does).
    ///
    /// Tolerance derivation: a single periodogram bin's power estimate
    /// averaged over K segments has relative variance ≈1/K (chi²(2K)/2K).
    /// Summing power over ~1800 roughly-independent bins in the
    /// 200–2000 Hz sub-band further reduces the *total*'s relative
    /// variance by ≈1/√1800 (central-limit-like reduction across bins;
    /// 50 % Welch overlap correlates adjacent segments but not
    /// far-apart frequency bins). Combined expected relative std ≈
    /// 1/√(K·1800) ≈ 1.3 % (K=2) → ≈0.1 dB. 1.0 dB tolerance clears
    /// that with an order of magnitude of margin while still catching a
    /// real N-dependence regression (the historical #142/#162 defects
    /// were multi-dB).
    #[test]
    fn broadband_level_invariant_to_welch_segment_count() {
        use crate::visualize::spl_level::weighted_broadband_dbfs;
        use crate::visualize::weighting_curves::WeightingCurve;

        let step = SR as usize / 2; // 50 % overlap, matches h1_estimate_core
        let len_k2 = SR as usize + step; // K=2 segments, 1.5 s
        let len_k8 = SR as usize + step * 7; // K=8 segments, 4.5 s

        // Same seed ⇒ same underlying noise stream; K8's tail is simply
        // more of the same stationary process, not a different signal.
        let noise_full = white_noise(len_k8, 0.3, 99);
        let noise_k2 = &noise_full[..len_k2];
        let noise_k8 = &noise_full[..];

        let r_k2 = h1_estimate(noise_k2, noise_k2, SR);
        let r_k8 = h1_estimate(noise_k8, noise_k8, SR);

        let sub_band = |r: &TransferResult| -> (Vec<f64>, Vec<f64>) {
            r.freqs
                .iter()
                .zip(r.meas_amp.iter())
                .filter(|(&f, _)| (200.0..2000.0).contains(&f))
                .map(|(&f, &a)| (f, a))
                .unzip()
        };
        let (freqs_k2, amp_k2) = sub_band(&r_k2);
        let (freqs_k8, amp_k8) = sub_band(&r_k8);
        assert!(freqs_k2.len() > 1000, "expected ~1800 bins in 200-2000 Hz");

        let db_k2 = weighted_broadband_dbfs(&amp_k2, &freqs_k2, WeightingCurve::Z);
        let db_k8 = weighted_broadband_dbfs(&amp_k8, &freqs_k8, WeightingCurve::Z);
        assert!(
            (db_k2 - db_k8).abs() < 1.0,
            "K=2 segments: {db_k2:.3} dB, K=8 segments: {db_k8:.3} dB — \
             band level should be segment-count-independent within 1.0 dB"
        );
    }

    // ---- capture_duration ----

    #[test]
    fn capture_duration_arithmetic() {
        assert_relative_eq!(capture_duration(1, SR), 1.0, epsilon = 1e-12);
        assert_relative_eq!(capture_duration(5, SR), 3.0, epsilon = 1e-12);
        assert_relative_eq!(capture_duration(10, SR), 5.5, epsilon = 1e-12);
    }

    // ---- Unity / delay / filter ----

    #[test]
    fn unity_loopback() {
        let sig = white_noise(N, 0.5, 42);
        let r = h1_estimate(&sig, &sig, SR);

        assert_eq!(r.delay_samples, 0);
        for k in 20..=20_000 {
            assert!(
                r.magnitude_db[k].abs() < 0.1,
                "bin {k}: mag {:.3} dB",
                r.magnitude_db[k]
            );
            assert!(
                r.phase_deg[k].abs() < 1.0,
                "bin {k}: phase {:.3}°",
                r.phase_deg[k]
            );
            assert!(r.coherence[k] > 0.999, "bin {k}: coh {:.4}", r.coherence[k]);
        }
    }

    /// unified.md Phase 3: re/im are populated parallel to mag/phase
    /// and consistent with them. Unity loopback should give Re ≈ 1,
    /// Im ≈ 0 (within Welch noise) at every bin in the audio band.
    #[test]
    fn unity_loopback_re_im_consistent() {
        let sig = white_noise(N, 0.5, 42);
        let r = h1_estimate(&sig, &sig, SR);

        assert_eq!(r.re.len(), r.magnitude_db.len(), "re len mismatch");
        assert_eq!(r.im.len(), r.magnitude_db.len(), "im len mismatch");
        for k in 20..=20_000 {
            // Round-trip check: |H| from re/im matches |H| from db.
            let mag_lin_re_im = (r.re[k].powi(2) + r.im[k].powi(2)).sqrt();
            let mag_lin_db = 10.0_f64.powf(r.magnitude_db[k] / 20.0);
            assert_relative_eq!(mag_lin_re_im, mag_lin_db, epsilon = 1e-9);
            // Phase round-trip: atan2(im, re) matches phase_deg.
            let p_re_im = r.im[k].atan2(r.re[k]).to_degrees();
            assert_relative_eq!(p_re_im, r.phase_deg[k], epsilon = 1e-9);
        }
        // Unity-gain expectation: Re ≈ 1, Im ≈ 0 in the audio band.
        for k in 200..=2_000 {
            assert!(
                (r.re[k] - 1.0).abs() < 0.05,
                "bin {k}: Re {:.4} (expected ≈ 1)",
                r.re[k]
            );
            assert!(
                r.im[k].abs() < 0.05,
                "bin {k}: Im {:.4} (expected ≈ 0)",
                r.im[k]
            );
        }
    }

    #[test]
    fn delay_only_path() {
        let sig = white_noise(N, 0.5, 42);
        let delay: usize = 100;

        let mut meas = vec![0.0f32; N];
        meas[delay..].copy_from_slice(&sig[..N - delay]);

        let r = h1_estimate(&sig, &meas, SR);

        assert_eq!(r.delay_samples, delay as i64);
        let expected_ms = delay as f64 / SR as f64 * 1000.0;
        assert_relative_eq!(r.delay_ms, expected_ms, epsilon = 0.01);

        for k in 100..=20_000 {
            assert!(
                r.magnitude_db[k].abs() < 0.5,
                "bin {k}: mag {:.3} dB",
                r.magnitude_db[k]
            );
            assert!(r.coherence[k] > 0.95, "bin {k}: coh {:.4}", r.coherence[k]);
        }
    }

    // ---- #227: earliest prominent peak ----

    /// Mix `src` into `dst` delayed by `delay` samples and scaled by `gain`.
    /// Building a measurement leg out of several of these is what every
    /// existing delay test lacks: one unambiguous correlation peak cannot
    /// express a reflection beating the direct sound.
    fn add_delayed(dst: &mut [f32], src: &[f32], delay: usize, gain: f32) {
        for i in delay..dst.len() {
            dst[i] += gain * src[i - delay];
        }
    }

    /// Sub-millisecond acceptance (#227, `handoff-lock-and-smoothing.md`
    /// decision 5): 1 ms is 48 samples at 48 kHz.
    const SUB_MS: i64 = SR as i64 / 1000;

    /// Run 1's measured failure shape: microphone under 1.5 m from the
    /// source, direct arrival at 5.9 ms, a reflection cluster near 30 ms
    /// carrying more energy than the direct sound. The global maximum is
    /// the 30 ms reflection — which is what the estimator locked to on the
    /// rig, 8 sessions out of 8 at position E. The direct peak is the
    /// answer.
    #[test]
    fn reflection_stronger_than_direct_locks_to_direct() {
        let sig = white_noise(N, 0.5, 42);
        let direct = 283; // 5.9 ms at 48 kHz

        let mut meas = vec![0.0f32; N];
        add_delayed(&mut meas, &sig, direct, 0.50);
        add_delayed(&mut meas, &sig, 1_094, 0.55); // 22.8 ms
        add_delayed(&mut meas, &sig, 1_455, 0.80); // 30.3 ms — global max
        add_delayed(&mut meas, &sig, 1_461, 0.70); // 30.4 ms

        let d = estimate_delay_samples(&sig, &meas, SR).expect("prominent direct peak");
        assert!(
            (d - direct as i64).abs() < SUB_MS,
            "locked to {d} samples ({:.2} ms), expected the direct arrival at \
             {direct} ({:.2} ms)",
            d as f64 / SR as f64 * 1000.0,
            direct as f64 / SR as f64 * 1000.0
        );
    }

    /// A poor direct-to-reverberant ratio: a dense decaying tail out to
    /// 120 ms whose individual reflections each rival the direct sound and
    /// whose total energy far exceeds it. This is the general case behind
    /// the Run 1 numbers — the estimator must still take the first arrival
    /// rather than whichever tail reflection happens to win.
    #[test]
    fn reverberant_tail_does_not_beat_direct() {
        let sig = white_noise(N, 0.5, 42);
        let direct = 400;

        let mut meas = vec![0.0f32; N];
        add_delayed(&mut meas, &sig, direct, 0.40);
        let mut rng = StdRng::seed_from_u64(9);
        let jitter = Normal::new(0.0, 1.0).unwrap();
        for k in 1..=60 {
            // Reflections spread from ~10 ms to ~120 ms, decaying, with
            // randomised spacing so no lag is special.
            let t = direct + 480 * k + (jitter.sample(&mut rng) * 40.0) as usize;
            if t >= N {
                break;
            }
            let gain = 0.62 * (-(k as f32) / 25.0).exp();
            add_delayed(&mut meas, &sig, t, gain);
        }

        let d = estimate_delay_samples(&sig, &meas, SR).expect("prominent direct peak");
        assert!(
            (d - direct as i64).abs() < SUB_MS,
            "locked to {d} samples, expected the direct arrival at {direct}"
        );
    }

    /// No correlated content at all — the two legs carry unrelated sources
    /// (Run 5's pair 1 locked confidently to 494 ms on exactly this). The
    /// estimator must refuse rather than return the largest noise ripple.
    /// Several seed pairs, because a threshold that only holds for one
    /// realisation of the noise floor is not a threshold.
    #[test]
    fn uncorrelated_legs_are_refused() {
        for (a, b) in [(1u64, 2u64), (3, 4), (5, 6), (7, 8), (9, 10)] {
            let ref_sig = white_noise(N, 0.5, a);
            let meas = white_noise(N, 0.5, b);
            let d = estimate_delay_samples(&ref_sig, &meas, SR);
            assert!(
                d.is_none(),
                "seeds ({a}, {b}): expected a refusal, got a lock at {:?} samples",
                d.unwrap()
            );
        }
    }

    /// A dead leg (unpatched reference, muted microphone) carries no energy
    /// to correlate. Refuse rather than divide by zero into a lag of 0.
    #[test]
    fn silent_leg_is_refused() {
        let sig = white_noise(N, 0.5, 42);
        let silence = vec![0.0f32; N];
        assert!(estimate_delay_samples(&sig, &silence, SR).is_none());
        assert!(estimate_delay_samples(&silence, &sig, SR).is_none());
    }

    /// The electrical-SNR half of #227: fixed geometry, a single clean
    /// arrival buried in an uncorrelated noise floor 20 dB above it. The
    /// peak is small in absolute terms but still prominent against the
    /// correlation floor, so this locks — the gain sweep showed reliability
    /// tracking SNR, and the low-gain end must not become a silent refusal
    /// where the direct peak is genuinely there.
    #[test]
    fn low_snr_single_arrival_still_locks() {
        let sig = white_noise(N, 0.5, 42);
        let noise = white_noise(N, 0.5, 77);
        let delay = 512;

        let mut meas = noise.clone();
        add_delayed(&mut meas, &sig, delay, 0.05);

        let d = estimate_delay_samples(&sig, &meas, SR).expect("peak above the correlation floor");
        assert!(
            (d - delay as i64).abs() < SUB_MS,
            "locked to {d} samples, expected {delay}"
        );
    }

    /// The measurement leg leading the reference is a negative lag, and no
    /// acoustic path can produce one: the microphone cannot hear the stimulus
    /// before the reference carries it. When it happens the cause is upstream
    /// — ring skew (#216, fixed in the daemon) or a ripple thrown up by the
    /// stimulus onset, which is what put a −826 ms lock at prominence 31.8 on
    /// screen in rig session 2 while the true arrival sat at +4.52 ms.
    ///
    /// Here there is nothing else in the capture: the only correlation is the
    /// non-causal one, so the causal range holds ripple, the prominence gate
    /// refuses on its own terms, and no separate causality refusal is needed
    /// to reach the right answer.
    ///
    /// What must **not** happen is the refusal being silent about why.
    /// `noncausal_peak_lag` names the skew, which is what keeps a recurrence
    /// diagnosable from a capture instead of from another rig session.
    #[test]
    fn a_skewed_pair_is_refused_and_the_skew_is_named() {
        let sig = white_noise(N, 0.5, 42);
        let delay = 300;

        // Delay the *reference* instead, so meas leads by `delay`.
        let mut ref_sig = vec![0.0f32; N];
        add_delayed(&mut ref_sig, &sig, delay, 1.0);

        let e = estimate_delay_detailed(&ref_sig, &sig, SR);
        assert_eq!(e.lag, None, "a non-causal lag must not be locked onto");
        assert!(
            e.prominence < NOISE_FLOOR_PROMINENCE,
            "the causal range holds only ripple, so the prominence gate is \
             what refuses — got {}",
            e.prominence
        );
        assert!(
            (e.noncausal_peak_lag + delay as i64).abs() < SUB_MS,
            "noncausal_peak_lag {} should name the skew at {}",
            e.noncausal_peak_lag,
            -(delay as i64)
        );
        assert!(
            e.noncausal_peak_value > e.peak_value,
            "the skew {} should dominate the causal ripple {}",
            e.noncausal_peak_value,
            e.peak_value
        );
        assert!(
            e.candidates.iter().any(|c| c.lag == e.noncausal_peak_lag),
            "the non-causal peak must be in its own evidence"
        );
    }

    /// A non-causal peak must not cost a measurement that is still there.
    ///
    /// This is rig session 2's −826 ms lock in miniature: a large peak at a
    /// negative lag, and a genuine arrival at a small positive one. The old
    /// rule scanned from −1 s and took the ripple. Refusing the whole capture
    /// instead would be the opposite error — the arrival is recoverable, and
    /// the session that produced this had a clean measurement in it
    /// (HF coherence 0.877).
    #[test]
    fn a_noncausal_peak_does_not_cost_the_causal_arrival() {
        let sig = white_noise(N, 0.5, 42);
        let arrival = 217usize;

        // The arrival, plus a stronger correlation at a negative lag — as if
        // the reference leg carried a copy of the measurement leg early.
        let mut meas = vec![0.0f32; N];
        add_delayed(&mut meas, &sig, arrival, 0.60);
        let mut ref_sig = sig.clone();
        add_delayed(&mut ref_sig, &sig, 900, 0.95);

        let e = estimate_delay_detailed(&ref_sig, &meas, SR);
        let lag = e.lag.expect("the causal arrival is prominent on its own");
        assert!(
            (lag - arrival as i64).abs() < SUB_MS,
            "locked to {lag}, expected the arrival at {arrival}"
        );
        assert!(
            e.noncausal_peak_value > 0.0 && e.noncausal_peak_lag < 0,
            "the fixture must actually put a peak at a negative lag — got \
             {} at {}",
            e.noncausal_peak_value,
            e.noncausal_peak_lag
        );
    }

    /// The two guaranteed entries are not independent of the ranked 32: the
    /// accepted lag is usually already among them, and on a clean single-peak
    /// capture it *is* the peak. A duplicate would double-count in any
    /// offline replay that sums or histograms the candidates.
    #[test]
    fn the_candidate_list_never_holds_a_duplicate_lag() {
        let sig = white_noise(N, 0.5, 42);

        let mut single = vec![0.0f32; N];
        add_delayed(&mut single, &sig, 512, 1.0);

        let mut cluster = white_noise(N, 0.02, 77);
        for k in 0..(MAX_CANDIDATES + 8) {
            let gain = 0.55 + 0.45 * (k as f32) / (MAX_CANDIDATES as f32 + 7.0);
            add_delayed(&mut cluster, &sig, 400 + k * 60, gain);
        }

        let mut skewed = vec![0.0f32; N];
        add_delayed(&mut skewed, &sig, 300, 1.0);

        for (name, meas) in [
            ("single arrival", &single),
            ("reverberant cluster", &cluster),
            ("uncorrelated", &white_noise(N, 0.5, 99)),
        ] {
            let e = estimate_delay_detailed(&sig, meas, SR);
            let mut lags: Vec<i64> = e.candidates.iter().map(|c| c.lag).collect();
            let before = lags.len();
            lags.dedup();
            assert_eq!(before, lags.len(), "{name}: duplicate lag in candidates");
            assert!(
                lags.windows(2).all(|w| w[0] < w[1]),
                "{name}: not in lag order"
            );
        }

        // And the skew case, where the guaranteed entries are three: the
        // accepted lag (none here), the causal peak, and the non-causal one.
        let e = estimate_delay_detailed(&skewed, &sig, SR);
        let mut lags: Vec<i64> = e.candidates.iter().map(|c| c.lag).collect();
        let before = lags.len();
        lags.dedup();
        assert_eq!(before, lags.len(), "skewed: duplicate lag in candidates");
    }

    /// Degrading SNR must move the estimator from *correct* straight to
    /// *refusing* — never through a band where it returns a confident wrong
    /// lag. That band is not hypothetical: gating the earliest-peak search on
    /// `0.5 x peak` alone, the arrival at gain 0.02 (prominence 13.1, just
    /// inside the 12x accept gate) put the selection floor at 6.5x the median
    /// while the uncorrelated ripple reached 7.1x, and the estimator locked to
    /// noise at -11065 samples — a worse answer than the global maximum this
    /// change replaced, which still had the arrival at 512.
    ///
    /// One arrival, fixed geometry, only the level swept: this is the gain
    /// sweep from the issue, where lock reliability tracked SNR while the
    /// acoustics were unchanged.
    #[test]
    fn degrading_snr_refuses_rather_than_locking_wrong() {
        let sig = white_noise(N, 0.5, 42);
        let delay = 512;

        let mut any_lock = false;
        let mut any_refusal = false;
        for gain in [0.05_f32, 0.03, 0.025, 0.02, 0.018, 0.015, 0.01, 0.005] {
            let mut meas = white_noise(N, 0.5, 77);
            add_delayed(&mut meas, &sig, delay, gain);
            match estimate_delay_samples(&sig, &meas, SR) {
                Some(d) => {
                    any_lock = true;
                    assert!(
                        (d - delay as i64).abs() < SUB_MS,
                        "gain {gain}: locked to {d} samples ({:.1} ms) — a confident \
                         wrong lag is the failure this estimator exists to prevent; \
                         refusing would have been correct",
                        d as f64 / SR as f64 * 1000.0
                    );
                }
                None => any_refusal = true,
            }
        }
        assert!(
            any_lock,
            "the sweep never locked — it does not test the gate"
        );
        assert!(
            any_refusal,
            "the sweep never refused — it does not reach the noise-limited end"
        );
    }

    /// Reflection rejection must not quietly switch itself off as SNR falls.
    ///
    /// Geometry is held fixed — the direct arrival always sits at 0.625 of
    /// the reflection that beats it — and only the noise floor is swept. The
    /// earlier `max(0.5 x peak, 12 x median)` candidate floor rose above that
    /// 0.625 ratio once the global peak's prominence fell under ~19, so the
    /// direct arrival stopped qualifying and the estimator returned the
    /// reflection: prominence 14.95, floor 0.803 x peak, lock at 1455 instead
    /// of 283. That is #227's original failure, reappearing silently at the
    /// low-SNR end. It was first answered by refusing the whole band
    /// (`MIN_PROMINENCE` = 24); #246 answers it by removing the median term
    /// from selection instead, so the fraction binds at every admitted
    /// prominence and the band is measured rather than refused.
    ///
    /// The sweep therefore has to reach into that band, not merely avoid it:
    /// the assertion below fails if every lock sits above prominence 24,
    /// where the two rules cannot differ. Locking correctly under 24 is the
    /// behaviour the change exists to produce, and it is what a re-derived
    /// coupling would break.
    ///
    /// The invariant is the issue's own acceptance criterion: a correct lock
    /// or an explicit refusal, never a third thing.
    #[test]
    fn reflection_rejection_survives_falling_snr() {
        let sig = white_noise(N, 0.5, 42);
        let direct = 283usize;
        let reflection = 1_455usize;

        // The prominence at which the deleted coupling would have taken over:
        // below it the `12 x median` term bound the selection floor above
        // `0.5 x peak`, and admission refused outright.
        let old_coupling = NOISE_FLOOR_PROMINENCE / DIRECT_PEAK_FRACTION;

        let mut any_lock = false;
        let mut any_refusal = false;
        let mut locked_below_old_coupling = false;
        for noise_scale in [0.0_f32, 4.0, 8.0, 16.0, 24.0, 32.0, 40.0, 64.0] {
            let mut meas = if noise_scale > 0.0 {
                white_noise(N, (0.5 * noise_scale) as f64, 77)
            } else {
                vec![0.0f32; N]
            };
            add_delayed(&mut meas, &sig, direct, 0.50);
            add_delayed(&mut meas, &sig, reflection, 0.80);

            let e = estimate_delay_detailed(&sig, &meas, SR);
            match e.lag {
                Some(d) => {
                    any_lock = true;
                    locked_below_old_coupling |= e.prominence < old_coupling;
                    assert!(
                        (d - direct as i64).abs() < SUB_MS,
                        "noise x{noise_scale}: locked to {d} samples at prominence \
                         {} — the direct arrival is at {direct} and the reflection \
                         at {reflection}; returning the reflection is the failure \
                         #227 exists to fix, and refusing would have been correct",
                        e.prominence
                    );
                }
                None => any_refusal = true,
            }
        }
        assert!(
            any_lock,
            "the sweep never locked — it does not test the rule"
        );
        assert!(
            any_refusal,
            "the sweep never refused — it does not reach the noise-limited end"
        );
        assert!(
            locked_below_old_coupling,
            "every lock sat above prominence {old_coupling}, where the fixed \
             window and the old max(0.5 x peak, {NOISE_FLOOR_PROMINENCE} x median) \
             floor agree — the sweep does not reach the band #246 changed"
        );
    }

    /// A refusal must still report the prominence it refused on. A bare
    /// "refused" cannot distinguish "move the microphone" from "the
    /// threshold is wrong", and the rig session is what sets that threshold
    /// — so a session that never locks still has to yield the measurement.
    #[test]
    fn refusal_reports_the_prominence_it_refused_on() {
        let sig = white_noise(N, 0.5, 42);

        // Uncorrelated: refused, and the prominence is the ripple ceiling —
        // the quantity NOISE_FLOOR_PROMINENCE is set against.
        let e = estimate_delay_detailed(&sig, &white_noise(N, 0.5, 99), SR);
        assert!(e.lag.is_none(), "expected a refusal, got {:?}", e.lag);
        assert!(
            e.prominence > 1.0 && e.prominence < NOISE_FLOOR_PROMINENCE,
            "uncorrelated prominence {} should sit above 1 and below the \
             {NOISE_FLOOR_PROMINENCE} gate",
            e.prominence
        );

        // A clean arrival: locked, and far above the gate.
        let mut meas = vec![0.0f32; N];
        add_delayed(&mut meas, &sig, 512, 1.0);
        let e = estimate_delay_detailed(&sig, &meas, SR);
        assert_eq!(e.lag, Some(512));
        assert!(
            e.prominence > NOISE_FLOOR_PROMINENCE,
            "clean arrival prominence {} should clear the {NOISE_FLOOR_PROMINENCE} gate",
            e.prominence
        );

        // A silent leg forms no correlation at all, so there is no ratio,
        // and nothing to say about either half of the lag range.
        let e = estimate_delay_detailed(&sig, &vec![0.0f32; N], SR);
        assert_eq!(e.lag, None);
        assert_eq!(e.prominence, 0.0);
        assert_eq!(e.noncausal_peak_value, 0.0);
        assert!(e.candidates.is_empty());
    }

    /// A refusal with *no causal correlation at all* must still carry the
    /// non-causal evidence, or the diagnosis it was added for is thrown away
    /// at exactly the point it becomes unambiguous.
    ///
    /// A pair skewed far enough that the arrival falls outside the search
    /// window is the shape: nothing after the reference, something clear
    /// before it. That is a crossed or skewed pair, and it must not read the
    /// same as a dead leg.
    #[test]
    fn a_refusal_with_no_causal_peak_still_reports_the_noncausal_one() {
        let sig = white_noise(N, 0.5, 42);
        let delay = 4_800usize; // 100 ms of skew at 48 kHz

        // The measurement leg is the stimulus; the reference carries it
        // `delay` later, and the causal half holds nothing but the tail of
        // the ring, deliberately zeroed.
        let mut ref_sig = vec![0.0f32; N];
        add_delayed(&mut ref_sig, &sig, delay, 1.0);
        let mut meas = sig.clone();
        meas[N - delay..].fill(0.0);

        let e = estimate_delay_detailed(&ref_sig, &meas, SR);
        assert_eq!(e.lag, None);
        assert!(
            e.noncausal_peak_value > 0.0,
            "the skew must be reported, not discarded with the refusal"
        );
        assert!(
            e.noncausal_peak_lag < 0,
            "noncausal_peak_lag {} should sit before the reference",
            e.noncausal_peak_lag
        );
        assert!(
            e.candidates.iter().any(|c| c.lag == e.noncausal_peak_lag),
            "the non-causal peak must appear in the evidence"
        );
    }

    /// `DIRECT_PEAK_FRACTION` must be settleable from a capture alone.
    ///
    /// Prominence fixes the noise floor but says nothing about where the
    /// direct arrival sits relative to the reflection that beats it, so the
    /// competing peaks have to be recorded alongside it. This asserts the
    /// recorded evidence is sufficient to recover that ratio offline —
    /// including on a **refusal**, which is the case the rig session is most
    /// likely to produce at the positions that matter.
    #[test]
    fn candidates_make_the_direct_to_reflection_ratio_recoverable() {
        let sig = white_noise(N, 0.5, 42);
        let direct = 283usize;
        let reflection = 1_455usize;

        // Loud enough to lock, and again buried enough to refuse. The
        // tolerance widens with the noise: an uncorrelated floor lifts the
        // weaker peak proportionally more, so a ratio recovered from a
        // marginal capture reads slightly *high*. That bias is the safe
        // direction — it overstates the direct arrival, so a fraction set
        // from it errs strict — but it means a refusing capture pins the
        // ratio to about 10%, not to the 2% a clean one gives.
        //
        // The refusing scale is x40 (prominence 11.9), not the x32 this case
        // used before #246: at x32 the prominence is 14.95, which the old
        // admission gate of 24 refused and the floor of 12 admits — and
        // admits *correctly*, locking to the direct arrival at 283. x32 is
        // now carried by `reflection_rejection_survives_falling_snr`, which
        // needs it to lock.
        for (noise_scale, expect_lock, tol) in [(0.0_f32, true, 0.02), (40.0, false, 0.10)] {
            let mut meas = if noise_scale > 0.0 {
                white_noise(N, (0.5 * noise_scale) as f64, 77)
            } else {
                vec![0.0f32; N]
            };
            add_delayed(&mut meas, &sig, direct, 0.50);
            add_delayed(&mut meas, &sig, reflection, 0.80);

            let e = estimate_delay_detailed(&sig, &meas, SR);
            assert_eq!(e.lag.is_some(), expect_lock, "noise x{noise_scale}");

            // The reflection is the strongest peak in both cases.
            assert!(
                (e.peak_lag - reflection as i64).abs() < SUB_MS,
                "noise x{noise_scale}: peak_lag {} should be the reflection at {reflection}",
                e.peak_lag
            );

            // Both arrivals must appear among the candidates, so the ratio
            // between them is recoverable without another rig session.
            let find = |want: usize| {
                e.candidates
                    .iter()
                    .find(|c| (c.lag - want as i64).abs() < SUB_MS)
                    .unwrap_or_else(|| {
                        panic!(
                            "noise x{noise_scale}: no candidate near {want}; got {:?}",
                            e.candidates.iter().map(|c| c.lag).collect::<Vec<_>>()
                        )
                    })
            };
            let d = find(direct);
            let r = find(reflection);

            // The synthesised ratio is 0.50/0.80 = 0.625. Recovering it is
            // what lets DIRECT_PEAK_FRACTION be set from captures rather
            // than guessed.
            let ratio = d.value / r.value;
            assert!(
                (ratio - 0.625).abs() < tol,
                "noise x{noise_scale}: recovered direct/reflection ratio {ratio:.3}, \
                 synthesised 0.625, tolerance {tol}"
            );

            // And the noise floor the thresholds are measured against.
            assert!(e.median_value > 0.0);
            assert_relative_eq!(e.prominence, e.peak_value / e.median_value, epsilon = 1e-9);
        }
    }

    /// A capture must be able to reproduce its own decision.
    ///
    /// Rank-by-strength alone cannot guarantee that. On the rig at 3 m the
    /// accepted arrival was weaker than `MAX_CANDIDATES` peaks of the
    /// reverberant cluster, so the list kept the cluster and dropped the
    /// arrival — and replaying the accept rule over the recorded candidates
    /// returned a different lag than the daemon chose, at every constant.
    /// Offline tuning is the entire reason the captures exist.
    ///
    /// The fixture is that failure in miniature: a cluster of arrivals all
    /// clearing `DIRECT_PEAK_FRACTION`, ordered so the **earliest** — the one
    /// the rule accepts — is also the weakest, and so the first to fall off a
    /// rank-ordered cut.
    #[test]
    fn the_accepted_lag_is_always_in_its_own_evidence() {
        let sig = white_noise(N, 0.5, 42);
        let cluster = MAX_CANDIDATES + 8;
        let first = 400usize;
        let spacing = 60usize;

        let mut meas = vec![0.0f32; N];
        for k in 0..cluster {
            // 0.55 rising to 1.0: every one clears the 6 dB window, and rank
            // is the exact reverse of arrival order.
            let gain = 0.55 + 0.45 * (k as f32) / (cluster as f32 - 1.0);
            add_delayed(&mut meas, &sig, first + k * spacing, gain);
        }

        let e = estimate_delay_detailed(&sig, &meas, SR);
        let lag = e.lag.expect("the cluster is far above the accept gate");
        assert!(
            (lag - first as i64).abs() < SUB_MS,
            "earliest-peak rule should take {first}, got {lag}"
        );
        assert!(
            e.candidates.len() >= MAX_CANDIDATES,
            "the fixture must overflow the rank cut to test anything"
        );
        assert!(
            e.candidates.iter().any(|c| c.lag == lag),
            "accepted lag {lag} absent from its own candidates {:?}",
            e.candidates.iter().map(|c| c.lag).collect::<Vec<_>>()
        );
        assert!(
            e.candidates.iter().any(|c| c.lag == e.peak_lag),
            "global peak {} absent from its own candidates",
            e.peak_lag
        );

        // Replaying the accept rule over the recorded evidence alone must
        // return what the estimator returned — that is the property the rig
        // session needs, and the one the strongest-32 cut destroyed.
        let floor = e.peak_value * DIRECT_PEAK_FRACTION;
        let replayed = e
            .candidates
            .iter()
            .filter(|c| c.lag >= 0 && c.value >= floor)
            .map(|c| c.lag)
            .min()
            .expect("evidence must contain at least one qualifying peak");
        assert_eq!(replayed, lag, "offline replay disagreed with the daemon");
    }

    /// The negative-lag floor is published, and it measures what it claims:
    /// a region a causal path puts no signal into.
    ///
    /// It decides nothing today. The next rig session decides whether it
    /// should — see `DelayEstimate::negative_lag_median`.
    #[test]
    fn the_negative_lag_floor_is_published_and_uncontaminated() {
        let sig = white_noise(N, 0.5, 42);
        let mut meas = white_noise(N, 0.02, 77);
        add_delayed(&mut meas, &sig, 283, 0.50);
        add_delayed(&mut meas, &sig, 1_455, 0.80);

        let e = estimate_delay_detailed(&sig, &meas, SR);
        assert!(e.lag.is_some());
        assert!(
            e.negative_lag_median > 0.0 && e.negative_lag_median < e.peak_value,
            "negative-lag median {} out of range against peak {}",
            e.negative_lag_median,
            e.peak_value
        );
        // Every arrival sits at a positive lag, so the all-lag median is the
        // one carrying reverberation; the negative-lag floor must not be
        // above it by any meaningful margin.
        assert!(
            e.negative_lag_median <= e.median_value * 1.10,
            "negative-lag median {} should not exceed the all-lag median {}",
            e.negative_lag_median,
            e.median_value
        );

        // A silent leg has no correlation to take any floor over.
        let e = estimate_delay_detailed(&sig, &vec![0.0f32; N], SR);
        assert_eq!(e.negative_lag_median, 0.0);
    }

    /// The clean single-peak case the whole existing suite is made of must
    /// be unmoved by the prominence rule — exactly, not within tolerance.
    #[test]
    fn single_unambiguous_peak_is_exact() {
        let sig = white_noise(N, 0.5, 42);
        let delay = 100;
        let mut meas = vec![0.0f32; N];
        add_delayed(&mut meas, &sig, delay, 1.0);

        assert_eq!(estimate_delay_samples(&sig, &meas, SR), Some(delay as i64));
    }

    #[test]
    fn single_pole_lowpass() {
        let ref_sig = white_noise(N, 0.5, 42);

        let fc = 2000.0_f64;
        let a = 1.0 - (-2.0 * PI * fc / SR as f64).exp();

        // Apply IIR: y[n] = a*x[n] + (1-a)*y[n-1]
        let mut meas = vec![0.0f32; N];
        let mut prev = 0.0_f64;
        for i in 0..N {
            let y = a * ref_sig[i] as f64 + (1.0 - a) * prev;
            meas[i] = y as f32;
            prev = y;
        }

        let r = h1_estimate(&ref_sig, &meas, SR);

        // Analytical: H(z) = a / (1 - (1-a)*z^{-1})
        let spot_checks: &[(f64, f64)] =
            &[(200.0, 0.5), (2000.0, 0.5), (10000.0, 1.0), (20000.0, 1.5)];
        for &(freq, tol) in spot_checks {
            let w = 2.0 * PI * freq / SR as f64;
            let z_inv = Complex::new(w.cos(), -w.sin());
            let denom = Complex::new(1.0, 0.0) - z_inv * (1.0 - a);
            let h = Complex::new(a, 0.0) / denom;
            let expected_db = 20.0 * h.norm().log10();
            let k = freq.round() as usize;
            assert!(
                (r.magnitude_db[k] - expected_db).abs() < tol,
                "f={freq}: got {:.2} dB, expected {:.2} dB",
                r.magnitude_db[k],
                expected_db
            );
        }
    }

    // ---- Noise & coherence ----

    #[test]
    fn noise_robustness() {
        let ref_sig = white_noise(N, 0.5, 42);
        let noise = white_noise(N, 0.05, 99);
        let meas: Vec<f32> = ref_sig.iter().zip(&noise).map(|(&s, &n)| s + n).collect();

        let r = h1_estimate(&ref_sig, &meas, SR);

        let range = 50..=20_000;
        let count = range.clone().count() as f64;
        let mean_mag_err: f64 = range.clone().map(|k| r.magnitude_db[k].abs()).sum::<f64>() / count;
        assert!(
            mean_mag_err < 0.5,
            "mean |mag error| {:.3} dB",
            mean_mag_err
        );
        let mean_coh: f64 = range.map(|k| r.coherence[k]).sum::<f64>() / count;
        assert!(mean_coh > 0.95, "mean coherence {:.4}", mean_coh);
    }

    #[test]
    fn coherence_uncorrelated() {
        let a = white_noise(N, 0.5, 42);
        let b = white_noise(N, 0.5, 99);
        let r = h1_estimate(&a, &b, SR);

        let mean_coh: f64 = r.coherence[1..].iter().sum::<f64>() / (r.coherence.len() - 1) as f64;
        assert!(
            mean_coh < 0.4,
            "uncorrelated signals should have low coherence, got {:.4}",
            mean_coh
        );
    }

    /// Phase 4b round-trip: a flat-spectrum H(ω) (Re ≡ 1, Im ≡ 0)
    /// represents an ideal unit-impulse system. The IFFT must recover
    /// a time-domain h(t) with a single positive peak centred at the
    /// middle of the array (after the centring shift) and ~zero
    /// energy elsewhere.
    #[test]
    fn impulse_response_recovers_unit_impulse() {
        // 4097 freq bins → 8192-sample IR (1 s at 8 kHz, etc.).
        let nfft = 4097;
        let re = vec![1.0; nfft];
        let im = vec![0.0; nfft];
        let ir = impulse_response_from_h(&re, &im);
        assert_eq!(ir.len(), (nfft - 1) * 2);
        let n = ir.len();
        let mid = n / 2;
        // The peak must be at the centre.
        let (peak_idx, peak_val) = ir
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap();
        assert_eq!(peak_idx, mid, "peak index {peak_idx}, expected {mid}");
        assert!(
            *peak_val > 0.0,
            "peak value should be positive, got {peak_val}"
        );
        // Off-peak energy must be ~zero (Re=1 IFFT is a Dirac delta).
        for (i, v) in ir.iter().enumerate() {
            if i != mid {
                assert!(v.abs() < 1e-3, "non-peak bin {i} = {v} (expected ~0)",);
            }
        }
    }

    /// Empty / mismatched inputs are defensive returns of Vec::new(),
    /// not panics — the daemon emits IR sidecar frames every tick and
    /// must not crash on edge cases (empty re/im on cold start, etc.).
    #[test]
    fn impulse_response_empty_inputs_yield_empty() {
        assert!(impulse_response_from_h(&[], &[]).is_empty());
        assert!(impulse_response_from_h(&[1.0], &[]).is_empty());
        assert!(impulse_response_from_h(&[1.0, 2.0], &[0.0]).is_empty());
        // Single-bin input is too short to IFFT meaningfully.
        assert!(impulse_response_from_h(&[1.0], &[0.0]).is_empty());
    }
}
