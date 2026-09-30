//! Sweep generation and Farina deconvolution: the stimulus `x(t)`, its
//! closed-form inverse filter `x_inv(t)`, and the FFT linear convolution
//! that turns a recording of the one into an impulse response through the
//! other. See the module docs on [`super`] for the theory and citations.

use std::f64::consts::PI;

use anyhow::Result;
use realfft::RealFftPlanner;

use super::SweepParams;

/// Instantaneous phase of the forward sweep, `K·(e^(t/L) − 1)` with
/// `K = 2π·f1·L` (Farina §2). Returned as a closure over the two
/// parameter-derived constants so [`log_sweep`] and [`inverse_sweep`]
/// share one expression of the formula rather than each restating it.
fn sweep_phase(p: &SweepParams) -> impl Fn(f64) -> f64 {
    let l = p.time_constant();
    let k_phase = 2.0 * PI * p.f1_hz * l;
    move |t| k_phase * ((t / l).exp() - 1.0)
}

/// The exponential sine sweep at full `f64` precision. [`log_sweep`] is
/// this rounded to `f32` for playback; [`inverse_sweep`] builds its
/// time-reversed filter from the unrounded values.
fn log_sweep_f64(p: &SweepParams) -> Result<Vec<f64>> {
    p.validate()?;
    let n = p.n_samples();
    let fs = p.sample_rate as f64;
    let phase = sweep_phase(p);
    Ok((0..n).map(|i| phase(i as f64 / fs).sin()).collect())
}

/// Generate the exponential sine sweep `x(t)` at unit peak amplitude.
pub fn log_sweep(p: &SweepParams) -> Result<Vec<f32>> {
    Ok(log_sweep_f64(p)?.into_iter().map(|v| v as f32).collect())
}

/// Generate Farina's inverse filter `x_inv(t)`: the time-reversed sweep,
/// amplitude-modulated by `exp(+(T-t)/L)` — equivalently `exp(-t/L)` in the
/// reversed filter's own time index, a 6 dB/octave decay — so that the
/// spectrum of `x(t) * x_inv(t)` is flat and the convolution approximates a
/// unit impulse at `t = T` regardless of `log(f2/f1)`.
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
    let a = 1.0 / p.time_constant();

    // Sample `i` of the time-reversed sweep is sample `n-1-i` of the
    // forward sweep, so build the forward sweep once and read it
    // backwards instead of re-deriving the phase here — the same buffer
    // then serves the normalisation dot products below.
    let x = log_sweep_f64(p)?;
    let mut inv: Vec<f64> = (0..n)
        .map(|i| {
            let j = n - 1 - i;
            let t_fwd = j as f64 / fs;
            // In the *reversed* filter's own time index the amplitude must
            // fall 6 dB/octave: the reversed sweep descends in frequency,
            // so an envelope proportional to the instantaneous frequency
            // cancels the ESS's f^-1/2 magnitude on both sides:
            //
            //   |X(f)| ∝ f^-1/2   (ESS, dt/df = L/f)
            //   w ∝ e^(+t_fwd/L) ∝ f
            //   |X| · w · |X| ∝ f^-1/2 · f · f^-1/2 = const
            //
            // #504: this read e^(-t_fwd/L) until 2026-09-30, tilting every
            // `plot ir` response by -12 dB/octave (≈ 110 dB across
            // 20 Hz–20 kHz) and smearing its impulse into a slow,
            // low-frequency tail, while the peak normalisation below kept
            // the identity peak at exactly 1 — so every peak-based test
            // passed. The fix was first written on 2026-08-28 (d6c8fd85,
            // rig-verified: loopback 100 dB p-p → 0.6 dB p-p) and never
            // merged. `identity_deconvolution_is_spectrally_flat` pins it.
            x[j] * (a * t_fwd).exp()
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

/// `realfft`'s `process` fails only when a buffer length disagrees with
/// the planned transform size. Every call site in this module allocates
/// its buffers from that same size, so a failure is a bug here, not a
/// runtime condition a caller could recover from — every call site says
/// so with the same message rather than one panicking and another
/// silently returning nothing.
pub(super) const FFT_LEN_INVARIANT: &str = "FFT buffer length matches the planned transform size";

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
    fft.process(&mut ap, &mut a_spec).expect(FFT_LEN_INVARIANT);
    fft.process(&mut bp, &mut b_spec).expect(FFT_LEN_INVARIANT);
    for (s_a, s_b) in a_spec.iter_mut().zip(b_spec.iter()) {
        *s_a *= *s_b;
    }
    let mut out = vec![0.0_f64; n];
    ifft.process(&mut a_spec, &mut out)
        .expect(FFT_LEN_INVARIANT);
    let norm = 1.0 / n as f64;
    for v in &mut out {
        *v *= norm;
    }
    out.truncate(out_len);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement::sweep::testkit::*;

    /// The deconvolution kernel `x ⊛ x_inv` must be spectrally *flat*, not
    /// merely unit-peaked (#504; first written on the never-merged
    /// d6c8fd85). The peak normalisation in `inverse_sweep` holds the
    /// identity peak at 1 whichever way the envelope leans, so a sign error
    /// passes every peak check while tilting the response 12 dB/octave.
    /// Computes the rejected (wrong-sign) kernel inline and requires the
    /// shipped one to be far flatter, so a gentler version of the same
    /// mistake cannot satisfy it.
    #[test]
    fn identity_deconvolution_is_spectrally_flat() {
        use rustfft::{num_complex::Complex, FftPlanner};

        let p = SweepParams {
            f1_hz: 20.0,
            f2_hz: 20_000.0,
            duration_s: 0.5,
            sample_rate: 96_000,
        };
        let n = p.n_samples();
        let fs = p.sample_rate as f64;
        let a = 1.0 / p.time_constant();
        let x = log_sweep(&p).unwrap();
        let x64 = log_sweep_f64(&p).unwrap();

        // Ripple, dB peak-to-peak, of |FFT(x ⊛ x_inv)| well inside the
        // swept band, for a given inverse.
        let ripple_db = |inv: &[f32]| -> f64 {
            let full = deconvolve_full(&x, inv);
            let fft_len = full.len().next_power_of_two();
            let mut buf: Vec<Complex<f64>> = full
                .iter()
                .map(|&v| Complex::new(v, 0.0))
                .chain(std::iter::repeat(Complex::new(0.0, 0.0)))
                .take(fft_len)
                .collect();
            FftPlanner::<f64>::new()
                .plan_fft_forward(fft_len)
                .process(&mut buf);
            let bin_hz = fs / fft_len as f64;
            let lo = (2.0 * p.f1_hz / bin_hz).ceil() as usize;
            let hi = (0.5 * p.f2_hz / bin_hz).floor() as usize;
            let mags: Vec<f64> = (lo..=hi)
                .map(|k| 20.0 * buf[k].norm().max(1e-300).log10())
                .collect();
            let max = mags.iter().cloned().fold(f64::MIN, f64::max);
            let min = mags.iter().cloned().fold(f64::MAX, f64::min);
            max - min
        };

        let shipped = ripple_db(&inverse_sweep(&p).unwrap());
        let rejected_inv: Vec<f32> = (0..n)
            .map(|i| {
                let j = n - 1 - i;
                (x64[j] * (-a * j as f64 / fs).exp()) as f32
            })
            .collect();
        let rejected = ripple_db(&rejected_inv);

        assert!(shipped < 6.0, "identity deconvolution {shipped:.1} dB p-p");
        assert!(
            rejected > 40.0,
            "the wrong sign should be grossly tilted: {rejected:.1} dB p-p"
        );
        assert!(
            rejected > shipped + 30.0,
            "shipped {shipped:.1} dB p-p vs rejected {rejected:.1} dB p-p"
        );
    }

    #[test]
    fn sweep_starts_at_zero_phase() {
        let x = log_sweep(&p_default()).unwrap();
        assert!(x[0].abs() < 1e-6);
        assert!(x.iter().all(|v| v.is_finite()));
    }

    /// Params whose `n_samples()` rounds to 0 reach `inverse_sweep` without
    /// a caller-side `validate()`; it must refuse them rather than compute
    /// `2 * n - 1` on `n = 0`.
    #[test]
    fn inverse_sweep_refuses_zero_length() {
        let mut p = p_default();
        p.duration_s = 0.49 / p.sample_rate as f64;
        assert_eq!(p.n_samples(), 0);
        assert!(inverse_sweep(&p).is_err());
    }
}
