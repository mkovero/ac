//! Fractional-octave smoothing for FFT magnitude-dB spectra.
//!
//! FFT bins are linearly spaced but audio perception is logarithmic, so a raw
//! magnitude-vs-log-freq plot is visually dense in the top octave and sparse
//! at the bottom. Averaging each bin with its neighbours inside a
//! `±f / 2^(1/2N)` window — "1/N-octave smoothing" — gives the clean curves
//! that measurement tools like REW / OSM / ARTA ship by default.
//!
//! Implementation notes:
//! - Input is dB (already log-magnitude). Arithmetic mean in dB is the
//!   geometric mean of linear magnitude — what every audio tool means by
//!   "1/3 octave smoothing" in practice.
//! - Per-bin window indices are precomputed once per (n, n_bins, sr) tuple
//!   and reused across frames. A linear scan over sorted freqs gives both
//!   lo and hi indices in O(n_bins) amortised.
//! - The DC / sub-audio bin (freqs near 0) is passed through unchanged so
//!   `log(0)` never appears — those bins are below the display range anyway.

/// Precomputed half-open `[lo, hi)` bin-index windows for each output bin.
/// `lo[i] == hi[i]` means "passthrough" (only happens near DC where a
/// fractional-octave window is narrower than a single bin).
#[derive(Clone, Debug)]
pub struct OctaveWindows {
    pub n_frac: u32,
    pub n_bins: usize,
    /// Last freq in the original grid the cache was built for. Two cases with
    /// the same `n_bins` and `n_frac` could still have different sample
    /// rates, which changes bin spacing, so we key on this too.
    pub last_freq: f32,
    pub lo: Vec<u32>,
    pub hi: Vec<u32>,
}

impl OctaveWindows {
    /// Whether this cache still matches the incoming spectrum. Cheap: three
    /// scalar compares. Freq grid is deterministic per (N, sr) so the last
    /// frequency is a good proxy for "same grid".
    pub fn matches(&self, n_frac: u32, n_bins: usize, last_freq: f32) -> bool {
        self.n_frac == n_frac
            && self.n_bins == n_bins
            && (self.last_freq - last_freq).abs() < 1e-3
    }

    /// Build window indices for every bin given the frequency grid. Windows
    /// are symmetric in log-frequency (`f / 2^(1/2N)` … `f * 2^(1/2N)`) and
    /// clamped to the valid bin range. Monotone-increasing `freqs` is
    /// assumed — FFT output always is.
    pub fn build(n_frac: u32, freqs: &[f32]) -> Self {
        let n_bins = freqs.len();
        let mut lo = vec![0u32; n_bins];
        let mut hi = vec![0u32; n_bins];
        let factor = 2f32.powf(1.0 / (2.0 * n_frac as f32));
        // Two-pointer sweep: `lo_p` / `hi_p` monotonically advance as centre
        // bin frequency grows. Saves an O(log n) bsearch per bin.
        let mut lo_p = 0usize;
        let mut hi_p = 0usize;
        for i in 0..n_bins {
            let f_c = freqs[i];
            if !f_c.is_finite() || f_c <= 0.0 {
                lo[i] = i as u32;
                hi[i] = i as u32 + 1;
                continue;
            }
            let f_lo = f_c / factor;
            let f_hi = f_c * factor;
            while lo_p < n_bins && freqs[lo_p] < f_lo {
                lo_p += 1;
            }
            while hi_p < n_bins && freqs[hi_p] <= f_hi {
                hi_p += 1;
            }
            // Guarantee the centre bin itself is always in the window,
            // otherwise the tightest octave at LF would collapse to a
            // zero-length slice.
            let l = lo_p.min(i);
            let h = hi_p.max(i + 1).min(n_bins);
            lo[i] = l as u32;
            hi[i] = h as u32;
        }
        Self {
            n_frac,
            n_bins,
            last_freq: *freqs.last().unwrap_or(&0.0),
            lo,
            hi,
        }
    }
}

/// Smooth a dB-magnitude spectrum with the given precomputed windows.
/// Arithmetic mean across each bin's window, skipping non-finite samples.
/// Returns a new Vec so the caller can keep the raw spectrum too.
pub fn smooth_db(spectrum: &[f32], windows: &OctaveWindows) -> Vec<f32> {
    let n = spectrum.len().min(windows.n_bins);
    let mut out = vec![0.0f32; n];
    for i in 0..n {
        let lo = windows.lo[i] as usize;
        let hi = (windows.hi[i] as usize).min(n);
        if hi <= lo {
            out[i] = spectrum[i];
            continue;
        }
        let mut sum = 0.0f32;
        let mut count = 0u32;
        for &v in &spectrum[lo..hi] {
            if v.is_finite() {
                sum += v;
                count += 1;
            }
        }
        out[i] = if count > 0 {
            sum / count as f32
        } else {
            spectrum[i]
        };
    }
    out
}

/// Human label for the smoothing notification/status.
pub fn label(n_frac: Option<u32>) -> &'static str {
    match n_frac {
        None => "off",
        Some(3) => "1/3 oct",
        Some(6) => "1/6 oct",
        Some(12) => "1/12 oct",
        Some(24) => "1/24 oct",
        _ => "custom",
    }
}

/// Cycle order: off → 1/24 → 1/12 → 1/6 → 1/3 → off.
/// 1/24 first because a single keypress from off should land on the gentlest
/// visible smoothing instead of blowing past 1/3 straight to the broadest.
pub fn next(cur: Option<u32>) -> Option<u32> {
    match cur {
        None => Some(24),
        Some(24) => Some(12),
        Some(12) => Some(6),
        Some(6) => Some(3),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linear_freqs(n: usize, sr: f32) -> Vec<f32> {
        (0..n).map(|i| i as f32 * sr / (2.0 * (n - 1) as f32)).collect()
    }

    #[test]
    fn windows_contain_centre() {
        let freqs = linear_freqs(1024, 48_000.0);
        let w = OctaveWindows::build(6, &freqs);
        for i in 0..freqs.len() {
            assert!(w.lo[i] as usize <= i, "lo[{i}] > i");
            assert!(w.hi[i] as usize > i, "hi[{i}] <= i");
        }
    }

    #[test]
    fn flat_spectrum_stays_flat() {
        let freqs = linear_freqs(1024, 48_000.0);
        let spec = vec![-40.0f32; freqs.len()];
        let w = OctaveWindows::build(3, &freqs);
        let out = smooth_db(&spec, &w);
        for v in out {
            assert!((v - -40.0).abs() < 1e-4);
        }
    }

    #[test]
    fn impulse_smooths_narrowest_at_1_over_24() {
        let freqs = linear_freqs(4096, 48_000.0);
        let mut spec = vec![-100.0f32; freqs.len()];
        // A sharp spike mid-band
        let spike_idx = 1000;
        spec[spike_idx] = 0.0;
        let narrow = OctaveWindows::build(24, &freqs);
        let wide = OctaveWindows::build(3, &freqs);
        let out_narrow = smooth_db(&spec, &narrow);
        let out_wide = smooth_db(&spec, &wide);
        // Wider smoothing spreads the spike over more neighbours, so the
        // peak value drops further.
        assert!(out_wide[spike_idx] < out_narrow[spike_idx]);
    }

    #[test]
    fn matches_detects_grid_change() {
        let a = OctaveWindows::build(6, &linear_freqs(1024, 48_000.0));
        assert!(a.matches(6, 1024, *linear_freqs(1024, 48_000.0).last().unwrap()));
        // Different last-freq → different sample rate → rebuild.
        assert!(!a.matches(6, 1024, 96_000.0 / 2.0));
        // Different n_frac → rebuild.
        assert!(!a.matches(12, 1024, *linear_freqs(1024, 48_000.0).last().unwrap()));
    }

    #[test]
    fn cycle_order() {
        assert_eq!(next(None), Some(24));
        assert_eq!(next(Some(24)), Some(12));
        assert_eq!(next(Some(12)), Some(6));
        assert_eq!(next(Some(6)), Some(3));
        assert_eq!(next(Some(3)), None);
    }
}
