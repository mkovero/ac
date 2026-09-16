use super::{check_ack, get_cal, level_to_dbfs, print_level, print_level_range};
use crate::client::AcClient;
use crate::io;
use crate::parse::CommandKind;

pub fn run(
    cmd: &CommandKind,
    cfg: &ac_core::config::Config,
    client: &mut AcClient,
    show_plot: bool,
) {
    let (start, stop, level, level_defaulted, ppd, bpo) = match cmd {
        CommandKind::Plot {
            start,
            stop,
            level,
            level_defaulted,
            ppd,
            bpo,
        } => (*start, *stop, level, *level_defaulted, *ppd, *bpo),
        _ => unreachable!(),
    };

    let cal = get_cal(client);
    let have_cal = cal.is_some();
    if have_cal {
        println!("  Loaded calibration from server.");
    } else {
        println!("  No calibration found \u{2014} levels in dBFS only.");
    }
    let level_db = level_to_dbfs(level, cal.as_ref());

    let start_hz = start.unwrap_or(cfg.range_start_hz);
    let stop_hz = stop.unwrap_or(cfg.range_stop_hz);

    println!("\n  Plot: {start_hz:.0} \u{2192} {stop_hz:.0} Hz  {ppd} pts/decade");
    io::print_freq_header(have_cal);

    let mut cmd_json = serde_json::json!({
        "cmd": "plot",
        "start_hz": start_hz,
        "stop_hz": stop_hz,
        "level_dbfs": level_db,
        "ppd": ppd,
    });
    if let Some(b) = bpo {
        cmd_json["bpo"] = serde_json::json!(b);
    }
    let ack = check_ack(client.send_cmd(&cmd_json, None), "plot");
    print_level(
        ack.get("level_dbfs").and_then(|v| v.as_f64()),
        level_defaulted,
        ack.get("max_dbfs").and_then(|v| v.as_f64()),
        cal.as_ref(),
        true,
    );
    if let (Some(out), Some(inp)) = (
        ack.get("out_port").and_then(|v| v.as_str()),
        ack.get("in_port").and_then(|v| v.as_str()),
    ) {
        println!("  Output: {out}  \u{2192}  Input: {inp}");
    }

    // Spawn the UI only after the daemon ACKed the request — otherwise a
    // refused command (busy daemon, invalid args) flashes a window that
    // immediately disconnects.
    if show_plot {
        launch_ui(LaunchKind::SweepFreq, cfg, None);
    }

    let (results, outcome) = collect_sweep(client, "plot");
    if outcome != SweepOutcome::Done || results.is_empty() {
        return;
    }
    io::print_summary(&results, "DUT", have_cal);
    save_results(&results, "plot", cfg);
}

pub fn run_level(
    cmd: &CommandKind,
    cfg: &ac_core::config::Config,
    client: &mut AcClient,
    show_plot: bool,
) {
    let (start, stop, level_defaulted, freq, steps) = match cmd {
        CommandKind::PlotLevel {
            start,
            stop,
            level_defaulted,
            freq,
            steps,
        } => (start, stop, *level_defaulted, *freq, *steps),
        _ => unreachable!(),
    };

    let cal = get_cal(client);
    let have_cal = cal.is_some();
    if have_cal {
        println!("  Loaded calibration from server.");
    } else {
        println!("  No calibration found \u{2014} levels in dBFS only.");
    }
    let start_db = level_to_dbfs(start, cal.as_ref());
    let stop_db = level_to_dbfs(stop, cal.as_ref());

    println!("\n  Plot level: {freq:.0} Hz  |  {steps} steps");
    io::print_freq_header(have_cal);

    let ack = check_ack(
        client.send_cmd(
            &serde_json::json!({
                "cmd": "plot_level",
                "freq_hz": freq,
                "start_dbfs": start_db,
                "stop_dbfs": stop_db,
                "steps": steps,
            }),
            None,
        ),
        "plot_level",
    );
    print_level_range(
        ack.get("start_dbfs").and_then(|v| v.as_f64()),
        ack.get("stop_dbfs").and_then(|v| v.as_f64()),
        level_defaulted,
        ack.get("max_dbfs").and_then(|v| v.as_f64()),
        cal.as_ref(),
    );
    if let (Some(out), Some(inp)) = (
        ack.get("out_port").and_then(|v| v.as_str()),
        ack.get("in_port").and_then(|v| v.as_str()),
    ) {
        println!("  Output: {out}  \u{2192}  Input: {inp}");
    }

    if show_plot {
        launch_ui(LaunchKind::SweepLevel, cfg, None);
    }

    let (results, outcome) = collect_sweep(client, "plot_level");
    if outcome != SweepOutcome::Done || results.is_empty() {
        return;
    }
    io::print_summary(&results, "DUT", have_cal);
    save_results(&results, "plot_level", cfg);
}

/// `ac plot ir` — Farina log-sweep impulse response (#282; moved from
/// `ac sweep ir`). Unlike `sweep_level`/`sweep_frequency`, this command
/// captures and analyses — it now shares `plot`'s calibration-line
/// convention and, unlike the old `sweep ir` (which called
/// `generate::wait_for_stop` and printed nothing else), actually reads the
/// `measurement/impulse_response` and `measurement/report` frames the
/// daemon already publishes.
pub fn run_ir(cmd: &CommandKind, cfg: &ac_core::config::Config, client: &mut AcClient) {
    let (f1, f2, duration, level, level_defaulted, n_harmonics, window_len, tail_s, distance_m) =
        match cmd {
            CommandKind::PlotIr {
                f1,
                f2,
                duration,
                level,
                level_defaulted,
                n_harmonics,
                window_len,
                tail_s,
                distance_m,
            } => (
                *f1,
                *f2,
                *duration,
                level,
                *level_defaulted,
                *n_harmonics,
                *window_len,
                *tail_s,
                *distance_m,
            ),
            _ => unreachable!(),
        };

    let cal = get_cal(client);
    let have_cal = cal.is_some();
    if have_cal {
        println!("  Loaded calibration from server.");
    } else {
        println!("  No calibration found \u{2014} levels in dBFS only.");
    }
    let level_db = level_to_dbfs(level, cal.as_ref());

    let gate = format!(
        "{} harmonics, {} window, {} tail",
        n_harmonics
            .map(|v| v.to_string())
            .unwrap_or_else(|| "default".into()),
        window_len
            .map(|v| format!("{v}-sample"))
            .unwrap_or_else(|| "default".into()),
        tail_s
            .map(|v| format!("{v:.2}s"))
            .unwrap_or_else(|| "default".into()),
    );
    println!("\n  IR: {f1:.0} \u{2192} {f2:.0} Hz  |  {duration:.1}s");
    println!("  gate       {gate}");
    // #460 UX: echo the typed distance before emission, so a token typo
    // (`0.8m` meant as `0.8s`) is visible before the result, and state its
    // absence rather than hide it.
    match distance_m {
        Some(d) => println!("  distance   {d} m"),
        None => println!("  distance   not given"),
    }

    let mut cmd_json = serde_json::json!({
        "cmd": "plot_ir",
        "f1_hz": f1,
        "f2_hz": f2,
        "duration": duration,
        "level_dbfs": level_db,
    });
    if let Some(v) = n_harmonics {
        cmd_json["n_harmonics"] = serde_json::json!(v);
    }
    if let Some(v) = window_len {
        cmd_json["window_len"] = serde_json::json!(v);
    }
    if let Some(v) = tail_s {
        cmd_json["tail_s"] = serde_json::json!(v);
    }
    if let Some(v) = distance_m {
        cmd_json["distance_m"] = serde_json::json!(v);
    }

    let ack = check_ack(client.send_cmd(&cmd_json, None), "plot_ir");
    print_level(
        ack.get("level_dbfs").and_then(|v| v.as_f64()),
        level_defaulted,
        ack.get("max_dbfs").and_then(|v| v.as_f64()),
        cal.as_ref(),
        true,
    );
    let out_port = ack.get("out_port").and_then(|v| v.as_str());
    if let Some(p) = out_port {
        println!("  output     {p}");
    }
    // #460 UX: every port the sweep leaves through or is referenced against,
    // printed before the result, on the same label grid as `gate` and
    // `level`. A reference output equal to the main output drives nothing
    // extra, so it is not named twice.
    if let Some(p) = ack.get("ref_out_port").and_then(|v| v.as_str()) {
        if Some(p) != out_port {
            println!("  ref out    {p}");
        }
    }
    if let Some(p) = ack.get("ref_in_port").and_then(|v| v.as_str()) {
        println!("  ref in     {p}");
    }
    println!("  Running IR measurement...\n");

    let (ir_frame, report_frame) = collect_ir(client, "plot_ir");
    print_ir_result(ir_frame.as_ref(), report_frame.as_ref(), duration, tail_s);
    print_ir_report(report_frame.as_ref(), cfg);
    print_ir_notes(report_frame.as_ref());
}

/// The edge guard's outcome, in the shape [`short_onset_rule`] takes, read
/// from the typed onset standing (#346 UX revision 4): `Unscored` (every
/// condition held) is a passed guard, `EdgeFollowing` carries its typed
/// re-pick, and every other standing is `None` — the guard did not run, or
/// another row already states the standing.
fn guard_outcome(
    standing: &ac_core::measurement::report::OnsetStanding,
) -> Option<ac_core::measurement::sweep::EdgeGuard> {
    use ac_core::measurement::report::OnsetStanding;
    use ac_core::measurement::sweep::EdgeGuard;
    match *standing {
        OnsetStanding::Unscored => Some(EdgeGuard::Passed),
        OnsetStanding::EdgeFollowing { repick } => Some(EdgeGuard::Failed { repick }),
        OnsetStanding::NoCausalBound
        | OnsetStanding::BoundNotBinding
        | OnsetStanding::PickerDeclined
        | OnsetStanding::PickOnWindowStart
        | OnsetStanding::DeconvolutionFailed => None,
    }
}

/// Derives the short terminal tag from `IrStats::onset_rule`'s full
/// sentence (#346 AC4, revised for #378's picker). Two facts a reader
/// needs a year later: which window the pick was made over, and whether
/// the pick landed inside it or on its edge — a pick sitting on the
/// window start is a stable, repeatable, possibly wrong number, and it
/// has to be visible on the line rather than inferable from the JSON.
///
/// Returns 3 lines normally (the onset's sample index with the rule's
/// intro, window start, and the causal-bound row built from `bound`'s own
/// fields, #460) and 4 when the pick is pinned to the window start or, per
/// `guard` (#346), the edge guard ran. `guard` is the guard's typed
/// outcome — `Passed`, `Failed { repick: Some(r) }` moved to `r`,
/// `Failed { repick: None }` no re-pick ran — and `None` when no guard row
/// prints. On a decline it returns the decline line, the case, and
/// a `check:` line — plus the bound row when the bound itself caused it. On a decline the second line is
/// the degenerate case named in `rule`, printed verbatim from between
/// its parentheses, so a case added in `ac-core` later reaches the
/// terminal without a change here. Falls back to the full string
/// verbatim if its shape ever changes underneath this, so a reader never
/// sees nothing.
fn short_onset_rule(
    rule: &str,
    onset_index: usize,
    bound: &ac_core::measurement::sweep::CausalBound,
    guard: Option<ac_core::measurement::sweep::EdgeGuard>,
) -> Vec<String> {
    if rule.contains("picker declined") {
        let case = rule
            .split_once('(')
            .and_then(|(_, rest)| rest.split_once(')'))
            .map(|(inside, _)| inside.to_string());
        let mut lines = vec!["picker declined \u{2014} no onset estimate".to_string()];
        let bound_caused_it = case.as_deref() == Some("causal bound at or after the peak");
        if let Some(case) = case {
            lines.push(case);
        }
        if bound_caused_it {
            // #460: the bound's own inputs put it there, so those are what
            // to check, not the gate.
            lines.push(causal_bound_row(bound));
            lines.push("check: typed distance, reference loopback routing".to_string());
        } else {
            lines.push("check: gate length, peak position in gate".to_string());
        }
        return lines;
    }
    let Some(start) = rule.find("window start at sample ").map(|at| {
        rule[at + "window start at sample ".len()..]
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap_or("")
            .to_string()
    }) else {
        return vec![rule.to_string()];
    };
    let limit = if rule.contains("search span is the tighter limit") {
        "search span"
    } else if rule.contains("causal bound enforced") {
        "causal bound"
    } else if rule.contains("no causal bound") {
        "search span"
    } else {
        return vec![rule.to_string()];
    };
    let intro = format!(
        "at sample {onset_index}  (AIC change-point pick, {:.1} ms window)",
        ac_core::measurement::sweep::ONSET_SEARCH_WINDOW_S * 1000.0
    );
    let pinned = rule.contains("pick landed on the window start");
    let clear = start
        .parse::<usize>()
        .map(|s| onset_index.saturating_sub(s))
        .unwrap_or(0);
    let mut lines = vec![
        intro,
        if pinned {
            format!("window start {start} ({limit}), pick ON start")
        } else {
            format!("window start {start} ({limit}), pick {clear} clear")
        },
    ];
    // #460 AC3 / UX row 3: what the bound was built from, or which input
    // it lacked — always on the same row, so the eye finds the answer there.
    lines.push(causal_bound_row(bound));
    if pinned {
        lines.push("onset may lie earlier than the window allows".to_string());
    }
    // #346 UX revision 4: the guard row, printed whenever the guard ran, so
    // a pass is not silent. The distance and the tolerance come from the
    // core constants; the extended start index is not printed, so the
    // m → samples conversion is not repeated here.
    use ac_core::measurement::sweep::{EdgeGuard, EDGE_GUARD_TOLERANCE_SAMPLES};
    let cm = ac_core::measurement::sweep::EDGE_GUARD_EXTENSION_M * 100.0;
    match guard {
        Some(EdgeGuard::Passed) => {
            let unit = if EDGE_GUARD_TOLERANCE_SAMPLES == 1 {
                "sample"
            } else {
                "samples"
            };
            lines.push(format!(
                "window start {cm:.0} cm earlier: pick moves \u{2264} \
                 {EDGE_GUARD_TOLERANCE_SAMPLES} {unit}"
            ));
        }
        Some(EdgeGuard::Failed {
            repick: Some(repick),
        }) => lines.push(format!(
            "window start {cm:.0} cm earlier: pick moves to {repick} ({:+})",
            repick as i64 - onset_index as i64
        )),
        Some(EdgeGuard::Failed { repick: None }) => lines.push(format!(
            "no re-pick \u{2014} window cannot start {cm:.0} cm earlier"
        )),
        None => {}
    }
    lines
}

/// Row 2 under `arrival` (#346 UX revision 4): the rule that produced the
/// arrival. One string: the arrival is always the magnitude peak. Takes the
/// source so a second [`ArrivalSource`] variant fails to compile here
/// rather than printing this text silently.
///
/// [`ArrivalSource`]: ac_core::measurement::report::ArrivalSource
fn arrival_source_line(source: &ac_core::measurement::report::ArrivalSource) -> String {
    use ac_core::measurement::report::ArrivalSource;
    let text = match source {
        ArrivalSource::Peak => "from peak (largest magnitude sample)",
    };
    format!("{CONT_INDENT}{text}")
}

/// The onset block's last row (#346 UX revision 4): the onset-to-peak gap,
/// always qualified `, not the arrival`. `None` when the onset is not
/// before the peak — a declined picker reports the peak.
fn onset_gap_line(stats: &ac_core::measurement::report::IrStats) -> Option<String> {
    if stats.onset_index >= stats.peak_index {
        return None;
    }
    Some(format!(
        "{CONT_INDENT}{} samples before peak, not the arrival",
        stats.peak_index - stats.onset_index
    ))
}

/// Onset block row 3 (#460 UX), printed from the bound's own fields rather
/// than parsed from `onset_rule`. An assumed speed of sound is said so: an
/// unset temperature silently uses the default `c`.
fn causal_bound_row(bound: &ac_core::measurement::sweep::CausalBound) -> String {
    use ac_core::measurement::sweep::{CausalBound, MissingBoundInput};
    match bound {
        CausalBound::Enforced { inputs, .. } => {
            let c = match inputs.temperature_c {
                Some(t) => format!("c {:.1} m/s at {t:.1} \u{b0}C", inputs.speed_of_sound_m_s),
                None => format!("c {:.1} m/s assumed", inputs.speed_of_sound_m_s),
            };
            format!("bound from ref latency + {} m, {c}", inputs.distance_m)
        }
        CausalBound::Unavailable(MissingBoundInput::Distance) => {
            "no causal bound \u{2014} distance not given (token: 1m)".to_string()
        }
        CausalBound::Unavailable(MissingBoundInput::ReferenceLatency { .. }) => {
            "no causal bound \u{2014} ref latency unavailable (below)".to_string()
        }
        CausalBound::Unavailable(MissingBoundInput::Both { .. }) => {
            "no causal bound \u{2014} no distance, ref latency unavailable".to_string()
        }
    }
}

/// Left-pads `label` to the column every #359 read-out line shares with
/// `ref latency`'s existing position: two leading spaces, then the label
/// padded to 14 columns, so every value in this block starts at column 16.
fn label_prefix(label: &str) -> String {
    format!("  {label:<14}")
}

/// 16 spaces — the continuation indent every line under a labelled block
/// uses, matching the column [`label_prefix`] leaves a value at.
const CONT_INDENT: &str = "                ";

/// Sample count formatted the way every #359/#460 latency line agrees on:
/// an integer when within 0.01 of one (rounding noise), otherwise one
/// decimal place.
fn format_samples(samples: f64) -> String {
    if (samples - samples.round()).abs() < 0.01 {
        format!("{}", samples.round() as i64)
    } else {
        format!("{samples:.1}")
    }
}

/// [`format_samples`] with an explicit sign — the `flight time` line's
/// samples figure can be negative.
fn format_samples_signed(samples: f64) -> String {
    if samples < 0.0 {
        format!("-{}", format_samples(-samples))
    } else {
        format!("+{}", format_samples(samples))
    }
}

/// Milliseconds figure for `latency` / `ref latency` / `ref stored`,
/// right-aligned to width 7 so the decimal points line up down the block
/// (#359 UX).
fn format_ms_aligned(ms: f64) -> String {
    format!("{ms:>7.4}")
}

/// Word-wraps `text` onto lines of at most `width` columns. Wraps at a
/// fixed width rather than detecting terminal width (#359 UX) — the text
/// is re-flowed, never rephrased.
fn word_wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate_len = if current.is_empty() {
            word.chars().count()
        } else {
            current.chars().count() + 1 + word.chars().count()
        };
        if candidate_len > width && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// A refusal reason under a labelled block: `none — <reason>`, word-wrapped
/// at a fixed 80 columns onto indent 16 (#359 UX). Never rephrases the
/// reason text — `TauRefusal::message()` and `ReferenceLatency`'s own
/// reasons stay verbatim.
fn labeled_wrapped(label: &str, reason: &str) -> Vec<String> {
    let text = format!("none \u{2014} {reason}");
    let wrapped = word_wrap(&text, 80 - CONT_INDENT.len());
    wrapped
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            if i == 0 {
                format!("{}{line}", label_prefix(label))
            } else {
                format!("{CONT_INDENT}{line}")
            }
        })
        .collect()
}

/// `measured <measured_at>, <age> before capture` (#359 UX) — the age is
/// this *report's own* `timestamp_utc` minus `measured_at`, never the wall
/// clock, so an archived report reads the same a year later. Falls back to
/// `measured <measured_at>` alone when either timestamp fails to parse.
fn measured_line(measured_at: &str, report_timestamp_utc: &str) -> String {
    match age_before_capture(measured_at, report_timestamp_utc) {
        Some(age) => format!("{CONT_INDENT}measured {measured_at}, {age} before capture"),
        None => format!("{CONT_INDENT}measured {measured_at}"),
    }
}

/// `report_timestamp_utc \u{2212} measured_at`, humanised per #359 UX's
/// buckets: under 120 s as seconds, under 120 min as minutes, under 48 h as
/// one-decimal hours, otherwise whole days. `None` when either timestamp
/// fails to parse as RFC3339.
fn age_before_capture(measured_at: &str, report_timestamp_utc: &str) -> Option<String> {
    let measured = chrono::DateTime::parse_from_rfc3339(measured_at).ok()?;
    let captured = chrono::DateTime::parse_from_rfc3339(report_timestamp_utc).ok()?;
    let secs = captured
        .with_timezone(&chrono::Utc)
        .signed_duration_since(measured.with_timezone(&chrono::Utc))
        .num_seconds();
    let abs_secs = secs.unsigned_abs();
    Some(if abs_secs < 120 {
        format!("{secs} s")
    } else if abs_secs < 120 * 60 {
        format!("{} min", secs / 60)
    } else if abs_secs < 48 * 3600 {
        format!("{:.1} h", secs as f64 / 3600.0)
    } else {
        format!("{} d", secs / 86_400)
    })
}

/// `UNVERIFIED — `, the prefix of every flagged enumeration verdict (#461
/// UX). Plain text so it reads without colour.
pub(super) const UNVERIFIED: &str = "UNVERIFIED \u{2014} ";

/// The action that clears a crossed or not-recorded enumeration flag
/// (#461 UX). Printed only where re-running does clear it.
const RECALIBRATE_CHECK: &str = "check: re-run `ac calibrate` with loopback patched";

/// Width of the `nodes: ` label; continuation lines are indented by it so
/// the node paths line up (#461 UX).
const NODES_LABEL: &str = "nodes: ";

/// Split a daemon-written `EnumerationCheck::Crossed::boundary` into its
/// head and, for a device re-enumeration, the node list (#461 UX) — the same
/// split `ref latency` makes on `"; check: "`.
pub(super) fn split_boundary(boundary: &str) -> (&str, Option<&str>) {
    match boundary.split_once(ac_core::shared::calibration::BOUNDARY_NODES_SEPARATOR) {
        Some((head, nodes)) => (head, Some(nodes)),
        None => (boundary, None),
    }
}

/// ` at <since>`, or ` after measurement` when a node only disappeared and
/// the observation carries no time (#461 UX).
pub(super) fn since_clause(since: Option<&str>) -> String {
    match since {
        Some(t) => format!(" at {t}"),
        None => " after measurement".to_string(),
    }
}

/// `nodes: <list>` wrapped at `80 − indent`, continuation lines indented
/// under the list (#461 UX). Wraps between entries, never inside one, so a
/// node path always stays on the line with its `new` / `re-created` /
/// `gone`.
pub(super) fn nodes_lines(list: &str, indent: &str) -> Vec<String> {
    let pad = " ".repeat(NODES_LABEL.len());
    let width = 80 - indent.len() - NODES_LABEL.len();
    let entries: Vec<&str> = list.split(", ").collect();
    let mut rows: Vec<String> = Vec::new();
    let mut current = String::new();
    for (i, entry) in entries.iter().enumerate() {
        let item = if i + 1 < entries.len() {
            format!("{entry},")
        } else {
            entry.to_string()
        };
        let candidate = if current.is_empty() {
            item.chars().count()
        } else {
            current.chars().count() + 1 + item.chars().count()
        };
        if candidate > width && !current.is_empty() {
            rows.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(&item);
    }
    if !current.is_empty() {
        rows.push(current);
    }
    rows.into_iter()
        .enumerate()
        .map(|(i, line)| {
            let label = if i == 0 { NODES_LABEL } else { pad.as_str() };
            format!("{indent}{label}{line}")
        })
        .collect()
}

/// `text` word-wrapped at `80 − indent`, every line at `indent`.
pub(super) fn indented_wrapped(text: &str, indent: &str) -> Vec<String> {
    word_wrap(text, 80 - indent.len())
        .into_iter()
        .map(|line| format!("{indent}{line}"))
        .collect()
}

/// The verdict on a stored τ's device enumeration, under its `measured`
/// line (#461 UX): verdict → `nodes:` → `check:`. Frozen report data only;
/// nothing is recomputed. `None` is a report or daemon older than the check,
/// which reads as not recorded — never as the same enumeration.
///
/// `ref_delta_agrees` is set for `ref stored` when this same capture's
/// `ref Δ` agreed: a crossed boundary then points at that evidence instead
/// of reading UNVERIFIED over `0 samples`, and needs no `check:`. The two
/// cannot-tell states stay UNVERIFIED regardless — no boundary was named
/// for the Δ to answer.
fn enumeration_lines(
    check: Option<&ac_core::shared::calibration::EnumerationCheck>,
    ref_delta_agrees: bool,
) -> Vec<String> {
    use ac_core::shared::calibration::EnumerationCheck;
    match check {
        Some(EnumerationCheck::Same) => vec![format!(
            "{CONT_INDENT}same device enumeration as this capture"
        )],
        Some(EnumerationCheck::Crossed { boundary, since }) => {
            let (head, nodes) = split_boundary(boundary);
            let at = since_clause(since.as_deref());
            let mut lines = vec![if ref_delta_agrees {
                format!("{CONT_INDENT}{head}{at} \u{2014} see ref \u{394}")
            } else {
                format!("{CONT_INDENT}{UNVERIFIED}{head}{at}")
            }];
            if let Some(list) = nodes {
                lines.extend(nodes_lines(list, CONT_INDENT));
            }
            if !ref_delta_agrees {
                lines.push(format!("{CONT_INDENT}{RECALIBRATE_CHECK}"));
            }
            lines
        }
        Some(EnumerationCheck::NotObservable { reason }) => {
            let mut lines = vec![format!(
                "{CONT_INDENT}{UNVERIFIED}device enumeration not observable"
            )];
            let (observation, places) = match reason.split_once("; check: ") {
                Some((o, p)) => (o, Some(p)),
                None => (reason.as_str(), None),
            };
            lines.extend(indented_wrapped(observation, CONT_INDENT));
            if let Some(places) = places {
                lines.extend(indented_wrapped(&format!("check: {places}"), CONT_INDENT));
            }
            lines
        }
        Some(EnumerationCheck::NotRecorded) | None => vec![
            format!("{CONT_INDENT}{UNVERIFIED}entry predates enumeration tracking"),
            format!("{CONT_INDENT}{RECALIBRATE_CHECK}"),
        ],
    }
}

/// The `ref latency` read-out (#460 UX), always printed: the same-capture
/// reference τ in `calibrate`'s `Delay:` format, or its unavailable reason
/// with any `; check: ` part on its own `check:` line.
fn reference_latency_lines(
    reference: Option<&ac_core::measurement::report::ReferenceLatency>,
    sample_rate_hz: u32,
) -> Vec<String> {
    use ac_core::measurement::report::ReferenceLatency;
    let unavailable = |reason: &str| match reason.split_once("; check: ") {
        Some((observation, places)) => vec![
            format!("  ref latency   unavailable \u{2014} {observation}"),
            format!("                check: {places}"),
        ],
        None => vec![format!("  ref latency   unavailable \u{2014} {reason}")],
    };
    match reference {
        Some(ReferenceLatency::Measured(m)) => {
            let samples = m.tau_s * sample_rate_hz as f64;
            let snr = m
                .pre_impulse_snr_db
                .map(|v| format!("{v:.1} dB"))
                .unwrap_or_else(|| "\u{221e} dB".to_string());
            vec![format!(
                "  ref latency   {} ms  ({} samples, SNR {snr}, same capture)",
                format_ms_aligned(m.tau_s * 1000.0),
                format_samples(samples),
            )]
        }
        Some(ReferenceLatency::Unavailable { reason }) => unavailable(reason),
        None => unavailable("not recorded (report predates schema v7)"),
    }
}

/// `latency` line (#359 UX): the τ subtracted from the arrival to produce
/// `flight time`, plus its measured-date line. Always printed, mirroring
/// `ref latency`'s own always-printed rule.
fn interface_latency_lines(
    latency: Option<&ac_core::measurement::report::InterfaceLatency>,
    schema_version: u32,
    sample_rate_hz: u32,
    report_timestamp_utc: &str,
) -> Vec<String> {
    use ac_core::measurement::report::InterfaceLatency;
    match latency {
        Some(InterfaceLatency::Measured(m)) => {
            let samples = m.tau_s * sample_rate_hz as f64;
            let mut lines = vec![
                format!(
                    "{}{} ms  ({} samples, stored)",
                    label_prefix("latency"),
                    format_ms_aligned(m.tau_s * 1000.0),
                    format_samples(samples),
                ),
                measured_line(&m.measured_at, report_timestamp_utc),
            ];
            // #461: τ is per channel pair, so `ref Δ` never clears this one.
            lines.extend(enumeration_lines(m.enumeration.as_ref(), false));
            lines
        }
        Some(InterfaceLatency::Unavailable { reason }) => labeled_wrapped("latency", reason),
        None => {
            let text = if schema_version < 5 {
                "not recorded (report predates schema v5)"
            } else {
                "not recorded"
            };
            vec![format!("{}{text}", label_prefix("latency"))]
        }
    }
}

/// `ref stored` line (#359 UX): the τ `calibrate` has on file for the
/// *reference* pair — the second input to `ref \u{394}`, shown next to the
/// first (`ref latency`). `ref_delta_agrees` is whether this capture's
/// `ref Δ` agreed (#461 UX): see [`enumeration_lines`].
fn reference_stored_latency_lines(
    stored: Option<&ac_core::measurement::report::InterfaceLatency>,
    schema_version: u32,
    sample_rate_hz: u32,
    report_timestamp_utc: &str,
    ref_delta_agrees: bool,
) -> Vec<String> {
    use ac_core::measurement::report::InterfaceLatency;
    match stored {
        Some(InterfaceLatency::Measured(m)) => {
            let samples = m.tau_s * sample_rate_hz as f64;
            let mut lines = vec![
                format!(
                    "{}{} ms  ({} samples, stored)",
                    label_prefix("ref stored"),
                    format_ms_aligned(m.tau_s * 1000.0),
                    format_samples(samples),
                ),
                measured_line(&m.measured_at, report_timestamp_utc),
            ];
            lines.extend(enumeration_lines(m.enumeration.as_ref(), ref_delta_agrees));
            lines
        }
        Some(InterfaceLatency::Unavailable { reason }) => labeled_wrapped("ref stored", reason),
        None => {
            let text = if schema_version < 9 {
                "not recorded (report predates schema v9)"
            } else {
                "not looked up \u{2014} no reference configured"
            };
            vec![format!("{}{text}", label_prefix("ref stored"))]
        }
    }
}

/// The four-line disagreement block (#359 UX), built from
/// [`ac_core::shared::calibration::TauDisagreement`]'s own fields rather
/// than its `message()` (~170 columns, one line) — the same split
/// `calibrate`'s own disagreement read-out makes. #347's phrases are
/// reused verbatim so the two faults are recognisably the same fault
/// (AC2/AC3).
fn disagreement_lines(d: &ac_core::shared::calibration::TauDisagreement) -> Vec<String> {
    let delta_ms = d.delta_samples as f64 / d.sample_rate as f64 * 1000.0;
    let mut lines = vec![format!(
        "{}{:+} samples = {:+.4} ms at {} Hz",
        label_prefix("ref \u{394}"),
        d.delta_samples,
        delta_ms,
        d.sample_rate,
    )];
    match d.periods {
        Some(n) => {
            let period = d.period_size.unwrap_or_default();
            lines.push(format!(
                "{CONT_INDENT}exactly {} period{} of {period} samples",
                n.unsigned_abs(),
                if n.unsigned_abs() == 1 { "" } else { "s" },
            ));
            lines.push(format!(
                "{CONT_INDENT}a graph-buffering shift, not hardware drift"
            ));
        }
        None => {
            let period_note = match d.period_size {
                Some(p) => format!("period {p} samples"),
                None => "period not reported by this backend".to_string(),
            };
            lines.push(format!(
                "{CONT_INDENT}not a period multiple ({period_note})"
            ));
        }
    }
    lines.push(format!(
        "{CONT_INDENT}stored {:.3} samples \u{2192} this capture {:.3} samples",
        d.reading1_s * d.sample_rate as f64,
        d.reading2_s * d.sample_rate as f64,
    ));
    lines.push(format!(
        "{CONT_INDENT}check: {}",
        if d.periods.is_some() {
            "re-run plot ir, then ac calibrate on both pairs"
        } else {
            "interface clock, device reconnects since measured"
        }
    ));
    lines
}

/// `ref \u{394}` block (#359 UX): the arrival-check evidence, printed as a
/// number, never a verdict (#363's own rule — `0 samples` is evidence,
/// `agree` would be a verdict). `stored_period_size` comes from
/// `report.reference_stored_latency` directly, since
/// [`ac_core::measurement::report::ArrivalCheck::Agree`] itself carries no
/// reading to name a period from.
fn arrival_check_lines(
    check: &ac_core::measurement::report::ArrivalCheck,
    stored_period_size: Option<u32>,
) -> Vec<String> {
    use ac_core::measurement::report::ArrivalCheck;
    match check {
        ArrivalCheck::Agree => {
            let period_note = match stored_period_size {
                Some(p) => format!("one period = {p}"),
                None => "period not reported by this backend".to_string(),
            };
            vec![format!(
                "{}0 samples  (same capture \u{2212} stored, {period_note})",
                label_prefix("ref \u{394}")
            )]
        }
        ArrivalCheck::Unchecked { .. } => vec![format!(
            "{}not checked \u{2014} needs ref latency and ref stored (above)",
            label_prefix("ref \u{394}")
        )],
        ArrivalCheck::PeriodShift(d) | ArrivalCheck::Mismatch(d) => disagreement_lines(d),
    }
}

/// `flight time` line (#359 UX), directly under `arrival`. `Some` prints
/// the τ-corrected figure; `None` distinguishes a withheld correction (a
/// detected `ref \u{394}` disagreement — the numbers exist, the check
/// declined to combine them) from one that was never possible (no stored
/// latency for this capture pair at all). `Some` alongside
/// `ArrivalCheck::Unchecked` (case D — a flight time exists, but the
/// same-capture corroboration never ran) gets a second, 16-space-indented
/// continuation line naming that (codex-qa on PR #477): otherwise an
/// unverified flight time prints identically to a checked one.
///
/// #461: a flight time over a stored τ whose device enumeration is anything
/// but `same` gets `latency UNVERIFIED — see latency (below)` directly under
/// the value, above the reference-check line — a reader who stops at
/// `flight time` would otherwise miss a caveat two blocks down.
fn flight_time_line(stats: &ac_core::measurement::report::IrStats) -> Vec<String> {
    use ac_core::measurement::report::ArrivalCheck;
    match (stats.flight_time_s, &stats.arrival_check) {
        (Some(ft), check) => {
            let samples = ft * stats.sample_rate_hz as f64;
            let mut lines = vec![format!(
                "{}{} samples  ({:+.3} ms, arrival \u{2212} latency)",
                label_prefix("flight time"),
                format_samples_signed(samples),
                ft * 1000.0,
            )];
            if stats.interface_latency_unverified() {
                lines.push(format!(
                    "{CONT_INDENT}latency UNVERIFIED \u{2014} see latency (below)"
                ));
            }
            if matches!(check, ArrivalCheck::Unchecked { .. }) {
                lines.push(format!(
                    "{CONT_INDENT}reference check not run \u{2014} see ref \u{394}"
                ));
            }
            lines
        }
        (None, ArrivalCheck::PeriodShift(_)) => vec![format!(
            "{}withheld \u{2014} ref \u{394} is a period shift (below)",
            label_prefix("flight time")
        )],
        (None, ArrivalCheck::Mismatch(_)) => vec![format!(
            "{}withheld \u{2014} ref \u{394} is not zero (below)",
            label_prefix("flight time")
        )],
        (None, _) => vec![format!(
            "{}not shown \u{2014} no stored latency for this pair (below)",
            label_prefix("flight time")
        )],
    }
}

/// The read-out: arrival (samples and ms, re gate centre), peak,
/// pre-impulse SNR, and the gate that produced them — decoded from the
/// `measurement/report` frame rather than recomputed off the raw IR
/// frame, so the printed numbers and the archived ones are the same
/// numbers by construction. No distance figure — #391 removed the
/// ms → m conversion this used to also print.
fn print_ir_report(report_frame: Option<&serde_json::Value>, cfg: &ac_core::config::Config) {
    use ac_core::measurement::report::{IrVerdict, MeasurementReport, PRE_IMPULSE_SNR_MIN_DB};

    let Some(value) = report_frame.and_then(|f| f.get("report")) else {
        eprintln!("  !! no measurement/report frame — nothing to summarise");
        return;
    };
    let report: MeasurementReport = match serde_json::from_value(value.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  !! could not decode report: {e}");
            return;
        }
    };
    let Some(stats) = report.ir_stats() else {
        eprintln!("  !! report carries no impulse-response payload to summarise");
        return;
    };

    // A capture whose peak cannot be trusted (#376) is reported as
    // failed, not as a result with a number in it: no arrival line —
    // that is the exact plausible-looking wrong-number shape the issue
    // exists to close.
    if let IrVerdict::Failed { reason } = &stats.verdict {
        println!("  DECONVOLUTION FAILED \u{2014} {reason}");
        println!("                check: drive level, mic gain, distance, room noise");
        println!();
    } else {
        println!(
            "  arrival       {:+} samples  ({:+.3} ms re gate centre @ {} Hz)",
            stats.delay_samples,
            stats.arrival_s * 1000.0,
            stats.sample_rate_hz,
        );
        // #346: the rule that produced the arrival, on the row under it.
        println!("{}", arrival_source_line(&stats.arrival_source));
        // #359: the one τ subtraction this report can offer, gated by
        // `arrival_check` — sits directly under `arrival` so the two
        // primary values stack. Never printed on a failed deconvolution
        // (#376's rule that a failed capture prints no arrival).
        for line in flight_time_line(&stats) {
            println!("{line}");
        }
    }
    println!(
        "  peak          {:.4} FS  ({:+.2} dB re unity)  at sample {}",
        stats.peak_magnitude,
        20.0 * stats.peak_magnitude.max(1e-12).log10(),
        stats.peak_index,
    );
    if matches!(stats.verdict, IrVerdict::Failed { .. }) {
        println!("                diagnostic only \u{2014} not a valid arrival");
    } else {
        // #346 UX: the onset is its own labelled block, a diagnostic
        // beside `peak`. The
        // rule that produced it reaches the terminal as a short derived tag
        // (the full sentence runs past 80 columns); the untruncated rule
        // still rides the persisted JSON via `IrStats::onset_rule`. The
        // guard row is driven by the typed `onset_standing`, not by
        // parsing the rule.
        let onset_lines = short_onset_rule(
            &stats.onset_rule,
            stats.onset_index,
            &stats.causal_bound,
            guard_outcome(&stats.onset_standing),
        );
        println!("{}{}", label_prefix("onset"), onset_lines[0]);
        for line in &onset_lines[1..] {
            println!("{CONT_INDENT}{line}");
        }
        // The onset-to-peak gap: the loudspeaker's group-delay excess, and
        // the quantity #378's AC6 found moving with position. Printed on
        // every capture so an operator who moves the mic sees it move.
        if let Some(line) = onset_gap_line(&stats) {
            println!("{line}");
        }
    }
    // #359 UX: the latency block — `latency`, `ref latency`, `ref stored`,
    // `ref Δ` — replaces the single `ref latency` line in its previous
    // position. All four print unconditionally, on a failed deconvolution
    // too: the reference leg is its own reading and says something about
    // this lifetime even when the IR itself failed.
    for line in interface_latency_lines(
        report.interface_latency.as_ref(),
        report.schema_version,
        stats.sample_rate_hz,
        &report.timestamp_utc,
    ) {
        println!("{line}");
    }
    for line in reference_latency_lines(report.reference_latency.as_ref(), stats.sample_rate_hz) {
        println!("{line}");
    }
    for line in reference_stored_latency_lines(
        report.reference_stored_latency.as_ref(),
        report.schema_version,
        stats.sample_rate_hz,
        &report.timestamp_utc,
        matches!(
            stats.arrival_check,
            ac_core::measurement::report::ArrivalCheck::Agree
        ),
    ) {
        println!("{line}");
    }
    let stored_period_size = match report.reference_stored_latency.as_ref() {
        Some(ac_core::measurement::report::InterfaceLatency::Measured(m)) => m.period_size,
        _ => None,
    };
    for line in arrival_check_lines(&stats.arrival_check, stored_period_size) {
        println!("{line}");
    }
    if stats.pre_impulse_snr_db.is_finite() {
        if matches!(stats.verdict, IrVerdict::Failed { .. }) {
            println!(
                "  pre-imp SNR   {:.1} dB  (required \u{2265} {:.1} dB, threshold set from rig data)",
                stats.pre_impulse_snr_db, PRE_IMPULSE_SNR_MIN_DB,
            );
        } else {
            println!("  pre-imp SNR   {:.1} dB", stats.pre_impulse_snr_db);
        }
    } else if let IrVerdict::Failed { reason } = &stats.verdict {
        // Non-finite here means `ir_stats` had nothing to measure a floor
        // from at all (see the reason already printed in the banner
        // above) — restate it rather than a generic "silence" that would
        // misdescribe a zero-peak or guard-band-exhausted capture alike.
        println!("  pre-imp SNR   {reason}");
    } else {
        // Non-finite but `Ok`: a zero floor against a nonzero peak is the
        // best possible capture, not an unmeasurable one.
        println!("  pre-imp SNR   \u{221e} dB  (zero measured floor)");
    }
    println!(
        "  gate          {} window, {} samples ({:.2} ms) → f_low {:.1} Hz",
        stats.gate_window_kind,
        stats.window_len,
        stats.gate_window_s * 1000.0,
        stats.gate_f_low_hz,
    );

    if let Some(dir) = cfg.report_dir.as_ref() {
        let stem = report.timestamp_utc.replace(':', "-");
        println!(
            "  report        {}",
            dir.join(format!("{stem}-plot_ir.json")).display()
        );
        println!(
            "  csv           {}",
            dir.join(format!("{stem}-plot_ir.csv")).display()
        );
    } else {
        eprintln!("  note: report_dir not configured — result not persisted (see `ac setup`)");
    }
}

/// Wait for `plot_ir`'s DATA frames: `measurement/impulse_response` and
/// `measurement/report` ride their own topics (not wrapped in a generic
/// `data` topic the way `plot`/`plot_level` per-point frames are), so this
/// mirrors `collect_sweep` but keys off the topic string directly.
fn collect_ir(
    client: &mut AcClient,
    cmd_name: &str,
) -> (Option<serde_json::Value>, Option<serde_json::Value>) {
    let mut ir_frame = None;
    let mut report_frame = None;
    loop {
        let frame = match client.recv_data(300_000) {
            Some(f) => f,
            None => {
                eprintln!("\n  error: timeout waiting for {cmd_name} data");
                break;
            }
        };
        let (topic, data) = frame;
        match topic.as_str() {
            "measurement/impulse_response" => ir_frame = Some(data),
            "measurement/report" => report_frame = Some(data),
            "done" => break,
            "error" => {
                let msg = data
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("error");
                eprintln!("\n  !! {msg}");
                break;
            }
            _ => {}
        }
    }
    (ir_frame, report_frame)
}

fn print_ir_result(
    ir_frame: Option<&serde_json::Value>,
    report_frame: Option<&serde_json::Value>,
    duration: f64,
    tail_s: Option<f64>,
) {
    let Some(data) = ir_frame.and_then(|f| f.get("data")) else {
        eprintln!("  !! no impulse response received");
        return;
    };
    // Peak, arrival and gate now come from the report frame via
    // `print_ir_report` (#283) — reading them off the raw IR frame here
    // as well would let the printed and archived numbers drift apart.
    if let Some(n) = data
        .get("harmonics")
        .and_then(|v| v.as_array())
        .map(Vec::len)
    {
        println!("  harmonics     {n} order(s) extracted");
    }
    // `tail_s` unset on the CLI side means "daemon default" (0.5s per
    // ZMQ.md's `plot_ir` request) — report the nominal figure either way,
    // the report `notes` line below carries the measured decay verdict.
    let tail = tail_s.unwrap_or(0.5);
    println!(
        "  captured      {:.2}s  ({duration:.2}s sweep + {tail:.2}s tail)",
        duration + tail
    );
    let _ = report_frame;
}

/// The report's `notes`: the ISO 18233 §6.3.2 measured tail-decay verdict
/// and the §B.5 linear-deconvolution artefact statement, one line each.
/// Printed last, and printed verbatim from the report so the operator
/// reads exactly what the archive records (#283).
fn print_ir_notes(report_frame: Option<&serde_json::Value>) {
    let Some(notes) = report_frame
        .and_then(|f| f.get("report"))
        .and_then(|r| r.get("notes"))
        .and_then(|v| v.as_str())
    else {
        return;
    };
    println!();
    for line in notes.lines() {
        println!("  {line}");
    }
}

/// Whether a sweep reached its terminal `done` frame. Anything else — a
/// terminal `error` (analyzer failure, #428) or a timeout — leaves
/// `results` holding only a prefix that must never be treated as a
/// complete artifact: no summary printed, no CSV written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SweepOutcome {
    Done,
    Failed,
}

fn collect_sweep(client: &mut AcClient, cmd_name: &str) -> (Vec<serde_json::Value>, SweepOutcome) {
    collect_sweep_frames(|| client.recv_data(300_000), cmd_name)
}

/// Core of `collect_sweep`, generic over the frame source so the
/// atomic-failure gating (a terminal `error` must never leave `outcome ==
/// Done`, however many `measurement/frequency_response/point` frames
/// preceded it) can be unit-tested without a real `AcClient`/socket —
/// see `tests::error_after_points_is_failed_not_done` below (#428 QA).
fn collect_sweep_frames(
    mut next_frame: impl FnMut() -> Option<(String, serde_json::Value)>,
    cmd_name: &str,
) -> (Vec<serde_json::Value>, SweepOutcome) {
    let mut results = Vec::new();
    let mut outcome = SweepOutcome::Failed;

    loop {
        let frame = match next_frame() {
            Some(f) => f,
            None => {
                eprintln!("\n  error: timeout waiting for {cmd_name} data");
                break;
            }
        };
        let (topic, data) = frame;

        if topic == "data" {
            if data.get("type").and_then(|v| v.as_str())
                == Some("measurement/frequency_response/point")
            {
                io::print_freq_row(&data);
                results.push(data);
            }
        } else if topic == "done" {
            if let Some(xruns) = data.get("xruns").and_then(|v| v.as_u64()) {
                if xruns > 0 {
                    println!("\n  !! {xruns} xrun(s) during {cmd_name}");
                }
            }
            outcome = SweepOutcome::Done;
            break;
        } else if topic == "error" {
            let msg = data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("error");
            let partial = match (
                data.get("requested_points").and_then(|v| v.as_u64()),
                data.get("completed_points").and_then(|v| v.as_u64()),
            ) {
                (Some(requested), Some(completed)) => {
                    format!(" ({completed} of {requested} points completed; no report written)")
                }
                _ => String::new(),
            };
            eprintln!("\n  !! {msg}{partial}");
            break;
        }
    }
    (results, outcome)
}

fn save_results(results: &[serde_json::Value], label: &str, cfg: &ac_core::config::Config) {
    let dir = io::output_dir(cfg);
    let ts = io::timestamp();
    let safe = label.replace(' ', "_");
    let path = dir.join(format!("{safe}_{ts}.csv"));
    io::save_csv(results, &path);
}

/// What `launch_ui` should do post-command. The GPU viewer this used to
/// spawn is gone; `Monitor` now always renders via the
/// terminal (`monitor_tui`), and the sweep variants just note that no
/// visual plot is shown — the CSV/stdout output already carries the data.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LaunchKind {
    /// Frequency sweep (paired with `ac plot ... show`).
    SweepFreq,
    /// Level sweep (paired with `ac plot level ... show`).
    SweepLevel,
    /// Live monitor view.
    Monitor,
}

pub(crate) fn launch_ui(kind: LaunchKind, cfg: &ac_core::config::Config, channels: Option<&[u32]>) {
    match kind {
        LaunchKind::Monitor => run_tui_fallback(cfg, channels),
        LaunchKind::SweepFreq | LaunchKind::SweepLevel => {
            eprintln!("  note: no visual plot display available — see CSV/stdout output above");
        }
    }
}

fn run_tui_fallback(cfg: &ac_core::config::Config, channels: Option<&[u32]>) {
    let chs: Vec<u32> = channels.map(|s| s.to_vec()).unwrap_or_default();
    if let Err(e) = super::monitor_tui::run(cfg, &chs) {
        eprintln!("  monitor: tui error: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::{
        arrival_check_lines, arrival_source_line, collect_sweep_frames, enumeration_lines,
        flight_time_line, guard_outcome, interface_latency_lines, label_prefix, onset_gap_line,
        reference_latency_lines, reference_stored_latency_lines, short_onset_rule, SweepOutcome,
        CONT_INDENT,
    };
    use ac_core::measurement::report::{
        ArrivalCheck, ArrivalSource, InterfaceLatency, IrStats, IrVerdict, MeasuredLatency,
        MeasuredReferenceLatency, OnsetStanding, ReferenceLatency,
    };
    use ac_core::measurement::sweep::{BoundInputs, CausalBound, EdgeGuard, MissingBoundInput};
    use ac_core::shared::calibration::{EnumerationCheck, TauDisagreement};
    use std::collections::VecDeque;

    fn point(freq_hz: f64) -> serde_json::Value {
        serde_json::json!({
            "type": "measurement/frequency_response/point",
            "freq_hz": freq_hz,
        })
    }

    /// PR #451 QA finding (#428): a terminal `error` after some points had
    /// already streamed must report `SweepOutcome::Failed` and only the
    /// completed prefix — `run`/`run_level` gate `print_summary`/
    /// `save_results` on `outcome == Done`, so this is what makes the
    /// atomic-failure guarantee reach the CLI's own summary/CSV output,
    /// not just the daemon's wire frames.
    #[test]
    fn error_after_points_is_failed_not_done() {
        let mut frames: VecDeque<(String, serde_json::Value)> = VecDeque::from([
            ("data".to_string(), point(100.0)),
            ("data".to_string(), point(200.0)),
            (
                "error".to_string(),
                serde_json::json!({
                    "cmd": "plot",
                    "message": "capture at 1000 Hz has 48 samples; minimum is 256",
                    "requested_points": 5,
                    "completed_points": 2,
                }),
            ),
            // Must never be reached: a real daemon does not publish a
            // point or `done` after a terminal `error`, and the loop must
            // not either.
            ("done".to_string(), serde_json::json!({"xruns": 0})),
        ]);

        let (results, outcome) = collect_sweep_frames(|| frames.pop_front(), "plot");

        assert_eq!(outcome, SweepOutcome::Failed);
        assert_eq!(
            results.len(),
            2,
            "only the pre-failure points should be retained: {results:?}"
        );
    }

    #[test]
    fn done_after_points_is_done() {
        let mut frames: VecDeque<(String, serde_json::Value)> = VecDeque::from([
            ("data".to_string(), point(100.0)),
            ("data".to_string(), point(200.0)),
            ("done".to_string(), serde_json::json!({"xruns": 0})),
        ]);

        let (results, outcome) = collect_sweep_frames(|| frames.pop_front(), "plot");

        assert_eq!(outcome, SweepOutcome::Done);
        assert_eq!(results.len(), 2);
    }

    fn unbounded() -> CausalBound {
        CausalBound::Unavailable(MissingBoundInput::Both {
            reference_reason: "no reference configured (ac setup reference)".into(),
        })
    }

    fn enforced(distance_m: f64, temperature_c: Option<f64>) -> CausalBound {
        CausalBound::Enforced {
            index: 1305,
            inputs: BoundInputs {
                reference_tau_s: 0.017_822_9,
                distance_m,
                speed_of_sound_m_s: ac_core::shared::conversions::speed_of_sound_from_config(
                    temperature_c,
                ),
                temperature_c,
            },
        }
    }

    /// QA (PR #377), carried into #378: `short_onset_rule`'s decline
    /// branch had no test naming its output — a typo in the
    /// `.contains("picker declined")` match here, or in the string it
    /// matches against in `ac_core::measurement::sweep::estimate_onset`,
    /// would fall through to the window-clause branch instead, silently
    /// dropping the operator-facing warning at the terminal.
    #[test]
    fn short_onset_rule_surfaces_the_decline_line() {
        let rule = "onset picker declined (search window shorter than 2 samples) — index is \
                    the peak, not an onset";
        let lines = short_onset_rule(rule, 1479, &unbounded(), None);
        assert_eq!(
            lines,
            vec![
                "picker declined — no onset estimate".to_string(),
                "search window shorter than 2 samples".to_string(),
                "check: gate length, peak position in gate".to_string(),
            ]
        );
    }

    /// A degenerate case added in `ac-core` later must reach the terminal
    /// without a change here: the parenthetical is printed verbatim, not
    /// matched against a list.
    #[test]
    fn short_onset_rule_prints_an_unknown_decline_case_verbatim() {
        let rule = "onset picker declined (a case invented by this test) — index is the peak, \
                    not an onset";
        let lines = short_onset_rule(rule, 1479, &unbounded(), None);
        assert_eq!(lines[1], "a case invented by this test".to_string());
    }

    /// #460 UX frame 5: a bound at or after the peak names the bound's inputs
    /// and says to check them, not the gate.
    #[test]
    fn short_onset_rule_names_the_bound_inputs_when_the_bound_caused_the_decline() {
        let rule = "onset picker declined (causal bound at or after the peak) — index is the \
                    peak, not an onset";
        let lines = short_onset_rule(rule, 1479, &enforced(3.0, None), None);
        assert_eq!(
            lines,
            vec![
                "picker declined — no onset estimate".to_string(),
                "causal bound at or after the peak".to_string(),
                "bound from ref latency + 3 m, c 343.0 m/s assumed".to_string(),
                "check: typed distance, reference loopback routing".to_string(),
            ]
        );
    }

    #[test]
    fn short_onset_rule_reports_the_window_start_how_clear_the_pick_is_and_the_bound() {
        let rule = "AIC change-point pick over a 10.0 ms window; window start at sample 1305, \
                    causal bound enforced";
        let lines = short_onset_rule(rule, 1369, &enforced(1.0, Some(21.5)), None);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(
            lines[0],
            "at sample 1369  (AIC change-point pick, 10.0 ms window)"
        );
        assert_eq!(lines[1], "window start 1305 (causal bound), pick 64 clear");
        assert!(
            lines[2].starts_with("bound from ref latency + 1 m, c ")
                && lines[2].ends_with(" m/s at 21.5 °C"),
            "{:?}",
            lines[2]
        );
    }

    /// #460 AC3 / UX: the old `(search span, no geometry known)` named
    /// neither input. The window line now says only which limit set it, and
    /// row 3 names what was missing.
    #[test]
    fn short_onset_rule_names_the_missing_input_when_no_bound_applies() {
        let rule = "AIC change-point pick over a 10.0 ms window; window start at sample 455, \
                    no causal bound (distance not given)";
        let cases = [
            (
                MissingBoundInput::Distance,
                "no causal bound — distance not given (token: 1m)",
            ),
            (
                MissingBoundInput::ReferenceLatency {
                    reason: "no reference configured (ac setup reference)".into(),
                },
                "no causal bound — ref latency unavailable (below)",
            ),
            (
                MissingBoundInput::Both {
                    reference_reason: "no reference configured (ac setup reference)".into(),
                },
                "no causal bound — no distance, ref latency unavailable",
            ),
        ];
        for (missing, row) in cases {
            let lines = short_onset_rule(rule, 519, &CausalBound::Unavailable(missing), None);
            assert_eq!(lines[1], "window start 455 (search span), pick 64 clear");
            assert_eq!(lines[2], row);
            assert!(
                !lines.iter().any(|l| l.contains("no geometry known")),
                "pre-#460 clause survives: {lines:?}"
            );
        }
    }

    /// A causal bound that did not set the window start must not read as
    /// though it did — the operator's question is which limit the pick is
    /// pinned against.
    #[test]
    fn short_onset_rule_names_the_search_span_when_the_bound_does_not_bind() {
        let rule = "AIC change-point pick over a 10.0 ms window; window start at sample 455, \
                    causal bound enforced at sample 10, search span is the tighter limit";
        let lines = short_onset_rule(rule, 519, &enforced(1.0, None), None);
        assert_eq!(
            lines[1],
            "window start 455 (search span), pick 64 clear".to_string()
        );
    }

    /// The abnormal case keeps the extra line (#460 UX frame 6): rows 1–3
    /// as normal, the pinned warning on row 4.
    #[test]
    fn short_onset_rule_flags_a_pick_pinned_to_the_window_start() {
        let rule = "AIC change-point pick over a 10.0 ms window; window start at sample 1305, \
                    causal bound enforced; pick landed on the window start — the true onset \
                    may lie earlier";
        let lines = short_onset_rule(rule, 1305, &enforced(1.0, None), None);
        assert_eq!(
            lines,
            vec![
                "at sample 1305  (AIC change-point pick, 10.0 ms window)".to_string(),
                "window start 1305 (causal bound), pick ON start".to_string(),
                "bound from ref latency + 1 m, c 343.0 m/s assumed".to_string(),
                "onset may lie earlier than the window allows".to_string(),
            ]
        );
    }

    /// #346: `print_ir_report` feeds `short_onset_rule` through
    /// `guard_outcome`. `Unscored` is a passed guard, both typed re-pick
    /// shapes of `EdgeFollowing` reach the row unchanged, and every other
    /// standing maps to `None`.
    #[test]
    fn guard_outcome_maps_every_onset_standing() {
        assert_eq!(
            guard_outcome(&OnsetStanding::Unscored),
            Some(EdgeGuard::Passed)
        );
        for standing in [
            OnsetStanding::NoCausalBound,
            OnsetStanding::BoundNotBinding,
            OnsetStanding::PickerDeclined,
            OnsetStanding::PickOnWindowStart,
            OnsetStanding::DeconvolutionFailed,
        ] {
            assert_eq!(guard_outcome(&standing), None, "{standing:?}");
        }
        assert_eq!(
            guard_outcome(&OnsetStanding::EdgeFollowing {
                repick: Some(10_471)
            }),
            Some(EdgeGuard::Failed {
                repick: Some(10_471)
            })
        );
        assert_eq!(
            guard_outcome(&OnsetStanding::EdgeFollowing { repick: None }),
            Some(EdgeGuard::Failed { repick: None })
        );
    }

    /// #346 UX revisions 3 and 4, frames 1 to 3: the guard row follows the
    /// bound row whenever the guard ran — a pass states the tolerance it was
    /// held to, a failure the re-pick; rows 1–3 are unchanged. The re-pick
    /// delta is always signed, in either direction. No row when the guard
    /// did not run.
    #[test]
    fn short_onset_rule_flags_a_pick_that_follows_the_window_edge() {
        let rule = "AIC change-point pick over a 10.0 ms window; window start at sample 10463, \
                    causal bound enforced; re-pick with the window start 5 cm earlier went to \
                    sample 10471 — the pick follows the window edge";
        let head = vec![
            "at sample 10483  (AIC change-point pick, 10.0 ms window)".to_string(),
            "window start 10463 (causal bound), pick 20 clear".to_string(),
            "bound from ref latency + 2 m, c 343.0 m/s assumed".to_string(),
        ];
        let with_row = |row: &str| {
            let mut v = head.clone();
            v.push(row.to_string());
            v
        };
        let failed = |repick| Some(EdgeGuard::Failed { repick });
        assert_eq!(
            short_onset_rule(rule, 10483, &enforced(2.0, None), failed(Some(10471))),
            with_row("window start 5 cm earlier: pick moves to 10471 (-12)")
        );
        assert_eq!(
            short_onset_rule(rule, 10483, &enforced(2.0, None), failed(Some(10486))),
            with_row("window start 5 cm earlier: pick moves to 10486 (+3)")
        );
        let unchecked = "AIC change-point pick over a 10.0 ms window; window start at sample \
                         10463, causal bound enforced; no re-pick — the window cannot start 5 cm \
                         earlier, so the pick could not be checked";
        assert_eq!(
            short_onset_rule(unchecked, 10483, &enforced(2.0, None), failed(None)),
            with_row("no re-pick — window cannot start 5 cm earlier")
        );
        let passed_rule = "AIC change-point pick over a 10.0 ms window; window start at sample \
                           10463, causal bound enforced";
        let passed = short_onset_rule(
            passed_rule,
            10483,
            &enforced(2.0, None),
            Some(EdgeGuard::Passed),
        );
        assert_eq!(
            passed,
            with_row("window start 5 cm earlier: pick moves ≤ 1 sample"),
            "a passed guard is stated, not silent"
        );
        let not_run = short_onset_rule(passed_rule, 10483, &enforced(2.0, None), None);
        assert_eq!(not_run, head, "no guard row when the guard did not run");
    }

    /// #346 UX revision 4: row 2 under `arrival` is one fixed string. Tested
    /// against the rejected text: it names no onset and points nowhere
    /// `(below)`, because no per-capture condition decides the arrival.
    #[test]
    fn arrival_source_line_names_the_peak_rule() {
        let line = arrival_source_line(&ArrivalSource::Peak);
        assert_eq!(
            line,
            format!("{CONT_INDENT}from peak (largest magnitude sample)")
        );
        assert!(!line.contains("onset"), "{line:?}");
        assert!(!line.contains("(below)"), "{line:?}");
        assert!(line.chars().count() <= 80, "{line:?}");
    }

    /// #346 UX revision 4: the gap row says `not the arrival` on every
    /// onset standing, and is absent when the onset is not before the peak.
    #[test]
    fn onset_gap_line_always_says_not_the_arrival() {
        let mut stats = stats_with(None, ArrivalCheck::Agree);
        stats.peak_index = 10_503;
        stats.onset_index = 10_483;
        for standing in [
            OnsetStanding::Unscored,
            OnsetStanding::NoCausalBound,
            OnsetStanding::BoundNotBinding,
            OnsetStanding::PickerDeclined,
            OnsetStanding::PickOnWindowStart,
            OnsetStanding::EdgeFollowing {
                repick: Some(10_474),
            },
            OnsetStanding::EdgeFollowing { repick: None },
            OnsetStanding::DeconvolutionFailed,
        ] {
            stats.onset_standing = standing;
            assert_eq!(
                onset_gap_line(&stats),
                Some(format!(
                    "{CONT_INDENT}20 samples before peak, not the arrival"
                )),
                "{standing:?}"
            );
        }
        stats.onset_index = stats.peak_index;
        assert_eq!(onset_gap_line(&stats), None);
    }

    /// #460 UX: the `ref latency` line in `calibrate`'s `Delay:` format, and
    /// unavailable reasons with their `check:` part on its own line.
    #[test]
    fn reference_latency_lines_print_the_measured_tau_or_the_reason() {
        let measured = ReferenceLatency::Measured(MeasuredReferenceLatency {
            tau_s: 1711.0 / 96_000.0,
            pre_impulse_snr_db: Some(61.8),
            pre_impulse_snr_floor_db: Some(64.5),
            method: "farina_same_capture_reference_v1".into(),
            output_port: "system:playback_2".into(),
            input_port: "system:capture_2".into(),
        });
        assert_eq!(
            reference_latency_lines(Some(&measured), 96_000),
            vec!["  ref latency   17.8229 ms  (1711 samples, SNR 61.8 dB, same capture)"]
        );

        let silent_floor = ReferenceLatency::Measured(MeasuredReferenceLatency {
            tau_s: 20.0 / 48_000.0,
            pre_impulse_snr_db: None,
            pre_impulse_snr_floor_db: None,
            method: "farina_same_capture_reference_v1".into(),
            output_port: "fake:playback_1".into(),
            input_port: "fake:capture_1".into(),
        });
        assert_eq!(
            reference_latency_lines(Some(&silent_floor), 48_000),
            vec!["  ref latency    0.4167 ms  (20 samples, SNR ∞ dB, same capture)"]
        );

        let refused = ReferenceLatency::Unavailable {
            reason:
                "peak SNR 9.3 dB, need 24.0 dB; check: reference loopback cable, ref input gain"
                    .into(),
        };
        assert_eq!(
            reference_latency_lines(Some(&refused), 96_000),
            vec![
                "  ref latency   unavailable — peak SNR 9.3 dB, need 24.0 dB",
                "                check: reference loopback cable, ref input gain",
            ]
        );

        let none_configured = ReferenceLatency::Unavailable {
            reason: "no reference configured (ac setup reference)".into(),
        };
        assert_eq!(
            reference_latency_lines(Some(&none_configured), 96_000),
            vec!["  ref latency   unavailable — no reference configured (ac setup reference)"]
        );
        assert_eq!(reference_latency_lines(None, 96_000).len(), 1);
    }

    fn measured_tau(tau_s: f64, period_size: Option<u32>) -> InterfaceLatency {
        InterfaceLatency::Measured(MeasuredLatency {
            tau_s,
            measured_at: "2026-09-15T09:10:40Z".into(),
            method: "farina_short_ess".into(),
            backend: "fake".into(),
            sample_rate_hz: 96_000,
            period_size,
            output_port: "system:playback_0".into(),
            input_port: "system:capture_0".into(),
            enumeration: Some(EnumerationCheck::Same),
        })
    }

    /// #359: the `latency` line — the value `flight time` subtracts — plus
    /// its measured-date line, and the schema-version-dependent wording
    /// when it was never recorded at all.
    #[test]
    fn interface_latency_lines_print_the_measured_tau_or_the_reason() {
        let measured = measured_tau(1711.4 / 96_000.0, Some(1024));
        let lines = interface_latency_lines(Some(&measured), 10, 96_000, "2026-09-16T11:30:40Z");
        assert_eq!(lines.len(), 3);
        assert!(
            lines[0].starts_with(&label_prefix("latency")),
            "{:?}",
            lines[0]
        );
        assert!(lines[0].contains("ms"), "{:?}", lines[0]);
        assert!(
            lines[0].contains("1711.4 samples, stored"),
            "{:?}",
            lines[0]
        );
        assert_eq!(
            lines[1],
            format!("{CONT_INDENT}measured 2026-09-15T09:10:40Z, 26.3 h before capture")
        );
        assert_eq!(
            lines[2],
            format!("{CONT_INDENT}same device enumeration as this capture")
        );

        let refused = InterfaceLatency::Unavailable {
            reason: "no calibration stored for this channel pair".into(),
        };
        let lines = interface_latency_lines(Some(&refused), 9, 96_000, "2026-09-16T11:30:40Z");
        assert_eq!(
            lines,
            vec![format!(
                "{}none \u{2014} no calibration stored for this channel pair",
                label_prefix("latency")
            )]
        );

        assert_eq!(
            interface_latency_lines(None, 4, 48_000, "2026-09-16T11:30:40Z"),
            vec![format!(
                "{}not recorded (report predates schema v5)",
                label_prefix("latency")
            )]
        );
        assert_eq!(
            interface_latency_lines(None, 9, 48_000, "2026-09-16T11:30:40Z"),
            vec![format!("{}not recorded", label_prefix("latency"))]
        );
    }

    fn host_rebooted() -> EnumerationCheck {
        EnumerationCheck::Crossed {
            boundary: "host rebooted".into(),
            since: Some("2026-09-16T13:41:52Z".into()),
        }
    }

    fn re_enumerated(since: Option<&str>, nodes: &str) -> EnumerationCheck {
        EnumerationCheck::Crossed {
            boundary: format!("audio device re-enumerated; nodes: {nodes}"),
            since: since.map(str::to_string),
        }
    }

    fn with_check(check: Option<EnumerationCheck>) -> InterfaceLatency {
        match measured_tau(1711.0 / 96_000.0, Some(256)) {
            InterfaceLatency::Measured(m) => InterfaceLatency::Measured(MeasuredLatency {
                enumeration: check,
                ..m
            }),
            other => other,
        }
    }

    /// #461 UX: every enumeration state under `latency`, in the order
    /// measured → verdict → nodes → check, with the exact wording.
    #[test]
    fn interface_latency_lines_render_every_enumeration_state() {
        let ts = "2026-09-16T13:52:10Z";
        let tail = |check: Option<EnumerationCheck>| {
            interface_latency_lines(Some(&with_check(check)), 10, 96_000, ts)[2..].to_vec()
        };
        let c = CONT_INDENT;
        assert_eq!(
            tail(Some(host_rebooted())),
            vec![
                format!("{c}UNVERIFIED \u{2014} host rebooted at 2026-09-16T13:41:52Z"),
                format!("{c}check: re-run `ac calibrate` with loopback patched"),
            ]
        );
        assert_eq!(
            tail(Some(re_enumerated(
                Some("2026-09-16T00:08:31Z"),
                "/dev/fw1 new, /dev/snd/controlC1 re-created, /dev/fw2 gone"
            ))),
            vec![
                format!(
                    "{c}UNVERIFIED \u{2014} audio device re-enumerated at 2026-09-16T00:08:31Z"
                ),
                format!("{c}nodes: /dev/fw1 new, /dev/snd/controlC1 re-created,"),
                format!("{c}       /dev/fw2 gone"),
                format!("{c}check: re-run `ac calibrate` with loopback patched"),
            ]
        );
        assert_eq!(
            tail(Some(re_enumerated(None, "/dev/fw2 gone"))),
            vec![
                format!("{c}UNVERIFIED \u{2014} audio device re-enumerated after measurement"),
                format!("{c}nodes: /dev/fw2 gone"),
                format!("{c}check: re-run `ac calibrate` with loopback patched"),
            ]
        );
        let not_recorded = vec![
            format!("{c}UNVERIFIED \u{2014} entry predates enumeration tracking"),
            format!("{c}check: re-run `ac calibrate` with loopback patched"),
        ];
        assert_eq!(tail(Some(EnumerationCheck::NotRecorded)), not_recorded);
        assert_eq!(tail(None), not_recorded, "absent must never read as same");
        assert_eq!(
            tail(Some(EnumerationCheck::NotObservable {
                reason: "cpal backend has no enumeration probe".into()
            })),
            vec![
                format!("{c}UNVERIFIED \u{2014} device enumeration not observable"),
                format!("{c}cpal backend has no enumeration probe"),
            ]
        );
        assert_eq!(
            tail(Some(EnumerationCheck::NotObservable {
                reason: "jack backend: no /dev/snd/controlC* or /dev/fw* nodes; \
                         check: /dev and /proc readable by the daemon user"
                    .into()
            })),
            vec![
                format!("{c}UNVERIFIED \u{2014} device enumeration not observable"),
                format!("{c}jack backend: no /dev/snd/controlC* or /dev/fw* nodes"),
                format!("{c}check: /dev and /proc readable by the daemon user"),
            ]
        );
    }

    /// #461 UX: `ref stored` crossed with an agreeing `ref Δ` points at that
    /// evidence and drops `check:`; the cannot-tell states stay UNVERIFIED
    /// even then, and `latency` is never cleared by `ref Δ`.
    #[test]
    fn reference_stored_crossed_with_an_agreeing_delta_points_at_the_delta() {
        let ts = "2026-09-16T13:52:10Z";
        let c = CONT_INDENT;
        let stored = with_check(Some(host_rebooted()));
        assert_eq!(
            reference_stored_latency_lines(Some(&stored), 10, 96_000, ts, true)[2..].to_vec(),
            vec![format!(
                "{c}host rebooted at 2026-09-16T13:41:52Z \u{2014} see ref \u{394}"
            )]
        );
        let stored = with_check(Some(re_enumerated(
            Some("2026-09-16T00:08:31Z"),
            "/dev/fw1 new",
        )));
        assert_eq!(
            reference_stored_latency_lines(Some(&stored), 10, 96_000, ts, true)[2..].to_vec(),
            vec![
                format!(
                    "{c}audio device re-enumerated at 2026-09-16T00:08:31Z \u{2014} see ref \u{394}"
                ),
                format!("{c}nodes: /dev/fw1 new"),
            ]
        );
        assert_eq!(
            reference_stored_latency_lines(Some(&stored), 10, 96_000, ts, false)[2],
            format!("{c}UNVERIFIED \u{2014} audio device re-enumerated at 2026-09-16T00:08:31Z")
        );
        let unrecorded = with_check(Some(EnumerationCheck::NotRecorded));
        assert!(
            reference_stored_latency_lines(Some(&unrecorded), 10, 96_000, ts, true)[2]
                .contains("UNVERIFIED"),
            "no boundary was named for the Δ to answer"
        );
        assert_eq!(
            enumeration_lines(Some(&EnumerationCheck::Same), true),
            enumeration_lines(Some(&EnumerationCheck::Same), false)
        );
    }

    /// #461 UX: the words the mechanism would suggest never reach operator
    /// text, and nothing opaque (boot id, session) is printed.
    #[test]
    fn enumeration_lines_never_name_a_mechanism() {
        for check in [
            Some(host_rebooted()),
            Some(re_enumerated(None, "/dev/fw2 gone")),
            Some(EnumerationCheck::NotRecorded),
            Some(EnumerationCheck::Same),
            None,
        ] {
            for line in enumeration_lines(check.as_ref(), false) {
                for word in ["mechanism", "SYT", "phase", "boot id", "session"] {
                    assert!(!line.contains(word), "{line:?} names {word}");
                }
            }
        }
    }

    /// #359: the `ref stored` line mirrors `latency`, but for the
    /// reference pair, and its own schema-version threshold (v9, not v5).
    #[test]
    fn reference_stored_latency_lines_print_the_measured_tau_or_the_reason() {
        let measured = measured_tau(57.0 / 96_000.0, Some(64));
        let lines = reference_stored_latency_lines(
            Some(&measured),
            10,
            96_000,
            "2026-09-16T11:30:40Z",
            false,
        );
        assert_eq!(lines.len(), 3);
        assert!(
            lines[0].starts_with(&label_prefix("ref stored")),
            "{:?}",
            lines[0]
        );

        assert_eq!(
            reference_stored_latency_lines(None, 8, 96_000, "2026-09-16T11:30:40Z", false),
            vec![format!(
                "{}not recorded (report predates schema v9)",
                label_prefix("ref stored")
            )]
        );
        assert_eq!(
            reference_stored_latency_lines(None, 9, 96_000, "2026-09-16T11:30:40Z", false),
            vec![format!(
                "{}not looked up \u{2014} no reference configured",
                label_prefix("ref stored")
            )]
        );
    }

    /// #359 AC2/AC4: the `ref Δ` block on a detected period shift uses
    /// #347's own phrases verbatim, so the two faults are recognisably the
    /// same fault.
    #[test]
    fn arrival_check_lines_name_a_period_shift_with_347s_own_wording() {
        let d = TauDisagreement {
            reading1_s: 1711.0 / 96_000.0,
            reading2_s: 2735.0 / 96_000.0,
            delta_samples: 1024,
            sample_rate: 96_000,
            period_size: Some(1024),
            periods: Some(1),
        };
        let lines = arrival_check_lines(&ArrivalCheck::PeriodShift(d), Some(1024));
        assert_eq!(lines.len(), 5);
        assert!(lines[0].contains("+1024 samples"), "{:?}", lines[0]);
        assert!(
            lines[1].contains("exactly 1 period of 1024 samples"),
            "{:?}",
            lines[1]
        );
        assert!(
            lines[2].contains("a graph-buffering shift, not hardware drift"),
            "{:?}",
            lines[2]
        );
        assert!(
            lines[3].contains("stored 1711.000 samples \u{2192} this capture 2735.000 samples"),
            "{:?}",
            lines[3]
        );
        assert!(lines[4].contains("check:"), "{:?}", lines[4]);
    }

    /// #359 AC3, tested against the rejected implementation: a mismatch
    /// must never print any of the period-shift phrases.
    #[test]
    fn arrival_check_lines_a_mismatch_never_reads_as_a_period_shift() {
        let d = TauDisagreement {
            reading1_s: 1711.0 / 96_000.0,
            reading2_s: 1727.0 / 96_000.0,
            delta_samples: 16,
            sample_rate: 96_000,
            period_size: Some(1024),
            periods: None,
        };
        let lines = arrival_check_lines(&ArrivalCheck::Mismatch(d), Some(1024));
        assert_eq!(lines.len(), 4);
        assert!(lines[1].contains("not a period multiple"), "{:?}", lines[1]);
        for line in &lines {
            assert!(!line.contains("period shift"), "{line:?}");
            assert!(!line.contains("graph-buffering"), "{line:?}");
        }
    }

    /// #363's own rule, applied here: the evidence prints as a number, not
    /// a verdict — `agree` never appears.
    #[test]
    fn arrival_check_lines_agree_prints_zero_samples_not_a_verdict() {
        let lines = arrival_check_lines(&ArrivalCheck::Agree, Some(64));
        assert_eq!(
            lines,
            vec![format!(
                "{}0 samples  (same capture \u{2212} stored, one period = 64)",
                label_prefix("ref \u{394}")
            )]
        );
        assert!(!lines[0].to_lowercase().contains("agree"));
    }

    /// The architect's own fixed line: the CLI never restates `Unchecked`'s
    /// `reason` — it is already on the `latency`/`ref stored` lines above.
    #[test]
    fn arrival_check_lines_unchecked_prints_the_fixed_line() {
        let lines = arrival_check_lines(
            &ArrivalCheck::Unchecked {
                reason: "no same-capture reference in this report".into(),
            },
            None,
        );
        assert_eq!(
            lines,
            vec![format!(
                "{}not checked \u{2014} needs ref latency and ref stored (above)",
                label_prefix("ref \u{394}")
            )]
        );
    }

    /// #359 QA (PR #477): `flight_time_line` is the value the arrival check
    /// exists to gate — the CLI's headline new read-out — but shipped with
    /// no test naming any of its four branches. Each arm here is the one
    /// that fails if the match reorders or the wording drifts from what
    /// `flight_time_line` actually prints.
    fn stats_with(flight_time_s: Option<f64>, arrival_check: ArrivalCheck) -> IrStats {
        IrStats {
            sample_rate_hz: 96_000,
            window_len: 1024,
            peak_index: 600,
            peak_magnitude: 0.5,
            onset_index: 590,
            onset_rule: String::new(),
            causal_bound: unbounded(),
            arrival_source: ArrivalSource::Peak,
            onset_standing: OnsetStanding::NoCausalBound,
            delay_samples: 88,
            arrival_s: 88.0 / 96_000.0,
            arrival_check,
            flight_time_s,
            interface_latency_enumeration: Some(EnumerationCheck::Same),
            pre_impulse_snr_db: 40.0,
            gate_window_s: 0.01,
            gate_f_low_hz: 100.0,
            gate_window_kind: "tukey".into(),
            verdict: IrVerdict::Ok,
        }
    }

    #[test]
    fn flight_time_line_names_every_branch_the_ux_spec_requires() {
        let agree = stats_with(Some(88.0 / 96_000.0), ArrivalCheck::Agree);
        let lines = flight_time_line(&agree);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("samples"), "{lines:?}");
        assert!(
            !lines[0].contains("withheld") && !lines[0].contains("not shown"),
            "{lines:?}"
        );
        assert_eq!(
            lines[0],
            format!(
                "{}+88 samples  (+0.917 ms, arrival \u{2212} latency)",
                label_prefix("flight time")
            )
        );

        let shift_d = TauDisagreement {
            reading1_s: 0.0,
            reading2_s: 1024.0 / 96_000.0,
            delta_samples: 1024,
            sample_rate: 96_000,
            period_size: Some(1024),
            periods: Some(1),
        };
        assert_eq!(
            flight_time_line(&stats_with(None, ArrivalCheck::PeriodShift(shift_d))),
            vec![format!(
                "{}withheld \u{2014} ref \u{394} is a period shift (below)",
                label_prefix("flight time")
            )]
        );

        let mismatch_d = TauDisagreement {
            reading1_s: 0.0,
            reading2_s: 16.0 / 96_000.0,
            delta_samples: 16,
            sample_rate: 96_000,
            period_size: Some(1024),
            periods: None,
        };
        assert_eq!(
            flight_time_line(&stats_with(None, ArrivalCheck::Mismatch(mismatch_d))),
            vec![format!(
                "{}withheld \u{2014} ref \u{394} is not zero (below)",
                label_prefix("flight time")
            )]
        );

        assert_eq!(
            flight_time_line(&stats_with(
                None,
                ArrivalCheck::Unchecked {
                    reason: "no same-capture reference in this report".into(),
                }
            )),
            vec![format!(
                "{}not shown \u{2014} no stored latency for this pair (below)",
                label_prefix("flight time")
            )]
        );
    }

    /// codex-qa on PR #477: a `Some` flight time alongside
    /// `ArrivalCheck::Unchecked` (case D — a flight time was computed, but
    /// the same-capture corroboration never ran, e.g. this pair's `ref
    /// stored` misses on exact conditions) printed identically to a checked
    /// value. The UX comment's field justification for
    /// `reference check not run — see ref Δ` is explicit: "The architect's
    /// `Unchecked` still passes a flight time through. This line keeps that
    /// value from reading as checked." Asserts the continuation line is
    /// present, 16-space indented, and that the checked (`Agree`) case does
    /// not carry it.
    /// #461 UX: a flight time over a non-`same` stored τ carries
    /// `latency UNVERIFIED — see latency (below)` directly under the value,
    /// above the reference-check line. A v9 report (no check) is flagged.
    #[test]
    fn flight_time_line_flags_an_unverified_latency_above_the_reference_line() {
        let c = CONT_INDENT;
        let value = format!(
            "{}+88 samples  (+0.917 ms, arrival \u{2212} latency)",
            label_prefix("flight time")
        );
        let flagged = format!("{c}latency UNVERIFIED \u{2014} see latency (below)");
        for check in [
            None,
            Some(EnumerationCheck::NotRecorded),
            Some(EnumerationCheck::Crossed {
                boundary: "host rebooted".into(),
                since: None,
            }),
        ] {
            let mut stats = stats_with(
                Some(88.0 / 96_000.0),
                ArrivalCheck::Unchecked {
                    reason: String::new(),
                },
            );
            stats.interface_latency_enumeration = check.clone();
            assert_eq!(
                flight_time_line(&stats),
                vec![
                    value.clone(),
                    flagged.clone(),
                    format!("{c}reference check not run \u{2014} see ref \u{394}"),
                ],
                "{check:?}"
            );
            stats.arrival_check = ArrivalCheck::Agree;
            assert_eq!(
                flight_time_line(&stats),
                vec![value.clone(), flagged.clone()],
                "{check:?}"
            );
        }
        // Withheld: nothing to qualify.
        let mut withheld = stats_with(
            None,
            ArrivalCheck::Unchecked {
                reason: String::new(),
            },
        );
        withheld.interface_latency_enumeration = None;
        assert_eq!(flight_time_line(&withheld).len(), 1);
    }

    #[test]
    fn flight_time_line_qualifies_an_unchecked_value_as_not_verified() {
        let unchecked = stats_with(
            Some(88.0 / 96_000.0),
            ArrivalCheck::Unchecked {
                reason: "no same-capture reference in this report".into(),
            },
        );
        assert_eq!(
            flight_time_line(&unchecked),
            vec![
                format!(
                    "{}+88 samples  (+0.917 ms, arrival \u{2212} latency)",
                    label_prefix("flight time")
                ),
                format!("{CONT_INDENT}reference check not run \u{2014} see ref \u{394}"),
            ]
        );

        let agree = stats_with(Some(88.0 / 96_000.0), ArrivalCheck::Agree);
        assert_eq!(
            flight_time_line(&agree).len(),
            1,
            "a checked flight time must not carry the unchecked qualifier"
        );
    }

    /// Every line the onset block and the `ref latency` read-out can emit must
    /// fit 80 columns at the indent `print_ir_report` uses (16, #346 UX) — at a 6-digit
    /// sample index, the widest distance and temperature the #460 UX pass
    /// measured, every decline case, and every reference reason.
    #[test]
    fn onset_and_reference_lines_fit_eighty_columns() {
        let mut rules = vec![
            "AIC change-point pick over a 10.0 ms window; window start at sample 262144, \
             causal bound enforced; pick landed on the window start — the true onset may \
             lie earlier"
                .to_string(),
            "AIC change-point pick over a 10.0 ms window; window start at sample 262144, \
             causal bound enforced at sample 9, search span is the tighter limit"
                .to_string(),
            "AIC change-point pick over a 10.0 ms window; window start at sample 262144, \
             no causal bound (no distance, reference latency unavailable)"
                .to_string(),
        ];
        // Every decline case `estimate_onset` can name, so a new one that
        // does not fit is caught here rather than at a rig terminal.
        for case in [
            "search window shorter than 2 samples",
            "zero variance in the search window",
            "peak at sample 0",
            "nothing in the search window above the pre-impulse floor",
            "no change point earlier than the peak in the window",
            "causal bound at or after the peak",
        ] {
            rules.push(format!(
                "onset picker declined ({case}) — index is the peak, not an onset"
            ));
        }
        let bounds = [
            enforced(12.25, Some(-10.5)),
            enforced(12.25, None),
            CausalBound::Unavailable(MissingBoundInput::Distance),
            CausalBound::Unavailable(MissingBoundInput::ReferenceLatency {
                reason: String::new(),
            }),
            unbounded(),
        ];
        for rule in &rules {
            for bound in &bounds {
                for guard in [
                    None,
                    Some(EdgeGuard::Passed),
                    Some(EdgeGuard::Failed { repick: None }),
                    Some(EdgeGuard::Failed {
                        repick: Some(123_456),
                    }),
                ] {
                    for line in short_onset_rule(rule, 262_144, bound, guard) {
                        assert!(
                            16 + line.chars().count() <= 80,
                            "line {:?} runs to {} columns",
                            line,
                            16 + line.chars().count()
                        );
                    }
                }
            }
        }

        let long_tau = ReferenceLatency::Measured(MeasuredReferenceLatency {
            tau_s: 0.123_456_7,
            pre_impulse_snr_db: Some(104.3),
            pre_impulse_snr_floor_db: Some(107.0),
            method: String::new(),
            output_port: String::new(),
            input_port: String::new(),
        });
        let mut references = vec![long_tau];
        for reason in [
            "no reference configured (ac setup reference)",
            "backend cpal cannot capture a reference",
            "peak SNR 9.3 dB, need 24.0 dB; check: reference loopback cable, ref input gain",
            "peak at reference window edge; check: reference loopback routing, capture tail",
            "xrun during capture; check: JACK period size, system load",
            "tail 0.08 s, reference window needs 0.10 s; check: lengthen the tail token (e.g. 0.8s)",
        ] {
            references.push(ReferenceLatency::Unavailable {
                reason: reason.to_string(),
            });
        }
        for reference in &references {
            for line in reference_latency_lines(Some(reference), 192_000) {
                assert!(
                    line.chars().count() <= 80,
                    "line {:?} runs to {} columns",
                    line,
                    line.chars().count()
                );
            }
        }

        // #359: `latency` / `ref stored` must wrap a long `TauRefusal`
        // message (one that lists every differing field) at 80 columns
        // rather than running off the edge.
        let long_refusal = InterfaceLatency::Unavailable {
            reason: "no \u{3c4} entry for these exact conditions; nearest stored entry \
                     (measured 2026-09-15T09:10:40Z) differs in device (requested 1, stored 0), \
                     backend (requested jack, stored fake), sample_rate (requested 96000 Hz, \
                     stored 48000 Hz), period_size (requested 1024, stored n/a), output_port \
                     (requested system:playback_4, stored system:playback_3), input_port \
                     (requested system:capture_4, stored system:capture_3)"
                .to_string(),
        };
        for line in interface_latency_lines(Some(&long_refusal), 9, 96_000, "2026-09-16T11:30:40Z")
        {
            assert!(
                line.chars().count() <= 80,
                "line {:?} runs to {} columns",
                line,
                line.chars().count()
            );
        }
        for line in reference_stored_latency_lines(
            Some(&long_refusal),
            9,
            96_000,
            "2026-09-16T11:30:40Z",
            false,
        ) {
            assert!(
                line.chars().count() <= 80,
                "line {:?} runs to {} columns",
                line,
                line.chars().count()
            );
        }

        // #461: every enumeration verdict, at the widest node list and
        // reason the UX pass measured.
        for check in [
            Some(EnumerationCheck::Crossed {
                boundary: "host rebooted".into(),
                since: Some("2026-09-16T13:41:52Z".into()),
            }),
            Some(EnumerationCheck::Crossed {
                boundary: "audio device re-enumerated; nodes: /dev/fw1 new, \
                           /dev/snd/controlC1 re-created, /dev/snd/controlC12 re-created, \
                           /dev/fw2 gone, /dev/fw10 gone"
                    .into(),
                since: Some("2026-09-16T00:08:31Z".into()),
            }),
            Some(EnumerationCheck::NotObservable {
                reason: "jack backend: /proc/sys/kernel/random/boot_id unreadable; \
                         check: /dev and /proc readable by the daemon user"
                    .into(),
            }),
            Some(EnumerationCheck::NotRecorded),
            None,
        ] {
            for agrees in [false, true] {
                for line in enumeration_lines(check.as_ref(), agrees) {
                    assert!(
                        line.chars().count() <= 80,
                        "line {:?} runs to {} columns",
                        line,
                        line.chars().count()
                    );
                }
            }
        }

        // #359: the `ref Δ` block at the widest figures the UX pass
        // measured (six-digit sample counts).
        let period_shift = TauDisagreement {
            reading1_s: 171_100.0 / 96_000.0,
            reading2_s: 273_500.0 / 96_000.0,
            delta_samples: 102_400,
            sample_rate: 96_000,
            period_size: Some(1024),
            periods: Some(100),
        };
        let mismatch = TauDisagreement {
            reading1_s: 171_100.0 / 96_000.0,
            reading2_s: 172_700.0 / 96_000.0,
            delta_samples: 1_600,
            sample_rate: 96_000,
            period_size: Some(1024),
            periods: None,
        };
        for check in [
            ArrivalCheck::PeriodShift(period_shift),
            ArrivalCheck::Mismatch(mismatch),
            ArrivalCheck::Agree,
            ArrivalCheck::Unchecked {
                reason: String::new(),
            },
        ] {
            for line in arrival_check_lines(&check, Some(1024)) {
                assert!(
                    line.chars().count() <= 80,
                    "line {:?} runs to {} columns",
                    line,
                    line.chars().count()
                );
            }
        }

        // #359 QA (PR #477): `flight_time_line` joins this width-fit test
        // too, per the UX comment's own instruction — widest `Some` case
        // is a 96 kHz five-digit sample count, plus both withheld cases.
        let wide_shift = TauDisagreement {
            reading1_s: 17_110.0 / 96_000.0,
            reading2_s: 18_134.0 / 96_000.0,
            delta_samples: 1024,
            sample_rate: 96_000,
            period_size: Some(1024),
            periods: Some(1),
        };
        let wide_mismatch = TauDisagreement {
            reading1_s: 17_110.0 / 96_000.0,
            reading2_s: 17_126.0 / 96_000.0,
            delta_samples: 16,
            sample_rate: 96_000,
            period_size: Some(1024),
            periods: None,
        };
        let flight_time_cases = [
            flight_time_line(&stats_with(Some(65_535.0 / 96_000.0), ArrivalCheck::Agree)),
            flight_time_line(&stats_with(None, ArrivalCheck::PeriodShift(wide_shift))),
            flight_time_line(&stats_with(None, ArrivalCheck::Mismatch(wide_mismatch))),
            flight_time_line(&stats_with(
                None,
                ArrivalCheck::Unchecked {
                    reason: String::new(),
                },
            )),
            // codex-qa on PR #477: the `Some` + `Unchecked` continuation
            // line (`reference check not run — see ref Δ`) joins this
            // width-fit test too, at the same 96 kHz five-digit sample
            // count as the `Agree` case above.
            flight_time_line(&stats_with(
                Some(65_535.0 / 96_000.0),
                ArrivalCheck::Unchecked {
                    reason: String::new(),
                },
            )),
        ];
        for lines in flight_time_cases {
            for line in lines {
                assert!(
                    line.chars().count() <= 80,
                    "line {:?} runs to {} columns",
                    line,
                    line.chars().count()
                );
            }
        }
    }
}
