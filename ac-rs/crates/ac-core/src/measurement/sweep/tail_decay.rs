//! ISO 18233 §6.3.2 capture-adequacy check: did the captured tail run long
//! enough for every in-range fractional-octave band to decay 30 dB?

use anyhow::{bail, Result};

use super::SweepParams;
use crate::measurement::filterbank::Filterbank;

/// Outcome of [`check_tail_decay`].
#[derive(Debug, Clone, PartialEq)]
pub struct TailDecayCheck {
    /// Bands-per-octave the check ran at (fixed at 1/3-octave — the
    /// resolution ISO 18233 §6.3.2's "each fractional-octave band"
    /// language is conventionally read at).
    pub bpo: u32,
    /// Centre frequency of the band the verdict names, Hz: the worst band
    /// still falling at the end of the capture when one is short of the
    /// requirement, otherwise the worst band overall.
    pub worst_band_hz: f64,
    /// Smallest per-band decay observed from the linear-IR peak to the
    /// end of the captured tail, dB.
    pub worst_decay_db: f64,
    /// ISO 18233 §6.3.2's required decay, dB (30).
    pub required_db: f64,
    /// Every band decayed the required 30 dB.
    pub passed: bool,
    /// Whether the named band was still falling over the capture's last
    /// stretch (#504): the one case a longer `tail_s` helps, and the only
    /// one reported as FAILED. A band that levelled off short of 30 dB
    /// reached its floor inside the capture — the path carries too little
    /// in that band at this level (the rig's 1083 at 25 Hz, −50 dBFS) — and
    /// more tail would only record more floor.
    pub still_falling: bool,
    /// Count of 1/3-octave bands in `[f1_hz, f2_hz]` this check considered.
    pub bands_total: usize,
}

impl TailDecayCheck {
    /// One-line verdict, meant for `MeasurementReport.notes` — this is
    /// where acceptance criterion 6 (issue #282) puts the tail_s basis:
    /// not a pre-capture guess, a stated post-hoc check against the room
    /// actually measured. A failure names what to check: a longer `tail_s`
    /// only when the band was still falling at the end (#504).
    pub fn note(&self) -> String {
        let head = "ISO 18233 \u{a7}6.3.2 tail-decay check";
        let (b, band, d, need) = (
            self.bpo,
            self.worst_band_hz,
            self.worst_decay_db,
            self.required_db,
        );
        if self.passed {
            format!(
                "{head}: worst-case 1/{b}-oct {band:.0} Hz band decayed {d:.1} dB from its \
                 peak by the end of the captured tail (need \u{2265}{need:.0} dB) \u{2014} \
                 capture adequate."
            )
        } else if self.still_falling {
            format!(
                "{head} FAILED: 1/{b}-oct {band:.0} Hz band only decayed {d:.1} dB from its \
                 peak and was still falling at the end of the capture (need \u{2265}{need:.0} \
                 dB) \u{2014} re-run with a longer tail_s."
            )
        } else {
            format!(
                "{head}: tail long enough; 1/{b}-oct {band:.0} Hz band rises only {d:.1} dB \
                 above its floor (need \u{2265}{need:.0} dB for band-resolved work) \u{2014} \
                 check the drive level and the background noise in that band."
            )
        }
    }
}

/// Envelope block, s: short against every in-range band's own response
/// (the 1/3-octave filter at 20 Hz rings for hundreds of ms), so a peak is
/// read whole rather than averaged away.
const BLOCK_S: f64 = 0.005;

/// Filter run-in before the linear-IR peak, s, so each band starts from
/// the quiet pre-impulse region rather than a step at the peak.
const PRE_S: f64 = 0.05;

/// Longest stretch before the linear-IR peak read as each band's floor, s.
/// Bounded further to half the gap to the 2nd-harmonic IR (`L·ln 2` before
/// the peak), so no distortion product sits in it.
const FLOOR_MAX_S: f64 = 0.25;

/// Least margin, dB, by which the end of the capture must sit above the
/// band's floor to count as still falling — before the level-estimate
/// scatter below is considered.
const FALLING_MIN_DB: f64 = 6.0;

/// Standard deviation, dB, of a mean-square level estimate of Gaussian
/// noise over `t_s` in a band `b_hz` wide: `10·log10(e)/√(B·T)`. A 1/3-octave
/// band at 25 Hz (B ≈ 5.8 Hz) over 62 ms scatters ±7 dB, so a fixed dB step
/// cannot tell falling from noise there.
fn level_sigma_db(b_hz: f64, t_s: f64) -> f64 {
    10.0 * std::f64::consts::LOG10_E / (b_hz * t_s).max(1e-9).sqrt()
}

/// Post-hoc verification that the captured tail satisfies ISO 18233
/// §6.3.2: "the recorded part of the response shall cover the time from the
/// start of excitation to the time where the response in each fractional
/// octave band has decayed by more than 30 dB." Per ISO 18233 B.2, sweep
/// duration is not related to reverberation time, so there is no
/// pre-capture estimator to size `tail_s` ahead of a real room — the check
/// runs after deconvolution, against the room actually measured.
///
/// Per 1/3-octave (IEC 61260-1) band across `[f1_hz, f2_hz]`: the band's
/// fractional-octave weighted response (§6.3.2, the filter applied to the
/// broadband IR from before its peak), its envelope in [`BLOCK_S`] blocks,
/// and the decay from the envelope's peak to the mean level of the capture's
/// last eighth. A band short of 30 dB is either still falling — the end of
/// the capture sits clearly above the band's own floor, read in the clean
/// stretch before the peak — or has levelled off at that floor. Only the
/// first is a tail-length failure; the second is too little signal in the
/// band for its noise, which a longer tail cannot change (#504).
///
/// #504: the rule this replaced compared mean band levels over a window at
/// the peak and one at the end, and told every band short of 30 dB to re-run
/// with a longer tail. Its clean-cable FAILED was the tilted kernel of #733
/// (with that fixed it reads 30.3 dB at the default tail); its advice was
/// wrong for any floor-limited band, such as the rig speaker's 25 Hz at
/// −50 dBFS (4.6 dB above its floor, on pupu 2026-09-30).
///
/// `full` is the full [`super::deconvolve_full`] output (not the windowed
/// `DeconvolvedIrs::linear` from `extract_irs`) — `tail_s` of captured
/// signal past the sweep endpoint has to still be present to check.
pub fn check_tail_decay(full: &[f64], p: &SweepParams, tail_s: f64) -> Result<TailDecayCheck> {
    p.validate()?;
    if !tail_s.is_finite() || tail_s <= 0.0 {
        bail!("tail_s must be positive (got {tail_s})");
    }
    const REQUIRED_DB: f64 = 30.0;
    const BPO: usize = 3;

    let fs = p.sample_rate as f64;
    let linear_centre = p.n_samples().saturating_sub(1);
    let tail_len = ((tail_s * fs).round() as usize).min(full.len().saturating_sub(linear_centre));
    let block = ((BLOCK_S * fs).round() as usize).max(1);
    let last = tail_len / 8;
    if last < block {
        bail!("captured tail too short to evaluate decay ({tail_len} samples past the sweep end)");
    }
    let f_min = p.f1_hz.max(20.0);
    let f_max = p.f2_hz.min(fs * 0.45 - 1.0);
    let fb = Filterbank::new(p.sample_rate, BPO, f_min, f_max)?;
    let centres = fb.centres_hz();
    let warm = fb.settle_samples().into_iter().max().unwrap_or(0);

    // Layout before the peak, latest first: `pre` of run-in, then the
    // floor stretch, then `warm` for the filters to settle into it.
    let pre = ((PRE_S * fs).round() as usize).min(linear_centre);
    let floor_s = (0.5 * p.harmonic_time_offset_s(2)).min(FLOOR_MAX_S);
    let floor_len = ((floor_s * fs).round() as usize).min(linear_centre - pre);
    let start = (linear_centre - pre - floor_len).saturating_sub(warm);
    let segment = &full[start..linear_centre + tail_len];
    let floor_at = linear_centre - pre - floor_len - start;
    let peak_from = linear_centre - pre - start;
    let mean_db = |y: &[f64]| {
        let ms = y.iter().map(|v| v * v).sum::<f64>() / y.len() as f64;
        10.0 * ms.log10()
    };

    // (centre_hz, decay_db, still_falling): the worst band overall, and the
    // worst of those cut off while still falling — the actionable one.
    let mut worst: Option<(f64, f64, bool)> = None;
    let mut worst_falling: Option<(f64, f64, bool)> = None;
    for (band, &c) in centres.iter().enumerate() {
        let y = fb
            .filter_band(band, segment)
            .expect("band index from the filterbank's own centres");
        let peak = y[peak_from..]
            .chunks(block)
            .map(mean_db)
            .fold(f64::NEG_INFINITY, f64::max);
        if !peak.is_finite() {
            continue; // no energy in this band at all — nothing to decay
        }
        let end = mean_db(&y[y.len() - last..]);
        let decay = if end.is_finite() {
            peak - end
        } else {
            f64::INFINITY
        };
        // Still falling: the end of the capture sits clearly above the
        // band's own floor before the impulse — by more than the scatter
        // of the two level estimates. With no floor stretch (a sweep too
        // short for one) a band short of 30 dB counts as still falling:
        // the tail advice is the one that cannot be wrong for lack of data.
        let still_falling = if floor_len >= block {
            let floor = mean_db(&y[floor_at..floor_at + floor_len]);
            let b_hz = c * (2f64.powf(1.0 / 6.0) - 2f64.powf(-1.0 / 6.0));
            let sigma = level_sigma_db(b_hz, last as f64 / fs)
                .hypot(level_sigma_db(b_hz, floor_len as f64 / fs));
            end - floor > FALLING_MIN_DB.max(2.0 * sigma)
        } else {
            true
        };
        if worst.map(|(_, d, _)| decay < d).unwrap_or(true) {
            worst = Some((c, decay, still_falling));
        }
        if still_falling
            && decay < REQUIRED_DB
            && worst_falling.map(|(_, d, _)| decay < d).unwrap_or(true)
        {
            worst_falling = Some((c, decay, still_falling));
        }
    }
    let worst = worst
        .ok_or_else(|| anyhow::anyhow!("no 1/3-octave band carried measurable energy to check"))?;
    let passed = worst.1 >= REQUIRED_DB;
    let (worst_band_hz, worst_decay_db, still_falling) = worst_falling.unwrap_or(worst);

    Ok(TailDecayCheck {
        bpo: BPO as u32,
        worst_band_hz,
        worst_decay_db,
        required_db: REQUIRED_DB,
        passed,
        still_falling,
        bands_total: centres.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement::sweep::{deconvolve_full, inverse_sweep, log_sweep};

    const SR: u32 = 48_000;

    /// `plot ir`'s defaults (#501): 20 Hz–20 kHz, 4 s.
    fn defaults(sample_rate: u32) -> SweepParams {
        SweepParams {
            f1_hz: super::super::IR_DEFAULT_F1_HZ,
            f2_hz: super::super::IR_DEFAULT_F2_HZ,
            duration_s: super::super::IR_DEFAULT_DURATION_S,
            sample_rate,
        }
    }

    /// A perfect loopback through the real chain: the sweep, `tail_s` of
    /// silence captured after it, deconvolved.
    fn identity_full(p: &SweepParams, tail_s: f64) -> Vec<f64> {
        let mut y = log_sweep(p).unwrap();
        let tail = (tail_s * p.sample_rate as f64).round() as usize;
        y.extend(std::iter::repeat_n(0.0f32, tail));
        deconvolve_full(&y, &inverse_sweep(p).unwrap())
    }

    /// Deterministic white noise in [-1, 1).
    fn noise(n: usize, seed: u64) -> Vec<f64> {
        let mut z = seed | 1;
        (0..n)
            .map(|_| {
                z = z
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                ((z >> 40) as f64 / (1u64 << 24) as f64) * 2.0 - 1.0
            })
            .collect()
    }

    /// A `full` buffer whose response starts at the linear-IR position: a
    /// unit impulse plus noise decaying at `rt60_s` (a room), plus a
    /// steady floor at `floor` (a noisy path), for `tail_s` after it.
    fn synthetic_full(p: &SweepParams, tail_s: f64, rt60_s: f64, floor: f64) -> Vec<f64> {
        let fs = p.sample_rate as f64;
        let lc = p.n_samples() - 1;
        let tail = (tail_s * fs).round() as usize;
        let mut full = vec![0.0; lc + tail + 1];
        let room = noise(tail, 11);
        let bed = noise(lc + tail + 1, 29);
        for (k, v) in full.iter_mut().enumerate() {
            *v = floor * bed[k];
        }
        full[lc] += 1.0;
        for k in 0..tail {
            let t = k as f64 / fs;
            full[lc + k] += 0.05 * room[k] * (-6.9078 * t / rt60_s).exp();
        }
        full
    }

    /// #504's acceptance: a perfect loopback at the shipped defaults passes,
    /// at both rig rates (on pupu, 2026-09-30: worst band 25 Hz at 32.6 dB,
    /// where the tilted kernel of #733 printed a FAILED on every run).
    #[test]
    fn a_perfect_loopback_passes_at_the_default_tail() {
        let tail_s = super::super::IR_DEFAULT_TAIL_S;
        for sr in [48_000u32, 96_000] {
            let p = defaults(sr);
            let check = check_tail_decay(&identity_full(&p, tail_s), &p, tail_s).unwrap();
            assert!(check.passed, "{sr} Hz: {}", check.note());
        }
    }

    /// A room whose decay is cut off by the capture: FAILED, and the note
    /// asks for a longer tail — the one remedy that helps.
    #[test]
    fn a_decay_cut_off_by_the_capture_fails_and_asks_for_more_tail() {
        let p = defaults(SR);
        let full = synthetic_full(&p, 0.5, 3.0, 0.0);
        let check = check_tail_decay(&full, &p, 0.5).unwrap();
        assert!(!check.passed && check.still_falling, "{check:?}");
        let note = check.note();
        assert!(
            note.contains("FAILED") && note.contains("longer tail_s"),
            "{note}"
        );
        // The same room with a tail long enough to decay 30 dB passes.
        let full = synthetic_full(&p, 2.5, 3.0, 0.0);
        assert!(check_tail_decay(&full, &p, 2.5).unwrap().passed);
    }

    /// A path whose floor sits within 30 dB of its peak: the tail is long
    /// enough (the band levelled off), so no FAILED and no tail advice —
    /// the note names level and noise instead (#504).
    #[test]
    fn a_floor_limited_band_is_not_a_tail_failure() {
        let p = defaults(SR);
        let full = synthetic_full(&p, 0.5, 0.05, 0.003);
        let check = check_tail_decay(&full, &p, 0.5).unwrap();
        assert!(!check.passed && !check.still_falling, "{check:?}");
        let note = check.note();
        assert!(
            !note.contains("FAILED") && !note.contains("tail_s"),
            "{note}"
        );
        assert!(
            note.contains("drive level") && note.contains("tail long enough"),
            "{note}"
        );
        // Against the rejected advice: the rule this replaced said "re-run
        // with a longer tail_s" for every band short of 30 dB. Here a tail
        // four times as long still falls short, so that advice was useless.
        let longer = synthetic_full(&p, 2.0, 0.05, 0.003);
        let again = check_tail_decay(&longer, &p, 2.0).unwrap();
        assert!(
            !again.passed && again.worst_decay_db < check.worst_decay_db + 3.0,
            "a 2.0 s tail reached {:.1} dB against {:.1} dB at 0.5 s",
            again.worst_decay_db,
            check.worst_decay_db
        );
    }

    /// Both at once: a floor-limited band must not hide a band the capture
    /// cut off. The verdict names the one a longer tail would fix.
    #[test]
    fn a_cut_off_band_outranks_a_floor_limited_one() {
        let p = defaults(SR);
        let fs = SR as f64;
        let lc = p.n_samples() - 1;
        let tail = (0.5 * fs) as usize;
        // Floor-limited everywhere, plus a slow 1 kHz ring still falling.
        let mut full = synthetic_full(&p, 0.5, 0.05, 0.003);
        for k in 0..tail {
            let t = k as f64 / fs;
            full[lc + k] +=
                0.5 * (2.0 * std::f64::consts::PI * 1000.0 * t).sin() * (-6.9078 * t / 4.0).exp();
        }
        let check = check_tail_decay(&full, &p, 0.5).unwrap();
        assert!(check.still_falling, "{check:?}");
        assert!(
            (check.worst_band_hz - 1000.0).abs() < 60.0,
            "named {:.0} Hz, not the ringing 1 kHz band",
            check.worst_band_hz
        );
        assert!(check.note().contains("FAILED"));
    }

    #[test]
    fn tail_decay_check_rejects_nonpositive_tail_s() {
        let p = defaults(SR);
        let full = vec![0.0; p.n_samples() * 2];
        assert!(check_tail_decay(&full, &p, 0.0).is_err());
        assert!(check_tail_decay(&full, &p, -1.0).is_err());
    }

    #[test]
    fn tail_decay_check_rejects_a_capture_with_no_tail() {
        let p = defaults(SR);
        let full = vec![0.0; p.n_samples()]; // nothing captured past the sweep end
        assert!(check_tail_decay(&full, &p, 0.5).is_err());
    }

    #[test]
    fn tail_decay_check_note_names_the_clause() {
        let p = defaults(SR);
        let note = check_tail_decay(&identity_full(&p, 0.5), &p, 0.5)
            .unwrap()
            .note();
        assert!(note.contains("18233") && note.contains("6.3.2") && note.contains("adequate"));
    }

    // ─── noise_tail_start_s / tukey_window / gated_frequency_response (#284) ───
}
