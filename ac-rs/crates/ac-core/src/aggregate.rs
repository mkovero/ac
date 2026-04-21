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
    let freqs64: Vec<f64> = if n_columns == 0 || !(f_min > 0.0) || !(f_max > f_min) {
        Vec::new()
    } else {
        let log_ratio = (f_max / f_min).ln();
        let n = n_columns as f64;
        (0..n_columns)
            .map(|i| f_min * (log_ratio * (i as f64 + 0.5) / n).exp())
            .collect()
    };
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
                f >= 20.0 && f <= 200.0
            })
            .collect();
        assert!(!in_band.is_empty(), "expected columns inside 20-200 Hz");
        for w in in_band.windows(3) {
            let (a, b, c) = (cols[w[0]], cols[w[1]], cols[w[2]]);
            let zigzag = a < -120.0 && b > -100.0 && c < -120.0;
            assert!(
                !zigzag,
                "zigzag at columns {:?}: {} {} {}",
                w, a, b, c
            );
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
                assert!(v <= -150.0, "column {} at {:.0} Hz leaked energy: {} dB", i, f, v);
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
                f >= 80.0 && f <= 130.0
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
        let gap = maxima
            .windows(2)
            .any(|p| p[1].0 > p[0].0 + 2);
        assert!(gap, "maxima are adjacent, no dip between them: {:?}", maxima);
    }

    #[test]
    fn empty_input_does_not_panic() {
        let cols = spectrum_to_columns(&[], 48000.0, 20.0, 20000.0, 100);
        assert_eq!(cols.len(), 100);
        assert!(cols.iter().all(|v| *v == f32::NEG_INFINITY));
    }
}
