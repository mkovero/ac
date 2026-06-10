/// Default number of log-spaced display columns the daemon ships over
/// the `spectrum` wire message. 4096 columns across 20-24000 Hz is ~3
/// cents per column — fine enough that the UI's local-maxima peak
/// picker isn't bottlenecked by aggregation below 2 kHz. The original
/// 2048 was picked to match 4K screen width.
pub const DEFAULT_WIRE_COLUMNS: usize = 4096;

/// Aggregate a linear-binned half-spectrum onto a log-frequency display.
///
/// `spectrum_db` holds `N/2 + 1` magnitudes in dBFS with DC at index 0;
/// bin `k` maps to frequency `k * sr / (2 * (len - 1))`.
///
/// Returns `n_columns` values, one per display column. Column `i` covers
/// `[f_min * r^(i/n), f_min * r^((i+1)/n)]` with `r = f_max/f_min`.
/// When ≥1 bin falls in the column the column holds the max of those bin
/// magnitudes (preserves narrow peaks). When 0 bins fall in the column
/// (low-frequency end, where columns are narrower than Δf), the value is
/// linearly interpolated in dB against `log10(f)` between the two nearest
/// bins — smooth curve rather than line segments between sparse samples.
///
/// `f32::NEG_INFINITY`-filled vec is returned for degenerate input
/// (`spectrum_db.len() < 2` or `n_columns == 0` is handled too).
// Negated `>` comparisons are intentional NaN-aware guards: `!(f_min > 0.0)`
// is true for NaN, zero, and negative inputs, all of which must short-circuit.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn spectrum_to_columns(
    spectrum_db: &[f32],
    sr: f32,
    f_min: f32,
    f_max: f32,
    n_columns: usize,
) -> Vec<f32> {
    if n_columns == 0 {
        return Vec::new();
    }
    let b = spectrum_db.len();
    if b < 2 || !(f_min > 0.0) || !(f_max > f_min) || !(sr > 0.0) {
        return vec![f32::NEG_INFINITY; n_columns];
    }

    let df = sr / (2.0 * (b - 1) as f32);
    let freq_of = |k: usize| k as f32 * df;
    let log_ratio = (f_max / f_min).ln();
    let n = n_columns as f32;
    let col_lo = |i: usize| f_min * (log_ratio * i as f32 / n).exp();
    let col_centre = |i: usize| f_min * (log_ratio * (i as f32 + 0.5) / n).exp();

    let mut out = Vec::with_capacity(n_columns);
    let mut k: usize = 0;

    for i in 0..n_columns {
        let lo = col_lo(i);
        let hi = col_lo(i + 1);
        while k < b && freq_of(k) < lo {
            k += 1;
        }
        let mut peak = f32::NEG_INFINITY;
        let mut j = k;
        while j < b && freq_of(j) < hi {
            if spectrum_db[j] > peak {
                peak = spectrum_db[j];
            }
            j += 1;
        }
        if peak.is_finite() {
            out.push(peak);
            continue;
        }
        let c = col_centre(i);
        let above = k.min(b - 1);
        let below = above.saturating_sub(1);
        let f_below = freq_of(below).max(f32::MIN_POSITIVE);
        let f_above = freq_of(above).max(f32::MIN_POSITIVE);
        let v = if below == above || c <= f_below {
            spectrum_db[below]
        } else if c >= f_above {
            spectrum_db[above]
        } else {
            let lb = f_below.log10();
            let la = f_above.log10();
            let t = (c.log10() - lb) / (la - lb);
            spectrum_db[below] * (1.0 - t) + spectrum_db[above] * t
        };
        out.push(v);
    }
    out
}

/// Per-column centre frequencies for a log-spaced display of `n_columns`
/// columns between `f_min` and `f_max`. Shared by the `_wire` helpers so
/// the magnitudes and the axis they ship alongside always agree. Returns
/// an empty vec for degenerate input.
// Negated `>` comparisons are intentional NaN-aware guards.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
fn column_centre_freqs(f_min: f64, f_max: f64, n_columns: usize) -> Vec<f64> {
    if n_columns == 0 || !(f_min > 0.0) || !(f_max > f_min) {
        return Vec::new();
    }
    let log_ratio = (f_max / f_min).ln();
    let n = n_columns as f64;
    (0..n_columns)
        .map(|i| f_min * (log_ratio * (i as f64 + 0.5) / n).exp())
        .collect()
}

/// Wire-format helper: aggregate an `f64` linear half-spectrum into a log-
/// frequency representation suitable for ZMQ publish. Returns `(magnitudes,
/// frequencies)` both as `Vec<f64>`, log-spaced between `f_min` and `f_max`.
/// Frequencies are per-column centres so axis labels align with the data.
pub fn spectrum_to_columns_wire(
    spectrum_db: &[f64],
    sr: f64,
    f_min: f64,
    f_max: f64,
    n_columns: usize,
) -> (Vec<f64>, Vec<f64>) {
    let spec32: Vec<f32> = spectrum_db.iter().map(|&v| v as f32).collect();
    let cols32 = spectrum_to_columns(&spec32, sr as f32, f_min as f32, f_max as f32, n_columns);
    let cols64: Vec<f64> = cols32.iter().map(|&v| v as f64).collect();
    let freqs64 = column_centre_freqs(f_min, f_max, n_columns);
    (cols64, freqs64)
}

/// Default crossover (Hz) splitting the long-FFT low band from the live
/// short-FFT high band in the dual-resolution monitor (#142). Chosen in a
/// typically quiet region so the splice blend is rarely on top of a strong
/// tone. The daemon owns this value and ships it to the UI so labels never
/// hardcode it.
pub const DEFAULT_LF_CROSSOVER_HZ: f32 = 750.0;

/// Half-width of the linear-in-dB blend band around the crossover, in
/// octaves. The two source spectra are cross-faded across
/// `[crossover / 2^OCT, crossover * 2^OCT]` so the splice has no visible
/// step or doubled peak.
const BLEND_HALF_OCTAVE: f32 = 1.0 / 6.0;

/// Merge two linear half-spectra of the **same** signal — a long-N low-
/// frequency spectrum (`lf_spectrum`) and a short-N high-frequency one
/// (`hf_spectrum`) — into a single log-column set (#142).
///
/// Below `crossover_hz` the finer `lf_spectrum` supplies each column;
/// above it the live `hf_spectrum` does; across a ±`BLEND_HALF_OCTAVE`
/// octave transition band the two are cross-faded linearly in dB so the
/// splice is seamless. Each band reuses [`spectrum_to_columns`] so the
/// max-per-column (peak-preserving) and log-interpolation behaviour is
/// identical to the single-FFT path.
///
/// Both spectra must share the same amplitude convention (peak-normalized,
/// N-independent — see `spectrum_only`). When `lf_spectrum` is unusable or
/// the crossover is out of band, this degrades to pure `hf_spectrum`
/// columns.
// Negated `>` comparison is an intentional NaN-aware guard.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn spectrum_to_columns_multiband(
    lf_spectrum: &[f32],
    hf_spectrum: &[f32],
    sr: f32,
    crossover_hz: f32,
    f_min: f32,
    f_max: f32,
    n_columns: usize,
) -> Vec<f32> {
    if n_columns == 0 {
        return Vec::new();
    }
    let hf = spectrum_to_columns(hf_spectrum, sr, f_min, f_max, n_columns);
    if lf_spectrum.len() < 2 || !(crossover_hz > f_min) {
        return hf;
    }
    let lf = spectrum_to_columns(lf_spectrum, sr, f_min, f_max, n_columns);

    let blend = 2.0_f32.powf(BLEND_HALF_OCTAVE);
    let lo = crossover_hz / blend;
    let hi = crossover_hz * blend;
    let log_ratio = (f_max / f_min).ln();
    let n = n_columns as f32;
    let col_centre = |i: usize| f_min * (log_ratio * (i as f32 + 0.5) / n).exp();

    let mut out = Vec::with_capacity(n_columns);
    for i in 0..n_columns {
        let c = col_centre(i);
        let v = if c <= lo {
            lf[i]
        } else if c >= hi {
            hf[i]
        } else {
            let t = (c.ln() - lo.ln()) / (hi.ln() - lo.ln());
            lf[i] * (1.0 - t) + hf[i] * t
        };
        out.push(v);
    }
    out
}

/// Wire-format dual-resolution merge: [`spectrum_to_columns_multiband`]
/// over `f64` half-spectra, returning `(magnitudes, frequencies)` ready
/// for ZMQ publish. The frequency axis is identical to
/// [`spectrum_to_columns_wire`], so the wire frame shape is unchanged.
pub fn spectrum_to_columns_multiband_wire(
    lf_spectrum_db: &[f64],
    hf_spectrum_db: &[f64],
    sr: f64,
    crossover_hz: f64,
    f_min: f64,
    f_max: f64,
    n_columns: usize,
) -> (Vec<f64>, Vec<f64>) {
    let lf32: Vec<f32> = lf_spectrum_db.iter().map(|&v| v as f32).collect();
    let hf32: Vec<f32> = hf_spectrum_db.iter().map(|&v| v as f32).collect();
    let cols32 = spectrum_to_columns_multiband(
        &lf32,
        &hf32,
        sr as f32,
        crossover_hz as f32,
        f_min as f32,
        f_max as f32,
        n_columns,
    );
    let cols64: Vec<f64> = cols32.iter().map(|&v| v as f64).collect();
    let freqs64 = column_centre_freqs(f_min, f_max, n_columns);
    (cols64, freqs64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_spectrum(n: usize, tone_bin: usize, tone_db: f32, floor_db: f32) -> Vec<f32> {
        let len = n / 2 + 1;
        let mut v = vec![floor_db; len];
        v[tone_bin] = tone_db;
        v
    }

    #[test]
    fn low_freq_no_zigzag() {
        let spec = make_spectrum(8192, 4, 0.0, -180.0);
        let cols = spectrum_to_columns(&spec, 96000.0, 20.0, 20000.0, 1000);
        let decade = (20000.0_f32 / 20.0).log10();
        let col_freq = |i: usize| 20.0_f32 * 10f32.powf(decade * (i as f32 + 0.5) / 1000.0);
        let in_band: Vec<usize> = (0..cols.len())
            .filter(|&i| {
                let f = col_freq(i);
                (20.0..=200.0).contains(&f)
            })
            .collect();
        assert!(!in_band.is_empty(), "expected columns inside 20-200 Hz");
        for w in in_band.windows(3) {
            let (a, b, c) = (cols[w[0]], cols[w[1]], cols[w[2]]);
            let zigzag = a < -120.0 && b > -100.0 && c < -120.0;
            assert!(!zigzag, "zigzag at columns {:?}: {} {} {}", w, a, b, c);
        }
    }

    #[test]
    fn high_freq_peak_preserved() {
        let tone_bin = (10_000.0_f32 / (96_000.0 / 8192.0)).round() as usize;
        let spec = make_spectrum(8192, tone_bin, 0.0, -180.0);
        let cols = spectrum_to_columns(&spec, 96000.0, 20.0, 20000.0, 500);
        let decade = (20000.0_f32 / 20.0).log10();
        let col_freq = |i: usize| 20.0_f32 * 10f32.powf(decade * (i as f32 + 0.5) / 500.0);
        let peak = cols.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert!(peak >= -1.0, "peak {} did not survive aggregation", peak);
        for (i, &v) in cols.iter().enumerate() {
            let f = col_freq(i);
            if (f - 10_000.0).abs() > 2_000.0 {
                assert!(
                    v <= -150.0,
                    "column {} at {:.0} Hz leaked energy: {} dB",
                    i,
                    f,
                    v
                );
            }
        }
    }

    #[test]
    fn close_tones_resolve() {
        let len = 65536 / 2 + 1;
        let mut spec = vec![-180.0_f32; len];
        let df = 48000.0_f32 / 65536.0;
        let bin_a = (100.0_f32 / df).round() as usize;
        let bin_b = (103.0_f32 / df).round() as usize;
        assert!(bin_b > bin_a + 1, "test setup: bins should be distinct");
        spec[bin_a] = 0.0;
        spec[bin_b] = 0.0;

        let cols = spectrum_to_columns(&spec, 48000.0, 20.0, 20000.0, 1920);
        let decade = (20000.0_f32 / 20.0).log10();
        let col_freq = |i: usize| 20.0_f32 * 10f32.powf(decade * (i as f32 + 0.5) / 1920.0);

        let window: Vec<(usize, f32)> = (0..cols.len())
            .filter(|&i| {
                let f = col_freq(i);
                (80.0..=130.0).contains(&f)
            })
            .map(|i| (i, cols[i]))
            .collect();

        let mut maxima: Vec<(usize, f32)> = Vec::new();
        for w in window.windows(3) {
            let (_, va) = w[0];
            let (ib, vb) = w[1];
            let (_, vc) = w[2];
            if vb > va && vb > vc && vb > -20.0 {
                maxima.push((ib, vb));
            }
        }
        assert!(
            maxima.len() >= 2,
            "expected two distinct local maxima in 80-130 Hz, got {:?}",
            maxima
        );
        let gap = maxima.windows(2).any(|p| p[1].0 > p[0].0 + 2);
        assert!(
            gap,
            "maxima are adjacent, no dip between them: {:?}",
            maxima
        );
    }

    #[test]
    fn empty_input_does_not_panic() {
        let cols = spectrum_to_columns(&[], 48000.0, 20.0, 20000.0, 100);
        assert_eq!(cols.len(), 100);
        assert!(cols.iter().all(|v| *v == f32::NEG_INFINITY));
    }

    /// Two 5 Hz-spaced tones below 100 Hz resolve as separate peaks when
    /// supplied by a long-N LF spectrum, while a short-N HF spectrum (too
    /// coarse to split them on its own) drives everything above the
    /// crossover (#142, acceptance criterion #1).
    #[test]
    fn close_tones_resolve_multiband() {
        let sr = 48_000.0_f32;
        let crossover = 750.0_f32;

        // LF: long FFT (N=65536) → Δf ≈ 0.73 Hz. Two tones 5 Hz apart at
        // 60 / 65 Hz, well below the crossover.
        let lf_len = 65536 / 2 + 1;
        let lf_df = sr / 65536.0;
        let mut lf = vec![-180.0_f32; lf_len];
        lf[(60.0 / lf_df).round() as usize] = 0.0;
        lf[(65.0 / lf_df).round() as usize] = 0.0;

        // HF: short FFT (N=8192) → Δf ≈ 5.86 Hz, can't split 60/65 Hz.
        let hf_len = 8192 / 2 + 1;
        let hf = vec![-180.0_f32; hf_len];

        let n_cols = 1920;
        let cols = spectrum_to_columns_multiband(&lf, &hf, sr, crossover, 20.0, 20000.0, n_cols);

        let decade = (20000.0_f32 / 20.0).log10();
        let col_freq = |i: usize| 20.0_f32 * 10f32.powf(decade * (i as f32 + 0.5) / n_cols as f32);
        let window: Vec<(usize, f32)> = (0..cols.len())
            .filter(|&i| {
                let f = col_freq(i);
                (50.0..=75.0).contains(&f)
            })
            .map(|i| (i, cols[i]))
            .collect();

        let mut maxima: Vec<(usize, f32)> = Vec::new();
        for w in window.windows(3) {
            if w[1].1 > w[0].1 && w[1].1 > w[2].1 && w[1].1 > -20.0 {
                maxima.push(w[1]);
            }
        }
        assert!(
            maxima.len() >= 2,
            "expected two distinct LF maxima in 50-75 Hz, got {maxima:?}",
        );
        assert!(
            maxima.windows(2).any(|p| p[1].0 > p[0].0 + 2),
            "LF maxima adjacent, no dip between them: {maxima:?}",
        );
    }

    /// Above the crossover the HF spectrum's narrow peak survives the merge
    /// unchanged — LF augmentation must not degrade mid/high rendering
    /// (#142, acceptance criterion #2).
    #[test]
    fn multiband_preserves_hf_peak() {
        let sr = 96_000.0_f32;
        let hf_n = 8192;
        let tone_bin = (10_000.0_f32 / (sr / hf_n as f32)).round() as usize;
        let hf = make_spectrum(hf_n, tone_bin, 0.0, -180.0);
        let lf = make_spectrum(65536, 4, -60.0, -180.0); // quiet LF content

        let n_cols = 500;
        let cols = spectrum_to_columns_multiband(&lf, &hf, sr, 750.0, 20.0, 20000.0, n_cols);
        let decade = (20000.0_f32 / 20.0).log10();
        let col_freq = |i: usize| 20.0_f32 * 10f32.powf(decade * (i as f32 + 0.5) / n_cols as f32);

        let peak = cols.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert!(peak >= -1.0, "HF peak {peak} did not survive merge");
        for (i, &v) in cols.iter().enumerate() {
            let f = col_freq(i);
            if (f - 10_000.0).abs() > 2_000.0 && f > 750.0 {
                assert!(
                    v <= -150.0,
                    "column {i} at {f:.0} Hz leaked HF energy: {v} dB"
                );
            }
        }
    }

    /// A flat input through both bands stays flat across the crossover —
    /// no splice step or doubled peak in the blend region (#142 risk).
    #[test]
    fn multiband_crossover_is_continuous() {
        let lf = vec![-40.0_f32; 65536 / 2 + 1];
        let hf = vec![-40.0_f32; 8192 / 2 + 1];
        let cols = spectrum_to_columns_multiband(&lf, &hf, 48_000.0, 750.0, 20.0, 20000.0, 1024);
        for (i, w) in cols.windows(2).enumerate() {
            assert!(
                (w[0] - w[1]).abs() < 0.5,
                "discontinuity at column {i}: {} -> {}",
                w[0],
                w[1],
            );
        }
    }

    /// Empty LF spectrum (ring not yet full) degrades to pure HF columns.
    #[test]
    fn multiband_empty_lf_falls_back_to_hf() {
        let hf = make_spectrum(8192, 100, 0.0, -180.0);
        let merged = spectrum_to_columns_multiband(&[], &hf, 48_000.0, 750.0, 20.0, 20000.0, 256);
        let hf_only = spectrum_to_columns(&hf, 48_000.0, 20.0, 20000.0, 256);
        assert_eq!(merged, hf_only);
    }
}
