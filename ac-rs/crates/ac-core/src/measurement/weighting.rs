//! Tier 1 — A / C / Z frequency weighting per IEC 61672-1:2013.
//!
//! Designs the analog A- and C-weighting transfer functions from the
//! standard's pole / zero locations, maps them to digital biquads via the
//! bilinear transform, and normalises each cascade to unity gain at 1 kHz.
//! Z weighting (flat) is the identity filter.
//!
//! Tests compare the digital magnitude response at standard reference
//! frequencies against IEC 61672-1 Table 2 values within published
//! Class 1 tolerances.

use std::f64::consts::PI;

use anyhow::{bail, Result};
use realfft::num_complex::Complex;

use crate::measurement::report::StandardsCitation;

/// Pole angular-frequency constants from IEC 61672-1:2013, Annex E.
const F_1: f64 = 20.598_997_f64;
const F_2: f64 = 107.652_65_f64;
const F_3: f64 = 737.862_23_f64;
const F_4: f64 = 12_194.217_f64;

/// Which weighting curve to apply.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Weighting {
    /// A-weighting — IEC 61672-1 §5.4.11, Annex E.
    A,
    /// C-weighting — IEC 61672-1 §5.4.11, Annex E.
    C,
    /// Z-weighting — IEC 61672-1 §5.4.6. Flat from 10 Hz to 20 kHz.
    Z,
}

#[derive(Clone, Debug, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad {
    fn transfer(&self, z_inv: Complex<f64>) -> Complex<f64> {
        let z2 = z_inv * z_inv;
        let num = Complex::new(self.b0, 0.0) + z_inv * self.b1 + z2 * self.b2;
        let den = Complex::new(1.0, 0.0) + z_inv * self.a1 + z2 * self.a2;
        num / den
    }
}

/// Streaming weighting filter. Keeps internal state across successive
/// `apply` calls; use `reset` to flush.
#[derive(Clone, Debug)]
pub struct WeightingFilter {
    sample_rate: u32,
    weighting: Weighting,
    biquads: Vec<Biquad>,
    /// DF2T state: `[s1, s2]` per biquad.
    state: Vec<[f64; 2]>,
}

impl WeightingFilter {
    pub fn new(weighting: Weighting, sample_rate: u32) -> Result<Self> {
        if sample_rate == 0 {
            bail!("sample_rate must be positive");
        }
        let fs = sample_rate as f64;
        let biquads = match weighting {
            Weighting::A => design_a_weighting(fs),
            Weighting::C => design_c_weighting(fs),
            Weighting::Z => vec![Biquad {
                b0: 1.0,
                b1: 0.0,
                b2: 0.0,
                a1: 0.0,
                a2: 0.0,
            }],
        };
        let state = vec![[0.0; 2]; biquads.len()];
        Ok(Self {
            sample_rate,
            weighting,
            biquads,
            state,
        })
    }

    pub fn weighting(&self) -> Weighting {
        self.weighting
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Run `samples` through the cascade, returning the filtered output.
    /// Streaming: state is preserved across calls.
    pub fn apply(&mut self, samples: &[f32]) -> Vec<f32> {
        samples
            .iter()
            .map(|&x| self.process_sample(x as f64) as f32)
            .collect()
    }

    fn process_sample(&mut self, x_in: f64) -> f64 {
        let mut x = x_in;
        for (bq, s) in self.biquads.iter().zip(self.state.iter_mut()) {
            // Direct Form II Transposed.
            let y = bq.b0 * x + s[0];
            s[0] = bq.b1 * x - bq.a1 * y + s[1];
            s[1] = bq.b2 * x - bq.a2 * y;
            x = y;
        }
        x
    }

    /// Flush internal state to zero.
    pub fn reset(&mut self) {
        for s in self.state.iter_mut() {
            *s = [0.0; 2];
        }
    }

    /// Magnitude of the cascade at `f_hz`, in dB.
    pub fn magnitude_db(&self, f_hz: f64) -> f64 {
        let omega = 2.0 * PI * f_hz / self.sample_rate as f64;
        let z_inv = Complex::from_polar(1.0, -omega);
        let h: Complex<f64> = self
            .biquads
            .iter()
            .fold(Complex::new(1.0, 0.0), |acc, bq| acc * bq.transfer(z_inv));
        20.0 * h.norm().log10()
    }

    pub fn citation() -> StandardsCitation {
        StandardsCitation {
            standard: "IEC 61672-1:2013".into(),
            clause: "§5.5 Frequency weightings, Annex E".into(),
            verified: false,
        }
    }
}

/// Bilinear-transform helper: analog pole/zero at `-ω` (real, LHP) →
/// digital pole/zero at z = (2·fs − ω) / (2·fs + ω).
fn blt_real(omega: f64, fs: f64) -> f64 {
    let fs2 = 2.0 * fs;
    (fs2 - omega) / (fs2 + omega)
}

fn cascade_magnitude(biquads: &[Biquad], f_hz: f64, fs: f64) -> f64 {
    let omega = 2.0 * PI * f_hz / fs;
    let z_inv = Complex::from_polar(1.0, -omega);
    let h: Complex<f64> = biquads
        .iter()
        .fold(Complex::new(1.0, 0.0), |acc, bq| acc * bq.transfer(z_inv));
    h.norm()
}

/// Design the A-weighting biquad cascade at `fs`. 4 analog zeros at s=0
/// and 6 real analog poles (doubled at ω₁ and ω₄, single at ω₂ and ω₃).
/// After BLT: 4 digital zeros at z=1 plus 2 "infinity" zeros at z=-1,
/// balancing the 6 digital poles. Normalised to 0 dB at 1 kHz.
fn design_a_weighting(fs: f64) -> Vec<Biquad> {
    let w1 = 2.0 * PI * F_1;
    let w2 = 2.0 * PI * F_2;
    let w3 = 2.0 * PI * F_3;
    let w4 = 2.0 * PI * F_4;

    let z1 = blt_real(w1, fs);
    let z2 = blt_real(w2, fs);
    let z3 = blt_real(w3, fs);
    let z4 = blt_real(w4, fs);

    // Biquad 1: 2 zeros at z=1, 2 poles at z1.
    let bq1 = Biquad {
        b0: 1.0,
        b1: -2.0,
        b2: 1.0,
        a1: -2.0 * z1,
        a2: z1 * z1,
    };
    // Biquad 2: 2 zeros at z=1, 2 poles at z4.
    let bq2 = Biquad {
        b0: 1.0,
        b1: -2.0,
        b2: 1.0,
        a1: -2.0 * z4,
        a2: z4 * z4,
    };
    // Biquad 3: 2 zeros at z=-1 (the "infinity" zeros), poles at z2, z3.
    let bq3 = Biquad {
        b0: 1.0,
        b1: 2.0,
        b2: 1.0,
        a1: -(z2 + z3),
        a2: z2 * z3,
    };

    let mut bqs = vec![bq1, bq2, bq3];
    let g = cascade_magnitude(&bqs, 1000.0, fs);
    let scale = 1.0 / g;
    bqs[0].b0 *= scale;
    bqs[0].b1 *= scale;
    bqs[0].b2 *= scale;
    bqs
}

/// Design the C-weighting biquad cascade at `fs`. 2 analog zeros at s=0
/// and 4 real analog poles (doubled at ω₁ and ω₄). After BLT: 2 digital
/// zeros at z=1 plus 2 "infinity" zeros at z=-1, with 4 digital poles.
/// Normalised to 0 dB at 1 kHz.
fn design_c_weighting(fs: f64) -> Vec<Biquad> {
    let w1 = 2.0 * PI * F_1;
    let w4 = 2.0 * PI * F_4;

    let z1 = blt_real(w1, fs);
    let z4 = blt_real(w4, fs);

    // Biquad 1: 2 zeros at z=1, 2 poles at z1.
    let bq1 = Biquad {
        b0: 1.0,
        b1: -2.0,
        b2: 1.0,
        a1: -2.0 * z1,
        a2: z1 * z1,
    };
    // Biquad 2: 2 zeros at z=-1, 2 poles at z4.
    let bq2 = Biquad {
        b0: 1.0,
        b1: 2.0,
        b2: 1.0,
        a1: -2.0 * z4,
        a2: z4 * z4,
    };

    let mut bqs = vec![bq1, bq2];
    let g = cascade_magnitude(&bqs, 1000.0, fs);
    let scale = 1.0 / g;
    bqs[0].b0 *= scale;
    bqs[0].b1 *= scale;
    bqs[0].b2 *= scale;
    bqs
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const FS: u32 = 48_000;

    /// Reference A-weighting values, IEC 61672-1:2013 Table 2.
    fn a_reference_db(f_hz: f64) -> f64 {
        match f_hz as u32 {
            10 => -70.4,
            31 => -39.4,
            1000 => 0.0,
            4000 => 1.0,
            8000 => -1.1,
            16000 => -6.6,
            _ => unreachable!(),
        }
    }

    /// Reference C-weighting values, IEC 61672-1:2013 Table 2.
    fn c_reference_db(f_hz: f64) -> f64 {
        match f_hz as u32 {
            10 => -14.3,
            31 => -3.0,
            1000 => 0.0,
            4000 => -0.8,
            8000 => -3.0,
            16000 => -8.5,
            _ => unreachable!(),
        }
    }

    #[test]
    fn rejects_zero_sample_rate() {
        assert!(WeightingFilter::new(Weighting::A, 0).is_err());
    }

    #[test]
    fn a_weighting_is_0db_at_1khz() {
        let f = WeightingFilter::new(Weighting::A, FS).unwrap();
        assert!(f.magnitude_db(1000.0).abs() < 0.01);
    }

    #[test]
    fn c_weighting_is_0db_at_1khz() {
        let f = WeightingFilter::new(Weighting::C, FS).unwrap();
        assert!(f.magnitude_db(1000.0).abs() < 0.01);
    }

    #[test]
    fn z_weighting_is_flat() {
        let f = WeightingFilter::new(Weighting::Z, FS).unwrap();
        for &fr in &[20.0, 100.0, 1000.0, 8000.0, 20_000.0] {
            assert!(f.magnitude_db(fr).abs() < 1e-9);
        }
    }

    #[test]
    fn a_weighting_matches_standard_within_class1_tolerance() {
        // Class 1 tolerance at these frequencies, per Table 2:
        // 31.5 Hz → ±1.5 dB, 1 kHz → ±0.7 dB, 4 kHz → ±1.6 dB,
        // 8 kHz → ±(2.1, 3.1) dB (use the tighter 2.1 for pass), 16 kHz →
        // +3.5/-17 dB (skip — tolerance unbounded below).
        let f = WeightingFilter::new(Weighting::A, FS).unwrap();
        for &(fr, tol) in &[(31.0_f64, 1.5), (1000.0, 0.7), (4000.0, 1.6), (8000.0, 2.1)] {
            let got = f.magnitude_db(fr);
            let expected = a_reference_db(fr);
            assert!(
                (got - expected).abs() <= tol,
                "A-weighting @ {fr} Hz: got {got:.2} dB, expected {expected:.2} (±{tol} dB)"
            );
        }
    }

    #[test]
    fn c_weighting_matches_standard_within_class1_tolerance() {
        let f = WeightingFilter::new(Weighting::C, FS).unwrap();
        for &(fr, tol) in &[(31.0_f64, 1.5), (1000.0, 0.7), (4000.0, 1.6), (8000.0, 2.1)] {
            let got = f.magnitude_db(fr);
            let expected = c_reference_db(fr);
            assert!(
                (got - expected).abs() <= tol,
                "C-weighting @ {fr} Hz: got {got:.2} dB, expected {expected:.2} (±{tol} dB)"
            );
        }
    }

    /// Drive a sine of frequency `f_hz` at peak amplitude 1 through the
    /// filter and compute the steady-state RMS ratio (post vs. pre).
    fn sine_through(filter: &mut WeightingFilter, f_hz: f64) -> f64 {
        let fs = filter.sample_rate() as f64;
        let n: usize = (fs * 1.0) as usize; // 1 second
        let w = 2.0 * PI * f_hz / fs;
        let x: Vec<f32> = (0..n).map(|i| (w * i as f64).sin() as f32).collect();
        let y = filter.apply(&x);
        // Skip settling transient (~ 0.1 s).
        let skip = (fs * 0.1) as usize;
        let mean_sq: f64 = y
            .iter()
            .skip(skip)
            .map(|v| (*v as f64).powi(2))
            .sum::<f64>()
            / (n - skip) as f64;
        mean_sq.sqrt()
    }

    #[test]
    fn a_weighting_sine_gain_matches_magnitude() {
        let mut f = WeightingFilter::new(Weighting::A, FS).unwrap();
        let rms_out = sine_through(&mut f, 1000.0);
        let expected_rms = (1.0_f64 / 2.0).sqrt(); // unit-peak sine RMS = 1/√2
        assert!(
            (rms_out - expected_rms).abs() < 0.01,
            "A-weighted 1 kHz RMS {rms_out:.4} should equal input RMS {expected_rms:.4}"
        );
    }

    #[test]
    fn a_weighting_rolls_off_below_100hz() {
        // A-weighting at 100 Hz is about −19.1 dB per Annex E.
        let f = WeightingFilter::new(Weighting::A, FS).unwrap();
        let db = f.magnitude_db(100.0);
        assert!(
            db < -15.0 && db > -23.0,
            "A-weighting @ 100 Hz = {db:.2} dB, expected ~-19 dB"
        );
    }

    #[test]
    fn citation_shape() {
        let c = WeightingFilter::citation();
        assert!(c.standard.contains("IEC 61672-1"));
        assert!(!c.verified);
    }
}
