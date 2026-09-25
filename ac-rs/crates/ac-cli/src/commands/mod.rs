pub mod calibrate;
pub mod devices;
pub mod dmm;
pub mod generate;
pub mod gpio;
pub mod monitor;
pub mod monitor_tui;
pub mod plot;
pub mod probe;
pub mod report;
pub mod report_verify;
pub mod server;
pub mod session;
pub mod setup;
pub mod stop;
pub mod sweep;
pub mod test;
pub mod transfer;

use crate::client::AcClient;
use crate::parse::{CommandKind, LevelSpec, ParsedCommand};

/// Spawn the `ac-view` window (M4d-CLI #185) and wait for it — `ac
/// monitor` (default) and `ac transfer` both launch through here. Host
/// comes from config's `server_host` (localhost when unset); ports are
/// the daemon defaults. `--transfer` selects the transfer view.
///
/// **No drive is ever passed.** The `ac-view` arg surface has no drive
/// option, so a CLI launch cannot bring a session up driving — drive only
/// starts through the in-app arm→fire machine. `meas_override` maps an
/// explicit CLI channel spec onto the measurement leg ("as today").
pub fn spawn_ac_view(cfg: &ac_core::config::Config, transfer: bool, meas_override: Option<u32>) {
    let Some(bin) = crate::spawn::find_binary("ac-view") else {
        eprintln!("  error: ac-view binary not found — build it with `cargo build -p ac-view`");
        return;
    };
    let host = cfg.server_host.as_deref().unwrap_or("localhost");
    let args = ac_view_args(host, transfer, meas_override);
    if let Err(e) = std::process::Command::new(bin).args(&args).status() {
        eprintln!("  error: failed to launch ac-view: {e}");
    }
}

/// Build the `ac-view` argv (pure, testable). Carries host, ports, the
/// view flag, and an optional meas override — and, load-bearingly, **no
/// drive option**: there is no path here to pass a drive/on argument, so
/// the CLI-launch AC (#185, "`ac transfer` never sets launch-time drive")
/// holds by construction of this arg list, not by the UI's separate proof.
fn ac_view_args(host: &str, transfer: bool, meas_override: Option<u32>) -> Vec<String> {
    let mut args = vec![host.to_string(), "5556".to_string(), "5557".to_string()];
    if transfer {
        args.push("--transfer".to_string());
    }
    if let Some(m) = meas_override {
        args.push("--meas".to_string());
        args.push(m.to_string());
    }
    args
}

pub fn dispatch(parsed: ParsedCommand, cfg: &ac_core::config::Config, client: &mut AcClient) {
    let show = parsed.show_plot;
    match parsed.cmd {
        CommandKind::Devices => devices::run(client),
        CommandKind::Setup { .. } => setup::run(&parsed.cmd, cfg, client),
        CommandKind::Stop => stop::run(client),
        CommandKind::DmmShow => dmm::run(client),
        CommandKind::ServerEnable => server::enable(client),
        CommandKind::ServerDisable => server::disable(client),
        CommandKind::ServerConnections => server::connections(client),
        CommandKind::Gpio { log } => gpio::run(client, log),

        CommandKind::GenerateSine { .. } => generate::run_sine(&parsed.cmd, client),
        CommandKind::GeneratePink { .. } => generate::run_pink(&parsed.cmd, client),

        CommandKind::Calibrate { .. } => calibrate::run(&parsed.cmd, client),
        CommandKind::CalibrateShow => calibrate::run_show(client),
        CommandKind::CalibrateCheck => calibrate::run_check(client),
        CommandKind::CalibrateSpl { .. } => calibrate::run_spl(&parsed.cmd, client),
        CommandKind::CalibrateMicCurve { .. } => calibrate::run_mic_curve(&parsed.cmd, client),

        CommandKind::SweepLevel { .. } => sweep::run_level(&parsed.cmd, client),
        CommandKind::SweepFrequency { .. } => sweep::run_frequency(&parsed.cmd, cfg, client),

        CommandKind::Plot { .. } => plot::run(&parsed.cmd, cfg, client, show),
        CommandKind::PlotLevel { .. } => plot::run_level(&parsed.cmd, cfg, client, show),
        CommandKind::PlotIr { .. } => plot::run_ir(&parsed.cmd, client),

        CommandKind::Monitor { .. } => monitor::run(&parsed.cmd, cfg),
        CommandKind::Transfer { .. } => transfer::run(&parsed.cmd, cfg),
        CommandKind::MonitorCwt { .. } => monitor::run_cwt(&parsed.cmd, cfg, client),
        CommandKind::MonitorCqt { .. } => monitor::run_cqt(&parsed.cmd, cfg, client),
        CommandKind::MonitorReassigned { .. } => monitor::run_reassigned(&parsed.cmd, cfg, client),

        CommandKind::Probe => probe::run(client),
        CommandKind::TestSoftware => test::run_software(client),
        CommandKind::TestHardware { .. } => test::run_hardware(&parsed.cmd, client),
        CommandKind::TestDut { .. } => test::run_dut(&parsed.cmd, cfg, client),

        // Handled before dispatch in main.rs
        CommandKind::ServerSetHost { .. }
        | CommandKind::SessionNew { .. }
        | CommandKind::SessionList
        | CommandKind::SessionUse { .. }
        | CommandKind::SessionRm { .. }
        | CommandKind::SessionDiff { .. }
        | CommandKind::Report { .. }
        | CommandKind::ReportVerify { .. } => unreachable!(),
    }
}

pub fn check_ack(ack: Option<serde_json::Value>, context: &str) -> serde_json::Value {
    match ack {
        None => {
            eprintln!(
                "  error: no response from server{}",
                if context.is_empty() {
                    String::new()
                } else {
                    format!(" ({context})")
                }
            );
            std::process::exit(1);
        }
        Some(v) => {
            if v.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                let err = v
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("unknown error");
                eprintln!("  error: {err}");
                std::process::exit(1);
            }
            // A successful reply may still carry advisories — a config whose
            // meaning changed under it, say (#225). Printed generically here
            // rather than per command, so a handler that adds one does not
            // also have to remember to display it.
            if let Some(ws) = v.get("warnings").and_then(|w| w.as_array()) {
                for w in ws.iter().filter_map(|w| w.as_str()) {
                    eprintln!("  warning: {w}");
                }
            }
            v
        }
    }
}

pub fn level_to_dbfs(level: &LevelSpec, cal: Option<&serde_json::Value>) -> f64 {
    let (unit, ref_vrms) = match level {
        LevelSpec::Dbfs(v) => return *v,
        LevelSpec::Dbu(_) => ("dBu", voltage_scale(cal)),
        LevelSpec::Vrms(_) => ("Vrms", voltage_scale(cal)),
    };
    let ref_vrms = match ref_vrms {
        Some(Scale::Usable(v, _)) => v,
        // #466: a stored scale a session check refused is never used to
        // decide what is emitted.
        Some(Scale::Withheld(verdict)) => {
            for line in refusal_error_lines(unit, &verdict, None) {
                eprintln!("{line}");
            }
            std::process::exit(1);
        }
        None => {
            eprintln!("  error: {unit} level requires output calibration (run: ac calibrate)");
            std::process::exit(1);
        }
    };
    match level {
        LevelSpec::Dbu(dbu) => {
            let target_vrms =
                ac_core::shared::constants::DBU_REF_EXACT * 10.0_f64.powf(*dbu / 20.0);
            20.0 * (target_vrms / ref_vrms).log10()
        }
        LevelSpec::Vrms(vrms) => 20.0 * (vrms / ref_vrms).log10(),
        LevelSpec::Dbfs(v) => *v,
    }
}

/// The `level_unit` a request carries for a typed level (#466): the daemon
/// refuses a physical level whose stored scale its session check refuses.
pub fn level_unit(level: &LevelSpec) -> &'static str {
    match level {
        LevelSpec::Dbfs(_) => "dbfs",
        LevelSpec::Dbu(_) => "dbu",
        LevelSpec::Vrms(_) => "vrms",
    }
}

// ---------------------------------------------------------------------------
// Session check (#466): verdicts, the scale they gate, and their rendering.
// The CLI never computes a verdict or a propagation; it renders the daemon's.
// ---------------------------------------------------------------------------

use ac_core::shared::calibration::session::{
    split_check, unverified_head, whole_seconds, DeltaBound, Evidence, UnverifiedCause,
};
use ac_core::shared::calibration::{LayerVerdict, SESSION_REFUSALS_FILE};

/// Continuation indent of the session-check block (label column 14 + 2).
pub const BLOCK_INDENT: &str = "                ";

/// Wrap width every session-check line fits in.
const WIDTH: usize = 80;

/// A verdict from the wire. `None` when the field is absent or null; a
/// verdict this client cannot read is unverified, never verified.
pub fn verdict_from(v: Option<&serde_json::Value>) -> Option<LayerVerdict> {
    let v = v.filter(|v| !v.is_null())?;
    Some(
        serde_json::from_value::<LayerVerdict>(v.clone()).unwrap_or_else(|_| {
            LayerVerdict::unverified(
                UnverifiedCause::Unknown,
                format!(
                    "unreadable verdict: {}",
                    v.get("state").and_then(|s| s.as_str()).unwrap_or("?")
                ),
            )
        }),
    )
}

/// The `get_calibration` voltage verdict. An older daemon that sends no
/// `session_check` reads as unverified.
pub fn cal_voltage_verdict(cal: &serde_json::Value) -> LayerVerdict {
    match cal.get("session_check") {
        None | Some(serde_json::Value::Null) => LayerVerdict::unverified(
            UnverifiedCause::Unknown,
            "daemon did not report a session check",
        ),
        Some(block) => verdict_from(block.get("voltage")).unwrap_or_else(|| {
            LayerVerdict::unverified(
                UnverifiedCause::Unknown,
                "daemon did not report a session check",
            )
        }),
    }
}

/// The stored output voltage scale, and whether it may be used.
#[derive(Debug, Clone, PartialEq)]
pub enum Scale {
    /// `vrms_at_0dbfs_out`, applied; the verdict is verified or unverified.
    Usable(f64, LayerVerdict),
    /// Refused by a session check: no dBu figure uses it.
    Withheld(LayerVerdict),
}

/// The output voltage scale of a `get_calibration` reply. `None` when no
/// scale is stored.
pub fn voltage_scale(cal: Option<&serde_json::Value>) -> Option<Scale> {
    let cal = cal?;
    let v = cal.get("vrms_at_0dbfs_out").and_then(|v| v.as_f64())?;
    let verdict = cal_voltage_verdict(cal);
    Some(if verdict.is_refused() {
        Scale::Withheld(verdict)
    } else {
        Scale::Usable(v, verdict)
    })
}

/// Put a fresh check's verdict for the command pair into a
/// `get_calibration` reply, so every later figure uses it. Only when the
/// frame's pair is this reply's pair.
pub fn apply_fresh_verdict(cal: &mut Option<serde_json::Value>, frame: &serde_json::Value) {
    let Some(pair) = frame.get("pair") else {
        return;
    };
    let Some(c) = cal.as_mut() else {
        return;
    };
    if pair.get("key") != c.get("key") {
        return;
    }
    if let Some(v) = pair.get("voltage").filter(|v| !v.is_null()) {
        if !c.get("session_check").is_some_and(|s| s.is_object()) {
            c["session_check"] = serde_json::json!({});
        }
        c["session_check"]["voltage"] = v.clone();
    }
}

/// Words of `text` on lines of at most `WIDTH` columns: the first after
/// `first`, the rest after `indent`.
fn wrap_words(first: &str, text: &str, indent: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = first.to_string();
    let mut empty = true;
    for word in text.split_whitespace() {
        let len = current.chars().count() + usize::from(!empty) + word.chars().count();
        if len > WIDTH && !empty {
            lines.push(std::mem::replace(&mut current, indent.to_string()));
            empty = true;
        }
        if !empty {
            current.push(' ');
        }
        current.push_str(word);
        empty = false;
    }
    if !empty || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// `entries` joined by `, `, wrapped between entries, never inside one.
fn wrap_entries(first: &str, entries: &[String], indent: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = first.to_string();
    let mut empty = first.trim().is_empty();
    for (i, e) in entries.iter().enumerate() {
        let item = if i + 1 < entries.len() {
            format!("{e},")
        } else {
            e.clone()
        };
        let len = current.chars().count() + usize::from(!empty) + item.chars().count();
        if len > WIDTH && !empty {
            lines.push(std::mem::replace(&mut current, indent.to_string()));
            empty = true;
        }
        if !empty {
            current.push(' ');
        }
        current.push_str(&item);
        empty = false;
    }
    lines.push(current);
    lines
}

/// `  <label padded to 14>` — the block's label column.
fn label14(label: &str) -> String {
    format!("  {label:<14}")
}

fn samples(x: f64) -> String {
    format!("{x:.0}")
}

/// The Δ clause of a voltage refusal or verification.
fn gain_delta(e: &Evidence, bound: Option<DeltaBound>) -> String {
    match bound {
        Some(DeltaBound::AtMost) => format!(
            "loop gain \u{394} \u{2264} {:+.2} dB, no tone returned",
            e.delta
        ),
        None => format!(
            "loop gain \u{394} {:+.2} dB, tolerance \u{b1}{:.2} dB",
            e.delta, e.tolerance
        ),
    }
}

/// `via [key], whose … moved …` for a propagated refusal.
fn via_clause(via: &str, e: &Evidence, bound: Option<DeltaBound>) -> String {
    let le = if bound.is_some() { "\u{2264} " } else { "" };
    match e.unit {
        ac_core::shared::calibration::session::VerdictUnit::Db => {
            format!("via [{via}], whose loop gain moved {le}{:+.2} dB", e.delta)
        }
        ac_core::shared::calibration::session::VerdictUnit::Samples => format!(
            "via [{via}], whose \u{3c4} moved {le}{:+.0} samples",
            e.delta
        ),
    }
}

/// `this capture` or `measured`: where a τ reading came from.
fn tau_source(e: &Evidence) -> &'static str {
    match e.source {
        ac_core::shared::calibration::session::CheckSource::SameCapture => "this capture",
        _ => "measured",
    }
}

/// The `check:` lines under a verdict, wrapped at the block indent.
fn check_lines(places: &str) -> Vec<String> {
    wrap_words(BLOCK_INDENT, &format!("check: {places}"), BLOCK_INDENT)
}

/// What the block needs to know about the command it sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// An emitting command: only consumed layers, `not applied` lines.
    Consumer,
    /// `ac calibrate check`: both layers, the loopback's own verdicts.
    Explicit,
}

/// The voltage row of the block.
fn voltage_rows(
    v: &LayerVerdict,
    rec: &serde_json::Value,
    pair_key: &str,
    kind: BlockKind,
) -> Vec<String> {
    let label = label14("voltage");
    let drive = rec
        .get("stimulus")
        .and_then(|s| s.get("level_dbfs"))
        .and_then(|v| v.as_f64())
        .unwrap_or(ac_core::shared::emission_level::DEFAULT_LEVEL_DBFS);
    let reach: Vec<String> = rec
        .get("reach")
        .and_then(|r| r.get("voltage"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|k| k.as_str())
                .filter(|k| *k != pair_key)
                .map(|k| format!("[{k}]"))
                .collect()
        })
        .unwrap_or_default();
    let mut lines = Vec::new();
    match v {
        LayerVerdict::Verified(e) => {
            lines.push(format!("{label}verified \u{2014} {}", gain_delta(e, None)));
            lines.push(format!(
                "{BLOCK_INDENT}loop gain {:+.2} dB now, {:+.2} dB at calibration",
                e.measured, e.stored
            ));
        }
        LayerVerdict::Refused {
            evidence: e,
            via,
            delta_bound,
        } => {
            match via {
                Some(via) => lines.push(format!(
                    "{label}REFUSED \u{2014} {}",
                    via_clause(via, e, *delta_bound)
                )),
                None => {
                    lines.push(format!(
                        "{label}REFUSED \u{2014} {}",
                        gain_delta(e, *delta_bound)
                    ));
                    lines.push(match delta_bound {
                        Some(_) => format!(
                            "{BLOCK_INDENT}return {:.2} dBFS at most, {:.2} dBFS expected",
                            e.measured + drive,
                            e.stored + drive
                        ),
                        None => format!(
                            "{BLOCK_INDENT}loop gain {:+.2} dB now, {:+.2} dB at calibration",
                            e.measured, e.stored
                        ),
                    });
                }
            }
            if kind == BlockKind::Consumer {
                lines.push(format!(
                    "{BLOCK_INDENT}Output and Input 0 dBFS of [{pair_key}] not applied"
                ));
            }
            if via.is_none() && !reach.is_empty() {
                lines.extend(wrap_entries(
                    &format!("{BLOCK_INDENT}also refused:"),
                    &reach,
                    BLOCK_INDENT,
                ));
            }
            match (via, delta_bound) {
                (Some(_), _) => lines.extend(check_lines(
                    "`ac calibrate check`; if still refused, re-run `ac calibrate` for this pair",
                )),
                (None, Some(_)) => {
                    lines.push(format!(
                        "{BLOCK_INDENT}check: interface stream routing, loopback cable;"
                    ));
                    lines.push(format!(
                        "{BLOCK_INDENT}after restoring them, `ac calibrate check`"
                    ));
                }
                (None, None) => {
                    lines.push(format!(
                        "{BLOCK_INDENT}check: interface output and input level settings;"
                    ));
                    lines.push(format!(
                        "{BLOCK_INDENT}after restoring them, `ac calibrate check`;"
                    ));
                    lines.push(format!(
                        "{BLOCK_INDENT}to store the new gain, `ac calibrate`"
                    ));
                }
            }
        }
        LayerVerdict::Unverified { cause, reason } => {
            lines.extend(unverified_rows(&label, *cause, reason));
        }
    }
    lines
}

/// An `UNVERIFIED` row: head, observation (for `not_measured`), `check:`.
fn unverified_rows(label: &str, cause: UnverifiedCause, reason: &str) -> Vec<String> {
    let (observation, places) = split_check(reason);
    let mut lines = wrap_words(
        label,
        &format!("UNVERIFIED \u{2014} {}", unverified_head(cause, reason)),
        BLOCK_INDENT,
    );
    // Only `not_measured` prints its observation under the head; the block
    // omits an unreadable record's parse error (`calibrate show` prints it).
    if cause == UnverifiedCause::NotMeasured {
        lines.extend(wrap_words(BLOCK_INDENT, observation, BLOCK_INDENT));
    }
    if let Some(places) = places {
        lines.extend(check_lines(places));
    }
    lines
}

/// The latency row of the block.
fn latency_rows(
    v: &LayerVerdict,
    rec: &serde_json::Value,
    loopback: &str,
    kind: BlockKind,
) -> Vec<String> {
    let label = label14("latency");
    let mut lines = Vec::new();
    match v {
        LayerVerdict::Verified(e) => {
            lines.push(format!(
                "{label}verified \u{2014} {} samples {}, {} stored",
                samples(e.measured),
                tau_source(e),
                samples(e.stored)
            ));
            if let Some(sep) = rec.get("latency_separation_s").and_then(|v| v.as_f64()) {
                lines.push(format!(
                    "{BLOCK_INDENT}2 lifetimes {sep:.3} s apart, identical to the sample"
                ));
            }
        }
        LayerVerdict::Refused {
            evidence: e, via, ..
        } => {
            match via {
                Some(via) => lines.push(format!(
                    "{label}REFUSED \u{2014} {}",
                    via_clause(via, e, None)
                )),
                None => lines.push(format!(
                    "{label}REFUSED \u{2014} {} samples {}, {} stored (\u{394} {:+.0})",
                    samples(e.measured),
                    tau_source(e),
                    samples(e.stored),
                    e.delta
                )),
            }
            // #544 (architect rev. 2): `plot_ir`'s flight time subtracts this
            // capture's own reference latency, never the stored τ this
            // verdict judges — a property of the command, not of the
            // evidence source (a voltage layer makes the frame `probe`).
            let flight_time_consumer = kind == BlockKind::Consumer
                && rec.get("cmd").and_then(|v| v.as_str()) == Some("plot_ir");
            if flight_time_consumer {
                lines.push(format!(
                    "{BLOCK_INDENT}not applied \u{2014} flight time uses this capture's ref latency"
                ));
            } else if kind == BlockKind::Consumer {
                lines.push(format!(
                    "{BLOCK_INDENT}stored \u{3c4} for [{loopback}] not applied"
                ));
            }
            let reach: Vec<String> = rec
                .get("reach")
                .and_then(|r| r.get("latency"))
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .map(|r| {
                            let key = r.get("key").and_then(|v| v.as_str()).unwrap_or("?");
                            let sr = r.get("sample_rate").and_then(|v| v.as_u64()).unwrap_or(0);
                            match r.get("period_size").and_then(|v| v.as_u64()) {
                                Some(p) => {
                                    format!("stored \u{3c4} of [{key}] at {sr} Hz, period {p}")
                                }
                                None => format!("stored \u{3c4} of [{key}] at {sr} Hz"),
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            if via.is_none() && !reach.is_empty() {
                lines.extend(wrap_entries(
                    &format!("{BLOCK_INDENT}also refused:"),
                    &reach,
                    BLOCK_INDENT,
                ));
            }
            // #544 UX: no instruction to re-calibrate above a flight time that
            // does not use the stored τ.
            if !flight_time_consumer {
                lines.push(format!(
                    "{BLOCK_INDENT}check: re-run `ac calibrate` with loopback patched"
                ));
            }
        }
        LayerVerdict::Unverified { cause, reason } => {
            lines.extend(unverified_rows(&label, *cause, reason));
        }
    }
    lines
}

/// The `NOT SAVED` head for a persist error kind.
fn not_saved_head(kind: Option<&str>) -> &'static str {
    match kind {
        Some("unreadable") => "session_refusals.json unreadable",
        Some("write_failed") => "writing session_refusals.json failed",
        _ => "session_refusals.json not written",
    }
}

/// The `check:` places for a persist error kind.
fn not_saved_places(kind: Option<&str>) -> Option<&'static str> {
    match kind {
        Some("unreadable") => Some("its permissions and contents, beside cal.json"),
        Some("write_failed") => Some("free space and write permission beside cal.json"),
        _ => None,
    }
}

/// The `record` row, when a refusal was not written.
fn record_rows(rec: &serde_json::Value) -> Vec<String> {
    if rec.get("persisted").and_then(|v| v.as_bool()) != Some(false) {
        return Vec::new();
    }
    let err = rec.get("persist_error");
    let kind = err.and_then(|e| e.get("kind")).and_then(|v| v.as_str());
    let mut lines = vec![format!(
        "{}NOT SAVED \u{2014} {}",
        label14("record"),
        not_saved_head(kind)
    )];
    if kind == Some("write_failed") {
        if let Some(detail) = err.and_then(|e| e.get("detail")).and_then(|v| v.as_str()) {
            lines.extend(wrap_words(BLOCK_INDENT, detail, BLOCK_INDENT));
        }
    }
    lines.push(format!(
        "{BLOCK_INDENT}held in daemon memory until a write succeeds;"
    ));
    lines.push(format!(
        "{BLOCK_INDENT}a daemon exit before then drops the refusal"
    ));
    if let Some(places) = not_saved_places(kind) {
        lines.extend(check_lines(places));
    }
    lines
}

/// The session-check block for one `session_check` frame (#466 UX).
/// `Consumer` renders the command pair's consumed layers (`pair` in the
/// frame); `Explicit` renders both of the loopback's layers.
pub fn session_block_lines(frame: &serde_json::Value, kind: BlockKind) -> Vec<String> {
    let ran_at = frame.get("ran_at").and_then(|v| v.as_str()).unwrap_or("?");
    let lb = frame.get("loopback");
    let lb_key = lb
        .and_then(|l| l.get("key"))
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let lb_out = lb
        .and_then(|l| l.get("output_port"))
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let lb_in = lb
        .and_then(|l| l.get("input_port"))
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let mut lines = vec![
        format!("  session check {}", whole_seconds(ran_at)),
        format!("{BLOCK_INDENT}loopback [{lb_key}] {lb_out} \u{2192} {lb_in}"),
    ];
    match frame.get("stimulus").filter(|s| !s.is_null()) {
        Some(s) => lines.push(format!(
            "{BLOCK_INDENT}stimulus {:.1} dBFS, {:.0} Hz, {:.2} s",
            s.get("level_dbfs")
                .and_then(|v| v.as_f64())
                .unwrap_or(f64::NAN),
            s.get("freq_hz")
                .and_then(|v| v.as_f64())
                .unwrap_or(f64::NAN),
            frame
                .get("duration_s")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
        )),
        None if frame.get("source").and_then(|v| v.as_str()) == Some("same_capture") => lines.push(
            format!("{BLOCK_INDENT}stimulus none \u{2014} latency read from this capture"),
        ),
        None => lines.push(format!("{BLOCK_INDENT}stimulus none")),
    }
    let pair = frame.get("pair").filter(|p| !p.is_null());
    let pair_key = pair
        .and_then(|p| p.get("key"))
        .and_then(|v| v.as_str())
        .unwrap_or(lb_key);
    let latency = verdict_from(frame.get("latency"));
    let voltage = match (kind, pair) {
        (BlockKind::Consumer, Some(p)) => verdict_from(p.get("voltage")),
        _ => verdict_from(frame.get("voltage")),
    };
    if let Some(v) = &latency {
        lines.extend(latency_rows(v, frame, lb_key, kind));
    }
    if let Some(v) = &voltage {
        lines.extend(voltage_rows(v, frame, pair_key, kind));
    }
    lines.extend(record_rows(frame));
    lines
}

/// `session check not run — <reason>` and its `check:` line.
pub fn not_run_lines(reason: &str) -> Vec<String> {
    let (observation, places) = split_check(reason);
    let mut lines = wrap_words(
        "  ",
        &format!("session check not run \u{2014} {observation}"),
        BLOCK_INDENT,
    );
    if let Some(places) = places {
        lines.extend(check_lines(places));
    }
    lines
}

/// The command-level refusal (#466 UX): a physical level whose stored scale
/// a session check refused. Nothing was emitted. `record` is the refusing
/// record, for its `NOT SAVED` warning.
pub fn refusal_error_lines(
    unit: &str,
    verdict: &LayerVerdict,
    record: Option<&serde_json::Value>,
) -> Vec<String> {
    const IND: &str = "         ";
    let mut lines = vec![format!(
        "  error: {unit} level refused \u{2014} stored output calibration is stale"
    )];
    if let LayerVerdict::Refused {
        evidence: e,
        via,
        delta_bound,
    } = verdict
    {
        let when = whole_seconds(&e.checked_at);
        match (via, delta_bound) {
            (Some(via), _) => {
                lines.push(format!("{IND}session check {when}:"));
                lines.push(format!("{IND}{};", via_clause(via, e, *delta_bound)));
                lines.push(format!("{IND}nothing was emitted"));
                lines.push(format!(
                    "{IND}check: `ac calibrate check`; if still refused,"
                ));
                lines.push(format!("{IND}re-run `ac calibrate` for this pair"));
            }
            (None, Some(_)) => {
                lines.push(format!(
                    "{IND}session check {when}: loop gain \u{394} \u{2264} {:+.2} dB,",
                    e.delta
                ));
                lines.push(format!("{IND}no tone returned; nothing was emitted"));
                lines.push(format!(
                    "{IND}check: interface stream routing, loopback cable;"
                ));
                lines.push(format!("{IND}after restoring them, `ac calibrate check`"));
            }
            (None, None) => {
                lines.push(format!(
                    "{IND}session check {when}: loop gain \u{394} {:+.2} dB,",
                    e.delta
                ));
                lines.push(format!(
                    "{IND}tolerance \u{b1}{:.2} dB; nothing was emitted",
                    e.tolerance
                ));
                lines.push(format!(
                    "{IND}check: interface level settings; after restoring them,"
                ));
                lines.push(format!(
                    "{IND}`ac calibrate check`; to store the new gain, `ac calibrate`"
                ));
            }
        }
    } else {
        lines.push(format!("{IND}nothing was emitted"));
    }
    if let Some(rec) =
        record.filter(|r| r.get("persisted").and_then(|v| v.as_bool()) == Some(false))
    {
        const WIND: &str = "           ";
        let err = rec.get("persist_error");
        let kind = err.and_then(|e| e.get("kind")).and_then(|v| v.as_str());
        lines.push(format!(
            "  warning: refusal not saved \u{2014} {}",
            not_saved_head(kind)
        ));
        if kind == Some("write_failed") {
            if let Some(detail) = err.and_then(|e| e.get("detail")).and_then(|v| v.as_str()) {
                lines.extend(wrap_words(WIND, detail, WIND));
            }
        }
        lines.push(format!(
            "{WIND}a daemon exit before a write succeeds drops the refusal"
        ));
        if let Some(places) = not_saved_places(kind) {
            lines.extend(wrap_words(WIND, &format!("check: {places}"), WIND));
        }
    }
    lines
}

/// How a command's wait for its `session_check` frame ended.
#[derive(Debug)]
pub enum SessionWait {
    /// The frame arrived.
    Frame(serde_json::Value),
    /// The daemon said the check does not run, with its reason.
    NotRun(Option<String>),
    /// The daemon did not report a check (an older daemon).
    NotReported,
    /// The command ended first: `(topic, frame)` of its `error` or `done`.
    Ended(String, serde_json::Value),
}

/// Wait for the command's `session_check` frame when its ack says one is
/// pending. The daemon publishes it before any measurement frame.
pub fn await_session_check(
    client: &mut AcClient,
    cmd_name: &str,
    ack: &serde_json::Value,
    timeout_ms: i64,
) -> SessionWait {
    match ack.get("session_check").and_then(|v| v.as_str()) {
        Some("pending") => {}
        Some("not_run") => {
            return SessionWait::NotRun(
                ack.get("session_check_reason")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            )
        }
        _ => return SessionWait::NotReported,
    }
    loop {
        let Some((topic, frame)) = client.recv_data(timeout_ms) else {
            return SessionWait::Ended(
                "error".to_string(),
                serde_json::json!({"message": "timeout waiting for the session check"}),
            );
        };
        let frame_cmd = frame.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
        if !(frame_cmd.is_empty() || frame_cmd == cmd_name) {
            continue;
        }
        match topic.as_str() {
            "session_check" => return SessionWait::Frame(frame),
            "error" | "done" => return SessionWait::Ended(topic, frame),
            _ => {}
        }
    }
}

/// Print the block (or the `not run` line) for a consumer and fold the
/// fresh verdict into `cal`. Returns `Err(())` after printing a command-level
/// refusal (exit status 1 is the caller's), or when the command ended with an
/// error before the check reported.
pub fn print_consumer_check(
    wait: SessionWait,
    cal: &mut Option<serde_json::Value>,
    level: Option<&LevelSpec>,
    consumes: bool,
) -> Result<(), ()> {
    match wait {
        SessionWait::Frame(frame) => {
            apply_fresh_verdict(cal, &frame);
            let physical = level.is_some_and(|l| !matches!(l, LevelSpec::Dbfs(_)));
            let refused = frame
                .get("pair")
                .and_then(|p| verdict_from(p.get("voltage")))
                .filter(LayerVerdict::is_refused);
            if let (true, Some(v)) = (physical, refused) {
                let unit = match level {
                    Some(LevelSpec::Vrms(_)) => "Vrms",
                    _ => "dBu",
                };
                for line in refusal_error_lines(unit, &v, Some(&frame)) {
                    eprintln!("{line}");
                }
                return Err(());
            }
            println!();
            for line in session_block_lines(&frame, BlockKind::Consumer) {
                println!("{line}");
            }
            Ok(())
        }
        SessionWait::NotRun(reason) => {
            if consumes {
                if let Some(reason) = reason {
                    println!();
                    for line in not_run_lines(&reason) {
                        println!("{line}");
                    }
                }
            }
            Ok(())
        }
        SessionWait::NotReported => Ok(()),
        SessionWait::Ended(topic, frame) => {
            if topic == "error" {
                // A command-level refusal carries the refusing verdict.
                if let Some(v) =
                    verdict_from(frame.get("voltage_check")).filter(LayerVerdict::is_refused)
                {
                    let unit = frame
                        .get("level_unit")
                        .and_then(|v| v.as_str())
                        .unwrap_or("dBu");
                    let rec = frame
                        .get("session_check")
                        .or(frame.get("session_check_record"));
                    for line in refusal_error_lines(unit, &v, rec) {
                        eprintln!("{line}");
                    }
                } else {
                    let msg = frame
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("error");
                    eprintln!("  error: {msg}");
                }
            }
            Err(())
        }
    }
}

/// Whether a command consumes a stored voltage layer: a stored scale, or a
/// level typed in dBu/Vrms.
pub fn consumes_voltage(cal: Option<&serde_json::Value>, level: Option<&LevelSpec>) -> bool {
    voltage_scale(cal).is_some() || level.is_some_and(|l| !matches!(l, LevelSpec::Dbfs(_)))
}

/// The level block's `voltage` row (#466 UX), when the scale is not
/// verified. `see session check above` points at the block for causes the
/// block explains.
fn voltage_level_row(scale: &Scale) -> Option<String> {
    const LABEL: &str = "  voltage    ";
    match scale {
        Scale::Withheld(_) => Some(format!(
            "{LABEL}REFUSED \u{2014} dBu not shown (session check above)"
        )),
        Scale::Usable(_, LayerVerdict::Unverified { cause, reason }) => Some(format!(
            "{LABEL}UNVERIFIED \u{2014} {}",
            verdict_inline(*cause, reason)
        )),
        Scale::Usable(..) => None,
    }
}

/// An unverified cause as one short clause, for consumer lines.
pub fn verdict_inline(cause: UnverifiedCause, reason: &str) -> String {
    match cause {
        UnverifiedCause::NotMeasured
        | UnverifiedCause::NoBaseline
        | UnverifiedCause::BaselineDriveDiffers => "see session check above".to_string(),
        UnverifiedCause::RefusalsUnreadable => format!("{SESSION_REFUSALS_FILE} unreadable"),
        _ => split_check(reason).0.to_string(),
    }
}

#[derive(Clone, Copy)]
pub enum LevelOrigin {
    Default,
    Typed,
    Fixed,
}

impl LevelOrigin {
    fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Typed => "typed",
            Self::Fixed => "fixed",
        }
    }
}

/// dBu at `dbfs`, only through a scale no session check refused (#466).
fn dbfs_to_dbu(dbfs: f64, cal: Option<&serde_json::Value>) -> Option<f64> {
    let Scale::Usable(vrms_0dbfs, _) = voltage_scale(cal)? else {
        return None;
    };
    Some(ac_core::shared::conversions::vrms_to_dbu(
        vrms_0dbfs * 10.0_f64.powf(dbfs / 20.0),
    ))
}

fn render_level_block(
    level_dbfs: Option<f64>,
    origin: LevelOrigin,
    max_dbfs: Option<f64>,
    cal: Option<&serde_json::Value>,
    show_dbu: bool,
) -> Vec<String> {
    let level = match level_dbfs {
        Some(v) => {
            let analog = show_dbu
                .then(|| dbfs_to_dbu(v, cal))
                .flatten()
                .map(|dbu| format!("  =  {dbu:+7.2} dBu"))
                .unwrap_or_default();
            format!("  level      {v:>6.1} dBFS{analog}  ({})", origin.label())
        }
        None => "  level      (not reported by this daemon)".to_string(),
    };
    let maximum = match max_dbfs {
        Some(v) => {
            let analog = show_dbu
                .then(|| dbfs_to_dbu(v, cal))
                .flatten()
                .map(|dbu| format!("  =  {dbu:+7.2} dBu"))
                .unwrap_or_default();
            let marker = if v == 0.0 { "  (full scale)" } else { "" };
            format!("  maximum    {v:>6.1} dBFS{analog}{marker}")
        }
        None => "  maximum    (not reported by this daemon)".to_string(),
    };
    let mut lines = vec![level, maximum];
    if show_dbu {
        lines.extend(voltage_scale(cal).as_ref().and_then(voltage_level_row));
    }
    lines
}

fn render_level_range_block(
    start_dbfs: Option<f64>,
    stop_dbfs: Option<f64>,
    origin: LevelOrigin,
    max_dbfs: Option<f64>,
    cal: Option<&serde_json::Value>,
) -> Vec<String> {
    let level = match (start_dbfs, stop_dbfs) {
        (Some(start_dbfs), Some(stop_dbfs)) => {
            let analog = match (dbfs_to_dbu(start_dbfs, cal), dbfs_to_dbu(stop_dbfs, cal)) {
                (Some(start), Some(stop)) => {
                    format!("  =  {start:+7.2} \u{2192} {stop:+.2} dBu")
                }
                _ => String::new(),
            };
            format!(
                "  level      {start_dbfs:>6.1} \u{2192} {stop_dbfs:.1} dBFS{analog}  ({})",
                origin.label()
            )
        }
        _ => "  level      (not reported by this daemon)".to_string(),
    };
    let mut lines = vec![level];
    lines.extend(
        render_level_block(None, origin, max_dbfs, cal, true)
            .into_iter()
            .skip(1),
    );
    lines
}

pub fn print_level(
    level_dbfs: Option<f64>,
    defaulted: bool,
    max_dbfs: Option<f64>,
    cal: Option<&serde_json::Value>,
    show_dbu: bool,
) {
    let origin = if defaulted {
        LevelOrigin::Default
    } else {
        LevelOrigin::Typed
    };
    for line in render_level_block(level_dbfs, origin, max_dbfs, cal, show_dbu) {
        println!("{line}");
    }
}

pub fn print_fixed_level(level_dbfs: Option<f64>, max_dbfs: Option<f64>) {
    for line in render_level_block(level_dbfs, LevelOrigin::Fixed, max_dbfs, None, false) {
        println!("{line}");
    }
}

pub fn print_level_range(
    start_dbfs: Option<f64>,
    stop_dbfs: Option<f64>,
    defaulted: bool,
    max_dbfs: Option<f64>,
    cal: Option<&serde_json::Value>,
) {
    let origin = if defaulted {
        LevelOrigin::Default
    } else {
        LevelOrigin::Typed
    };
    for line in render_level_range_block(start_dbfs, stop_dbfs, origin, max_dbfs, cal) {
        println!("{line}");
    }
}

pub fn get_cal(client: &mut AcClient) -> Option<serde_json::Value> {
    let reply = client.send_cmd(&serde_json::json!({"cmd": "get_calibration"}), None)?;
    if reply.get("found").and_then(|v| v.as_bool()) == Some(true) {
        Some(reply)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{ac_view_args, render_level_block, render_level_range_block, LevelOrigin};

    // The CLI-path drive-off AC (#185), asserted through the CLI's own arg
    // construction — not by reusing ac-view's in-app proof. `ac transfer`
    // spawns ac-view with a view flag and channels only; no argument
    // mentions drive/on, so a session launched this way is always
    // drive-off. If a `--drive` ever gets added here, this trips.
    #[test]
    fn ac_view_launch_args_carry_no_drive_option() {
        for transfer in [true, false] {
            for meas in [None, Some(3u32)] {
                let args = ac_view_args("localhost", transfer, meas);
                let joined = args.join(" ").to_lowercase();
                assert!(
                    !joined.contains("drive") && !joined.contains("--on"),
                    "ac-view launch args must carry no drive option: {args:?}"
                );
            }
        }
    }

    #[test]
    fn transfer_flag_selects_the_transfer_view() {
        assert!(ac_view_args("h", true, None).contains(&"--transfer".to_string()));
        assert!(!ac_view_args("h", false, None).contains(&"--transfer".to_string()));
    }

    #[test]
    fn meas_override_maps_the_channel() {
        let args = ac_view_args("h", true, Some(5));
        let i = args
            .iter()
            .position(|a| a == "--meas")
            .expect("--meas present");
        assert_eq!(args[i + 1], "5");
    }

    #[test]
    fn scalar_default_typed_fixed_and_missing_maximum_render() {
        assert_eq!(
            render_level_block(Some(-40.0), LevelOrigin::Default, Some(0.0), None, false),
            vec![
                "  level       -40.0 dBFS  (default)",
                "  maximum       0.0 dBFS  (full scale)"
            ]
        );
        assert!(
            render_level_block(Some(-30.0), LevelOrigin::Typed, Some(0.0), None, false)[0]
                .ends_with("(typed)")
        );
        assert!(
            render_level_block(Some(-30.0), LevelOrigin::Fixed, Some(0.0), None, false)[0]
                .ends_with("(fixed)")
        );
        assert_eq!(
            render_level_block(Some(-40.0), LevelOrigin::Default, None, None, false)[1],
            "  maximum    (not reported by this daemon)"
        );
        assert_eq!(
            render_level_block(None, LevelOrigin::Fixed, Some(0.0), None, false)[0],
            "  level      (not reported by this daemon)"
        );
    }

    #[test]
    fn range_and_calibrated_scalar_render() {
        assert_eq!(
            render_level_range_block(
                Some(-40.0),
                Some(-30.0),
                LevelOrigin::Default,
                Some(0.0),
                None,
            )[0],
            "  level       -40.0 \u{2192} -30.0 dBFS  (default)"
        );
        let cal = serde_json::json!({"vrms_at_0dbfs_out": 1.0});
        let lines =
            render_level_block(Some(-40.0), LevelOrigin::Typed, Some(0.0), Some(&cal), true);
        assert!(lines[0].contains("dBu"));
        assert!(lines[1].contains("+2.22 dBu"));
        assert!(lines[1].ends_with("(full scale)"));
    }

    #[test]
    fn nonzero_maximum_has_no_full_scale_marker_and_range_uses_ack_values() {
        let max = render_level_block(Some(-40.0), LevelOrigin::Default, Some(-20.0), None, false);
        assert!(!max[1].contains("full scale"));

        let range = render_level_range_block(
            Some(-42.0),
            Some(-31.0),
            LevelOrigin::Typed,
            Some(0.0),
            None,
        );
        assert!(range[0].contains("-42.0 \u{2192} -31.0"));
    }

    // ─── #466 session check ────────────────────────────────────────────

    use super::{
        refusal_error_lines, session_block_lines, verdict_from, voltage_scale, BlockKind, Scale,
    };
    use serde_json::{json, Value};

    fn gain(state: &str, delta: f64) -> Value {
        json!({
            "state": state, "measured": -0.60 + delta, "stored": -0.60, "delta": delta,
            "tolerance": 0.10, "unit": "dB", "stored_at": "2026-09-15T23:43:04Z",
            "checked_at": "2026-09-16T14:02:11.345Z", "source": "probe",
        })
    }

    fn tau(state: &str, measured: f64) -> Value {
        json!({
            "state": state, "measured": measured, "stored": 1711.0,
            "delta": measured - 1711.0, "tolerance": 0.5, "unit": "samples",
            "stored_at": "2026-09-15T23:43:04Z", "checked_at": "2026-09-16T14:02:11Z",
            "source": "same_capture",
        })
    }

    fn frame(voltage: Option<Value>, latency: Option<Value>) -> Value {
        let mut f = json!({
            "id": "1-1", "ran_at": "2026-09-16T14:02:11.345Z", "cmd": "plot_level",
            "source": "probe",
            "loopback": {"key": "out1_in1", "output_port": "system:playback_2",
                         "input_port": "system:capture_2"},
            "stimulus": {"level_dbfs": -40.0, "freq_hz": 1000.0},
            "duration_s": 0.45,
            "reach": {"voltage": ["out0_in0", "out3_in3"], "latency": []},
        });
        if let Some(v) = voltage {
            f["voltage"] = v.clone();
            f["pair"] = json!({"key": "out1_in1", "voltage": v, "latency": null});
        }
        if let Some(l) = latency {
            f["latency"] = l;
        }
        f
    }

    fn unsaved(mut f: Value, kind: &str, detail: &str) -> Value {
        f["persisted"] = json!(false);
        f["persist_error"] = json!({"kind": kind, "detail": detail});
        f
    }

    #[test]
    fn refused_voltage_block_matches_ux() {
        let lines = session_block_lines(
            &frame(Some(gain("refused", 3.02)), None),
            BlockKind::Consumer,
        );
        assert_eq!(
            lines,
            vec![
                "  session check 2026-09-16T14:02:11Z",
                "                loopback [out1_in1] system:playback_2 \u{2192} system:capture_2",
                "                stimulus -40.0 dBFS, 1000 Hz, 0.45 s",
                "  voltage       REFUSED \u{2014} loop gain \u{394} +3.02 dB, tolerance \u{b1}0.10 dB",
                "                loop gain +2.42 dB now, -0.60 dB at calibration",
                "                Output and Input 0 dBFS of [out1_in1] not applied",
                "                also refused: [out0_in0], [out3_in3]",
                "                check: interface output and input level settings;",
                "                after restoring them, `ac calibrate check`;",
                "                to store the new gain, `ac calibrate`",
            ]
        );
    }

    #[test]
    fn absent_tone_refusal_is_a_bound() {
        let mut v = gain("refused", -58.62);
        v["delta_bound"] = json!("at_most");
        let lines = session_block_lines(&frame(Some(v), None), BlockKind::Consumer);
        assert_eq!(
            lines[3],
            "  voltage       REFUSED \u{2014} loop gain \u{394} \u{2264} -58.62 dB, no tone returned"
        );
        assert_eq!(
            lines[4],
            "                return -99.22 dBFS at most, -40.60 dBFS expected"
        );
        assert!(lines.iter().all(|l| !l.contains("store the new gain")));
    }

    /// UX R4-1: the head names the kind from data — the rejected revision-3
    /// line said `unreadable` for every unsaved refusal.
    #[test]
    fn not_saved_row_names_the_kind() {
        let base = frame(Some(gain("refused", 3.02)), None);
        let failed = session_block_lines(
            &unsaved(
                base.clone(),
                "write_failed",
                "No space left on device (os error 28)",
            ),
            BlockKind::Consumer,
        )
        .join("\n");
        assert!(failed.contains("NOT SAVED \u{2014} writing session_refusals.json failed"));
        assert!(failed.contains("No space left on device (os error 28)"));
        assert!(!failed.contains("unreadable"), "{failed}");

        let unreadable = session_block_lines(
            &unsaved(base, "unreadable", "expected value at line 1 column 1"),
            BlockKind::Consumer,
        )
        .join("\n");
        assert!(unreadable.contains("NOT SAVED \u{2014} session_refusals.json unreadable"));
        assert!(!unreadable.contains("writing session_refusals.json failed"));
        assert!(
            !unreadable.contains("expected value"),
            "the unreadable detail is not printed in the block"
        );
    }

    /// UX R4-2: one `record` row per record, after the last layer's `check:`.
    #[test]
    fn one_not_saved_row_after_both_refused_layers() {
        let f = unsaved(
            frame(Some(gain("refused", 3.02)), Some(tau("refused", 1743.0))),
            "write_failed",
            "Read-only file system (os error 30)",
        );
        let lines = session_block_lines(&f, BlockKind::Consumer);
        let text = lines.join("\n");
        assert_eq!(text.matches("NOT SAVED").count(), 1, "{text}");
        let not_saved = lines.iter().position(|l| l.contains("NOT SAVED")).unwrap();
        let last_check = lines
            .iter()
            .rposition(|l| l.contains("check: interface"))
            .unwrap();
        assert!(not_saved > last_check, "{text}");
        assert!(lines
            .iter()
            .any(|l| l == "  latency       REFUSED \u{2014} 1743 samples this capture, 1711 stored (\u{394} +32)"));
    }

    /// #544 (architect rev. 2, UX): under `plot_ir` a refused latency is
    /// scoped — `plot_ir`'s flight time does not subtract the stored τ — and
    /// carries no re-calibrate `check:`, in the direct and the `via` form,
    /// and whether the frame's evidence is `same_capture` or `probe`. Tested
    /// against a leak: every other consumer and `ac calibrate check` keep
    /// today's `not applied` and `check:` lines.
    #[test]
    fn plot_ir_refused_latency_is_scoped_to_the_stored_tau() {
        let recalibrate = "                check: re-run `ac calibrate` with loopback patched";
        let scoped =
            "                not applied \u{2014} flight time uses this capture's ref latency";
        let stored = "                stored \u{3c4} for [out1_in1] not applied";
        let plot_ir = |mut f: Value, source: &str| {
            f["cmd"] = json!("plot_ir");
            f["source"] = json!(source);
            f
        };
        let mut via = tau("refused", 1743.0);
        via["via"] = json!("out1_in1");
        let mut direct = frame(None, Some(tau("refused", 1679.0)));
        direct["stimulus"] = Value::Null;
        direct["reach"]["latency"] =
            json!([{"key": "out0_in0", "sample_rate": 96000, "period_size": 256}]);
        for f in [
            plot_ir(direct.clone(), "same_capture"),
            plot_ir(
                frame(Some(gain("verified", 0.01)), Some(via.clone())),
                "probe",
            ),
        ] {
            let lines = session_block_lines(&f, BlockKind::Consumer);
            assert!(lines.iter().any(|l| l == scoped), "{lines:#?}");
            assert!(!lines.iter().any(|l| l == recalibrate), "{lines:#?}");
            assert!(!lines.iter().any(|l| l == stored), "{lines:#?}");
            assert!(
                lines
                    .iter()
                    .any(|l| l.starts_with("  latency       REFUSED")),
                "{lines:#?}"
            );
        }
        let lines = session_block_lines(
            &plot_ir(direct.clone(), "same_capture"),
            BlockKind::Consumer,
        );
        assert_eq!(
            lines[3..6].to_vec(),
            vec![
                "  latency       REFUSED \u{2014} 1679 samples this capture, 1711 stored (\u{394} -32)",
                scoped,
                "                also refused: stored \u{3c4} of [out0_in0] at 96000 Hz, period 256",
            ]
        );
        assert_eq!(lines.len(), 6, "{lines:#?}");

        let other = frame(None, Some(tau("refused", 1679.0)));
        let lines = session_block_lines(&other, BlockKind::Consumer);
        assert!(lines.iter().any(|l| l == stored), "{lines:#?}");
        assert!(lines.iter().any(|l| l == recalibrate), "{lines:#?}");
        assert!(!lines.iter().any(|l| l == scoped), "{lines:#?}");
        for f in [other, plot_ir(direct, "same_capture")] {
            let lines = session_block_lines(&f, BlockKind::Explicit);
            assert!(lines.iter().any(|l| l == recalibrate), "{lines:#?}");
            assert!(!lines.iter().any(|l| l == scoped), "{lines:#?}");
        }
    }

    /// UX R4-4: every new form fits 80 columns, a 70-character detail too.
    #[test]
    fn session_check_forms_fit_eighty_columns() {
        let long_detail = "x".repeat(30) + " " + &"y".repeat(39);
        assert_eq!(long_detail.len(), 70);
        let mut all: Vec<String> = Vec::new();
        let mut via = gain("refused", 3.02);
        via["via"] = json!("out1_in1");
        for f in [
            unsaved(
                frame(Some(gain("refused", 3.02)), Some(tau("refused", 1743.0))),
                "write_failed",
                &long_detail,
            ),
            frame(Some(via.clone()), Some(tau("verified", 1711.0))),
            frame(
                Some(json!({"state": "unverified", "cause": "not_measured",
                    "reason": "tone SNR 31.2 dB, need 50.8 dB; check: loopback cable, reference input gain"})),
                Some(
                    json!({"state": "unverified", "cause": "refusals_unreadable",
                    "reason": "session_refusals.json unreadable: invalid type: string \"x\", expected struct SessionCheckRecord at line 3 column 17; check: its permissions and contents, beside cal.json"}),
                ),
            ),
        ] {
            all.extend(session_block_lines(&f, BlockKind::Consumer));
            all.extend(session_block_lines(&f, BlockKind::Explicit));
        }
        let rec = unsaved(json!({}), "write_failed", &long_detail);
        all.extend(refusal_error_lines(
            "dBu",
            &verdict_from(Some(&gain("refused", 3.02))).unwrap(),
            Some(&rec),
        ));
        all.extend(refusal_error_lines(
            "Vrms",
            &verdict_from(Some(&via)).unwrap(),
            Some(&rec),
        ));
        all.extend(super::not_run_lines(
            "reference input system:capture_9 not found; check: `ac devices`, `ac setup reference`",
        ));
        for line in &all {
            assert!(
                line.chars().count() <= 80,
                "{} columns: {line:?}",
                line.chars().count()
            );
        }
        assert!(
            all.iter().any(|l| l.trim() == "y".repeat(39)),
            "the detail wraps"
        );
    }

    #[test]
    fn command_level_refusal_matches_ux() {
        let rec = unsaved(
            json!({}),
            "write_failed",
            "No space left on device (os error 28)",
        );
        let lines = refusal_error_lines(
            "dBu",
            &verdict_from(Some(&gain("refused", 3.02))).unwrap(),
            Some(&rec),
        );
        assert_eq!(
            lines,
            vec![
                "  error: dBu level refused \u{2014} stored output calibration is stale",
                "         session check 2026-09-16T14:02:11Z: loop gain \u{394} +3.02 dB,",
                "         tolerance \u{b1}0.10 dB; nothing was emitted",
                "         check: interface level settings; after restoring them,",
                "         `ac calibrate check`; to store the new gain, `ac calibrate`",
                "  warning: refusal not saved \u{2014} writing session_refusals.json failed",
                "           No space left on device (os error 28)",
                "           a daemon exit before a write succeeds drops the refusal",
                "           check: free space and write permission beside cal.json",
            ]
        );
    }

    /// A refused scale is withheld from every dBu figure; a missing or
    /// unreadable verdict is applied but never reads as verified.
    #[test]
    fn scale_follows_the_verdict() {
        let refused = json!({"vrms_at_0dbfs_out": 1.0,
            "session_check": {"voltage": gain("refused", 3.02)}});
        assert!(matches!(
            voltage_scale(Some(&refused)),
            Some(Scale::Withheld(_))
        ));
        let lines = render_level_block(
            Some(-40.0),
            LevelOrigin::Default,
            Some(0.0),
            Some(&refused),
            true,
        );
        assert!(!lines.iter().any(|l| l.contains("dBu  ")), "{lines:?}");
        assert_eq!(
            lines[2],
            "  voltage    REFUSED \u{2014} dBu not shown (session check above)"
        );

        let old_daemon = json!({"vrms_at_0dbfs_out": 1.0});
        match voltage_scale(Some(&old_daemon)) {
            Some(Scale::Usable(_, v)) => assert!(!v.is_verified()),
            other => panic!("{other:?}"),
        }
        let malformed = json!({"vrms_at_0dbfs_out": 1.0,
            "session_check": {"voltage": {"state": "verified"}}});
        match voltage_scale(Some(&malformed)) {
            Some(Scale::Usable(_, v)) => assert!(!v.is_verified(), "{v:?}"),
            other => panic!("{other:?}"),
        }

        let no_loop = json!({"vrms_at_0dbfs_out": 1.0, "session_check": {"voltage": {
            "state": "unverified", "cause": "no_loopback",
            "reason": "no reference loopback configured; check: `ac setup reference`"}}});
        let lines = render_level_block(
            Some(-40.0),
            LevelOrigin::Default,
            Some(0.0),
            Some(&no_loop),
            true,
        );
        assert_eq!(
            lines[2],
            "  voltage    UNVERIFIED \u{2014} no reference loopback configured"
        );
        let verified = json!({"vrms_at_0dbfs_out": 1.0,
            "session_check": {"voltage": gain("verified", -0.01)}});
        assert_eq!(
            render_level_block(
                Some(-40.0),
                LevelOrigin::Default,
                Some(0.0),
                Some(&verified),
                true
            )
            .len(),
            2
        );
    }
}
