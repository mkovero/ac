//! Per-session check of the stored calibration layers — issue #466.
//!
//! The rules (what a measurement says about a stored value, how long that
//! holds, how far a refusal reaches) live in
//! [`ac_core::shared::calibration::session`]. This module does the parts
//! that need a daemon: the loopback probe (its own engine lifecycle), the
//! records this process holds, `session_refusals.json` beside `cal.json`,
//! the gate every emitting command runs before it emits, and the explicit
//! `session_check` command.
//!
//! **Where the check runs** (design option C): emitting commands run a
//! fresh probe inside their worker, before their own emission, when a
//! reference loopback is configured and the command uses a stored voltage
//! scale (or a level typed in dBu/Vrms). Consumers that do not emit —
//! `monitor_spectrum`, `transfer_stream`, `get_calibration` — read the
//! latest recorded result. `plot_ir` takes τ from its same-capture
//! reference (#460) and never probes τ separately.
//!
//! **The refusal record is read on every access** and written
//! read-modify-write, the way `cal.json` is. An unreadable file is never
//! written over: a refusal that cannot be written is held in this process
//! and merged in at the next access that reads the file.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

use ac_core::config::Config;
use ac_core::shared::calibration::session::{
    self, judge_latency, judge_voltage, merge_refusals, no_stored_latency_reason, propagate,
    voltage_precheck, CheckCtx, CheckScope, CheckSource, Effective, JudgedIdentity,
    LatencyIdentity, LayerVerdict, LoopbackRef, PersistError, PersistErrorKind, ProbeReading,
    ProbeStimulus, RecordSet, SessionCheckRecord, SessionRefusals, StoredEntry, StoredTau, Target,
    ToneReading, UnverifiedCause, VoltageIdentity, PROBE_CAPTURE_S, PROBE_FREQ_HZ, PROBE_SETTLE_S,
};
use ac_core::shared::calibration::{
    cal_key, default_session_refusals_path, read_session_refusals, write_session_refusals,
    Calibration, DeviceEpoch, LoopGainBaseline, TauEntry,
};
use ac_core::shared::emission_level::DEFAULT_LEVEL_DBFS;

use crate::audio::epoch::current_epoch;
use crate::audio::{make_engine, AudioEngine};
use crate::server::ServerState;

use super::super::{
    busy_guard, cfg_guard, resolve_ref_input, resolve_ref_output, send_pub, spawn_worker,
};
use super::tau::{measure_tau_twice, tau_result, TauOutcome};

/// How many records this process keeps. Only the latest decisive record per
/// loopback and layer decides anything; the rest are history.
const MAX_PROCESS_RECORDS: usize = 64;

/// Where the loopback probe is emitted, and at what level.
pub(crate) const PROBE_DRIVE_DBFS: f64 = DEFAULT_LEVEL_DBFS;

/// This process's session-check records, plus the verdicts the running
/// `transfer_stream` session applied at its start.
#[derive(Default)]
pub struct SessionChecks {
    process: Vec<SessionCheckRecord>,
    seq: u64,
    /// Ids of records without a refusal whose verified layers are already
    /// applied to the file. They are merged once, never again (R3-3): a
    /// later refusal another daemon wrote must not be cleared by an old pass.
    applied: BTreeSet<String>,
    /// Voltage verdict per capture channel, as the current transfer session
    /// applied it at start. Read by the `snapshot` handler.
    transfer_applied: BTreeMap<u32, LayerVerdict>,
}

/// `session_refusals.json` as read now, or the observation that it is
/// unreadable.
pub(crate) type FileView = Result<SessionRefusals, String>;

impl SessionChecks {
    fn next_id(&mut self) -> String {
        self.seq += 1;
        format!("{}-{}", std::process::id(), self.seq)
    }

    fn push(&mut self, record: SessionCheckRecord) {
        self.process.retain(|r| r.id != record.id);
        self.applied.remove(&record.id);
        self.process.push(record);
        if self.process.len() > MAX_PROCESS_RECORDS {
            let excess = self.process.len() - MAX_PROCESS_RECORDS;
            self.process.drain(..excess);
            let process = &self.process;
            self.applied
                .retain(|id| process.iter().any(|r| &r.id == id));
        }
    }

    fn get(&self, id: &str) -> Option<&SessionCheckRecord> {
        self.process.iter().find(|r| r.id == id)
    }

    fn mark_pending(&mut self, error: PersistError) {
        for r in self.process.iter_mut() {
            if r.has_refusal() && r.persisted != Some(true) {
                r.set_persisted(Err(error.clone()));
            }
        }
    }

    fn mark_persisted(&mut self) {
        for r in self.process.iter_mut() {
            if r.has_refusal() {
                r.set_persisted(Ok(()));
            } else {
                self.applied.insert(r.id.clone());
            }
        }
    }

    /// The records a sync still has to merge: refusals not yet on disk and
    /// passes not yet applied.
    fn unsynced(&self) -> Vec<SessionCheckRecord> {
        self.process
            .iter()
            .filter(|r| r.persisted != Some(true) && !self.applied.contains(&r.id))
            .cloned()
            .collect()
    }

    /// Read the refusal record, merge this process's records into it, and
    /// write it back when that changed anything. Never writes over a file
    /// it could not read.
    fn sync(&mut self) -> FileView {
        let path = default_session_refusals_path();
        let file = match read_session_refusals(&path) {
            Ok(file) => file,
            Err(e) => {
                self.mark_pending(PersistError {
                    kind: PersistErrorKind::Unreadable,
                    detail: e.observation.clone(),
                });
                return Err(e.observation);
            }
        };
        let merged = merge_refusals(&file, &self.unsynced());
        if merged == file {
            self.mark_persisted();
            return Ok(file);
        }
        match write_session_refusals(&path, &merged) {
            Ok(()) => {
                self.mark_persisted();
                Ok(merged)
            }
            Err(e) => {
                self.mark_pending(PersistError {
                    kind: PersistErrorKind::WriteFailed,
                    detail: e.root_cause().to_string(),
                });
                Ok(file)
            }
        }
    }
}

/// `"ok"` or `{ "unreadable": "<observation>" }` — the `refusal_record`
/// wire field.
pub(crate) fn refusal_record_value(view: &FileView) -> Value {
    match view {
        Ok(_) => json!("ok"),
        Err(observation) => json!({ "unreadable": observation }),
    }
}

/// Read and sync the refusal record now.
pub(crate) fn file_view(state: &ServerState) -> FileView {
    state.session_checks.lock().unwrap().sync()
}

/// Store `record` in this process, try to persist it, and return it with
/// `persisted` / `persist_error` set.
fn record(state: &ServerState, mut rec: SessionCheckRecord) -> SessionCheckRecord {
    let mut checks = state.session_checks.lock().unwrap();
    rec.persisted = rec.has_refusal().then_some(false);
    checks.push(rec.clone());
    let _ = checks.sync();
    checks.get(&rec.id).cloned().unwrap_or(rec)
}

/// A new, empty record for a check that runs now.
fn new_record(
    state: &ServerState,
    cmd: &str,
    source: CheckSource,
    loopback: &Loopback,
    epoch: DeviceEpoch,
) -> SessionCheckRecord {
    let id = state.session_checks.lock().unwrap().next_id();
    SessionCheckRecord {
        id,
        ran_at: now_ms(),
        epoch,
        source,
        cmd: cmd.to_string(),
        loopback: loopback.as_ref(),
        stimulus: None,
        duration_s: 0.0,
        reach: Default::default(),
        latency: None,
        voltage: None,
        judged: JudgedIdentity::default(),
        probe: None,
        latency_separation_s: None,
        persisted: None,
        persist_error: None,
    }
}

/// RFC3339 with milliseconds: records are ordered by it, and two checks in
/// one second must still order.
fn now_ms() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Fill `reach` from every stored entry. An unreadable `cal.json` reaches
/// nothing further — the consumers refuse on it themselves.
fn fill_reach(rec: &mut SessionCheckRecord) {
    let all = Calibration::load_all(None).unwrap_or_default();
    let keys: Vec<String> = all.iter().map(Calibration::key).collect();
    let entries: Vec<StoredEntry<'_>> = all
        .iter()
        .zip(keys.iter())
        .map(|(c, key)| StoredEntry {
            key,
            has_voltage: c.has_voltage(),
            tau_conditions: c.tau_history.iter().map(|e| &e.conditions).collect(),
        })
        .collect();
    propagate(rec, &entries);
}

// ---------------------------------------------------------------------------
// The loopback
// ---------------------------------------------------------------------------

/// The configured reference loopback, resolved to ports.
#[derive(Debug, Clone)]
pub(crate) struct Loopback {
    pub(crate) out_ch: u32,
    pub(crate) in_ch: u32,
    pub(crate) output_port: String,
    pub(crate) input_port: String,
}

impl Loopback {
    pub(crate) fn key(&self) -> String {
        cal_key(self.out_ch, self.in_ch)
    }

    fn as_ref(&self) -> LoopbackRef {
        LoopbackRef {
            key: self.key(),
            output_port: self.output_port.clone(),
            input_port: self.input_port.clone(),
        }
    }
}

/// The loopback's calibration key from config alone, without resolving
/// ports. `None` when no reference input is configured.
pub(crate) fn loopback_key(cfg: &Config) -> Option<String> {
    let in_ch = cfg.reference_channel?;
    let out_ch = cfg.reference_output_channel.unwrap_or(cfg.output_channel);
    Some(cal_key(out_ch, in_ch))
}

/// The reference loopback, resolved. `Ok(None)` when none is configured;
/// `Err` names what to check when one is configured but unresolvable.
pub(crate) fn configured_loopback(
    cfg: &Config,
    state: &ServerState,
) -> Result<Option<Loopback>, String> {
    let with_check = |e: String| format!("{e}; check: `ac devices`, `ac setup reference`");
    let Some(input_port) = resolve_ref_input(cfg, state).map_err(with_check)? else {
        return Ok(None);
    };
    let output_port = resolve_ref_output(cfg, state).map_err(with_check)?;
    Ok(Some(Loopback {
        out_ch: cfg.reference_output_channel.unwrap_or(cfg.output_channel),
        in_ch: cfg
            .reference_channel
            .expect("resolve_ref_input returned a port, so reference_channel is set"),
        output_port,
        input_port,
    }))
}

/// The no-loopback reason, with the places to look.
pub(crate) const NO_LOOPBACK: &str =
    "no reference loopback configured; check: `ac setup reference`, `ac setup reference-output`";

/// The backend `fake_audio`/`required` selects, resolved without opening an
/// engine: an open would consume the fake backend's engine-open fault slots
/// (#432) that a consumer's own open is meant to hit.
fn selected_backend(fake_audio: bool, required: Option<&str>) -> Result<String, String> {
    let (required, _, selected) = crate::audio::backend_status(fake_audio, required);
    selected.ok_or_else(|| {
        format!("audio backend unavailable \u{2014} required {required}; measurement not started")
    })
}

/// The device-enumeration epoch the configured backend is in now.
fn epoch_now(state: &ServerState) -> DeviceEpoch {
    let required = state.cfg.lock().unwrap().backend.clone();
    epoch_for(state.fake_audio, required.as_deref())
}

// ---------------------------------------------------------------------------
// Recorded verdicts (consumers that do not emit)
// ---------------------------------------------------------------------------

/// What a consumer needs to decide verdicts: the records, the refusal file
/// as read now, the configured loopback, and the current epoch.
pub(crate) struct Recorded {
    process: Vec<SessionCheckRecord>,
    view: FileView,
    loopback_key: Option<String>,
    epoch: DeviceEpoch,
}

impl Recorded {
    /// Read everything once.
    pub(crate) fn now(state: &ServerState) -> Recorded {
        let loopback_key = loopback_key(&state.cfg.lock().unwrap());
        let epoch = epoch_now(state);
        let mut checks = state.session_checks.lock().unwrap();
        let view = checks.sync();
        Recorded {
            process: checks.process.clone(),
            view,
            loopback_key,
            epoch,
        }
    }

    pub(crate) fn view(&self) -> &FileView {
        &self.view
    }

    fn eval(&self, target: Target<'_>) -> Effective {
        session::effective(
            target,
            RecordSet {
                process: &self.process,
                file: self.view.as_ref().map_err(String::as_str),
            },
            CheckScope {
                loopback_key: self.loopback_key.as_deref(),
                drive_dbfs: PROBE_DRIVE_DBFS,
                current_epoch: &self.epoch,
            },
        )
    }

    /// The voltage verdict for `cal`'s stored scale.
    pub(crate) fn voltage(&self, cal: &Calibration) -> Effective {
        let key = cal.key();
        self.eval(Target::Voltage {
            key: &key,
            has_voltage: cal.has_voltage(),
            baseline: cal.loop_gain_baseline.as_ref(),
        })
    }

    /// The voltage verdict, only when `cal` stores a voltage scale (the
    /// presence rule every wire `voltage_check` follows).
    pub(crate) fn voltage_check(&self, cal: Option<&Calibration>) -> Option<LayerVerdict> {
        cal.filter(|c| c.has_voltage())
            .map(|c| self.voltage(c).verdict)
    }

    /// The latency verdict for the τ entry `entry` of `cal`, or `not_stored`
    /// with `missing` as its reason.
    pub(crate) fn latency(&self, cal: &Calibration, entry: Result<&TauEntry, &str>) -> Effective {
        let key = cal.key();
        let stored;
        let target = match entry {
            Ok(e) => {
                stored = StoredTau {
                    tau_s: e.tau_s,
                    measured_at: e.measured_at.clone(),
                };
                Target::Latency {
                    key: &key,
                    stored: Ok((&stored, &e.conditions)),
                }
            }
            Err(reason) => Target::Latency {
                key: &key,
                stored: Err(reason),
            },
        };
        self.eval(target)
    }
}

/// `cal` with its voltage scale withheld when `verdict` refuses it.
pub(crate) fn gated(
    cal: Option<Calibration>,
    verdict: Option<&LayerVerdict>,
) -> Option<Calibration> {
    match (cal, verdict) {
        (Some(c), Some(v)) if v.is_refused() => Some(c.without_voltage()),
        (cal, _) => cal,
    }
}

/// The `get_calibration` / `list_calibrations` `session_check` block for
/// one entry: voltage, latency (for the entry `tau_for` resolves now, or the
/// newest one), and the records that decided them.
pub(crate) fn live_block(recorded: &Recorded, cal: &Calibration) -> Value {
    let voltage = recorded.voltage(cal);
    let entry = latest_tau_entry(recorded, cal);
    let no_tau = "no stored latency for this pair".to_string();
    let latency = recorded.latency(cal, entry.ok_or(no_tau.as_str()));
    json!({
        "voltage": voltage.verdict,
        "latency": latency.verdict,
        "recorded": {
            "voltage": voltage.record,
            "latency": latency.record,
        },
    })
}

/// The τ entry a live block judges: among entries sharing device, backend,
/// rate and period with the conditions the latest check on (or reaching)
/// this pair judged, the newest; else the newest entry.
fn latest_tau_entry<'a>(recorded: &Recorded, cal: &'a Calibration) -> Option<&'a TauEntry> {
    let key = cal.key();
    let file_records: Vec<&SessionCheckRecord> = recorded
        .view
        .as_ref()
        .map(|f| f.latency.values().collect())
        .unwrap_or_default();
    let judged = recorded
        .process
        .iter()
        .chain(file_records)
        .filter(|r| r.loopback.key == key || r.reach.latency.iter().any(|l| l.key == key))
        .filter_map(|r| r.judged.latency.as_ref().map(|j| (r.ran_at.as_str(), j)))
        .max_by(|a, b| a.0.cmp(b.0))
        .map(|(_, j)| j);
    let newest = |it: &mut dyn Iterator<Item = &'a TauEntry>| {
        it.max_by(|a, b| a.measured_at.cmp(&b.measured_at))
    };
    if let Some(j) = judged {
        let c = &j.conditions;
        let mut sharing = cal.tau_history.iter().filter(|e| {
            e.conditions.device == c.device
                && e.conditions.backend == c.backend
                && e.conditions.sample_rate == c.sample_rate
                && e.conditions.period_size == c.period_size
        });
        if let Some(e) = newest(&mut sharing) {
            return Some(e);
        }
    }
    newest(&mut cal.tau_history.iter())
}

/// Record the voltage verdicts a transfer session applied at start, for the
/// `snapshot` handler.
pub(crate) fn set_transfer_applied(state: &ServerState, applied: BTreeMap<u32, LayerVerdict>) {
    state.session_checks.lock().unwrap().transfer_applied = applied;
}

/// The voltage verdict the running transfer session applied to `input_channel`.
pub(crate) fn transfer_applied(state: &ServerState, input_channel: u32) -> Option<LayerVerdict> {
    state
        .session_checks
        .lock()
        .unwrap()
        .transfer_applied
        .get(&input_channel)
        .cloned()
}

// ---------------------------------------------------------------------------
// The probe
// ---------------------------------------------------------------------------

/// Turn one captured block into a [`ProbeReading`]. A capture error stays an
/// error — never mapped to silence, which would read as a silent loop and be
/// refused.
pub(crate) fn reading_from_block(
    block: Result<&[f32], String>,
    sample_rate: u32,
    xruns_delta: u32,
) -> ProbeReading {
    let block = match block {
        Ok(b) => b,
        Err(e) => return ProbeReading::capture_failed(e, xruns_delta),
    };
    let tone = ac_core::measurement::thd::analyze(block, sample_rate, PROBE_FREQ_HZ, 10)
        .ok()
        .map(|r| ToneReading {
            fundamental_dbfs: r.fundamental_dbfs,
            noise_floor_dbfs: r.noise_floor_dbfs,
            n: block.len(),
        });
    ProbeReading {
        capture: Ok(()),
        xruns_delta,
        total_peak_dbfs: session::total_peak_dbfs(block),
        tone,
    }
}

/// Settle and capture one probe block on a running engine that is already
/// playing the probe tone. Shared with `calibrate` step 2 so the baseline
/// and the check read the loop the same way.
pub(crate) fn capture_probe_block(eng: &mut dyn AudioEngine) -> (Result<Vec<f32>, String>, u32) {
    let before = eng.xruns();
    eng.flush_capture();
    std::thread::sleep(std::time::Duration::from_secs_f64(PROBE_SETTLE_S));
    let block = eng
        .capture_block(PROBE_CAPTURE_S)
        .map_err(|e| format!("{e:#}"));
    (block, eng.xruns().wrapping_sub(before))
}

/// Probe the loopback in its own engine lifecycle: start, play the tone,
/// capture one block, stop.
fn probe_loop_gain(fake: bool, required: Option<&str>, loopback: &Loopback) -> ProbeReading {
    let mut eng = match make_engine(fake, required) {
        Ok(e) => e,
        Err(e) => return ProbeReading::capture_failed(format!("{e:#}"), 0),
    };
    if let Err(e) = eng.start(
        std::slice::from_ref(&loopback.output_port),
        Some(&loopback.input_port),
    ) {
        return ProbeReading::capture_failed(format!("{e:#}"), 0);
    }
    let sr = eng.sample_rate();
    eng.set_tone(
        PROBE_FREQ_HZ,
        ac_core::shared::generator::dbfs_to_amplitude(PROBE_DRIVE_DBFS),
    );
    let (block, xruns) = capture_probe_block(&mut *eng);
    eng.set_silence();
    eng.stop();
    reading_from_block(block.as_deref().map_err(Clone::clone), sr, xruns)
}

/// Run the voltage layer of a check on `loopback` into `rec`.
fn check_voltage(
    rec: &mut SessionCheckRecord,
    fake: bool,
    required: Option<&str>,
    loopback: &Loopback,
    source: CheckSource,
) {
    let cal = Calibration::load(loopback.out_ch, loopback.in_ch, None)
        .ok()
        .flatten();
    let baseline: Option<LoopGainBaseline> =
        cal.as_ref().and_then(|c| c.loop_gain_baseline.clone());
    let ctx = CheckCtx {
        key: loopback.key(),
        checked_at: rec.ran_at.clone(),
        source,
    };
    let verdict = match cal.as_ref().filter(|c| c.has_voltage()) {
        None => {
            LayerVerdict::unverified(UnverifiedCause::NotStored, "no voltage calibration stored")
        }
        Some(_) => match voltage_precheck(baseline.as_ref(), PROBE_DRIVE_DBFS, &ctx.key) {
            Some(v) => v,
            None => {
                let reading = probe_loop_gain(fake, required, loopback);
                rec.stimulus = Some(ProbeStimulus {
                    level_dbfs: PROBE_DRIVE_DBFS,
                    freq_hz: PROBE_FREQ_HZ,
                });
                rec.duration_s = PROBE_SETTLE_S + PROBE_CAPTURE_S;
                rec.probe = Some(reading.summary(PROBE_DRIVE_DBFS));
                judge_voltage(baseline.as_ref(), &reading, PROBE_DRIVE_DBFS, &ctx)
            }
        },
    };
    if let Some(b) = &baseline {
        rec.judged.voltage = Some(VoltageIdentity {
            measured_at: b.measured_at.clone(),
        });
    }
    rec.voltage = Some(verdict);
}

// ---------------------------------------------------------------------------
// The gate every emitting command runs
// ---------------------------------------------------------------------------

/// The unit a command's level was typed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LevelUnit {
    Dbfs,
    Dbu,
    Vrms,
}

impl LevelUnit {
    /// `level_unit` from a request; absent is dBFS.
    pub(crate) fn from_request(cmd: &Value) -> Result<LevelUnit, Value> {
        match cmd.get("level_unit") {
            None | Some(Value::Null) => Ok(LevelUnit::Dbfs),
            Some(v) => match v.as_str() {
                Some("dbfs") => Ok(LevelUnit::Dbfs),
                Some("dbu") => Ok(LevelUnit::Dbu),
                Some("vrms") => Ok(LevelUnit::Vrms),
                _ => Err(json!({
                    "ok": false,
                    "error": format!("level_unit must be \"dbfs\", \"dbu\" or \"vrms\", got {v}"),
                })),
            },
        }
    }

    fn physical(self) -> bool {
        self != LevelUnit::Dbfs
    }

    fn name(self) -> &'static str {
        match self {
            LevelUnit::Dbfs => "dBFS",
            LevelUnit::Dbu => "dBu",
            LevelUnit::Vrms => "Vrms",
        }
    }
}

/// The command pair's stored calibration, for an emitting command that
/// does not otherwise load one (`generate`, `sweep_*`). An unreadable store
/// refuses only when the level was typed in a physical unit — the one case
/// where the scale decides what is emitted.
pub(crate) fn gate_pair_cal(
    cfg: &Config,
    level_unit: LevelUnit,
) -> Result<Option<Calibration>, Value> {
    match super::super::load_calibration_or_refuse(
        cfg.output_channel,
        cfg.input_channel,
        "measurement",
        None,
    ) {
        Ok(cal) => Ok(cal),
        Err(msg) if level_unit.physical() => Err(json!({"ok": false, "error": msg})),
        Err(_) => Ok(None),
    }
}

/// A command's session check, planned at launch and run in its worker.
pub(crate) struct Gate {
    state: ServerState,
    cmd: &'static str,
    pair_cal: Option<Calibration>,
    level_unit: LevelUnit,
    /// `Ok` when the voltage probe will run; `Err(reason)` when it will not.
    probe: Result<Loopback, String>,
    /// The loopback for a same-capture τ verdict (`plot_ir`).
    tau_loopback: Option<Loopback>,
    fake: bool,
    required: Option<String>,
}

/// What the gate hands the command's worker.
pub(crate) struct GateOutcome {
    /// The pair's calibration with a refused voltage scale withheld.
    pub(crate) cal: Option<Calibration>,
    /// The pair's voltage verdict; `None` when the pair stores no scale.
    pub(crate) voltage: Option<LayerVerdict>,
    /// The record the probe made, when it ran.
    pub(crate) record: Option<SessionCheckRecord>,
}

impl Gate {
    /// Decide at launch whether the check runs. `uses_tau` is `plot_ir`'s
    /// same-capture latency verdict, which needs a loopback but no probe.
    pub(crate) fn plan(
        state: &ServerState,
        cfg: &Config,
        cmd: &'static str,
        pair_cal: Option<Calibration>,
        level_unit: LevelUnit,
        uses_tau: bool,
    ) -> Gate {
        let loopback = configured_loopback(cfg, state);
        let needs_voltage =
            pair_cal.as_ref().is_some_and(Calibration::has_voltage) || level_unit.physical();
        // A missing loopback is the reason whenever a layer is consumed:
        // `plot_ir`'s latency verdict needs it even with no voltage in use.
        let probe = match (&loopback, needs_voltage || uses_tau) {
            (_, false) => Err("no stored voltage scale in use".to_string()),
            (Ok(None), true) => Err(NO_LOOPBACK.to_string()),
            (Err(e), true) => Err(e.clone()),
            (Ok(Some(_)), true) if !needs_voltage => {
                Err("no stored voltage scale in use".to_string())
            }
            (Ok(Some(l)), true) => Ok(l.clone()),
        };
        let tau_loopback = if uses_tau {
            loopback.ok().flatten()
        } else {
            None
        };
        let required = cfg.backend.clone();
        let fake = state.fake_audio
            || selected_backend(state.fake_audio, required.as_deref()).as_deref() == Ok("fake");
        Gate {
            state: state.clone(),
            cmd,
            pair_cal,
            level_unit,
            probe,
            tau_loopback,
            fake,
            required,
        }
    }

    /// Whether a `session_check` frame will be published.
    pub(crate) fn pending(&self) -> bool {
        self.probe.is_ok() || self.tau_loopback.is_some()
    }

    /// `session_check: "pending" | "not_run"` and, with `not_run`,
    /// `session_check_reason`, on the command's REQ reply.
    pub(crate) fn write_reply(&self, reply: &mut Value) {
        if self.pending() {
            reply["session_check"] = json!("pending");
        } else {
            reply["session_check"] = json!("not_run");
            if let Err(reason) = &self.probe {
                reply["session_check_reason"] = json!(reason);
            }
        }
    }

    /// Run the voltage check (when planned) and decide the pair's verdict.
    /// `Err(())` means a command-level refusal: the `error` frame is already
    /// published and the command must emit nothing. With `publish`, the
    /// `session_check` frame goes out now; `plot_ir` publishes its own later.
    pub(crate) fn run(
        &self,
        pub_tx: &crossbeam_channel::Sender<Vec<u8>>,
        publish: bool,
    ) -> Result<GateOutcome, ()> {
        let pair_key = self.pair_cal.as_ref().map(Calibration::key);
        let mut fresh: Option<SessionCheckRecord> = None;
        if let Ok(loopback) = &self.probe {
            let mut rec = new_record(
                &self.state,
                self.cmd,
                CheckSource::Probe,
                loopback,
                epoch_for(self.fake, self.required.as_deref()),
            );
            check_voltage(
                &mut rec,
                self.fake,
                self.required.as_deref(),
                loopback,
                CheckSource::Probe,
            );
            fill_reach(&mut rec);
            fresh = Some(record(&self.state, rec));
        } else if let Some(loopback) = &self.tau_loopback {
            // `plot_ir` without a voltage layer: a latency-only record,
            // stimulus none, filled in after analysis.
            let rec = new_record(
                &self.state,
                self.cmd,
                CheckSource::SameCapture,
                loopback,
                epoch_for(self.fake, self.required.as_deref()),
            );
            fresh = Some(rec);
        }

        let recorded = Recorded::now(&self.state);
        let voltage = self.pair_cal.as_ref().filter(|c| c.has_voltage()).map(|c| {
            let effective = recorded.voltage(c).verdict;
            let attempted = fresh
                .as_ref()
                .filter(|r| Some(&r.loopback.key) == pair_key.as_ref())
                .and_then(|r| r.voltage.clone());
            match attempted {
                // A layer the probe attempted reads what the probe found,
                // unless an earlier refusal still stands.
                Some(v) if v.is_decisive() || !effective.is_refused() => v,
                _ => effective,
            }
        });

        if let Some(r) = fresh.as_ref().filter(|_| publish && self.probe.is_ok()) {
            publish_frame(pub_tx, r, pair_key.as_deref(), voltage.as_ref(), None);
        }

        let refused = voltage.as_ref().is_some_and(LayerVerdict::is_refused);
        if refused && self.level_unit.physical() {
            let mut frame = json!({
                "cmd": self.cmd,
                "message": format!(
                    "{} level refused \u{2014} stored output calibration is stale; nothing was emitted",
                    self.level_unit.name()
                ),
                "voltage_check": voltage,
                "level_unit": self.level_unit.name(),
            });
            if let Some(r) = &fresh {
                frame["session_check"] = json!(r);
            } else if let Some(v) = voltage.as_ref().and_then(|_| {
                self.pair_cal
                    .as_ref()
                    .and_then(|c| recorded.voltage(c).record)
            }) {
                frame["session_check_record"] = json!(v);
            }
            send_pub(pub_tx, "error", &frame);
            return Err(());
        }

        Ok(GateOutcome {
            cal: gated(self.pair_cal.clone(), voltage.as_ref()),
            voltage,
            record: fresh,
        })
    }

    /// `plot_ir`: add the same-capture τ verdict to the gate's record, store
    /// it, and publish the `session_check` frame. `pair_latency` receives the
    /// measurement pair's own latency verdict for the caller to apply.
    pub(crate) fn finish_latency(
        &self,
        pub_tx: &crossbeam_channel::Sender<Vec<u8>>,
        outcome: &GateOutcome,
        latency: Option<(LayerVerdict, Option<LatencyIdentity>)>,
        pair_latency: impl FnOnce(&Recorded, Option<&SessionCheckRecord>) -> Option<LayerVerdict>,
    ) -> Option<LayerVerdict> {
        let Some(mut rec) = outcome.record.clone() else {
            let recorded = Recorded::now(&self.state);
            return pair_latency(&recorded, None);
        };
        if let Some((verdict, identity)) = latency {
            rec.latency = Some(verdict);
            rec.judged.latency = identity;
            fill_reach(&mut rec);
            rec = record(&self.state, rec);
        } else if rec.voltage.is_some() {
            rec = record(&self.state, rec);
        }
        let recorded = Recorded::now(&self.state);
        let pair = pair_latency(&recorded, Some(&rec));
        let pair_key = self.pair_cal.as_ref().map(Calibration::key);
        publish_frame(
            pub_tx,
            &rec,
            pair_key.as_deref(),
            outcome.voltage.as_ref(),
            pair.as_ref(),
        );
        pair
    }
}

/// The epoch of the backend `fake`/`required` selects.
fn epoch_for(fake: bool, required: Option<&str>) -> DeviceEpoch {
    match selected_backend(fake, required) {
        Ok(backend) => current_epoch(&backend),
        Err(reason) => DeviceEpoch::NotObservable { reason },
    }
}

/// Publish a `session_check` frame: the record, plus the command pair's own
/// verdicts when the command consumes a pair.
fn publish_frame(
    pub_tx: &crossbeam_channel::Sender<Vec<u8>>,
    rec: &SessionCheckRecord,
    pair_key: Option<&str>,
    pair_voltage: Option<&LayerVerdict>,
    pair_latency: Option<&LayerVerdict>,
) {
    let mut frame = json!(rec);
    if let Some(key) = pair_key {
        frame["pair"] = json!({
            "key": key,
            "voltage": pair_voltage,
            "latency": pair_latency,
        });
    }
    send_pub(pub_tx, "session_check", &frame);
}

// ---------------------------------------------------------------------------
// The `session_check` command
// ---------------------------------------------------------------------------

/// The reason a τ attempt gives when it did not measure.
fn tau_not_measured_reason(outcome: &TauOutcome) -> String {
    let samples = |s: f64, c: Option<&ac_core::shared::calibration::TauConditions>| {
        c.map(|c| format!("{:.0}", s * c.sample_rate as f64))
            .unwrap_or_else(|| format!("{s:.6} s"))
    };
    match outcome {
        TauOutcome::Disagree {
            conditions,
            reading1_s,
            reading2_s,
            ..
        } => format!(
            "2 lifetimes disagree: {} and {} samples",
            samples(*reading1_s, Some(conditions)),
            samples(*reading2_s, Some(conditions))
        ),
        other => {
            let mut frame = json!({});
            other.write_frame(&mut frame);
            match frame.get("tau_error").and_then(Value::as_str) {
                Some(e) => e.to_string(),
                None => format!("\u{3c4} reading refused ({})", other.state()),
            }
        }
    }
}

/// `session_check` — measure both layers on the loopback now.
pub fn session_check(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "session_check");
    cfg_guard!(state);
    let (want_voltage, want_latency) = match cmd.get("layers") {
        None | Some(Value::Null) => (true, true),
        Some(Value::Array(items)) => {
            let mut v = false;
            let mut l = false;
            for item in items {
                match item.as_str() {
                    Some("voltage") => v = true,
                    Some("latency") => l = true,
                    _ => {
                        return json!({"ok": false,
                            "error": format!("layers: unknown layer {item}; use \"latency\", \"voltage\"")})
                    }
                }
            }
            (v, l)
        }
        Some(other) => {
            return json!({"ok": false,
                "error": format!("layers must be an array, got {other}")})
        }
    };
    let refusal_record = refusal_record_value(&file_view(state));
    let cfg = state.cfg.lock().unwrap().clone();
    let loopback = match configured_loopback(&cfg, state) {
        Ok(Some(l)) => l,
        Ok(None) => {
            return json!({"ok": false, "error": NO_LOOPBACK, "refusal_record": refusal_record})
        }
        Err(e) => return json!({"ok": false, "error": e, "refusal_record": refusal_record}),
    };
    let required = cfg.backend.clone();
    let backend = match selected_backend(state.fake_audio, required.as_deref()) {
        Ok(b) => b,
        Err(e) => return json!({"ok": false, "error": e, "refusal_record": refusal_record}),
    };
    let fake = state.fake_audio || backend == "fake";
    let reply_backend = backend.clone();
    let device = cfg.device;
    let pub_tx = state.pub_tx.clone();
    let worker_state = state.clone();
    let lb = loopback.clone();

    let worker = spawn_worker(state, "session_check", move |stop: Arc<AtomicBool>| {
        let state = worker_state;
        let mut rec = new_record(
            &state,
            "session_check",
            CheckSource::Explicit,
            &lb,
            epoch_for(fake, required.as_deref()),
        );
        if want_voltage {
            check_voltage(
                &mut rec,
                fake,
                required.as_deref(),
                &lb,
                CheckSource::Explicit,
            );
        }
        if want_latency && !stop.load(Ordering::Relaxed) {
            let amp = ac_core::shared::generator::dbfs_to_amplitude(PROBE_DRIVE_DBFS);
            let outcome = tau_result(|| {
                measure_tau_twice(
                    fake,
                    required.as_deref(),
                    device,
                    &lb.output_port,
                    &lb.input_port,
                    amp,
                )
            });
            let ctx = CheckCtx {
                key: lb.key(),
                checked_at: rec.ran_at.clone(),
                source: CheckSource::Explicit,
            };
            let verdict = match outcome.conditions() {
                None => LayerVerdict::unverified(
                    UnverifiedCause::NotMeasured,
                    tau_not_measured_reason(&outcome),
                ),
                Some(cond) => {
                    let cal = Calibration::load(lb.out_ch, lb.in_ch, None).ok().flatten();
                    let resolved = cal
                        .as_ref()
                        .and_then(|c| c.tau_for(cond, &rec.epoch).ok().map(|r| r.entry.clone()));
                    let missing = no_stored_latency_reason(cond.sample_rate, cond.period_size);
                    let stored = resolved.as_ref().map(|e| StoredTau {
                        tau_s: e.tau_s,
                        measured_at: e.measured_at.clone(),
                    });
                    if let Some(e) = &resolved {
                        rec.judged.latency = Some(LatencyIdentity {
                            measured_at: e.measured_at.clone(),
                            tau_s: e.tau_s,
                            conditions: e.conditions.clone(),
                        });
                    }
                    let measured = match &outcome {
                        TauOutcome::Measured {
                            tau_s,
                            separation_s,
                            ..
                        } => {
                            rec.latency_separation_s = Some(*separation_s);
                            Ok(*tau_s)
                        }
                        other => Err(tau_not_measured_reason(other)),
                    };
                    judge_latency(
                        stored.as_ref().ok_or(missing.as_str()),
                        measured.as_ref().map(|v| *v).map_err(String::as_str),
                        cond.sample_rate,
                        cond.period_size,
                        &ctx,
                    )
                }
            };
            rec.latency = Some(verdict);
        }
        if stop.load(Ordering::Relaxed) {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd": "session_check", "message": "session check stopped"}),
            );
            return;
        }
        fill_reach(&mut rec);
        let rec = record(&state, rec);
        publish_frame(&pub_tx, &rec, None, None, None);
        send_pub(
            &pub_tx,
            "done",
            &json!({"cmd": "session_check", "backend": backend}),
        );
    });
    state
        .workers
        .lock()
        .unwrap()
        .insert("session_check".to_string(), worker);

    json!({
        "ok": true,
        "loopback": {
            "key": loopback.key(),
            "output_port": loopback.output_port,
            "input_port": loopback.input_port,
        },
        "stimulus": want_voltage.then(|| json!({
            "level_dbfs": PROBE_DRIVE_DBFS,
            "freq_hz": PROBE_FREQ_HZ,
        })),
        "refusal_record": refusal_record,
        "backend": reply_backend,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(n: usize, sr: u32, amp: f64) -> Vec<f32> {
        (0..n)
            .map(|i| {
                (amp * (2.0 * std::f64::consts::PI * PROBE_FREQ_HZ * i as f64 / sr as f64).sin())
                    as f32
            })
            .collect()
    }

    /// The rejected shape: `capture_rms` maps a capture error to 0.0, which
    /// a silence rule would refuse. The probe keeps the error.
    #[test]
    fn a_capture_error_is_kept_as_an_error_not_silence() {
        let r = reading_from_block(Err("device gone".to_string()), 48_000, 0);
        assert_eq!(r.capture, Err("device gone".to_string()));
        assert!(r.tone.is_none());
        let rejected = super::super::super::rms_to_dbfs(0.0);
        assert!(rejected < -200.0, "the rejected mapping reads as silence");
    }

    #[test]
    fn digital_silence_has_no_tone_and_the_floor_level() {
        let r = reading_from_block(Ok(&vec![0.0; 14_400]), 48_000, 0);
        assert!(r.tone.is_none());
        assert!(r.total_peak_dbfs < -200.0);
    }

    #[test]
    fn a_clean_tone_reads_its_peak_level() {
        let block = sine(14_400, 48_000, 0.01);
        let r = reading_from_block(Ok(&block), 48_000, 0);
        let tone = r.tone.expect("tone");
        assert!((tone.fundamental_dbfs - (-40.0)).abs() < 0.05, "{tone:?}");
        assert!((r.total_peak_dbfs - (-40.0)).abs() < 0.05);
        assert!(r.snr_db().unwrap() > session::PROBE_SNR_MIN_DB);
    }

    fn check_record(id: &str, voltage: LayerVerdict) -> SessionCheckRecord {
        SessionCheckRecord {
            id: id.to_string(),
            ran_at: "2026-09-16T14:02:11.000Z".to_string(),
            epoch: current_epoch("fake"),
            source: CheckSource::Explicit,
            cmd: "session_check".to_string(),
            loopback: LoopbackRef {
                key: "out1_in1".to_string(),
                output_port: "system:playback_2".to_string(),
                input_port: "system:capture_2".to_string(),
            },
            stimulus: None,
            duration_s: 0.0,
            reach: Default::default(),
            latency: None,
            voltage: Some(voltage),
            judged: JudgedIdentity::default(),
            probe: None,
            latency_separation_s: None,
            persisted: None,
            persist_error: None,
        }
    }

    fn evidence() -> session::Evidence {
        session::Evidence {
            measured: -0.6,
            stored: -0.6,
            delta: 0.0,
            tolerance: 0.1,
            unit: session::VerdictUnit::Db,
            stored_at: "2026-09-15T23:43:04Z".to_string(),
            checked_at: "2026-09-16T14:02:11.000Z".to_string(),
            source: CheckSource::Explicit,
        }
    }

    /// QA 3 on PR #534 (R6-6): after a successful sync, neither a pass nor
    /// a written refusal is merged again. Before it, both are — the
    /// rejected behaviour merged every process record on every sync.
    #[test]
    fn a_synced_record_is_not_merged_again() {
        let mut checks = SessionChecks::default();
        let mut refused = check_record(
            "1-1",
            LayerVerdict::Refused {
                evidence: evidence(),
                via: None,
                delta_bound: None,
            },
        );
        refused.persisted = Some(false);
        checks.push(refused);
        checks.push(check_record("1-2", LayerVerdict::Verified(evidence())));
        assert_eq!(checks.unsynced().len(), 2);

        checks.mark_persisted();
        assert!(checks.unsynced().is_empty());

        // Re-pushing a record (a gate adding a layer) makes it unsynced.
        checks.push(check_record("1-2", LayerVerdict::Verified(evidence())));
        assert_eq!(checks.unsynced().len(), 1);
    }

    #[test]
    fn level_unit_parses_and_refuses_unknown_units() {
        assert_eq!(
            LevelUnit::from_request(&json!({})).unwrap(),
            LevelUnit::Dbfs
        );
        assert_eq!(
            LevelUnit::from_request(&json!({"level_unit": "dbu"})).unwrap(),
            LevelUnit::Dbu
        );
        assert!(LevelUnit::from_request(&json!({"level_unit": "volts"})).is_err());
    }
}
