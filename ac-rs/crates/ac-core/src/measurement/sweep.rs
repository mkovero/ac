//! Tier 1 — Farina exponential-sweep impulse response measurement.
//!
//! Per Farina 2000, *Simultaneous measurement of impulse response and
//! distortion with a swept-sine technique*, AES 108th convention preprint
//! #5093, §2 "Theoretical basis". Verified against the full preprint at
//! `stddocs/iec-full/Simultaneous_Measurement_of_Impulse_Response_and_D.pdf`:
//! the log-sweep `x(t) = sin[K·(e^(t/L) − 1)]` with `K = T·ω1/ln(ω2/ω1)`
//! and `L = T/ln(ω2/ω1)`, the `exp(-t/L)` inverse-filter envelope, and
//! the harmonic offset `Δt_N = T·ln(N)/ln(ω2/ω1)` all match the formulae
//! implemented below.
//!
//! The technique:
//! 1. Drive the DUT with a logarithmic (exponential) sine sweep `x(t)`
//!    covering `[f1, f2]` over `T` seconds.
//! 2. Record the response `y(t)`.
//! 3. Convolve `y` with the time-reversed, amplitude-modulated inverse
//!    filter `x_inv(t)` — Farina's closed-form inverse that makes
//!    `x(t) ∗ x_inv(t) ≈ δ(t−T)`.
//! 4. The linear IR appears centred at the end of the convolution
//!    (offset `≈ N−1` for equal-length sweeps). k-th-order harmonic IRs
//!    appear earlier at known offsets
//!    `Δt_k = (T / ln(f2/f1)) · ln(k)` seconds before the linear IR,
//!    because the k-th harmonic of an exponential sweep is the
//!    fundamental of a time-shifted version of the same sweep.
//!
//! Time-gating the pre-impulse region into windows centred at each
//! `Δt_k` yields per-order harmonic impulse responses, suitable for
//! calculating a frequency-resolved THD curve.

use std::f64::consts::PI;

use anyhow::{bail, Result};
use realfft::RealFftPlanner;
use serde::{Deserialize, Serialize};

use crate::measurement::filterbank::Filterbank;
use crate::measurement::report::StandardsCitation;

/// Parameters for a Farina log sweep.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SweepParams {
    pub f1_hz: f64,
    pub f2_hz: f64,
    pub duration_s: f64,
    pub sample_rate: u32,
}

impl SweepParams {
    pub fn validate(&self) -> Result<()> {
        if self.sample_rate == 0 {
            bail!("sample_rate must be positive");
        }
        if !self.f1_hz.is_finite() || !self.f2_hz.is_finite() || !self.duration_s.is_finite() {
            bail!("non-finite parameter");
        }
        if self.f1_hz <= 0.0 {
            bail!("f1_hz must be positive (got {})", self.f1_hz);
        }
        if self.f2_hz <= self.f1_hz {
            bail!(
                "f2_hz must exceed f1_hz (got f1={}, f2={})",
                self.f1_hz,
                self.f2_hz
            );
        }
        if self.duration_s <= 0.0 {
            bail!("duration_s must be positive (got {})", self.duration_s);
        }
        if self.f2_hz >= self.sample_rate as f64 * 0.5 {
            bail!(
                "f2_hz must be below Nyquist ({} Hz); got {}",
                self.sample_rate as f64 * 0.5,
                self.f2_hz
            );
        }
        Ok(())
    }

    pub fn n_samples(&self) -> usize {
        (self.duration_s * self.sample_rate as f64).round() as usize
    }

    /// `L = T / ln(f2/f1)` — the exponential-sweep time constant.
    /// Instantaneous frequency is `f1 · exp(t / L)`.
    pub fn time_constant(&self) -> f64 {
        self.duration_s / (self.f2_hz / self.f1_hz).ln()
    }

    /// Time offset at which the k-th harmonic IR appears BEFORE the
    /// linear IR in a Farina deconvolution, in seconds.
    ///
    /// `Δt_k = L · ln(k)`. `k = 1` returns 0.
    pub fn harmonic_time_offset_s(&self, k: u32) -> f64 {
        if k == 0 {
            return 0.0;
        }
        self.time_constant() * (k as f64).ln()
    }
}

/// Generate the exponential sine sweep `x(t)` at unit peak amplitude.
pub fn log_sweep(p: &SweepParams) -> Result<Vec<f32>> {
    p.validate()?;
    let n = p.n_samples();
    let l = p.time_constant();
    let k_phase = 2.0 * PI * p.f1_hz * l;
    let fs = p.sample_rate as f64;
    Ok((0..n)
        .map(|i| {
            let t = i as f64 / fs;
            let phase = k_phase * ((t / l).exp() - 1.0);
            phase.sin() as f32
        })
        .collect())
}

/// Generate Farina's inverse filter `x_inv(t)`: the time-reversed sweep,
/// amplitude-modulated by `exp(-(T-t)/L)` so that the spectrum of
/// `x(t) * x_inv(t)` is flat and the convolution approximates a unit
/// impulse at `t = T` regardless of `log(f2/f1)`.
///
/// The returned buffer is normalised so that for a unity-amplitude sweep
/// `log_sweep(p)` the peak of `deconvolve_full(log_sweep(p), x_inv)` is
/// unity — i.e. an identity system yields a unit-magnitude IR. Without
/// that normalisation the peak carries an arbitrary Farina scale factor
/// that users would otherwise have to back out by hand.
pub fn inverse_sweep(p: &SweepParams) -> Result<Vec<f32>> {
    p.validate()?;
    let n = p.n_samples();
    let fs = p.sample_rate as f64;
    let l = p.time_constant();
    let k_phase = 2.0 * PI * p.f1_hz * l;
    let a = 1.0 / l;
    let t_end = (n - 1) as f64 / fs;

    let mut inv: Vec<f64> = (0..n)
        .map(|i| {
            let t = i as f64 / fs;
            // Sample value of forward sweep at time (t_end - t):
            let t_fwd = t_end - t;
            let phase = k_phase * ((t_fwd / l).exp() - 1.0);
            let env = (-a * t_fwd).exp();
            phase.sin() * env
        })
        .collect();

    // Normalise so the identity-system IR has peak magnitude 1. Farina's
    // construction places that peak exactly at convolution lag `N-1`
    // (= length-N forward ⊛ length-N reverse, the centre sample of the
    // 2N-1 output). Discrete sampling can shift the maximum by ±1 sample,
    // so evaluate the convolution at a small window of central lags via
    // direct dot products and take the max. O(k·N) for `k` lags beats
    // the previous full FFT convolution (one FFT + one IFFT of
    // next_pow_of_2(2N-1)) by an order of magnitude on long sweeps.
    let x: Vec<f64> = log_sweep(p)?.into_iter().map(|v| v as f64).collect();
    const HALF_WIN: i64 = 4;
    let centre = n as i64 - 1;
    let full_len = 2 * n - 1;
    let mut peak = 0.0_f64;
    for offset in -HALF_WIN..=HALF_WIN {
        let m = centre + offset;
        if m < 0 || (m as usize) >= full_len {
            continue;
        }
        let m = m as usize;
        let k_lo = m.saturating_sub(n - 1);
        let k_hi = m.min(n - 1);
        let mut s = 0.0_f64;
        for k in k_lo..=k_hi {
            s += x[k] * inv[m - k];
        }
        let av = s.abs();
        if av > peak {
            peak = av;
        }
    }
    if peak > 0.0 && peak.is_finite() {
        let scale = 1.0 / peak;
        for v in &mut inv {
            *v *= scale;
        }
    }

    Ok(inv.into_iter().map(|v| v as f32).collect())
}

/// Full linear convolution of `y` and `x_inv` via FFT. Returned length
/// is `y.len() + x_inv.len() - 1`. All math `f64` internally.
pub fn deconvolve_full(y: &[f32], x_inv: &[f32]) -> Vec<f64> {
    let y64: Vec<f64> = y.iter().map(|&v| v as f64).collect();
    let inv64: Vec<f64> = x_inv.iter().map(|&v| v as f64).collect();
    fft_linear_convolve(&y64, &inv64)
}

fn fft_linear_convolve(a: &[f64], b: &[f64]) -> Vec<f64> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let out_len = a.len() + b.len() - 1;
    let n = out_len.next_power_of_two();
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(n);
    let ifft = planner.plan_fft_inverse(n);

    let mut ap = vec![0.0_f64; n];
    ap[..a.len()].copy_from_slice(a);
    let mut bp = vec![0.0_f64; n];
    bp[..b.len()].copy_from_slice(b);

    let mut a_spec = fft.make_output_vec();
    let mut b_spec = fft.make_output_vec();
    fft.process(&mut ap, &mut a_spec).unwrap();
    fft.process(&mut bp, &mut b_spec).unwrap();
    for (s_a, s_b) in a_spec.iter_mut().zip(b_spec.iter()) {
        *s_a *= *s_b;
    }
    let mut out = vec![0.0_f64; n];
    ifft.process(&mut a_spec, &mut out).unwrap();
    let norm = 1.0 / n as f64;
    for v in &mut out {
        *v *= norm;
    }
    out.truncate(out_len);
    out
}

/// A single harmonic-order impulse response extracted from a Farina
/// deconvolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarmonicIr {
    pub order: u32,
    pub samples: Vec<f64>,
}

/// Outcome of `extract_irs`: the linear IR plus per-order harmonic IRs.
#[derive(Debug, Clone, PartialEq)]
pub struct DeconvolvedIrs {
    pub linear: Vec<f64>,
    pub harmonics: Vec<HarmonicIr>,
}

/// Split the full deconvolution output `full` into the linear IR plus
/// `n_harmonics - 1` pre-impulse harmonic IRs.
///
/// `full` is the output of [`deconvolve_full`] on a recording of a
/// sweep generated by [`log_sweep`] on `p`. `window_len` controls the
/// window length (samples) used for each IR; it must be ≤ the sample
/// distance between adjacent harmonic peaks to avoid cross-contamination
/// between orders.
///
/// The linear IR is centred at the sweep endpoint (sample `N−1` of the
/// forward sweep). Each harmonic IR is centred at
/// `linear_centre − round(Δt_k · fs)`.
pub fn extract_irs(
    full: &[f64],
    p: &SweepParams,
    n_harmonics: usize,
    window_len: usize,
) -> Result<DeconvolvedIrs> {
    p.validate()?;
    if n_harmonics == 0 {
        bail!("n_harmonics must be ≥ 1");
    }
    if window_len == 0 {
        bail!("window_len must be ≥ 1");
    }
    let n_sweep = p.n_samples();
    if full.len() < n_sweep {
        bail!(
            "convolution output too short: got {} samples, need at least {}",
            full.len(),
            n_sweep
        );
    }
    let linear_centre = n_sweep - 1;
    let linear = gate(full, linear_centre, window_len);

    let mut harmonics = Vec::with_capacity(n_harmonics.saturating_sub(1));
    let fs = p.sample_rate as f64;
    for k in 2..=(n_harmonics as u32) {
        let dt = p.harmonic_time_offset_s(k);
        let offset = (dt * fs).round() as i64;
        let centre = linear_centre as i64 - offset;
        if centre < 0 {
            harmonics.push(HarmonicIr {
                order: k,
                samples: Vec::new(),
            });
            continue;
        }
        let samples = gate(full, centre as usize, window_len);
        harmonics.push(HarmonicIr { order: k, samples });
    }
    Ok(DeconvolvedIrs { linear, harmonics })
}

/// Return `window_len` samples centred on `centre` within `buf`, padding
/// with zeros outside the buffer. The IR peak is placed at
/// `window_len / 2`.
fn gate(buf: &[f64], centre: usize, window_len: usize) -> Vec<f64> {
    let half = window_len / 2;
    let start = centre as i64 - half as i64;
    (0..window_len)
        .map(|i| {
            let idx = start + i as i64;
            if idx < 0 || (idx as usize) >= buf.len() {
                0.0
            } else {
                buf[idx as usize]
            }
        })
        .collect()
}

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
}

impl TailDecayCheck {
    /// One-line verdict, meant for `MeasurementReport.notes` — this is
    /// where acceptance criterion 6 (issue #282) puts the tail_s basis:
    /// not a pre-capture guess, a stated post-hoc check against the room
    /// actually measured.
    pub fn note(&self) -> String {
        if self.passed {
            format!(
                "ISO 18233 \u{a7}6.3.2 tail-decay check: worst-case 1/{}-oct {:.0} Hz band decayed \
                 {:.1} dB over the captured tail (need \u{2265}{:.0} dB) \u{2014} capture adequate.",
                self.bpo, self.worst_band_hz, self.worst_decay_db, self.required_db
            )
        } else {
            format!(
                "ISO 18233 \u{a7}6.3.2 tail-decay check FAILED: 1/{}-oct {:.0} Hz band only decayed \
                 {:.1} dB over the captured tail (need \u{2265}{:.0} dB) \u{2014} this capture may be \
                 unreliable for band-resolved work; re-run with a longer tail_s.",
                self.bpo, self.worst_band_hz, self.worst_decay_db, self.required_db
            )
        }
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
/// `[f1_hz, f2_hz]`. Reports the worst (smallest) per-band decay. Bands
/// with no measurable energy in the early window are excluded — there is
/// nothing there to decay.
///
/// `full` is the full [`deconvolve_full`] output (not the windowed
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
    let win = tail_len / 4;
    if win < 2 || linear_centre + tail_len > full.len() {
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

    let f_min = p.f1_hz.max(20.0);
    let f_max = p.f2_hz.min(fs * 0.45 - 1.0);
    let fb = Filterbank::new(p.sample_rate, BPO, f_min, f_max)?;
    let early_db = fb.process(&early);
    let late_db = fb.process(&late);
    let centres = fb.centres_hz();

    let mut worst: Option<(f64, f64)> = None; // (centre_hz, decay_db)
    for ((&e, &l), &c) in early_db.iter().zip(late_db.iter()).zip(centres.iter()) {
        if !e.is_finite() {
            continue; // no energy at capture start in this band — nothing to decay
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
    })
}

/// Citation for a `MeasurementReport` emitted from a Farina-sweep run.
///
/// The Farina technique is not covered by an IEC or AES standard; the
/// canonical reference is the AES 108th Convention preprint #5093 by
/// Angelo Farina, "Simultaneous measurement of impulse response and
/// distortion with a swept-sine technique" (Paris, 2000). Verified
/// against the full preprint PDF under `stddocs/iec-full/`.
pub fn citation() -> StandardsCitation {
    StandardsCitation {
        standard: "Farina, AES 108th Convention preprint #5093 (2000)".into(),
        clause: "§2 Theoretical basis (log sweep, inverse filter, harmonic offsets)".into(),
        verified: true,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn p_default() -> SweepParams {
        SweepParams {
            f1_hz: 20.0,
            f2_hz: 20_000.0,
            duration_s: 1.0,
            sample_rate: SR,
        }
    }

    #[test]
    fn params_validate() {
        assert!(p_default().validate().is_ok());
        let mut p = p_default();
        p.f1_hz = 0.0;
        assert!(p.validate().is_err());
        let mut p = p_default();
        p.f2_hz = p.f1_hz;
        assert!(p.validate().is_err());
        let mut p = p_default();
        p.duration_s = 0.0;
        assert!(p.validate().is_err());
        let mut p = p_default();
        p.f2_hz = 30_000.0; // above Nyquist/2
        assert!(p.validate().is_err());
    }

    #[test]
    fn harmonic_time_offsets_are_log_spaced() {
        let p = p_default();
        let dt2 = p.harmonic_time_offset_s(2);
        let dt3 = p.harmonic_time_offset_s(3);
        let dt4 = p.harmonic_time_offset_s(4);
        // ln(4) = 2·ln(2)
        assert!((dt4 - 2.0 * dt2).abs() < 1e-12);
        // ln(3) / ln(2) ≈ 1.585
        let ratio = dt3 / dt2;
        assert!((ratio - 3f64.ln() / 2f64.ln()).abs() < 1e-12);
    }

    #[test]
    fn sweep_starts_at_zero_phase() {
        let x = log_sweep(&p_default()).unwrap();
        assert!(x[0].abs() < 1e-6);
        assert!(x.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn identity_system_produces_unit_linear_ir_peak() {
        let p = p_default();
        let x = log_sweep(&p).unwrap();
        let xi = inverse_sweep(&p).unwrap();
        let full = deconvolve_full(&x, &xi);
        let irs = extract_irs(&full, &p, 1, 128).unwrap();
        let peak = irs
            .linear
            .iter()
            .cloned()
            .fold(0.0_f64, |m, v| m.max(v.abs()));
        // Normalisation inside inverse_sweep should bring this to 1.
        assert!(
            (peak - 1.0).abs() < 0.05,
            "identity IR peak = {peak}, expected ~1"
        );
        // Peak should be at the window centre.
        let peak_idx = irs
            .linear
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap()
            .0;
        assert_eq!(
            peak_idx, 64,
            "expected peak at window centre, got {peak_idx}"
        );
    }

    #[test]
    fn delayed_impulse_shifts_linear_ir() {
        // Model a pure delay: y(n) = x(n - d). Linear IR from Farina
        // should be a spike at (window_centre + d).
        let p = p_default();
        let x = log_sweep(&p).unwrap();
        let d = 17_usize;
        let mut y = vec![0.0_f32; x.len() + d];
        y[d..].copy_from_slice(&x);
        let xi = inverse_sweep(&p).unwrap();
        let full = deconvolve_full(&y, &xi);
        let irs = extract_irs(&full, &p, 1, 128).unwrap();
        let peak_idx = irs
            .linear
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap()
            .0;
        assert_eq!(peak_idx, 64 + d);
    }

    #[test]
    fn scaled_delay_recovers_magnitude_and_sign() {
        // A pure inverting half-gain channel y(n) = -0.5 · x(n − d). The
        // Farina-deconvolved IR should be a negative spike at
        // (window_centre + d) with peak magnitude ≈ 0.5.
        let p = p_default();
        let x = log_sweep(&p).unwrap();
        let d = 9_usize;
        let mut y = vec![0.0_f32; x.len() + d];
        for (i, &v) in x.iter().enumerate() {
            y[i + d] = -0.5 * v;
        }
        let xi = inverse_sweep(&p).unwrap();
        let full = deconvolve_full(&y, &xi);
        let irs = extract_irs(&full, &p, 1, 128).unwrap();
        let (peak_idx, peak_val) = irs
            .linear
            .iter()
            .enumerate()
            .map(|(i, v)| (i, *v))
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap();
        assert_eq!(peak_idx, 64 + d, "peak at wrong offset");
        assert!(peak_val < 0.0, "peak should be negative: {peak_val}");
        assert!(
            (peak_val.abs() - 0.5).abs() < 0.05,
            "|peak| {} should be ~0.5",
            peak_val.abs()
        );
    }

    #[test]
    fn cubic_nonlinearity_produces_third_harmonic_ir() {
        // y = a·x + b·x³ has a 3rd-harmonic component at scale |b|/4
        // relative to the fundamental's scale (a + 3b/4). The extracted
        // 3rd-harmonic IR is carried by the Farina inverse filter with a
        // frequency-dependent gain, so we don't pin the absolute ratio —
        // we just verify the 3rd-harmonic IR has meaningful energy when
        // the input is clearly nonlinear, and is essentially zero when
        // the input is linear.
        let p = p_default();
        let x = log_sweep(&p).unwrap();
        let xi = inverse_sweep(&p).unwrap();
        let window = 128;

        // Linear baseline: no 3rd-harmonic energy.
        let full_lin = deconvolve_full(&x, &xi);
        let irs_lin = extract_irs(&full_lin, &p, 3, window).unwrap();
        let lin_only_h3_peak = irs_lin
            .harmonics
            .iter()
            .find(|h| h.order == 3)
            .unwrap()
            .samples
            .iter()
            .cloned()
            .fold(0.0_f64, |m, v| m.max(v.abs()));

        // Cubic nonlinearity: substantial 3rd-harmonic energy.
        let b = 0.3_f32;
        let y: Vec<f32> = x.iter().map(|&v| v + b * v * v * v).collect();
        let full_nl = deconvolve_full(&y, &xi);
        let irs_nl = extract_irs(&full_nl, &p, 3, window).unwrap();
        let nl_h3_peak = irs_nl
            .harmonics
            .iter()
            .find(|h| h.order == 3)
            .unwrap()
            .samples
            .iter()
            .cloned()
            .fold(0.0_f64, |m, v| m.max(v.abs()));

        assert!(
            nl_h3_peak > 3.0 * lin_only_h3_peak,
            "expected nonlinear 3rd-harmonic peak ({nl_h3_peak:.5}) to clearly exceed the linear baseline ({lin_only_h3_peak:.5})"
        );
        assert!(
            nl_h3_peak > 0.001,
            "nonlinear 3rd-harmonic peak too small: {nl_h3_peak}"
        );
    }

    #[test]
    fn citation_shape() {
        let c = citation();
        assert!(c.standard.contains("Farina"));
        assert!(c.clause.contains("§2"));
        assert!(c.verified);
    }

    // ─── check_tail_decay (#282 acceptance criterion 6) ────────────

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
}
