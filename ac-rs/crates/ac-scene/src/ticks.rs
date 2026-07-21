//! Axis/tick generation for the log-frequency and dB axes (deliverable
//! 3). Ranges are always caller-given (architect review, decision 5) —
//! nothing here infers a range from data.

/// One axis tick: a normalized `[0,1]` position plus its label string.
/// Both are part of the contract (AC3) — a renderer must never
/// reformat a label itself.
#[derive(Debug, Clone, PartialEq)]
pub struct Tick {
    pub position: f64,
    pub label: String,
}

/// One axis: an ordered set of ticks over a caller-given range.
#[derive(Debug, Clone, PartialEq)]
pub struct Axis {
    pub ticks: Vec<Tick>,
}

/// Standard decade/2-5 candidate frequencies, labelled per convention
/// (`1000` -> `"1k"`). Fixed list rather than a computed step so labels
/// are exactly the ones users expect on a spectrum axis, and so AC3's
/// character-for-character check has a stable target.
const FREQ_CANDIDATES_HZ: &[f64] = &[
    20.0, 50.0, 100.0, 200.0, 500.0, 1_000.0, 2_000.0, 5_000.0, 10_000.0, 20_000.0,
];

fn freq_label(hz: f64) -> String {
    if hz >= 1_000.0 {
        let k = hz / 1_000.0;
        if (k.round() - k).abs() < 1e-9 {
            format!("{}k", k as i64)
        } else {
            format!("{k}k")
        }
    } else {
        format!("{}", hz as i64)
    }
}

/// Log-frequency axis: ticks at the standard candidate frequencies that
/// fall within `[f_min, f_max]`, positioned by `log(f/f_min) /
/// log(f_max/f_min)` — the same log mapping trace x-coordinates use, so
/// a tick's position and a trace point's x-coordinate agree for the
/// same frequency (AC3's log-mapping-correctness requirement).
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn freq_axis(f_min: f64, f_max: f64) -> Axis {
    // Defensive (handoff: ac-view M3, deliverable 5 — a degenerate
    // range must be unrepresentable in `ac-view`'s own UI state, but
    // this function shouldn't trust that and produce NaN/Inf ticks if
    // it's ever called with one anyway): `f_min <= 0` makes the log
    // mapping undefined, and `f_min >= f_max` makes `freq_to_x`'s
    // denominator zero.
    if !(f_min > 0.0) || !(f_max > f_min) {
        return Axis { ticks: Vec::new() };
    }
    let ticks = FREQ_CANDIDATES_HZ
        .iter()
        .filter(|&&f| f >= f_min && f <= f_max)
        .map(|&f| Tick {
            position: freq_to_x(f, f_min, f_max),
            label: freq_label(f),
        })
        .collect();
    Axis { ticks }
}

/// Normalized x for `f_hz` within `[f_min, f_max]` (log-mapped, `x=0` at
/// `f_min`) — the shared mapping between trace points and axis ticks.
pub fn freq_to_x(f_hz: f64, f_min: f64, f_max: f64) -> f64 {
    (f_hz / f_min).ln() / (f_max / f_min).ln()
}

/// dB axis: ticks every 20 dB within `[db_min, db_max]`, labelled as a
/// bare integer (e.g. `"-60"`, `"-40"`) — the unit itself is an axis
/// title, not part of each tick's label.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn db_axis(db_min: f64, db_max: f64) -> Axis {
    // Defensive, same reasoning as `freq_axis` above: `db_min >=
    // db_max` makes `db_to_y`'s denominator zero.
    if !(db_max > db_min) {
        return Axis { ticks: Vec::new() };
    }
    let start = (db_min / 20.0).ceil() as i64;
    let end = (db_max / 20.0).floor() as i64;
    let ticks = (start..=end)
        .map(|step| {
            let db = (step * 20) as f64;
            Tick {
                position: db_to_y(db, db_min, db_max),
                label: format!("{}", step * 20),
            }
        })
        .collect();
    Axis { ticks }
}

/// Normalized y for `db` within `[db_min, db_max]` (`y=0` = bottom =
/// low level, per the crate's orientation rule) — shared mapping
/// between trace points and axis ticks.
pub fn db_to_y(db: f64, db_min: f64, db_max: f64) -> f64 {
    (db - db_min) / (db_max - db_min)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freq_axis_ac3_case_a_100_to_10k() {
        // Hand-enumerated: candidates in [100, 10000] are
        // 100, 200, 500, 1k, 2k, 5k, 10k.
        let axis = freq_axis(100.0, 10_000.0);
        let labels: Vec<&str> = axis.ticks.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, vec!["100", "200", "500", "1k", "2k", "5k", "10k"]);

        // Known frequency, log-mapping correctness: 1000 Hz within
        // [100, 10000] -> position = ln(10)/ln(100) = 0.5 exactly
        // (100..10000 spans exactly two decades, 1000 is the midpoint
        // in log space).
        let tick_1k = axis.ticks.iter().find(|t| t.label == "1k").unwrap();
        assert!(
            (tick_1k.position - 0.5).abs() < 1e-9,
            "{}",
            tick_1k.position
        );
    }

    #[test]
    fn freq_axis_ac3_case_b_20_to_20k() {
        // Hand-enumerated: full candidate list, all ten fall in range.
        let axis = freq_axis(20.0, 20_000.0);
        let labels: Vec<&str> = axis.ticks.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["20", "50", "100", "200", "500", "1k", "2k", "5k", "10k", "20k"]
        );
        // Endpoints land exactly on 0 and 1.
        assert!((axis.ticks.first().unwrap().position - 0.0).abs() < 1e-9);
        assert!((axis.ticks.last().unwrap().position - 1.0).abs() < 1e-9);
    }

    #[test]
    fn db_axis_ac3_case_minus80_to_0() {
        let axis = db_axis(-80.0, 0.0);
        let labels: Vec<&str> = axis.ticks.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, vec!["-80", "-60", "-40", "-20", "0"]);
        let tick_minus40 = axis.ticks.iter().find(|t| t.label == "-40").unwrap();
        assert!((tick_minus40.position - 0.5).abs() < 1e-9);
    }

    // ---------------------------------------------------------------
    // Defensive degenerate-input tests (handoff: ac-view M3, deliverable
    // 5 — sanctioned additive edit). `ac-view`'s own `FreqRange`/
    // `DbRange` types make these inputs unrepresentable in UI state,
    // but this module shouldn't rely on that and produce NaN/Inf ticks
    // if it's ever handed one directly.
    // ---------------------------------------------------------------

    #[test]
    fn freq_axis_degenerate_equal_bounds_is_empty_not_nan() {
        let axis = freq_axis(1_000.0, 1_000.0);
        assert!(axis.ticks.is_empty());
    }

    #[test]
    fn freq_axis_degenerate_inverted_bounds_is_empty_not_nan() {
        let axis = freq_axis(20_000.0, 20.0);
        assert!(axis.ticks.is_empty());
    }

    #[test]
    fn freq_axis_degenerate_zero_or_negative_min_is_empty_not_nan() {
        assert!(freq_axis(0.0, 20_000.0).ticks.is_empty());
        assert!(freq_axis(-20.0, 20_000.0).ticks.is_empty());
    }

    #[test]
    fn db_axis_degenerate_equal_bounds_is_empty_not_nan() {
        let axis = db_axis(0.0, 0.0);
        assert!(axis.ticks.is_empty());
    }

    #[test]
    fn db_axis_degenerate_inverted_bounds_is_empty_not_nan() {
        let axis = db_axis(0.0, -80.0);
        assert!(axis.ticks.is_empty());
    }
}
