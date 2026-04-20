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

use ac_core::calibration::Calibration;
use ac_core::config::Config;

use crate::audio::{make_engine, AudioEngine};
use crate::server::ServerState;
use crate::workers::{cmd_group, Group, WorkerHandle};

mod admin;
mod audio;
mod calibrate;
mod test_dut;
mod test_hw;
mod transfer;

pub use admin::{
    devices, dmm_read, get_analysis_mode, get_calibration, list_calibrations,
    quit, server_connections, server_disable, server_enable,
    set_analysis_mode, set_monitor_params, setup, status, stop, tuner_config,
    tuner_range,
};
pub use audio::{
    generate, generate_pink, monitor_spectrum, plot, plot_level,
    sweep_frequency, sweep_level,
};
pub use calibrate::{cal_reply, calibrate};
pub use test_dut::{dut_reply, test_dut};
pub use test_hw::test_hardware;
pub use transfer::{probe, transfer_stream};

// ---------------------------------------------------------------------------
// Busy guard
// ---------------------------------------------------------------------------

/// Check the busy guard and return Err string if a conflict exists.
pub(super) fn check_busy(state: &ServerState, new_cmd: &str) -> Option<String> {
    let new_group = match cmd_group(new_cmd) {
        Some(g) => g,
        None    => return None, // non-audio command, always allowed
    };

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
    WorkerHandle { stop_flag: stop, thread: Some(t) }
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
    *state.capture_ports_cache .lock().unwrap() = Some(eng.capture_ports());
}

// ---------------------------------------------------------------------------
// Port resolution
// ---------------------------------------------------------------------------

/// Resolve output port: config sticky name, or fall back to channel index in engine list.
pub(super) fn resolve_output(cfg: &Config, state: &ServerState) -> String {
    if let Some(p) = &cfg.output_port {
        return p.clone();
    }
    let ports = cached_playback_ports(state);
    let ch = cfg.output_channel as usize;
    ports.get(ch).cloned().unwrap_or_else(|| "system:playback_1".to_string())
}

/// Resolve input port: config sticky name, or fall back to channel index in engine list.
pub(super) fn resolve_input(cfg: &Config, state: &ServerState) -> String {
    if let Some(p) = &cfg.input_port {
        return p.clone();
    }
    let ports = cached_capture_ports(state);
    let ch = cfg.input_channel as usize;
    ports.get(ch).cloned().unwrap_or_else(|| "system:capture_1".to_string())
}

pub(super) fn resolve_ref_input(cfg: &Config, state: &ServerState) -> Option<String> {
    let ch = cfg.reference_channel? as usize;
    if let Some(p) = &cfg.reference_port {
        return Some(p.clone());
    }
    cached_capture_ports(state).get(ch).cloned()
}

pub(super) fn resolve_ref_output(cfg: &Config, state: &ServerState) -> String {
    if let Some(ch) = cfg.reference_channel {
        let ports = cached_playback_ports(state);
        if let Some(p) = ports.get(ch as usize) {
            return p.clone();
        }
    }
    resolve_output(cfg, state)
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
    let n_points  = (n_decades * ppd as f64).round() as usize;
    let n_points  = n_points.max(2);
    let mut freqs: Vec<f64> = (0..n_points)
        .map(|i| start * (stop / start).powf(i as f64 / (n_points - 1) as f64))
        .collect();
    freqs.dedup_by(|a, b| (*a as u64) == (*b as u64));
    freqs
}

pub(super) fn linspace(start: f64, stop: f64, n: usize) -> Vec<f64> {
    if n <= 1 { return vec![start]; }
    (0..n).map(|i| start + (stop - start) * i as f64 / (n - 1) as f64).collect()
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

pub(super) fn sweep_point_frame(
    r: &ac_core::types::AnalysisResult,
    cal: Option<&Calibration>,
    n: usize,
    cmd_name: &str,
    level_dbfs: f64,
    freq_hz: Option<f64>,
) -> Value {
    let out_vrms = cal.and_then(|c| c.out_vrms(level_dbfs));
    let in_vrms  = cal.and_then(|c| c.in_vrms(r.linear_rms));
    let in_dbu   = in_vrms .map(ac_core::conversions::vrms_to_dbu);
    let out_dbu  = out_vrms.map(ac_core::conversions::vrms_to_dbu);
    let gain_db  = in_dbu.zip(out_dbu).map(|(i, o)| i - o);

    let (spec_ds, freqs_ds) = if r.spectrum.len() > 1 {
        downsample(&r.spectrum[1..], &r.freqs[1..], 1000)
    } else {
        (r.spectrum.clone(), r.freqs.clone())
    };

    let harmonic_levels: Vec<Value> = r.harmonic_levels.iter()
        .map(|&(hz, amp)| json!([hz, amp]))
        .collect();

    let mut frame = json!({
        "type":              "sweep_point",
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
        ).ok()?;
        stream.write_all(b"MEAS:VOLT:AC?\n").ok()?;
        let mut buf = [0u8; 64];
        let bytes = stream.read(&mut buf).ok()?;
        let s = std::str::from_utf8(&buf[..bytes]).ok()?.trim().to_string();
        if let Ok(v) = s.parse::<f64>() {
            sum += v;
            count += 1;
        }
    }
    if count > 0 { Some(sum / count as f64) } else { None }
}

/// Poll a cal_reply channel until a value arrives, stop flag fires, or timeout.
pub(super) fn wait_cal_reply(
    rx:           &crossbeam_channel::Receiver<Option<f64>>,
    stop:         &Arc<AtomicBool>,
    timeout_secs: u64,
) -> Option<f64> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        if stop.load(Ordering::Relaxed) { return None; }
        if std::time::Instant::now() > deadline { return None; }
        if let Ok(v) = rx.try_recv() { return v; }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------------------
// Test result + capture helpers (shared by test_hw and test_dut)
// ---------------------------------------------------------------------------

pub(super) struct TestResult {
    pub name:      String,
    pub pass:      bool,
    pub detail:    String,
    pub tolerance: String,
}

impl TestResult {
    pub(super) fn new(name: &str, pass: bool, detail: String, tolerance: &str) -> Self {
        Self { name: name.to_string(), pass, detail, tolerance: tolerance.to_string() }
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
) -> Option<ac_core::types::AnalysisResult> {
    let dur = duration.max(20.0 / freq.max(1.0)); // at least 20 cycles
    let _ = eng.capture_block(0.05); // brief flush
    eng.flush_capture();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let samples = eng.capture_block(dur).ok()?;
    ac_core::analysis::analyze(&samples, sr, freq, 10).ok()
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
        let dbu  = ac_core::conversions::vrms_to_dbu(vrms);
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
    if vals.len() < 2 { return 0.0; }
    let mean = vals.iter().sum::<f64>() / vals.len() as f64;
    let var  = vals.iter().map(|&x| (x - mean).powi(2)).sum::<f64>() / (vals.len() - 1) as f64;
    var.sqrt()
}

pub(super) fn median(vals: &[f64]) -> f64 {
    if vals.is_empty() { return 0.0; }
    let mut sorted = vals.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = sorted.len();
    if n % 2 == 0 {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    } else {
        sorted[n / 2]
    }
}
