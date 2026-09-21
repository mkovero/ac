use super::{
    await_session_check, check_ack, consumes_voltage, get_cal, level_to_dbfs, level_unit,
    print_consumer_check, print_level, print_level_range, voltage_scale, Scale,
};
use crate::client::AcClient;
use crate::io;
use crate::parse::CommandKind;
use ac_core::measurement::report::{MeasurementReport, ReportReadError};
use ac_core::shared::calibration::LayerVerdict;

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

    let mut cal = get_cal(client);
    if cal.is_some() {
        println!("  Loaded calibration from server.");
    } else {
        println!("  No calibration found \u{2014} levels in dBFS only.");
    }
    let level_db = level_to_dbfs(level, cal.as_ref());
    let consumes = consumes_voltage(cal.as_ref(), Some(level));

    let start_hz = start.unwrap_or(cfg.range_start_hz);
    let stop_hz = stop.unwrap_or(cfg.range_stop_hz);

    println!("\n  Plot: {start_hz:.0} \u{2192} {stop_hz:.0} Hz  {ppd} pts/decade");

    let mut cmd_json = serde_json::json!({
        "cmd": "plot",
        "start_hz": start_hz,
        "stop_hz": stop_hz,
        "level_dbfs": level_db,
        "ppd": ppd,
        "level_unit": level_unit(level),
    });
    if let Some(b) = bpo {
        cmd_json["bpo"] = serde_json::json!(b);
    }
    let ack = check_ack(client.send_cmd(&cmd_json, None), "plot");
    // #466: the level block and the table wait for the session check; a
    // refused scale leaves the table in dBFS.
    let wait = await_session_check(client, "plot", &ack, 30_000);
    if print_consumer_check(wait, &mut cal, Some(level), consumes).is_err() {
        std::process::exit(1);
    }
    let have_cal = matches!(voltage_scale(cal.as_ref()), Some(Scale::Usable(..)));
    io::print_freq_header(have_cal);
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

    let mut cal = get_cal(client);
    if cal.is_some() {
        println!("  Loaded calibration from server.");
    } else {
        println!("  No calibration found \u{2014} levels in dBFS only.");
    }
    let start_db = level_to_dbfs(start, cal.as_ref());
    let stop_db = level_to_dbfs(stop, cal.as_ref());
    let typed = if level_unit(start) != "dbfs" {
        start
    } else {
        stop
    };
    let consumes = consumes_voltage(cal.as_ref(), Some(typed));

    println!("\n  Plot level: {freq:.0} Hz  |  {steps} steps");

    let ack = check_ack(
        client.send_cmd(
            &serde_json::json!({
                "cmd": "plot_level",
                "freq_hz": freq,
                "start_dbfs": start_db,
                "stop_dbfs": stop_db,
                "steps": steps,
                "level_unit": level_unit(typed),
            }),
            None,
        ),
        "plot_level",
    );
    let wait = await_session_check(client, "plot_level", &ack, 30_000);
    if print_consumer_check(wait, &mut cal, Some(typed), consumes).is_err() {
        std::process::exit(1);
    }
    let have_cal = matches!(voltage_scale(cal.as_ref()), Some(Scale::Usable(..)));
    io::print_freq_header(have_cal);
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
pub fn run_ir(cmd: &CommandKind, client: &mut AcClient) {
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

    let mut cal = get_cal(client);
    let have_cal = cal.is_some();
    if have_cal {
        println!("  Loaded calibration from server.");
    } else {
        println!("  No calibration found \u{2014} levels in dBFS only.");
    }
    let level_db = level_to_dbfs(level, cal.as_ref());
    // #466: `plot ir` consumes a stored τ as well as a voltage scale.
    let stored_tau = cal
        .as_ref()
        .and_then(|c| c.get("tau_history"))
        .and_then(|h| h.as_array())
        .is_some_and(|h| !h.is_empty());
    let consumes = stored_tau || consumes_voltage(cal.as_ref(), Some(level));

    // Only typed fields go on the wire: the daemon applies `ac-core`'s
    // defaults to the rest and echoes what it accepted (#501).
    let mut cmd_json = serde_json::json!({
        "cmd": "plot_ir",
        "level_dbfs": level_db,
        "level_unit": level_unit(level),
    });
    if let Some(v) = f1 {
        cmd_json["f1_hz"] = serde_json::json!(v);
    }
    if let Some(v) = f2 {
        cmd_json["f2_hz"] = serde_json::json!(v);
    }
    if let Some(v) = duration {
        cmd_json["duration"] = serde_json::json!(v);
    }
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
    // #501 UX: the whole stimulus block prints after the ack, because
    // every value in it is the daemon's echo, defaults applied.
    let typed = IrTyped {
        f1: f1.is_some(),
        f2: f2.is_some(),
        duration: duration.is_some(),
        window: window_len.is_some(),
        n_harmonics: n_harmonics.is_some(),
        tail: tail_s.is_some(),
    };
    println!();
    for line in ir_stimulus_lines(&ack, typed) {
        println!("{line}");
    }
    // #460 UX: echo the typed distance before emission, so a token typo
    // (`0.8m` meant as `0.8s`) is visible before the result, and state its
    // absence rather than hide it.
    match distance_m {
        Some(d) => println!("  distance   {d} m"),
        None => println!("  distance   not given"),
    }
    let out_port = ack.get("out_port").and_then(|v| v.as_str());
    if let Some(p) = out_port {
        println!("  output     {p}");
    }
    // #460 UX: every port the sweep leaves through or is referenced against,
    // printed before the result, on the same label grid as the stimulus
    // rows and `level`. A reference output equal to the main output drives nothing
    // extra, so it is not named twice.
    if let Some(p) = ack.get("ref_out_port").and_then(|v| v.as_str()) {
        if Some(p) != out_port {
            println!("  ref out    {p}");
        }
    }
    if let Some(p) = ack.get("ref_in_port").and_then(|v| v.as_str()) {
        println!("  ref in     {p}");
    }
    println!("  Running IR measurement...");

    // #466: `plot ir` reports its session check after analysis, before the
    // result; the level block waits for it.
    let wait = await_session_check(client, "plot_ir", &ack, 300_000);
    if print_consumer_check(wait, &mut cal, Some(level), consumes).is_err() {
        std::process::exit(1);
    }
    print_level(
        ack.get("level_dbfs").and_then(|v| v.as_f64()),
        level_defaulted,
        ack.get("max_dbfs").and_then(|v| v.as_f64()),
        cal.as_ref(),
        true,
    );
    println!();

    let (ir_frame, report_frame, done_frame) = collect_ir(client, "plot_ir");
    print_ir_result(
        ir_frame.as_ref(),
        report_frame.as_ref(),
        ack.get("duration").and_then(|v| v.as_f64()),
        ack.get("tail_s").and_then(|v| v.as_f64()),
    );
    // Decoded once through the checked reader (#429): the summary and the
    // notes both read this accepted value, so a refused schema version
    // leaves nothing of the report body to print.
    let report = match decode_ir_report(report_frame.as_ref()) {
        Ok(r) => Some(r),
        Err(line) => {
            eprintln!("{line}");
            None
        }
    };
    if let Some(r) = report.as_ref() {
        print_ir_report(r);
    }
    if let Some(done) = done_frame.as_ref() {
        for line in report_files_lines(done) {
            println!("{line}");
        }
    }
    for line in ir_notes_lines(report.as_ref()) {
        println!("{line}");
    }
}

/// Which `plot ir` stimulus fields the operator typed; the rest are the
/// daemon's defaults (#501).
#[derive(Debug, Clone, Copy, Default)]
struct IrTyped {
    f1: bool,
    f2: bool,
    duration: bool,
    window: bool,
    n_harmonics: bool,
    tail: bool,
}

/// The `level` block's fallback for an ack field an older daemon does not
/// send, verbatim.
const NOT_REPORTED: &str = "(not reported by this daemon)";

fn origin_tag(typed: bool) -> &'static str {
    if typed {
        "(typed)"
    } else {
        "(default)"
    }
}

/// The `IR sweep` block printed before emission (#501 UX): band, length,
/// window, harmonics and tail, each read from the `plot_ir` ack's echo and
/// tagged `typed` or `default`. Decimal points sit on `level`'s column.
/// A defaulted window is shown in seconds, the quantity the daemon holds
/// before the engine rate is known; a typed one in the samples typed.
fn ir_stimulus_lines(ack: &serde_json::Value, typed: IrTyped) -> Vec<String> {
    let f64_of = |key: &str| ack.get(key).and_then(|v| v.as_f64());
    let u64_of = |key: &str| ack.get(key).and_then(|v| v.as_u64());
    let row = |label: &str, value: Option<String>| {
        format!(
            "  {label:<11}{}",
            value.unwrap_or_else(|| NOT_REPORTED.to_string())
        )
    };

    let band_tag = match (typed.f1, typed.f2) {
        (true, true) => "(typed)",
        (false, false) => "(default)",
        (true, false) => "(start typed, stop default)",
        (false, true) => "(start default, stop typed)",
    };
    let band = match (f64_of("f1_hz"), f64_of("f2_hz")) {
        (Some(f1), Some(f2)) => Some(format!("{f1} Hz \u{2192} {f2} Hz  {band_tag}")),
        _ => None,
    };
    let length = f64_of("duration").map(|d| format!("{d:>7.2} s  {}", origin_tag(typed.duration)));
    let window = match (u64_of("window_len"), f64_of("window_default_s")) {
        (Some(n), _) => Some(format!("{n:>4} samples  {}", origin_tag(typed.window))),
        (None, Some(s)) => Some(format!("{s:>7.2} s  {}", origin_tag(typed.window))),
        (None, None) => None,
    };
    let harmonics = u64_of("n_harmonics").map(|n| {
        let unit = if n == 1 { "order" } else { "orders" };
        format!("{n:>4} {unit}  {}", origin_tag(typed.n_harmonics))
    });
    let tail = f64_of("tail_s").map(|t| format!("{t:>7.2} s  {}", origin_tag(typed.tail)));

    vec![
        "  IR sweep".to_string(),
        row("band", band),
        row("length", length),
        row("window", window),
        row("harmonics", harmonics),
        row("tail", tail),
    ]
}

/// The `captured` row: sweep plus tail as the daemon accepted them (#501 —
/// no CLI-side default that could drift from the daemon's).
fn captured_line(duration: Option<f64>, tail_s: Option<f64>) -> String {
    match (duration, tail_s) {
        (Some(d), Some(t)) => format!(
            "  captured      {:.2} s  ({d:.2} s sweep + {t:.2} s tail)",
            d + t
        ),
        _ => format!("  captured      {NOT_REPORTED}"),
    }
}

/// The failed-deconvolution banner (#376), with the places to check
/// (#501 UX): the sweep rows printed above, then the capture chain.
fn deconvolution_failed_lines(reason: &str) -> Vec<String> {
    vec![
        format!("  DECONVOLUTION FAILED \u{2014} {reason}"),
        format!("{CONT_INDENT}check: sweep length, band, window (above)"),
        format!("{CONT_INDENT}check: drive level, input gain, distance, room noise"),
    ]
}

/// The `pre-imp SNR` rows. A finite figure is shown next to the threshold
/// it was compared against, pass or fail, with the threshold's basis under
/// it (#501). A non-finite figure has no comparison to qualify.
fn pre_impulse_snr_lines(stats: &ac_core::measurement::report::IrStats) -> Vec<String> {
    use ac_core::measurement::report::{IrVerdict, PRE_IMPULSE_SNR_BASIS, PRE_IMPULSE_SNR_MIN_DB};
    if stats.pre_impulse_snr_db.is_finite() {
        vec![
            format!(
                "  pre-imp SNR   {:.1} dB  (required \u{2265} {:.1} dB)",
                stats.pre_impulse_snr_db, PRE_IMPULSE_SNR_MIN_DB,
            ),
            format!("{CONT_INDENT}{PRE_IMPULSE_SNR_BASIS}"),
        ]
    } else if let IrVerdict::Failed { reason } = &stats.verdict {
        // Non-finite here means `ir_stats` had nothing to measure a floor
        // from at all (see the reason already printed in the banner
        // above) — restate it rather than a generic "silence" that would
        // misdescribe a zero-peak or guard-band-exhausted capture alike.
        vec![format!("  pre-imp SNR   {reason}")]
    } else {
        // Non-finite but `Ok`: a zero floor against a nonzero peak is the
        // best possible capture, not an unmeasurable one.
        vec!["  pre-imp SNR   \u{221e} dB  (zero measured floor)".to_string()]
    }
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

/// Row 2 under `arrival` (#346 UX revision 4, #537 UX): the rule that
/// produced the arrival, with the corner formatted from the source, never a
/// literal. `ArrivalSource::Peak` arises only when the band did not allow the
/// band-limited rule, so it says `not band-limited`. Takes the source so a
/// new [`ArrivalSource`] variant fails to compile here rather than printing
/// this text silently.
///
/// [`ArrivalSource`]: ac_core::measurement::report::ArrivalSource
fn arrival_source_line(source: &ac_core::measurement::report::ArrivalSource) -> String {
    use ac_core::measurement::report::ArrivalSource;
    let text = match source {
        ArrivalSource::Peak => "from peak (largest magnitude sample), not band-limited".to_string(),
        ArrivalSource::BandLimitedPeak { corner_hz } => format!(
            "from peak of IR high-passed at {} (zero-phase)",
            format_corner(*corner_hz)
        ),
    };
    format!("{CONT_INDENT}{text}")
}

/// A high-pass corner as the #537 rows print it: `1 kHz`, `500 Hz`.
fn format_corner(corner_hz: f64) -> String {
    if corner_hz >= 1000.0 {
        format!("{} kHz", corner_hz / 1000.0)
    } else {
        format!("{corner_hz} Hz")
    }
}

/// The corner of a band-limited arrival, as `above 1 kHz`. `None` when the
/// arrival is the broadband peak.
fn above_corner(stats: &ac_core::measurement::report::IrStats) -> Option<String> {
    match stats.arrival_source {
        ac_core::measurement::report::ArrivalSource::BandLimitedPeak { corner_hz } => {
            Some(format!("above {}", format_corner(corner_hz)))
        }
        ac_core::measurement::report::ArrivalSource::Peak => None,
    }
}

/// Why the band-limited arrival could not run (#537 UX), shared by the three
/// rows that say so.
fn band_limit_unavailable_reason(band_top_hz: f64, required_hz: f64) -> String {
    format!("sweep ends at {band_top_hz:.0} Hz, needs \u{2265} {required_hz:.0} Hz")
}

/// The `earlier peak` row under the source row (#537 UX), on
/// `EarlierComparable` only: the evidence for withholding, placed on the
/// arrival it disputes.
fn earlier_peak_line(stats: &ac_core::measurement::report::IrStats) -> Option<String> {
    use ac_core::measurement::report::ArrivalCrossCheck;
    let ArrivalCrossCheck::EarlierComparable { index, level_db } = stats.arrival_cross_check else {
        return None;
    };
    let above = above_corner(stats).unwrap_or_default();
    Some(format!(
        "{CONT_INDENT}earlier peak {above}: {} samples before, {level_db:+.1} dB",
        stats.arrival_index.saturating_sub(index)
    ))
}

/// The `second lobe` row under the source row (#537 UX revision 2): how far
/// the pick beat its nearest rival half-cycle, with the side it sits on and
/// the threshold, on every band-limited capture — pass or refuse, the same
/// row in the same place. `None` when the arrival is not band-limited.
fn second_lobe_line(stats: &ac_core::measurement::report::IrStats) -> Option<String> {
    use ac_core::measurement::report::ArrivalSource;
    use ac_core::measurement::report::ARRIVAL_LOBE_MARGIN_MIN_DB;
    let margin_db = stats.arrival_lobe_margin_db?;
    let ArrivalSource::BandLimitedPeak { corner_hz } = stats.arrival_source else {
        return None;
    };
    let text = match stats.arrival_lobe_offset {
        Some(offset) => format!(
            "second lobe {} samples {}, {margin_db:.1} dB down (required \u{2265} \
             {ARRIVAL_LOBE_MARGIN_MIN_DB:.1} dB)",
            offset.unsigned_abs(),
            if offset < 0 { "before" } else { "after" }
        ),
        None => {
            let window =
                ac_core::measurement::sweep::lobe_window_samples(stats.sample_rate_hz, corner_hz);
            format!(
                "no second lobe within \u{b1}{window} samples (\u{b1}{:.3} ms)",
                window as f64 / stats.sample_rate_hz as f64 * 1000.0
            )
        }
    };
    Some(format!("{CONT_INDENT}{text}"))
}

/// A signed figure with the typographic minus the #537 rows print (`−25`,
/// `+121`), at `decimals` places.
fn signed_minus(value: f64, decimals: usize) -> String {
    let text = format!("{:.*}", decimals, value.abs());
    let zero = text.chars().all(|c| c == '0' || c == '.');
    if value < 0.0 && !zero {
        format!("\u{2212}{text}")
    } else {
        format!("+{text}")
    }
}

/// The `distance` block (#537 UX revision 3): the flight time against the
/// typed distance — the only row that says anything about the path, and all
/// it says is whether the number fits the window. A verdict prints only when
/// the flight time exists or the distance is a reason it does not;
/// otherwise one row says why it is silent. Without a distance, one row
/// says the flight time was not checked.
fn distance_lines(stats: &ac_core::measurement::report::IrStats) -> Vec<String> {
    use ac_core::measurement::report::{
        DistanceCheck, ARRIVAL_EXCESS_DELAY_ALLOWANCE_S, DISTANCE_SPEED_OF_SOUND_REL_TOL,
        DISTANCE_TAPE_TOLERANCE_M,
    };
    let label = label_prefix("distance");
    let typed = |d: f64| format!("{d} m");
    let fs = stats.sample_rate_hz as f64;
    let (window, excess_s) = match &stats.distance_check {
        DistanceCheck::NotGiven => {
            return vec![format!(
                "{label}not given \u{2014} flight time not checked (token: 1m)"
            )]
        }
        DistanceCheck::NotPositive { distance_m } => {
            return vec![format!(
                "{label}{}: not checked \u{2014} needs a distance > 0",
                typed(*distance_m)
            )]
        }
        DistanceCheck::NoLatency { distance_m } => {
            return vec![format!(
                "{label}{}: not checked \u{2014} no stored latency (below)",
                typed(*distance_m)
            )]
        }
        check => check.scored().expect("the remaining variants are scored"),
    };
    let reason = stats.distance_check.withholds_flight_time();
    if stats.flight_time_s.is_none() && !reason {
        return vec![format!(
            "{label}{}: not checked \u{2014} flight time withheld (above)",
            typed(window.distance_m)
        )];
    }
    let c = match window.temperature_c {
        Some(t) => format!("c {:.1} m/s at {t:.1} \u{b0}C", window.speed_of_sound_m_s),
        None => format!("c {:.1} m/s assumed", window.speed_of_sound_m_s),
    };
    let mut lines = vec![
        format!(
            "{label}{}: {} samples ({} ms) re d/c, {} window",
            typed(window.distance_m),
            signed_minus(excess_s * fs, 0),
            signed_minus(excess_s * 1000.0, 3),
            if reason { "outside" } else { "inside" }
        ),
        format!(
            "{CONT_INDENT}d/c {} samples ({} ms), {c}",
            signed_minus(window.expected_s * fs, 0),
            signed_minus(window.expected_s * 1000.0, 3)
        ),
        format!(
            "{CONT_INDENT}window {} \u{2026} {} samples re d/c",
            signed_minus(window.low_s * fs, 0),
            signed_minus(window.high_s * fs, 0)
        ),
        format!(
            "{CONT_INDENT}from tape \u{b1}{:.0} cm, c \u{b1}{:.0} %, speaker allowance {} ms \
             assumed",
            DISTANCE_TAPE_TOLERANCE_M * 100.0,
            DISTANCE_SPEED_OF_SOUND_REL_TOL * 100.0,
            signed_minus(ARRIVAL_EXCESS_DELAY_ALLOWANCE_S * 1000.0, 1)
        ),
    ];
    match stats.distance_check {
        DistanceCheck::TooLate { .. } => lines.push(format!(
            "{CONT_INDENT}check: typed distance, IR before arrival, speaker DSP latency"
        )),
        DistanceCheck::TooEarly { .. } => lines.push(format!(
            "{CONT_INDENT}check: typed distance, temperature, latency (below)"
        )),
        _ => {}
    }
    lines
}

/// The `withheld` reason the distance check gives (#537 UX revision 3), in
/// the #359 shape: a direction word and a pointer to the distance block.
fn distance_withheld_reason(stats: &ac_core::measurement::report::IrStats) -> Option<String> {
    use ac_core::measurement::report::DistanceCheck;
    match &stats.distance_check {
        DistanceCheck::TooEarly { window, .. } => Some(format!(
            "earlier than {} m allows (below)",
            window.distance_m
        )),
        DistanceCheck::TooLate { window, .. } => {
            Some(format!("later than {} m allows (below)", window.distance_m))
        }
        _ => None,
    }
}

/// The `arrival SNR` block (#537 UX): the band-limited IR's pre-impulse SNR
/// beside the gate it is held to, and what that gate rests on. On
/// `BandLimitUnavailable`, one row saying why it was not measured.
fn arrival_snr_lines(stats: &ac_core::measurement::report::IrStats) -> Vec<String> {
    use ac_core::measurement::report::{ArrivalCrossCheck, ARRIVAL_SNR_BASIS, ARRIVAL_SNR_MIN_DB};
    if let ArrivalCrossCheck::BandLimitUnavailable {
        band_top_hz,
        required_hz,
    } = stats.arrival_cross_check
    {
        return vec![format!(
            "{}not measured \u{2014} {}",
            label_prefix("arrival SNR"),
            band_limit_unavailable_reason(band_top_hz, required_hz)
        )];
    }
    let Some(snr) = stats.band_limited_snr_db else {
        return Vec::new();
    };
    let above = above_corner(stats).unwrap_or_default();
    let value = if snr.is_finite() {
        format!("{snr:.1} dB  ({above}, required \u{2265} {ARRIVAL_SNR_MIN_DB:.1} dB)")
    } else {
        format!("\u{221e} dB  ({above}, zero measured floor)")
    };
    vec![
        format!("{}{value}", label_prefix("arrival SNR")),
        format!("{CONT_INDENT}{ARRIVAL_SNR_BASIS}"),
    ]
}

/// The `broadband Δ` block (#537 UX): the cross-check as a signed number,
/// the tolerance it was compared to with that tolerance's provenance, and —
/// outside the tolerance only — where to look. Never a verdict word.
fn broadband_delta_lines(stats: &ac_core::measurement::report::IrStats) -> Vec<String> {
    use ac_core::measurement::report::{
        arrival_cross_check_tolerance_samples, ArrivalCrossCheck, ARRIVAL_BROADBAND_COMPARABLE_DB,
        ARRIVAL_CROSS_CHECK_BASIS, ARRIVAL_CROSS_CHECK_TOLERANCE_S,
    };
    if let ArrivalCrossCheck::BandLimitUnavailable {
        band_top_hz,
        required_hz,
    } = stats.arrival_cross_check
    {
        return vec![format!(
            "{}not computed \u{2014} {}",
            label_prefix("broadband \u{394}"),
            band_limit_unavailable_reason(band_top_hz, required_hz)
        )];
    }
    let Some(delta) = stats.broadband_delta_samples() else {
        return Vec::new();
    };
    let mut lines = vec![format!(
        "{}{delta:+} samples  ({:+.3} ms, broadband peak \u{2212} arrival)",
        label_prefix("broadband \u{394}"),
        delta as f64 / stats.sample_rate_hz as f64 * 1000.0
    )];
    // #537 UX revision 2: which peak the Δ is measured to (`r`), in the
    // `peak` row's own coordinate, so the two rows can be reconciled.
    if let Some(level_db) = stats.broadband_delta_level_db {
        lines.push(format!(
            "{CONT_INDENT}to broadband peak at sample {}, {} dB (first \u{2265} {} dB)",
            stats.arrival_index as i64 + delta,
            signed_minus(level_db, 1).trim_start_matches('+'),
            signed_minus(-ARRIVAL_BROADBAND_COMPARABLE_DB, 1)
        ));
    }
    lines.push(format!(
        "{CONT_INDENT}tolerance \u{b1}{} samples (\u{b1}{:.1} ms), {ARRIVAL_CROSS_CHECK_BASIS}",
        arrival_cross_check_tolerance_samples(stats.sample_rate_hz),
        ARRIVAL_CROSS_CHECK_TOLERANCE_S * 1000.0
    ));
    let corner = match stats.arrival_source {
        ac_core::measurement::report::ArrivalSource::BandLimitedPeak { corner_hz } => {
            format_corner(corner_hz)
        }
        ac_core::measurement::report::ArrivalSource::Peak => String::new(),
    };
    match stats.arrival_cross_check {
        ArrivalCrossCheck::BroadbandLater { .. } => lines.push(format!(
            "{CONT_INDENT}check: IR below {corner}, speaker and mic placement"
        )),
        ArrivalCrossCheck::BroadbandEarlier { .. } => lines.push(format!(
            "{CONT_INDENT}check: IR above {corner}, mic axis, obstructions"
        )),
        _ => {}
    }
    lines
}

/// The onset block's last row (#346 UX revision 4, #537 UX): the
/// onset-to-arrival gap, always qualified `, not used for flight time`.
/// `None` when the onset is not before the arrival — a declined picker
/// reports the arrival.
fn onset_gap_line(stats: &ac_core::measurement::report::IrStats) -> Option<String> {
    if stats.onset_index >= stats.arrival_index {
        return None;
    }
    Some(format!(
        "{CONT_INDENT}{} samples before arrival, not used for flight time",
        stats.arrival_index - stats.onset_index
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

/// [`enumeration_lines`] under a stored τ a session check measured (#466
/// UX): the event is named plainly, without `UNVERIFIED` and without the
/// re-calibrate `check:` — the verdict below answered it and carries its
/// own `check:` line.
fn answered_enumeration_lines(
    check: Option<&ac_core::shared::calibration::EnumerationCheck>,
) -> Vec<String> {
    use ac_core::shared::calibration::EnumerationCheck;
    match check {
        Some(EnumerationCheck::Same) => enumeration_lines(check, false),
        Some(EnumerationCheck::Crossed { boundary, since }) => {
            let (head, nodes) = split_boundary(boundary);
            let mut lines = vec![format!(
                "{CONT_INDENT}{head}{}",
                since_clause(since.as_deref())
            )];
            if let Some(list) = nodes {
                lines.extend(nodes_lines(list, CONT_INDENT));
            }
            lines
        }
        Some(EnumerationCheck::NotObservable { reason }) => {
            let mut lines = vec![format!("{CONT_INDENT}device enumeration not observable")];
            let (observation, _) = ac_core::shared::calibration::session::split_check(reason);
            lines.extend(indented_wrapped(observation, CONT_INDENT));
            lines
        }
        Some(EnumerationCheck::NotRecorded) | None => {
            vec![format!("{CONT_INDENT}entry predates enumeration tracking")]
        }
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
    // #494 UX: display-only wrap past 79 columns, hanging under the first
    // item. Stored reasons are unchanged; older reports render the same way.
    const MAX_COLS: usize = 79;
    let unavailable = |reason: &str| match reason.split_once("; check: ") {
        Some((observation, places)) => {
            let mut lines = wrap_comma_list(
                "  ref latency   unavailable \u{2014} ",
                observation,
                MAX_COLS,
            );
            lines.extend(wrap_comma_list("                check: ", places, MAX_COLS));
            lines
        }
        None => wrap_comma_list("  ref latency   unavailable \u{2014} ", reason, MAX_COLS),
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

/// `prefix` followed by the `, `-separated list `text`, on one line when it
/// fits in `max_cols` columns, otherwise wrapped at `, ` boundaries with a
/// hanging indent under the first item (#494 UX). Every line but the last
/// keeps its trailing comma.
///
/// The wrap uses the fewest lines that fit, and among those the split whose
/// widest line is narrowest — so related items stay together (`SNR 21.34 dB,
/// need 24.00 dB`) where a greedy fill would split them. Every rendering UX
/// specified for #494 is this rule's output. An item wider than the space
/// left gets a line of its own and overflows; nothing is cut.
pub(super) fn wrap_comma_list(prefix: &str, text: &str, max_cols: usize) -> Vec<String> {
    let indent = prefix.chars().count();
    if indent + text.chars().count() <= max_cols {
        return vec![format!("{prefix}{text}")];
    }
    let items: Vec<&str> = text.split(", ").collect();
    let n = items.len();
    let avail = max_cols.saturating_sub(indent);
    // Width of items[a..b] as one line, with its trailing comma unless it
    // is the last line.
    let width = |a: usize, b: usize| -> usize {
        let joined: usize = items[a..b].iter().map(|s| s.chars().count()).sum();
        joined + 2 * (b - a - 1) + usize::from(b < n)
    };
    // Bit i of a mask set = break after item i. Lists here are a handful of
    // items; past 16 fall back to one item per line.
    let mut best: Option<(u32, usize, u32)> = None;
    if n <= 16 {
        for mask in 0u32..(1u32 << (n - 1)) {
            let mut start = 0;
            let mut widest = 0;
            for end in 1..=n {
                if end == n || mask & (1 << (end - 1)) != 0 {
                    widest = widest.max(width(start, end));
                    start = end;
                }
            }
            if widest > avail {
                continue;
            }
            let key = (mask.count_ones(), widest, mask);
            if best.is_none_or(|b| (key.0, key.1) < (b.0, b.1)) {
                best = Some(key);
            }
        }
    }
    let breaks_after = |end: usize| match best {
        Some((_, _, mask)) => mask & (1 << (end - 1)) != 0,
        None => true,
    };
    let mut lines = Vec::new();
    let mut start = 0;
    for end in 1..=n {
        if end == n || breaks_after(end) {
            let lead = if start == 0 {
                prefix.to_string()
            } else {
                " ".repeat(indent)
            };
            let comma = if end < n { "," } else { "" };
            lines.push(format!("{lead}{}{comma}", items[start..end].join(", ")));
            start = end;
        }
    }
    lines
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
            let refused = m
                .session_check
                .as_ref()
                .is_some_and(LayerVerdict::is_refused);
            let mut lines = vec![
                format!(
                    "{}{} ms  ({} samples, {})",
                    label_prefix("latency"),
                    format_ms_aligned(m.tau_s * 1000.0),
                    format_samples(samples),
                    if refused {
                        "stored, not applied"
                    } else {
                        "stored"
                    },
                ),
                measured_line(&m.measured_at, report_timestamp_utc),
            ];
            // #461: τ is per channel pair, so `ref Δ` never clears this one.
            // #466: a measured verdict answered the event, so it is named
            // plainly under one.
            if m.session_check
                .as_ref()
                .is_some_and(LayerVerdict::is_decisive)
            {
                lines.extend(answered_enumeration_lines(m.enumeration.as_ref()));
            } else {
                lines.extend(enumeration_lines(m.enumeration.as_ref(), false));
            }
            lines.extend(session_check_latency_lines(m));
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

/// The session check's verdict on the capture pair's stored τ, under its
/// `latency` lines (#466 UX, architect R6-5). Frozen report data only. A
/// report with no verdict (before v11) and a verified one print nothing
/// here: the session check block above carries a verified verdict.
fn session_check_latency_lines(m: &ac_core::measurement::report::MeasuredLatency) -> Vec<String> {
    use ac_core::shared::calibration::session::{split_check, UnverifiedCause};
    let Some(verdict) = m.session_check.as_ref() else {
        return Vec::new();
    };
    match verdict {
        LayerVerdict::Verified(_) => Vec::new(),
        LayerVerdict::Refused { evidence, via, .. } => {
            let mut lines = vec![match via {
                Some(via) => format!(
                    "{CONT_INDENT}{}",
                    super::calibrate::latency_refused_via(via, evidence.delta)
                ),
                None => format!(
                    "{CONT_INDENT}REFUSED \u{2014} {:.0} samples read {} (\u{394} {:+.0})",
                    evidence.measured, evidence.checked_at, evidence.delta
                ),
            }];
            lines.extend(
                super::calibrate::refusal_predates_line(
                    m.enumeration.as_ref(),
                    Some(&evidence.checked_at),
                )
                .map(|line| format!("{CONT_INDENT}{line}")),
            );
            if via.is_some() {
                lines.push(format!(
                    "{CONT_INDENT}check: `ac calibrate check`; if still refused,"
                ));
                lines.push(format!("{CONT_INDENT}re-run `ac calibrate` for this pair"));
            } else {
                lines.push(format!("{CONT_INDENT}{RECALIBRATE_CHECK}"));
            }
            lines
        }
        LayerVerdict::Unverified {
            cause: UnverifiedCause::NotCovered,
            ..
        } => vec![match m.session_check_loopback.as_deref() {
            Some(key) => {
                format!("{CONT_INDENT}session check does not cover this pair (loopback [{key}])")
            }
            None => format!("{CONT_INDENT}session check does not cover this pair"),
        }],
        LayerVerdict::Unverified { reason, .. } => {
            let (observation, places) = split_check(reason);
            let mut lines = indented_wrapped(&format!("{UNVERIFIED}{observation}"), CONT_INDENT);
            if let Some(places) = places {
                lines.extend(indented_wrapped(&format!("check: {places}"), CONT_INDENT));
            }
            lines
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
        // #466: the stored τ is in the report but the session check refused
        // it. After the #359 arms: their line is the more specific one.
        (None, _)
            if stats
                .interface_latency_check
                .as_ref()
                .is_some_and(LayerVerdict::is_refused) =>
        {
            vec![format!(
                "{}not shown \u{2014} stored latency refused (below)",
                label_prefix("flight time")
            )]
        }
        (None, _) => vec![format!(
            "{}not shown \u{2014} no stored latency for this pair (below)",
            label_prefix("flight time")
        )],
    }
}

/// The `flight time` block with #537's guards folded in. A reason that
/// withholds the flight time on the IR side (the cross-check) or against the
/// typed distance takes the `withheld` row — fixing τ would not help — in
/// that order, then τ (#537 UX revision 3); each further reason gets one
/// `also:` row. Otherwise the #359/#461/#466 rows print as before, with the
/// standing's mark (if any) directly under the value, above `latency
/// UNVERIFIED` and the reference line: the mark qualifies the arrival, which
/// is upstream of τ.
fn flight_time_block(
    stats: &ac_core::measurement::report::IrStats,
    interface_latency: Option<&ac_core::measurement::report::InterfaceLatency>,
) -> Vec<String> {
    use ac_core::measurement::report::{
        ArrivalCrossCheck, ARRIVAL_EARLIER_COMPARABLE_DB, ARRIVAL_LOBE_MARGIN_MIN_DB,
        ARRIVAL_SNR_MIN_DB,
    };
    let cross_check = match stats.arrival_cross_check {
        ArrivalCrossCheck::BandLimitUnavailable {
            band_top_hz,
            required_hz,
        } => Some(band_limit_unavailable_reason(band_top_hz, required_hz)),
        ArrivalCrossCheck::BandLimitedSnrLow { snr_db } => Some(format!(
            "arrival SNR {snr_db:.1} dB, required \u{2265} {ARRIVAL_SNR_MIN_DB:.1} dB"
        )),
        ArrivalCrossCheck::ArrivalAmbiguous { .. } => Some(format!(
            "second lobe within {ARRIVAL_LOBE_MARGIN_MIN_DB:.1} dB (above)"
        )),
        ArrivalCrossCheck::EarlierComparable { .. } => Some(format!(
            "earlier peak within {ARRIVAL_EARLIER_COMPARABLE_DB:.1} dB (above)"
        )),
        ArrivalCrossCheck::BroadbandEarlier { .. } => {
            Some("broadband peak is earlier (see broadband \u{394})".to_string())
        }
        ArrivalCrossCheck::BroadbandLater { .. } | ArrivalCrossCheck::Agrees { .. } => None,
    };
    let distance = distance_withheld_reason(stats);
    if cross_check.is_some() || distance.is_some() {
        let mut reasons = cross_check.into_iter().chain(distance);
        let first = reasons.next().expect("at least one reason");
        let mut lines = vec![format!(
            "{}withheld \u{2014} {first}",
            label_prefix("flight time")
        )];
        let tau = tau_withheld_reason(stats, interface_latency);
        for also in reasons.chain(tau) {
            lines.push(format!("{CONT_INDENT}also: {also}"));
        }
        return lines;
    }
    let mut lines = flight_time_line(stats);
    if let ArrivalCrossCheck::BroadbandLater { gap } = stats.arrival_cross_check {
        lines.insert(
            1,
            format!("{CONT_INDENT}broadband peak disagrees by {gap:+} samples (below)"),
        );
    }
    lines
}

/// The reason [`flight_time_line`] would give for no flight time on the τ
/// side, in its words: for the `also:` row under a withheld arrival. `None`
/// when τ would have allowed one.
fn tau_withheld_reason(
    stats: &ac_core::measurement::report::IrStats,
    interface_latency: Option<&ac_core::measurement::report::InterfaceLatency>,
) -> Option<String> {
    use ac_core::measurement::report::{ArrivalCheck, InterfaceLatency};
    match &stats.arrival_check {
        ArrivalCheck::PeriodShift(_) => {
            return Some("ref \u{394} is a period shift (below)".to_string())
        }
        ArrivalCheck::Mismatch(_) => return Some("ref \u{394} is not zero (below)".to_string()),
        _ => {}
    }
    if stats
        .interface_latency_check
        .as_ref()
        .is_some_and(LayerVerdict::is_refused)
    {
        return Some("stored latency refused (below)".to_string());
    }
    match interface_latency {
        Some(InterfaceLatency::Measured(_)) => None,
        _ => Some("no stored latency for this pair (below)".to_string()),
    }
}

/// The `measurement/report` frame's body through the checked reader
/// (#429). `Err` is the refusal line for stderr — missing frame,
/// unsupported schema version, or undecodable body; nothing of a refused
/// report reaches the operator.
fn decode_ir_report(report_frame: Option<&serde_json::Value>) -> Result<MeasurementReport, String> {
    let Some(value) = report_frame.and_then(|f| f.get("report")) else {
        return Err("  !! no measurement/report frame — nothing to summarise".to_string());
    };
    match MeasurementReport::from_value(value.clone()) {
        Ok(r) => Ok(r),
        Err(ReportReadError::UnsupportedSchema { found, supported }) => Err(format!(
            "  !! unsupported measurement report schema — found v{found}, supported v{}–v{}",
            supported.start(),
            supported.end()
        )),
        Err(ReportReadError::Malformed(msg)) => Err(format!("  !! could not decode report: {msg}")),
    }
}

/// The read-out: arrival (samples and ms, re gate centre), peak,
/// pre-impulse SNR, and the gate that produced them — decoded from the
/// `measurement/report` frame rather than recomputed off the raw IR
/// frame, so the printed numbers and the archived ones are the same
/// numbers by construction. No distance figure — #391 removed the
/// ms → m conversion this used to also print.
fn print_ir_report(report: &MeasurementReport) {
    use ac_core::measurement::report::IrVerdict;

    let Some(stats) = report.ir_stats() else {
        eprintln!("  !! report carries no impulse-response payload to summarise");
        return;
    };

    // A capture whose peak cannot be trusted (#376) is reported as
    // failed, not as a result with a number in it: no arrival line —
    // that is the exact plausible-looking wrong-number shape the issue
    // exists to close.
    if let IrVerdict::Failed { reason } = &stats.verdict {
        for line in deconvolution_failed_lines(reason) {
            println!("{line}");
        }
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
        if let Some(line) = second_lobe_line(&stats) {
            println!("{line}");
        }
        if let Some(line) = earlier_peak_line(&stats) {
            println!("{line}");
        }
        // #359: the one τ subtraction this report can offer, gated by
        // `arrival_check` and (#537) the arrival's cross-check — sits
        // directly under `arrival` so the two primary values stack. Never
        // printed on a failed deconvolution (#376's rule that a failed
        // capture prints no arrival).
        for line in flight_time_block(&stats, report.interface_latency.as_ref()) {
            println!("{line}");
        }
        // #537 architect revision 3: the flight time against the typed
        // distance, directly under the number it scores.
        for line in distance_lines(&stats) {
            println!("{line}");
        }
        // #537: the arrival's own gate, under the value it decides.
        for line in arrival_snr_lines(&stats) {
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
        // #537: the cross-check as evidence, next to the peak it is
        // measured from.
        for line in broadband_delta_lines(&stats) {
            println!("{line}");
        }
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
        // The onset-to-arrival gap: the loudspeaker's group-delay excess,
        // and the quantity #378's AC6 found moving with position. Printed on
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
    for line in pre_impulse_snr_lines(&stats) {
        println!("{line}");
    }
    println!(
        "  gate          {} window, {} samples ({:.2} ms) → f_low {:.1} Hz",
        stats.gate_window_kind,
        stats.window_len,
        stats.gate_window_s * 1000.0,
        stats.gate_f_low_hz,
    );
}

/// The `setup` command that sets the report directory, as the `report`
/// line prints it (#472): built from the parser's own token, and fed back
/// through the parser by a test, so it cannot name something `ac setup`
/// does not accept.
fn report_dir_remedy() -> String {
    format!("ac setup {} <dir>", crate::parse::REPORT_DIR_TOKEN)
}

/// The `report` / `csv` lines (#472 UX), from what the daemon's `done` frame
/// says it wrote — never from this process's config, which may not be the
/// daemon's. A `done` frame without `report_files` comes from a daemon that
/// predates the field, and says so rather than guessing a path.
fn report_files_lines(done: &serde_json::Value) -> Vec<String> {
    let Some(files) = done.get("report_files") else {
        return vec![format!(
            "{}not reported by this daemon",
            label_prefix("report")
        )];
    };
    let Some(dir) = files.get("dir").and_then(|v| v.as_str()) else {
        return vec![format!(
            "{}not saved \u{2014} no report directory  ({})",
            label_prefix("report"),
            report_dir_remedy()
        )];
    };
    let path_of = |key: &str| {
        files
            .get(key)
            .and_then(|f| f.get("path"))
            .and_then(|v| v.as_str())
    };
    let error_of = |key: &str| {
        files
            .get(key)
            .and_then(|f| f.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("reason not reported")
    };
    let mut lines = Vec::new();
    match path_of("json") {
        Some(p) => lines.push(format!("{}{p}", label_prefix("report"))),
        None => {
            // The JSON is the report; when it failed, the CSV's own outcome
            // is not repeated unless it was written after all.
            lines.push(format!(
                "{}not saved \u{2014} write failed in {dir}",
                label_prefix("report")
            ));
            lines.push(format!("{CONT_INDENT}{}", error_of("json")));
            if let Some(p) = path_of("csv") {
                lines.push(format!("{}{p}", label_prefix("csv")));
            }
            return lines;
        }
    }
    match path_of("csv") {
        Some(p) => lines.push(format!("{}{p}", label_prefix("csv"))),
        None => lines.push(format!(
            "{}not saved \u{2014} {}",
            label_prefix("csv"),
            error_of("csv")
        )),
    }
    lines
}

/// Wait for `plot_ir`'s DATA frames: `measurement/impulse_response` and
/// `measurement/report` ride their own topics (not wrapped in a generic
/// `data` topic the way `plot`/`plot_level` per-point frames are), so this
/// mirrors `collect_sweep` but keys off the topic string directly.
///
/// The `done` frame is returned too: it carries `report_files` (#472). It is
/// `None` when the command ended on `error` or a timeout, both already
/// reported.
fn collect_ir(
    client: &mut AcClient,
    cmd_name: &str,
) -> (
    Option<serde_json::Value>,
    Option<serde_json::Value>,
    Option<serde_json::Value>,
) {
    let mut ir_frame = None;
    let mut report_frame = None;
    let mut done_frame = None;
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
            "done" => {
                done_frame = Some(data);
                break;
            }
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
    (ir_frame, report_frame, done_frame)
}

fn print_ir_result(
    ir_frame: Option<&serde_json::Value>,
    report_frame: Option<&serde_json::Value>,
    duration: Option<f64>,
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
    // `duration` and `tail_s` are the ack's echo (#501): the nominal
    // figures the daemon ran; the report `notes` line below carries the
    // measured decay verdict.
    println!("{}", captured_line(duration, tail_s));
    let _ = report_frame;
}

/// The report's `notes`: the ISO 18233 §6.3.2 measured tail-decay verdict
/// and the §B.5 linear-deconvolution artefact statement, one line each.
/// Printed last, and printed verbatim from the report so the operator
/// reads exactly what the archive records (#283). Takes the decoded
/// report, not the raw frame, so a report the checked reader refused
/// (#429) has no notes to print.
fn ir_notes_lines(report: Option<&MeasurementReport>) -> Vec<String> {
    let Some(notes) = report.and_then(|r| r.notes.as_deref()) else {
        return Vec::new();
    };
    std::iter::once(String::new())
        .chain(notes.lines().map(|line| format!("  {line}")))
        .collect()
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
        arrival_check_lines, arrival_snr_lines, arrival_source_line, broadband_delta_lines,
        captured_line, collect_sweep_frames, decode_ir_report, deconvolution_failed_lines,
        distance_lines, earlier_peak_line, enumeration_lines, flight_time_block, flight_time_line,
        guard_outcome, interface_latency_lines, ir_notes_lines, ir_stimulus_lines, label_prefix,
        onset_gap_line, pre_impulse_snr_lines, reference_latency_lines,
        reference_stored_latency_lines, report_files_lines, second_lobe_line, short_onset_rule,
        wrap_comma_list, IrTyped, SweepOutcome, CONT_INDENT,
    };
    use ac_core::measurement::report::{
        ArrivalCheck, ArrivalCrossCheck, ArrivalSource, DistanceCheck, InterfaceLatency, IrStats,
        IrVerdict, MeasuredLatency, MeasuredReferenceLatency, OnsetStanding, ReferenceLatency,
    };
    use ac_core::measurement::sweep::{BoundInputs, CausalBound, EdgeGuard, MissingBoundInput};
    use ac_core::shared::calibration::{EnumerationCheck, LayerVerdict, TauDisagreement};
    use std::collections::VecDeque;

    /// #429 (Codex on PR #536): `plot ir` decodes the report frame once,
    /// and the notes come from that decoded value. A future-schema
    /// report carrying `notes` is refused, and its notes are not printed —
    /// the raw read the notes helper used to do would have printed them.
    #[test]
    fn unsupported_report_schema_prints_no_notes() {
        use ac_core::measurement::report::{MIN_SCHEMA_VERSION, SCHEMA_VERSION};
        let future = SCHEMA_VERSION + 1;
        let frame = serde_json::json!({"report": {
            "schema_version": future,
            "notes": "future-schema interpretation",
        }});

        // The rejected implementation: reading `notes` straight off the
        // frame finds the text, so the fixture can make the check fail.
        let raw = frame
            .get("report")
            .and_then(|r| r.get("notes"))
            .and_then(|v| v.as_str());
        assert_eq!(raw, Some("future-schema interpretation"));

        let decoded = decode_ir_report(Some(&frame));
        let refusal = decoded.as_ref().expect_err("future schema is refused");
        assert_eq!(
            refusal,
            &format!(
                "  !! unsupported measurement report schema \u{2014} found v{future}, \
                 supported v{MIN_SCHEMA_VERSION}\u{2013}v{SCHEMA_VERSION}"
            )
        );
        let notes = ir_notes_lines(decoded.ok().as_ref());
        assert!(notes.is_empty(), "refused report printed notes: {notes:?}");
        assert!(!notes.iter().any(|l| l.contains("future-schema")));
    }

    #[test]
    fn missing_or_malformed_report_frame_is_refused() {
        assert!(decode_ir_report(None)
            .expect_err("no frame is refused")
            .contains("no measurement/report frame"));
        let no_version = serde_json::json!({"report": {"notes": "x"}});
        assert!(decode_ir_report(Some(&no_version))
            .expect_err("versionless report is refused")
            .contains("could not decode report"));
        assert!(ir_notes_lines(None).is_empty());
    }

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

    /// #472 UX: the three `report` states, all on the `report` slot.
    #[test]
    fn report_files_lines_render_each_state() {
        let saved = serde_json::json!({"cmd": "plot_ir", "report_files": {
            "dir": "/r",
            "json": {"path": "/r/t-plot_ir.json"},
            "csv": {"path": "/r/t-plot_ir.csv"},
        }});
        assert_eq!(
            report_files_lines(&saved),
            vec![
                "  report        /r/t-plot_ir.json",
                "  csv           /r/t-plot_ir.csv",
            ]
        );

        let unset = serde_json::json!({"report_files": {"dir": null}});
        assert_eq!(
            report_files_lines(&unset),
            vec![
                "  report        not saved \u{2014} no report directory  (ac setup report-dir <dir>)"
            ]
        );

        let failed = serde_json::json!({"report_files": {
            "dir": "/mnt/rigdata/ac-reports",
            "json": {"error": "Permission denied (os error 13)"},
            "csv": {"error": "Permission denied (os error 13)"},
        }});
        assert_eq!(
            report_files_lines(&failed),
            vec![
                "  report        not saved \u{2014} write failed in /mnt/rigdata/ac-reports",
                "                Permission denied (os error 13)",
            ]
        );

        let csv_only_failed = serde_json::json!({"report_files": {
            "dir": "/r",
            "json": {"path": "/r/t-plot_ir.json"},
            "csv": {"error": "No space left on device (os error 28)"},
        }});
        assert_eq!(
            report_files_lines(&csv_only_failed),
            vec![
                "  report        /r/t-plot_ir.json",
                "  csv           not saved \u{2014} No space left on device (os error 28)",
            ]
        );

        let old_daemon = serde_json::json!({"cmd": "plot_ir"});
        assert_eq!(
            report_files_lines(&old_daemon),
            vec!["  report        not reported by this daemon"]
        );
    }

    /// #472: the unset line's remedy must be a command the real parser
    /// accepts as a report-directory setter. Comparing the token with itself
    /// could never fail; this renders the line, cuts the command out of it,
    /// and parses it — so a remedy renamed to `reports-dir`, given an extra
    /// argument, or pointed at another command goes red.
    #[test]
    fn unset_report_line_names_a_setup_command_the_parser_accepts() {
        let lines = report_files_lines(&serde_json::json!({"report_files": {"dir": null}}));
        assert_eq!(lines.len(), 1, "{lines:?}");
        let remedy = remedy_of(&lines[0]);
        assert_parses_as_report_dir_setter(&remedy);

        // The check itself must be able to fail: the rejected remedies.
        for wrong in [
            "ac setup reports-dir <dir>",
            "ac setup report-dir <dir> extra",
            "ac plot ir <dir>",
            "ac setup <dir>",
        ] {
            let outcome = std::panic::catch_unwind(|| assert_parses_as_report_dir_setter(wrong));
            assert!(outcome.is_err(), "{wrong:?} must not pass the drift check");
        }
    }

    fn remedy_of(line: &str) -> String {
        let start = line.find("(ac ").expect("remedy opens with `(ac `") + 1;
        let end = start + line[start..].find(')').expect("remedy closes with `)`");
        line[start..end].to_string()
    }

    fn assert_parses_as_report_dir_setter(remedy: &str) {
        let argv: Vec<String> = remedy
            .replace("<dir>", "/srv/ac-reports")
            .split_whitespace()
            .skip(1) // `ac`, the binary name
            .map(String::from)
            .collect();
        match crate::parse::parse(&argv) {
            Ok(p) => match p.cmd {
                crate::parse::CommandKind::Setup {
                    report_dir: Some(Some(ref d)),
                    ..
                } => assert_eq!(d, "/srv/ac-reports"),
                other => panic!("{remedy:?} parses as {other:?}, not a report-dir setter"),
            },
            Err(e) => panic!("{remedy:?} does not parse: {e}"),
        }
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

    /// #346 UX revision 4, #537 UX: row 2 under `arrival` names the rule,
    /// with the corner formatted from the source. It names no onset and
    /// points nowhere `(below)`. The broadband peak is only the rule when
    /// the band did not allow the band-limited one, and says so.
    #[test]
    fn arrival_source_line_names_the_rule_and_its_corner() {
        let band_limited =
            arrival_source_line(&ArrivalSource::BandLimitedPeak { corner_hz: 1000.0 });
        assert_eq!(
            band_limited,
            format!("{CONT_INDENT}from peak of IR high-passed at 1 kHz (zero-phase)")
        );
        assert_eq!(
            arrival_source_line(&ArrivalSource::BandLimitedPeak { corner_hz: 2500.0 }),
            format!("{CONT_INDENT}from peak of IR high-passed at 2.5 kHz (zero-phase)")
        );
        let peak = arrival_source_line(&ArrivalSource::Peak);
        assert_eq!(
            peak,
            format!("{CONT_INDENT}from peak (largest magnitude sample), not band-limited")
        );
        for line in [band_limited, peak] {
            assert!(!line.contains("onset"), "{line:?}");
            assert!(!line.contains("(below)"), "{line:?}");
            assert!(line.chars().count() <= 80, "{line:?}");
        }
    }

    /// #537 UX: the gap row is measured to the arrival, not the peak, and
    /// says `not used for flight time` on every onset standing; it is absent
    /// when the onset is not before the arrival.
    #[test]
    fn onset_gap_line_is_measured_to_the_arrival() {
        let mut stats = stats_with(None, ArrivalCheck::Agree);
        stats.peak_index = 12_005;
        stats.arrival_index = 10_503;
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
                    "{CONT_INDENT}20 samples before arrival, not used for flight time"
                )),
                "{standing:?}"
            );
        }
        stats.onset_index = stats.arrival_index;
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

        // #494: the stored reasons from before the change still render
        // unwrapped, one-decimal SNR included.
        let edge = ReferenceLatency::Unavailable {
            reason:
                "peak at reference window edge; check: reference loopback routing, capture tail"
                    .into(),
        };
        assert_eq!(
            reference_latency_lines(Some(&edge), 96_000),
            vec![
                "  ref latency   unavailable — peak at reference window edge",
                "                check: reference loopback routing, capture tail",
            ]
        );
    }

    /// #494 UX: the combined reference reason is wider than 79 columns, so
    /// both its observation and its `check:` line wrap at `, ` under their
    /// first item.
    #[test]
    fn reference_latency_lines_wrap_the_combined_edge_and_snr_reason() {
        let both = ReferenceLatency::Unavailable {
            reason: "peak at reference window edge, SNR 21.34 dB, need 24.00 dB; check: \
                     reference loopback routing, capture tail, reference loopback cable, \
                     ref input gain"
                .into(),
        };
        assert_eq!(
            reference_latency_lines(Some(&both), 96_000),
            vec![
                "  ref latency   unavailable — peak at reference window edge,",
                "                              SNR 21.34 dB, need 24.00 dB",
                "                check: reference loopback routing, capture tail,",
                "                       reference loopback cable, ref input gain",
            ]
        );

        let low = ReferenceLatency::Unavailable {
            reason:
                "peak SNR 17.28 dB, need 24.00 dB; check: reference loopback cable, ref input gain"
                    .into(),
        };
        assert_eq!(
            reference_latency_lines(Some(&low), 96_000),
            vec![
                "  ref latency   unavailable — peak SNR 17.28 dB, need 24.00 dB",
                "                check: reference loopback cable, ref input gain",
            ]
        );
    }

    /// #494: the wrap rule itself. Fits → untouched; too wide → fewest
    /// lines, then the narrowest widest line; an item too wide for any line
    /// gets its own line rather than being cut.
    #[test]
    fn wrap_comma_list_prefers_fewest_then_balanced_lines() {
        assert_eq!(wrap_comma_list("> ", "a, b, c", 20), vec!["> a, b, c"]);
        // Greedy would give "aaaa, bb," / "cc"; balanced gives the split
        // whose widest line is narrowest.
        assert_eq!(
            wrap_comma_list("> ", "aaaa, bb, cc", 12),
            vec!["> aaaa,", "  bb, cc"]
        );
        assert_eq!(
            wrap_comma_list("> ", "aaaaaaaaaaaa, b", 8),
            vec!["> aaaaaaaaaaaa,", "  b"]
        );
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
            session_check: None,
            session_check_loopback: None,
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
            arrival_source: ArrivalSource::BandLimitedPeak { corner_hz: 2000.0 },
            arrival_index: 600,
            band_limited_snr_db: Some(40.0),
            arrival_lobe_margin_db: Some(6.1),
            arrival_lobe_offset: Some(40),
            broadband_delta_level_db: Some(0.0),
            arrival_cross_check: ArrivalCrossCheck::Agrees { gap: 0 },
            onset_standing: OnsetStanding::NoCausalBound,
            delay_samples: 88,
            arrival_s: 88.0 / 96_000.0,
            arrival_check,
            flight_time_s,
            distance_check: DistanceCheck::NotGiven,
            interface_latency_enumeration: Some(EnumerationCheck::Same),
            interface_latency_check: None,
            pre_impulse_snr_db: 40.0,
            gate_window_s: 0.01,
            gate_f_low_hz: 100.0,
            gate_window_kind: "tukey".into(),
            verdict: IrVerdict::Ok,
        }
    }

    // ─── #466: the session check's verdict on the stored τ ──────────────

    fn tau_evidence(checked_at: &str) -> ac_core::shared::calibration::session::Evidence {
        use ac_core::shared::calibration::session::{CheckSource, Evidence, VerdictUnit};
        Evidence {
            measured: 1743.0,
            stored: 1711.0,
            delta: 32.0,
            tolerance: 0.0,
            unit: VerdictUnit::Samples,
            stored_at: "2026-09-15T23:43:04Z".into(),
            checked_at: checked_at.into(),
            source: CheckSource::Explicit,
        }
    }

    fn checked_tau(
        enumeration: EnumerationCheck,
        verdict: Option<LayerVerdict>,
        loopback: Option<&str>,
    ) -> InterfaceLatency {
        InterfaceLatency::Measured(MeasuredLatency {
            tau_s: 1711.0 / 96_000.0,
            measured_at: "2026-09-15T23:43:04Z".into(),
            method: "farina_short_ess".into(),
            backend: "jack".into(),
            sample_rate_hz: 96_000,
            period_size: Some(256),
            output_port: "system:playback_2".into(),
            input_port: "system:capture_2".into(),
            enumeration: Some(enumeration),
            session_check: verdict,
            session_check_loopback: loopback.map(str::to_string),
        })
    }

    /// UX rev 4, "refused, and the refusal predates a boundary": the whole
    /// block, line for line.
    #[test]
    fn a_refused_stored_tau_that_predates_a_reboot_renders_ux_block() {
        let tau = checked_tau(
            EnumerationCheck::Crossed {
                boundary: "host rebooted".into(),
                since: Some("2026-09-16T15:10:03Z".into()),
            },
            Some(LayerVerdict::Refused {
                evidence: tau_evidence("2026-09-16T14:02:11Z"),
                via: None,
                delta_bound: None,
            }),
            Some("out1_in1"),
        );
        let c = CONT_INDENT;
        assert_eq!(
            interface_latency_lines(Some(&tau), 11, 96_000, "2026-09-16T15:19:04Z"),
            vec![
                "  latency       17.8229 ms  (1711 samples, stored, not applied)".to_string(),
                format!("{c}measured 2026-09-15T23:43:04Z, 15.6 h before capture"),
                format!("{c}host rebooted at 2026-09-16T15:10:03Z"),
                format!("{c}REFUSED \u{2014} 1743 samples read 2026-09-16T14:02:11Z (\u{394} +32)"),
                format!("{c}refusal predates the reboot; stands until a check passes"),
                format!("{c}check: re-run `ac calibrate` with loopback patched"),
            ]
        );
    }

    /// UX rev 4, "acoustic pair, propagated refusal". A refusal made after
    /// the boundary prints no `predates` line.
    #[test]
    fn a_propagated_refusal_renders_the_via_block() {
        let tau = checked_tau(
            EnumerationCheck::Same,
            Some(LayerVerdict::Refused {
                evidence: tau_evidence("2026-09-16T14:02:11Z"),
                via: Some("out1_in1".into()),
                delta_bound: None,
            }),
            Some("out1_in1"),
        );
        let lines = interface_latency_lines(Some(&tau), 11, 96_000, "2026-09-16T13:54:44Z");
        let c = CONT_INDENT;
        assert_eq!(
            lines[0],
            "  latency       17.8229 ms  (1711 samples, stored, not applied)"
        );
        assert_eq!(
            lines[2..].to_vec(),
            vec![
                format!("{c}same device enumeration as this capture"),
                format!("{c}REFUSED \u{2014} via [out1_in1], whose \u{3c4} moved +32 samples"),
                format!("{c}check: `ac calibrate check`; if still refused,"),
                format!("{c}re-run `ac calibrate` for this pair"),
            ]
        );

        let after = checked_tau(
            EnumerationCheck::Crossed {
                boundary: "host rebooted".into(),
                since: Some("2026-09-16T13:41:52Z".into()),
            },
            Some(LayerVerdict::Refused {
                evidence: tau_evidence("2026-09-16T14:02:11Z"),
                via: None,
                delta_bound: None,
            }),
            None,
        );
        let lines = interface_latency_lines(Some(&after), 11, 96_000, "2026-09-16T15:19:04Z");
        assert!(!lines.iter().any(|l| l.contains("predates")), "{lines:?}");
        assert!(
            !lines.iter().any(|l| l.contains("UNVERIFIED")),
            "a measured verdict answered the boundary: {lines:?}"
        );
    }

    /// `not_covered` names the loopback in lowercase; any other unverified
    /// cause keeps the `(stored)` head and adds its verdict line.
    #[test]
    fn unverified_verdicts_keep_the_stored_head_and_add_their_line() {
        use ac_core::shared::calibration::session::UnverifiedCause;
        let c = CONT_INDENT;
        let not_covered = checked_tau(
            EnumerationCheck::Same,
            Some(LayerVerdict::unverified(
                UnverifiedCause::NotCovered,
                "check covers [out1_in1], not [out0_in0]",
            )),
            Some("out1_in1"),
        );
        let lines = interface_latency_lines(Some(&not_covered), 11, 96_000, "2026-09-16T13:54:44Z");
        assert!(lines[0].ends_with("(1711 samples, stored)"), "{lines:?}");
        assert_eq!(
            lines.last().unwrap(),
            &format!("{c}session check does not cover this pair (loopback [out1_in1])")
        );

        let unreadable = checked_tau(
            EnumerationCheck::Same,
            Some(LayerVerdict::unverified(
                UnverifiedCause::RefusalsUnreadable,
                "session_refusals.json unreadable: expected value at line 1 column 1; \
                 check: its permissions and contents, beside cal.json",
            )),
            None,
        );
        let lines = interface_latency_lines(Some(&unreadable), 11, 96_000, "2026-09-16T13:54:44Z");
        assert!(lines[0].ends_with("(1711 samples, stored)"), "{lines:?}");
        assert!(
            lines.contains(&format!(
                "{c}check: its permissions and contents, beside cal.json"
            )),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with(&format!(
                "{c}UNVERIFIED \u{2014} session_refusals.json unreadable"
            ))),
            "{lines:?}"
        );

        // A report from before v11: no verdict line at all.
        let old = checked_tau(EnumerationCheck::Same, None, None);
        assert_eq!(
            interface_latency_lines(Some(&old), 10, 96_000, "2026-09-16T13:54:44Z").len(),
            3
        );
    }

    /// R6-5: the refused flight-time line sits after the #359 arms, so a
    /// reference disagreement keeps its own, more specific line.
    #[test]
    fn flight_time_line_names_a_refused_stored_latency() {
        let refused = LayerVerdict::Refused {
            evidence: tau_evidence("2026-09-16T14:02:11Z"),
            via: Some("out1_in1".into()),
            delta_bound: None,
        };
        let mut stats = stats_with(
            None,
            ArrivalCheck::Unchecked {
                reason: String::new(),
            },
        );
        stats.interface_latency_check = Some(refused.clone());
        assert_eq!(
            flight_time_line(&stats),
            vec![format!(
                "{}not shown \u{2014} stored latency refused (below)",
                label_prefix("flight time")
            )]
        );

        let shift = TauDisagreement {
            reading1_s: 0.0,
            reading2_s: 1024.0 / 96_000.0,
            delta_samples: 1024,
            sample_rate: 96_000,
            period_size: Some(1024),
            periods: Some(1),
        };
        let mut stats = stats_with(None, ArrivalCheck::PeriodShift(shift));
        stats.interface_latency_check = Some(refused);
        assert!(flight_time_line(&stats)[0].contains("period shift"));
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
            "peak SNR -103.45 dB, need 124.00 dB; check: reference loopback cable, ref input gain",
            "peak at reference window edge; check: reference loopback routing, capture tail",
            "peak at reference window edge, SNR -103.45 dB, need 124.00 dB; check: reference \
             loopback routing, capture tail, reference loopback cable, ref input gain",
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

    // ── #501: the `IR sweep` block, `captured`, banner and SNR rows ──────

    /// The ack a current daemon sends for a bare `plot ir`.
    fn default_ack() -> serde_json::Value {
        serde_json::json!({
            "ok": true, "f1_hz": 20.0, "f2_hz": 20000.0, "duration": 4.0,
            "n_harmonics": 5, "tail_s": 0.5, "window_default_s": 0.4,
        })
    }

    /// UX frame "default arguments": values from the ack, every tag
    /// `default`, decimal points on `level`'s column (index 17).
    #[test]
    fn ir_stimulus_lines_print_the_default_frame() {
        let lines = ir_stimulus_lines(&default_ack(), IrTyped::default());
        assert_eq!(
            lines,
            vec![
                "  IR sweep",
                "  band       20 Hz \u{2192} 20000 Hz  (default)",
                "  length        4.00 s  (default)",
                "  window        0.40 s  (default)",
                "  harmonics     5 orders  (default)",
                "  tail          0.50 s  (default)",
            ]
        );
        let level = "  level       -40.0 dBFS  (typed)";
        for line in &lines[2..] {
            if let Some(dot) = line.find('.') {
                assert_eq!(dot, level.find('.').unwrap(), "{line:?}");
            }
        }
    }

    /// UX worst case: every field typed; a typed window is in samples.
    #[test]
    fn ir_stimulus_lines_print_the_typed_frame() {
        let ack = serde_json::json!({
            "f1_hz": 200.0, "f2_hz": 8000.0, "duration": 4.0,
            "n_harmonics": 12, "tail_s": 0.25, "window_len": 16384,
        });
        let typed = IrTyped {
            f1: true,
            f2: true,
            duration: true,
            window: true,
            n_harmonics: true,
            tail: true,
        };
        assert_eq!(
            ir_stimulus_lines(&ack, typed)[1..],
            [
                "  band       200 Hz \u{2192} 8000 Hz  (typed)",
                "  length        4.00 s  (typed)",
                "  window     16384 samples  (typed)",
                "  harmonics    12 orders  (typed)",
                "  tail          0.25 s  (typed)",
            ]
        );
    }

    /// An older daemon echoes nothing: every row says so, in `level`'s
    /// fallback wording, and no CLI-side default is shown in its place.
    #[test]
    fn ir_stimulus_lines_fall_back_for_an_older_daemon() {
        let lines = ir_stimulus_lines(&serde_json::json!({"ok": true}), IrTyped::default());
        for (line, label) in
            lines[1..]
                .iter()
                .zip(["band", "length", "window", "harmonics", "tail"])
        {
            assert_eq!(*line, format!("  {label:<11}(not reported by this daemon)"));
        }
    }

    #[test]
    fn ir_stimulus_lines_tag_a_partly_typed_band_per_edge() {
        let typed = IrTyped {
            f1: true,
            ..IrTyped::default()
        };
        assert!(
            ir_stimulus_lines(&default_ack(), typed)[1].ends_with("(start typed, stop default)")
        );
    }

    /// `captured` reads the ack: 4.00 s + 0.50 s under the new defaults,
    /// never the pre-#501 CLI copy (1.00 s).
    #[test]
    fn captured_line_reads_the_echoed_sweep_and_tail() {
        assert_eq!(
            captured_line(Some(4.0), Some(0.5)),
            "  captured      4.50 s  (4.00 s sweep + 0.50 s tail)"
        );
        assert_eq!(
            captured_line(None, Some(0.5)),
            "  captured      (not reported by this daemon)"
        );
    }

    #[test]
    fn deconvolution_failed_lines_name_the_sweep_and_the_chain() {
        assert_eq!(
            deconvolution_failed_lines("pre-impulse SNR below threshold"),
            vec![
                "  DECONVOLUTION FAILED \u{2014} pre-impulse SNR below threshold".to_string(),
                format!("{CONT_INDENT}check: sweep length, band, window (above)"),
                format!("{CONT_INDENT}check: drive level, input gain, distance, room noise"),
            ]
        );
    }

    /// The threshold and its basis print on a pass as well as a failure;
    /// a non-finite figure gets neither.
    #[test]
    fn pre_impulse_snr_lines_show_the_threshold_and_basis_on_pass_and_fail() {
        let basis = format!("{CONT_INDENT}fixed threshold, scored for the default sweep only");
        let mut pass = stats_with(None, ArrivalCheck::Agree);
        pass.pre_impulse_snr_db = 21.5;
        assert_eq!(
            pre_impulse_snr_lines(&pass),
            vec![
                "  pre-imp SNR   21.5 dB  (required \u{2265} 18.0 dB)".to_string(),
                basis.clone(),
            ]
        );
        let mut fail = pass.clone();
        fail.pre_impulse_snr_db = 12.3;
        fail.verdict = IrVerdict::Failed {
            reason: "pre-impulse SNR below threshold".into(),
        };
        assert_eq!(
            pre_impulse_snr_lines(&fail),
            vec![
                "  pre-imp SNR   12.3 dB  (required \u{2265} 18.0 dB)".to_string(),
                basis,
            ]
        );
        let mut silent = pass.clone();
        silent.pre_impulse_snr_db = f64::INFINITY;
        assert_eq!(
            pre_impulse_snr_lines(&silent),
            vec!["  pre-imp SNR   \u{221e} dB  (zero measured floor)".to_string()]
        );
    }
    // ─── #537: the band-limited arrival's rows ───────────────────────────

    /// #537 UX's `BroadbandLater` shape: 96 kHz, τ 1711, arrival +596 after
    /// τ and the broadband peak +1502 samples after the arrival.
    fn broadband_later_stats() -> IrStats {
        let mut stats = stats_with(Some(596.0 / 96_000.0), ArrivalCheck::Agree);
        stats.arrival_index = 2_346;
        stats.peak_index = 2_346 + 1_502;
        stats.band_limited_snr_db = Some(45.6);
        stats.arrival_cross_check = ArrivalCrossCheck::BroadbandLater { gap: 1_502 };
        stats
    }

    fn stored_tau() -> InterfaceLatency {
        checked_tau(EnumerationCheck::Same, None, None)
    }

    /// A scored distance check at `distance_m` (default c) for a flight of
    /// `flight_samples` at 96 kHz.
    fn distance_scored(distance_m: f64, flight_samples: f64) -> DistanceCheck {
        use ac_core::measurement::report::DistanceWindow;
        let window = DistanceWindow::new(distance_m, None);
        let excess_s = flight_samples / 96_000.0 - window.expected_s;
        if excess_s < window.low_s {
            DistanceCheck::TooEarly { window, excess_s }
        } else if excess_s > window.high_s {
            DistanceCheck::TooLate { window, excess_s }
        } else {
            DistanceCheck::Consistent { window, excess_s }
        }
    }

    /// #537 UX revision 3's headline, rig capture `22-05-38Z` (2 m, default
    /// band, no temperature), rows from the source row through `broadband Δ`
    /// minus `peak`, verbatim.
    fn headline_stats() -> IrStats {
        let mut stats = stats_with(Some(596.0 / 96_000.0), ArrivalCheck::Agree);
        stats.arrival_index = 21_635 - 128;
        stats.peak_index = 23_053;
        stats.band_limited_snr_db = Some(53.8);
        stats.arrival_lobe_margin_db = Some(6.1);
        stats.arrival_lobe_offset = Some(40);
        stats.broadband_delta_level_db = Some(-4.4);
        stats.arrival_cross_check = ArrivalCrossCheck::Agrees { gap: 128 };
        stats.distance_check = distance_scored(2.0, 596.0);
        stats
    }

    #[test]
    fn the_headline_block_matches_ux_revision_3() {
        let stats = headline_stats();
        let mut lines: Vec<String> = second_lobe_line(&stats).into_iter().collect();
        lines.extend(flight_time_block(&stats, Some(&stored_tau())));
        lines.extend(distance_lines(&stats));
        lines.extend(arrival_snr_lines(&stats));
        lines.extend(broadband_delta_lines(&stats));
        let want = [
            "                second lobe 40 samples after, 6.1 dB down (required \u{2265} 3.0 dB)",
            "  flight time   +596 samples  (+6.208 ms, arrival \u{2212} latency)",
            "  distance      2 m: +36 samples (+0.377 ms) re d/c, inside window",
            "                d/c +560 samples (+5.831 ms), c 343.0 m/s assumed",
            "                window \u{2212}25 \u{2026} +121 samples re d/c",
            "                from tape \u{b1}5 cm, c \u{b1}2 %, speaker allowance +1.0 ms assumed",
            "  arrival SNR   53.8 dB  (above 2 kHz, required \u{2265} 35.0 dB)",
            "                ISO 3382-1:2009 \u{a7}A.3.4 trigger (\u{2212}20 dB) above noise peaks",
            "  broadband \u{394}   +128 samples  (+1.333 ms, broadband peak \u{2212} arrival)",
            "                to broadband peak at sample 21635, \u{2212}4.4 dB (first \u{2265} \u{2212}6.0 dB)",
            "                tolerance \u{b1}192 samples (\u{b1}2.0 ms), rig-scored on 1 speaker",
        ];
        assert_eq!(lines, want);
    }

    /// UX revision 3's 0.5 m frame: only the distance block differs — the
    /// excess moves and the window's lower edge tightens with d.
    #[test]
    fn the_half_metre_distance_block_matches_ux_revision_3() {
        let mut stats = headline_stats();
        stats.flight_time_s = Some(198.0 / 96_000.0);
        stats.distance_check = distance_scored(0.5, 198.0);
        assert_eq!(
            distance_lines(&stats),
            [
                "  distance      0.5 m: +58 samples (+0.605 ms) re d/c, inside window",
                "                d/c +140 samples (+1.458 ms), c 343.0 m/s assumed",
                "                window \u{2212}17 \u{2026} +113 samples re d/c",
                "                from tape \u{b1}5 cm, c \u{b1}2 %, speaker allowance +1.0 ms assumed",
            ]
        );
    }

    /// `TooLate` and `TooEarly` withhold on the flight time row, point at
    /// the distance block, and add a `check:` row naming places, per side.
    #[test]
    fn a_flight_outside_the_window_is_withheld_with_its_side() {
        let mut late = headline_stats();
        late.flight_time_s = None;
        late.distance_check = distance_scored(2.0, 1_076.0);
        assert_eq!(
            flight_time_block(&late, Some(&stored_tau())),
            [format!(
                "{}withheld \u{2014} later than 2 m allows (below)",
                label_prefix("flight time")
            )]
        );
        assert_eq!(
            distance_lines(&late),
            [
                "  distance      2 m: +516 samples (+5.377 ms) re d/c, outside window",
                "                d/c +560 samples (+5.831 ms), c 343.0 m/s assumed",
                "                window \u{2212}25 \u{2026} +121 samples re d/c",
                "                from tape \u{b1}5 cm, c \u{b1}2 %, speaker allowance +1.0 ms assumed",
                "                check: typed distance, IR before arrival, speaker DSP latency",
            ]
        );
        let mut early = headline_stats();
        early.flight_time_s = None;
        early.distance_check = distance_scored(2.0, 512.0);
        assert_eq!(
            flight_time_block(&early, Some(&stored_tau())),
            [format!(
                "{}withheld \u{2014} earlier than 2 m allows (below)",
                label_prefix("flight time")
            )]
        );
        let lines = distance_lines(&early);
        assert_eq!(
            lines[0],
            "  distance      2 m: \u{2212}48 samples (\u{2212}0.498 ms) re d/c, outside window"
        );
        assert_eq!(
            lines[4],
            "                check: typed distance, temperature, latency (below)"
        );
    }

    /// The rows that are not verdicts: no distance, a typed 0 m, no stored
    /// τ, and a flight time withheld upstream.
    #[test]
    fn the_distance_row_says_why_it_did_not_score() {
        let mut stats = headline_stats();
        stats.distance_check = DistanceCheck::NotGiven;
        assert_eq!(
            distance_lines(&stats),
            ["  distance      not given \u{2014} flight time not checked (token: 1m)"]
        );
        stats.distance_check = DistanceCheck::NotPositive { distance_m: 0.0 };
        assert_eq!(
            distance_lines(&stats),
            ["  distance      0 m: not checked \u{2014} needs a distance > 0"]
        );
        stats.distance_check = DistanceCheck::NoLatency { distance_m: 2.0 };
        assert_eq!(
            distance_lines(&stats),
            ["  distance      2 m: not checked \u{2014} no stored latency (below)"]
        );
        stats.flight_time_s = None;
        stats.arrival_cross_check = ArrivalCrossCheck::BandLimitedSnrLow { snr_db: 31.2 };
        stats.distance_check = distance_scored(0.5, 198.0);
        assert_eq!(
            distance_lines(&stats),
            ["  distance      0.5 m: not checked \u{2014} flight time withheld (above)"]
        );
        assert_eq!(
            flight_time_block(&stats, Some(&stored_tau())),
            [format!(
                "{}withheld \u{2014} arrival SNR 31.2 dB, required \u{2265} 35.0 dB",
                label_prefix("flight time")
            )]
        );
    }

    /// UX revision 3's precedence: IR side, then distance, then τ — the
    /// first takes the `withheld` row, each further reason one `also:` row.
    #[test]
    fn withheld_reasons_run_cross_check_then_distance_then_tau() {
        let mut stats = headline_stats();
        stats.flight_time_s = None;
        stats.arrival_cross_check = ArrivalCrossCheck::EarlierComparable {
            index: stats.arrival_index - 312,
            level_db: -14.2,
        };
        stats.distance_check = distance_scored(2.0, 1_076.0);
        stats.arrival_check = ArrivalCheck::Mismatch(TauDisagreement {
            reading1_s: 0.0,
            reading2_s: 0.0,
            delta_samples: 16,
            sample_rate: 96_000,
            period_size: Some(1024),
            periods: None,
        });
        assert_eq!(
            flight_time_block(&stats, Some(&stored_tau())),
            [
                format!(
                    "{}withheld \u{2014} earlier peak within 20.0 dB (above)",
                    label_prefix("flight time")
                ),
                format!("{CONT_INDENT}also: later than 2 m allows (below)"),
                format!("{CONT_INDENT}also: ref \u{394} is not zero (below)"),
            ]
        );
        // The distance is a reason, so its verdict prints.
        assert!(distance_lines(&stats)[0].ends_with("outside window"));
        assert_eq!(
            earlier_peak_line(&stats),
            Some(format!(
                "{CONT_INDENT}earlier peak above 2 kHz: 312 samples before, -14.2 dB"
            ))
        );
    }

    /// The `second lobe` row: the same row on a pass and on a refusal, the
    /// side from the offset's sign, and the window when there is no rival.
    #[test]
    fn the_second_lobe_row_prints_on_every_band_limited_capture() {
        let mut stats = headline_stats();
        stats.arrival_lobe_margin_db = Some(0.93);
        stats.arrival_lobe_offset = Some(-17);
        stats.arrival_cross_check = ArrivalCrossCheck::ArrivalAmbiguous {
            margin_db: 0.93,
            offset: -17,
        };
        stats.flight_time_s = None;
        assert_eq!(
            second_lobe_line(&stats),
            Some(format!(
                "{CONT_INDENT}second lobe 17 samples before, 0.9 dB down (required \u{2265} 3.0 dB)"
            ))
        );
        assert_eq!(
            flight_time_block(&stats, Some(&stored_tau())),
            [format!(
                "{}withheld \u{2014} second lobe within 3.0 dB (above)",
                label_prefix("flight time")
            )]
        );
        stats.arrival_lobe_margin_db = Some(f64::INFINITY);
        stats.arrival_lobe_offset = None;
        assert_eq!(
            second_lobe_line(&stats),
            Some(format!(
                "{CONT_INDENT}no second lobe within \u{b1}48 samples (\u{b1}0.500 ms)"
            ))
        );
        stats.arrival_source = ArrivalSource::Peak;
        stats.arrival_lobe_margin_db = None;
        assert_eq!(second_lobe_line(&stats), None);
    }

    /// `BroadbandLater`: the flight time is printed and marked on the row
    /// under it; `broadband Δ` carries the number, the peak it is measured
    /// to, the tolerance and where to look.
    #[test]
    fn broadband_later_prints_the_flight_time_marked() {
        let stats = broadband_later_stats();
        let tau = stored_tau();
        assert_eq!(
            flight_time_block(&stats, Some(&tau)),
            vec![
                format!(
                    "{}+596 samples  (+6.208 ms, arrival \u{2212} latency)",
                    label_prefix("flight time")
                ),
                format!("{CONT_INDENT}broadband peak disagrees by +1502 samples (below)"),
            ]
        );
        assert_eq!(
            broadband_delta_lines(&stats),
            vec![
                format!(
                    "{}+1502 samples  (+15.646 ms, broadband peak \u{2212} arrival)",
                    label_prefix("broadband \u{394}")
                ),
                format!(
                    "{CONT_INDENT}to broadband peak at sample 3848, 0.0 dB (first \u{2265} \
                     \u{2212}6.0 dB)"
                ),
                format!(
                    "{CONT_INDENT}tolerance \u{b1}192 samples (\u{b1}2.0 ms), rig-scored on 1 \
                     speaker"
                ),
                format!("{CONT_INDENT}check: IR below 2 kHz, speaker and mic placement"),
            ]
        );
    }

    /// The mark sits above #461's `latency UNVERIFIED` and #477's reference
    /// line: it qualifies the arrival, which is upstream of τ.
    #[test]
    fn the_cross_check_mark_precedes_the_latency_rows() {
        let mut stats = broadband_later_stats();
        stats.arrival_check = ArrivalCheck::Unchecked {
            reason: String::new(),
        };
        stats.interface_latency_enumeration = None;
        let lines = flight_time_block(&stats, Some(&stored_tau()));
        assert_eq!(lines.len(), 4, "{lines:#?}");
        assert!(lines[1].contains("broadband peak disagrees"));
        assert!(lines[2].contains("latency UNVERIFIED"));
        assert!(lines[3].contains("reference check not run"));
    }

    /// `Agrees`: no mark, no `check:` row — but the target and the
    /// tolerance still print, so a pass is not silent.
    #[test]
    fn agrees_prints_no_mark_and_no_check_row() {
        let mut stats = stats_with(Some(332.0 / 96_000.0), ArrivalCheck::Agree);
        stats.arrival_cross_check = ArrivalCrossCheck::Agrees { gap: 148 };
        let lines = flight_time_block(&stats, Some(&stored_tau()));
        assert_eq!(lines, flight_time_line(&stats));
        assert_eq!(lines.len(), 1);
        let delta = broadband_delta_lines(&stats);
        assert_eq!(delta.len(), 3, "{delta:#?}");
        assert!(delta[0].contains("+148 samples  (+1.542 ms"));
        assert!(delta[1].contains("to broadband peak at sample 748"));
        assert!(delta[2].contains("tolerance"));
    }

    /// Each withholding standing takes the `withheld` row, naming its
    /// evidence row.
    #[test]
    fn withholding_standings_name_their_reason_on_the_flight_time_row() {
        let tau = stored_tau();
        let cases = [
            (
                ArrivalCrossCheck::BandLimitUnavailable {
                    band_top_hz: 2_000.0,
                    required_hz: 4_000.0,
                },
                "withheld \u{2014} sweep ends at 2000 Hz, needs \u{2265} 4000 Hz",
            ),
            (
                ArrivalCrossCheck::BandLimitedSnrLow { snr_db: 14.2 },
                "withheld \u{2014} arrival SNR 14.2 dB, required \u{2265} 35.0 dB",
            ),
            (
                ArrivalCrossCheck::ArrivalAmbiguous {
                    margin_db: 0.93,
                    offset: -17,
                },
                "withheld \u{2014} second lobe within 3.0 dB (above)",
            ),
            (
                ArrivalCrossCheck::EarlierComparable {
                    index: 312,
                    level_db: -14.2,
                },
                "withheld \u{2014} earlier peak within 20.0 dB (above)",
            ),
            (
                ArrivalCrossCheck::BroadbandEarlier { gap: -410 },
                "withheld \u{2014} broadband peak is earlier (see broadband \u{394})",
            ),
        ];
        for (standing, want) in cases {
            let mut stats = stats_with(None, ArrivalCheck::Agree);
            stats.arrival_cross_check = standing.clone();
            assert_eq!(
                flight_time_block(&stats, Some(&tau)),
                vec![format!("{}{want}", label_prefix("flight time"))],
                "{standing:?}"
            );
        }
    }

    /// Withheld for two reasons: the arrival's takes the row, τ's follows on
    /// one `also:` row in #359/#466's words.
    #[test]
    fn a_second_reason_follows_on_an_also_row() {
        let mut stats = stats_with(None, ArrivalCheck::Agree);
        stats.arrival_cross_check = ArrivalCrossCheck::BandLimitedSnrLow { snr_db: 14.2 };
        assert_eq!(
            flight_time_block(&stats, None),
            vec![
                format!(
                    "{}withheld \u{2014} arrival SNR 14.2 dB, required \u{2265} 35.0 dB",
                    label_prefix("flight time")
                ),
                format!("{CONT_INDENT}also: no stored latency for this pair (below)"),
            ]
        );
        stats.arrival_check = ArrivalCheck::Mismatch(TauDisagreement {
            reading1_s: 0.0,
            reading2_s: 0.0,
            delta_samples: 16,
            sample_rate: 96_000,
            period_size: Some(1024),
            periods: None,
        });
        assert_eq!(
            flight_time_block(&stats, Some(&stored_tau()))[1],
            format!("{CONT_INDENT}also: ref \u{394} is not zero (below)")
        );
    }

    /// `EarlierComparable`: the evidence row under the source row.
    #[test]
    fn earlier_comparable_prints_the_earlier_peak_under_the_source() {
        let mut stats = stats_with(None, ArrivalCheck::Agree);
        stats.arrival_index = 600;
        stats.arrival_cross_check = ArrivalCrossCheck::EarlierComparable {
            index: 312,
            level_db: -14.2,
        };
        assert_eq!(
            earlier_peak_line(&stats),
            Some(format!(
                "{CONT_INDENT}earlier peak above 2 kHz: 288 samples before, -14.2 dB"
            ))
        );
        stats.arrival_cross_check = ArrivalCrossCheck::Agrees { gap: 0 };
        assert_eq!(earlier_peak_line(&stats), None);
    }

    /// `BroadbandEarlier`: a negative Δ measured to the maximum (no target
    /// row), and a check row that names places above the corner without
    /// claiming a direct path is there (#537 UX revision 3).
    #[test]
    fn broadband_earlier_names_places_to_check_without_a_path_claim() {
        let mut stats = stats_with(None, ArrivalCheck::Agree);
        stats.peak_index = stats.arrival_index - 410;
        stats.broadband_delta_level_db = None;
        stats.arrival_cross_check = ArrivalCrossCheck::BroadbandEarlier { gap: -410 };
        let lines = broadband_delta_lines(&stats);
        assert!(lines[0].contains("-410 samples  (-4.271 ms"), "{lines:#?}");
        assert_eq!(lines.len(), 3, "{lines:#?}");
        assert_eq!(
            lines[2],
            format!("{CONT_INDENT}check: IR above 2 kHz, mic axis, obstructions")
        );
        assert!(!lines.iter().any(|l| l.contains("direct")));
    }

    /// `BandLimitUnavailable`: the broadband peak is the arrival and the
    /// flight time is withheld (#537 UX revision 2); the two band-limited
    /// rows say why they are absent, and there is no `second lobe` row.
    #[test]
    fn band_limit_unavailable_rows_say_why() {
        let mut stats = stats_with(None, ArrivalCheck::Agree);
        stats.arrival_source = ArrivalSource::Peak;
        stats.band_limited_snr_db = None;
        stats.arrival_lobe_margin_db = None;
        stats.arrival_lobe_offset = None;
        stats.broadband_delta_level_db = None;
        stats.arrival_cross_check = ArrivalCrossCheck::BandLimitUnavailable {
            band_top_hz: 2_000.0,
            required_hz: 4_000.0,
        };
        let why = "sweep ends at 2000 Hz, needs \u{2265} 4000 Hz";
        let mut mismatch = stats.clone();
        assert_eq!(
            flight_time_block(&stats, Some(&stored_tau())),
            [format!(
                "{}withheld \u{2014} {why}",
                label_prefix("flight time")
            )]
        );
        mismatch.arrival_check = ArrivalCheck::Mismatch(TauDisagreement {
            reading1_s: 0.0,
            reading2_s: 0.0,
            delta_samples: -3,
            sample_rate: 96_000,
            period_size: Some(256),
            periods: None,
        });
        assert_eq!(
            flight_time_block(&mismatch, Some(&stored_tau())),
            [
                format!("{}withheld \u{2014} {why}", label_prefix("flight time")),
                format!("{CONT_INDENT}also: ref \u{394} is not zero (below)"),
            ]
        );
        assert_eq!(second_lobe_line(&stats), None);
        assert_eq!(
            arrival_snr_lines(&stats),
            vec![format!(
                "{}not measured \u{2014} {why}",
                label_prefix("arrival SNR")
            )]
        );
        assert_eq!(
            broadband_delta_lines(&stats),
            vec![format!(
                "{}not computed \u{2014} {why}",
                label_prefix("broadband \u{394}")
            )]
        );
    }

    /// Every #537 row fits 80 columns at a 96 kHz five-digit sample count,
    /// and at a 10 m distance.
    #[test]
    fn band_limited_rows_fit_80_columns() {
        let mut wide = broadband_later_stats();
        wide.peak_index = wide.arrival_index + 38_399;
        wide.arrival_cross_check = ArrivalCrossCheck::BroadbandLater { gap: 38_399 };
        wide.broadband_delta_level_db = Some(-5.9);
        wide.arrival_lobe_margin_db = Some(12.3);
        wide.arrival_lobe_offset = Some(-48);
        let mut lines = flight_time_block(&wide, None);
        lines.extend(second_lobe_line(&wide));
        lines.extend(broadband_delta_lines(&wide));
        lines.extend(arrival_snr_lines(&wide));
        let mut unavailable = wide.clone();
        unavailable.arrival_cross_check = ArrivalCrossCheck::BandLimitUnavailable {
            band_top_hz: 3_999.0,
            required_hz: 4_000.0,
        };
        lines.extend(flight_time_block(&unavailable, Some(&stored_tau())));
        lines.extend(broadband_delta_lines(&unavailable));
        lines.extend(arrival_snr_lines(&unavailable));
        let mut earlier = wide.clone();
        earlier.arrival_cross_check = ArrivalCrossCheck::EarlierComparable {
            index: 0,
            level_db: -19.9,
        };
        lines.extend(earlier_peak_line(&earlier));
        lines.extend(flight_time_block(&earlier, None));
        for (distance, flight) in [(10.0, 2_798.0 + 166.0), (10.0, 5_000.0), (0.5, 100.0)] {
            let mut d = wide.clone();
            d.distance_check = distance_scored(distance, flight);
            d.flight_time_s = None;
            lines.extend(distance_lines(&d));
            lines.extend(flight_time_block(&d, Some(&stored_tau())));
        }
        for line in lines {
            assert!(
                line.chars().count() <= 80,
                "line {line:?} runs to {} columns",
                line.chars().count()
            );
        }
    }
}
