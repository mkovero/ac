use std::path::Path;

pub fn save_csv(results: &[serde_json::Value], path: &Path) {
    let fields = [
        "freq_hz",
        "drive_db",
        "out_vrms",
        "out_dbu",
        "fundamental_dbfs",
        "in_vrms",
        "in_dbu",
        "thd_pct",
        "thdn_pct",
        "noise_floor_dbfs",
    ];
    let headers = fields.map(|field| match field {
        "thd_pct" => "thd_pct_re_total",
        "thdn_pct" => "thdn_pct_re_total",
        _ => field,
    });

    let mut wtr = match csv::Writer::from_path(path) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("  error: cannot write CSV: {e}");
            return;
        }
    };

    wtr.write_record(headers).ok();
    for r in results {
        let mut row = Vec::with_capacity(fields.len());
        for &f in &fields {
            let key = if f == "freq_hz" {
                r.get("freq_hz").or_else(|| r.get("fundamental_hz"))
            } else {
                r.get(f)
            };
            match key {
                Some(serde_json::Value::Number(n)) => row.push(n.to_string()),
                Some(serde_json::Value::String(s)) => row.push(s.clone()),
                _ => row.push(String::new()),
            }
        }
        wtr.write_record(&row).ok();
    }
    wtr.flush().ok();
    println!("  CSV  -> {}", path.display());
}

/// THD/THD+N percent expressed as dB relative to the total output.
///
/// `dB = 20·log10(pct/100)`. Percent compresses the interesting region near
/// the floor; the dB-re-total form is linear in the floor and is the
/// meaningful engineering figure. Returns `-` for a non-positive percent (no
/// valid measurement), where the dB form is undefined.
fn thd_db_re_total(pct: f64) -> String {
    if pct > 0.0 {
        format!("{:.1}", 20.0 * (pct / 100.0).log10())
    } else {
        "-".to_string()
    }
}

/// The fallback for a field an older daemon does not send, verbatim.
pub const NOT_REPORTED: &str = "(not reported by this daemon)";

/// What `noise_floor_dbfs` is measured against. It describes
/// `ac_core::measurement::thd::analyze`: the RMS of the residual left after
/// the fundamental and its harmonics are removed, with no weighting and no
/// band limit. If that computation ever gains weighting or a band limit,
/// this text — and the `noise_floor_dbfs` line in `ZMQ.md` — must change
/// with it.
const NOISE_FLOOR_BASIS: &str = "unweighted, full band";

/// Two leading spaces, then `label` left-aligned in 14 columns — the grid
/// `plot ir`'s read-out block uses, so the binary has one register.
fn label(l: &str) -> String {
    format!("  {l:<14}")
}

fn f64_of(r: &serde_json::Value, key: &str) -> Option<f64> {
    r.get(key).and_then(|v| v.as_f64())
}

fn bool_of(r: &serde_json::Value, key: &str) -> bool {
    r.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn points(n: usize) -> &'static str {
    if n == 1 {
        "point"
    } else {
        "points"
    }
}

/// One `%` / dB summary row: 5-wide qualifier, `%` value, then the dB value
/// in the column the noise floor shares.
fn pct_row(lbl: &str, qual: &str, pct: f64) -> String {
    format!(
        "{}{qual:<5}  {pct:>8.4} %  {:>6} dB re total",
        label(lbl),
        thd_db_re_total(pct)
    )
}

/// `captured` value: one figure when every point captured the same length,
/// else the `min–max` range; [`NOT_REPORTED`] when no frame carries
/// `capture_s` (a daemon older than #116). Never defaulted on this side:
/// the daemon owns the capture.
fn captured_value(results: &[serde_json::Value]) -> String {
    let caps: Vec<f64> = results
        .iter()
        .filter_map(|r| f64_of(r, "capture_s"))
        .collect();
    if caps.is_empty() {
        return NOT_REPORTED.to_string();
    }
    let lo = caps.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = caps.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let (lo_s, hi_s) = (format!("{lo:.2}"), format!("{hi:.2}"));
    if lo_s == hi_s {
        format!("{lo_s} s per point")
    } else {
        format!("{lo_s}\u{2013}{hi_s} s per point")
    }
}

fn range_line(lbl: &str, lo: f64, hi: f64) -> String {
    use ac_core::shared::conversions::{fmt_vrms, vrms_to_dbu};
    format!(
        "{}{:+.1} dBu  ->  {:+.1} dBu   ({} -> {})",
        label(lbl),
        vrms_to_dbu(lo),
        vrms_to_dbu(hi),
        fmt_vrms(lo),
        fmt_vrms(hi),
    )
}

/// The summary block printed after a `plot` / `plot level` table.
///
/// Worst and average figures come from the points that are neither clipped
/// nor AC-coupled; when every point is flagged they come from all points,
/// and the warning lines say so.
///
/// The noise-floor `worst` is the **highest** `noise_floor_dbfs` over that
/// same set — the least favourable floor. The sign runs opposite to a worst
/// THD, which is also the highest value but of a figure where lower is
/// better for the same reason.
pub fn summary_lines(
    results: &[serde_json::Value],
    device_name: &str,
    have_cal: bool,
) -> Vec<String> {
    if results.is_empty() {
        return Vec::new();
    }
    let n = results.len();

    let mut clean = Vec::new();
    let mut clipped_n = 0usize;
    let mut ac_n = 0usize;
    for r in results {
        let clip = bool_of(r, "clipping");
        let ac = bool_of(r, "ac_coupled");
        if clip {
            clipped_n += 1;
        }
        if ac {
            ac_n += 1;
        }
        if !clip && !ac {
            clean.push(r);
        }
    }
    let all_flagged = clean.is_empty();
    let valid: Vec<&serde_json::Value> = if all_flagged {
        results.iter().collect()
    } else {
        clean
    };

    let worst_thd = valid
        .iter()
        .filter_map(|r| f64_of(r, "thd_pct"))
        .fold(0.0_f64, f64::max);
    let worst_thdn = valid
        .iter()
        .filter_map(|r| f64_of(r, "thdn_pct"))
        .fold(0.0_f64, f64::max);
    let thds: Vec<f64> = valid.iter().filter_map(|r| f64_of(r, "thd_pct")).collect();
    let avg_thd = if thds.is_empty() {
        0.0
    } else {
        thds.iter().sum::<f64>() / thds.len() as f64
    };
    let worst_noise = valid
        .iter()
        .filter_map(|r| f64_of(r, "noise_floor_dbfs"))
        .fold(None, |acc: Option<f64>, v| {
            Some(acc.map_or(v, |a| a.max(v)))
        });

    let mut lines = vec![
        String::new(),
        format!(
            "{}{device_name} \u{00b7} {n} {}",
            label("summary"),
            points(n)
        ),
        String::new(),
        pct_row("THD", "worst", worst_thd),
        pct_row("", "avg", avg_thd),
        pct_row("THD+N", "worst", worst_thdn),
    ];
    lines.push(match worst_noise {
        Some(v) => format!(
            "{}{:<5}  {:10}  {v:>6.1} dBFS  {NOISE_FLOOR_BASIS}",
            label("noise floor"),
            "worst",
            ""
        ),
        None => format!("{}{NOT_REPORTED}", label("noise floor")),
    });
    lines.push(format!("{}{}", label("captured"), captured_value(results)));

    if have_cal {
        let lo = results.first().and_then(|r| f64_of(r, "out_vrms"));
        let hi = results.last().and_then(|r| f64_of(r, "out_vrms"));
        let ivs: Vec<f64> = results
            .iter()
            .filter_map(|r| f64_of(r, "in_vrms"))
            .collect();
        let mut range = Vec::new();
        if let (Some(lo), Some(hi)) = (lo, hi) {
            range.push(range_line("output", lo, hi));
        }
        if !ivs.is_empty() {
            let lo = ivs.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = ivs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            range.push(range_line("DUT out", lo, hi));
        }
        if !range.is_empty() {
            lines.push(String::new());
            lines.extend(range);
        }
    }

    let consequence = if all_flagged {
        "worst / avg include flagged points"
    } else {
        "excluded from worst / avg"
    };
    let mut warnings = Vec::new();
    for (count, what) in [(clipped_n, "clipped"), (ac_n, "AC-coupled")] {
        if count > 0 {
            warnings.push(format!(
                "{}{count} of {n} {} {what} \u{2014} {consequence}",
                label("warning"),
                points(n)
            ));
        }
    }
    if !warnings.is_empty() {
        lines.push(String::new());
        lines.extend(warnings);
    }
    lines.push(String::new());
    lines
}

pub fn print_summary(results: &[serde_json::Value], device_name: &str, have_cal: bool) {
    for line in summary_lines(results, device_name, have_cal) {
        println!("{line}");
    }
}

pub fn session_dir(name: &str) -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home)
        .join(".local/share/ac/sessions")
        .join(name)
}

pub fn output_dir(cfg: &ac_core::config::Config) -> std::path::PathBuf {
    if let Some(ref sess) = cfg.session {
        let d = session_dir(sess);
        std::fs::create_dir_all(&d).ok();
        d
    } else {
        std::path::PathBuf::from(".")
    }
}

pub fn timestamp() -> String {
    ac_core::shared::time::now_utc_filename_stamp()
}

/// The per-point table header: IEC caption, one blank line, then column
/// names over their units, both right-aligned to the column. Calibrated
/// tables show levels in dBu and the gain; uncalibrated ones the drive in
/// dBFS. `verbose` adds the per-point noise floor.
pub fn freq_header_lines(have_cal: bool, verbose: bool) -> Vec<String> {
    let (names, units) = if have_cal {
        let mut n = format!(
            "  {:>8}{:>10}{:>10}{:>9}{:>11}{:>11}",
            "freq", "out", "in", "gain", "THD", "THD+N"
        );
        let mut u = format!(
            "  {:>8}{:>10}{:>10}{:>9}{:>11}{:>11}",
            "Hz", "dBu", "dBu", "dB", "%", "%"
        );
        if verbose {
            n.push_str(&format!("{:>9}", "noise"));
            u.push_str(&format!("{:>9}", "dBFS"));
        }
        (n, u)
    } else {
        let mut n = format!(
            "  {:>10}{:>10}{:>11}{:>11}",
            "freq", "drive", "THD", "THD+N"
        );
        let mut u = format!("  {:>10}{:>10}{:>11}{:>11}", "Hz", "dBFS", "%", "%");
        if verbose {
            n.push_str(&format!("{:>12}", "noise"));
            u.push_str(&format!("{:>12}", "dBFS"));
        }
        (n, u)
    };
    vec![
        String::new(),
        "  THD, THD+N: residual / total output  (IEC 60268-3 \u{00a7}15.12.3.2)".to_string(),
        String::new(),
        names,
        units,
    ]
}

pub fn print_freq_header(have_cal: bool, verbose: bool) {
    for line in freq_header_lines(have_cal, verbose) {
        println!("{line}");
    }
}

/// One table row plus, when the point is clipped or AC-coupled, a
/// `warning` line of its own under it. Columns match
/// [`freq_header_lines`] for the same `have_cal` / `verbose`; a value the
/// frame lacks prints as `-`.
pub fn freq_row_lines(frame: &serde_json::Value, have_cal: bool, verbose: bool) -> Vec<String> {
    let freq = f64_of(frame, "freq_hz")
        .or_else(|| f64_of(frame, "fundamental_hz"))
        .unwrap_or(0.0);
    let thd = f64_of(frame, "thd_pct").unwrap_or(0.0);
    let thdn = f64_of(frame, "thdn_pct").unwrap_or(0.0);
    let fmt = |key: &str, f: &dyn Fn(f64) -> String| {
        f64_of(frame, key).map(f).unwrap_or_else(|| "-".into())
    };
    let noise = fmt("noise_floor_dbfs", &|v| format!("{v:.1}"));

    let mut row = if have_cal {
        let odbu = fmt("out_dbu", &|v| format!("{v:+.2}"));
        let idbu = fmt("in_dbu", &|v| format!("{v:+.2}"));
        let gain = fmt("gain_db", &|v| format!("{v:+.2}"));
        let mut row = format!("  {freq:>8.0}{odbu:>10}{idbu:>10}{gain:>9}{thd:>11.4}{thdn:>11.4}");
        if verbose {
            row.push_str(&format!("{noise:>9}"));
        }
        row
    } else {
        let drive = fmt("drive_db", &|v| format!("{v:.1}"));
        let mut row = format!("  {freq:>10.0}{drive:>10}{thd:>11.4}{thdn:>11.4}");
        if verbose {
            row.push_str(&format!("{noise:>12}"));
        }
        row
    };

    let mut lines = vec![row];
    let flags = match (bool_of(frame, "clipping"), bool_of(frame, "ac_coupled")) {
        (true, true) => Some("clipped, AC-coupled"),
        (true, false) => Some("clipped"),
        (false, true) => Some("AC-coupled"),
        (false, false) => None,
    };
    if let Some(f) = flags {
        lines.push(format!(
            "{}{f} \u{2014} point excluded from summary",
            label("warning")
        ));
    }
    lines
}

pub fn print_freq_row(frame: &serde_json::Value, have_cal: bool, verbose: bool) {
    for line in freq_row_lines(frame, have_cal, verbose) {
        println!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn cols(line: &str) -> usize {
        line.chars().count()
    }

    fn uncal_point(freq: f64, thd: f64, noise: f64, capture: f64) -> Value {
        json!({
            "freq_hz": freq, "drive_db": -10.0, "thd_pct": thd, "thdn_pct": thd * 2.0,
            "noise_floor_dbfs": noise, "capture_s": capture,
            "clipping": false, "ac_coupled": false,
        })
    }

    /// Worst-case calibrated point: widest values in every column.
    fn cal_point(clip: bool, ac: bool) -> Value {
        json!({
            "freq_hz": 20000.0, "drive_db": 0.0, "thd_pct": 100.0, "thdn_pct": 100.0,
            "noise_floor_dbfs": -142.7, "capture_s": 0.15,
            "out_vrms": 12.3, "out_dbu": 24.0, "in_vrms": 0.009504, "in_dbu": -38.2,
            "gain_db": -62.2, "clipping": clip, "ac_coupled": ac,
        })
    }

    fn mixed_run(cal: bool) -> Vec<Value> {
        let mut v: Vec<Value> = (0..97)
            .map(|i| {
                if cal {
                    cal_point(false, false)
                } else {
                    uncal_point(20.0 * (i + 1) as f64, 0.0023, -94.1, 1.0)
                }
            })
            .collect();
        v.push(if cal {
            cal_point(true, false)
        } else {
            json!({"clipping": true})
        });
        v.push(if cal {
            cal_point(false, true)
        } else {
            json!({"ac_coupled": true})
        });
        v
    }

    #[test]
    fn every_summary_and_table_line_fits_80_columns() {
        for cal in [false, true] {
            let run = mixed_run(cal);
            for line in summary_lines(&run, "DUT", cal) {
                assert!(cols(&line) <= 80, "{} cols: {line:?}", cols(&line));
            }
            // Every point flagged: the longer "include flagged points" form.
            let flagged: Vec<Value> = (0..99).map(|_| cal_point(true, true)).collect();
            for line in summary_lines(&flagged, "DUT", cal) {
                assert!(cols(&line) <= 80, "{} cols: {line:?}", cols(&line));
            }
            for verbose in [false, true] {
                for line in freq_header_lines(cal, verbose) {
                    assert!(cols(&line) <= 80, "{line:?}");
                }
                for line in freq_row_lines(&cal_point(true, true), cal, verbose) {
                    assert!(cols(&line) <= 80, "{line:?}");
                }
            }
        }
    }

    #[test]
    fn no_rules_or_shouted_labels_remain() {
        let run = mixed_run(true);
        let mut all = summary_lines(&run, "DUT", true);
        all.extend(freq_header_lines(true, true));
        all.extend(freq_row_lines(&cal_point(true, true), true, true));
        for line in &all {
            assert!(
                !line.contains('\u{2500}') && !line.contains("=="),
                "{line:?}"
            );
            assert!(
                !line.contains("SUMMARY") && !line.contains("[CLIP]"),
                "{line:?}"
            );
            assert!(!line.contains("[AC]") && !line.contains("valid points only"));
        }
    }

    #[test]
    fn table_columns_line_up_with_header() {
        for (cal, verbose) in [(false, false), (false, true), (true, false), (true, true)] {
            let header = freq_header_lines(cal, verbose);
            let point = if cal {
                cal_point(false, false)
            } else {
                uncal_point(20000.0, 0.0412, -142.7, 1.0)
            };
            let row = &freq_row_lines(&point, cal, verbose)[0];
            assert_eq!(cols(&header[3]), cols(row), "{header:?} / {row:?}");
            assert_eq!(cols(&header[4]), cols(row), "{header:?} / {row:?}");
            assert_eq!(header[4].trim_end().ends_with("dBFS"), verbose);
        }
        let row = freq_row_lines(&uncal_point(20.0, 0.0041, -96.2, 1.0), false, true);
        assert_eq!(
            row,
            vec!["          20     -10.0     0.0041     0.0082       -96.2"]
        );
    }

    #[test]
    fn flagged_point_gets_its_own_warning_line() {
        let lines = freq_row_lines(&cal_point(true, false), true, false);
        assert_eq!(lines.len(), 2);
        assert!(!lines[0].contains("clipped"));
        assert_eq!(
            lines[1],
            "  warning       clipped \u{2014} point excluded from summary"
        );
        let lines = freq_row_lines(&cal_point(true, true), true, false);
        assert_eq!(
            lines[1],
            "  warning       clipped, AC-coupled \u{2014} point excluded from summary"
        );
        assert_eq!(
            freq_row_lines(&cal_point(false, false), true, false).len(),
            1
        );
    }

    #[test]
    fn noise_floor_worst_is_the_highest_over_valid_points() {
        let run = vec![
            uncal_point(20.0, 0.004, -96.2, 1.0),
            uncal_point(200.0, 0.002, -94.1, 1.0),
            uncal_point(2000.0, 0.002, -101.4, 1.0),
            // Flagged: excluded, so its higher floor must not win.
            json!({"thd_pct": 0.04, "noise_floor_dbfs": -71.9, "clipping": true}),
        ];
        let lines = summary_lines(&run, "DUT", false);
        let noise = lines
            .iter()
            .find(|l| l.starts_with("  noise floor"))
            .expect("noise floor line");
        assert_eq!(
            noise,
            "  noise floor   worst               -94.1 dBFS  unweighted, full band"
        );
    }

    #[test]
    fn noise_floor_shares_the_db_column() {
        let lines = summary_lines(&[uncal_point(20.0, 0.0023, -94.1, 1.0)], "DUT", false);
        let thd = lines.iter().find(|l| l.starts_with("  THD ")).unwrap();
        let noise = lines
            .iter()
            .find(|l| l.starts_with("  noise floor"))
            .unwrap();
        assert_eq!(thd, "  THD           worst    0.0023 %   -92.8 dB re total");
        assert_eq!(thd.find(" dB re").unwrap(), noise.find(" dBFS").unwrap());
    }

    #[test]
    fn captured_reads_one_value_a_range_or_not_reported() {
        let find = |run: &[Value]| {
            summary_lines(run, "DUT", false)
                .into_iter()
                .find(|l| l.starts_with("  captured"))
                .expect("captured line is never omitted")
        };
        let same = [
            uncal_point(20.0, 0.002, -94.0, 1.0),
            uncal_point(2e3, 0.002, -94.0, 1.0),
        ];
        assert_eq!(find(&same), "  captured      1.00 s per point");
        let varied = [
            uncal_point(20.0, 0.002, -94.0, 0.15),
            uncal_point(2e3, 0.002, -94.0, 0.1),
        ];
        assert_eq!(
            find(&varied),
            "  captured      0.10\u{2013}0.15 s per point"
        );
        let old = [json!({"thd_pct": 0.002, "noise_floor_dbfs": -94.0})];
        assert_eq!(find(&old), format!("  captured      {NOT_REPORTED}"));
    }

    #[test]
    fn summary_warnings_count_out_of_total_and_change_when_all_flagged() {
        let mut run: Vec<Value> = (0..7)
            .map(|i| uncal_point(20.0 * (i + 1) as f64, 0.002, -94.0, 1.0))
            .collect();
        run.push(json!({"thd_pct": 5.0, "clipping": true}));
        run.push(json!({"thd_pct": 5.0, "clipping": true, "ac_coupled": true}));
        let lines = summary_lines(&run, "DUT", false);
        assert_eq!(lines[1], "  summary       DUT \u{00b7} 9 points");
        let warnings: Vec<&String> = lines
            .iter()
            .filter(|l| l.starts_with("  warning"))
            .collect();
        assert_eq!(
            warnings,
            vec![
                "  warning       2 of 9 points clipped \u{2014} excluded from worst / avg",
                "  warning       1 of 9 points AC-coupled \u{2014} excluded from worst / avg",
            ]
        );
        // Warnings come last, after one blank line.
        let first_warn = lines
            .iter()
            .position(|l| l.starts_with("  warning"))
            .unwrap();
        assert_eq!(lines[first_warn - 1], "");
        assert!(lines[first_warn..]
            .iter()
            .all(|l| l.is_empty() || l.starts_with("  warning")));

        let all: Vec<Value> = (0..9)
            .map(|_| json!({"thd_pct": 1.0, "clipping": true}))
            .collect();
        let lines = summary_lines(&all, "DUT", false);
        assert!(lines.contains(
            &"  warning       9 of 9 points clipped \u{2014} worst / avg include flagged points"
                .to_string()
        ));
    }

    #[test]
    fn calibrated_summary_puts_dbu_before_vrms() {
        let run = vec![cal_point(false, false)];
        let lines = summary_lines(&run, "DUT", true);
        assert!(lines.contains(
            &"  DUT out       -38.2 dBu  ->  -38.2 dBu   (9.504 mVrms -> 9.504 mVrms)".to_string()
        ));
        assert!(lines
            .iter()
            .any(|l| l.starts_with("  output        +24.0 dBu")));
    }

    #[test]
    fn thd_db_re_total_matches_formula() {
        // 100% THD is equal to the total output -> 0 dB re total.
        assert_eq!(thd_db_re_total(100.0), "0.0");
        // 1% -> 20*log10(0.01) = -40 dB.
        assert_eq!(thd_db_re_total(1.0), "-40.0");
        // Spec example: 0.0042% -> -87.5 dB re total (1 decimal).
        assert_eq!(thd_db_re_total(0.0042), "-87.5");
    }

    #[test]
    fn thd_db_re_total_dash_for_non_positive() {
        // dB form is undefined for a non-positive percent (no valid point).
        assert_eq!(thd_db_re_total(0.0), "-");
        assert_eq!(thd_db_re_total(-1.0), "-");
    }
}
