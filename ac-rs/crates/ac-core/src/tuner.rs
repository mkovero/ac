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
#[derive(Debug, Clone, PartialEq)]
pub struct Partial {
    pub mode: (u8, u8),
    pub ideal_ratio: f64,
    pub measured_hz: f64,
    pub measured_ratio: f64,
    /// `(measured_ratio - ideal_ratio) / ideal_ratio * 100`.
    pub deviation_pct: f64,
    pub magnitude_db: f64,
}

#[derive(Debug, Clone, PartialEq)]
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

    // Score each peak as a candidate (0,1). For every membrane mode ratio,
    // look for a peak within ±5% of f0 * ratio; reward by that peak's dB
    // above floor. A drum with clean overtones lights up many modes; a
    // random lone peak scores only the fundamental slot.
    let mut best: Option<(f64, FundamentalCandidate)> = None;
    for cand in &peaks {
        let f0 = cand.freq_hz;
        if f0 <= 0.0 {
            continue;
        }
        let mut matched: Vec<Partial> = Vec::new();
        let mut score = 0.0_f64;
        for &(m, n, ratio) in MEMBRANE_MODES {
            let target = f0 * ratio;
            // Prefer the peak with the smallest relative deviation from
            // target, not the loudest — a loud non-matching peak would
            // otherwise poison the match for a nearby overtone mode.
            let mut best_p: Option<(f64, Peak)> = None;
            for p in &peaks {
                let dev = (p.freq_hz - target) / target;
                if dev.abs() <= MATCH_TOL {
                    let d = dev.abs();
                    if best_p.map(|(x, _)| d < x).unwrap_or(true) {
                        best_p = Some((d, *p));
                    }
                }
            }
            if let Some((_, p)) = best_p {
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

        // Raw energy ratio is 1.0 whenever every peak above the floor happens
        // to fall near a mode ratio — which includes the degenerate case of
        // a single isolated peak. Weight by matched-partial count so a thin
        // spectrum scores proportionally low: 4+ partials is enough to say
        // "yes, this is a drum"; fewer earns a penalty.
        let energy_ratio = (score / total_above_floor).clamp(0.0, 1.0);
        let stack_weight = (matched.len() as f64 / 4.0).min(1.0);
        let confidence = (energy_ratio * stack_weight).clamp(0.0, 1.0);
        let this = FundamentalCandidate {
            freq_hz: f0,
            confidence,
            partials: matched,
        };

        // Tie-break: higher score wins; on near-tie prefer the lower f0
        // so an octave-up candidate (whose ratios are all sub-harmonics)
        // doesn't pre-empt the real fundamental.
        let replace = match &best {
            None => true,
            Some((best_score, best_cand)) => {
                if score > *best_score * 1.001 {
                    true
                } else if (score - *best_score).abs() <= best_score * 0.001 {
                    f0 < best_cand.freq_hz
                } else {
                    false
                }
            }
        };
        if replace {
            best = Some((score, this));
        }
    }

    best.map(|(_, c)| c)
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
    fn cents_math_roundtrip() {
        assert!(cents(440.0, 440.0).abs() < 1e-9);
        assert!((cents(880.0, 440.0) - 1200.0).abs() < 1e-9);
        assert!((cents(466.163_76, 440.0) - 100.0).abs() < 0.1);
    }
}
