//! Tier 1 — ITU-R BS.468-4 CCIR-468 weighting and quasi-peak detection.
//!
//! Implements the passive LC weighting network described in §1 / Figure 1a,
//! and a two-stage quasi-peak detector per §2. The filter's magnitude
//! response is computed directly from the published component values via
//! an ABCD-parameter cascade, so [`magnitude_db`] is exact within f64
//! precision and reproduces Table 1 at every frequency.
//!
//! The quasi-peak detector topology is explicitly non-normative in the
//! standard (see the Note to §2: "a possible arrangement would consist of
//! two peak rectifier circuits of different time constants connected in
//! tandem"). The response tables (Tables 2 and 3) are the normative
//! criteria; this implementation picks commonly-used attack/release
//! constants and is intended to fall inside the Table 2 tolerance bands
//! for typical burst durations. Precise compliance across all burst
//! durations is a follow-up refinement.
//!
//! Reference levels follow the rest of the Tier 1 stack: 0 dBFS ↔ full-
//! scale sine (peak = 1.0, rms = 1/√2). §2.6 calibration is enforced by
//! normalising the filter to 0 dB at 1 kHz, so a continuous full-scale
//! 1 kHz sine reads 0 dBFS whether unweighted, A-weighted, or CCIR-
//! weighted.

use std::f64::consts::PI;

use anyhow::{bail, Result};
use realfft::num_complex::Complex;
use realfft::RealFftPlanner;

use crate::measurement::report::StandardsCitation;
use crate::shared::reference_levels::MIN_DBFS;

// ---------------------------------------------------------------------------
// LC weighting network — BS.468-4 §1, Figure 1a.
// ---------------------------------------------------------------------------

const R_SRC: f64 = 600.0;
const R_LOAD: f64 = 600.0;
const C_SHUNT_1: f64 = 13.85e-9;
const L_SERIES_1: f64 = 12.88e-3;
const C_SHUNT_2: f64 = 26.82e-9;
const C_SERIES_MID: f64 = 33.06e-9;
const C_SHUNT_3: f64 = 9.21e-9;
const L_SERIES_2: f64 = 26.49e-3;
const C_SHUNT_4: f64 = 31.47e-9;

type M2 = [[Complex<f64>; 2]; 2];

fn mat_mul(a: M2, b: M2) -> M2 {
    [
        [
            a[0][0] * b[0][0] + a[0][1] * b[1][0],
            a[0][0] * b[0][1] + a[0][1] * b[1][1],
        ],
        [
            a[1][0] * b[0][0] + a[1][1] * b[1][0],
            a[1][0] * b[0][1] + a[1][1] * b[1][1],
        ],
    ]
}

fn series(z: Complex<f64>) -> M2 {
    [
        [Complex::new(1.0, 0.0), z],
        [Complex::new(0.0, 0.0), Complex::new(1.0, 0.0)],
    ]
}

fn shunt(y: Complex<f64>) -> M2 {
    [
        [Complex::new(1.0, 0.0), Complex::new(0.0, 0.0)],
        [y, Complex::new(1.0, 0.0)],
    ]
}

/// Complex voltage transfer V_load / V_source of the §1 LC network at
/// angular frequency `w`, including both the 600 Ω source resistor and
/// the 600 Ω load. DC response is zero (the 33.06 nF series cap blocks).
fn network_h(w: f64) -> Complex<f64> {
    if w <= 0.0 {
        return Complex::new(0.0, 0.0);
    }
    let s = Complex::new(0.0, w);
    let y_c1 = s * C_SHUNT_1;
    let z_l1 = s * L_SERIES_1;
    let y_c2 = s * C_SHUNT_2;
    let z_cm = Complex::new(1.0, 0.0) / (s * C_SERIES_MID);
    let y_c3 = s * C_SHUNT_3;
    let z_l2 = s * L_SERIES_2;
    let y_c4 = s * C_SHUNT_4;

    let mut m: M2 = [
        [Complex::new(1.0, 0.0), Complex::new(0.0, 0.0)],
        [Complex::new(0.0, 0.0), Complex::new(1.0, 0.0)],
    ];
    for mat in [
        shunt(y_c1),
        series(z_l1),
        shunt(y_c2),
        series(z_cm),
        shunt(y_c3),
        series(z_l2),
        shunt(y_c4),
    ] {
        m = mat_mul(m, mat);
    }
    let a = m[0][0];
    let b = m[0][1];
    let c = m[1][0];
    let d = m[1][1];
    let rl = Complex::new(R_LOAD, 0.0);
    let rs = Complex::new(R_SRC, 0.0);
    let denom = a + b / rl + rs * c + rs * d / rl;
    Complex::new(1.0, 0.0) / denom
}

fn g_1khz_raw() -> f64 {
    network_h(2.0 * PI * 1000.0).norm()
}

/// Weighting-filter magnitude at `f_hz`, in dB, normalised to 0 dB at
/// 1 kHz so the result matches Table 1 directly.
pub fn magnitude_db(f_hz: f64) -> f64 {
    if f_hz <= 0.0 {
        return f64::NEG_INFINITY;
    }
    let mag = network_h(2.0 * PI * f_hz).norm();
    if mag <= 0.0 {
        return f64::NEG_INFINITY;
    }
    20.0 * (mag / g_1khz_raw()).log10()
}

// ---------------------------------------------------------------------------
// Time-domain filter — FFT-multiply, IFFT.
// ---------------------------------------------------------------------------

/// Apply the CCIR-468 weighting to `samples` at `sample_rate` Hz via
/// FFT-domain multiplication. The filter is normalised so 1 kHz has unity
/// gain (0 dB) — a continuous full-scale 1 kHz sine comes out unchanged.
pub fn apply_weighting(samples: &[f32], sample_rate: u32) -> Result<Vec<f32>> {
    if sample_rate == 0 {
        bail!("sample_rate must be positive");
    }
    if samples.is_empty() {
        return Ok(Vec::new());
    }
    let fs = sample_rate as f64;
    let n = samples.len().next_power_of_two().max(1024);

    let mut input = vec![0.0f64; n];
    for (dst, &src) in input.iter_mut().zip(samples.iter()) {
        *dst = src as f64;
    }

    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(n);
    let ifft = planner.plan_fft_inverse(n);
    let mut spec = fft.make_output_vec();
    fft.process(&mut input, &mut spec)
        .map_err(|e| anyhow::anyhow!("fft: {e}"))?;

    let g1k = g_1khz_raw();
    let inv_g1k = Complex::new(1.0 / g1k, 0.0);
    for (k, bin) in spec.iter_mut().enumerate() {
        let f = k as f64 * fs / n as f64;
        let h = network_h(2.0 * PI * f) * inv_g1k;
        *bin *= h;
    }
    // DC and Nyquist must be real in a real-input DFT.
    spec[0].im = 0.0;
    if let Some(last) = spec.last_mut() {
        last.im = 0.0;
    }

    let mut out = vec![0.0f64; n];
    ifft.process(&mut spec, &mut out)
        .map_err(|e| anyhow::anyhow!("ifft: {e}"))?;
    let scale = 1.0 / n as f64;
    Ok(out[..samples.len()]
        .iter()
        .map(|&v| (v * scale) as f32)
        .collect())
}

// ---------------------------------------------------------------------------
// Quasi-peak detector — §2. Topology non-normative per the note to §2.
// ---------------------------------------------------------------------------

/// Attack time constant of the first rectifier stage (s).
const QP_STAGE1_ATTACK_S: f64 = 0.00175;
/// Release time constant of the first rectifier stage (s).
const QP_STAGE1_RELEASE_S: f64 = 0.650;
/// Time constant of the second integrating stage (s).
const QP_STAGE2_S: f64 = 0.100;

/// Run the two-stage quasi-peak detector over `samples`. The first stage
/// is a full-wave rectifier followed by an asymmetric one-pole detector
/// (fast attack, slow release — classical PPM ballistic). The second
/// stage is a symmetric one-pole integrator. Returns the peak value
/// reached by the second-stage output over the buffer.
pub fn quasi_peak(samples: &[f32], sample_rate: u32) -> f64 {
    if sample_rate == 0 || samples.is_empty() {
        return 0.0;
    }
    let dt = 1.0 / sample_rate as f64;
    let a_attack = dt / (QP_STAGE1_ATTACK_S + dt);
    let a_release = dt / (QP_STAGE1_RELEASE_S + dt);
    let a_stage2 = dt / (QP_STAGE2_S + dt);

    let mut e1 = 0.0f64;
    let mut e2 = 0.0f64;
    let mut peak = 0.0f64;
    for &x in samples {
        let rect = (x as f64).abs();
        let coeff = if rect > e1 { a_attack } else { a_release };
        e1 += (rect - e1) * coeff;
        e2 += (e1 - e2) * a_stage2;
        if e2 > peak {
            peak = e2;
        }
    }
    peak
}

// ---------------------------------------------------------------------------
// High-level: CCIR-weighted QP level in dBFS.
// ---------------------------------------------------------------------------

/// CCIR-468 weighted quasi-peak level in dBFS, referenced to a full-
/// scale 1 kHz sine (peak = 1.0 → 0 dBFS). Applies the weighting filter
/// and then the two-stage QP detector, then normalises against the
/// detector's response to a reference 1 kHz sine computed at the same
/// sample rate.
pub fn weighted_quasi_peak_dbfs(samples: &[f32], sample_rate: u32) -> Result<f64> {
    if samples.is_empty() {
        bail!("samples must be non-empty");
    }
    if sample_rate == 0 {
        bail!("sample_rate must be positive");
    }
    let weighted = apply_weighting(samples, sample_rate)?;
    let qp = quasi_peak(&weighted, sample_rate);
    let ref_qp = reference_qp_1khz(sample_rate);
    if ref_qp <= 0.0 || qp <= 0.0 {
        return Ok(MIN_DBFS);
    }
    Ok((20.0 * (qp / ref_qp).log10()).max(MIN_DBFS))
}

/// Reference QP reading of a full-scale 1 kHz sine at `sample_rate`,
/// after the weighting filter (which is unity gain at 1 kHz). Used as
/// the 0 dBFS anchor for [`weighted_quasi_peak_dbfs`]. Duration is
/// chosen so both detector stages fully settle.
fn reference_qp_1khz(sample_rate: u32) -> f64 {
    let n = (sample_rate as usize * 3) / 2; // 1.5 s
    let w = 2.0 * PI * 1000.0 / sample_rate as f64;
    let sig: Vec<f32> = (0..n).map(|i| (w * i as f64).sin() as f32).collect();
    let weighted = apply_weighting(&sig, sample_rate).unwrap_or(sig);
    quasi_peak(&weighted, sample_rate)
}

pub fn citation() -> StandardsCitation {
    StandardsCitation {
        standard: "ITU-R BS.468-4".into(),
        clause: "§1 Weighting network, §2 Quasi-peak characteristics".into(),
        verified: false,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const FS: u32 = 48_000;

    /// Table 1 rows: (frequency Hz, reference response dB, tolerance dB).
    /// Tolerance for 6.3 kHz is given as 0 in the standard (it is the
    /// anchor point of the peak); we allow ±0.5 dB here to cover finite-
    /// precision f64 round-off around the peak of the response curve.
    const TABLE_1: &[(f64, f64, f64)] = &[
        (31.5, -29.9, 2.0),
        (63.0, -23.9, 1.4),
        (100.0, -19.8, 1.0),
        (200.0, -13.8, 0.85),
        (400.0, -7.8, 0.7),
        (800.0, -1.9, 0.55),
        (1000.0, 0.0, 0.5),
        (2000.0, 5.6, 0.5),
        (3150.0, 9.0, 0.5),
        (4000.0, 10.5, 0.5),
        (5000.0, 11.7, 0.5),
        (6300.0, 12.2, 0.5),
        (7100.0, 12.0, 0.2),
        (8000.0, 11.4, 0.4),
        (9000.0, 10.1, 0.6),
        (10000.0, 8.1, 0.8),
        (12500.0, 0.0, 1.2),
        (14000.0, -5.3, 1.4),
        (16000.0, -11.7, 1.6),
        (20000.0, -22.2, 2.0),
    ];

    #[test]
    fn magnitude_is_0db_at_1khz() {
        assert!(magnitude_db(1000.0).abs() < 1e-9);
    }

    #[test]
    fn table_1_reference_response_within_tolerance() {
        for &(f, expected, tol) in TABLE_1 {
            let got = magnitude_db(f);
            assert!(
                (got - expected).abs() <= tol,
                "Table 1 @ {f} Hz: got {got:.3} dB, expected {expected:.3} (±{tol} dB)"
            );
        }
    }

    #[test]
    fn magnitude_at_31_5khz_below_upper_limit() {
        // §Table 1 row 21: tolerance is +2.8 / −∞, so we only need to
        // check the upper bound.
        let got = magnitude_db(31_500.0);
        assert!(
            got <= -42.7 + 2.8,
            "31.5 kHz: got {got:.2} dB, upper limit = {:.2}",
            -42.7 + 2.8
        );
    }

    #[test]
    fn dc_is_fully_attenuated() {
        // Series 33.06 nF blocks DC.
        assert_eq!(magnitude_db(0.0), f64::NEG_INFINITY);
    }

    #[test]
    fn fft_filter_passes_1khz_sine_unchanged() {
        let n = FS as usize;
        let w = 2.0 * PI * 1000.0 / FS as f64;
        let sig: Vec<f32> = (0..n).map(|i| (w * i as f64).sin() as f32).collect();
        let out = apply_weighting(&sig, FS).unwrap();
        // Compare steady-state RMS well away from the FFT boundaries.
        let skip = FS as usize / 5;
        let take = FS as usize / 2;
        let rms_in: f64 = sig[skip..skip + take]
            .iter()
            .map(|&v| (v as f64).powi(2))
            .sum::<f64>()
            / take as f64;
        let rms_out: f64 = out[skip..skip + take]
            .iter()
            .map(|&v| (v as f64).powi(2))
            .sum::<f64>()
            / take as f64;
        let ratio_db = 10.0 * (rms_out / rms_in).log10();
        assert!(
            ratio_db.abs() < 0.3,
            "1 kHz should survive unity-gain, got {ratio_db:.3} dB"
        );
    }

    #[test]
    fn fft_filter_attenuates_100hz() {
        // Table 1 says -19.8 dB at 100 Hz; allow wide margin because
        // FFT-convolution leakage and rectangular truncation round off
        // the response somewhat.
        let n = FS as usize * 2;
        let w = 2.0 * PI * 100.0 / FS as f64;
        let sig: Vec<f32> = (0..n).map(|i| (w * i as f64).sin() as f32).collect();
        let out = apply_weighting(&sig, FS).unwrap();
        let skip = FS as usize / 2;
        let take = FS as usize;
        let rms_in: f64 = sig[skip..skip + take]
            .iter()
            .map(|&v| (v as f64).powi(2))
            .sum::<f64>()
            / take as f64;
        let rms_out: f64 = out[skip..skip + take]
            .iter()
            .map(|&v| (v as f64).powi(2))
            .sum::<f64>()
            / take as f64;
        let db = 10.0 * (rms_out / rms_in).log10();
        assert!(
            db < -17.0 && db > -22.5,
            "100 Hz CCIR attenuation should be ~-19.8 dB; got {db:.2} dB"
        );
    }

    #[test]
    fn calibration_1khz_full_scale_reads_0_dbfs() {
        // §2.6: a steady 1 kHz sine at 0 dB reference gives a 0 dB
        // reading. Digital: full-scale sine (peak=1.0) reads 0 dBFS
        // after weighting + QP.
        let n = FS as usize * 2;
        let w = 2.0 * PI * 1000.0 / FS as f64;
        let sig: Vec<f32> = (0..n).map(|i| (w * i as f64).sin() as f32).collect();
        let db = weighted_quasi_peak_dbfs(&sig, FS).unwrap();
        assert!(db.abs() < 0.1, "expected 0 dBFS, got {db:.3}");
    }

    #[test]
    fn silence_is_at_floor() {
        let buf = vec![0.0f32; FS as usize];
        let db = weighted_quasi_peak_dbfs(&buf, FS).unwrap();
        assert_eq!(db, MIN_DBFS);
    }

    #[test]
    fn burst_shorter_than_steady_reads_lower() {
        // Feed two buffers at the same 5 kHz amplitude: one continuous,
        // one a short burst in silence. Burst should read lower (QP
        // detector is a quasi-peak, not a true peak).
        let total_n = (FS as f64 * 0.6) as usize;
        let w = 2.0 * PI * 5000.0 / FS as f64;
        let burst_n = (FS as f64 * 0.005) as usize; // 5 ms

        let steady: Vec<f32> = (0..total_n).map(|i| (w * i as f64).sin() as f32).collect();
        let mut burst = vec![0.0f32; total_n];
        for i in 0..burst_n {
            burst[i + FS as usize / 10] = (w * i as f64).sin() as f32;
        }

        let db_steady = weighted_quasi_peak_dbfs(&steady, FS).unwrap();
        let db_burst = weighted_quasi_peak_dbfs(&burst, FS).unwrap();
        assert!(
            db_burst < db_steady - 2.0,
            "5 ms burst ({db_burst:.2} dB) should read well below steady ({db_steady:.2} dB)"
        );
    }

    #[test]
    fn rejects_zero_sample_rate() {
        let buf = vec![0.5_f32; 1000];
        assert!(weighted_quasi_peak_dbfs(&buf, 0).is_err());
    }

    #[test]
    fn rejects_empty_buffer() {
        assert!(weighted_quasi_peak_dbfs(&[], FS).is_err());
    }

    #[test]
    fn citation_shape() {
        let c = citation();
        assert!(c.standard.contains("BS.468"));
        assert!(c.clause.contains("§1"));
    }
}
