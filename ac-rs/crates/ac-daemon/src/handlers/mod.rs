//! Command handlers, split by concern.
//!
//! - `admin`     — status, quit, stop, devices, setup, server_*, calibrations, dmm_read
//! - `audio`     — generate / sweep / plot / monitor
//! - `calibrate` — calibrate state machine + cal_reply
//! - `transfer`  — transfer_stream + probe (port-routing aware)
//! - `test_hw`   — hardware self-tests
//! - `test_dut`  — DUT qualification suite + dut_reply
//!
//! Shared helpers (busy guard, port resolution, frame builders, common DSP
//! helpers used by more than one submodule) live in this module and are
//! `pub(super)` so submodules import them via `use super::*`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use serde_json::{json, Value};

use ac_core::config::Config;
use ac_core::shared::calibration::Calibration;

use crate::audio::{make_engine, AudioEngine};
use crate::server::ServerState;
use crate::workers::{cmd_group, Group, WorkerHandle};

mod admin;
mod audio;
mod calibrate;
pub(crate) mod mic;
pub mod snapshot;
mod test_dut;
mod test_hw;
mod test_software;
mod transfer;

pub use admin::{
    devices, dmm_read, get_analysis_mode, get_band_weighting, get_calibration,
    get_time_integration, list_calibrations, quit, reset_leq, reset_loudness, server_connections,
    server_disable, server_enable, set_analysis_mode, set_band_weighting, set_ioct_bpo,
    set_monitor_params, set_time_integration, setup, status, stop,
};
pub use audio::{
    generate, generate_pink, monitor_spectrum, plot, plot_ir, plot_level, sweep_frequency,
    sweep_level,
};
pub use calibrate::{
    cal_reply, calibrate, calibrate_mic_curve, calibrate_spl, set_mic_correction_enabled,
};
pub use snapshot::{snapshot, snapshot_delete, snapshot_fetch, snapshot_list};
pub use test_dut::{dut_reply, test_dut};
pub use test_hw::test_hardware;
pub use test_software::test_software;
pub use transfer::{probe, relock, set_drive, transfer_stream};

// ---------------------------------------------------------------------------
// Busy guard
// ---------------------------------------------------------------------------

/// Check the busy guard and return Err string if a conflict exists.
pub(super) fn check_busy(state: &ServerState, new_cmd: &str) -> Option<String> {
    let new_group = cmd_group(new_cmd)?;

    let workers = state.workers.lock().unwrap();
    if workers.is_empty() {
        return None;
    }

    for name in workers.keys() {
        if matches!(cmd_group(name), Some(Group::Exclusive)) {
            return Some(format!("busy: {name} running — send stop first"));
        }
    }

    if new_group == Group::Exclusive {
        let running: Vec<&String> = workers.keys().collect();
        return Some(format!("busy: {} running — send stop first", running[0]));
    }

    for name in workers.keys() {
        if cmd_group(name) == Some(new_group) {
            return Some(format!("busy: {name} running — send stop first"));
        }
    }

    None
}

macro_rules! busy_guard {
    ($state:expr, $name:expr) => {
        if let Some(msg) = $crate::handlers::check_busy($state, $name) {
            return ::serde_json::json!({"ok": false, "error": msg});
        }
    };
}
pub(super) use busy_guard;

// ---------------------------------------------------------------------------
// Worker spawn
// ---------------------------------------------------------------------------

pub(super) fn spawn_worker<F>(_state: &ServerState, _cmd_name: &str, f: F) -> WorkerHandle
where
    F: FnOnce(Arc<AtomicBool>) + Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let t = thread::spawn(move || f(stop2));
    WorkerHandle {
        stop_flag: stop,
        thread: Some(t),
    }
}

// ---------------------------------------------------------------------------
// Port cache (Issue #30)
//
// JACK port queries open a fresh `ac-daemon-probe` client every call, which
// costs a full activate/deactivate round-trip (~50–150 ms on a busy jackd).
// Before this cache, a single `test_hardware` invocation would spin up 4+
// probe clients just to resolve sticky port names for routing. The cache is
// populated lazily on first read and refreshed whenever the user issues
// `devices` (which is the documented way to "rescan hardware").
// ---------------------------------------------------------------------------

pub(super) fn cached_playback_ports(state: &ServerState) -> Vec<String> {
    let mut guard = state.playback_ports_cache.lock().unwrap();
    if guard.is_none() {
        let eng = make_engine(state.fake_audio);
        *guard = Some(eng.playback_ports());
    }
    guard.clone().unwrap_or_default()
}

pub(super) fn cached_capture_ports(state: &ServerState) -> Vec<String> {
    let mut guard = state.capture_ports_cache.lock().unwrap();
    if guard.is_none() {
        let eng = make_engine(state.fake_audio);
        *guard = Some(eng.capture_ports());
    }
    guard.clone().unwrap_or_default()
}

/// Force a rescan on the next port query. Called by `devices`.
pub(super) fn refresh_port_cache(state: &ServerState) {
    let eng = make_engine(state.fake_audio);
    *state.playback_ports_cache.lock().unwrap() = Some(eng.playback_ports());
    *state.capture_ports_cache.lock().unwrap() = Some(eng.capture_ports());
}

// ---------------------------------------------------------------------------
// Port resolution
// ---------------------------------------------------------------------------

/// Shared error text for an out-of-range channel index.
///
/// Names the kind, the index, and every port that *is* available, because the
/// operator's next question is always "then what should I have said?".
fn out_of_range(kind: &str, channel: u32, ports: &[String]) -> String {
    if ports.is_empty() {
        return format!(
            "no physical {kind} ports are available, so channel {channel} cannot be \
             resolved — is the audio server running?"
        );
    }
    format!(
        "{kind} channel {channel} is out of range — only {} physical {kind} port(s) \
         available: {}",
        ports.len(),
        ports.join(", "),
    )
}

/// Resolve output port: config sticky name, or the channel index in the
/// engine's playback list.
///
/// **Fails rather than fabricating** (issue #206). This used to fall back to
/// `"system:playback_1"` on an out-of-range index, which on any modern
/// pipewire-jack setup — where ports are named e.g. `Babyface Pro Pro:playback_5`
/// — matches nothing at all. `jack_backend::start` discards the connect result
/// with `.ok()`, so the session came up wired to nothing while the reply and
/// every subsequent frame named a port the operator never configured. On the
/// drive path it was worse than cosmetic: a mistyped `output_channel` silently
/// retargeted the stimulus to channel 1.
pub(super) fn resolve_output(cfg: &Config, state: &ServerState) -> Result<String, String> {
    if let Some(p) = &cfg.output_port {
        return Ok(p.clone());
    }
    let ports = cached_playback_ports(state);
    ports
        .get(cfg.output_channel as usize)
        .cloned()
        .ok_or_else(|| out_of_range("playback", cfg.output_channel, &ports))
}

/// Resolve a specific output channel index to a playback port. Used when
/// the caller supplied an explicit `channels` list, where the sticky
/// `cfg.output_port` only applies to `cfg.output_channel` and other
/// indices must come from the engine's playback-port list. Returns
/// `Err` with a human-readable message when the index is out of range
/// — the caller surfaces it as a loud failure rather than silently
/// connecting to a synthesized port name JACK doesn't recognise.
pub(super) fn resolve_output_by_channel(
    cfg: &Config,
    state: &ServerState,
    channel: u32,
) -> Result<String, String> {
    if channel == cfg.output_channel {
        return resolve_output(cfg, state);
    }
    let ports = cached_playback_ports(state);
    ports
        .get(channel as usize)
        .cloned()
        .ok_or_else(|| out_of_range("playback", channel, &ports))
}

/// Resolve input port: config sticky name, or the channel index in the
/// engine's capture list. Fails rather than fabricating — see
/// [`resolve_output`].
pub(super) fn resolve_input(cfg: &Config, state: &ServerState) -> Result<String, String> {
    if let Some(p) = &cfg.input_port {
        return Ok(p.clone());
    }
    let ports = cached_capture_ports(state);
    ports
        .get(cfg.input_channel as usize)
        .cloned()
        .ok_or_else(|| out_of_range("capture", cfg.input_channel, &ports))
}

/// Error for a sticky port name that is set but gated off by a missing
/// channel, so it would be read, ignored, and never mentioned.
///
/// #225 was a configured value silently doing the wrong thing. Accepting an
/// explicitly-set port that has no effect is the same failure in a new place,
/// so both gated legs refuse it rather than resolving past it.
fn orphaned_sticky(port_key: &str, channel_key: &str) -> String {
    format!(
        "{port_key} is set but {channel_key} is not, so the port would be ignored — \
         set {channel_key}, or clear {port_key}"
    )
}

/// Resolve the reference *input* port.
///
/// `Ok(None)` means no reference channel is configured, which is a legitimate
/// state. `Err` means one is configured but does not exist. Those were both
/// `None` before, so an out-of-range reference channel silently presented as
/// "no reference" — the measurement then ran single-ended while the operator
/// believed a reference was wired in.
pub(super) fn resolve_ref_input(
    cfg: &Config,
    state: &ServerState,
) -> Result<Option<String>, String> {
    let Some(ch) = cfg.reference_channel else {
        // A sticky port with no channel is not "no reference configured" — it
        // is a configured value with no effect. `test_hardware`/`test_dut`
        // admit this config (their guard passes on either field) and would
        // then measure single-ended against the measurement input while the
        // operator believed the named port was wired in.
        if cfg.reference_port.is_some() {
            return Err(orphaned_sticky("reference_port", "reference_channel"));
        }
        return Ok(None);
    };
    if let Some(p) = &cfg.reference_port {
        return Ok(Some(p.clone()));
    }
    let ports = cached_capture_ports(state);
    ports
        .get(ch as usize)
        .cloned()
        .map(Some)
        .ok_or_else(|| out_of_range("capture", ch, &ports))
}

/// Fires the stderr half of [`ref_output_migration_warning`] once per process.
static REF_OUTPUT_MIGRATION_WARNED: AtomicBool = AtomicBool::new(false);

/// Warning for a config whose *meaning* changed with #225: one that sets
/// `reference_channel` and nothing else.
///
/// Before #225, that config drove the reference stimulus out
/// `playback[reference_channel]`. Now it leaves on the main output, because the
/// reference output leg has its own index and this one is unset. On a rig where
/// the loopback source happened to sit at that playback index, the old code was
/// accidentally right, and the upgrade moves the drive off it — the loopback
/// goes silent, which is the very failure #225 exists to fix.
///
/// Defaulting the output index from the input index is not available as a
/// remedy: deriving one from the other *is* the bug. So the change is announced
/// instead.
///
/// **The warning is a stopgap, not the fix.** It reaches a CLI user through the
/// reply's `warnings` array and a daemon operator through stderr — and an
/// `ac-view` user through neither. What catches this condition at runtime is
/// #228's `NO REFERENCE` state (driving, reference leg at the floor), which
/// observes the symptom instead of predicting it from config shape. Do not read
/// the presence of this warning as the case being covered.
///
/// The text is returned on every call, so a client that connects later still
/// receives it; only the stderr line is deduplicated, so a long-running daemon
/// does not repeat it for every session.
pub(super) fn ref_output_migration_warning(cfg: &Config) -> Option<String> {
    let ref_ch = cfg.reference_channel?;
    if cfg.reference_output_channel.is_some() {
        return None;
    }
    let text = format!(
        "reference_output_channel is not set, so the reference stimulus leaves on the main \
         output. Before #225 this config drove it out playback[{ref_ch}] — if that is where \
         the loopback source is wired, run: ac setup refout {ref_ch}"
    );
    if !REF_OUTPUT_MIGRATION_WARNED.swap(true, Ordering::Relaxed) {
        eprintln!("warning: {text}");
    }
    Some(text)
}

/// Resolve the reference *output* port, falling back to the main output when
/// no reference output channel is configured.
///
/// Resolves from `reference_output_channel`, a playback index. It used to
/// resolve from `reference_channel` — a *capture* index — so on any rig where
/// the loopback source and the reference capture sat at different indices, the
/// daemon opened and drove a playback port nothing was patched to and the
/// reference leg stayed at digital silence (#225). The two index spaces are
/// unrelated; neither is derived from the other in either direction.
///
/// Only the *fully* unconfigured case falls back. A configured-but-out-of-range
/// channel is an error — silently emitting the reference stimulus on the main
/// output is a drive-path hazard, not a graceful degradation — and so is a
/// sticky port with no channel to gate it (see [`orphaned_sticky`]).
pub(super) fn resolve_ref_output(cfg: &Config, state: &ServerState) -> Result<String, String> {
    let Some(ch) = cfg.reference_output_channel else {
        if cfg.reference_output_port.is_some() {
            return Err(orphaned_sticky(
                "reference_output_port",
                "reference_output_channel",
            ));
        }
        return resolve_output(cfg, state);
    };
    if let Some(p) = &cfg.reference_output_port {
        return Ok(p.clone());
    }
    let ports = cached_playback_ports(state);
    ports
        .get(ch as usize)
        .cloned()
        .ok_or_else(|| out_of_range("playback", ch, &ports))
}

// ---------------------------------------------------------------------------
// PUB frame helper
// ---------------------------------------------------------------------------

pub(super) fn send_pub(tx: &crossbeam_channel::Sender<Vec<u8>>, topic: &str, frame: &Value) {
    let mut msg = topic.as_bytes().to_vec();
    msg.push(b' ');
    msg.extend_from_slice(serde_json::to_vec(frame).unwrap_or_default().as_slice());
    let _ = tx.send(msg);
}

// ---------------------------------------------------------------------------
// Sweep math helpers
// ---------------------------------------------------------------------------

pub(super) fn log_freq_points(start: f64, stop: f64, ppd: usize) -> Vec<f64> {
    let n_decades = (stop / start).log10();
    let n_points = (n_decades * ppd as f64).round() as usize;
    let n_points = n_points.max(2);
    let mut freqs: Vec<f64> = (0..n_points)
        .map(|i| start * (stop / start).powf(i as f64 / (n_points - 1) as f64))
        .collect();
    freqs.dedup_by(|a, b| (*a as u64) == (*b as u64));
    freqs
}

pub(super) fn linspace(start: f64, stop: f64, n: usize) -> Vec<f64> {
    if n <= 1 {
        return vec![start];
    }
    (0..n)
        .map(|i| start + (stop - start) * i as f64 / (n - 1) as f64)
        .collect()
}

pub(super) fn downsample(spec: &[f64], freqs: &[f64], max_pts: usize) -> (Vec<f64>, Vec<f64>) {
    if spec.len() <= max_pts {
        return (spec.to_vec(), freqs.to_vec());
    }
    let n = spec.len();
    let indices: Vec<usize> = {
        let mut v: Vec<usize> = (0..max_pts)
            .map(|i| {
                let t = i as f64 / (max_pts - 1) as f64;
                ((n - 1) as f64 * t) as usize
            })
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let s: Vec<f64> = indices.iter().map(|&i| spec[i]).collect();
    let f: Vec<f64> = indices.iter().map(|&i| freqs[i]).collect();
    (s, f)
}

/// Build a `CalibrationSnapshot` (Tier 1 archival record, #94) from a
/// loaded `Calibration`. The snapshot carries provenance only for the
/// mic-curve — the full curve stays in `cal.json`. `None` when no cal
/// was loaded.
pub(super) fn snapshot_from_cal(
    cal: Option<&Calibration>,
) -> Option<ac_core::measurement::report::CalibrationSnapshot> {
    use ac_core::measurement::report::{CalibrationSnapshot, MicResponseRef};
    cal.map(|c| CalibrationSnapshot {
        output_channel: c.output_channel,
        input_channel: c.input_channel,
        vrms_at_0dbfs_out: c.vrms_at_0dbfs_out,
        vrms_at_0dbfs_in: c.vrms_at_0dbfs_in,
        ref_freq_hz: c.ref_freq,
        ref_level_dbfs: c.ref_dbfs,
        mic_sensitivity_dbfs_at_94db_spl: c.mic_sensitivity_dbfs_at_94db_spl,
        mic_response: c.mic_response.as_ref().map(|r| MicResponseRef {
            n_points: r.freqs_hz.len(),
            source_path: r.source_path.clone(),
            imported_at: r.imported_at.clone(),
        }),
    })
}

/// Processing-context envelope stamped on every Tier 1 frame so the
/// reader (UI, CSV exporter, downstream tooling) can tell exactly what
/// state was active when the measurement was taken — same envelope
/// Tier 2 frames already carry. See #98.
pub(super) struct Tier1Ctx<'a> {
    pub mic_correction: &'a str, // "on" | "off" | "none"
    pub spl_offset_db: Option<f64>,
    pub weighting: &'a str,        // "off" | "a" | "c" | "z"
    pub time_integration: &'a str, // "off" | "fast" | "slow" | "leq"
    /// Still reserved: the daemon does not smooth, and `None` says so
    /// truthfully. #229's fractional-octave smoothing is a *display*
    /// operation applied in `ac-scene` after the frame and captioned on
    /// screen, so it must not be reported here — a Tier 1 measurement that
    /// claimed a smoothing it never applied would be worse than one that
    /// claimed none.
    pub smoothing_bpo: Option<u32>,
}

impl Default for Tier1Ctx<'_> {
    fn default() -> Self {
        Self {
            mic_correction: "none",
            spl_offset_db: None,
            weighting: "off",
            time_integration: "off",
            smoothing_bpo: None,
        }
    }
}

pub(super) fn sweep_point_frame(
    r: &ac_core::shared::types::AnalysisResult,
    cal: Option<&Calibration>,
    n: usize,
    cmd_name: &str,
    level_dbfs: f64,
    freq_hz: Option<f64>,
    ctx: &Tier1Ctx<'_>,
) -> Value {
    let out_vrms = cal.and_then(|c| c.out_vrms(level_dbfs));
    let in_vrms = cal.and_then(|c| c.in_vrms(r.linear_rms));
    let in_dbu = in_vrms.map(ac_core::shared::conversions::vrms_to_dbu);
    let out_dbu = out_vrms.map(ac_core::shared::conversions::vrms_to_dbu);
    let gain_db = in_dbu.zip(out_dbu).map(|(i, o)| i - o);

    let (spec_ds, freqs_ds) = if r.spectrum.len() > 1 {
        downsample(&r.spectrum[1..], &r.freqs[1..], 1000)
    } else {
        (r.spectrum.clone(), r.freqs.clone())
    };

    let harmonic_levels: Vec<Value> = r
        .harmonic_levels
        .iter()
        .map(|&(hz, amp)| json!([hz, amp]))
        .collect();

    let mut frame = json!({
        "type":              "measurement/frequency_response/point",
        "cmd":               cmd_name,
        "n":                 n,
        "drive_db":          level_dbfs,
        "thd_pct":           r.thd_pct,
        "thdn_pct":          r.thdn_pct,
        "fundamental_hz":    r.fundamental_hz,
        "fundamental_dbfs":  r.fundamental_dbfs,
        "linear_rms":        r.linear_rms,
        "harmonic_levels":   harmonic_levels,
        "noise_floor_dbfs":  r.noise_floor_dbfs,
        "spectrum":          spec_ds,
        "freqs":             freqs_ds,
        "clipping":          r.clipping,
        "ac_coupled":        r.ac_coupled,
        "out_vrms":          out_vrms,
        "out_dbu":           out_dbu,
        "in_vrms":           in_vrms,
        "in_dbu":            in_dbu,
        "gain_db":           gain_db,
        "vrms_at_0dbfs_out": cal.and_then(|c| c.vrms_at_0dbfs_out),
        "vrms_at_0dbfs_in":  cal.and_then(|c| c.vrms_at_0dbfs_in),
        // Processing-context envelope (#98) — matches the keys the Tier 2
        // monitor frames carry. `null` / `"off"` / `"none"` when inactive
        // so external subscribers always see the keys.
        "mic_correction":    ctx.mic_correction,
        "spl_offset_db":     ctx.spl_offset_db,
        "weighting":         ctx.weighting,
        "time_integration":  ctx.time_integration,
        "smoothing_bpo":     ctx.smoothing_bpo,
    });

    if let Some(f) = freq_hz {
        frame["freq_hz"] = json!(f);
    }
    frame
}

// ---------------------------------------------------------------------------
// DMM SCPI client
// ---------------------------------------------------------------------------

/// Best-effort DMM read over SCPI TCP (port 5025).
pub(super) fn read_dmm_vrms(host: &str, n: usize) -> Option<f64> {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for _ in 0..n {
        let mut stream = TcpStream::connect_timeout(
            &format!("{host}:5025").parse().ok()?,
            std::time::Duration::from_secs(2),
        )
        .ok()?;
        stream.write_all(b"MEAS:VOLT:AC?\n").ok()?;
        let mut buf = [0u8; 64];
        let bytes = stream.read(&mut buf).ok()?;
        let s = std::str::from_utf8(&buf[..bytes]).ok()?.trim().to_string();
        if let Ok(v) = s.parse::<f64>() {
            sum += v;
            count += 1;
        }
    }
    if count > 0 {
        Some(sum / count as f64)
    } else {
        None
    }
}

/// What a `cal_reply` asks the calibrate worker to do with one field.
///
/// Three intents, not two: "I did not measure this leg" and "erase what
/// you have" are different requests, and collapsing them onto the same
/// reply is what let a skipped prompt delete a stored voltage cal (#279).
/// A timed-out or cancelled prompt resolves to `Skip`, so the failure
/// mode of every path that produces no answer is "keep what is stored".
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum CalReply {
    /// A measured reading, in Vrms at the played/captured level.
    Value(f64),
    /// No reading supplied — leave the stored value alone.
    Skip,
    /// Explicitly erase the stored value.
    Clear,
}

impl CalReply {
    /// The reading, if one was supplied. `Skip` and `Clear` both yield
    /// `None` — use this only where a number is wanted, never to decide
    /// what to write.
    pub(super) fn value(self) -> Option<f64> {
        match self {
            CalReply::Value(v) => Some(v),
            CalReply::Skip | CalReply::Clear => None,
        }
    }
}

/// Poll a cal_reply channel until a reply arrives, stop flag fires, or timeout.
pub(super) fn wait_cal_reply(
    rx: &crossbeam_channel::Receiver<CalReply>,
    stop: &Arc<AtomicBool>,
    timeout_secs: u64,
) -> CalReply {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        if stop.load(Ordering::Relaxed) {
            return CalReply::Skip;
        }
        if std::time::Instant::now() > deadline {
            return CalReply::Skip;
        }
        if let Ok(v) = rx.try_recv() {
            return v;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------------------
// Test result + capture helpers (shared by test_hw and test_dut)
// ---------------------------------------------------------------------------

pub(super) struct TestResult {
    pub name: String,
    pub pass: bool,
    pub detail: String,
    pub tolerance: String,
}

impl TestResult {
    pub(super) fn new(name: &str, pass: bool, detail: String, tolerance: &str) -> Self {
        Self {
            name: name.to_string(),
            pass,
            detail,
            tolerance: tolerance.to_string(),
        }
    }
}

pub(super) fn capture_rms(eng: &mut dyn AudioEngine, duration: f64) -> f64 {
    match eng.capture_block(duration) {
        Ok(data) => {
            let sum_sq: f64 = data.iter().map(|&x| (x as f64).powi(2)).sum();
            (sum_sq / data.len().max(1) as f64).sqrt()
        }
        Err(_) => 0.0,
    }
}

pub(super) fn rms_to_dbfs(rms: f64) -> f64 {
    20.0 * rms.max(1e-12).log10()
}

pub(super) fn analyze_mono(
    eng: &mut dyn AudioEngine,
    freq: f64,
    duration: f64,
    sr: u32,
) -> Option<ac_core::shared::types::AnalysisResult> {
    let dur = duration.max(20.0 / freq.max(1.0)); // at least 20 cycles
    let _ = eng.capture_block(0.05); // brief flush
    eng.flush_capture();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let samples = eng.capture_block(dur).ok()?;
    ac_core::measurement::thd::analyze(&samples, sr, freq, 10).ok()
}

// ---------------------------------------------------------------------------
// Calibration unit helpers (display strings for test results)
// ---------------------------------------------------------------------------

pub(super) fn cal_dbu_str(dbfs: f64, cal: Option<&Calibration>, use_output: bool) -> String {
    let vrms_ref = if use_output {
        cal.and_then(|c| c.vrms_at_0dbfs_out)
    } else {
        cal.and_then(|c| c.vrms_at_0dbfs_in)
    };
    if let Some(ref_vrms) = vrms_ref {
        let vrms = ref_vrms * 10f64.powf(dbfs / 20.0);
        let dbu = ac_core::shared::conversions::vrms_to_dbu(vrms);
        format!("{dbu:+.1} dBu")
    } else {
        format!("{dbfs:.1} dBFS")
    }
}

pub(super) fn cal_out_dbu_str(dbfs: f64, cal: Option<&Calibration>) -> String {
    cal_dbu_str(dbfs, cal, true)
}

// ---------------------------------------------------------------------------
// Math utilities
// ---------------------------------------------------------------------------

pub(super) fn std_dev(vals: &[f64]) -> f64 {
    if vals.len() < 2 {
        return 0.0;
    }
    let mean = vals.iter().sum::<f64>() / vals.len() as f64;
    let var = vals.iter().map(|&x| (x - mean).powi(2)).sum::<f64>() / (vals.len() - 1) as f64;
    var.sqrt()
}

pub(super) fn median(vals: &[f64]) -> f64 {
    if vals.is_empty() {
        return 0.0;
    }
    let mut sorted = vals.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = sorted.len();
    if n.is_multiple_of(2) {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    } else {
        sorted[n / 2]
    }
}
