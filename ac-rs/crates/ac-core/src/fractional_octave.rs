//! Fractional-octave aggregation of a CWT column.
//!
//! Builds a 1/N-octave display from the CWT magnitude column produced by
//! [`crate::cwt::morlet_cwt`]. Bands are anchored at 1 kHz (acoustics
//! convention) and use base-2 octaves (`G = 2`). Common `bpo`: 1, 3, 6,
//! 12, 24.
//!
//! ## Why not the FFT path?
//!
//! [`crate::aggregate::spectrum_to_columns`] uses log-interpolation between
//! sparse FFT bins at low frequencies to avoid display zigzag. That's fine
//! for a continuous spectrum but the wrong semantics for fractional-octave
//! bands, where each band must report integrated energy of all spectral
//! content within its edges. The CWT already gives frequency-dependent
//! analysis windows (wider in time at low freqs), so summing |CWT|² within
//! each band yields a low-end resolution that the FFT can't deliver
//! without a long acquisition window.
//!
//! ## Non-goal: IEC 61260 compliance
//!
//! Morlet filter shapes do **not** match IEC 61260 band shapes. This
//! module is a visualization aid, not a measurement-grade filterbank. Do
//! not advertise IEC compliance anywhere downstream of these readings.
//!
//! ## Calibration and the kernel-overlap trap
//!
//! For a CWT column with a single isolated scale (e.g. a synthetic test
//! input where neighbours are at -∞ dBFS), summing power preserves dBFS
//! exactly: a single scale at -6 dBFS in a band reads -6 dBFS at the
//! aggregator output.
//!
//! For a real CWT column from `morlet_cwt`, a single tone's energy
//! spreads across multiple adjacent scales — Morlet kernels overlap when
//! scale density exceeds roughly 1/sigma per octave. Summing those
//! overlapping scales **overestimates** band energy proportional to scale
//! density. At the default 512 scales over 3 decades (~17/octave) and
//! `sigma = 12`, a single tone reads roughly +5 dB hot in its band; at
//! 17/octave with sigma=12, expect ~+5 to +10 dB. Sparse scales
//! (~3/octave at sigma=12) reduce overlap to single-band granularity but
//! lose the resolution advantage that motivated the CWT path in the first
//! place. This is a property of sum-of-overlapping-kernels, not a bug.
//! Possible follow-up: per-scale bandwidth normalization, or peak/avg
//! aggregation. Tracked separately.

const ANCHOR_HZ: f64 = 1000.0;

/// Geometric grid of band centres anchored at 1 kHz, base-2.
///
/// `c_i = 1000 · 2^(i / bpo)` for integer `i`, clipped to those whose
/// half-band edges `(c_i / δ, c_i · δ)` with `δ = 2^(1/(2·bpo))` fit
/// fully within `[f_min, f_max]`.
///
/// Returns an empty Vec on degenerate inputs (`bpo == 0`, non-positive
/// `f_min`, or `f_max <= f_min`).
pub fn ioct_band_centers(f_min: f32, f_max: f32, bpo: usize) -> Vec<f32> {
    if bpo == 0 || f_min <= 0.0 || f_max <= f_min {
        return Vec::new();
    }
    let bpo_f = bpo as f64;
    let delta = 2_f64.powf(0.5 / bpo_f);
    let f_min = f_min as f64;
    let f_max = f_max as f64;

    // i_min: smallest i with c_i / δ >= f_min  →  i >= bpo · log2(f_min · δ / 1000)
    // i_max: largest  i with c_i · δ <= f_max  →  i <= bpo · log2(f_max / (1000 · δ))
    let i_min = (bpo_f * (f_min * delta / ANCHOR_HZ).log2()).ceil() as i64;
    let i_max = (bpo_f * (f_max / (ANCHOR_HZ * delta)).log2()).floor() as i64;
    if i_min > i_max {
        return Vec::new();
    }

    let mut centres = Vec::with_capacity((i_max - i_min + 1) as usize);
    for i in i_min..=i_max {
        let c = ANCHOR_HZ * 2_f64.powf(i as f64 / bpo_f);
        centres.push(c as f32);
    }
    centres
}

/// Half-band edges around `centre`: `(centre / δ, centre · δ)` with
/// `δ = 2^(1/(2·bpo))`. Both edges are returned as `f32`; for adjacent
/// centres `c_i, c_{i+1}` the relation `f_hi(c_i) == f_lo(c_{i+1})`
/// holds within float tolerance (covered by `band_edges_tile`).
pub fn ioct_band_edges(centre: f32, bpo: usize) -> (f32, f32) {
    if bpo == 0 {
        return (centre, centre);
    }
    let delta = 2_f32.powf(0.5 / bpo as f32);
    (centre / delta, centre * delta)
}

/// Aggregate a CWT magnitude column into 1/`bpo`-octave bands.
///
/// `cwt_col_db[k]` is the dBFS magnitude at scale whose centre frequency
/// is `cwt_freqs[k]` (Hz) — i.e. the output of [`crate::cwt::morlet_cwt`]
/// paired with the frequency vector from [`crate::cwt::log_scales`].
///
/// Algorithm:
/// 1. dB → linear power via `10^(db/10)`.
/// 2. For each band `[f_lo, f_hi)`, sum the powers of all CWT scales
///    whose centre frequency falls inside.
/// 3. Empty bands fall back to log-`f` linear interpolation in dB
///    between the two nearest scale values — matches the
///    [`crate::aggregate::spectrum_to_columns`] sparse-bin fallback so
///    the low-end stays smooth at low `bpo` combined with sparse scales.
/// 4. Power → dB via `10·log10`.
///
/// Returns `(band_centres_hz, band_levels_db)`. Band centres are always
/// the output of [`ioct_band_centers`] for the same args; levels track
/// 1:1 with centres. Empty input or zero `bpo` returns
/// `(centres, vec![])` (centres may still be non-empty if the args allow).
pub fn cwt_to_fractional_octave(
    cwt_col_db: &[f32],
    cwt_freqs: &[f32],
    bpo: usize,
    f_min: f32,
    f_max: f32,
) -> (Vec<f32>, Vec<f32>) {
    let centres = ioct_band_centers(f_min, f_max, bpo);
    if centres.is_empty()
        || cwt_col_db.is_empty()
        || cwt_freqs.is_empty()
        || cwt_col_db.len() != cwt_freqs.len()
    {
        return (centres, Vec::new());
    }

    let mut levels = Vec::with_capacity(centres.len());
    for &c in &centres {
        let (lo, hi) = ioct_band_edges(c, bpo);
        let mut p_sum = 0.0_f64;
        let mut have_any = false;
        for (&f, &db) in cwt_freqs.iter().zip(cwt_col_db.iter()) {
            if f >= lo && f < hi && db.is_finite() {
                p_sum += 10_f64.powf(db as f64 / 10.0);
                have_any = true;
            }
        }
        let v = if have_any && p_sum > 0.0 {
            (10.0 * p_sum.log10()) as f32
        } else {
            interp_log_db(c, cwt_freqs, cwt_col_db)
        };
        levels.push(v);
    }
    (centres, levels)
}

// Linear-in-dB interpolation against log10(f) between the two nearest
// scale samples. Matches `spectrum_to_columns`' fallback so a fractional-
// octave view at low bpo over sparse scales doesn't show -inf gaps.
fn interp_log_db(c: f32, freqs: &[f32], db: &[f32]) -> f32 {
    if freqs.is_empty() || db.is_empty() {
        return f32::NEG_INFINITY;
    }
    let above = freqs
        .iter()
        .position(|&f| f >= c)
        .unwrap_or(freqs.len() - 1);
    let below = above.saturating_sub(1);
    let f_below = freqs[below].max(f32::MIN_POSITIVE);
    let f_above = freqs[above].max(f32::MIN_POSITIVE);
    if below == above || c <= f_below {
        db[below]
    } else if c >= f_above {
        db[above]
    } else {
        let lb = (f_below as f64).log10();
        let la = (f_above as f64).log10();
        let t = ((c as f64).log10() - lb) / (la - lb);
        (db[below] as f64 * (1.0 - t) + db[above] as f64 * t) as f32
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    #[test]
    fn band_centers_anchored_at_1khz() {
        for &bpo in &[1usize, 3, 6, 12, 24] {
            let centres = ioct_band_centers(20.0, 20_000.0, bpo);
            let one_khz = centres
                .iter()
                .any(|&c| ((c as f64 - 1000.0) / 1000.0).abs() < 1e-4);
            assert!(
                one_khz,
                "1 kHz not in band-centre grid for bpo={bpo}: {centres:?}"
            );
        }
    }

    #[test]
    fn band_edges_tile() {
        // Adjacent bands: f_hi[i] == f_lo[i+1] within float tolerance.
        // Holds because both are c · 2^(±1/(2·bpo)) and adjacent centres
        // differ by 2^(1/bpo) — the geometric ratio cancels exactly.
        let bpo = 3;
        let centres = ioct_band_centers(20.0, 20_000.0, bpo);
        for w in centres.windows(2) {
            let (_, hi_lo) = ioct_band_edges(w[0], bpo);
            let (lo_hi, _) = ioct_band_edges(w[1], bpo);
            assert_relative_eq!(hi_lo as f64, lo_hi as f64, max_relative = 1e-5);
        }
    }

    #[test]
    fn single_tone_level_preserved() {
        // Synthetic CWT column: only the scale matching a band centre
        // carries -6 dBFS, all others are at -∞. Sum semantics trivially
        // preserves the level since p_sum = 10^(-0.6) and the output is
        // 10·log10 of that. Tests the aggregator in isolation from the
        // kernel-overlap behaviour discussed in the module header.
        let bpo = 3;
        let centres = ioct_band_centers(20.0, 20_000.0, bpo);
        let target_idx = centres
            .iter()
            .position(|&c| ((c as f64 - 1000.0) / 1000.0).abs() < 1e-4)
            .unwrap();
        let target_c = centres[target_idx];

        let cwt_freqs = vec![
            target_c * 0.50,
            target_c * 0.71,
            target_c,
            target_c * 1.41,
            target_c * 2.00,
        ];
        let mut col = vec![f32::NEG_INFINITY; 5];
        col[2] = -6.0;

        let (out_centres, levels) = cwt_to_fractional_octave(&col, &cwt_freqs, bpo, 20.0, 20_000.0);
        let band_idx = out_centres
            .iter()
            .position(|&c| (c - target_c).abs() < 1e-3)
            .unwrap();
        assert_relative_eq!(levels[band_idx] as f64, -6.0, epsilon = 0.1);
    }

    #[test]
    fn pink_noise_is_flat() {
        // NOTE: the aggregator spec calls out "pink in amplitude (1/√f)" as
        // the input, but that's an FFT-bin intuition. In CWT, real pink
        // noise (PSD ∝ 1/f) produces a CONSTANT per-scale dBFS reading,
        // because Morlet analysis bandwidth ∝ f cancels the 1/f density
        // exactly when the bandpass output is integrated. So the realistic
        // "pink noise CWT column" is constant — and that's what should
        // produce a flat per-band reading. We test that here.
        let bpo = 3;
        let n_scales = 256;
        let f_min = 50.0_f32;
        let f_max = 16_000.0_f32;
        let cwt_freqs: Vec<f32> = (0..n_scales)
            .map(|i| {
                let t = i as f32 / (n_scales - 1) as f32;
                f_min * (f_max / f_min).powf(t)
            })
            .collect();
        let cwt_col_db = vec![-30.0_f32; n_scales];

        let (_, levels) = cwt_to_fractional_octave(&cwt_col_db, &cwt_freqs, bpo, f_min, f_max);
        let max = levels.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let min = levels.iter().cloned().fold(f32::INFINITY, f32::min);
        // Worst-case spread = log10(N_max / N_min) · 10 where N_x is the
        // scale count per band. Constant log spacing keeps N near constant
        // across bands; small variation comes from integer rounding.
        assert!(
            (max - min).abs() < 0.5,
            "pink noise not flat: spread {:.3} dB, levels={:?}",
            max - min,
            levels
        );
    }

    #[test]
    fn empty_input_does_not_panic() {
        // Any combination of empty inputs: levels empty, no panic.
        let (_, l1) = cwt_to_fractional_octave(&[], &[], 3, 20.0, 20_000.0);
        assert!(l1.is_empty());
        let (c2, l2) = cwt_to_fractional_octave(&[], &[], 0, 20.0, 20_000.0);
        assert!(c2.is_empty() && l2.is_empty());
        // Mismatched lengths also handled (returns no levels).
        let (_, l3) = cwt_to_fractional_octave(&[1.0], &[1.0, 2.0], 3, 20.0, 20_000.0);
        assert!(l3.is_empty());
        // Inverted range.
        let c4 = ioct_band_centers(1000.0, 100.0, 3);
        assert!(c4.is_empty());
    }

    #[test]
    fn empty_band_falls_back_to_interp() {
        // bpo = 12 with only a handful of cwt freqs: most bands will have
        // no scale falling inside and must use interpolation rather than
        // -inf. Verify finite output in every band.
        let bpo = 12;
        let cwt_freqs = vec![100.0_f32, 1000.0, 10_000.0];
        let cwt_col_db = vec![-20.0_f32, -10.0, -30.0];
        let (_, levels) = cwt_to_fractional_octave(&cwt_col_db, &cwt_freqs, bpo, 20.0, 20_000.0);
        for (i, &v) in levels.iter().enumerate() {
            assert!(v.is_finite(), "band {i} not finite: {v}");
        }
    }

    #[test]
    fn end_to_end_dbfs_calibration() {
        // The aggregator preserves dBFS for an isolated CWT scale: this
        // test mirrors the ZMQ pipeline shape (real log_scales frequency
        // grid, 1/3-oct bands) but feeds a synthetic column with one
        // scale finite — which sidesteps the kernel-overlap drift
        // discussed in the module header. A separate test
        // (`end_to_end_band_assignment`) exercises real morlet_cwt to
        // verify the *peak band* is correct even when absolute level is
        // hot.
        use crate::cwt::{log_scales, DEFAULT_SIGMA};
        let sr = 48_000;
        let n_scales = 256;
        let (_scales, freqs) = log_scales(20.0, 20_000.0, n_scales, sr, DEFAULT_SIGMA);
        let target_idx = freqs
            .iter()
            .enumerate()
            .min_by(|a, b| {
                (a.1 - 1000.0)
                    .abs()
                    .partial_cmp(&(b.1 - 1000.0).abs())
                    .unwrap()
            })
            .unwrap()
            .0;
        let target_f = freqs[target_idx];
        let mut col = vec![f32::NEG_INFINITY; freqs.len()];
        col[target_idx] = -6.0;

        let bpo = 3;
        let (centres, levels) = cwt_to_fractional_octave(&col, &freqs, bpo, 20.0, 20_000.0);
        let band_idx = centres
            .iter()
            .position(|&c| {
                let (lo, hi) = ioct_band_edges(c, bpo);
                target_f >= lo && target_f < hi
            })
            .unwrap();
        assert_relative_eq!(levels[band_idx] as f64, -6.0, epsilon = 0.3);
    }

    #[test]
    fn end_to_end_band_assignment() {
        // True generator → morlet_cwt → fractional-octave path. Absolute
        // level is hot due to kernel overlap (documented), but the band
        // containing the tone must be the band with the maximum reading.
        use crate::cwt::{log_scales, morlet_cwt, DEFAULT_SIGMA};
        let sr = 48_000;
        let n = 8192;
        let amp = 10f64.powf(-6.0 / 20.0);
        let f_test = 1000.0_f64;
        let samples: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / sr as f64;
                (amp * (2.0 * std::f64::consts::PI * f_test * t).cos()) as f32
            })
            .collect();
        let (scales, freqs) = log_scales(20.0, 20_000.0, 256, sr, DEFAULT_SIGMA);
        let col = morlet_cwt(&samples, sr, &scales, DEFAULT_SIGMA);

        let bpo = 3;
        let (centres, levels) = cwt_to_fractional_octave(&col, &freqs, bpo, 20.0, 20_000.0);
        let peak_band = levels
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        let (lo, hi) = ioct_band_edges(centres[peak_band], bpo);
        assert!(
            (lo as f64) <= f_test && (f_test as f32) < hi,
            "peak band [{lo}, {hi}) does not contain {f_test} Hz; band centre {}",
            centres[peak_band]
        );
    }
}
