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

/// Normalized y for a phase in degrees on the fixed ±180° pane
/// (`y=0` = −180°, `y=1` = +180°) — the shared mapping between the phase
/// trace and its axis ticks (M4a's `transfer` phase mapping uses this).
pub fn phase_to_y(phase_deg: f64) -> f64 {
    (phase_deg + 180.0) / 360.0
}

/// Phase axis for the transfer view's ±180° pane (#194). A **new**
/// degrees-linear tick model — not the log/dB ones reused.
///
/// The gridline set is `{+180°, +90°, 0°, −90°}` — note **+180 is
/// present, −180 is absent**. The phase trace wraps at `(−180, +180]`
/// (M4a's ruling): a point can land exactly on +180 (the closed end) but
/// never on −180 (the open end, the same physical value as +180). A
/// −180 gridline would mark a value the data never touches; +180 is
/// where a wrapped point lands. The 0° line is the crossover reference a
/// field operator reads phase against.
pub fn phase_axis() -> Axis {
    let ticks = [180, 90, 0, -90]
        .into_iter()
        .map(|deg| Tick {
            position: phase_to_y(deg as f64),
            label: phase_label(deg),
        })
        .collect();
    Axis { ticks }
}

fn phase_label(deg: i64) -> String {
    if deg == 0 {
        "0°".to_string()
    } else {
        format!("{deg:+}°")
    }
}

/// Normalized x for `t_ms` within `[t_min_ms, t_max_ms]` (linear, `x=0`
/// at `t_min_ms`) — the IR panel's one time mapping, shared between the
/// h(t) trace and its axis ticks and arrival marker (#286).
pub fn time_to_x(t_ms: f64, t_min_ms: f64, t_max_ms: f64) -> f64 {
    (t_ms - t_min_ms) / (t_max_ms - t_min_ms)
}

/// The IR panel's time axis (#286): ticks at both endpoints plus `0 ms`
/// when it falls inside the range (a live sidecar always straddles it —
/// `t_origin_ms` is negative by construction — but the guard keeps this
/// total for a caller-given range that doesn't).
///
/// Labels: endpoints get a signed `"{±N} ms"`; the zero tick is bare
/// `"0"`, matching [`phase_axis`]'s `"0°"` convention — the unit is the
/// axis, not every tick's business to repeat.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn time_axis(t_min_ms: f64, t_max_ms: f64) -> Axis {
    if !(t_max_ms > t_min_ms) {
        return Axis { ticks: Vec::new() };
    }
    let mut ticks = vec![
        Tick {
            position: time_to_x(t_min_ms, t_min_ms, t_max_ms),
            label: time_label(t_min_ms),
        },
        Tick {
            position: time_to_x(t_max_ms, t_min_ms, t_max_ms),
            label: time_label(t_max_ms),
        },
    ];
    if t_min_ms < 0.0 && t_max_ms > 0.0 {
        ticks.insert(
            1,
            Tick {
                position: time_to_x(0.0, t_min_ms, t_max_ms),
                label: time_label(0.0),
            },
        );
    }
    Axis { ticks }
}

fn time_label(t_ms: f64) -> String {
    if t_ms == 0.0 {
        "0".to_string()
    } else {
        format!("{t_ms:+.0} ms")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_axis_positions_and_labels_are_exact() {
        let axis = phase_axis();
        let got: Vec<(f64, &str)> = axis
            .ticks
            .iter()
            .map(|t| (t.position, t.label.as_str()))
            .collect();
        // +180 at y=1, +90 at 0.75, 0 at 0.5, -90 at 0.25 — hand-derived
        // from (deg+180)/360.
        assert_eq!(
            got,
            vec![(1.0, "+180°"), (0.75, "+90°"), (0.5, "0°"), (0.25, "-90°"),]
        );
    }

    // The seam #194 flags: the phase gridlines must agree with the trace's
    // wrap boundary (M4a: (−180, +180]). +180 is a real gridline (the
    // trace lands there); −180 is NOT (the trace never reaches it).
    #[test]
    fn phase_axis_has_plus_180_gridline_but_no_minus_180() {
        let axis = phase_axis();
        // A tick at y=1.0 (+180), none at y=0.0 (−180).
        assert!(
            axis.ticks
                .iter()
                .any(|t| t.position == 1.0 && t.label == "+180°"),
            "the +180 boundary gridline must be present"
        );
        assert!(
            !axis.ticks.iter().any(|t| t.position == 0.0),
            "there must be no −180 gridline — the trace wraps there, never lands there"
        );
        // And the 0° reference is present at the midline.
        assert!(axis
            .ticks
            .iter()
            .any(|t| t.position == 0.5 && t.label == "0°"));
    }

    #[test]
    fn phase_to_y_matches_the_transfer_pane_mapping() {
        assert_eq!(phase_to_y(-180.0), 0.0);
        assert_eq!(phase_to_y(0.0), 0.5);
        assert_eq!(phase_to_y(180.0), 1.0);
    }

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

    // ---------------------------------------------------------------
    // IR panel time axis (#286)
    // ---------------------------------------------------------------

    #[test]
    fn time_axis_symmetric_range_has_three_ticks_min_zero_max() {
        let axis = time_axis(-500.0, 500.0);
        let got: Vec<(f64, &str)> = axis
            .ticks
            .iter()
            .map(|t| (t.position, t.label.as_str()))
            .collect();
        assert_eq!(got, vec![(0.0, "-500 ms"), (0.5, "0"), (1.0, "+500 ms")]);
    }

    #[test]
    fn time_axis_range_not_straddling_zero_has_only_endpoints() {
        let axis = time_axis(100.0, 300.0);
        let got: Vec<(f64, &str)> = axis
            .ticks
            .iter()
            .map(|t| (t.position, t.label.as_str()))
            .collect();
        assert_eq!(got, vec![(0.0, "+100 ms"), (1.0, "+300 ms")]);
    }

    #[test]
    fn time_axis_degenerate_bounds_is_empty_not_nan() {
        assert!(time_axis(0.0, 0.0).ticks.is_empty());
        assert!(time_axis(500.0, -500.0).ticks.is_empty());
    }

    #[test]
    fn time_to_x_maps_endpoints_and_midpoint() {
        assert_eq!(time_to_x(-500.0, -500.0, 500.0), 0.0);
        assert_eq!(time_to_x(0.0, -500.0, 500.0), 0.5);
        assert_eq!(time_to_x(500.0, -500.0, 500.0), 1.0);
    }
}
