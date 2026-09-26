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
    // One-shot: find the delay the way a live session does at start —
    // the peak of the unaligned live IR ([`live_ir_peak_lag`]) — then
    // estimate aligned. A silent leg has no peak and stays unaligned.
    let unaligned = h1_estimate_core(&r, &m, sr, 0);
    let delay_samples =
        live_ir_peak_lag(&impulse_response_from_h(&unaligned.re, &unaligned.im)).unwrap_or(0);
    if delay_samples == 0 {
        return unaligned;
    }
    h1_estimate_core(&r, &m, sr, delay_samples)
}

/// Variant of [`h1_estimate`] at a caller-supplied `delay_samples`. The
/// streaming transfer worker holds the session's delay — found at start or
/// set by the operator — and feeds it in on every tick.
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

/// Welch segment length of the H₁ estimator at `sr`: one second, so 1 Hz
/// resolution. The single definition — [`h1_estimate_with_delay`],
/// [`capture_duration`] and `pair_derivation::PairDerivation::welch_nperseg`
/// all read it here, so the segment a snapshot readout names is the one
/// the derivation actually used (#221).
pub fn h1_nperseg(sr: u32) -> usize {
    sr as usize
}

fn h1_estimate_core(r: &[f64], m: &[f64], sr: u32, delay_samples: i64) -> TransferResult {
    assert_eq!(r.len(), m.len(), "ref and meas must have equal length");

    let nperseg = h1_nperseg(sr); // 1 Hz resolution
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
/// `h1_estimate_core` Welch path, that's [`h1_nperseg`] samples = 1 s
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

/// Arrival of a live impulse response from [`impulse_response_from_h`], as a
/// signed lag in samples from the alignment the H it came from was computed
/// at — the Smaart Delay Finder rule: the highest peak of the IR.
///
/// The picker is [`crate::measurement::sweep::ir_peak`], the one used by
/// `plot ir` and `calibrate` τ; this only undoes the fftshift, so index
/// `n/2` reads 0. Range is `[-n/2, n/2)`: ±0.5 s at the 1 s Welch segment.
/// An H computed at delay `D` yields the residual `true − D`, so the
/// absolute arrival is `D + lag`.
///
/// `None` when the IR is empty or holds no nonzero finite sample — a
/// silent leg, where there is no peak to report.
pub fn live_ir_peak_lag(ir: &[f32]) -> Option<i64> {
    if ir.is_empty() {
        return None;
    }
    let linear: Vec<f64> = ir.iter().map(|&v| v as f64).collect();
    let (idx, mag) = crate::measurement::sweep::ir_peak(&linear);
    if !mag.is_finite() || mag <= 0.0 {
        return None;
    }
    Some(idx as i64 - (ir.len() / 2) as i64)
}

/// Number of capture seconds needed for `n_averages` Welch segments at `sr`.
pub fn capture_duration(n_averages: usize, sr: u32) -> f64 {
    let nperseg = h1_nperseg(sr);
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

    // ---- Live-IR arrival (Smaart Delay Finder rule, #669) ----

    /// `meas` = `sig` delayed by `delay` samples (negative: `meas` leads).
    fn shifted(sig: &[f32], delay: i64) -> Vec<f32> {
        let n = sig.len() as i64;
        (0..n)
            .map(|i| {
                let j = i - delay;
                if (0..n).contains(&j) {
                    sig[j as usize]
                } else {
                    0.0
                }
            })
            .collect()
    }

    fn live_lag(r: &[f32], m: &[f32], at: i64) -> Option<i64> {
        let h = h1_estimate_with_delay(r, m, SR, at);
        live_ir_peak_lag(&impulse_response_from_h(&h.re, &h.im))
    }

    /// The unaligned live IR puts its peak at the true delay, either sign,
    /// out to a quarter of the 1 s segment. Negative matters: the IR is
    /// circular, and a pre-causal peak sits at the *end* of the raw IFFT —
    /// reading it without undoing the fftshift gives `n − d`, not `−d`.
    #[test]
    fn unaligned_live_ir_peak_is_the_delay() {
        let sig = white_noise(N, 0.5, 42);
        for d in [0i64, 1, 100, 283, -100, 12_000] {
            let meas = shifted(&sig, d);
            assert_eq!(live_lag(&sig, &meas, 0), Some(d), "true delay {d}");
        }
    }

    /// Delta Delay: an H computed at the current setting `D` peaks at the
    /// residual `true − D`, not at `true`. This is what "Find" on a running
    /// measurement reads, and what gets added to the setting on Insert.
    #[test]
    fn aligned_live_ir_peak_is_the_residual() {
        let sig = white_noise(N, 0.5, 42);
        let meas = shifted(&sig, 1_000);
        for (at, residual) in [(1_000i64, 0i64), (990, 10), (1_003, -3), (0, 1_000)] {
            assert_eq!(live_lag(&sig, &meas, at), Some(residual), "set at {at}");
        }
    }

    /// The one-shot estimator reports the same delay the live IR does.
    #[test]
    fn one_shot_delay_is_the_live_ir_peak() {
        let sig = white_noise(N, 0.5, 42);
        let meas = shifted(&sig, 283);
        assert_eq!(h1_estimate(&sig, &meas, SR).delay_samples, 283);
    }

    /// The peak is the peak: a reflection stronger than the direct sound
    /// wins. Smaart's Delay Finder does the same and the operator nudges;
    /// the rule retired here (#227's earliest-within-6 dB) took the direct
    /// sound, and this pins that the retirement is deliberate.
    #[test]
    fn stronger_reflection_wins_the_peak() {
        let sig = white_noise(N, 0.5, 42);
        let mut meas: Vec<f32> = shifted(&sig, 283).iter().map(|v| 0.5 * v).collect();
        for (m, r) in meas.iter_mut().zip(shifted(&sig, 1_455)) {
            *m += 0.8 * r;
        }
        assert_eq!(live_lag(&sig, &meas, 0), Some(1_455));
    }

    /// A silent leg has no peak to report.
    #[test]
    fn silent_leg_has_no_live_ir_peak() {
        let sig = white_noise(N, 0.5, 42);
        assert_eq!(live_lag(&sig, &vec![0.0; N], 0), None);
        assert_eq!(live_ir_peak_lag(&[]), None);
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
