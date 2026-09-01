//! Quasi-anechoic frequency response: time-gate the linear impulse
//! response with a tapered-cosine window and FFT the result.

use std::f64::consts::PI;

use realfft::RealFftPlanner;

use super::deconv::FFT_LEN_INVARIANT;
use super::harmonics::{gate_centre_index, gate_weighted};

/// Tapered-cosine (Tukey) window of `len` samples. `alpha` is the
/// fraction of the window given over to the cosine taper at each edge:
/// `alpha = 0.0` is rectangular (no taper), `alpha = 1.0` is a full Hann
/// window, and values between taper only the edges while the interior
/// stays flat at unity gain. Used to gate the linear IR into a
/// magnitude/phase curve (#284) without the spectral leakage a hard
/// rectangular edge would cause, and without a full Hann's bias on
/// legitimate early-response samples near the gate edges.
pub fn tukey_window(len: usize, alpha: f64) -> Vec<f64> {
    if len == 0 {
        return Vec::new();
    }
    if len == 1 {
        return vec![1.0];
    }
    let alpha = alpha.clamp(0.0, 1.0);
    if alpha == 0.0 {
        return vec![1.0; len];
    }
    let n = (len - 1) as f64;
    let taper_len = alpha * n / 2.0;
    (0..len)
        .map(|i| {
            let x = i as f64;
            if taper_len <= 0.0 {
                1.0
            } else if x < taper_len {
                0.5 * (1.0 + (PI * (x / taper_len - 1.0)).cos())
            } else if x > n - taper_len {
                0.5 * (1.0 + (PI * ((x - n) / taper_len + 1.0)).cos())
            } else {
                1.0
            }
        })
        .collect()
}

/// One point of a gated frequency response: magnitude and (wrapped)
/// phase of the FFT of a time-gated impulse response at `freq_hz`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GatedResponsePoint {
    pub freq_hz: f64,
    /// `20·log10(|H(f)|)`. `H` is the raw FFT of the gated, windowed
    /// impulse response — no additional normalisation — so an identity
    /// system (unit-magnitude IR spike) reads 0 dB at every bin.
    pub magnitude_db: f64,
    /// `atan2` phase in degrees, wrapped to `(-180, 180]` — not
    /// unwrapped. Unwrapping is a separate algorithmic choice with its
    /// own correctness bar; wrapped `atan2` output is what was actually
    /// measured (#284).
    pub phase_deg: f64,
}

/// Gate `linear_ir` to `gate_length_s` seconds starting `gate_start_s`
/// seconds after the IR's peak-reference sample (`linear_ir.len() / 2` —
/// the same zero-delay reference [`crate::measurement::report::MeasurementReport::ir_stats`]
/// uses, stable across window-length changes per #278), taper the gated
/// region with a [`tukey_window`] of shape `alpha`, and FFT the result to
/// a magnitude/phase curve.
///
/// Samples the gate requests outside `linear_ir`'s bounds are zero —
/// matching [`super::extract_irs`]'s own `gate` helper. Returns an empty vec
/// when `linear_ir` is empty or the requested gate is shorter than 2
/// samples (nothing to FFT).
pub fn gated_frequency_response(
    linear_ir: &[f64],
    sample_rate: u32,
    gate_start_s: f64,
    gate_length_s: f64,
    alpha: f64,
) -> Vec<GatedResponsePoint> {
    if linear_ir.is_empty() || sample_rate == 0 {
        return Vec::new();
    }
    let fs = sample_rate as f64;
    let gate_len = (gate_length_s * fs).round() as i64;
    if gate_len < 2 {
        return Vec::new();
    }
    let gate_len = gate_len as usize;

    let centre = gate_centre_index(linear_ir.len()) as i64;
    let start = centre + (gate_start_s * fs).round() as i64;
    let window = tukey_window(gate_len, alpha);
    let mut buf = gate_weighted(linear_ir, start, gate_len, |i| window[i]);

    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(gate_len);
    let mut spec = fft.make_output_vec();
    fft.process(&mut buf, &mut spec).expect(FFT_LEN_INVARIANT);

    spec.iter()
        .enumerate()
        .map(|(k, c)| {
            let freq_hz = k as f64 * fs / gate_len as f64;
            let mag = c.norm().max(1e-300);
            GatedResponsePoint {
                freq_hz,
                magnitude_db: 20.0 * mag.log10(),
                phase_deg: c.arg().to_degrees(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement::sweep::extract_irs;
    use crate::measurement::sweep::testkit::*;

    #[test]
    fn tukey_window_alpha_zero_is_rectangular() {
        let w = tukey_window(16, 0.0);
        assert_eq!(w, vec![1.0; 16]);
    }

    #[test]
    fn tukey_window_alpha_one_matches_hand_derived_hann() {
        let n = 32usize;
        let w = tukey_window(n, 1.0);
        for (i, &v) in w.iter().enumerate() {
            // Hand-derived Hann formula, computed independently of the
            // implementation's cosine-taper construction.
            let expect = 0.5 * (1.0 - (2.0 * PI * i as f64 / (n - 1) as f64).cos());
            assert!(
                (v - expect).abs() < 1e-9,
                "index {i}: got {v}, hand-derived Hann expects {expect}"
            );
        }
    }

    #[test]
    fn tukey_window_edges_taper_and_interior_stays_flat() {
        let w = tukey_window(100, 0.25);
        assert!(w[0] < 0.01, "window should start near zero: {}", w[0]);
        assert!(
            (w[99] - w[0]).abs() < 1e-9,
            "window should be symmetric: {} vs {}",
            w[0],
            w[99]
        );
        // Interior (well clear of the 12.5%-per-side taper) stays at unity.
        assert!(
            (w[50] - 1.0).abs() < 1e-9,
            "interior should be flat: {}",
            w[50]
        );
    }

    #[test]
    fn gated_frequency_response_of_a_flat_system_is_flat() {
        // A pure unit impulse at the gate's centre reference — FFT of a
        // delta is flat magnitude at every bin, by hand: |FFT(delta)| = 1.
        let sr = 48_000u32;
        let len = 256usize;
        let mut ir = vec![0.0_f64; len];
        ir[len / 2] = 1.0;
        let points = gated_frequency_response(&ir, sr, 0.0, (len / 2) as f64 / sr as f64, 0.0);
        assert!(
            points.len() > 4,
            "expected several bins, got {}",
            points.len()
        );
        for p in &points {
            assert!(
                p.magnitude_db.abs() < 0.1,
                "flat system should read ~0 dB at {} Hz, got {} dB",
                p.freq_hz,
                p.magnitude_db
            );
        }
    }

    #[test]
    fn gated_frequency_response_recovers_a_known_single_pole() {
        // h[n] = r^n for n = 0..gate_len, a causal one-pole IR, placed
        // starting exactly at the gate's centre reference so a rectangular
        // (alpha = 0.0) gate captures it whole with no truncation-vs-window
        // ambiguity. Hand-derived expectation: the finite geometric-sum DTFT
        // H(w) = sum_{n=0}^{N-1} r^n e^{-jwn} = (1 - (r e^{-jw})^N) / (1 - r e^{-jw}),
        // computed independently of `gated_frequency_response`'s FFT path.
        let sr = 48_000u32;
        let gate_len = 512usize;
        let r = 0.85_f64;
        let mut ir = vec![0.0_f64; gate_len * 2];
        let centre = ir.len() / 2;
        for n in 0..gate_len {
            ir[centre + n] = r.powi(n as i32);
        }
        let gate_length_s = gate_len as f64 / sr as f64;
        let points = gated_frequency_response(&ir, sr, 0.0, gate_length_s, 0.0);
        assert_eq!(points.len(), gate_len / 2 + 1);

        for p in points.iter().step_by(7) {
            let w = 2.0 * PI * p.freq_hz / sr as f64;
            let re_pole = r * w.cos();
            let im_pole = -r * w.sin();
            // (r e^{-jw})^N via repeated squaring on the complex pole.
            let (mut pow_re, mut pow_im) = (1.0_f64, 0.0_f64);
            for _ in 0..gate_len {
                let nr = pow_re * re_pole - pow_im * im_pole;
                let ni = pow_re * im_pole + pow_im * re_pole;
                pow_re = nr;
                pow_im = ni;
            }
            let num_re = 1.0 - pow_re;
            let num_im = -pow_im;
            let den_re = 1.0 - re_pole;
            let den_im = -im_pole;
            let den_mag_sq = den_re * den_re + den_im * den_im;
            let h_re = (num_re * den_re + num_im * den_im) / den_mag_sq;
            let h_im = (num_im * den_re - num_re * den_im) / den_mag_sq;
            let expect_mag_db = 20.0 * (h_re * h_re + h_im * h_im).sqrt().max(1e-300).log10();
            assert!(
                (p.magnitude_db - expect_mag_db).abs() < 0.5,
                "at {} Hz: got {} dB, hand-derived single-pole expects {} dB",
                p.freq_hz,
                p.magnitude_db,
                expect_mag_db
            );
        }
    }

    #[test]
    fn gate_length_implies_f_low_by_hand_arithmetic() {
        // f_low_hz = 1 / gate_length_s, the same formula `GateParams`
        // stores (#280) — pinned here independent of any report/daemon
        // plumbing. 960 samples @ 48 kHz = 20 ms -> f_low = 50 Hz.
        let sr = 48_000u32;
        let gate_len_samples = 960usize;
        let gate_length_s = gate_len_samples as f64 / sr as f64;
        let f_low_hz = 1.0 / gate_length_s;
        assert!((gate_length_s - 0.020).abs() < 1e-12);
        assert!((f_low_hz - 50.0).abs() < 1e-9);
    }

    #[test]
    fn gated_response_differs_from_ungated_when_a_reflection_is_excluded() {
        // Direct impulse at the gate's centre reference plus a later,
        // scaled "reflection" impulse. A short gate that ends before the
        // reflection must produce a smoother (lower-ripple) response than
        // the same IR analysed with a gate long enough to include the
        // reflection's comb-filter interference — mutation-verify: compute
        // the rejected case (comb-filtering left in) directly and confirm
        // the gated result is measurably different from it.
        let sr = 48_000u32;
        let len = 2048usize;
        let mut ir = vec![0.0_f64; len];
        let centre = len / 2;
        let reflection_offset = 200usize; // samples after the direct sound
        ir[centre] = 1.0;
        ir[centre + reflection_offset] = 0.5;

        let short_gate_s = (reflection_offset - 20) as f64 / sr as f64; // excludes the reflection
        let long_gate_s = (reflection_offset + 400) as f64 / sr as f64; // includes it
        let gated = gated_frequency_response(&ir, sr, 0.0, short_gate_s, 0.0);
        let ungated = gated_frequency_response(&ir, sr, 0.0, long_gate_s, 0.0);

        // Ripple (variance of magnitude_db across bins) is what a
        // reflection's comb filtering predicts: present when the
        // reflection is inside the gate, reduced when it is excluded.
        let variance = |pts: &[GatedResponsePoint]| -> f64 {
            let mean = pts.iter().map(|p| p.magnitude_db).sum::<f64>() / pts.len() as f64;
            pts.iter()
                .map(|p| (p.magnitude_db - mean).powi(2))
                .sum::<f64>()
                / pts.len() as f64
        };
        let gated_var = variance(&gated);
        let ungated_var = variance(&ungated);
        assert!(
            gated_var < ungated_var,
            "gate excluding the reflection should read smoother than the \
             gate including it: gated variance {gated_var}, ungated (with \
             reflection) variance {ungated_var}"
        );
    }

    /// `extract_irs` centres its gate on the linear IR at
    /// `gate_centre_index(window_len)`, and `gated_frequency_response`
    /// measures `gate_start_s` from `gate_centre_index(linear_ir.len())`.
    /// Those two conventions are only useful together: if either side
    /// moved by a sample, a `gate_start_s` of 0.0 would no longer land on
    /// the IR peak and every gated response would carry a bogus linear
    /// phase ramp — silently, because magnitude would be unchanged.
    ///
    /// Nothing joined the two before: the other gated tests hand-build an
    /// IR with the peak at `len / 2`, restating the convention instead of
    /// taking it from `extract_irs`. This one runs the real path — a pure
    /// `d`-sample delay through `extract_irs` — and checks the phase the
    /// FFT reports against the hand-derived phase of a delta at lag `d`,
    /// `-360·f·d/fs`, which a one-sample disagreement would break by
    /// `360·f/fs` (90° at 12 kHz).
    ///
    /// What it catches is the two sides *diverging* — confirmed by
    /// shifting `gated_frequency_response`'s centre alone, which reddens
    /// this at bin 1. It does not catch a redefinition of
    /// `gate_centre_index` itself: both sides read it, so a common-mode
    /// change cancels exactly, and the pairing is still right in that case.
    #[test]
    fn extracted_linear_ir_is_centred_where_gated_response_expects_it() {
        let p = p_default();
        const DELAY: usize = 5;
        const WINDOW_LEN: usize = 256;
        const GATE_LEN: usize = 128;

        // A system that is a pure DELAY-sample delay: the Farina
        // deconvolution of it is a unit impulse DELAY samples past the
        // sweep endpoint.
        let linear_centre = p.n_samples() - 1;
        let mut full = vec![0.0_f64; linear_centre + WINDOW_LEN];
        full[linear_centre + DELAY] = 1.0;

        let irs = extract_irs(&full, &p, 1, WINDOW_LEN).unwrap();
        let points = gated_frequency_response(
            &irs.linear,
            p.sample_rate,
            0.0,
            GATE_LEN as f64 / p.sample_rate as f64,
            0.0,
        );
        assert_eq!(points.len(), GATE_LEN / 2 + 1);

        // Wrap into (-180, 180], matching `atan2`'s own range.
        fn wrap_deg(d: f64) -> f64 {
            let mut d = d % 360.0;
            if d > 180.0 {
                d -= 360.0;
            }
            if d <= -180.0 {
                d += 360.0;
            }
            d
        }

        for (k, pt) in points.iter().enumerate() {
            assert!(
                pt.magnitude_db.abs() < 1e-9,
                "a unit delta gates to 0 dB at every bin; bin {k} read {} dB",
                pt.magnitude_db
            );
            // |FFT(delta at lag d)| = 1 with phase -2*pi*k*d/GATE_LEN.
            let expected = wrap_deg(-360.0 * (k * DELAY) as f64 / GATE_LEN as f64);
            let err = wrap_deg(pt.phase_deg - expected).abs();
            assert!(
                err < 1e-6,
                "gate centring drifted: bin {k} ({} Hz) read {}\u{b0}, a delta \
                 at lag {DELAY} is {expected}\u{b0}",
                pt.freq_hz,
                pt.phase_deg
            );
        }
    }
}
