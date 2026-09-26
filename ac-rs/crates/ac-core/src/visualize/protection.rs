//! Data protection for the live transfer function (#670) — Smaart v7
//! User Guide pp. 97–99. Physical reasons only, nothing else:
//!
//! - a buffer with a run of full-scale samples on either leg is thrown away
//!   ([`clipped`]);
//! - no reference signal pauses processing ([`REFERENCE_FLOOR_DBFS`]);
//! - where the reference is too weak at a frequency, that frequency keeps
//!   its last good value ([`hold_weak_reference`]).
//!
//! Pure rules; the daemon applies them per tick.

use super::mtw::splice::Column;

/// Consecutive full-scale samples that mark a clipped buffer: "three or
/// more consecutive samples with maximal amplitude values" (guide p. 97).
pub const CLIP_RUN: usize = 3;

/// "Full scale" for [`CLIP_RUN`]: within 0.00026 dB of 1.0, so a converter
/// that stops a code short of full scale still counts, and a large but
/// unclipped peak does not.
pub const FULL_SCALE: f32 = 0.99997;

/// A leg whose raw peak is at or below this is silent (dBFS). The one
/// definition: `ac-scene`'s fault indicator reads it too.
pub const REFERENCE_FLOOR_DBFS: f64 = -80.0;

/// How far below the median reference density a column may sit and still
/// update, in dB. Below it the column keeps its last good value (Smaart's
/// Magnitude Thresholding). A pink-noise reference spans about 30 dB of
/// density from 20 Hz to 20 kHz (3 dB/octave), so 40 dB leaves every column
/// of a broadband stimulus updating; a band the stimulus does not reach (a
/// notch, the region above a band-limited stimulus) sits far outside it.
pub const REFERENCE_HOLD_BELOW_DB: f64 = 40.0;

/// Whether `buf` holds [`CLIP_RUN`] or more consecutive full-scale
/// samples (either polarity).
pub fn clipped(buf: &[f32]) -> bool {
    let mut run = 0;
    for &x in buf {
        if x.abs() >= FULL_SCALE {
            run += 1;
            if run >= CLIP_RUN {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// Replace every column of `fresh` whose reference density sits more than
/// [`REFERENCE_HOLD_BELOW_DB`] below the median over all columns with the
/// matching column of `held` — the last value that cleared the threshold.
/// With no matching held column (none yet, or the grid changed), the column
/// is blanked by setting its coherence to 0, so the display's mask drops
/// it. Returns how many columns were held or blanked.
///
/// Density, `ref_level / df`: every ladder stage uses the same FFT size, so
/// per-bin power divided by the stage's bin width is a power per hertz that
/// compares across stages (decimating by D divides both the per-sample
/// variance and the bin width by D). Per-bin levels alone do not — and a
/// per-stage median would miss a stimulus that leaves a whole stage's band.
pub fn hold_weak_reference(fresh: &mut [Column], held: Option<&[Column]>) -> usize {
    if fresh.is_empty() {
        return 0;
    }
    let density = |c: &Column| if c.df > 0.0 { c.ref_level / c.df } else { 0.0 };
    let mut densities: Vec<f64> = fresh.iter().map(density).collect();
    densities.sort_by(|a, b| a.total_cmp(b));
    let floor = densities[densities.len() / 2] * 10f64.powf(-REFERENCE_HOLD_BELOW_DB / 10.0);
    let same_grid = held.filter(|h| {
        h.len() == fresh.len() && h.iter().zip(fresh.iter()).all(|(a, b)| a.freq == b.freq)
    });
    let mut count = 0;
    for (i, col) in fresh.iter_mut().enumerate() {
        if density(col) >= floor {
            continue;
        }
        count += 1;
        match same_grid {
            Some(h) => {
                col.h1 = h[i].h1;
                col.coherence = h[i].coherence;
                col.ref_level = h[i].ref_level;
            }
            None => col.coherence = 0.0,
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use realfft::num_complex::Complex;

    #[test]
    fn three_full_scale_samples_in_a_row_clip_two_do_not() {
        assert!(!clipped(&[0.5, 1.0, 1.0, 0.2, -1.0, -1.0]));
        assert!(clipped(&[0.5, 1.0, -1.0, 0.99998, 0.2]));
        assert!(!clipped(&[0.9, 0.95, 0.999, 0.9999]));
    }

    fn col(freq: f64, stage: usize, ref_level: f64, h: f64) -> Column {
        Column {
            freq,
            lo: freq * 0.99,
            hi: freq * 1.01,
            h1: Complex::new(h, 0.0),
            coherence: 0.9,
            df: 1.0,
            window_s: 1.0,
            n: 4,
            stage,
            blend: 0.0,
            bins: 1,
            ref_level,
        }
    }

    #[test]
    fn a_weak_reference_column_keeps_its_last_good_value() {
        let held = vec![
            col(100.0, 0, 1.0, 0.5),
            col(200.0, 0, 1.0, 0.6),
            col(400.0, 0, 1.0, 0.7),
        ];
        // 400 Hz: reference 50 dB below the stage median.
        let mut fresh = vec![
            col(100.0, 0, 1.0, 2.0),
            col(200.0, 0, 1.0, 2.0),
            col(400.0, 0, 1e-5, 9.0),
        ];
        assert_eq!(hold_weak_reference(&mut fresh, Some(&held)), 1);
        assert_eq!(fresh[0].h1.re, 2.0, "a strong column updates");
        assert_eq!(fresh[2].h1.re, 0.7, "the weak column keeps the held value");
        // 30 dB below still updates.
        let mut fresh = vec![
            col(100.0, 0, 1.0, 2.0),
            col(200.0, 0, 1.0, 2.0),
            col(400.0, 0, 1e-3, 9.0),
        ];
        assert_eq!(hold_weak_reference(&mut fresh, Some(&held)), 0);
        assert_eq!(fresh[2].h1.re, 9.0);
    }

    #[test]
    fn with_nothing_held_a_weak_column_is_blanked() {
        let mut fresh = vec![
            col(100.0, 0, 1.0, 2.0),
            col(200.0, 0, 1.0, 2.0),
            col(400.0, 0, 1e-5, 9.0),
        ];
        assert_eq!(hold_weak_reference(&mut fresh, None), 1);
        assert_eq!(fresh[2].coherence, 0.0);
    }

    /// Levels compare as density across stages: a deeper stage's lower
    /// per-bin level over a narrower bin is the same power per hertz, not a
    /// weak reference.
    #[test]
    fn stages_compare_by_density() {
        let deep = |f: f64| Column {
            df: 0.01,
            ..col(f, 1, 0.01, 1.0)
        };
        let mut fresh = vec![
            deep(20.0),
            deep(30.0),
            col(1000.0, 0, 1.0, 1.0),
            col(2000.0, 0, 1.0, 1.0),
        ];
        assert_eq!(hold_weak_reference(&mut fresh, None), 0);
        // A whole stage without stimulus is caught, which a per-stage
        // median would miss.
        let silent = |f: f64| Column {
            df: 0.01,
            ..col(f, 1, 1e-8, 1.0)
        };
        let mut fresh = vec![
            silent(20.0),
            silent(30.0),
            col(1000.0, 0, 1.0, 1.0),
            col(2000.0, 0, 1.0, 1.0),
            col(4000.0, 0, 1.0, 1.0),
        ];
        assert_eq!(hold_weak_reference(&mut fresh, None), 2);
    }
}
