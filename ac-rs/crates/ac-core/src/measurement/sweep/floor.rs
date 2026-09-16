//! The noiseless pre-impulse floor of a deconvolved sweep (#471).
//!
//! [`super::pre_impulse_snr_db`] is named like a signal-to-noise ratio, and on
//! an acoustic IR it behaves like one. On a short electrical loopback leg it
//! does not: the pre-peak region is dominated by the deconvolution's own
//! residue, whose level relative to the peak is fixed by the *stimulus*, not by
//! how quiet the capture was. Measured on pupu 2026-09-16 and reproduced
//! synthetically within 0.5 dB: adding white noise from −120 through −20 dBFS
//! moves the figure by 0.0 dB, and a route attenuated by 40 dB reads the same
//! as a good cable. What does move it is bandwidth — 20–20000 Hz floors at
//! ~17 dB, 500–4000 Hz at ~35 dB — while sweep duration is worth ~0.5 dB in the
//! wrong direction.
//!
//! A fixed threshold therefore cannot mean the same thing under two stimuli.
//! This module computes what the figure *would* read for a given sweep with a
//! mathematically perfect loopback, so a caller can ask the only question the
//! statistic can actually answer: is this peak as clean as this stimulus
//! allows, or is it noise?
//!
//! What it cannot do, so the next reader does not re-derive the
//! disappointment: it says nothing about routing. Peak and residue scale
//! together, so an attenuated or crosstalk path reads exactly like a correct
//! one. Wrong routing is caught by port resolution (#225) and the level
//! read-out, never here.

use super::{
    deconvolve_full, extract_irs, inverse_sweep, log_sweep, pre_impulse_snr_db, SweepParams,
};

/// The value [`super::pre_impulse_snr_db`] would return for `p` with a perfect
/// loopback whose peak lands at `peak_index` in a `window_len` gate.
///
/// Synthesises the sweep, delays it by the same offset the caller measured
/// (`peak_index - window_len / 2`, which is τ in samples), and runs the
/// identical chain a real reading runs — [`inverse_sweep`], [`deconvolve_full`],
/// [`extract_irs`] at one harmonic and the same `window_len`, then
/// [`super::pre_impulse_snr_db`] at the synthetic's own argmax. Reusing those
/// functions rather than reimplementing the window arithmetic is the whole
/// point: floor and measurement must come off one code path or they drift.
///
/// Amplitude is not a parameter because the statistic is a ratio — the rig
/// measured 0.1 dB of movement across a 10 dB drive change — so the synthetic
/// is built at unit amplitude.
///
/// Capture length is not a parameter either: the floor is identical (to 0.00 dB
/// across five sweep shapes) whether the synthetic carries a tail of
/// `window_len`, 0.15 s, 0.5 s or 1.0 s, so this uses the shortest sufficient
/// one and pays the smallest FFT.
///
/// `None` when no floor can be established, and a caller must then fall back to
/// a fixed threshold rather than treat the gate as unclearable:
/// - `peak_index` earlier than the gate centre (a negative τ, which no
///   loopback produces and which cannot be synthesised as a delay), or
/// - the guard band consumes the whole pre-peak region, which makes
///   [`super::pre_impulse_snr_db`] answer `INFINITY`. A refused, noise-only leg
///   can argmax near index 0 and land here.
///
/// Cost is one sweep synthesis and one deconvolution of the same size the real
/// capture already paid: ~20 ms for a 1 s sweep at 96 kHz, ~82 ms at 4 s, ~3 s
/// at the 60 s / 192 kHz ceiling. It runs after the stimulus is silent, so it
/// delays a reply, never an emission — and it is worth computing once per run
/// and carrying, not recomputing per decision.
pub fn pre_impulse_snr_floor_db(
    p: &SweepParams,
    window_len: usize,
    peak_index: usize,
) -> Option<f64> {
    p.validate().ok()?;
    if window_len == 0 {
        return None;
    }
    let delay = peak_index as i64 - (window_len / 2) as i64;
    if delay < 0 {
        return None;
    }
    let delay = delay as usize;

    let sweep = log_sweep(p).ok()?;
    let mut captured = vec![0.0f32; sweep.len() + delay + window_len];
    for (i, &s) in sweep.iter().enumerate() {
        captured[i + delay] += s;
    }

    let inv = inverse_sweep(p).ok()?;
    let full = deconvolve_full(&captured, &inv);
    let irs = extract_irs(&full, p, 1, window_len).ok()?;
    let (idx, _) = irs
        .linear
        .iter()
        .enumerate()
        .map(|(i, v)| (i, *v))
        .max_by(|a, b| {
            a.1.abs()
                .partial_cmp(&b.1.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
    let floor = pre_impulse_snr_db(&irs.linear, idx);
    floor.is_finite().then_some(floor)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// τ window as `calibrate` and the #460 reference leg build it.
    fn window_len(sample_rate: u32) -> usize {
        2 * (0.05 * sample_rate as f64).ceil() as usize
    }

    fn params(duration_s: f64, f1_hz: f64, f2_hz: f64, sample_rate: u32) -> SweepParams {
        SweepParams {
            f1_hz,
            f2_hz,
            duration_s,
            sample_rate,
        }
    }

    /// A noise-only capture — what a disconnected reference input produces —
    /// read through the same chain. `seed` because this is a random draw, not
    /// a constant: the figure it yields moves several dB between seeds.
    fn no_cable_snr_db(p: &SweepParams, window_len: usize, seed: u64) -> f64 {
        let n = log_sweep(p).expect("sweep").len() + window_len;
        let mut state = seed | 1;
        let captured: Vec<f32> = (0..n)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let u = ((state >> 40) as f64 / (1u64 << 24) as f64) * 2.0 - 1.0;
                (0.001 * u) as f32
            })
            .collect();
        let inv = inverse_sweep(p).expect("inverse");
        let full = deconvolve_full(&captured, &inv);
        let irs = extract_irs(&full, p, 1, window_len).expect("irs");
        let (idx, _) = irs
            .linear
            .iter()
            .enumerate()
            .map(|(i, v)| (i, *v))
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap();
        pre_impulse_snr_db(&irs.linear, idx)
    }

    /// The characterisation this rule rests on: the floor tracks the sweep
    /// shape over ~18 dB. A single constant cannot sit under all four good
    /// readings and above all four bad ones — that is the defect #471 records.
    #[test]
    fn floor_tracks_the_sweep_shape() {
        let sr = 96_000;
        let wl = window_len(sr);
        let peak = wl / 2 + 1711; // pupu's measured loopback τ at 96 kHz
        let cases = [
            (params(1.0, 20.0, 20_000.0, sr), 17.2),
            (params(1.0, 50.0, 16_000.0, sr), 20.1),
            (params(4.0, 200.0, 8_000.0, sr), 28.5),
            (params(8.0, 500.0, 4_000.0, sr), 35.1),
            (params(0.2, 100.0, 20_000.0, sr), 26.8), // calibrate's own short ESS
        ];
        for (p, expected) in cases {
            let got = pre_impulse_snr_floor_db(&p, wl, peak).expect("floor");
            assert!(
                (got - expected).abs() < 0.5,
                "{} Hz–{} Hz / {} s floored at {got:.1} dB, expected ≈{expected} dB",
                p.f1_hz,
                p.f2_hz,
                p.duration_s
            );
        }
    }

    /// #471's acceptance: `floor − margin` separates a good cable from a
    /// disconnected input on every characterised shape, including `plot ir`'s
    /// defaults, where the shipped 24 dB constant refuses a perfect cable.
    ///
    /// The no-cable side is sampled over several seeds rather than pinned:
    /// it is a noise draw, and one lucky seed would make this test green
    /// while proving nothing.
    #[test]
    fn derived_threshold_separates_a_good_cable_from_no_cable() {
        let sr = 96_000;
        let wl = window_len(sr);
        let peak = wl / 2 + 1711;
        let margin_db = 3.0;
        for p in [
            params(1.0, 20.0, 20_000.0, sr),
            params(1.0, 50.0, 16_000.0, sr),
            params(4.0, 200.0, 8_000.0, sr),
            params(8.0, 500.0, 4_000.0, sr),
            params(0.2, 100.0, 20_000.0, sr),
        ] {
            let floor = pre_impulse_snr_floor_db(&p, wl, peak).expect("floor");
            let threshold = floor - margin_db;
            // A good cable reads the floor by construction, so it clears.
            assert!(
                floor >= threshold,
                "a perfect loopback must clear its own derived threshold"
            );
            for seed in [0x1234_5678u64, 0xBEEF_1234, 0xDEAD_BEEF, 0x0F0F_0F0F, 7] {
                let bad = no_cable_snr_db(&p, wl, seed);
                assert!(
                    bad < threshold,
                    "{} Hz–{} Hz / {} s: no-cable read {bad:.1} dB, which clears the \
                     {threshold:.1} dB derived threshold (floor {floor:.1}, seed {seed:#x})",
                    p.f1_hz,
                    p.f2_hz,
                    p.duration_s
                );
            }
        }
    }

    /// Test against the rejected implementation: the shipped constant refuses
    /// a mathematically perfect loopback at `plot ir`'s default sweep. If this
    /// ever stops holding, #471's premise is gone and the derived floor is
    /// unnecessary.
    #[test]
    fn the_shipped_constant_refuses_a_perfect_cable_at_the_default_sweep() {
        let sr = 96_000;
        let wl = window_len(sr);
        let floor = pre_impulse_snr_floor_db(&params(1.0, 20.0, 20_000.0, sr), wl, wl / 2 + 1711)
            .expect("floor");
        assert!(
            floor < 24.0,
            "the default sweep floors at {floor:.1} dB, which the 24 dB constant would accept — \
             #471's premise no longer holds"
        );
    }

    /// The floor must be evaluated at the *measured* peak: it moves by about
    /// the whole margin across the plausible τ range, so a nominal τ would
    /// spend the margin before any noise was considered.
    #[test]
    fn floor_moves_with_the_peak_index() {
        let sr = 96_000;
        let wl = window_len(sr);
        let p = params(4.0, 200.0, 8_000.0, sr);
        let near = pre_impulse_snr_floor_db(&p, wl, wl / 2 + 64).expect("floor");
        let far = pre_impulse_snr_floor_db(&p, wl, wl / 2 + 4700).expect("floor");
        assert!(
            (near - 27.2).abs() < 0.5 && (far - 30.1).abs() < 0.5,
            "expected ≈27.2 dB near and ≈30.1 dB far, got {near:.1} / {far:.1}"
        );
        assert!(
            far - near > 2.0,
            "peak-index dependence vanished ({near:.1} → {far:.1}); a nominal τ would now be safe \
             and this test no longer guards anything"
        );
    }

    /// Same sweep, same τ in *seconds*, three sample rates: the floor is a
    /// property of the stimulus, not of the rate it was played at.
    #[test]
    fn floor_is_sample_rate_invariant() {
        let tau_s = 0.017_823;
        let mut seen = Vec::new();
        for sr in [48_000u32, 96_000, 192_000] {
            let wl = window_len(sr);
            let peak = wl / 2 + (tau_s * sr as f64).round() as usize;
            seen.push(
                pre_impulse_snr_floor_db(&params(4.0, 200.0, 8_000.0, sr), wl, peak).unwrap(),
            );
        }
        let spread = seen.iter().cloned().fold(f64::MIN, f64::max)
            - seen.iter().cloned().fold(f64::MAX, f64::min);
        assert!(
            spread < 0.2,
            "floor moved {spread:.2} dB across rates: {seen:?}"
        );
    }

    /// The defensive case the whole change turns on: no floor means fall back,
    /// never "refuse everything". A peak at or before the gate centre has no
    /// synthesisable delay, and a peak inside the guard band leaves no
    /// pre-region to measure.
    #[test]
    fn degenerate_peaks_have_no_floor() {
        let sr = 96_000;
        let wl = window_len(sr);
        let p = params(1.0, 200.0, 8_000.0, sr);
        assert_eq!(
            pre_impulse_snr_floor_db(&p, wl, 0),
            None,
            "peak before centre"
        );
        assert_eq!(
            pre_impulse_snr_floor_db(&p, wl, wl / 2 - 1),
            None,
            "peak one sample before centre is still a negative τ"
        );
        assert_eq!(pre_impulse_snr_floor_db(&p, 0, 10), None, "no window");
    }
}
