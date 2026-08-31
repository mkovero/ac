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
    /// Centre frequency of the worst-margin band, Hz.
    pub worst_band_hz: f64,
    /// Smallest per-band decay observed from the linear-IR peak to the
    /// end of the captured tail, dB.
    pub worst_decay_db: f64,
    /// ISO 18233 §6.3.2's required decay, dB (30).
    pub required_db: f64,
    pub passed: bool,
    /// How many of `bands_total` in-range bands had a long enough analysis
    /// window to clear their own settling prefix and be candidates for the
    /// worst-case comparison (whether or not they ended up carrying
    /// measurable energy). `bands_settled < bands_total` means the capture
    /// was too short for `tail_s` to say anything about the missing bands —
    /// distinct from a band settling fine and genuinely reading silence.
    pub bands_settled: usize,
    /// Count of 1/3-octave bands in `[f1_hz, f2_hz]` this check considered.
    pub bands_total: usize,
}

impl TailDecayCheck {
    /// One-line verdict, meant for `MeasurementReport.notes` — this is
    /// where acceptance criterion 6 (issue #282) puts the tail_s basis:
    /// not a pre-capture guess, a stated post-hoc check against the room
    /// actually measured.
    pub fn note(&self) -> String {
        let coverage = if self.bands_settled < self.bands_total {
            format!(
                " ({} of {} in-range 1/{}-oct bands never cleared their filter's settling \
                 prefix within this tail_s and were not evaluated \u{2014} increase tail_s to \
                 cover them.)",
                self.bands_total - self.bands_settled,
                self.bands_total,
                self.bpo
            )
        } else {
            String::new()
        };
        // Pass and fail differ only in the verdict, the "only", and the
        // closing advice — the band, margin and requirement are the same
        // sentence either way, so they are written once.
        let (verdict, qualifier, advice) = if self.passed {
            (": worst-case", "decayed", "capture adequate.")
        } else {
            (
                " FAILED:",
                "only decayed",
                "this capture may be unreliable for band-resolved work; \
                 re-run with a longer tail_s.",
            )
        };
        format!(
            "ISO 18233 \u{a7}6.3.2 tail-decay check{verdict} 1/{}-oct {:.0} Hz band {qualifier} \
             {:.1} dB over the captured tail (need \u{2265}{:.0} dB) \u{2014} {advice}{}",
            self.bpo, self.worst_band_hz, self.worst_decay_db, self.required_db, coverage
        )
    }
}

/// Post-hoc verification that the captured tail satisfies ISO 18233
/// §6.3.2: "the capture covers from the start of excitation until the
/// response in each fractional-octave band has decayed by more than
/// 30 dB." Per ISO 18233 B.2, sweep duration is not related to
/// reverberation time (unlike periodic excitation), so there is no
/// pre-capture RT60 estimator this crate could use to size `tail_s`
/// ahead of a real room — the check instead runs after deconvolution,
/// against the room actually measured. This inline reference to the ISO
/// clause is not a `StandardsCitation`: nobody here has cross-checked it
/// against the published ISO 18233 text (see `report.rs`'s
/// `every_measurement_module_emits_populated_citation` and
/// `ARCHITECTURE.md`'s citation-audit workflow), and this repo does not
/// carry that PDF under `stddocs/iec-full/` to check against.
///
/// Compares the in-band level of a short window right at the linear-IR
/// peak (`early`) against a window of the same length at the end of the
/// captured tail (`late`), per 1/3-octave (IEC 61260-1) band across
/// `[f1_hz, f2_hz]`. Reports the worst (smallest) per-band decay.
///
/// The early/late window is sized to clear the narrowest (lowest-frequency,
/// longest time-constant) in-range band's own settling prefix — capped at
/// half the tail so the two windows never overlap — rather than a flat
/// quarter of `tail_len`. A flat quarter split left the lowest 1/3-oct
/// bands permanently `NEG_INFINITY` from [`Filterbank::process`] (settling
/// prefix longer than the window) at the daemon's own shipped `tail_s`
/// default, which the old code folded into the same "no measurable energy"
/// bucket as genuine silence and silently dropped from the worst-case
/// comparison — invisible even though those bands carried real energy and
/// are exactly the ones ISO 18233 §6.3.2 is hardest on. Bands that still
/// can't clear their settling prefix within the capped window (`tail_s`
/// itself too short for the room, not a windowing artefact) are counted in
/// `TailDecayCheck::bands_settled` / surfaced in `TailDecayCheck::note`
/// instead; bands that settle fine and genuinely read back silence are
/// still excluded from the worst-case pick — there is nothing there to
/// decay.
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

    let f_min = p.f1_hz.max(20.0);
    let f_max = p.f2_hz.min(fs * 0.45 - 1.0);
    let fb = Filterbank::new(p.sample_rate, BPO, f_min, f_max)?;
    let settle = fb.settle_samples();
    let max_settle = settle.iter().copied().max().unwrap_or(0);
    // Quarter-tail is the floor (unchanged for short tails / high f_min,
    // where no in-range band needs more); otherwise grow to clear the
    // widest settling prefix plus a measurement margin, never past half the
    // tail.
    let win = (tail_len / 4)
        .max(max_settle + max_settle / 4)
        .min(tail_len / 2);
    if win < 2 {
        bail!("captured tail too short to evaluate decay ({tail_len} samples past the sweep end)");
    }

    let early: Vec<f32> = full[linear_centre..linear_centre + win]
        .iter()
        .map(|&v| v as f32)
        .collect();
    let late_start = linear_centre + tail_len - win;
    let late: Vec<f32> = full[late_start..late_start + win]
        .iter()
        .map(|&v| v as f32)
        .collect();

    let early_db = fb.process(&early);
    let late_db = fb.process(&late);
    let centres = fb.centres_hz();

    let mut worst: Option<(f64, f64)> = None; // (centre_hz, decay_db)
    let mut bands_settled = 0usize;
    for (((&e, &l), &c), &s) in early_db
        .iter()
        .zip(late_db.iter())
        .zip(centres.iter())
        .zip(settle.iter())
    {
        if win <= s {
            continue; // tail_s itself too short for this band to settle — not evaluated
        }
        bands_settled += 1;
        if !e.is_finite() {
            continue; // settled fine, genuinely no energy at capture start — nothing to decay
        }
        let decay = if l.is_finite() { e - l } else { f64::INFINITY };
        if worst.map(|(_, d)| decay < d).unwrap_or(true) {
            worst = Some((c, decay));
        }
    }
    let (worst_band_hz, worst_decay_db) = worst
        .ok_or_else(|| anyhow::anyhow!("no 1/3-octave band carried measurable energy to check"))?;

    Ok(TailDecayCheck {
        bpo: BPO as u32,
        worst_band_hz,
        worst_decay_db,
        required_db: REQUIRED_DB,
        passed: worst_decay_db >= REQUIRED_DB,
        bands_settled,
        bands_total: centres.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement::sweep::testkit::*;
    use crate::measurement::sweep::{deconvolve_full, inverse_sweep, log_sweep};

    /// Build a synthetic `full` deconvolution buffer whose early tail
    /// window carries a real broadband signal and whose late tail window
    /// is exact digital silence — the case ISO 18233 §6.3.2 describes as
    /// adequate capture, without depending on how deep a real Farina
    /// deconvolution's own residual skirt happens to be at a given window
    /// length (that skirt is real but its depth is not what this check
    /// means to pin down).
    fn full_with_silent_tail(p: &SweepParams, tail_s: f64) -> Vec<f64> {
        let linear_centre = p.n_samples() - 1;
        let tail_len = (tail_s * p.sample_rate as f64).round() as usize;
        let win = tail_len / 4;
        let mut full = vec![0.0_f64; linear_centre + tail_len + 1];
        let x = log_sweep(p).unwrap();
        for i in 0..win {
            full[linear_centre + i] = x[i] as f64;
        }
        full
    }

    #[test]
    fn tail_decay_check_passes_when_the_tail_is_true_silence() {
        let p = p_default();
        let full = full_with_silent_tail(&p, 0.3);
        let check = check_tail_decay(&full, &p, 0.3).unwrap();
        assert!(check.passed, "expected pass, got {check:?}");
        assert!(check.worst_decay_db >= check.required_db);
    }

    #[test]
    fn tail_decay_check_fails_when_the_tail_never_decays() {
        // Test against the rejected case directly: poison the end of the
        // tail with the same raw samples the check reads as "right at the
        // IR peak", so every band it can evaluate reports 0 dB of decay —
        // a room whose reverberation is nowhere close to 30 dB down by the
        // end of the captured tail.
        let p = p_default();
        let x = log_sweep(&p).unwrap();
        let xi = inverse_sweep(&p).unwrap();
        let mut full = deconvolve_full(&x, &xi);
        let linear_centre = p.n_samples() - 1;
        let tail_s = 0.3;
        let tail_len = (tail_s * p.sample_rate as f64).round() as usize;
        let win = tail_len / 4;
        let src = full[linear_centre..linear_centre + win].to_vec();
        let late_start = linear_centre + tail_len - win;
        full[late_start..late_start + win].copy_from_slice(&src);

        let check = check_tail_decay(&full, &p, tail_s).unwrap();
        assert!(!check.passed, "expected failure, got {check:?}");
        assert!(check.worst_decay_db < check.required_db);
    }

    #[test]
    fn tail_decay_check_rejects_nonpositive_tail_s() {
        let p = p_default();
        let full = vec![0.0; p.n_samples() * 2];
        assert!(check_tail_decay(&full, &p, 0.0).is_err());
        assert!(check_tail_decay(&full, &p, -1.0).is_err());
    }

    #[test]
    fn tail_decay_check_rejects_a_capture_with_no_tail() {
        let p = p_default();
        let full = vec![0.0; p.n_samples()]; // nothing captured past the sweep end
        assert!(check_tail_decay(&full, &p, 0.5).is_err());
    }

    #[test]
    fn tail_decay_check_note_names_the_band_and_margin() {
        let p = p_default();
        let full = full_with_silent_tail(&p, 0.3);
        let check = check_tail_decay(&full, &p, 0.3).unwrap();
        let note = check.note();
        assert!(note.contains("18233"));
        assert!(note.contains("6.3.2"));
        assert!(note.contains("adequate"));
    }

    /// Regression for correctness issue 1 (PR #296 QA review), adopting the
    /// review's suggested test near-verbatim: at the daemon's own shipped
    /// default `tail_s = 0.5` (the earlier `tail_decay_check_fails_when_...`
    /// test only exercised `tail_s = 0.3`), poison the tail end with the
    /// same broadband early-window content the check reads as "right at the
    /// IR peak". Before the settle-aware window fix, the flat `tail_len / 4`
    /// window (125 ms) was shorter than the lowest in-range band's settling
    /// prefix (~137 ms), so that band read `NEG_INFINITY` from
    /// `Filterbank::process` and was folded into "no energy, exclude" —
    /// this test's ~0 dB decay was still visible via other, higher bands in
    /// the old code, so it does not by itself prove the exclusion is fixed;
    /// `tail_decay_check_reports_full_band_coverage_when_tail_is_adequate`
    /// below is what actually pins the settled-band count.
    #[test]
    fn tail_decay_check_fails_at_shipped_default_tail_s() {
        let p = p_default();
        let x = log_sweep(&p).unwrap();
        let xi = inverse_sweep(&p).unwrap();
        let mut full = deconvolve_full(&x, &xi);
        let linear_centre = p.n_samples() - 1;
        let tail_s = 0.5; // daemon's shipped default (handlers/audio/plot.rs)
        let fs = p.sample_rate as f64;
        let tail_len = (tail_s * fs).round() as usize;
        let win = tail_len / 4;
        let src = full[linear_centre..linear_centre + win].to_vec();
        let late_start = linear_centre + tail_len - win;
        full[late_start..late_start + win].copy_from_slice(&src);

        let check = check_tail_decay(&full, &p, tail_s).unwrap();
        assert!(!check.passed, "expected failure, got {check:?}");
        assert!(check.worst_decay_db < check.required_db);
    }

    #[test]
    fn tail_decay_check_reports_full_band_coverage_when_tail_is_adequate() {
        let p = p_default();
        let full = full_with_silent_tail(&p, 0.5);
        let check = check_tail_decay(&full, &p, 0.5).unwrap();
        assert_eq!(
            check.bands_settled, check.bands_total,
            "expected every in-range band to clear its settling prefix at tail_s=0.5: {check:?}"
        );
    }

    // ─── noise_tail_start_s / tukey_window / gated_frequency_response (#284) ───
}
