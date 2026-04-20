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
    let peaks = find_peaks(spectrum_db, freqs_hz, floor_db, search_range_hz);
    if peaks.is_empty() {
        return None;
    }

    let total_above_floor: f64 = peaks
        .iter()
        .map(|p| (p.magnitude_db - floor_db as f64).max(0.0))
        .sum();
    if total_above_floor <= 0.0 {
        return None;
    }
    let loudest_in_range = peaks
        .iter()
        .map(|p| p.magnitude_db)
        .fold(f64::NEG_INFINITY, f64::max);

    // Score each peak as a candidate (0,1). For every membrane mode ratio,
    // look for a peak within ±5% of f0 * ratio; reward by that peak's dB
    // above floor. A drum with clean overtones lights up many modes; a
    // random lone peak scores only the fundamental slot.
    let mut best: Option<(f64, FundamentalCandidate, f64)> = None;
    for cand in &peaks {
        let f0 = cand.freq_hz;
        if f0 <= 0.0 {
            continue;
        }
        // f0 itself must be a prominent peak. Without this gate a quiet
        // sub-harmonic artifact can farm matches against the real loud
        // peak via 2.136× / 2.296× / 2.918× aliasing and out-score the
        // true fundamental whose only match is itself.
        if cand.magnitude_db < loudest_in_range - F0_PROMINENCE_DB {
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
            if let Some((_, idx, p)) = best_p {
                used[idx] = true;
                let dev_pct = (p.freq_hz / target - 1.0) * 100.0;
                score += (p.magnitude_db - floor_db as f64).max(0.0);
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

        // Raw energy ratio is 1.0 whenever every peak above the floor happens
        // to fall near a mode ratio — which includes the degenerate case of
        // a single isolated peak. Weight by matched-partial count so a thin
        // spectrum scores proportionally low: 4+ partials is enough to say
        // "yes, this is a drum"; fewer earns a penalty.
        let energy_ratio = (score / total_above_floor).clamp(0.0, 1.0);
        let stack_weight = (matched.len() as f64 / 4.0).min(1.0);
        let confidence = (energy_ratio * stack_weight).clamp(0.0, 1.0);
        let f0_mag = cand.magnitude_db;
        let this = FundamentalCandidate {
            freq_hz: f0,
            confidence,
            partials: matched,
        };

        // Tie-break: higher score wins; on near-tie prefer the candidate
        // whose own f0 peak is louder. The previous "prefer lower f0"
        // rule backfired — a sub-harmonic bin whose mode-stack hits the
        // real fundamental as 2×/(2,1) is exactly the failure mode that
        // manifests as "tuned one step too low".
        let replace = match &best {
            None => true,
            Some((best_score, best_cand, best_mag)) => {
                if score > *best_score * 1.001 {
                    true
                } else if (score - *best_score).abs() <= best_score * 0.001 {
                    f0_mag > *best_mag
                        || (f0_mag == *best_mag && f0 > best_cand.freq_hz)
                } else {
                    false
                }
            }
        };
        if replace {
            best = Some((score, this, f0_mag));
        }
    }

    best.map(|(_, c, _)| c)
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
    /// Seconds without any bin rising before the internal peak-hold
    /// starts decaying. Shorter = more reactive, longer = holds a hit
    /// steady for post-trigger analysis.
    pub peak_hold_idle_s: f32,
    /// dB/s release rate once the idle window has elapsed. Applied
    /// per-bin, clamped to the live spectrum so a bin never drops
    /// below the current reading.
    pub peak_release_db_per_sec: f32,
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
            peak_hold_idle_s: 1.0,
            peak_release_db_per_sec: 20.0,
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
    /// Seconds since any bin rose against `peak_hold`. When this passes
    /// `cfg.peak_hold_idle_s` the held trace starts decaying toward the
    /// live spectrum at `cfg.peak_release_db_per_sec`.
    peak_idle_s: f32,
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
            peak_idle_s: 0.0,
        }
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

    /// Clear baseline, disarm flag, last candidate, history, and the
    /// internal peak-hold buffer. Range lock is left alone — the caller
    /// decides whether a reset should also drop the lock.
    pub fn reset(&mut self) {
        self.baseline_db = f32::NEG_INFINITY;
        self.armed = true;
        self.last = None;
        self.history.clear();
        self.peak_hold.clear();
        self.peak_idle_s = 0.0;
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

        // --- Peak-hold accumulation ---
        if self.peak_hold.len() != spectrum_db.len() {
            self.peak_hold = spectrum_db.to_vec();
            self.peak_idle_s = 0.0;
        } else {
            let mut any_rose = false;
            for (held, &fresh) in self.peak_hold.iter_mut().zip(spectrum_db.iter()) {
                if fresh.is_finite() && fresh > *held {
                    *held = fresh;
                    any_rose = true;
                }
            }
            if any_rose {
                self.peak_idle_s = 0.0;
            } else {
                self.peak_idle_s += dt;
                if self.peak_idle_s > self.cfg.peak_hold_idle_s {
                    let drop = self.cfg.peak_release_db_per_sec * dt;
                    for (held, &fresh) in self.peak_hold.iter_mut().zip(spectrum_db.iter()) {
                        if fresh.is_finite() {
                            *held = (*held - drop).max(fresh);
                        }
                    }
                }
            }
        }

        // --- Level detector on peak-hold ---
        let (fmin, fmax) = self.cfg.search_range_hz;
        let mut current = f32::NEG_INFINITY;
        for (f, &m) in freqs_hz.iter().zip(self.peak_hold.iter()) {
            let fh = *f as f64;
            if fh >= fmin && fh <= fmax && m.is_finite() && m > current {
                current = m;
            }
        }
        if !current.is_finite() {
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
        if !(self.armed && delta >= self.cfg.trigger_delta_db) {
            return Triggered::No;
        }
        let floor = median_f32_local(&self.peak_hold)
            .map(|m| m + self.cfg.floor_over_median_db)
            .unwrap_or(-60.0);
        let range = self.range_lock.unwrap_or(self.cfg.search_range_hz);
        let cand = identify_fundamental(&self.peak_hold, freqs_hz, floor, range);
        let confident = cand
            .as_ref()
            .is_some_and(|c| c.confidence >= self.cfg.min_confidence);
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

    #[test]
    fn cents_math_roundtrip() {
        assert!(cents(440.0, 440.0).abs() < 1e-9);
        assert!((cents(880.0, 440.0) - 1200.0).abs() < 1e-9);
        assert!((cents(466.163_76, 440.0) - 100.0).abs() < 0.1);
    }
}
