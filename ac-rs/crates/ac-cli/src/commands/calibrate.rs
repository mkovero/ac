use std::io::{self, Write};

use super::{check_ack, get_cal, level_to_dbfs, print_level};
use crate::client::AcClient;
use crate::parse::{CommandKind, LevelSpec};

pub fn run(cmd: &CommandKind, client: &mut AcClient) {
    let (level, level_defaulted, out_ch, in_ch) = match cmd {
        CommandKind::Calibrate {
            level,
            level_defaulted,
            output_channel,
            input_channel,
        } => (level, *level_defaulted, output_channel, input_channel),
        _ => unreachable!(),
    };

    let cal_info = get_cal(client);
    let ref_dbfs = match level {
        LevelSpec::Dbfs(v) => *v,
        other => level_to_dbfs(other, cal_info.as_ref()),
    };

    let mut cmd_json = serde_json::json!({"cmd": "calibrate", "ref_dbfs": ref_dbfs});
    if let Some(ch) = out_ch {
        cmd_json["output_channel"] = (*ch).into();
    }
    if let Some(ch) = in_ch {
        cmd_json["input_channel"] = (*ch).into();
    }

    let ack = check_ack(client.send_cmd(&cmd_json, Some(5000)), "calibrate");
    println!("  Calibration started: 1 kHz");
    print_level(
        ack.get("ref_dbfs").and_then(|v| v.as_f64()),
        level_defaulted,
        ack.get("max_dbfs").and_then(|v| v.as_f64()),
        cal_info.as_ref(),
        true,
    );
    println!("  Press Ctrl+C or type q to cancel.\n");

    loop {
        let frame = match client.recv_data(120000) {
            Some(f) => f,
            None => {
                eprintln!("  error: calibration timed out");
                return;
            }
        };
        let (topic, data) = frame;

        if topic == "cal_prompt" {
            let text = data.get("text").and_then(|v| v.as_str()).unwrap_or("");
            println!("\n  {text}\n");

            let dmm_vrms = data.get("dmm_vrms").and_then(|v| v.as_f64());

            let (prompt, try_hint) = if let Some(dmm) = dmm_vrms {
                let hint = format!("{:.4} mVrms", dmm * 1000.0);
                (
                    format!(
                        "  Enter to accept ({hint}), or override \
                         (skip to keep stored, clear to erase, q to cancel): "
                    ),
                    "  Try:  0.245  or  245mV  (or skip / clear)",
                )
            } else {
                (
                    "  DMM reading (e.g. 245mV or 0.245; Enter or skip keeps the stored \
                     value, clear erases it, q to cancel): "
                        .to_string(),
                    "  Try:  0.245  or  245mV  (or clear)",
                )
            };

            let reply = loop {
                print!("{prompt}");
                io::stdout().flush().ok();
                let raw = read_line();
                match classify_entry(&raw, dmm_vrms) {
                    Entry::Cancel => {
                        println!("  Calibration cancelled.");
                        client.send_cmd(&serde_json::json!({"cmd": "stop"}), None);
                        return;
                    }
                    Entry::Reply(r) => break r,
                    Entry::Unparsed => println!("{try_hint}"),
                }
            };

            client.send_cmd(&reply.to_cmd(), None);
        } else if topic == "cal_done" {
            let key = data.get("key").and_then(|v| v.as_str()).unwrap_or("?");
            println!("\n  Calibration saved: [{key}]");
            if let (Some(out), Some(inp)) = (
                data.get("output_port").and_then(|v| v.as_str()),
                data.get("input_port").and_then(|v| v.as_str()),
            ) {
                println!("  Output: {out}  \u{2192}  Input: {inp}");
            }
            print_cal_leg("Output", &data, "vrms_at_0dbfs_out", "out_state");
            print_cal_leg("Input", &data, "vrms_at_0dbfs_in", "in_state");
            print_tau_leg(&data);
            if let Some(err) = data.get("error").and_then(|v| v.as_str()) {
                println!("  Note: {err}");
            }
            println!();
            return;
        } else if topic == "error" {
            let msg = data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("error");
            eprintln!("  error: {msg}");
            return;
        }
    }
}

/// What the user asked the daemon to do with one voltage leg. Kept
/// distinct from `Option<f64>` so "I did not measure this" cannot be
/// mistaken on the wire for "erase it" (#279).
#[derive(Debug, PartialEq)]
enum Reply {
    Value(f64),
    Skip,
    Clear,
}

impl Reply {
    fn to_cmd(&self) -> serde_json::Value {
        match self {
            Reply::Value(v) => serde_json::json!({"cmd": "cal_reply", "vrms": v}),
            Reply::Skip => serde_json::json!({"cmd": "cal_reply", "vrms": null}),
            Reply::Clear => serde_json::json!({"cmd": "cal_reply", "vrms": null, "clear": true}),
        }
    }
}

/// One line of operator input, resolved to an intent. Split out of the
/// prompt loop so the keystroke -> intent mapping is testable without a
/// terminal — getting that mapping wrong is exactly what #279 was: Enter
/// meant "skip" to the user and `None` meant "erase" to the daemon.
///
/// `dmm` is the pre-filled reading the daemon offered, if any. It is the
/// only thing that changes what an empty line means: accept the offered
/// reading when there is one, keep the stored value when there is not.
#[derive(Debug, PartialEq)]
enum Entry {
    Cancel,
    Reply(Reply),
    Unparsed,
}

fn classify_entry(input: &str, dmm: Option<f64>) -> Entry {
    let t = input.trim();
    if t.eq_ignore_ascii_case("q") {
        Entry::Cancel
    } else if t.is_empty() {
        match dmm {
            Some(v) => Entry::Reply(Reply::Value(v)),
            None => Entry::Reply(Reply::Skip),
        }
    } else if t.eq_ignore_ascii_case("skip") {
        Entry::Reply(Reply::Skip)
    } else if t.eq_ignore_ascii_case("clear") {
        Entry::Reply(Reply::Clear)
    } else {
        match parse_vrms(t) {
            Some(v) => Entry::Reply(Reply::Value(v)),
            None => Entry::Unparsed,
        }
    }
}

/// Render one voltage leg of a `cal_done` frame. The `*_state` word is
/// what separates a value this run measured from one it left alone, and
/// an absent value from either.
fn print_cal_leg(label: &str, data: &serde_json::Value, vrms_key: &str, state_key: &str) {
    let state = data.get(state_key).and_then(|v| v.as_str());
    let label = format!("{label}:");
    match data.get(vrms_key).and_then(|v| v.as_f64()) {
        Some(v) => {
            let dbu = ac_core::shared::conversions::vrms_to_dbu(v);
            let note = match state {
                Some("unchanged") => "   (unchanged)",
                Some("measured") => "   (measured)",
                _ => "",
            };
            println!(
                "  {label:<8}0 dBFS = {:>14}  =  {dbu:+.2} dBu{note}",
                ac_core::shared::conversions::fmt_vrms(v)
            );
        }
        None => println!("  {label:<8}not calibrated"),
    }
}

/// Render the `Delay:` leg of a `cal_done` frame (#281/#347) — third leg of
/// the same block, same `{label:<8}` alignment as `print_cal_leg`'s
/// Output/Input rows.
///
/// τ has no `(unchanged)` state — unlike the voltage legs, it is not
/// prompt-driven, so a skipped voltage prompt never touches it (see
/// ZMQ.md's `cal_done` schema). The sample rate + period size are printed
/// alongside the value on purpose: without them the number is
/// unfalsifiable a year and three `-p` changes later, which is the exact
/// failure #281 exists to close (device/backend/port identity is already
/// implied by the session and the `[out_in]` key on the line above, so
/// repeating those here would be redundant, not missing).
///
/// #347: a single reading is not a measurement of τ on this stack, so
/// `calibrate` now runs two independent client lifecycles and refuses
/// rather than storing on disagreement — `"measured"` names how many
/// readings agreed, and the two new disagreement states show both raw
/// readings so an operator can see the evidence, not just the conclusion.
fn print_tau_leg(data: &serde_json::Value) {
    for line in render_tau_leg(data) {
        println!("{line}");
    }
}

/// Pure line-rendering core of [`print_tau_leg`]. Returns each output line
/// so the mapping from `cal_done` JSON to text is unit-testable — split out
/// for the same reason [`render_tau_history_leg`] is: a `println!`-only
/// function cannot be asserted against without capturing stdout (#388 QA).
fn render_tau_leg(data: &serde_json::Value) -> Vec<String> {
    let state = data.get("tau_state").and_then(|v| v.as_str()).unwrap_or("");
    let sample_rate = data.get("tau_sample_rate").and_then(|v| v.as_u64());
    let period_size = data.get("tau_period_size").and_then(|v| v.as_u64());
    let conditions = match (sample_rate, period_size) {
        (Some(sr), Some(p)) => format!("{sr} Hz, period {p}"),
        (Some(sr), None) => format!("{sr} Hz"),
        (None, _) => String::new(),
    };
    match state {
        "measured" => match data.get("tau_s").and_then(|v| v.as_f64()) {
            Some(tau_s) => {
                // #363: the value alone on its line, the evidence beneath it.
                // `corroborated` / `N readings agree` are gone: they compress
                // evidence into a verdict the instrument cannot reach — 42 of
                // 97 rig runs printed that verdict over a τ one period short.
                let mut lines = vec![format!(
                    "  {:<8}{:.4} ms   {}   (measured, {conditions})",
                    "Delay:",
                    tau_s * 1000.0,
                    samples_clause(tau_s, sample_rate),
                )];
                lines.push(format!(
                    "          {}",
                    lifetimes_clause(
                        data.get("tau_agreement_count").and_then(|v| v.as_u64()),
                        data.get("tau_reading_separation_s")
                            .and_then(|v| v.as_f64()),
                        true,
                    )
                ));
                if let Some(line) = declared_clause(data, true) {
                    lines.push(format!("          {line}"));
                }
                if let (Some(snr), Some(threshold)) = (
                    data.get("tau_pre_impulse_snr_db").and_then(|v| v.as_f64()),
                    data.get("tau_snr_threshold_db").and_then(|v| v.as_f64()),
                ) {
                    lines.push(format!(
                        "          peak SNR {snr:.1} dB pre-impulse, threshold {threshold:.1} dB"
                    ));
                }
                lines
            }
            None => vec![format!("  {:<8}not measured", "Delay:")],
        },
        // #363: the graph's own account of the path moved between the two
        // lifecycles, so agreement between the readings proves nothing.
        "disagree_declared_latency" => render_tau_declared_latency_leg(data, sample_rate),
        "error" => {
            // A real measurement failure with a loopback present — state
            // the observed cause, unlike the no-loopback case below where
            // the daemon has no cause to report, only an observation.
            let msg = data
                .get("tau_error")
                .and_then(|v| v.as_str())
                .unwrap_or("measurement failed");
            vec![format!("  {:<8}not measured ({msg})", "Delay:")]
        }
        "disagree_period_shift" | "disagree_other" => {
            render_tau_disagreement_leg(state, data, sample_rate)
        }
        // #368: the peak's own SNR fell short of the threshold it was
        // judged against — both are what the daemon actually measured, so
        // print them rather than an inferred wiring conclusion.
        "not_measured_low_snr" => render_tau_low_snr_leg(data).unwrap_or_else(|| {
            // Fields absent (older daemon claiming this state without
            // them): fall through to the raw-state rendering below rather
            // than assert numbers the daemon never sent.
            vec![format!("  {:<8}not measured (state: {state})", "Delay:")]
        }),
        // #494: the peak sat inside the window's edge margin — and maybe
        // failed the SNR gate too, which the daemon says, not this client.
        "not_measured_window_edge" => render_tau_window_edge_leg(data, sample_rate)
            .unwrap_or_else(|| vec![format!("  {:<8}not measured (state: {state})", "Delay:")]),
        "refused_xrun" => render_tau_xrun_leg(data),
        // Anything unrecognised (older daemon, or a future state this
        // client doesn't know): state the raw wire value, not an inferred
        // cause the instrument cannot verify. Also covers the retired
        // `"not_measured_no_loopback"` state — an old daemon predating
        // #368 that still sends it renders here, on the raw value, rather
        // than asserting the wiring conclusion #368 removed.
        _ => vec![format!("  {:<8}not measured (state: {state})", "Delay:")],
    }
}

/// Places to check when a τ peak sat at the window edge (#494 UX): the
/// round trip the interface adds has to land inside the window.
const TAU_EDGE_CHECK: &str =
    "interface buffer size, loopback routing, delay devices in the loopback path";
/// Places to check when a τ peak's SNR fell below the threshold (#494 UX).
const TAU_LOW_SNR_CHECK: &str = "loopback cable, output level, input gain";
/// Evidence lines under a `Delay:` headline start at column 10.
const TAU_EVIDENCE_INDENT: &str = "          ";

/// `reading N of 2 refused, nothing stored` (#494). A daemon between #368
/// and #494 sends no reading number; the line then says only what is known.
fn tau_refused_line(data: &serde_json::Value) -> String {
    match data.get("tau_refused_reading").and_then(|v| v.as_u64()) {
        Some(n) => format!("{TAU_EVIDENCE_INDENT}reading {n} of 2 refused, nothing stored"),
        None => format!("{TAU_EVIDENCE_INDENT}nothing stored"),
    }
}

fn tau_snr_pair(data: &serde_json::Value) -> Option<(f64, f64)> {
    Some((
        data.get("tau_pre_impulse_snr_db")?.as_f64()?,
        data.get("tau_snr_threshold_db")?.as_f64()?,
    ))
}

/// Two decimals on both figures (#494 UX): a refused 23.96 against 24.00
/// must not print as `24.0 … threshold 24.0`.
fn tau_snr_line(snr: f64, threshold: f64) -> String {
    format!(
        "{TAU_EVIDENCE_INDENT}{:<10}{snr:.2} dB pre-impulse, threshold {threshold:.2} dB",
        "peak SNR"
    )
}

fn tau_check_lines(places: &str) -> Vec<String> {
    super::plot::wrap_comma_list(&format!("{TAU_EVIDENCE_INDENT}check: "), places, 80)
}

/// Render #368's `not_measured_low_snr` in #494's evidence layout, or
/// `None` when the SNR pair is missing. No peak position: a noise argmax's
/// position means nothing and would only invite reading it as a τ.
fn render_tau_low_snr_leg(data: &serde_json::Value) -> Option<Vec<String>> {
    let (snr, threshold) = tau_snr_pair(data)?;
    let mut lines = vec![
        format!("  {:<8}not measured (peak SNR below threshold)", "Delay:"),
        tau_refused_line(data),
        tau_snr_line(snr, threshold),
    ];
    lines.extend(tau_check_lines(TAU_LOW_SNR_CHECK));
    Some(lines)
}

/// A millisecond figure without trailing zeros: `50`, `48.625`.
fn trimmed_ms(ms: f64) -> String {
    let s = format!("{ms:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Render #494's `not_measured_window_edge`: where the peak sat against the
/// window bounds and margin, in the same units, so "at the edge" can be
/// checked by eye. The headline names every gate the peak failed, as the
/// daemon's `tau_snr_below_threshold` says — this client never compares the
/// two figures itself. `None` when the position fields are missing.
fn render_tau_window_edge_leg(
    data: &serde_json::Value,
    sample_rate: Option<u64>,
) -> Option<Vec<String>> {
    let peak = data.get("tau_peak_offset_samples")?.as_i64()?;
    let first = data.get("tau_window_first_offset_samples")?.as_i64()?;
    let last = data.get("tau_window_last_offset_samples")?.as_i64()?;
    let margin = data.get("tau_edge_margin_samples")?.as_u64()?;
    // The SNR pair is absent after an xrun (the gate never ran); the flag
    // travels with it.
    let snr = tau_snr_pair(data);
    let below = snr.is_some()
        && data
            .get("tau_snr_below_threshold")
            .and_then(|v| v.as_bool())
            == Some(true);
    let headline = if below {
        "peak at window edge, peak SNR below threshold"
    } else {
        "peak at window edge"
    };
    let (peak_ms, half_ms) = match sample_rate {
        Some(sr) if sr > 0 => (
            format!("   {:+.4} ms", peak as f64 * 1000.0 / sr as f64),
            format!(
                " (\u{b1}{} ms)",
                trimmed_ms(first.unsigned_abs() as f64 * 1000.0 / sr as f64)
            ),
        ),
        _ => (String::new(), String::new()),
    };
    let mut lines = vec![
        format!("  {:<8}not measured ({headline})", "Delay:"),
        tau_refused_line(data),
        format!(
            "{TAU_EVIDENCE_INDENT}{:<10}{peak:+} samples{peak_ms}",
            "peak"
        ),
        format!(
            "{TAU_EVIDENCE_INDENT}{:<10}{first} to {last:+} samples{half_ms}, \
             edge margin {margin} samples",
            "window"
        ),
    ];
    if let Some((snr, threshold)) = snr {
        lines.push(tau_snr_line(snr, threshold));
    }
    // Window-side places first: the edge observation is the more specific
    // one. The order is not a claim about the cause.
    let places = if below {
        format!("{TAU_EDGE_CHECK}, {TAU_LOW_SNR_CHECK}")
    } else {
        TAU_EDGE_CHECK.to_string()
    };
    lines.extend(tau_check_lines(&places));
    Some(lines)
}

/// Render one of the two #347 disagreement states: a period-shift (the
/// issue's own root cause — a graph-buffering shift, software, not
/// hardware drift) versus any other mismatch (a different fault class).
/// Both raw readings are shown, not a compressed delta — the fractional
/// part staying identical across a period-shift jump is the exact
/// diagnostic clue #347's rig data uses to prove the fault is software.
fn render_tau_disagreement_leg(
    state: &str,
    data: &serde_json::Value,
    sample_rate: Option<u64>,
) -> Vec<String> {
    let reading1_s = data.get("tau_reading1_s").and_then(|v| v.as_f64());
    let reading2_s = data.get("tau_reading2_s").and_then(|v| v.as_f64());
    let delta_samples = data.get("tau_delta_samples").and_then(|v| v.as_i64());
    let periods = data.get("tau_periods").and_then(|v| v.as_i64());

    let headline = if state == "disagree_period_shift" {
        let n = periods.map(|p| p.unsigned_abs()).unwrap_or(0);
        let plural = if n == 1 { "" } else { "s" };
        format!("2 readings disagree by exactly {n} period{plural}")
    } else {
        "2 readings disagree, not a period multiple".to_string()
    };
    let mut lines = vec![format!("  {:<8}not measured ({headline})", "Delay:")];

    if let (Some(sr), Some(r1), Some(r2), Some(delta)) =
        (sample_rate, reading1_s, reading2_s, delta_samples)
    {
        let r1_samples = r1 * sr as f64;
        let r2_samples = r2 * sr as f64;
        let delta_ms = delta as f64 / sr as f64 * 1000.0;
        // #363: split — one line was 88 columns — and the pair gains the noun
        // `readings`, now that a *declared* pair can appear in the same block.
        lines.push(format!(
            "          readings {r1_samples:.3} samples \u{2192} {r2_samples:.3} samples"
        ));
        lines.push(format!(
            "          \u{394} {delta} samples = {delta_ms:.4} ms at {sr} Hz"
        ));
    }
    // #363: how far apart the lifecycles were, and what the graph declared —
    // a recurrence that names its own layer instead of costing a session.
    if let Some(separation_s) = data
        .get("tau_reading_separation_s")
        .and_then(|v| v.as_f64())
    {
        lines.push(format!(
            "          {}",
            lifetimes_clause(Some(2), Some(separation_s), false)
        ));
    }
    if let Some(line) = declared_clause(data, true) {
        lines.push(format!("          {line}"));
    }
    lines
}

/// τ in whole samples beside the millisecond figure (#363). The failure class
/// this issue documents is an exact multiple of the period, and the period is
/// printed one clause away in samples — in milliseconds the check needs
/// mental arithmetic, in samples it is a division.
fn samples_clause(tau_s: f64, sample_rate: Option<u64>) -> String {
    match sample_rate {
        Some(sr) => format!("{} samples", (tau_s * sr as f64).round() as i64),
        None => String::new(),
    }
}

/// How many lifecycles produced the value and how far apart they were (#363).
///
/// The separation is the quantity this issue is about: a graph-buffering
/// state that persists over seconds defeats a 1.2 s separation, and printing
/// it lets a reader make that judgement instead of being handed a verdict.
/// `with_resolution` adds the match resolution, which belongs only where a
/// comparison concluded the readings matched.
fn lifetimes_clause(
    count: Option<u64>,
    separation_s: Option<f64>,
    with_resolution: bool,
) -> String {
    let n = count.unwrap_or(0);
    if n < 2 {
        return "1 reading, nothing compared".to_string();
    }
    let resolution = if with_resolution {
        ", identical to the sample"
    } else {
        ""
    };
    match separation_s {
        Some(s) => format!("{n} lifetimes {s:.3} s apart{resolution}"),
        None if with_resolution => {
            format!("{n} lifetimes{resolution}, separation not recorded")
        }
        None => format!("{n} lifetimes, separation not recorded"),
    }
}

/// What the graph declared about the path (#363), in samples and never in
/// milliseconds: one word per unit, and keeping it out of ms makes
/// subtracting it from a τ unnatural rather than inviting. That subtraction
/// must never appear — the figure carries `jackd`'s unvalidated `-I`/`-O`.
///
/// `None` (omit the line entirely) when the daemon predates #363 and sent no
/// field at all; a present-but-`null` field means the backend declares
/// nothing, which is stated.
fn declared_clause(data: &serde_json::Value, live: bool) -> Option<String> {
    let field = data.get("tau_reading1_declared_frames")?;
    let both = if live { " (both lifetimes)" } else { "" };
    match field.as_u64() {
        Some(frames) => Some(format!("graph declared {frames} samples{both}")),
        None => Some("graph declared: not reported by this backend".to_string()),
    }
}

/// Render #363's `disagree_declared_latency`: the graph described the path
/// differently across the two lifecycles.
///
/// The readings print *even though they match*, and they print second. A
/// reader seeing a refusal wants to know what was thrown away, and two
/// identical readings beneath a moved declaration is the fastest statement of
/// why agreement was not enough.
fn render_tau_declared_latency_leg(
    data: &serde_json::Value,
    sample_rate: Option<u64>,
) -> Vec<String> {
    let mut lines = vec![format!(
        "  {:<8}not measured (graph latency moved between lifetimes \u{2014} not stored)",
        "Delay:"
    )];
    if let (Some(d1), Some(d2)) = (
        data.get("tau_reading1_declared_frames")
            .and_then(|v| v.as_u64()),
        data.get("tau_reading2_declared_frames")
            .and_then(|v| v.as_u64()),
    ) {
        let delta = d2 as i64 - d1 as i64;
        lines.push(format!(
            "          graph declared {d1} samples \u{2192} {d2} samples  \
             (\u{394} {delta} samples)"
        ));
    }
    if let (Some(sr), Some(r1), Some(r2)) = (
        sample_rate,
        data.get("tau_reading1_s").and_then(|v| v.as_f64()),
        data.get("tau_reading2_s").and_then(|v| v.as_f64()),
    ) {
        lines.push(format!(
            "          readings {:.3} samples \u{2192} {:.3} samples",
            r1 * sr as f64,
            r2 * sr as f64
        ));
    }
    if let Some(separation_s) = data
        .get("tau_reading_separation_s")
        .and_then(|v| v.as_f64())
    {
        lines.push(format!(
            "          {}",
            lifetimes_clause(Some(2), Some(separation_s), false)
        ));
    }
    lines
}

/// Render the #369 `refused_xrun` τ state: an xrun crossed one or both
/// lifecycles that measured this run's τ, so nothing was stored regardless
/// of whether the two readings would otherwise have agreed (dispatch is
/// xrun-first in the daemon). States which lifecycle and how many, never
/// that τ itself is wrong — the instrument cannot know that, only that a
/// dropout happened during the reading that was supposed to measure it.
fn render_tau_xrun_leg(data: &serde_json::Value) -> Vec<String> {
    let r1 = data
        .get("tau_reading1_xruns")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let r2 = data
        .get("tau_reading2_xruns")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let mut dirty = Vec::new();
    if r1 > 0 {
        dirty.push("reading 1");
    }
    if r2 > 0 {
        dirty.push("reading 2");
    }
    let which = dirty.join(", ");
    let mut lines = vec![format!(
        "  {:<8}not measured (xrun during {which} — not stored)",
        "Delay:"
    )];

    if r1 > 0 {
        lines.push(format!(
            "  !! reading 1: {r1} xrun(s) during that lifecycle"
        ));
    }
    if r2 > 0 {
        lines.push(format!(
            "  !! reading 2: {r2} xrun(s) during that lifecycle"
        ));
    }
    lines
}

pub fn run_show(client: &mut AcClient) {
    let ack = check_ack(
        client.send_cmd(&serde_json::json!({"cmd": "list_calibrations"}), None),
        "list_calibrations",
    );
    let cals = ack
        .get("calibrations")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let cal_path = ac_core::shared::calibration::default_cal_path();
    if cals.is_empty() {
        println!("\n  No calibrations stored  ({})\n", cal_path.display());
        return;
    }

    println!("\n  Stored calibrations  ({})\n", cal_path.display());
    for c in &cals {
        let key = c.get("key").and_then(|v| v.as_str()).unwrap_or("?");
        println!("  [{key}]");
        match c.get("vrms_at_0dbfs_out").and_then(|v| v.as_f64()) {
            Some(v) => {
                let dbu = ac_core::shared::conversions::vrms_to_dbu(v);
                println!(
                    "    Output: 0 dBFS = {:>14}  =  {dbu:+.2} dBu",
                    ac_core::shared::conversions::fmt_vrms(v)
                );
            }
            None => println!("    Output: not calibrated"),
        }
        match c.get("vrms_at_0dbfs_in").and_then(|v| v.as_f64()) {
            Some(v) => {
                let dbu = ac_core::shared::conversions::vrms_to_dbu(v);
                println!(
                    "    Input:  0 dBFS = {:>14}  =  {dbu:+.2} dBu",
                    ac_core::shared::conversions::fmt_vrms(v)
                );
            }
            None => println!("    Input:  not calibrated"),
        }
        print_tau_history_leg(c);
        println!();
    }
}

/// Render the `Delay:` leg of a stored `list_calibrations` entry (#297) —
/// third leg alongside Output/Input, symmetric with `print_cal_leg`'s
/// "found vs not calibrated" shape. Unlike `print_tau_leg` (the live
/// `cal_done` render), this reads `tau_history` — a possibly-multi-entry
/// array with no active session to imply which entry is current — so it
/// picks the newest by `measured_at` for the primary row, states the
/// conditions and ports that entry was measured under (this command has no
/// live session to imply them from context), and names any older entries
/// rather than hiding them. Split from the pure [`render_tau_history_leg`]
/// so the line content is testable without capturing stdout.
fn print_tau_history_leg(c: &serde_json::Value) {
    for line in render_tau_history_leg(c) {
        println!("{line}");
    }
}

/// Pure line-rendering core of [`print_tau_history_leg`]. Returns each
/// output line (indentation included) so the mapping from JSON to text is
/// unit-testable.
fn render_tau_history_leg(c: &serde_json::Value) -> Vec<String> {
    let history = c
        .get("tau_history")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let newest = history.iter().max_by(|a, b| {
        let a_ts = a.get("measured_at").and_then(|v| v.as_str()).unwrap_or("");
        let b_ts = b.get("measured_at").and_then(|v| v.as_str()).unwrap_or("");
        a_ts.cmp(b_ts)
    });

    let Some(entry) = newest else {
        return vec!["    Delay:  not measured".to_string()];
    };

    let mut lines = Vec::new();

    let tau_s = entry.get("tau_s").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let measured_at = entry
        .get("measured_at")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let age = ac_core::shared::time::age_from_iso8601(measured_at);
    // #347: an entry from before this landed has no `agreement_count` and
    // deserializes to 0 — it must not read the same as a corroborated one.
    let agreement_count = entry
        .get("agreement_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    // #363: `corroborated ×N` is gone — a conclusion the 97-run evidence
    // disproves, and the count is structurally invariant so it carried no
    // information. What replaces it is the evidence: how many lifecycles, how
    // far apart, and what the graph declared. The timestamp moves onto its
    // own line, which also fixes a line that was 84 columns.
    let sample_rate = entry
        .get("conditions")
        .and_then(|c| c.get("sample_rate"))
        .and_then(|v| v.as_u64());
    lines.push(format!(
        "    Delay:  {:.4} ms   {}",
        tau_s * 1000.0,
        samples_clause(tau_s, sample_rate)
    ));
    lines.push(format!("            measured {measured_at}, {age}"));
    lines.push(format!(
        "            {}",
        lifetimes_clause(
            Some(agreement_count),
            entry.get("reading_separation_s").and_then(|v| v.as_f64()),
            true,
        )
    ));
    // On disk `None` cannot tell a pre-#363 entry from a non-declaring
    // backend, so the text asserts neither.
    let declared = match entry
        .get("declared_latency_frames")
        .and_then(|v| v.as_u64())
    {
        Some(frames) => format!("graph declared {frames} samples"),
        None => "graph declared: not recorded".to_string(),
    };
    lines.push(format!("            {declared}"));

    if let Some(cond) = entry.get("conditions") {
        let device = cond.get("device").and_then(|v| v.as_u64()).unwrap_or(0);
        let backend = cond.get("backend").and_then(|v| v.as_str()).unwrap_or("?");
        let sample_rate = cond
            .get("sample_rate")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let period_size = cond.get("period_size").and_then(|v| v.as_u64());
        let period = period_size
            .map(|p| p.to_string())
            .unwrap_or_else(|| "n/a".to_string());
        lines.push(format!(
            "            {backend}, dev {device}, {sample_rate} Hz, period {period}"
        ));

        let out_port = cond
            .get("output_port")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let in_port = cond
            .get("input_port")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        lines.push(format!("            {out_port} \u{2192} {in_port}"));
    }

    let more = history.len() - 1;
    if more > 0 {
        lines.push(format!(
            "            +{more} more \u{3c4} entries in history — see cal.json"
        ));
    }

    lines
}

/// `ac calibrate spl [input N] [output N]` — pistonphone-reference SPL.
/// Sends `calibrate_spl`, prompts the user to seat the calibrator, and
/// passes the keystroke through as `cal_reply` so the daemon's worker
/// proceeds to the audio capture step. The captured dBFS shows up in the
/// `cal_done` frame and is what later `dbfs → dB SPL` conversions use.
pub fn run_spl(cmd: &CommandKind, client: &mut AcClient) {
    let (out_ch, in_ch) = match cmd {
        CommandKind::CalibrateSpl {
            output_channel,
            input_channel,
        } => (output_channel, input_channel),
        _ => unreachable!(),
    };

    let mut cmd_json = serde_json::json!({"cmd": "calibrate_spl"});
    if let Some(ch) = out_ch {
        cmd_json["output_channel"] = (*ch).into();
    }
    if let Some(ch) = in_ch {
        cmd_json["input_channel"] = (*ch).into();
    }

    check_ack(client.send_cmd(&cmd_json, Some(5000)), "calibrate_spl");
    println!("  SPL calibration started.");
    println!("  Press Ctrl+C or type q to cancel.\n");

    loop {
        let frame = match client.recv_data(300_000) {
            Some(f) => f,
            None => {
                eprintln!("  error: SPL calibration timed out");
                return;
            }
        };
        let (topic, data) = frame;

        if topic == "cal_prompt" {
            let text = data.get("text").and_then(|v| v.as_str()).unwrap_or("");
            println!("\n  {text}");
            print!("  Press Enter to capture (q to cancel): ");
            io::stdout().flush().ok();
            let raw = read_line();
            if raw.trim().eq_ignore_ascii_case("q") {
                println!("  Calibration cancelled.");
                client.send_cmd(&serde_json::json!({"cmd": "stop"}), None);
                return;
            }
            // Any non-cancel reply releases the worker. The daemon ignores
            // the value for SPL prompts (it just needs a sync point).
            client.send_cmd(
                &serde_json::json!({"cmd": "cal_reply", "vrms": serde_json::Value::Null}),
                None,
            );
        } else if topic == "cal_done" {
            let key = data.get("key").and_then(|v| v.as_str()).unwrap_or("?");
            let dbfs = data
                .get("mic_sensitivity_dbfs_at_94db_spl")
                .and_then(|v| v.as_f64());
            println!("\n  SPL calibration saved: [{key}]");
            if let Some(d) = dbfs {
                let offset = ac_core::shared::calibration::PISTONPHONE_REF_SPL - d;
                println!("  Mic sensitivity: {d:.2} dBFS @ 94 dB SPL");
                println!("  Offset:          dB SPL = dBFS + {offset:+.2}");
            }
            if let Some(err) = data.get("error").and_then(|v| v.as_str()) {
                println!("  Note: {err}");
            }
            println!();
            return;
        } else if topic == "error" {
            let msg = data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("error");
            eprintln!("  error: {msg}");
            return;
        }
    }
}

/// `ac calibrate mic-curve <path|clear> [input N] [output N]` — parse the
/// .frd / .txt file CLI-side (so bad files fail before the daemon round
/// trip) and upload validated arrays to the daemon. `clear` drops any
/// stored curve.
pub fn run_mic_curve(cmd: &CommandKind, client: &mut AcClient) {
    let (path, out_ch, in_ch) = match cmd {
        CommandKind::CalibrateMicCurve {
            path,
            output_channel,
            input_channel,
        } => (path.clone(), output_channel, input_channel),
        _ => unreachable!(),
    };

    let mut cmd_json = match path {
        None => serde_json::json!({"cmd": "calibrate_mic_curve", "op": "clear"}),
        Some(ref p) => {
            let text = match std::fs::read_to_string(p) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("  error: cannot read {p}: {e}");
                    std::process::exit(1);
                }
            };
            let curve = match ac_core::shared::calibration::parse_mic_curve(&text, Some(p.clone()))
            {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("  error: parsing {p}: {e}");
                    std::process::exit(1);
                }
            };
            serde_json::json!({
                "cmd":         "calibrate_mic_curve",
                "op":          "set",
                "freqs_hz":    curve.freqs_hz,
                "gain_db":     curve.gain_db,
                "source_path": p,
            })
        }
    };
    if let Some(ch) = out_ch {
        cmd_json["output_channel"] = (*ch).into();
    }
    if let Some(ch) = in_ch {
        cmd_json["input_channel"] = (*ch).into();
    }

    let ack = check_ack(
        client.send_cmd(&cmd_json, Some(5000)),
        "calibrate_mic_curve",
    );
    let key = ack.get("key").and_then(|v| v.as_str()).unwrap_or("?");
    let n = ack.get("loaded").and_then(|v| v.as_u64()).unwrap_or(0);
    if n == 0 {
        println!("  Mic curve cleared on [{key}].");
    } else {
        println!("  Mic curve loaded on [{key}]: {n} points.");
    }
}

fn read_line() -> String {
    let mut line = String::new();
    io::stdin().read_line(&mut line).ok();
    line
}

fn parse_vrms(raw: &str) -> Option<f64> {
    let s = raw.to_lowercase().replace(' ', "");
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_suffix("mv") {
        return rest.parse::<f64>().ok().map(|v| v / 1000.0);
    }
    if let Some(rest) = s.strip_suffix('m') {
        return rest.parse::<f64>().ok().map(|v| v / 1000.0);
    }
    if let Some(rest) = s.strip_suffix('v') {
        return rest.parse::<f64>().ok();
    }
    s.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #279: Enter with no DMM reading offered must resolve to `Skip`,
    /// never to something that erases the stored value.
    #[test]
    fn enter_keeps_the_stored_value_when_no_reading_is_offered() {
        assert_eq!(classify_entry("", None), Entry::Reply(Reply::Skip));
        assert_eq!(classify_entry("   ", None), Entry::Reply(Reply::Skip));
        assert_eq!(classify_entry("skip", None), Entry::Reply(Reply::Skip));
        assert_eq!(classify_entry("SKIP", None), Entry::Reply(Reply::Skip));
    }

    #[test]
    fn enter_accepts_the_offered_reading_but_skip_still_skips() {
        assert_eq!(
            classify_entry("", Some(0.245)),
            Entry::Reply(Reply::Value(0.245))
        );
        assert_eq!(
            classify_entry("skip", Some(0.245)),
            Entry::Reply(Reply::Skip)
        );
    }

    #[test]
    fn only_the_clear_word_erases() {
        assert_eq!(classify_entry("clear", None), Entry::Reply(Reply::Clear));
        assert_eq!(
            classify_entry("Clear", Some(0.245)),
            Entry::Reply(Reply::Clear)
        );
    }

    #[test]
    fn q_cancels_from_either_branch() {
        assert_eq!(classify_entry("q", None), Entry::Cancel);
        assert_eq!(classify_entry("Q", Some(0.245)), Entry::Cancel);
    }

    #[test]
    fn unparseable_input_reprompts_and_sends_nothing() {
        assert_eq!(classify_entry("banana", None), Entry::Unparsed);
        assert_eq!(classify_entry("banana", Some(0.245)), Entry::Unparsed);
    }

    /// The wire encoding is the other half of #279: a skip must not carry
    /// `clear`, or the daemon reads "I did not measure this" as "erase it".
    #[test]
    fn skip_and_clear_encode_to_distinct_wire_frames() {
        let skip = Reply::Skip.to_cmd();
        assert_eq!(skip["vrms"], serde_json::Value::Null);
        assert!(
            skip.get("clear").is_none(),
            "a skip must not carry `clear`: {skip}"
        );

        let clear = Reply::Clear.to_cmd();
        assert_eq!(clear["clear"], serde_json::json!(true));

        let value = Reply::Value(0.245).to_cmd();
        assert_eq!(value["vrms"], serde_json::json!(0.245));
        assert!(value.get("clear").is_none());
    }

    // ─── run_show τ (tau_history) rendering — issue #297 ────────────────

    #[test]
    fn render_tau_history_leg_absent_prints_not_measured() {
        let entry = serde_json::json!({"key": "out0_in0", "tau_history": []});
        let lines = render_tau_history_leg(&entry);
        assert_eq!(lines, vec!["    Delay:  not measured".to_string()]);
    }

    #[test]
    fn render_tau_history_leg_missing_field_prints_not_measured() {
        // Older daemon without the #297 field: `tau_history` absent
        // entirely must render the same as an explicit `[]`, not panic.
        let entry = serde_json::json!({"key": "out0_in0"});
        let lines = render_tau_history_leg(&entry);
        assert_eq!(lines, vec!["    Delay:  not measured".to_string()]);
    }

    #[test]
    fn render_tau_history_leg_single_entry_shows_value_conditions_and_ports() {
        let entry = serde_json::json!({
            "key": "out1_in2",
            "tau_history": [
                {
                    "conditions": {
                        "device": 0,
                        "backend": "jack",
                        "sample_rate": 48000,
                        "period_size": 128,
                        "output_port": "system:playback_3",
                        "input_port": "system:capture_1"
                    },
                    "tau_s": 0.0011931,
                    "measured_at": "2020-01-01T00:00:00Z",
                    "method": "farina_short_ess"
                }
            ]
        });
        let lines = render_tau_history_leg(&entry);
        // #363 split the value line: the value alone, then the evidence,
        // then the conditions and ports that were always there.
        assert_eq!(lines.len(), 6, "got {lines:?}");
        assert_eq!(lines[0], "    Delay:  1.1931 ms   57 samples".to_string());
        assert!(
            lines[1].starts_with("            measured 2020-01-01T00:00:00Z, "),
            "got {:?}",
            lines[1]
        );
        assert_eq!(
            lines[2],
            "            1 reading, nothing compared".to_string()
        );
        assert_eq!(
            lines[3],
            "            graph declared: not recorded".to_string()
        );
        assert_eq!(
            lines[4],
            "            jack, dev 0, 48000 Hz, period 128".to_string()
        );
        assert_eq!(
            lines[5],
            "            system:playback_3 \u{2192} system:capture_1".to_string()
        );
    }

    #[test]
    fn render_tau_history_leg_picks_newest_and_counts_the_rest() {
        let entry = serde_json::json!({
            "key": "out0_in0",
            "tau_history": [
                {
                    "conditions": {
                        "device": 0, "backend": "jack", "sample_rate": 48000,
                        "period_size": 1024, "output_port": "a", "input_port": "b"
                    },
                    "tau_s": 0.001, "measured_at": "2020-01-01T00:00:00Z",
                    "method": "farina_short_ess"
                },
                {
                    "conditions": {
                        "device": 0, "backend": "jack", "sample_rate": 48000,
                        "period_size": 256, "output_port": "a", "input_port": "b"
                    },
                    "tau_s": 0.002, "measured_at": "2024-06-15T12:00:00Z",
                    "method": "farina_short_ess"
                }
            ]
        });
        let lines = render_tau_history_leg(&entry);
        // Newest (2024) entry's value must be the one shown, not the older.
        assert!(lines[0].contains("2.0000 ms"), "got {:?}", lines[0]);
        // #363 moved the timestamp onto its own line, so the identity check
        // for "which entry was picked" moves with it.
        assert!(
            lines[1].contains("2024-06-15T12:00:00Z"),
            "got {:?}",
            lines[1]
        );
        assert_eq!(lines[4], "            jack, dev 0, 48000 Hz, period 256");
        let last = lines.last().unwrap();
        assert!(
            last.contains("+1 more") && last.contains("cal.json"),
            "got {last:?}"
        );
    }

    // ─── run_show τ evidence rendering — issues #347, #363 ──────────────

    /// #363: the stored entry states its *evidence* — how many lifecycles,
    /// how far apart, what the graph declared — never the verdict
    /// `corroborated`. 42 of 97 rig runs printed that verdict over a τ one
    /// period short, so the word claims more than the instrument can reach.
    #[test]
    fn render_tau_history_leg_states_the_evidence_not_a_verdict() {
        let entry = serde_json::json!({
            "key": "out0_in0",
            "tau_history": [
                {
                    "conditions": {
                        "device": 0, "backend": "jack", "sample_rate": 48000,
                        "period_size": 128, "output_port": "a", "input_port": "b"
                    },
                    "tau_s": 0.0011931,
                    "measured_at": "2024-06-15T12:00:00Z",
                    "method": "farina_short_ess",
                    "agreement_count": 2,
                    "declared_latency_frames": 244,
                    "reading_separation_s": 1.204
                }
            ]
        });
        let lines = render_tau_history_leg(&entry);
        assert!(
            lines[0].starts_with("    Delay:  1.1931 ms   57 samples"),
            "value line carries ms and samples, nothing else: {:?}",
            lines[0]
        );
        // The age is computed against the real clock, so only its shape can
        // be pinned — a literal here would rot on a calendar boundary.
        assert!(
            lines[1].starts_with("            measured 2024-06-15T12:00:00Z, ")
                && lines[1].ends_with(" ago"),
            "got {:?}",
            lines[1]
        );
        assert_eq!(
            lines[2],
            "            2 lifetimes 1.204 s apart, identical to the sample"
        );
        assert_eq!(lines[3], "            graph declared 244 samples");
        assert!(
            !lines.iter().any(|l| l.contains("corroborated")),
            "the verdict must not survive anywhere: {lines:?}"
        );
    }

    /// The separation is the number #363 is about, so an entry written
    /// before it existed must say so rather than look like a fresh one.
    #[test]
    fn render_tau_history_leg_names_a_missing_separation_as_not_recorded() {
        let entry = serde_json::json!({
            "key": "out0_in0",
            "tau_history": [
                {
                    "conditions": {
                        "device": 0, "backend": "jack", "sample_rate": 48000,
                        "period_size": 128, "output_port": "a", "input_port": "b"
                    },
                    "tau_s": 0.0011931,
                    "measured_at": "2026-08-30T11:02:44Z",
                    "method": "farina_short_ess_v2",
                    "agreement_count": 2
                }
            ]
        });
        let lines = render_tau_history_leg(&entry);
        assert_eq!(
            lines[2],
            "            2 lifetimes, identical to the sample, separation not recorded"
        );
        assert_eq!(lines[3], "            graph declared: not recorded");
    }

    /// #347's requirement, preserved through #363's rewording: a pre-#347
    /// entry must never read like a two-lifecycle one. The distinction is
    /// now carried by what is *missing*, named as missing, rather than by an
    /// adjective the instrument cannot support.
    #[test]
    fn render_tau_history_leg_missing_agreement_count_reads_as_one_reading() {
        let entry = serde_json::json!({
            "key": "out0_in0",
            "tau_history": [
                {
                    "conditions": {
                        "device": 0, "backend": "jack", "sample_rate": 48000,
                        "period_size": 128, "output_port": "a", "input_port": "b"
                    },
                    "tau_s": 0.0011931,
                    "measured_at": "2020-01-01T00:00:00Z",
                    "method": "farina_short_ess"
                }
            ]
        });
        let lines = render_tau_history_leg(&entry);
        assert_eq!(lines[2], "            1 reading, nothing compared");
        assert!(
            !lines.iter().any(|l| l.contains("uncorroborated")),
            "got {lines:?}"
        );
    }

    // ─── live cal_done τ leg rendering — issue #388 QA gap ──────────────

    #[test]
    fn render_tau_leg_measured_prints_no_xrun_text() {
        // Clean-path regression the #369 spec's own last AC asks for: zero
        // xruns on both lifecycles must not leak xrun-shaped text into the
        // ordinary "measured" render, even though the fields are present.
        let data = serde_json::json!({
            "tau_state": "measured",
            "tau_s": 0.001,
            "tau_sample_rate": 48_000,
            "tau_agreement_count": 2,
            "tau_reading1_xruns": 0,
            "tau_reading2_xruns": 0,
        });
        let lines = render_tau_leg(&data);
        assert!(lines.iter().all(|l| !l.contains("xrun")), "got {lines:?}");
    }

    #[test]
    fn render_tau_xrun_leg_names_only_the_dirty_reading() {
        let data = serde_json::json!({
            "tau_state": "refused_xrun",
            "tau_reading1_xruns": 0,
            "tau_reading2_xruns": 1,
        });
        let lines = render_tau_xrun_leg(&data);
        assert_eq!(
            lines,
            vec![
                "  Delay:  not measured (xrun during reading 2 — not stored)".to_string(),
                "  !! reading 2: 1 xrun(s) during that lifecycle".to_string(),
            ]
        );
    }

    #[test]
    fn render_tau_xrun_leg_both_dirty_lists_both_lines_not_summed() {
        let data = serde_json::json!({
            "tau_state": "refused_xrun",
            "tau_reading1_xruns": 2,
            "tau_reading2_xruns": 1,
        });
        let lines = render_tau_xrun_leg(&data);
        assert_eq!(lines.len(), 3, "got {lines:?}");
        assert!(lines[1].contains("reading 1: 2 xrun"));
        assert!(lines[2].contains("reading 2: 1 xrun"));
    }

    #[test]
    fn render_tau_leg_dispatches_refused_xrun_through_render_tau_leg() {
        // render_tau_leg's own dispatch, not just render_tau_xrun_leg in
        // isolation — catches a wrong match arm the unit test above can't.
        let data = serde_json::json!({
            "tau_state": "refused_xrun",
            "tau_reading1_xruns": 1,
            "tau_reading2_xruns": 0,
        });
        let lines = render_tau_leg(&data);
        assert_eq!(lines, render_tau_xrun_leg(&data));
    }

    /// #494 UX, edge only: the issue's d = 4820, peak pinned at the last
    /// sample, SNR clears the gate.
    #[test]
    fn render_tau_leg_window_edge_only_matches_ux() {
        let data = serde_json::json!({
            "tau_state": "not_measured_window_edge",
            "tau_sample_rate": 96_000,
            "tau_period_size": 1024,
            "tau_refused_reading": 1,
            "tau_peak_offset_samples": 4799,
            "tau_window_first_offset_samples": -4800,
            "tau_window_last_offset_samples": 4799,
            "tau_edge_margin_samples": 480,
            "tau_pre_impulse_snr_db": 26.41,
            "tau_snr_threshold_db": 24.0,
            "tau_snr_below_threshold": false,
        });
        assert_eq!(
            render_tau_leg(&data),
            vec![
                "  Delay:  not measured (peak at window edge)",
                "          reading 1 of 2 refused, nothing stored",
                "          peak      +4799 samples   +49.9896 ms",
                "          window    -4800 to +4799 samples (±50 ms), edge margin 480 samples",
                "          peak SNR  26.41 dB pre-impulse, threshold 24.00 dB",
                "          check: interface buffer size, loopback routing,",
                "                 delay devices in the loopback path",
            ]
        );
    }

    /// #494 UX, both observations: the issue's d = 4885 skirt peak. The
    /// headline follows the daemon's flag, not a comparison made here.
    #[test]
    fn render_tau_leg_window_edge_and_low_snr_matches_ux() {
        let data = serde_json::json!({
            "tau_state": "not_measured_window_edge",
            "tau_sample_rate": 96_000,
            "tau_period_size": 1024,
            "tau_refused_reading": 1,
            "tau_peak_offset_samples": 4616,
            "tau_window_first_offset_samples": -4800,
            "tau_window_last_offset_samples": 4799,
            "tau_edge_margin_samples": 480,
            "tau_pre_impulse_snr_db": 21.34,
            "tau_snr_threshold_db": 24.0,
            "tau_snr_below_threshold": true,
        });
        assert_eq!(
            render_tau_leg(&data),
            vec![
                "  Delay:  not measured (peak at window edge, peak SNR below threshold)",
                "          reading 1 of 2 refused, nothing stored",
                "          peak      +4616 samples   +48.0833 ms",
                "          window    -4800 to +4799 samples (±50 ms), edge margin 480 samples",
                "          peak SNR  21.34 dB pre-impulse, threshold 24.00 dB",
                "          check: interface buffer size, loopback routing,",
                "                 delay devices in the loopback path,",
                "                 loopback cable, output level, input gain",
            ]
        );
    }

    /// #494 UX, low SNR only: no peak position, the reading number, and the
    /// constant threshold — no `threshold derived`, which was false here.
    #[test]
    fn render_tau_leg_low_snr_matches_ux() {
        let data = serde_json::json!({
            "tau_state": "not_measured_low_snr",
            "tau_sample_rate": 96_000,
            "tau_refused_reading": 2,
            "tau_pre_impulse_snr_db": 17.28,
            "tau_snr_threshold_db": 24.0,
        });
        assert_eq!(
            render_tau_leg(&data),
            vec![
                "  Delay:  not measured (peak SNR below threshold)",
                "          reading 2 of 2 refused, nothing stored",
                "          peak SNR  17.28 dB pre-impulse, threshold 24.00 dB",
                "          check: loopback cable, output level, input gain",
            ]
        );
    }

    /// #494: after an xrun the SNR pair is absent, so no SNR line, and the
    /// check list is the edge one alone. A daemon between #368 and #494
    /// sends no reading number; the line says only what is known.
    #[test]
    fn render_tau_leg_window_edge_without_snr_or_reading() {
        let data = serde_json::json!({
            "tau_state": "not_measured_window_edge",
            "tau_sample_rate": 48_000,
            "tau_peak_offset_samples": 2399,
            "tau_window_first_offset_samples": -2400,
            "tau_window_last_offset_samples": 2399,
            "tau_edge_margin_samples": 240,
        });
        let lines = render_tau_leg(&data);
        assert_eq!(lines[0], "  Delay:  not measured (peak at window edge)");
        assert_eq!(lines[1], "          nothing stored");
        assert!(lines.iter().all(|l| !l.contains("SNR")), "{lines:?}");
        assert!(lines.iter().all(|l| !l.contains("input gain")), "{lines:?}");
    }

    /// #494: the required keys missing falls back to the raw state rather
    /// than inventing numbers.
    #[test]
    fn render_tau_leg_refusals_without_fields_print_the_raw_state() {
        for state in ["not_measured_window_edge", "not_measured_low_snr"] {
            let data = serde_json::json!({ "tau_state": state });
            assert_eq!(
                render_tau_leg(&data),
                vec![format!("  Delay:  not measured (state: {state})")]
            );
        }
    }

    #[test]
    fn render_tau_history_leg_missing_period_size_shows_na() {
        let entry = serde_json::json!({
            "key": "out0_in0",
            "tau_history": [
                {
                    "conditions": {
                        "device": 0, "backend": "cpal", "sample_rate": 44100,
                        "period_size": null, "output_port": "a", "input_port": "b"
                    },
                    "tau_s": 0.0005, "measured_at": "2020-01-01T00:00:00Z",
                    "method": "farina_short_ess"
                }
            ]
        });
        let lines = render_tau_history_leg(&entry);
        assert_eq!(lines[4], "            cpal, dev 0, 44100 Hz, period n/a");
    }

    /// #363 UX tabulated every line at 80 columns or narrower. Two lines
    /// overflowed before the split — the stored entry at 84 and the
    /// period-shift evidence at 88 — so the claim needs a test rather than a
    /// table: worst realistic content, every state that renders τ evidence.
    #[test]
    fn tau_render_lines_fit_eighty_columns() {
        let mut rendered: Vec<String> = Vec::new();

        let live_measured = serde_json::json!({
            "tau_state": "measured",
            "tau_s": 0.017_822_9,
            "tau_sample_rate": 192_000,
            "tau_period_size": 2048,
            "tau_agreement_count": 2,
            "tau_reading_separation_s": 12.345,
            "tau_reading1_declared_frames": 12288,
            "tau_reading2_declared_frames": 12288,
            "tau_pre_impulse_snr_db": 41.2,
            "tau_snr_threshold_db": 24.0,
        });
        rendered.extend(render_tau_leg(&live_measured));

        // The same run on a backend that declares nothing — the longer of
        // the two declaration clauses.
        let mut undeclared = live_measured.clone();
        undeclared["tau_reading1_declared_frames"] = serde_json::Value::Null;
        undeclared["tau_reading2_declared_frames"] = serde_json::Value::Null;
        rendered.extend(render_tau_leg(&undeclared));

        rendered.extend(render_tau_leg(&serde_json::json!({
            "tau_state": "disagree_declared_latency",
            "tau_sample_rate": 192_000,
            "tau_period_size": 2048,
            "tau_reading1_s": 0.064,
            "tau_reading2_s": 0.064,
            "tau_reading1_declared_frames": 12288,
            "tau_reading2_declared_frames": 1268,
            "tau_reading_separation_s": 12.345,
        })));

        rendered.extend(render_tau_leg(&serde_json::json!({
            "tau_state": "disagree_period_shift",
            "tau_sample_rate": 96_000,
            "tau_period_size": 1024,
            "tau_reading1_s": 0.033_083_3,
            "tau_reading2_s": 0.043_750_0,
            "tau_delta_samples": 1024,
            "tau_periods": 1,
            "tau_reading_separation_s": 1.187,
            "tau_reading1_declared_frames": 1268,
            "tau_reading2_declared_frames": 1268,
        })));

        // #494: the widest window line UX tabulated — five-digit offsets at
        // 384 kHz — under the combined headline.
        rendered.extend(render_tau_leg(&serde_json::json!({
            "tau_state": "not_measured_window_edge",
            "tau_sample_rate": 384_000,
            "tau_period_size": 4096,
            "tau_refused_reading": 2,
            "tau_peak_offset_samples": -19199,
            "tau_window_first_offset_samples": -19200,
            "tau_window_last_offset_samples": 19199,
            "tau_edge_margin_samples": 1920,
            "tau_pre_impulse_snr_db": -103.45,
            "tau_snr_threshold_db": 24.0,
            "tau_snr_below_threshold": true,
        })));
        rendered.extend(render_tau_leg(&serde_json::json!({
            "tau_state": "not_measured_low_snr",
            "tau_refused_reading": 2,
            "tau_pre_impulse_snr_db": -103.45,
            "tau_snr_threshold_db": 24.0,
        })));

        rendered.extend(render_tau_history_leg(&serde_json::json!({
            "key": "out1_in4",
            "tau_history": [{
                "conditions": {
                    "device": 12, "backend": "coreaudio", "sample_rate": 192_000,
                    "period_size": 2048,
                    "output_port": "system:playback_2", "input_port": "system:capture_4"
                },
                "tau_s": 0.017_822_9,
                "measured_at": "2026-09-14T22:31:08Z",
                "method": "farina_short_ess_v2",
                "agreement_count": 2,
                "declared_latency_frames": 12288,
                "reading_separation_s": 12.345
            }]
        })));

        for line in rendered {
            assert!(
                line.chars().count() <= 80,
                "line runs to {} columns: {line:?}",
                line.chars().count()
            );
        }
    }
}
