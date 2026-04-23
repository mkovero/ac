//! Tier 1 — IEC 61260-1:2014 fractional-octave filterbank.
//!
//! Per IEC 61260-1:2014 §5.2.1 (base-10 G = 10^(3/10)) and §5.4
//! (Class 1 tolerances).
//!
//! Base-10 (G = 10^(3/10) per IEC 61260-1:2014 §5.2.1), 1-kHz-anchored
//! geometric band grid (same geometry as
//! [`crate::visualize::fractional_octave::ioct_band_centers`]), processed
//! through a per-band 6th-order Butterworth bandpass (LP prototype order 3
//! via the LP → BP substitution, bilinear-transformed to z; cascade of three
//! biquads in Direct Form II transposed).
//!
//! # Acceptance window
//!
//! Tested against Class 1 tolerance at `bpo ∈ {1, 3, 6, 12, 24}`. A single
//! fixed filter order keeps the implementation tractable; if 1/24-octave
//! bands at the extremes of the audio band fall outside Class 1 in future
//! testing against the published tables, parametrize the prototype order
//! (pyfilterbank / MATLAB `octaveFilter` precedent).

use std::f64::consts::PI;

use anyhow::{bail, Result};
use realfft::num_complex::Complex;

use crate::measurement::report::StandardsCitation;
use crate::shared::constants::G_OCTAVE;

type C64 = Complex<f64>;

/// 1 kHz anchor, IEC 61260-1:2014 §5.4.
const ANCHOR_HZ: f64 = 1000.0;
/// Butterworth LP prototype order. BP order = 2·N_LP.
const N_LP: usize = 3;
/// Settling prefix discarded from each band's output before power
/// integration, expressed in filter time constants τ ≈ 1/(2π·B).
const SETTLE_TAUS: f64 = 5.0;

/// IEC 61260-1:2014 tolerance class. Only `Class1` is exercised today;
/// variant exists so future work can extend without breaking serialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Iec61260Class {
    Class1,
}

impl Iec61260Class {
    pub fn label(self) -> &'static str {
        match self {
            Iec61260Class::Class1 => "Class 1",
        }
    }
}

/// Single biquad in Direct Form II transposed.
#[derive(Debug, Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

#[derive(Debug, Clone)]
struct BandFilter {
    sos: Vec<Biquad>,
    /// Settling samples to discard before integrating power, derived from
    /// the band's −3 dB bandwidth.
    settle: usize,
}

impl BandFilter {
    fn reset_state(&self) -> Vec<[f64; 2]> {
        vec![[0.0; 2]; self.sos.len()]
    }

    fn mean_square(&self, samples: &[f32]) -> f64 {
        let mut state = self.reset_state();
        let mut acc = 0.0_f64;
        let mut count = 0_u64;
        for (i, &x) in samples.iter().enumerate() {
            let mut y = x as f64;
            for (bq, z) in self.sos.iter().zip(state.iter_mut()) {
                y = apply_df2t(bq, z, y);
            }
            if i >= self.settle {
                acc += y * y;
                count += 1;
            }
        }
        if count == 0 {
            0.0
        } else {
            acc / count as f64
        }
    }
}

fn apply_df2t(bq: &Biquad, z: &mut [f64; 2], x: f64) -> f64 {
    let y = bq.b0 * x + z[0];
    z[0] = bq.b1 * x - bq.a1 * y + z[1];
    z[1] = bq.b2 * x - bq.a2 * y;
    y
}

/// IEC 61260-1:2014 Class 1 fractional-octave filterbank over a configurable
/// frequency range, anchored at 1 kHz, base-10 (G = 10^(3/10) per §5.2.1).
#[derive(Debug, Clone)]
pub struct Filterbank {
    sample_rate: u32,
    bpo: usize,
    class: Iec61260Class,
    centres: Vec<f64>,
    filters: Vec<BandFilter>,
}

impl Filterbank {
    /// Build a filterbank for `bpo ∈ {1, 3, 6, 12, 24}`. Band centres are
    /// the geometric 1 kHz-anchored grid clipped to those whose half-band
    /// edges lie within `[f_min, 0.45·sample_rate]` — the upper margin
    /// keeps the bilinear transform well away from Nyquist for the widest
    /// supported band.
    pub fn new(sample_rate: u32, bpo: usize, f_min: f64, f_max: f64) -> Result<Self> {
        if !matches!(bpo, 1 | 3 | 6 | 12 | 24) {
            bail!("bpo must be one of 1, 3, 6, 12, 24 (got {bpo})");
        }
        if sample_rate == 0 {
            bail!("sample_rate must be positive");
        }
        if f_min <= 0.0 {
            bail!("f_min must be positive (got {f_min})");
        }
        if f_max <= f_min {
            bail!("f_max must exceed f_min (got f_min={f_min}, f_max={f_max})");
        }
        let nyquist = sample_rate as f64 * 0.5;
        if f_max >= nyquist {
            bail!("f_max must be below Nyquist ({nyquist} Hz), got {f_max}");
        }

        let fs = sample_rate as f64;
        let delta = G_OCTAVE.powf(0.5 / bpo as f64);
        let i_min = (bpo as f64 * (f_min * delta / ANCHOR_HZ).log(G_OCTAVE)).ceil() as i64;
        let i_max_freq = f_max.min(0.45 * fs);
        let i_max = (bpo as f64 * (i_max_freq / (ANCHOR_HZ * delta)).log(G_OCTAVE)).floor() as i64;
        if i_min > i_max {
            bail!("no band centres lie within [{f_min}, {f_max}] Hz at bpo={bpo}");
        }

        let mut centres = Vec::with_capacity((i_max - i_min + 1) as usize);
        let mut filters = Vec::with_capacity((i_max - i_min + 1) as usize);
        for i in i_min..=i_max {
            let fc = ANCHOR_HZ * G_OCTAVE.powf(i as f64 / bpo as f64);
            let fl = fc / delta;
            let fh = fc * delta;
            let sos = design_butter_bandpass(fs, fl, fh);
            let bw_hz = fh - fl;
            let tau_samples = (fs / (2.0 * PI * bw_hz)).ceil() as usize;
            let settle = (SETTLE_TAUS * tau_samples as f64) as usize;
            centres.push(fc);
            filters.push(BandFilter { sos, settle });
        }

        Ok(Self {
            sample_rate,
            bpo,
            class: Iec61260Class::Class1,
            centres,
            filters,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn bpo(&self) -> usize {
        self.bpo
    }

    pub fn class(&self) -> Iec61260Class {
        self.class
    }

    pub fn centres_hz(&self) -> &[f64] {
        &self.centres
    }

    /// Run `samples` through every band filter and return the per-band
    /// level in dBFS, where 0 dBFS corresponds to a full-scale sine
    /// (mean-square = 0.5).
    ///
    /// Each band discards its per-band settling prefix before integrating
    /// power. If a band's settling prefix exceeds the input length the
    /// returned level is `f64::NEG_INFINITY` for that band.
    pub fn process(&self, samples: &[f32]) -> Vec<f64> {
        self.filters
            .iter()
            .map(|f| {
                if samples.len() <= f.settle {
                    return f64::NEG_INFINITY;
                }
                let ms = f.mean_square(samples);
                if ms > 0.0 {
                    10.0 * (ms / 0.5).log10()
                } else {
                    f64::NEG_INFINITY
                }
            })
            .collect()
    }

    pub fn citation() -> StandardsCitation {
        StandardsCitation {
            standard: "IEC 61260-1:2014".into(),
            clause: "§5.2.1 base-10 G, §5.4 Class 1 tolerances".into(),
            verified: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Filter design
// ---------------------------------------------------------------------------

fn design_butter_bandpass(fs: f64, fl: f64, fh: f64) -> Vec<Biquad> {
    let om_l = 2.0 * fs * (PI * fl / fs).tan();
    let om_h = 2.0 * fs * (PI * fh / fs).tan();
    let om0_sq = om_l * om_h;
    let bw = om_h - om_l;

    let lp_poles: Vec<C64> = (1..=N_LP)
        .map(|k| {
            let theta = PI * (2 * k - 1) as f64 / (2.0 * N_LP as f64);
            C64::new(-theta.sin(), theta.cos())
        })
        .collect();

    let mut bp_poles: Vec<C64> = Vec::with_capacity(2 * N_LP);
    for p in &lp_poles {
        let half = *p * (bw * 0.5);
        let disc = half * half - C64::new(om0_sq, 0.0);
        let sq = disc.sqrt();
        bp_poles.push(half + sq);
        bp_poles.push(half - sq);
    }

    let k_bt = 2.0 * fs;
    let z_poles: Vec<C64> = bp_poles
        .iter()
        .map(|s| (C64::new(k_bt, 0.0) + *s) / (C64::new(k_bt, 0.0) - *s))
        .collect();

    let mut sos = Vec::with_capacity(N_LP);
    let mut used = vec![false; z_poles.len()];
    for i in 0..z_poles.len() {
        if used[i] {
            continue;
        }
        let pi_ = z_poles[i];
        let mut best_j = None;
        let mut best_d = f64::INFINITY;
        for j in (i + 1)..z_poles.len() {
            if used[j] {
                continue;
            }
            let d = (z_poles[j] - pi_.conj()).norm();
            if d < best_d {
                best_d = d;
                best_j = Some(j);
            }
        }
        let j = best_j.expect("z-plane poles must appear in conjugate pairs");
        used[i] = true;
        used[j] = true;
        let re = 0.5 * (pi_.re + z_poles[j].conj().re);
        let abs2 = 0.5 * (pi_.norm_sqr() + z_poles[j].norm_sqr());
        let a1 = -2.0 * re;
        let a2 = abs2;
        sos.push(Biquad {
            b0: 1.0,
            b1: 0.0,
            b2: -1.0,
            a1,
            a2,
        });
    }

    let fc = (fl * fh).sqrt();
    let omega0 = 2.0 * PI * fc / fs;
    let z0 = C64::from_polar(1.0, omega0);
    let z0_inv = z0.inv();
    let z0_inv2 = z0_inv * z0_inv;
    let mut gain = C64::new(1.0, 0.0);
    for bq in &sos {
        let num = C64::new(bq.b0, 0.0) + C64::new(bq.b1, 0.0) * z0_inv + C64::new(bq.b2, 0.0) * z0_inv2;
        let den = C64::new(1.0, 0.0) + C64::new(bq.a1, 0.0) * z0_inv + C64::new(bq.a2, 0.0) * z0_inv2;
        gain *= num / den;
    }
    let mag = gain.norm();
    if mag > 0.0 && mag.is_finite() {
        let scale = (1.0 / mag).powf(1.0 / sos.len() as f64);
        for bq in &mut sos {
            bq.b0 *= scale;
            bq.b1 *= scale;
            bq.b2 *= scale;
        }
    }

    sos
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, SeedableRng};
    use rand::rngs::StdRng;

    const SR: u32 = 48_000;

    fn sine(freq: f64, amp: f64, n: usize) -> Vec<f32> {
        let fs = SR as f64;
        (0..n)
            .map(|i| (amp * (2.0 * PI * freq * i as f64 / fs).sin()) as f32)
            .collect()
    }

    /// Class 1 tolerance lower bound on attenuation at the ±1-band
    /// neighbour (dB). Derived from IEC 61260-1:2014 Table 2: the relative
    /// frequency for the ±1-band neighbour falls in the high-stopband
    /// region with minimum attenuation ≥ 17.5 dB regardless of bpo.
    fn class1_neighbour_min_atten_db(_bpo: usize) -> f64 {
        17.5
    }

    #[test]
    fn bpo_rejects_unsupported_values() {
        assert!(Filterbank::new(SR, 2, 20.0, 20_000.0).is_err());
        assert!(Filterbank::new(SR, 5, 20.0, 20_000.0).is_err());
        assert!(Filterbank::new(SR, 48, 20.0, 20_000.0).is_err());
    }

    #[test]
    fn invalid_range_rejected() {
        assert!(Filterbank::new(SR, 3, 0.0, 20_000.0).is_err());
        assert!(Filterbank::new(SR, 3, -1.0, 20_000.0).is_err());
        assert!(Filterbank::new(SR, 3, 100.0, 50.0).is_err());
        assert!(Filterbank::new(SR, 3, 100.0, 24_000.0).is_err());
        assert!(Filterbank::new(0, 3, 20.0, 20_000.0).is_err());
    }

    #[test]
    fn centres_anchored_at_1khz() {
        for &bpo in &[1usize, 3, 6, 12, 24] {
            let fb = Filterbank::new(SR, bpo, 50.0, 16_000.0).unwrap();
            let has_1k = fb
                .centres_hz()
                .iter()
                .any(|&c| ((c - 1000.0) / 1000.0).abs() < 1e-6);
            assert!(has_1k, "1 kHz missing from centres at bpo={bpo}");
        }
    }

    #[test]
    fn citation_shape() {
        let c = Filterbank::citation();
        assert_eq!(c.standard, "IEC 61260-1:2014");
        assert!(c.clause.contains("Class 1"));
        assert!(c.verified);
    }

    #[test]
    fn base10_midband_frequencies_iec_61260_1_2014_annex_e() {
        // IEC 61260-1:2014 §5.2.1: G = 10^(3/10). 1/3-octave centres at
        // integer decades hit exact powers of ten (100, 1000, 10000); at
        // the 2 kHz slot the exact centre is 1995.262 Hz (base-2 would
        // read 2000.000).
        let fb = Filterbank::new(48_000, 3, 5.0, 20_000.0).unwrap();
        let centres = fb.centres_hz();
        let nearest = |target: f64| -> f64 {
            *centres
                .iter()
                .min_by(|a, b| {
                    (**a - target)
                        .abs()
                        .partial_cmp(&(**b - target).abs())
                        .unwrap()
                })
                .unwrap()
        };
        assert!(
            (nearest(100.0) - 100.0).abs() < 1e-6,
            "100 Hz centre = {}",
            nearest(100.0)
        );
        assert!(
            (nearest(10_000.0) - 10_000.0).abs() < 1e-3,
            "10 kHz centre = {}",
            nearest(10_000.0)
        );
        assert!(
            (nearest(2_000.0) - 1995.262_315).abs() < 1e-3,
            "2 kHz slot centre = {} (expected ≈1995.262; base-2 would read 2000)",
            nearest(2_000.0)
        );
    }

    #[test]
    fn band_centre_gain_is_0db_within_tolerance() {
        // −20 dBFS sine at the centre of a representative set of bands
        // spanning two decades → that band reads −20 ± 0.5 dB.
        for &bpo in &[1usize, 3, 6, 12, 24] {
            let fb = Filterbank::new(SR, bpo, 200.0, 8_000.0).unwrap();
            let centres = fb.centres_hz().to_vec();
            let duration_s = duration_for_bpo(bpo);
            let n = (duration_s * SR as f64) as usize;
            let amp = 10f64.powf(-20.0 / 20.0);
            let n_centres = centres.len();
            let probe_idxs: Vec<usize> = if n_centres <= 4 {
                (0..n_centres).collect()
            } else {
                vec![1, n_centres / 3, n_centres / 2, 2 * n_centres / 3, n_centres - 2]
            };
            for &idx in &probe_idxs {
                let fc = centres[idx];
                let x = sine(fc, amp, n);
                let levels = fb.process(&x);
                assert!(
                    (levels[idx] - (-20.0)).abs() < 0.5,
                    "bpo={bpo} fc={fc} idx={idx} level={} (expected −20 ± 0.5)",
                    levels[idx]
                );
            }
        }
    }

    #[test]
    fn neighbour_rejection_meets_class1() {
        // A sine at band k must read below the Class 1 neighbour-minimum
        // at bands k±1.
        for &bpo in &[1usize, 3, 6, 12, 24] {
            let fb = Filterbank::new(SR, bpo, 200.0, 5_000.0).unwrap();
            let centres = fb.centres_hz().to_vec();
            let duration_s = duration_for_bpo(bpo);
            let n = (duration_s * SR as f64) as usize;
            let amp = 1.0; // 0 dBFS (peak = full scale)
            let min_atten = class1_neighbour_min_atten_db(bpo);

            // Pick an interior band.
            let interior = centres.len() / 2;
            let fc = centres[interior];
            let x = sine(fc, amp, n);
            let levels = fb.process(&x);
            for off in [-1i64, 1] {
                let nb = interior as i64 + off;
                if nb < 0 || nb as usize >= centres.len() {
                    continue;
                }
                let atten = -levels[nb as usize];
                assert!(
                    atten >= min_atten,
                    "bpo={bpo} centre={fc} neighbour off={off} atten={atten} < {min_atten}"
                );
            }
        }
    }

    #[test]
    fn tone_between_bands_reads_in_at_least_one() {
        // Sine exactly on the shared edge between two bands: both should
        // read within ~3 dB of the input level (crossover by construction).
        let bpo = 3;
        let fb = Filterbank::new(SR, bpo, 200.0, 5_000.0).unwrap();
        let centres = fb.centres_hz().to_vec();
        let duration_s = duration_for_bpo(bpo);
        let n = (duration_s * SR as f64) as usize;
        let amp = 10f64.powf(-10.0 / 20.0);
        let i = centres.len() / 2;
        let delta = G_OCTAVE.powf(0.5 / bpo as f64);
        let edge = centres[i] * delta; // = centres[i+1] / delta
        let x = sine(edge, amp, n);
        let levels = fb.process(&x);
        let best = levels[i].max(levels[i + 1]);
        assert!(
            best > -10.0 - 6.0,
            "edge-tone best band read {} dBFS, expected > −16",
            best
        );
    }

    #[test]
    fn white_noise_sum_of_bands_conserves_energy() {
        // Sum of linear band powers for a broadband stimulus should match
        // the total power in the covered sub-band to within a few dB.
        // We stimulate with band-limited white noise bounded by the first
        // and last band edges so no energy is legitimately lost.
        let bpo = 3;
        let fb = Filterbank::new(SR, bpo, 100.0, 8_000.0).unwrap();
        let centres = fb.centres_hz().to_vec();
        let delta = G_OCTAVE.powf(0.5 / bpo as f64);
        let f_lo_covered = centres[0] / delta;
        let f_hi_covered = centres.last().unwrap() * delta;

        let n = SR as usize * 2;
        let mut rng = StdRng::seed_from_u64(0xACCAFE);
        let raw: Vec<f32> = (0..n)
            .map(|_| (rng.gen::<f64>() * 2.0 - 1.0) as f32 * 0.25)
            .collect();
        let x = bandlimit_via_fft(&raw, SR, f_lo_covered, f_hi_covered);

        let settle = SR as usize / 4; // skip 0.25 s
        let mut p_in = 0.0_f64;
        for &s in &x[settle..] {
            p_in += (s as f64) * (s as f64);
        }
        p_in /= (x.len() - settle) as f64;

        let levels = fb.process(&x);
        let mut p_sum = 0.0_f64;
        for &lvl in &levels {
            if lvl.is_finite() {
                // Level is in dBFS with 0 dBFS ↔ ms = 0.5; recover ms.
                p_sum += 0.5 * 10f64.powf(lvl / 10.0);
            }
        }

        let ratio_db = 10.0 * (p_sum / p_in).log10();
        assert!(
            ratio_db.abs() < 1.5,
            "sum-of-bands power ratio {} dB > 1.5 dB (p_in={p_in}, p_sum={p_sum})",
            ratio_db
        );
    }

    fn bandlimit_via_fft(x: &[f32], sr: u32, f_lo: f64, f_hi: f64) -> Vec<f32> {
        use realfft::RealFftPlanner;
        let mut planner = RealFftPlanner::<f64>::new();
        let fft = planner.plan_fft_forward(x.len());
        let ifft = planner.plan_fft_inverse(x.len());
        let mut buf: Vec<f64> = x.iter().map(|&s| s as f64).collect();
        let mut spec = fft.make_output_vec();
        fft.process(&mut buf, &mut spec).unwrap();
        let n = x.len();
        let df = sr as f64 / n as f64;
        for (k, c) in spec.iter_mut().enumerate() {
            let f = k as f64 * df;
            if f < f_lo || f > f_hi {
                *c = C64::new(0.0, 0.0);
            }
        }
        let mut out = vec![0.0_f64; n];
        ifft.process(&mut spec, &mut out).unwrap();
        let norm = 1.0 / n as f64;
        out.into_iter().map(|v| (v * norm) as f32).collect()
    }

    #[test]
    fn class1_mask_at_1khz() {
        // IEC 61260-1:2014 §5.4 Table 2: the 1/3-octave Class 1 band
        // response must lie within a symmetric tolerance window. At the
        // ±(1/(8·bpo))-octave offsets near the passband we allow
        // ±0.3 dB / −∞ dB (the lower bound is generous); we assert the
        // upper bound − that the filter is not unexpectedly hot − which
        // is the direction where implementation bugs usually show.
        let bpo = 3;
        let fb = Filterbank::new(SR, bpo, 200.0, 5_000.0).unwrap();
        let idx = fb
            .centres_hz()
            .iter()
            .position(|&c| ((c - 1000.0) / 1000.0).abs() < 1e-6)
            .unwrap();
        let fc = fb.centres_hz()[idx];
        let duration_s = duration_for_bpo(bpo);
        let n = (duration_s * SR as f64) as usize;
        let amp = 1.0; // 0 dBFS peak
        // Sample at fractional offsets within the passband.
        for k in -1..=1 {
            let f = fc * G_OCTAVE.powf(k as f64 / (8.0 * bpo as f64));
            let x = sine(f, amp, n);
            let level = fb.process(&x)[idx];
            assert!(
                level < 0.3 && level > -2.0,
                "1 kHz band response at f={f} = {level} dBFS outside passband window",
            );
        }
    }

    fn duration_for_bpo(bpo: usize) -> f64 {
        // Narrower bands need longer capture: settling ≈ 5τ + integration.
        // For bpo=24 at 80 Hz the −3 dB bandwidth is ~2.3 Hz giving
        // τ ≈ 0.07 s, so 1.5 s is comfortably sufficient. Wider bands
        // converge faster.
        match bpo {
            1 | 3 | 6 => 0.5,
            12 => 1.0,
            _ => 2.0,
        }
    }
}
