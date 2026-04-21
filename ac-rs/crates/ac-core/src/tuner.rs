//! Drum-head fundamental identifier.
//!
//! A struck drum is tonal but inharmonic: the loudest spectral peak is
//! usually the (1,1) mode, not the true (0,1) membrane fundamental that
//! governs pitch perception. Picking the argmax would report the wrong
//! note. Instead we find all peaks above the noise floor and score each
//! candidate `f0` by how well the surrounding peaks match the Bessel-zero
//! ratios of an ideal circular membrane. The candidate whose overtone
//! stack best explains the observed energy wins.
//!
//! Input is a peak-hold dBFS spectrum (the live spectrum decays too fast
//! after a drum hit to be useful). Output is `Option<FundamentalCandidate>`
//! with a confidence score in `[0, 1]`.

/// Mode frequencies of an ideal circular membrane, expressed as the ratio
/// `f(m,n) / f(0,1)` where `(m, n)` = (nodal diameters, nodal circles).
/// Values are zeros of Bessel functions of the first kind. Real drums
/// deviate by ±a few percent from these ratios due to bending stiffness
/// and air loading; the matcher allows ±5%.
pub const MEMBRANE_MODES: &[(u8, u8, f64)] = &[
    (0, 1, 1.000),
    (1, 1, 1.594),
    (2, 1, 2.136),
    (0, 2, 2.296),
    (3, 1, 2.653),
    (1, 2, 2.918),
    (4, 1, 3.156),
    (2, 2, 3.501),
    (0, 3, 3.600),
    (5, 1, 3.652),
    (3, 2, 4.060),
];

/// A single overtone that was matched against one of the `MEMBRANE_MODES`
/// ratios for a given candidate fundamental.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Partial {
    pub mode: (u8, u8),
    pub ideal_ratio: f64,
    pub measured_hz: f64,
    pub measured_ratio: f64,
    /// `(measured_ratio - ideal_ratio) / ideal_ratio * 100`.
    pub deviation_pct: f64,
    pub magnitude_db: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FundamentalCandidate {
    pub freq_hz: f64,
    /// Fraction of in-band peak energy (above floor, linear in dB) that
    /// is explained by membrane-mode ratios around `freq_hz`. Clamped to
    /// `[0, 1]`. Drums with a clean overtone stack score above ~0.6.
    pub confidence: f64,
    /// Matched overtones sorted by `ideal_ratio`. Entry 0 is the (0,1)
    /// fundamental itself.
    pub partials: Vec<Partial>,
}

/// Per-candidate scoring trace — survives the physicality gates and is
/// emitted by [`identify_fundamental_with_candidates`] so callers (daemon
/// debug log, tests) can see why the identifier picked what it picked
/// without re-running the scorer. Not written on every frame — only
/// produced when the caller asks for the diagnostic variant.
#[derive(Debug, Clone)]
pub struct CandidateDiag {
    pub f0: f64,
    /// Candidate came from a detected local-maximum peak (true) or was
    /// derived as `peak.freq / ratio` (false).
    pub has_peak: bool,
    /// Score used for ranking — sum of `(mag_db - floor_db) * fit` over
    /// matched partials.
    pub score: f64,
    pub matched: Vec<Partial>,
    /// Ideal ratio of the loudest matched partial.
    pub loudest_ratio: f64,
    /// Max magnitude in dBFS across matched partials with `ideal_ratio <= 1.65`.
    /// `NEG_INFINITY` when no low-mode match exists.
    pub low_mode_max_db: f64,
}

/// Tolerance when matching a measured peak against an ideal mode ratio.
const MATCH_TOL: f64 = 0.05;

/// Stack-level prominence gate — at least one of the candidate's
/// matched partials (including (0,1) itself) must be within this many
/// dB of the loudest peak in range. Loose enough to accept the (0,1)
/// of a heavily-damped drum; tight enough to reject a bass artefact
/// whose entire mode stack is composed of floor-noise wiggles.
const MATCHED_PROMINENCE_DB: f64 = 25.0;

/// Minimum prominence the candidate f0 peak itself must clear, relative
/// to the loudest peak in the search range. A sub-harmonic that would
/// otherwise aggregate a high score by hijacking the real peak through
/// ratio aliasing must still be loud enough to plausibly be the true
/// fundamental. 30 dB keeps the sub-harmonic reject intact (typical
/// aliasing artefacts land 30+ dB below the dominant) while leaving
/// headroom for real drum (0,1)s, which can sit 20–25 dB below the
/// (1,1) argmax on damped or low-tuned heads.
const F0_PROMINENCE_DB: f64 = 30.0;

#[derive(Debug, Clone, Copy)]
struct Peak {
    freq_hz: f64,
    magnitude_db: f64,
}

/// Return the sub-bin-accurate peak built from the quadratic through
/// `(y_left, y_mid, y_right)` at the bin spacing given by `freqs[i-1..=i+1]`.
fn interpolate(y_l: f64, y_m: f64, y_r: f64, f_l: f64, f_m: f64, f_r: f64) -> (f64, f64) {
    let denom = y_l - 2.0 * y_m + y_r;
    // Degenerate parabola (flat top): fall back to the bin centre.
    if denom.abs() < 1e-12 {
        return (f_m, y_m);
    }
    let delta = 0.5 * (y_l - y_r) / denom;
    // `delta` is in units of "bin spacing". The two spacings around the
    // centre bin can differ slightly for non-uniform grids; use the
    // appropriate side so we stay accurate near the FFT's linear grid too.
    let span = if delta >= 0.0 { f_r - f_m } else { f_m - f_l };
    let f = f_m + delta.signum() * delta.abs() * span;
    let peak_db = y_m - 0.25 * (y_l - y_r) * delta;
    (f, peak_db)
}

fn find_peaks(spectrum_db: &[f32], freqs_hz: &[f32], floor_db: f32, range: (f64, f64)) -> Vec<Peak> {
    let n = spectrum_db.len().min(freqs_hz.len());
    if n < 3 {
        return Vec::new();
    }
    let (fmin, fmax) = range;
    let mut out = Vec::new();
    for i in 1..n - 1 {
        let f_m = freqs_hz[i] as f64;
        if f_m < fmin || f_m > fmax {
            continue;
        }
        let y_m = spectrum_db[i];
        if !y_m.is_finite() || y_m <= floor_db {
            continue;
        }
        let y_l = spectrum_db[i - 1];
        let y_r = spectrum_db[i + 1];
        if !(y_l.is_finite() && y_r.is_finite() && y_m > y_l && y_m > y_r) {
            continue;
        }
        let (f, db) = interpolate(
            y_l as f64,
            y_m as f64,
            y_r as f64,
            freqs_hz[i - 1] as f64,
            f_m,
            freqs_hz[i + 1] as f64,
        );
        out.push(Peak { freq_hz: f, magnitude_db: db });
    }
    out
}

/// Return `true` when `direct_f0` sits within `MATCH_TOL` of
/// `derived_f0 * r` for some membrane mode ratio `r >= 1.594`. Used by the
/// tie-break rule: a direct peak that happens to be an overtone of the
/// derived candidate is evidence *for* the derived f0, not against it.
fn is_overtone_of(derived_f0: f64, direct_f0: f64) -> bool {
    if derived_f0 <= 0.0 || direct_f0 <= 0.0 {
        return false;
    }
    let ratio = direct_f0 / derived_f0;
    for &(_, _, r) in MEMBRANE_MODES {
        if r <= 1.001 {
            continue;
        }
        if ((ratio - r) / r).abs() <= MATCH_TOL {
            return true;
        }
    }
    false
}

/// Identify the `(0,1)` membrane fundamental in `spectrum_db`.
///
/// `spectrum_db` and `freqs_hz` must have the same length and describe a
/// peak-hold (or other stationary) dBFS spectrum. `floor_db` is the noise
/// floor below which local maxima are ignored — typically
/// `median(spectrum_db) + 12.0`. `search_range_hz` constrains the candidate
/// fundamental search; 40..2000 Hz covers kick through piccolo.
///
/// Returns `None` if no peaks are above the floor.
pub fn identify_fundamental(
    spectrum_db: &[f32],
    freqs_hz: &[f32],
    floor_db: f32,
    search_range_hz: (f64, f64),
) -> Option<FundamentalCandidate> {
    identify_fundamental_with_candidates(spectrum_db, freqs_hz, floor_db, search_range_hz).0
}

/// Same as [`identify_fundamental`] but also returns a per-candidate trace
/// of every candidate that survived the physicality gates. Used by the
/// daemon's `AC_TUNER_DEBUG` log path and by tests — the diagnostic Vec is
/// allocated unconditionally, so callers on the hot path should keep using
/// [`identify_fundamental`] to avoid the tiny extra work.
pub fn identify_fundamental_with_candidates(
    spectrum_db: &[f32],
    freqs_hz: &[f32],
    floor_db: f32,
    search_range_hz: (f64, f64),
) -> (Option<FundamentalCandidate>, Vec<CandidateDiag>) {
    let peaks = find_peaks(spectrum_db, freqs_hz, floor_db, search_range_hz);
    if peaks.is_empty() {
        return (None, Vec::new());
    }

    let total_above_floor: f64 = peaks
        .iter()
        .map(|p| (p.magnitude_db - floor_db as f64).max(0.0))
        .sum();
    if total_above_floor <= 0.0 {
        return (None, Vec::new());
    }
    let loudest_in_range = peaks
        .iter()
        .map(|p| p.magnitude_db)
        .fold(f64::NEG_INFINITY, f64::max);

    // Build the candidate-f0 set. Each detected peak is a candidate, plus
    // `peak / ratio` for every mode ratio > 1 — the derived half rescues
    // a real (0,1) that the local-max finder missed because the
    // fundamental is a broad bump (common at large FFT N) or sits 30+ dB
    // below the (1,1) argmax on damped heads. Without them the identifier
    // can only pick an overtone as f0 and reports 100–200 Hz high.
    let (fmin, fmax) = search_range_hz;
    #[derive(Clone, Copy)]
    struct Cand {
        f0: f64,
        f0_mag: f64,
        has_peak: bool,
    }
    let mut cand_set: Vec<Cand> = Vec::with_capacity(peaks.len() * (MEMBRANE_MODES.len() + 1));
    for p in &peaks {
        cand_set.push(Cand { f0: p.freq_hz, f0_mag: p.magnitude_db, has_peak: true });
        for &(_, _, ratio) in MEMBRANE_MODES {
            if ratio <= 1.001 {
                continue;
            }
            let f0 = p.freq_hz / ratio;
            if f0 < fmin || f0 > fmax {
                continue;
            }
            cand_set.push(Cand { f0, f0_mag: f64::NEG_INFINITY, has_peak: false });
        }
    }
    cand_set.sort_by(|a, b| a.f0.partial_cmp(&b.f0).unwrap_or(std::cmp::Ordering::Equal));
    let mut dedup: Vec<Cand> = Vec::with_capacity(cand_set.len());
    for c in cand_set {
        if let Some(p) = dedup.last().copied() {
            if (c.f0 - p.f0).abs() / p.f0.max(1e-9) < 0.005 {
                // Prefer a real detected peak over a derived duplicate.
                if c.has_peak && !p.has_peak {
                    *dedup.last_mut().unwrap() = c;
                }
                continue;
            }
        }
        dedup.push(c);
    }

    // Score each candidate f0. For every membrane mode ratio, look for a
    // peak within ±5% of f0 * ratio; reward by that peak's dB above floor.
    // A drum with clean overtones lights up many modes; a random lone
    // peak scores only the fundamental slot.
    let mut best: Option<(f64, FundamentalCandidate, f64, bool)> = None;
    let mut diag: Vec<CandidateDiag> = Vec::new();
    for cand in &dedup {
        let f0 = cand.f0;
        if f0 <= 0.0 {
            continue;
        }
        // f0 itself must be a prominent peak — but only for candidates
        // that *are* a detected peak. Derived candidates (peak/ratio) are
        // precisely the case where (0,1) lacks a prominent peak, so this
        // gate would defeat the point.
        if cand.has_peak && cand.f0_mag < loudest_in_range - F0_PROMINENCE_DB {
            continue;
        }
        let mut matched: Vec<Partial> = Vec::new();
        let mut score = 0.0_f64;
        let mut used = vec![false; peaks.len()];
        for &(m, n, ratio) in MEMBRANE_MODES {
            let target = f0 * ratio;
            // Prefer the peak with the smallest relative deviation from
            // target, not the loudest — a loud non-matching peak would
            // otherwise poison the match for a nearby overtone mode. Skip
            // peaks already consumed by an earlier mode so a lone strong
            // peak can't get credited to two neighbouring ratios at once
            // (the classic sub-harmonic amplifier: f0/2 sees the real
            // peak as both (2,1) and (0,2) and wins on duplicate score).
            let mut best_p: Option<(f64, usize, Peak)> = None;
            for (idx, p) in peaks.iter().enumerate() {
                if used[idx] {
                    continue;
                }
                let dev = (p.freq_hz - target) / target;
                if dev.abs() <= MATCH_TOL {
                    let d = dev.abs();
                    if best_p.map(|(x, _, _)| d < x).unwrap_or(true) {
                        best_p = Some((d, idx, *p));
                    }
                }
            }
            if let Some((d_abs, idx, p)) = best_p {
                used[idx] = true;
                let dev_pct = (p.freq_hz / target - 1.0) * 100.0;
                // Weight the score contribution by how cleanly the peak
                // aligns with the ideal ratio. A zero-deviation match
                // counts in full; a match at the ±5% edge counts half.
                // This prevents a derived candidate (peak / ratio) from
                // winning by coincidentally aligning many peaks at the
                // edge of tolerance when a direct candidate fits the
                // same peaks at zero deviation.
                let fit = 1.0 - 0.5 * (d_abs / MATCH_TOL);
                score += (p.magnitude_db - floor_db as f64).max(0.0) * fit;
                matched.push(Partial {
                    mode: (m, n),
                    ideal_ratio: ratio,
                    measured_hz: p.freq_hz,
                    measured_ratio: p.freq_hz / f0,
                    deviation_pct: dev_pct,
                    magnitude_db: p.magnitude_db,
                });
            }
        }

        // Must at least match the fundamental itself — otherwise the
        // candidate is below its own search window after interpolation.
        if matched.is_empty() {
            continue;
        }

        // Prominence gate: reject candidates whose entire matched stack
        // sits far below the dominant peak in range. Otherwise a quiet
        // sub-harmonic (e.g. PSU hum at 47 Hz) with a handful of random
        // low-freq noise matches can out-score a clean dominant tone
        // whose only match is itself.
        let max_matched = matched
            .iter()
            .map(|p| p.magnitude_db)
            .fold(f64::NEG_INFINITY, f64::max);
        if max_matched < loudest_in_range - MATCHED_PROMINENCE_DB {
            continue;
        }

        // Physicality gate: the loudest matched partial must be a
        // low-order mode. Real drums put their energy in (0,1), (1,1),
        // (2,1), or (0,2) — higher modes are always quieter. A derived
        // candidate like f0 = peak / 3.501 that farms the dominant peak
        // as a (3,2) overtone plus random noise matches violates this
        // and is the pattern we want to reject.
        let loudest_ratio = matched
            .iter()
            .find(|p| (p.magnitude_db - max_matched).abs() < 1e-9)
            .map(|p| p.ideal_ratio)
            .unwrap_or(0.0);
        if loudest_ratio > 2.3 {
            continue;
        }
        // Low-mode energy requirement: if the loudest match is beyond
        // (1,1), a (0,1) or (1,1) match must still exist within 25 dB
        // of max_matched. Without this, a candidate whose only real
        // evidence is a single high-order overtone (e.g. peak/2.136)
        // farms (0,1) and (1,1) from noise and out-scores the direct
        // candidate on that loud peak.
        let low_mode_max = matched
            .iter()
            .filter(|p| p.ideal_ratio <= 1.65)
            .map(|p| p.magnitude_db)
            .fold(f64::NEG_INFINITY, f64::max);
        if loudest_ratio > 1.65 && low_mode_max < max_matched - 25.0 {
            continue;
        }

        // Prominence scaling: candidates whose loudest matched partial IS
        // the loudest peak in the search range are explaining the drum's
        // dominant signal. Candidates whose matched stack sits far below
        // the range's loudest peak are farming tail harmonics of a
        // different fundamental — at high FFT N they can rack up 10+
        // upper-bin matches and out-score the correct candidate whose
        // (0,1) IS the dominant ring. Scale score by the dominance gap
        // so the existing ranking picks "the peak the ear actually hears"
        // first.
        let prominence = {
            let dominance = (loudest_in_range - max_matched).max(0.0);
            (1.0 - 0.04 * dominance).clamp(0.50, 1.0)
        };
        let score = score * prominence;

        // Raw energy ratio is 1.0 whenever every peak above the floor happens
        // to fall near a mode ratio — which includes the degenerate case of
        // a single isolated peak. Weight by matched-partial count so a thin
        // spectrum scores proportionally low: 4+ partials is enough to say
        // "yes, this is a drum"; fewer earns a penalty.
        let energy_ratio = (score / total_above_floor).clamp(0.0, 1.0);
        let stack_weight = (matched.len() as f64 / 4.0).min(1.0);
        let confidence = (energy_ratio * stack_weight).clamp(0.0, 1.0);
        // Tie-break mag: for derived candidates the loudest matched
        // partial stands in for a (missing) f0 peak.
        let f0_mag = if cand.has_peak {
            cand.f0_mag
        } else {
            matched
                .iter()
                .map(|p| p.magnitude_db)
                .fold(f64::NEG_INFINITY, f64::max)
        };
        let this = FundamentalCandidate {
            freq_hz: f0,
            confidence,
            partials: matched.clone(),
        };
        diag.push(CandidateDiag {
            f0,
            has_peak: cand.has_peak,
            score,
            matched,
            loudest_ratio,
            low_mode_max_db: low_mode_max,
        });

        // Tie-break: higher score wins. On near-tie:
        //   1. If exactly one candidate is derived (has_peak=false) and the
        //      direct candidate's f0 sits on one of the derived candidate's
        //      membrane-mode ratios (within MATCH_TOL), prefer the derived
        //      one — but ONLY when the derived stack has ≥2 matched partials,
        //      i.e. independent evidence beyond just the direct peak itself.
        //      Otherwise every lone peak at f would spawn a derived (1,1)
        //      candidate at f/1.594 and steal the ID with zero extra support
        //      (see rejects_quiet_subharmonic_with_dominant_peak).
        //   2. Otherwise prefer the louder f0 (derived candidates substitute
        //      the loudest matched partial for a missing f0 peak); on equal
        //      mag prefer higher f0. Previous "prefer lower f0" rule
        //      backfired — a sub-harmonic bin whose mode-stack hits the real
        //      fundamental as 2×/(2,1) lands one octave low.
        let replace = match &best {
            None => true,
            Some((best_score, best_cand, best_mag, best_has_peak)) => {
                if score > *best_score * 1.001 {
                    true
                } else if (score - *best_score).abs() <= best_score * 0.001 {
                    let here_n = this.partials.len();
                    let best_n = best_cand.partials.len();
                    match (cand.has_peak, *best_has_peak) {
                        (false, true)
                            if here_n >= 2 && is_overtone_of(f0, best_cand.freq_hz) =>
                        {
                            true
                        }
                        (true, false)
                            if best_n >= 2 && is_overtone_of(best_cand.freq_hz, f0) =>
                        {
                            false
                        }
                        _ => {
                            f0_mag > *best_mag
                                || (f0_mag == *best_mag && f0 > best_cand.freq_hz)
                        }
                    }
                } else {
                    false
                }
            }
        };
        if replace {
            best = Some((score, this, f0_mag, cand.has_peak));
        }
    }

    (best.map(|(_, c, _, _)| c), diag)
}

/// Parameters for the stateful [`TunerState`] trigger/history pipeline.
/// Separated from the pure [`identify_fundamental`] fn so the scorer can
/// be re-used without forcing a state object on callers that just want
/// a one-shot analysis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TunerConfig {
    /// Fundamental search range, Hz. Outside this band a candidate is
    /// rejected even if it scores well — overtones may still fall above.
    pub search_range_hz: (f64, f64),
    /// Noise-floor offset above the per-frame median, in dB.
    pub floor_over_median_db: f32,
    /// Rising-edge dB delta (current − baseline) that arms a fresh analysis.
    pub trigger_delta_db: f32,
    /// Level must fall back within `rearm_delta_db` of baseline before the
    /// next trigger can fire. Hysteresis keeps a ringing tone from
    /// retriggering every frame.
    pub rearm_delta_db: f32,
    /// EMA time-constant for the trigger baseline, seconds.
    pub baseline_tau_s: f32,
    /// Max entries kept in the recent-hits history ring.
    pub history_cap: usize,
    /// Hits closer than this fraction to the prior hit are deduped.
    pub history_dedupe_frac: f64,
    /// Below this confidence a triggered candidate is dropped instead
    /// of being stored — a wrong-but-labelled peak is worse than none.
    pub min_confidence: f64,
    /// dB/s per-bin release rate for the internal peak-hold. Applied every
    /// frame, clamped so a bin never drops below the current live reading
    /// (so rising bins still track the live spectrum up). The old design
    /// only decayed after an "idle" window with zero rising bins, but in
    /// real audio ambient noise keeps at least one bin rising every frame,
    /// so the peak-hold would saturate upward indefinitely — its median
    /// then dragged the identifier's noise floor up until drum partials
    /// fell below it. Constant decay sidesteps that.
    pub peak_release_db_per_sec: f32,
    /// Absolute-level gate: a candidate is rejected unless the peak level
    /// across the search band is `≥` this dBFS. Kills noise-floor fires
    /// where `delta` crosses the trigger on ambient wiggle. `None` disables
    /// the gate — the default. Typical tuning: `Some(-45.0)` for close-mic
    /// drums, `Some(-60.0)` for distant or quiet mics.
    pub min_level_dbfs: Option<f32>,
}

impl Default for TunerConfig {
    fn default() -> Self {
        Self {
            search_range_hz: (40.0, 2000.0),
            floor_over_median_db: 12.0,
            trigger_delta_db: 6.0,
            rearm_delta_db: 3.0,
            baseline_tau_s: 2.0,
            history_cap: 5,
            history_dedupe_frac: 0.015,
            min_confidence: 0.25,
            // 8 dB/s: a -30 dB peak takes ~3 s to fall below a -50 dB
            // floor, long enough to hold a drum ring across the analysis
            // window but fast enough that the peak-hold median tracks the
            // actual noise floor within a few seconds of silence.
            peak_release_db_per_sec: 8.0,
            min_level_dbfs: None,
        }
    }
}

/// Per-channel tuner state machine. Owns the EMA trigger baseline, the
/// armed/disarmed flag, the recent-hit history ring, and an optional
/// range-lock that narrows the search window after a confirmed hit.
/// A fresh `feed()` of a peak-hold dBFS spectrum either fires a trigger
/// (running [`identify_fundamental`]) or bumps the baseline and returns
/// `Triggered::No`.
#[derive(Debug, Clone)]
pub struct TunerState {
    cfg: TunerConfig,
    baseline_db: f32,
    armed: bool,
    last: Option<FundamentalCandidate>,
    history: std::collections::VecDeque<f64>,
    range_lock: Option<(f64, f64)>,
    /// Internal per-bin peak-hold accumulator. Sized to match the last
    /// spectrum fed in; re-seeded on bin-count change.
    peak_hold: Vec<f32>,
    /// Last [`LevelStatus`] snapshot, refreshed on every `feed`.
    last_status: LevelStatus,
}

/// Result of a single [`TunerState::feed`] call.
#[derive(Debug, Clone, PartialEq)]
pub enum Triggered {
    /// No trigger this frame — baseline tracked, state otherwise unchanged.
    No,
    /// Level crossed the trigger threshold and the identifier ran.
    /// `candidate` carries the result (possibly `None` if the spectrum is
    /// degenerate) and `confident` is `true` when the candidate cleared
    /// `cfg.min_confidence` and was appended to history.
    Fired {
        candidate: Option<FundamentalCandidate>,
        confident: bool,
    },
}

/// Per-frame diagnostic snapshot updated by every [`TunerState::feed`] call.
/// Read via [`TunerState::status`] — lets callers (daemon, test harness)
/// log what the trigger/identifier is actually seeing without having to
/// duplicate the level math. Purely informational; the state machine works
/// off the internal fields, not this struct.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LevelStatus {
    /// Loudest live-spectrum bin inside `search_range_hz`, dBFS.
    pub current_db: f32,
    /// EMA baseline of `current_db`.
    pub baseline_db: f32,
    /// `current_db - baseline_db`. Trigger fires when this clears
    /// `trigger_delta_db` AND `armed`.
    pub delta_db: f32,
    /// Whether the edge detector is armed. Cleared after a fire, re-armed
    /// when delta falls below `rearm_delta_db`.
    pub armed: bool,
    /// Identifier noise floor for this frame (`median(live) + offset`).
    pub floor_db: f32,
    /// Peaks the identifier found above floor in the peak-hold buffer.
    pub peak_count: usize,
    /// Fundamental of the last candidate the identifier returned — `NaN`
    /// when none. Written on every trigger attempt, not just confident ones.
    pub last_candidate_hz: f32,
    /// Confidence of the last candidate — `NaN` when none.
    pub last_confidence: f32,
}

impl TunerState {
    pub fn new(cfg: TunerConfig) -> Self {
        Self {
            cfg,
            baseline_db: f32::NEG_INFINITY,
            armed: true,
            last: None,
            history: std::collections::VecDeque::new(),
            range_lock: None,
            peak_hold: Vec::new(),
            last_status: LevelStatus::default(),
        }
    }

    /// Diagnostic snapshot of the most recent [`feed`](Self::feed) call:
    /// level detector state, noise floor, peak count, last candidate freq /
    /// confidence. Lets callers trace why a trigger did or didn't fire
    /// without re-implementing the math.
    pub fn status(&self) -> LevelStatus {
        self.last_status
    }

    /// Read-only view of the internal peak-hold buffer — exposed so
    /// rendering code can display the same accumulator the tuner sees.
    pub fn peak_hold(&self) -> &[f32] {
        &self.peak_hold
    }

    pub fn config(&self) -> &TunerConfig {
        &self.cfg
    }
    pub fn baseline_db(&self) -> f32 {
        self.baseline_db
    }
    pub fn armed(&self) -> bool {
        self.armed
    }
    pub fn last(&self) -> Option<&FundamentalCandidate> {
        self.last.as_ref()
    }
    pub fn history(&self) -> &std::collections::VecDeque<f64> {
        &self.history
    }
    pub fn range_lock(&self) -> Option<(f64, f64)> {
        self.range_lock
    }
    pub fn set_range_lock(&mut self, range: Option<(f64, f64)>) {
        self.range_lock = range;
    }

    /// Replace the active config. Leaves level-detector state intact so a
    /// live sensitivity tweak doesn't disarm or clear the baseline EMA.
    pub fn set_config(&mut self, cfg: TunerConfig) {
        self.cfg = cfg;
    }

    /// Clear baseline, disarm flag, last candidate, history, and the
    /// internal peak-hold buffer. Range lock is left alone — the caller
    /// decides whether a reset should also drop the lock.
    pub fn reset(&mut self) {
        self.baseline_db = f32::NEG_INFINITY;
        self.armed = true;
        self.last = None;
        self.history.clear();
        self.peak_hold.clear();
    }

    /// Feed a fresh dBFS spectrum (linear-binned or log-aggregated both
    /// work; `freqs_hz` must agree in length). The internal peak-hold
    /// accumulator takes a bin-wise max; after `cfg.peak_hold_idle_s`
    /// without any bin rising, the held trace decays toward the live
    /// spectrum. Returns whether a trigger fired this frame.
    pub fn feed(&mut self, spectrum_db: &[f32], freqs_hz: &[f32], dt_s: f32) -> Triggered {
        if spectrum_db.is_empty() || spectrum_db.len() != freqs_hz.len() {
            return Triggered::No;
        }
        let dt = dt_s.clamp(1e-4, 0.5);

        // --- Peak-hold accumulation (leaky) ---
        // Always decay: `held = max(held - drop, fresh)`. Rising bins follow
        // the live spectrum up; falling/steady bins bleed off at
        // `peak_release_db_per_sec`. No idle gate — gating on "any bin rose"
        // never released in real audio because ambient noise kept at least
        // one bin rising every frame, and an unbounded peak-hold dragged the
        // identifier's median-based noise floor up over drum partials.
        if self.peak_hold.len() != spectrum_db.len() {
            self.peak_hold = spectrum_db.to_vec();
        } else {
            let drop = self.cfg.peak_release_db_per_sec * dt;
            for (held, &fresh) in self.peak_hold.iter_mut().zip(spectrum_db.iter()) {
                if fresh.is_finite() {
                    *held = (*held - drop).max(fresh);
                }
            }
        }

        // --- Level detector on LIVE spectrum (not peak-hold) ---
        // Peak-hold is used by the identifier (needs the transient preserved
        // across the analysis window). Trigger detection on peak-hold would
        // saturate at the first hit in real-world noise — ambient flicker
        // keeps `any_rose` true so peak-hold never decays, baseline climbs to
        // match, and subsequent hits can't beat the previous one. Running the
        // edge detector on the live spectrum fixes that: the live trace
        // actually falls between hits.
        let (fmin, fmax) = self.cfg.search_range_hz;
        let mut current = f32::NEG_INFINITY;
        for (f, &m) in freqs_hz.iter().zip(spectrum_db.iter()) {
            let fh = *f as f64;
            if fh >= fmin && fh <= fmax && m.is_finite() && m > current {
                current = m;
            }
        }
        if !current.is_finite() {
            self.last_status = LevelStatus {
                current_db: f32::NAN,
                baseline_db: self.baseline_db,
                delta_db: f32::NAN,
                armed: self.armed,
                floor_db: f32::NAN,
                peak_count: 0,
                last_candidate_hz: f32::NAN,
                last_confidence: f32::NAN,
            };
            return Triggered::No;
        }
        if !self.baseline_db.is_finite() {
            self.baseline_db = current;
        }
        let alpha = 1.0 - (-dt / self.cfg.baseline_tau_s).exp();
        if current < self.baseline_db {
            self.baseline_db = current;
        } else {
            self.baseline_db += alpha * (current - self.baseline_db);
        }
        let delta = current - self.baseline_db;
        if !self.armed && delta < self.cfg.rearm_delta_db {
            self.armed = true;
        }
        // Noise floor from the LIVE spectrum, not peak-hold. Peak-hold's
        // median is inflated by every transient that has ever hit the bin,
        // which would hide drum partials behind a floor that climbed above
        // them.
        let floor = median_f32_local(spectrum_db)
            .map(|m| m + self.cfg.floor_over_median_db)
            .unwrap_or(-60.0);
        let level_ok = self
            .cfg
            .min_level_dbfs
            .map(|thr| current >= thr)
            .unwrap_or(true);
        if !(self.armed && delta >= self.cfg.trigger_delta_db && level_ok) {
            self.last_status = LevelStatus {
                current_db: current,
                baseline_db: self.baseline_db,
                delta_db: delta,
                armed: self.armed,
                floor_db: floor,
                peak_count: 0,
                last_candidate_hz: f32::NAN,
                last_confidence: f32::NAN,
            };
            return Triggered::No;
        }
        let range = self.range_lock.unwrap_or(self.cfg.search_range_hz);
        let peak_count = find_peaks(&self.peak_hold, freqs_hz, floor, range).len();
        let cand = identify_fundamental(&self.peak_hold, freqs_hz, floor, range);
        let confident = cand
            .as_ref()
            .is_some_and(|c| c.confidence >= self.cfg.min_confidence);
        let (cand_hz, cand_conf) = cand
            .as_ref()
            .map(|c| (c.freq_hz as f32, c.confidence as f32))
            .unwrap_or((f32::NAN, f32::NAN));
        if let Some(c) = &cand {
            if confident {
                let dup = self.history.back().is_some_and(|&prev| {
                    (c.freq_hz - prev).abs() / prev.max(1.0) < self.cfg.history_dedupe_frac
                });
                if !dup {
                    self.history.push_back(c.freq_hz);
                    while self.history.len() > self.cfg.history_cap {
                        self.history.pop_front();
                    }
                }
            }
        }
        self.last = cand.clone();
        self.armed = false;
        self.last_status = LevelStatus {
            current_db: current,
            baseline_db: self.baseline_db,
            delta_db: delta,
            armed: self.armed,
            floor_db: floor,
            peak_count,
            last_candidate_hz: cand_hz,
            last_confidence: cand_conf,
        };
        Triggered::Fired { candidate: cand, confident }
    }
}

fn median_f32_local(samples: &[f32]) -> Option<f32> {
    let mut v: Vec<f32> = samples.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a linear-grid synthetic spectrum: `bin_hz` wide, `n` bins.
    /// Floor is `-80 dB`; each `(freq, db)` tuple adds a triangle peak
    /// centred on the nearest bin with ±1-bin shoulders so the parabolic
    /// interpolator has something to work with.
    fn synth(bin_hz: f64, n: usize, peaks: &[(f64, f64)]) -> (Vec<f32>, Vec<f32>) {
        let mut spec = vec![-80.0_f32; n];
        let freqs: Vec<f32> = (0..n).map(|i| (i as f64 * bin_hz) as f32).collect();
        for &(f, db) in peaks {
            let idx = (f / bin_hz).round() as isize;
            if idx <= 0 || (idx as usize) >= n - 1 {
                continue;
            }
            let i = idx as usize;
            let db = db as f32;
            spec[i] = spec[i].max(db);
            spec[i - 1] = spec[i - 1].max(db - 6.0);
            spec[i + 1] = spec[i + 1].max(db - 6.0);
        }
        (spec, freqs)
    }

    fn cents(a: f64, b: f64) -> f64 {
        1200.0 * (a / b).log2()
    }

    #[test]
    fn identifies_clean_fundamental() {
        let (spec, freqs) = synth(
            1.0,
            4096,
            &[
                (200.0, -10.0),
                (200.0 * 1.594, -14.0),
                (200.0 * 2.136, -18.0),
                (200.0 * 2.296, -20.0),
            ],
        );
        let c = identify_fundamental(&spec, &freqs, -50.0, (40.0, 2000.0)).unwrap();
        assert!((c.freq_hz - 200.0).abs() < 1.0, "got {}", c.freq_hz);
        assert!(c.confidence > 0.8, "conf {}", c.confidence);
    }

    #[test]
    fn rejects_loud_overtone_as_fundamental() {
        // (1,1) is the loudest peak at f0*1.594. Naive argmax would report
        // it as the fundamental. The scorer must still pick 200 Hz because
        // its overtone stack explains everything.
        let f0 = 200.0;
        let (spec, freqs) = synth(
            1.0,
            4096,
            &[
                (f0, -20.0),
                (f0 * 1.594, -6.0),
                (f0 * 2.136, -15.0),
                (f0 * 2.296, -18.0),
                (f0 * 2.653, -22.0),
            ],
        );
        let c = identify_fundamental(&spec, &freqs, -50.0, (40.0, 2000.0)).unwrap();
        assert!(
            (c.freq_hz - f0).abs() < 2.0,
            "expected ~{f0} Hz, got {}",
            c.freq_hz
        );
    }

    #[test]
    fn returns_none_for_noise() {
        let spec = vec![-80.0_f32; 4096];
        let freqs: Vec<f32> = (0..4096).map(|i| i as f32).collect();
        assert!(identify_fundamental(&spec, &freqs, -50.0, (40.0, 2000.0)).is_none());
    }

    #[test]
    fn low_confidence_for_single_peak() {
        let (spec, freqs) = synth(1.0, 4096, &[(200.0, -10.0)]);
        let c = identify_fundamental(&spec, &freqs, -50.0, (40.0, 2000.0)).unwrap();
        assert!(c.confidence < 0.3, "conf {}", c.confidence);
    }

    #[test]
    fn identifies_realistic_inharmonic_drum() {
        // Bubinga 14×6 snare ballpark: (0,1) + 4 overtones, none ideal.
        let (spec, freqs) = synth(
            1.0,
            4096,
            &[
                (214.0, -12.0),
                (341.0, -15.0),
                (458.0, -18.0),
                (493.0, -19.0),
                (567.0, -22.0),
            ],
        );
        let c = identify_fundamental(&spec, &freqs, -50.0, (40.0, 2000.0)).unwrap();
        assert!(
            (c.freq_hz - 214.0).abs() < 2.0,
            "expected ~214 Hz, got {}",
            c.freq_hz
        );
        assert!(c.confidence > 0.6, "conf {}", c.confidence);
        // Must have matched at least the fundamental + 3 more overtones.
        assert!(c.partials.len() >= 4, "matched {}", c.partials.len());
    }

    #[test]
    fn parabolic_interp_gives_subbin_accuracy() {
        // True peak at 214.3 Hz on a 1 Hz grid. Without interpolation the
        // reported frequency would snap to 214 Hz; with interpolation we
        // should be within a few cents of the truth.
        let true_hz = 214.3;
        let mut spec = vec![-80.0_f32; 4096];
        let freqs: Vec<f32> = (0..4096).map(|i| i as f32).collect();
        for (i, bin) in spec.iter_mut().enumerate().take(219).skip(210) {
            let d = (i as f64 - true_hz).abs();
            *bin = (-10.0_f64 - d * d * 2.0) as f32;
        }
        // Plus a weak upper partial so the candidate registers as a
        // legitimate fundamental (single-peak confidence tests cover the
        // low-conf case).
        let lo = (214.0_f64 * 1.594 - 2.0) as usize;
        let hi = (214.0_f64 * 1.594 + 2.0) as usize;
        for bin in &mut spec[lo..hi] {
            *bin = (-18.0_f32).max(*bin);
        }
        let c = identify_fundamental(&spec, &freqs, -50.0, (40.0, 2000.0)).unwrap();
        let cents_off = cents(c.freq_hz, true_hz).abs();
        assert!(cents_off < 5.0, "got {} Hz ({} cents off)", c.freq_hz, cents_off);
    }

    #[test]
    fn rejects_quiet_subharmonic_with_dominant_peak() {
        // Real-world failure: a loud drum-ish peak at 221 Hz with a quiet
        // bass artifact at 47 Hz. Naïve matching gives the subharmonic
        // multiple matches on random low-freq noise and beats the
        // dominant 221 Hz candidate. Prominence gate + corrected
        // tie-break must keep 221 as the identified fundamental.
        let mut spec = vec![-110.0_f32; 4096];
        let freqs: Vec<f32> = (0..4096).map(|i| i as f32).collect();
        // Triangle peak builder — inline so we can add partials at custom levels.
        let add = |spec: &mut [f32], f: f64, db: f32| {
            let i = f.round() as usize;
            spec[i] = spec[i].max(db);
            spec[i - 1] = spec[i - 1].max(db - 6.0);
            spec[i + 1] = spec[i + 1].max(db - 6.0);
        };
        // Dominant peak at 221 Hz.
        add(&mut spec, 221.0, -60.0);
        // Subharmonic artifact at 47 Hz, 30 dB quieter.
        add(&mut spec, 47.0, -90.0);
        // A scatter of low-freq noise peaks that 47 Hz can mode-match
        // coincidentally: 75.6, 101, 108, 149, 166, 192.
        for &f in &[75.6, 101.0, 108.0, 149.0, 166.0, 192.0] {
            add(&mut spec, f, -92.0);
        }
        let c = identify_fundamental(&spec, &freqs, -100.0, (40.0, 2000.0)).unwrap();
        assert!(
            (c.freq_hz - 221.0).abs() < 2.0,
            "expected ~221 Hz, got {}",
            c.freq_hz,
        );
    }

    #[test]
    fn identifies_f0_when_fundamental_peak_missing() {
        // Damped / low-tuned drum: (0,1) is a broad bump the local-max
        // finder misses, but (1,1) / (2,1) / (0,2) are sharp peaks.
        // Derived-candidate expansion (peak / ratio) must recover f0
        // from the overtone stack alone. Without it the identifier
        // would report the (1,1) peak as f0 — ~100-200 Hz high for
        // drums in the 200 Hz range.
        let f0 = 200.0_f64;
        let (spec, freqs) = synth(
            1.0,
            4096,
            &[
                // No (0,1) peak.
                (f0 * 1.594, -6.0),
                (f0 * 2.136, -12.0),
                (f0 * 2.296, -14.0),
                (f0 * 2.653, -18.0),
            ],
        );
        let c = identify_fundamental(&spec, &freqs, -50.0, (40.0, 2000.0)).unwrap();
        assert!(
            (c.freq_hz - f0).abs() < 3.0,
            "expected ~{f0} Hz from overtone stack alone, got {}",
            c.freq_hz
        );
    }

    #[test]
    fn identifies_f0_when_loud_overtone_dominates_observed_peaks() {
        // Real-drum regression for issue #59. (0,1) is present but 20 dB
        // below (1,1) — the common case on a tuned drum whose (1,1) is
        // the loudest partial. Both candidates (direct 319 Hz = (1,1)
        // and derived 200 Hz = 319/1.594) survive scoring. Without the
        // overtone-aware tie-break, the old rule picks higher f0 on mag
        // ties and reports ~319 Hz (one octave-ish high).
        let f0 = 200.0_f64;
        let (spec, freqs) = synth(
            1.0,
            4096,
            &[
                (f0, -40.0),           // (0,1) present but quiet
                (f0 * 1.594, -20.0),   // (1,1) dominant
                (f0 * 2.136, -30.0),   // (2,1)
                (f0 * 2.296, -32.0),   // (0,2)
            ],
        );
        let c = identify_fundamental(&spec, &freqs, -60.0, (40.0, 2000.0)).unwrap();
        assert!(
            (c.freq_hz - f0).abs() < 3.0,
            "expected ~{f0} Hz — loud (1,1) must not steal the fundamental, got {}",
            c.freq_hz
        );
    }

    fn flat_spectrum(level: f32, freqs: &[f32]) -> Vec<f32> {
        vec![level; freqs.len()]
    }

    fn drum_spectrum(freqs: &[f32], f0: f64, peak_db: f32, floor_db: f32) -> Vec<f32> {
        let mut spec = vec![floor_db; freqs.len()];
        let partials = [(f0, peak_db), (f0 * 1.594, peak_db - 4.0), (f0 * 2.136, peak_db - 8.0)];
        for (f, db) in partials {
            let idx = freqs
                .iter()
                .position(|&x| x as f64 >= f)
                .unwrap_or(freqs.len() - 1);
            if idx > 0 && idx < freqs.len() - 1 {
                spec[idx] = spec[idx].max(db);
                spec[idx - 1] = spec[idx - 1].max(db - 6.0);
                spec[idx + 1] = spec[idx + 1].max(db - 6.0);
            }
        }
        spec
    }

    #[test]
    fn tuner_state_does_not_fire_below_threshold() {
        let mut s = TunerState::new(TunerConfig::default());
        let freqs: Vec<f32> = (0..4096).map(|i| i as f32).collect();
        let spec = flat_spectrum(-70.0, &freqs);
        for _ in 0..30 {
            let t = s.feed(&spec, &freqs, 1.0 / 30.0);
            assert!(matches!(t, Triggered::No));
        }
        assert!(s.armed());
        assert!(s.last().is_none());
    }

    #[test]
    fn tuner_state_fires_on_rising_edge() {
        let mut s = TunerState::new(TunerConfig::default());
        let freqs: Vec<f32> = (0..4096).map(|i| i as f32).collect();
        let quiet = flat_spectrum(-70.0, &freqs);
        for _ in 0..60 {
            s.feed(&quiet, &freqs, 1.0 / 30.0);
        }
        let loud = drum_spectrum(&freqs, 214.0, -30.0, -70.0);
        let t = s.feed(&loud, &freqs, 1.0 / 30.0);
        match t {
            Triggered::Fired { candidate, confident } => {
                let c = candidate.expect("should have candidate");
                assert!((c.freq_hz - 214.0).abs() < 3.0, "f0 {}", c.freq_hz);
                assert!(confident, "expected confident");
            }
            Triggered::No => panic!("expected trigger"),
        }
        assert!(!s.armed(), "should disarm after fire");
        assert_eq!(s.history().len(), 1);
    }

    #[test]
    fn tuner_state_rearms_after_signal_drops() {
        let mut s = TunerState::new(TunerConfig::default());
        let freqs: Vec<f32> = (0..4096).map(|i| i as f32).collect();
        let quiet = flat_spectrum(-70.0, &freqs);
        for _ in 0..30 {
            s.feed(&quiet, &freqs, 1.0 / 30.0);
        }
        let loud = drum_spectrum(&freqs, 214.0, -30.0, -70.0);
        s.feed(&loud, &freqs, 1.0 / 30.0);
        assert!(!s.armed());
        for _ in 0..120 {
            s.feed(&quiet, &freqs, 1.0 / 30.0);
        }
        assert!(s.armed(), "should rearm after signal drops back to baseline");
    }

    #[test]
    fn tuner_state_dedupes_identical_hits() {
        let mut s = TunerState::new(TunerConfig::default());
        let freqs: Vec<f32> = (0..4096).map(|i| i as f32).collect();
        let quiet = flat_spectrum(-70.0, &freqs);
        let loud = drum_spectrum(&freqs, 214.0, -30.0, -70.0);
        for _ in 0..30 {
            s.feed(&quiet, &freqs, 1.0 / 30.0);
        }
        s.feed(&loud, &freqs, 1.0 / 30.0);
        for _ in 0..120 {
            s.feed(&quiet, &freqs, 1.0 / 30.0);
        }
        s.feed(&loud, &freqs, 1.0 / 30.0);
        assert_eq!(s.history().len(), 1, "duplicate within dedupe tol should not append");
    }

    #[test]
    fn tuner_state_range_lock_narrows_search() {
        let mut s = TunerState::new(TunerConfig::default());
        let freqs: Vec<f32> = (0..4096).map(|i| i as f32).collect();
        let quiet = flat_spectrum(-70.0, &freqs);
        let loud = drum_spectrum(&freqs, 214.0, -30.0, -70.0);
        s.set_range_lock(Some((300.0, 500.0)));
        for _ in 0..30 {
            s.feed(&quiet, &freqs, 1.0 / 30.0);
        }
        let t = s.feed(&loud, &freqs, 1.0 / 30.0);
        match t {
            Triggered::Fired { candidate, .. } => {
                if let Some(c) = candidate {
                    assert!(
                        c.freq_hz >= 300.0 && c.freq_hz <= 500.0,
                        "range-locked candidate must land in [300,500], got {}",
                        c.freq_hz
                    );
                }
            }
            Triggered::No => {}
        }
    }

    #[test]
    fn tuner_state_reset_clears_all() {
        let mut s = TunerState::new(TunerConfig::default());
        let freqs: Vec<f32> = (0..4096).map(|i| i as f32).collect();
        let quiet = flat_spectrum(-70.0, &freqs);
        let loud = drum_spectrum(&freqs, 214.0, -30.0, -70.0);
        for _ in 0..30 {
            s.feed(&quiet, &freqs, 1.0 / 30.0);
        }
        s.feed(&loud, &freqs, 1.0 / 30.0);
        assert!(s.last().is_some());
        s.reset();
        assert!(s.armed());
        assert!(s.last().is_none());
        assert!(s.history().is_empty());
        assert!(!s.baseline_db().is_finite());
    }

    /// Build a noisy silence spectrum — flat floor with small per-bin
    /// random variation. Deterministic LCG so the test stays reproducible.
    fn noisy_silence(freqs: &[f32], floor_db: f32, noise_peak_db: f32, seed: u64) -> Vec<f32> {
        let mut state = seed;
        freqs.iter().map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let u = ((state >> 33) as u32) as f32 / u32::MAX as f32;
            floor_db + u * (noise_peak_db - floor_db)
        }).collect()
    }

    #[test]
    fn tuner_state_fires_on_repeated_hits_with_noise() {
        // Regression: with the level detector running on peak-hold, ambient
        // noise keeps `any_rose` true every frame → peak-hold never decays →
        // baseline climbs to match the first hit → subsequent hits can't beat
        // it. The trigger must fire on each of three separated hits even
        // with a non-flat noise floor between them.
        let mut s = TunerState::new(TunerConfig::default());
        let freqs: Vec<f32> = (0..4096).map(|i| i as f32).collect();
        let dt = 1.0 / 30.0;
        let mut fires = 0;
        for hit in 0..3 {
            for k in 0..120 {
                // Fresh noisy silence each frame.
                let n = noisy_silence(&freqs, -95.0, -80.0, (hit * 1000 + k) as u64);
                s.feed(&n, &freqs, dt);
            }
            let loud = drum_spectrum(&freqs, 214.0, -30.0, -80.0);
            if let Triggered::Fired { candidate: Some(_), confident: true } =
                s.feed(&loud, &freqs, dt)
            {
                fires += 1;
            }
        }
        assert_eq!(fires, 3, "expected 3 triggers across 3 separated hits");
    }

    #[test]
    fn tuner_state_triggers_on_analyze_output() {
        // End-to-end pipeline check: synthetic drum audio → analysis::analyze
        // → dBFS conversion → TunerState::feed. Catches the dB/linear unit
        // mismatch the daemon was previously shipping.
        use std::f64::consts::PI;
        let sr: u32 = 48_000;
        let n = 16384;
        let make_samples = |amp: f32| -> Vec<f32> {
            // Drum-like: f0 + 1.594·f0 + 2.136·f0, each a partial, small noise.
            let f0 = 214.0_f64;
            let mut s = Vec::with_capacity(n);
            let mut rng = 0x1234_5678u64;
            for i in 0..n {
                let t = i as f64 / sr as f64;
                let mut v = 0.0;
                v += (2.0 * PI * f0 * t).sin() * amp as f64;
                v += (2.0 * PI * f0 * 1.594 * t).sin() * amp as f64 * 0.6;
                v += (2.0 * PI * f0 * 2.136 * t).sin() * amp as f64 * 0.4;
                rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
                let u = ((rng >> 33) as u32) as f64 / u32::MAX as f64;
                v += (u - 0.5) * 1e-4;
                s.push(v as f32);
            }
            s
        };
        let to_db = |spec: &[f64]| -> Vec<f32> {
            spec.iter().map(|&v| 20.0 * (v as f32).max(1e-12).log10()).collect()
        };
        let mut state = TunerState::new(TunerConfig::default());
        let dt = 1.0 / 30.0;
        // Quiet preamble: noise only.
        for _ in 0..60 {
            let quiet = make_samples(1e-4);
            let (spec, f) = crate::analysis::spectrum_only(&quiet, sr);
            let spec_db = to_db(&spec);
            let freqs_f32: Vec<f32> = f.iter().map(|&v| v as f32).collect();
            state.feed(&spec_db, &freqs_f32, dt);
        }
        // Loud drum hit.
        let loud = make_samples(0.3);
        let r = crate::analysis::analyze(&loud, sr, 214.0, 10)
            .expect("analyze should succeed on clean synthetic drum");
        let spec_db = to_db(&r.spectrum);
        let freqs_f32: Vec<f32> = r.freqs.iter().map(|&v| v as f32).collect();
        match state.feed(&spec_db, &freqs_f32, dt) {
            Triggered::Fired { candidate: Some(c), confident: true } => {
                assert!((c.freq_hz - 214.0).abs() < 5.0, "f0 {}", c.freq_hz);
            }
            other => panic!("expected confident fire, got {other:?}"),
        }
    }

    #[test]
    fn tuner_state_identifier_survives_accumulated_peak_hold_history() {
        // Regression: peak-hold saturated upward in real audio because any
        // bin rising reset the idle counter, so its median climbed over drum
        // partials and the identifier's floor gate hid them. After a minute
        // of prior hits + noise, a fresh hit at moderate level must still
        // be identifiable.
        let mut s = TunerState::new(TunerConfig::default());
        let freqs: Vec<f32> = (0..4096).map(|i| i as f32).collect();
        let dt = 1.0 / 30.0;
        // Simulate 20 s of mixed drum hits + noise so peak-hold is fully
        // populated with history.
        for hit in 0..10 {
            for k in 0..60 {
                let n = noisy_silence(&freqs, -95.0, -75.0, (hit * 7919 + k) as u64);
                s.feed(&n, &freqs, dt);
            }
            let loud = drum_spectrum(&freqs, 214.0, -30.0, -80.0);
            s.feed(&loud, &freqs, dt);
        }
        // 10 s of silence so baseline can track down and rearm.
        for k in 0..300 {
            let n = noisy_silence(&freqs, -95.0, -75.0, (99999 + k) as u64);
            s.feed(&n, &freqs, dt);
        }
        // Fresh hit — must still fire with a confident candidate.
        let loud = drum_spectrum(&freqs, 214.0, -30.0, -80.0);
        match s.feed(&loud, &freqs, dt) {
            Triggered::Fired { candidate: Some(c), confident: true } => {
                assert!((c.freq_hz - 214.0).abs() < 3.0, "f0 {}", c.freq_hz);
            }
            other => panic!("expected confident fire after history accumulation, got {other:?}"),
        }
    }

    #[test]
    fn cents_math_roundtrip() {
        assert!(cents(440.0, 440.0).abs() < 1e-9);
        assert!((cents(880.0, 440.0) - 1200.0).abs() < 1e-9);
        assert!((cents(466.163_76, 440.0) - 100.0).abs() < 0.1);
    }
}
