//! Axis domains, gridline steps and tick formatting — shared by the
//! HTML and PDF renderers so the same report plots on the same grid in
//! both. Before this module the two carried independent `pick_db_step`
//! / `nice_db_step` pairs with different thresholds and different dB
//! padding rules, so one report produced two different-looking plots.
//!
//! Every entry point here is total: degenerate input (empty, all-NaN,
//! a single distinct frequency, a DC bin) yields a usable fallback
//! domain rather than an infinity, a NaN coordinate, or a hang.

/// Log-frequency x-domain over `freqs`, ignoring DC and non-finite
/// values. Falls back to the audio band when nothing usable is left or
/// when every point sits at one frequency (a zero-width log span would
/// divide by zero downstream).
pub fn log_freq_domain(freqs: impl IntoIterator<Item = f64>) -> (f64, f64) {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for f in freqs {
        if f > 0.0 && f.is_finite() {
            lo = lo.min(f);
            hi = hi.max(f);
        }
    }
    if !lo.is_finite() || !hi.is_finite() || lo >= hi {
        return (20.0, 20_000.0);
    }
    (lo, hi)
}

/// dB y-domain padded out to whole decibels so gridlines land on round
/// numbers, widened to at least 6 dB so a flat trace does not render as
/// a line pinned to the frame.
pub fn db_domain(values: impl IntoIterator<Item = f64>) -> (f64, f64) {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for v in values {
        if v.is_finite() {
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    if !lo.is_finite() || !hi.is_finite() {
        return (-60.0, 0.0);
    }
    let mut min = lo.floor() - 1.0;
    let mut max = hi.ceil() + 1.0;
    if (max - min) < 6.0 {
        min -= 3.0;
        max += 3.0;
    }
    (min, max)
}

/// Gridline spacing for a dB span, chosen so a plot carries roughly
/// four to eight labelled lines.
pub fn db_step(span: f64) -> f64 {
    if span > 80.0 {
        20.0
    } else if span > 40.0 {
        10.0
    } else if span > 16.0 {
        5.0
    } else {
        2.0
    }
}

/// dB gridline values inside `[min, max]`, ascending.
pub fn db_gridlines(min: f64, max: f64) -> Vec<f64> {
    let step = db_step(max - min);
    if !step.is_finite() || step <= 0.0 || !min.is_finite() || !max.is_finite() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut v = (min / step).ceil() * step;
    // Bounded by construction: `step > 0`, so at most (span / step) + 1
    // iterations. The old renderers multiplied a running decade by ten,
    // which never advances from a zero seed.
    while v <= max {
        out.push(v);
        v += step;
    }
    out
}

/// Frequency tick positions inside `[fmin, fmax]`.
///
/// Decades where at least two of them fall in range, and a 1-1.5-2-3-5-7
/// ladder within the decade where they do not: a sweep narrower than a
/// decade (20 Hz to 38 Hz, say) contains no decade boundary at all and
/// used to draw with a completely unlabelled frequency axis.
///
/// Walks an integer exponent rather than repeatedly multiplying a
/// running value by ten: a `freq_hz` of 0 seeds that multiplication
/// with `10f64.powf(-inf) == 0.0`, which never advances and spins
/// forever. See `log_freq_domain`, which also refuses DC.
pub fn freq_ticks(fmin: f64, fmax: f64) -> Vec<f64> {
    if !fmin.is_finite() || !fmax.is_finite() || fmin <= 0.0 || fmax < fmin {
        return Vec::new();
    }
    let decades = ladder(fmin, fmax, &[1.0]);
    if decades.len() >= 2 {
        return decades;
    }
    let fine = ladder(fmin, fmax, &[1.0, 1.5, 2.0, 3.0, 5.0, 7.0]);
    if fine.len() >= 2 {
        return fine;
    }
    // Nothing on the ladder lands inside a very narrow span; label the
    // endpoints so the axis still states what it covers.
    vec![fmin, fmax]
}

/// `mantissas` scaled across every decade overlapping `[fmin, fmax]`.
fn ladder(fmin: f64, fmax: f64, mantissas: &[f64]) -> Vec<f64> {
    let hi = fmax.log10();
    let mut exp = fmin.log10().floor() as i32;
    let mut out = Vec::new();
    while (exp as f64) <= hi {
        let decade = 10f64.powi(exp);
        for m in mantissas {
            let f = m * decade;
            if f >= fmin && f <= fmax {
                out.push(f);
            }
        }
        exp += 1;
    }
    out
}

/// Width of a log-frequency domain, floored away from zero so callers
/// can divide by it unconditionally.
pub fn log_span(fmin: f64, fmax: f64) -> f64 {
    (fmax.log10() - fmin.log10()).max(1e-9)
}

/// Fractional position of `f` within the log domain, clamped to
/// `[0, 1]`. A non-finite input yields 0 rather than propagating a NaN
/// into a coordinate.
pub fn log_pos(f: f64, fmin: f64, fmax: f64) -> f64 {
    if !f.is_finite() {
        return 0.0;
    }
    clamp01((f.max(fmin).log10() - fmin.log10()) / log_span(fmin, fmax))
}

/// Fractional position of `v` within a linear domain, clamped to
/// `[0, 1]`. A non-finite input yields 0.
pub fn lin_pos(v: f64, min: f64, max: f64) -> f64 {
    if !v.is_finite() {
        return 0.0;
    }
    clamp01((v - min) / (max - min).max(1e-9))
}

/// `f64::clamp` panics on a NaN bound and returns NaN for a NaN input;
/// this maps every unusable value to 0.
fn clamp01(v: f64) -> f64 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}

/// Decade tick label: `100`, `1k`, `10k`.
pub fn format_freq(f: f64) -> String {
    if f >= 1000.0 {
        format!("{:.0}k", f / 1000.0)
    } else {
        format!("{f:.0}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freq_ticks_terminate_on_a_dc_bin() {
        // A `freq_hz` of 0 used to seed the renderers' running decade
        // with 0.0, which `*= 10.0` never moves off — an infinite loop
        // appending gridlines to the output string. Domain and ticks
        // must both refuse it.
        let (fmin, fmax) = log_freq_domain([0.0, 100.0, 1_000.0]);
        assert_eq!((fmin, fmax), (100.0, 1_000.0));
        assert_eq!(freq_ticks(fmin, fmax), vec![100.0, 1_000.0]);
        // And directly, in case a caller skips the domain helper.
        assert!(freq_ticks(0.0, 1_000.0).is_empty());
        assert!(freq_ticks(f64::NAN, 1_000.0).is_empty());
    }

    #[test]
    fn a_full_range_sweep_is_labelled_by_decade() {
        assert_eq!(freq_ticks(20.0, 20_000.0), vec![100.0, 1_000.0, 10_000.0]);
    }

    #[test]
    fn a_sweep_narrower_than_a_decade_still_gets_labels() {
        // 20 Hz to 38 Hz contains no decade boundary; the axis used to
        // render with no frequency labels at all.
        let ticks = freq_ticks(20.0, 37.97);
        assert!(ticks.len() >= 2, "{ticks:?}");
        assert!(ticks.iter().all(|f| *f >= 20.0 && *f <= 37.97), "{ticks:?}");
    }

    #[test]
    fn a_span_too_narrow_for_any_ladder_step_labels_its_endpoints() {
        let ticks = freq_ticks(1_000.0, 1_010.0);
        assert_eq!(ticks, vec![1_000.0, 1_010.0]);
    }

    #[test]
    fn single_frequency_does_not_produce_nan_positions() {
        // Two points at one frequency give a zero-width log span; the
        // old HTML renderer divided by it and emitted NaN coordinates.
        let (fmin, fmax) = log_freq_domain([1_000.0, 1_000.0]);
        assert_eq!((fmin, fmax), (20.0, 20_000.0));
        assert!(log_pos(1_000.0, fmin, fmax).is_finite());
        assert!(log_span(5.0, 5.0).is_finite());
        assert!(log_pos(5.0, 5.0, 5.0).is_finite());
        // A non-finite sample must not become a non-finite coordinate.
        assert_eq!(log_pos(f64::NAN, 20.0, 20_000.0), 0.0);
        assert_eq!(lin_pos(f64::NAN, -60.0, 0.0), 0.0);
        assert_eq!(lin_pos(f64::INFINITY, -60.0, 0.0), 0.0);
    }

    #[test]
    fn empty_and_nan_input_fall_back_instead_of_diverging() {
        assert_eq!(log_freq_domain([]), (20.0, 20_000.0));
        assert_eq!(db_domain([]), (-60.0, 0.0));
        assert_eq!(db_domain([f64::NAN, f64::INFINITY]), (-60.0, 0.0));
        assert!(db_gridlines(f64::NAN, 0.0).is_empty());
    }

    #[test]
    fn flat_trace_is_widened_to_a_readable_span() {
        let (min, max) = db_domain([-20.0, -20.0, -20.0]);
        assert!(max - min >= 6.0, "got {min}..{max}");
        assert!(min < -20.0 && max > -20.0);
    }

    #[test]
    fn db_gridlines_stay_inside_the_domain_and_are_bounded() {
        let (min, max) = db_domain([-60.0, 0.0]);
        let lines = db_gridlines(min, max);
        assert!(!lines.is_empty());
        assert!(lines.iter().all(|v| *v >= min && *v <= max), "{lines:?}");
        assert!(lines.len() < 100);
    }

    #[test]
    fn format_freq_switches_to_k_at_a_kilohertz() {
        assert_eq!(format_freq(100.0), "100");
        assert_eq!(format_freq(1_000.0), "1k");
        assert_eq!(format_freq(20_000.0), "20k");
    }
}
