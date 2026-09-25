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

/// Width the verb pads to in the run header: the longest measuring verb,
/// `test hardware`. Keeps every header's stamp in one column (#127 UX).
const RUN_HEADER_VERB_WIDTH: usize = 13;

/// The run header (#127): `ac`, the verb as typed padded to
/// [`RUN_HEADER_VERB_WIDTH`], then the run's start stamp. Column 0, so it
/// is the one line above the body's two-space indent.
pub fn run_header_line(verb: &str, ts: &str) -> String {
    format!("ac  {verb:<RUN_HEADER_VERB_WIDTH$}   {ts}")
}

/// Print the run header, stamped now with
/// [`ac_core::shared::time::now_utc_iso8601`] — the display format, never
/// the filename stamp [`timestamp`] — and return the stamp printed. The
/// stamp is the client's start instant, not the report's `timestamp_utc`.
/// Only the header line prints: `plot*` follow it with a blank line of
/// their own, while `test *` go straight into a block that already opens
/// with one, so either way exactly one blank line sits under it.
pub fn print_run_header(verb: &str) -> String {
    let ts = ac_core::shared::time::now_utc_iso8601();
    println!("{}", run_header_line(verb, &ts));
    ts
}

fn f64_of(r: &serde_json::Value, key: &str) -> Option<f64> {
    r.get(key).and_then(|v| v.as_f64())
}

fn bool_of(r: &serde_json::Value, key: &str) -> bool {
    r.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

/// A warning line in the post-table register: the `warning` label, then
/// the message. The one form every run warning in `ac-cli` takes.
pub fn warning_line(msg: &str) -> String {
    format!("{}{msg}", label("warning"))
}

/// `1 xrun` / `{n} xruns`: the count and the condition, nothing else.
pub fn xruns_text(n: u64) -> String {
    if n == 1 {
        "1 xrun".to_string()
    } else {
        format!("{n} xruns")
    }
}

/// The warning line for a run's xrun count, or `None` for a clean run —
/// zero xruns prints nothing.
pub fn xrun_warning_line(xruns: u64) -> Option<String> {
    (xruns > 0).then(|| warning_line(&xruns_text(xruns)))
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
/// nor AC-coupled; when every point is flagged they come from all points.
/// Each warning line gives only a count and a condition. Order: the clip
/// and AC-coupled warnings, then one continuation line under them stating
/// how many points the figures used, then the xrun warning. The xrun line
/// excludes no point, so an xrun-only run has no continuation line. With
/// no results, only the xrun warning (if any) is returned, so a path that
/// collected no points still reports its xruns.
///
/// The noise-floor `worst` is the **highest** `noise_floor_dbfs` over that
/// same set — the least favourable floor. The sign runs opposite to a worst
/// THD, which is also the highest value but of a figure where lower is
/// better for the same reason.
pub fn summary_lines(
    results: &[serde_json::Value],
    device_name: &str,
    have_cal: bool,
    xruns: u64,
) -> Vec<String> {
    let xrun_line = xrun_warning_line(xruns);
    if results.is_empty() {
        return match xrun_line {
            Some(l) => vec![String::new(), l, String::new()],
            None => Vec::new(),
        };
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
    let clean_n = clean.len();
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

    let mut warnings = Vec::new();
    for (count, what) in [(clipped_n, "clipped"), (ac_n, "AC-coupled")] {
        if count > 0 {
            warnings.push(warning_line(&format!(
                "{count} of {n} {} {what}",
                points(n)
            )));
        }
    }
    let has_flag_warnings = !warnings.is_empty();
    if has_flag_warnings {
        lines.push(String::new());
        lines.extend(warnings);
        // One continuation line states the basis of every figure above.
        // `clean_n` counts points with neither flag, so overlapping flags
        // are not double-subtracted.
        lines.push(if all_flagged {
            format!(
                "{}worst / avg include all {n} flagged {}",
                label(""),
                points(n)
            )
        } else {
            format!(
                "{}worst / avg from the {clean_n} unflagged {}",
                label(""),
                points(clean_n)
            )
        });
    }
    if let Some(l) = xrun_line {
        if !has_flag_warnings {
            lines.push(String::new());
        }
        lines.push(l);
    }
    lines.push(String::new());
    lines
}

pub fn print_summary(results: &[serde_json::Value], device_name: &str, have_cal: bool, xruns: u64) {
    for line in summary_lines(results, device_name, have_cal, xruns) {
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
/// dBFS. `verbose` adds the per-point fundamental level (`fund`, the
/// reference of the harmonic table) and noise floor, both dBFS.
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
            n.push_str(&format!("{:>9}{:>9}", "fund", "noise"));
            u.push_str(&format!("{:>9}{:>9}", "dBFS", "dBFS"));
        }
        (n, u)
    } else {
        let mut n = format!(
            "  {:>10}{:>10}{:>11}{:>11}",
            "freq", "drive", "THD", "THD+N"
        );
        let mut u = format!("  {:>10}{:>10}{:>11}{:>11}", "Hz", "dBFS", "%", "%");
        if verbose {
            n.push_str(&format!("{:>12}{:>12}", "fund", "noise"));
            u.push_str(&format!("{:>12}{:>12}", "dBFS", "dBFS"));
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
    let fund = fmt("fundamental_dbfs", &|v| format!("{v:.1}"));

    let row = if have_cal {
        let odbu = fmt("out_dbu", &|v| format!("{v:+.2}"));
        let idbu = fmt("in_dbu", &|v| format!("{v:+.2}"));
        let gain = fmt("gain_db", &|v| format!("{v:+.2}"));
        let mut row = format!("  {freq:>8.0}{odbu:>10}{idbu:>10}{gain:>9}{thd:>11.4}{thdn:>11.4}");
        if verbose {
            row.push_str(&format!("{fund:>9}{noise:>9}"));
        }
        row
    } else {
        let drive = fmt("drive_db", &|v| format!("{v:.1}"));
        let mut row = format!("  {freq:>10.0}{drive:>10}{thd:>11.4}{thdn:>11.4}");
        if verbose {
            row.push_str(&format!("{fund:>12}{noise:>12}"));
        }
        row
    };

    let mut lines = vec![row];
    if let Some(f) = point_flags(frame) {
        lines.push(warning_line(f));
    }
    lines
}

/// The per-point flag text a table prints on a `warning` line under a
/// clipped and/or AC-coupled row; `None` for a clean point.
fn point_flags(frame: &serde_json::Value) -> Option<&'static str> {
    match (bool_of(frame, "clipping"), bool_of(frame, "ac_coupled")) {
        (true, true) => Some("clipped, AC-coupled"),
        (true, false) => Some("clipped"),
        (false, true) => Some("AC-coupled"),
        (false, false) => None,
    }
}

/// Harmonics the harmonic table shows: H2..=H11, the ten `thd::analyze`
/// tracks for the point path.
const HARMONIC_COLUMNS: usize = 10;

/// The swept variable a harmonic-table row is keyed by — the same column,
/// in the same format, as the main table, so a row reads on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HarmonicKey {
    /// `plot`: fundamental frequency, Hz.
    Freq,
    /// Uncalibrated `plot level`: drive, dBFS.
    Drive,
    /// Calibrated `plot level`: output level, dBu.
    OutDbu,
}

/// One harmonic cell, dB re fundamental. `None` is a harmonic absent from
/// `harmonic_levels` (above Nyquist): the cell stays blank. `-` is a
/// harmonic searched for and not found (`amp <= 0`), one whose level sits
/// at the [`MIN_DBFS`](ac_core::shared::reference_levels::MIN_DBFS) clamp,
/// or any harmonic on a row with no `fundamental_dbfs` — a clamp value is
/// never printed as a reading.
fn harmonic_cell(entry: Option<&serde_json::Value>, fund_dbfs: Option<f64>) -> String {
    use ac_core::shared::reference_levels::{amplitude_to_dbfs, MIN_DBFS};
    let Some(entry) = entry else {
        return String::new();
    };
    let amp = entry.get(1).and_then(|v| v.as_f64());
    match (amp, fund_dbfs) {
        (Some(amp), Some(fund)) if amp > 0.0 => {
            let dbfs = amplitude_to_dbfs(amp);
            if dbfs <= MIN_DBFS {
                "-".to_string()
            } else {
                format!("{:.1}", dbfs - fund)
            }
        }
        _ => "-".to_string(),
    }
}

/// The harmonic table printed after a completed `plot` / `plot level
/// --verbose` run, before the summary: one row per point, keyed by `key`,
/// then H2–H11 in dB re that point's `fundamental_dbfs`. `harmonic_levels`
/// carries linear amplitudes (`ZMQ.md`), converted with the same
/// `amplitude_to_dbfs` `thd::analyze` uses for the fundamental. Entry *i*
/// is H(*i*+2); cells follow [`harmonic_cell`] and trailing blanks are
/// trimmed. A flagged point gets the main table's `warning` line under its
/// row. No results prints nothing; results with no `harmonic_levels` on
/// any frame print a single [`NOT_REPORTED`] line.
pub fn harmonic_table_lines(results: &[serde_json::Value], key: HarmonicKey) -> Vec<String> {
    if results.is_empty() {
        return Vec::new();
    }
    if !results.iter().any(|r| r.get("harmonic_levels").is_some()) {
        return vec![
            String::new(),
            format!("{}{NOT_REPORTED}", label("harmonics")),
        ];
    }
    let (key_name, key_unit) = match key {
        HarmonicKey::Freq => ("freq", "Hz"),
        HarmonicKey::Drive => ("drive", "dBFS"),
        HarmonicKey::OutDbu => ("out", "dBu"),
    };
    let mut names = format!("  {key_name:>7}");
    let mut units = format!("  {key_unit:>7}");
    for h in 0..HARMONIC_COLUMNS {
        names.push_str(&format!("{:>7}", format!("H{}", h + 2)));
        units.push_str(&format!("{:>7}", "dB"));
    }
    let mut lines = vec![
        String::new(),
        format!("{}level re fundamental", label("harmonics")),
        String::new(),
        names,
        units,
    ];
    for r in results {
        let key_text = match key {
            HarmonicKey::Freq => f64_of(r, "freq_hz")
                .or_else(|| f64_of(r, "fundamental_hz"))
                .map(|v| format!("{v:.0}")),
            HarmonicKey::Drive => f64_of(r, "drive_db").map(|v| format!("{v:.1}")),
            HarmonicKey::OutDbu => f64_of(r, "out_dbu").map(|v| format!("{v:+.2}")),
        }
        .unwrap_or_else(|| "-".into());
        let fund = f64_of(r, "fundamental_dbfs");
        let levels = r.get("harmonic_levels").and_then(|v| v.as_array());
        let mut row = format!("  {key_text:>7}");
        for h in 0..HARMONIC_COLUMNS {
            let cell = match levels {
                Some(l) => harmonic_cell(l.get(h), fund),
                // This frame reports no harmonics at all: not the same as
                // above Nyquist, so every cell reads `-`.
                None => "-".to_string(),
            };
            row.push_str(&format!("{cell:>7}"));
        }
        lines.push(row.trim_end().to_string());
        if let Some(f) = point_flags(r) {
            lines.push(warning_line(f));
        }
    }
    lines
}

pub fn print_harmonic_table(results: &[serde_json::Value], key: HarmonicKey) {
    for line in harmonic_table_lines(results, key) {
        println!("{line}");
    }
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
            for line in summary_lines(&run, "DUT", cal, 0) {
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

    /// MAX_SWEEP_POINTS is 10 000 (ac-daemon handlers/mod.rs); `plot level`
    /// at 20 Hz with >= 100 steps into an AC-coupled DUT flags every point.
    #[test]
    fn summary_fits_80_columns_up_to_max_sweep_points() {
        let flag_sets: [&[&str]; 3] = [&["clipping"], &["ac_coupled"], &["clipping", "ac_coupled"]];
        for n in [1usize, 99, 100, 1_000, 10_000] {
            for flags in flag_sets {
                // All flagged, then all but one flagged.
                for clean in [0usize, 1] {
                    let mut run: Vec<Value> = (0..n)
                        .map(|_| {
                            let mut p = json!({"thd_pct": 1.0, "noise_floor_dbfs": -90.0});
                            for f in flags {
                                p[*f] = json!(true);
                            }
                            p
                        })
                        .collect();
                    for p in run.iter_mut().take(clean) {
                        for f in flags {
                            p[*f] = json!(false);
                        }
                    }
                    for cal in [false, true] {
                        for line in summary_lines(&run, "DUT", cal, 0) {
                            assert!(
                                cols(&line) <= 80,
                                "n={n} {flags:?} clean={clean}: {} cols: {line:?}",
                                cols(&line)
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn header_rows_match_the_ux_mockup() {
        let h = freq_header_lines(false, true);
        assert_eq!(
            h[3],
            "        freq     drive        THD      THD+N        fund       noise"
        );
        assert_eq!(
            h[4],
            "          Hz      dBFS          %          %        dBFS        dBFS"
        );
        let h = freq_header_lines(true, true);
        assert_eq!(
            h[3],
            "      freq       out        in     gain        THD      THD+N     fund    noise"
        );
        assert_eq!(
            h[4],
            "        Hz       dBu       dBu       dB          %          %     dBFS     dBFS"
        );
    }

    #[test]
    fn no_rules_or_shouted_labels_remain() {
        let run = mixed_run(true);
        let mut all = summary_lines(&run, "DUT", true, 0);
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
            vec!["          20     -10.0     0.0041     0.0082           -       -96.2"]
        );
    }

    /// Linear peak amplitude of a level in dBFS — the wire form of
    /// `harmonic_levels` entries.
    fn amp(dbfs: f64) -> f64 {
        10f64.powf(dbfs / 20.0)
    }

    /// A point frame carrying `fundamental_dbfs` and `harmonic_levels`.
    fn harm_point(key: Value, fund: f64, harm_dbfs: &[Option<f64>]) -> Value {
        let mut p = key;
        p["fundamental_dbfs"] = json!(fund);
        p["harmonic_levels"] = json!(harm_dbfs
            .iter()
            .enumerate()
            .map(|(i, d)| json!([1000.0 * (i + 2) as f64, d.map_or(0.0, amp)]))
            .collect::<Vec<_>>());
        p
    }

    /// Cell `h` (0 = H2) of a harmonic-table row, split on whitespace.
    fn cell(row: &str, h: usize) -> &str {
        row.split_whitespace().nth(h + 1).unwrap()
    }

    #[test]
    fn harmonic_is_re_fundamental_not_linear_rms() {
        let mut p = harm_point(
            json!({"drive_db": -10.0, "freq_hz": 1000.0, "thd_pct": 1.0, "thdn_pct": 1.0}),
            -10.0,
            &[Some(-50.0)],
        );
        let linear_rms = amp(-10.0) / 2f64.sqrt();
        p["linear_rms"] = json!(linear_rms);
        let lines = harmonic_table_lines(&[p.clone()], HarmonicKey::Drive);
        let row = lines.last().unwrap();
        assert_eq!(cell(row, 0), "-40.0");
        // The rejected reference: H2 against `linear_rms` in dB.
        let rejected = format!("{:.1}", -50.0 - 20.0 * linear_rms.log10());
        assert_eq!(rejected, "-37.0");
        assert_ne!(cell(row, 0), rejected);
        let main = &freq_row_lines(&p, false, true)[0];
        assert_eq!(main.split_whitespace().nth(4), Some("-10.0"));
    }

    #[test]
    fn floor_clamped_or_unfound_harmonic_reads_dash() {
        let raw = -274.7;
        let mut h = vec![Some(-60.0); 10];
        h[6] = Some(raw); // H8
        h[3] = None; // H5, amp 0.0: not found
        let p = harm_point(json!({"drive_db": -20.0}), -20.0, &h);
        let lines = harmonic_table_lines(&[p], HarmonicKey::Drive);
        let row = lines.last().unwrap();
        // The rejected value: the clamp printed as a reading.
        let rejected = format!(
            "{:.1}",
            ac_core::shared::reference_levels::amplitude_to_dbfs(amp(raw)) - -20.0
        );
        assert_eq!(rejected, "-180.0");
        assert_eq!(cell(row, 6), "-");
        assert_eq!(cell(row, 3), "-");
        assert_eq!(cell(row, 0), "-40.0");
        assert!(!row.contains(&rejected), "{row:?}");

        // No fundamental on the row: every cell is `-`.
        let mut p = harm_point(json!({"drive_db": -20.0}), -20.0, &[Some(-60.0); 10]);
        p.as_object_mut().unwrap().remove("fundamental_dbfs");
        let row = harmonic_table_lines(&[p], HarmonicKey::Drive)
            .pop()
            .unwrap();
        assert_eq!(row.split_whitespace().filter(|c| *c == "-").count(), 10);
    }

    #[test]
    fn above_nyquist_cells_are_blank_and_trimmed() {
        let run = vec![
            harm_point(json!({"freq_hz": 5000.0}), -10.0, &[Some(-96.1); 4]),
            harm_point(json!({"freq_hz": 20000.0}), -10.0, &[]),
        ];
        let lines = harmonic_table_lines(&run, HarmonicKey::Freq);
        let n = lines.len();
        assert_eq!(lines[n - 2], "     5000  -86.1  -86.1  -86.1  -86.1");
        assert_eq!(lines[n - 1], "    20000");
        for l in &lines {
            assert_eq!(l.trim_end(), l, "{l:?}");
        }
    }

    #[test]
    fn harmonic_table_matches_the_ux_mockup_and_flags_rows() {
        let mut p = harm_point(
            json!({"drive_db": -30.0}),
            -30.0,
            &[
                Some(-116.4),
                Some(-120.2),
                Some(-131.7),
                Some(-134.9),
                Some(-139.3),
                Some(-140.8),
                Some(-141.5),
                None,
                Some(-142.0),
                Some(-143.1),
            ],
        );
        p["clipping"] = json!(true);
        let lines = harmonic_table_lines(&[p], HarmonicKey::Drive);
        assert_eq!(
            lines,
            vec![
                "",
                "  harmonics     level re fundamental",
                "",
                "    drive     H2     H3     H4     H5     H6     H7     H8     H9    H10    H11",
                "     dBFS     dB     dB     dB     dB     dB     dB     dB     dB     dB     dB",
                "    -30.0  -86.4  -90.2 -101.7 -104.9 -109.3 -110.8 -111.5      - -112.0 -113.1",
                "  warning       clipped",
            ]
        );
    }

    #[test]
    fn worst_case_harmonic_and_fund_rows_fit_80_columns() {
        let mut p = cal_point(true, true);
        p["fundamental_dbfs"] = json!(-0.4);
        let h = harm_point(p.clone(), -0.4, &[Some(-122.3); 10]);
        let lines = harmonic_table_lines(&[h], HarmonicKey::OutDbu);
        assert_eq!(
            lines[5],
            "   +24.00 -121.9 -121.9 -121.9 -121.9 -121.9 -121.9 -121.9 -121.9 -121.9 -121.9"
        );
        let mut all = lines;
        all.extend(freq_header_lines(true, true));
        all.extend(freq_row_lines(&p, true, true));
        for l in &all {
            assert!(cols(l) <= 80, "{} cols: {l:?}", cols(l));
        }
    }

    #[test]
    fn harmonic_table_not_reported_or_empty() {
        let old = [json!({"freq_hz": 1000.0, "thd_pct": 0.01})];
        assert_eq!(
            harmonic_table_lines(&old, HarmonicKey::Freq),
            vec![String::new(), format!("  harmonics     {NOT_REPORTED}")]
        );
        assert!(harmonic_table_lines(&[], HarmonicKey::Freq).is_empty());
    }

    /// `verbose = false` output is unchanged by #132: no `fund` column.
    #[test]
    fn non_verbose_table_is_unchanged() {
        let h = freq_header_lines(false, false);
        assert_eq!(h[3], "        freq     drive        THD      THD+N");
        let mut p = uncal_point(20.0, 0.0041, -96.2, 1.0);
        p["fundamental_dbfs"] = json!(-10.0);
        assert_eq!(
            freq_row_lines(&p, false, false),
            vec!["          20     -10.0     0.0041     0.0082"]
        );
    }

    #[test]
    fn flagged_point_gets_its_own_warning_line() {
        let lines = freq_row_lines(&cal_point(true, false), true, false);
        assert_eq!(lines.len(), 2);
        assert!(!lines[0].contains("clipped"));
        assert_eq!(lines[1], "  warning       clipped");
        let lines = freq_row_lines(&cal_point(true, true), true, false);
        assert_eq!(lines[1], "  warning       clipped, AC-coupled");
        let lines = freq_row_lines(&cal_point(false, true), false, true);
        assert_eq!(lines[1], "  warning       AC-coupled");
        // A row cannot know whether its point ends up in the summary.
        for (clip, ac) in [(true, false), (false, true), (true, true)] {
            for (cal, verbose) in [(false, false), (true, true)] {
                for line in freq_row_lines(&cal_point(clip, ac), cal, verbose) {
                    assert!(!line.contains("summary"), "{line:?}");
                }
            }
        }
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
        let lines = summary_lines(&run, "DUT", false, 0);
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
        let lines = summary_lines(&[uncal_point(20.0, 0.0023, -94.1, 1.0)], "DUT", false, 0);
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
            summary_lines(run, "DUT", false, 0)
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
    fn summary_warnings_count_out_of_total_then_state_the_basis() {
        // 9 points: 2 clipped, 1 AC-coupled, one point carrying both.
        let mut run: Vec<Value> = (0..7)
            .map(|i| uncal_point(20.0 * (i + 1) as f64, 0.002, -94.0, 1.0))
            .collect();
        run.push(json!({"thd_pct": 5.0, "clipping": true}));
        run.push(json!({"thd_pct": 5.0, "clipping": true, "ac_coupled": true}));
        let lines = summary_lines(&run, "DUT", false, 0);
        assert_eq!(lines[1], "  summary       DUT \u{00b7} 9 points");
        // Warnings and their continuation come last, after one blank line.
        let first_warn = lines
            .iter()
            .position(|l| l.starts_with("  warning"))
            .unwrap();
        assert_eq!(lines[first_warn - 1], "");
        assert_eq!(
            lines[first_warn..],
            [
                "  warning       2 of 9 points clipped",
                "  warning       1 of 9 points AC-coupled",
                "                worst / avg from the 7 unflagged points",
                "",
            ]
        );
        // The rejected count, n − clipped − ac, differs on this overlapping run.
        let flagged = |k: &str| run.iter().filter(|r| bool_of(r, k)).count();
        let rejected = run.len() - flagged("clipping") - flagged("ac_coupled");
        assert_eq!(rejected, 6);
        assert!(!lines
            .iter()
            .any(|l| l.contains(&format!("the {rejected} unflagged"))));

        let all: Vec<Value> = (0..9)
            .map(|_| json!({"thd_pct": 1.0, "clipping": true}))
            .collect();
        let lines = summary_lines(&all, "DUT", false, 0);
        let first_warn = lines
            .iter()
            .position(|l| l.starts_with("  warning"))
            .unwrap();
        assert_eq!(
            lines[first_warn..],
            [
                "  warning       9 of 9 points clipped",
                "                worst / avg include all 9 flagged points",
                "",
            ]
        );

        // Singular forms.
        let mut one_clean: Vec<Value> = (0..3)
            .map(|_| json!({"thd_pct": 1.0, "ac_coupled": true}))
            .collect();
        one_clean.push(uncal_point(1e3, 0.002, -94.0, 1.0));
        let lines = summary_lines(&one_clean, "DUT", false, 0);
        assert!(
            lines.contains(&"                worst / avg from the 1 unflagged point".to_string())
        );
        let single = [json!({"thd_pct": 1.0, "ac_coupled": true})];
        let lines = summary_lines(&single, "DUT", false, 0);
        assert!(lines.contains(&"  warning       1 of 1 point AC-coupled".to_string()));
        assert!(
            lines.contains(&"                worst / avg include all 1 flagged point".to_string())
        );

        // No flags: no warning lines and no continuation line.
        let lines = summary_lines(&run[..7], "DUT", false, 0);
        assert!(!lines
            .iter()
            .any(|l| l.contains("warning") || l.contains("worst / avg")));
    }

    /// #130: an xrun-only run gets the xrun warning after one blank line
    /// and no basis line — an xrun excludes no point.
    #[test]
    fn summary_xrun_only_has_no_basis_line() {
        let run: Vec<Value> = (0..3)
            .map(|i| uncal_point(1e3 * (i + 1) as f64, 0.002, -94.0, 1.0))
            .collect();
        let lines = summary_lines(&run, "DUT", false, 1);
        let first_warn = lines
            .iter()
            .position(|l| l.starts_with("  warning"))
            .unwrap();
        assert_eq!(lines[first_warn - 1], "");
        assert_eq!(lines[first_warn..], ["  warning       1 xrun", ""]);
        assert!(!lines.iter().any(|l| l.contains("worst / avg")));

        // Zero xruns prints nothing xrun-shaped.
        let clean = summary_lines(&run, "DUT", false, 0);
        assert!(!clean.iter().any(|l| l.contains("xrun")));
    }

    /// #130: with clip warnings, the xrun line comes after the basis line.
    #[test]
    fn summary_clip_and_xrun_puts_xrun_last() {
        let mut run: Vec<Value> = (0..7)
            .map(|i| uncal_point(1e3 * (i + 1) as f64, 0.002, -94.0, 1.0))
            .collect();
        run.push(json!({"thd_pct": 5.0, "clipping": true}));
        run.push(json!({"thd_pct": 5.0, "clipping": true}));
        let lines = summary_lines(&run, "DUT", false, 3);
        let first_warn = lines
            .iter()
            .position(|l| l.starts_with("  warning"))
            .unwrap();
        assert_eq!(lines[first_warn - 1], "");
        assert_eq!(
            lines[first_warn..],
            [
                "  warning       2 of 9 points clipped",
                "                worst / avg from the 7 unflagged points",
                "  warning       3 xruns",
                "",
            ]
        );
    }

    /// #130: no results still reports the xruns, and nothing else.
    #[test]
    fn summary_empty_results_keeps_xrun_line() {
        assert_eq!(
            summary_lines(&[], "DUT", false, 3),
            ["", "  warning       3 xruns", ""]
        );
        assert!(summary_lines(&[], "DUT", false, 0).is_empty());
    }

    #[test]
    fn calibrated_summary_puts_dbu_before_vrms() {
        let run = vec![cal_point(false, false)];
        let lines = summary_lines(&run, "DUT", true, 0);
        assert!(lines.contains(
            &"  DUT out       -38.2 dBu  ->  -38.2 dBu   (9.504 mVrms -> 9.504 mVrms)".to_string()
        ));
        assert!(lines
            .iter()
            .any(|l| l.starts_with("  output        +24.0 dBu")));
    }

    const MEASURING_VERBS: [&str; 5] =
        ["plot", "plot level", "plot ir", "test dut", "test hardware"];

    /// The stamp is UTC ISO 8601 at one-second resolution. The filename
    /// stamp (`io::timestamp()`, `%Y%m%dT%H%M%SZ`) and any local-time
    /// format fail this parse.
    #[test]
    fn run_header_stamp_parses_as_utc_iso8601() {
        let ts = ac_core::shared::time::now_utc_iso8601();
        let line = run_header_line("plot ir", &ts);
        let stamp = line.rsplit(' ').next().unwrap();
        assert!(
            chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%dT%H:%M:%SZ").is_ok(),
            "{line:?}"
        );
        assert!(
            chrono::NaiveDateTime::parse_from_str(&timestamp(), "%Y-%m-%dT%H:%M:%SZ").is_err(),
            "the filename stamp must not pass as the display stamp"
        );
    }

    #[test]
    fn run_header_fits_80_columns_for_every_verb() {
        let ts = ac_core::shared::time::now_utc_iso8601();
        for verb in MEASURING_VERBS {
            let line = run_header_line(verb, &ts);
            assert!(cols(&line) <= 80, "{line:?}");
            assert!(line.starts_with("ac  "), "{line:?}");
        }
    }

    /// Every verb's stamp starts in the same column (21), so a run of
    /// headers reads down one column. An unpadded `ac  {verb}   {ts}`
    /// fails this.
    #[test]
    fn run_header_stamp_sits_in_one_column_for_every_verb() {
        let ts = "2026-09-25T09:41:18Z";
        for verb in MEASURING_VERBS {
            let line = run_header_line(verb, ts);
            assert_eq!(line.find(ts), Some(20), "{line:?}");
        }
        assert_eq!(
            run_header_line("test hardware", ts),
            "ac  test hardware   2026-09-25T09:41:18Z"
        );
        assert_eq!(
            run_header_line("plot", ts),
            "ac  plot            2026-09-25T09:41:18Z"
        );
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
