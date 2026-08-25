//! ZMQ REP+PUB event loop.
//!
//! The main thread owns both sockets.  Worker threads push DATA frames into a
//! `crossbeam_channel::Receiver`; the main loop drains it between REP rounds.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender};
use serde_json::{json, Value};

use crate::handlers;
use crate::workers::WorkerHandle;

/// ZMQ PUB send high-water-mark. Default libzmq HWM is 1000, which silently
/// drops terminal frames (`done`, `error`, `cal_done`) mid-sweep when the
/// Python client lags. 50_000 lets a whole freq sweep buffer in memory before
/// anything is dropped; the internal backlog log below warns the operator.
const PUB_HWM: i32 = 50_000;

/// If the worker → main-loop channel ever accumulates this many pending
/// frames between drains, log once so slow subscribers become visible.
const PUB_BACKLOG_WARN: usize = 1_000;

/// Shared server state, accessible to every handler.
#[derive(Clone)]
pub struct ServerState {
    pub cfg: Arc<Mutex<ac_core::config::Config>>,
    /// Set when the last per-request reload of `config.json` (#370, in
    /// `dispatch()`) failed — bad JSON, or a file caught mid-write. `cfg`
    /// above keeps the last-good value in this case; routing-affecting
    /// handlers check this via `cfg_guard!` and refuse rather than serve
    /// against config the daemon can no longer confirm is current.
    /// Cleared on the next reload that succeeds — self-healing, no restart
    /// needed once the file is fixed.
    pub cfg_error: Arc<Mutex<Option<String>>>,
    pub workers: Arc<Mutex<HashMap<String, WorkerHandle>>>,
    /// Worker threads → main thread → PUB socket.
    pub pub_tx: Sender<Vec<u8>>,
    pub src_mtime: f64,
    pub fake_audio: bool,
    /// Human-readable mode string for `status` / `server_connections` replies.
    pub listen_mode: Arc<Mutex<String>>,
    /// Identity fields (#385) — captured once at startup, read-only after,
    /// same lifetime as `src_mtime`. Let a client tell one running daemon
    /// apart from another: an auto-spawned daemon left over from an
    /// isolated test/rig `HOME` is otherwise indistinguishable from the
    /// operator's own, on the same hardcoded 5556/5557 ports.
    pub home: String,
    pub config_path: PathBuf,
    pub pid: u32,
    /// RFC3339, UTC, second precision — when this process started.
    pub started_at: String,
    /// `"auto"` when started by `ac-cli`'s `spawn_daemon()` (the
    /// `--auto-spawned` flag), `"manual"` for a hand-run `ac-daemon`.
    pub spawn_mode: String,
    /// Signal the main loop to rebind: send the new bind host ("*" or "127.0.0.1").
    /// The rebind happens AFTER the current CTRL reply is sent (per ZMQ.md spec).
    pub rebind_tx: Sender<String>,
    /// Ports, so handlers can report correct endpoints.
    pub ctrl_port: u16,
    pub data_port: u16,
    /// Optional channel to signal the running test_dut worker (compare-mode hand-off).
    pub dut_reply_tx: Arc<Mutex<Option<Sender<()>>>>,
    /// Optional channel to signal the running calibrate worker.
    /// Sends a [`CalReply`]: a reading, a skip (keep the stored value),
    /// or an explicit clear.
    pub cal_reply_tx: Arc<Mutex<Option<Sender<crate::handlers::CalReply>>>>,
    /// Handle to the active `transfer_stream` session's snapshot ring
    /// (handoff: snapshot-backend M1). `None` when no transfer session is
    /// running — the `snapshot` handler's "only while a transfer session
    /// runs" rule (deliverable 2) is exactly this check. Populated at
    /// worker start, cleared at stop, same lifecycle as `cal_reply_tx`.
    /// Double `Arc` so the worker thread can hold and mutate its own
    /// clone of the inner `Mutex<SnapshotRingState>` after the outer
    /// slot is read once by a CTRL handler.
    pub snapshot_ring: Arc<Mutex<Option<Arc<Mutex<crate::handlers::snapshot::SnapshotRingState>>>>>,
    /// Live stimulus state for the active `transfer_stream` session
    /// (§4.3). `None` when no transfer session runs — the `set_drive`
    /// handler's precondition is exactly this check, mirroring
    /// `snapshot_ring`'s "only while a transfer session runs" rule.
    /// Populated at worker start, cleared at stop.
    pub drive_state: Arc<Mutex<Option<Arc<crate::workers::DriveState>>>>,
    /// Session-wide re-lock request for the active `transfer_stream`
    /// session (#226). `None` when no transfer session runs — the
    /// `relock` handler's precondition is exactly this check, mirroring
    /// `drive_state`'s lifecycle. Populated at worker start, cleared at
    /// stop.
    pub relock_state: Arc<Mutex<Option<Arc<crate::workers::RelockRequest>>>>,
    /// Spooled `.acsnap` files, keyed by `id` (= the file's own sha256 —
    /// content-addressed, so identical snapshots share one spool entry
    /// and no separate ID generator/dependency is needed). Cleared at
    /// `transfer_stream` session end (deliverable 3's retention policy —
    /// see `handlers::snapshot`'s module doc for the full rationale).
    pub snapshot_spool: Arc<Mutex<HashMap<String, crate::handlers::snapshot::SpoolEntry>>>,
    /// Cached port lists. JACK port queries open a fresh probe client every
    /// call, so before this cache `test_hardware` would build 4+ probe clients
    /// per invocation just to resolve sticky port names. Populated lazily and
    /// refreshed by the `devices` command.
    pub playback_ports_cache: Arc<Mutex<Option<Vec<String>>>>,
    pub capture_ports_cache: Arc<Mutex<Option<Vec<String>>>>,
    /// Spectrum analysis mode: `"fft"` (default) or `"cwt"` (Morlet wavelet).
    /// Read by the `monitor_spectrum` worker on each tick so toggling it via
    /// `set_analysis_mode` takes effect on the next published frame.
    pub analysis_mode: Arc<Mutex<String>>,
    pub cwt_sigma: Arc<Mutex<f32>>,
    pub cwt_n_scales: Arc<Mutex<usize>>,
    /// 1/N-octave aggregation of the CWT column. `None` = disabled (no
    /// extra frame published). `Some(N)` = publish a `type:
    /// "fractional_octave"` frame after each CWT frame with `bpo = N`.
    /// Read every tick by the `monitor_spectrum` worker so `set_ioct_bpo`
    /// takes effect live.
    pub ioct_bpo: Arc<Mutex<Option<u32>>>,
    /// Frequency-weighting curve applied to each band level before the
    /// `fractional_octave` / `fractional_octave_leq` frames are emitted.
    /// One of `"off"` (identity), `"a"`, `"c"`, `"z"`. `"off"` and
    /// `"z"` are functionally identical; both are accepted so the
    /// wire protocol mirrors UI affordances (`off` key state vs `Z`
    /// mode tag). Toggled via `set_band_weighting` and read every tick
    /// by the monitor worker — no restart required.
    pub band_weighting: Arc<Mutex<String>>,
    /// Time-integration mode applied to each `fractional_octave` frame. One of
    /// `"off"`, `"fast"`, `"slow"`, `"leq"`. When non-`off`, the worker
    /// publishes an additional `fractional_octave_leq` frame per channel
    /// carrying the integrated levels and the mode label. Toggled via
    /// `set_time_integration`; Leq can be zeroed live via `reset_leq`.
    pub time_integration_mode: Arc<Mutex<String>>,
    /// One-shot flag set by `reset_leq` and consumed by the monitor worker
    /// on its next tick. Applies to the Leq integrators; fast/slow modes
    /// ignore it (they'd re-prime from the next input anyway).
    pub leq_reset_request: Arc<std::sync::atomic::AtomicBool>,
    /// One-shot flag set by `reset_loudness` and consumed by the monitor
    /// worker on its next tick. Clears per-channel LKFS-I / LRA / dBTP
    /// accumulators without restarting the monitor.
    pub loudness_reset_request: Arc<std::sync::atomic::AtomicBool>,
    /// Process-wide toggle for daemon-side mic-curve correction. Defaults
    /// to `true` so a freshly loaded curve takes effect immediately;
    /// flipped via `set_mic_correction_enabled`. Per-channel curves are
    /// still respected — this just gates whether they're applied.
    pub mic_correction_enabled: Arc<std::sync::atomic::AtomicBool>,
    /// Live-tunable parameters for the `monitor_spectrum` FFT path. The worker
    /// re-reads these every tick so `set_monitor_params` takes effect without
    /// a restart. `active` flips true on worker spawn and false on exit;
    /// `set_monitor_params` uses it to reject changes when no monitor runs.
    pub monitor_params: Arc<Mutex<MonitorParams>>,
}

/// Live-tunable parameters for the FFT spectrum monitor.
#[derive(Clone, Copy, Debug)]
pub struct MonitorParams {
    /// Tick cadence in seconds (refresh rate). Worker sleeps after publishing
    /// each cycle to reach this cadence; capture never stretches to fill it.
    pub interval: f64,
    /// FFT window length in samples. Must be a power of 2 in [256, 131072].
    pub fft_n: u32,
    /// Low-frequency FFT window length for the dual-resolution path (#142).
    /// A second, longer FFT over the same ring drives the spectrum below
    /// `crossover_hz`; the live `fft_n` keeps driving everything above it.
    /// The LF band is inactive whenever `fft_n >= lf_fft_n`.
    pub lf_fft_n: u32,
    /// Crossover (Hz) splitting the LF long-FFT band from the HF live band.
    /// Daemon-owned constant, echoed to the UI so labels never hardcode it.
    pub crossover_hz: f32,
    /// `monitor_spectrum` worker is running.
    pub active: bool,
}

impl Default for MonitorParams {
    fn default() -> Self {
        // 8192 @ 48 kHz ≈ 5.86 Hz bin spacing — close to legacy 0.2 s × 48 k
        // = 9600 samples (≈ 5 Hz) while being a clean pow2 for the planner.
        // LF N=65536 @ 48 kHz ≈ 0.73 Hz, enough to split 5 Hz tones < 100 Hz
        // (block latency ≈ 1.37 s, applied to the LF band only — #142).
        Self {
            interval: 0.2,
            fft_n: 8192,
            lf_fft_n: 65536,
            crossover_hz: ac_core::visualize::aggregate::DEFAULT_LF_CROSSOVER_HZ,
            active: false,
        }
    }
}

pub fn run(
    ctrl_port: u16,
    data_port: u16,
    local_only: bool,
    fake_audio: bool,
    auto_spawned: bool,
) -> Result<()> {
    let ctx = zmq::Context::new();

    let ctrl = ctx.socket(zmq::REP).context("CTRL socket")?;
    let data = ctx.socket(zmq::PUB).context("DATA socket")?;
    data.set_sndhwm(PUB_HWM).context("set PUB sndhwm")?;

    let mut bind_host = if local_only { "127.0.0.1" } else { "*" }.to_string();
    if let Err(e) = ctrl.bind(&format!("tcp://{bind_host}:{ctrl_port}")) {
        report_bind_conflict(ctrl_port);
        return Err(e).with_context(|| format!("bind CTRL tcp://{bind_host}:{ctrl_port}"));
    }
    if let Err(e) = data.bind(&format!("tcp://{bind_host}:{data_port}")) {
        // Not `report_bind_conflict(ctrl_port)`: CTRL already bound *in this
        // process* two lines above, so probing `ctrl_port` here would reach
        // our own not-yet-serving REP socket and time out — misreporting a
        // self-probe timeout as "could not identify existing listener" when
        // no incumbent was ever queried. DATA is a PUB socket with no
        // `status` responder, so whoever holds `data_port` genuinely can't
        // be identified this way; say why instead of sounding like a failed
        // probe (#385 / PR #396 QA correctness #2).
        eprintln!(
            "ac-daemon: DATA port already in use — cannot identify the \
             existing listener (PUB sockets don't answer status queries)"
        );
        return Err(e).with_context(|| format!("bind DATA tcp://{bind_host}:{data_port}"));
    }

    eprintln!("ac-daemon: CTRL tcp://{bind_host}:{ctrl_port}  DATA tcp://{bind_host}:{data_port}");

    let (pub_tx, pub_rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = crossbeam_channel::unbounded();
    let (rebind_tx, rebind_rx): (Sender<String>, Receiver<String>) = crossbeam_channel::unbounded();

    let cfg = ac_core::config::load(None).unwrap_or_default();
    let listen_mode = if local_only { "local" } else { "public" }.to_string();

    // Identity (#385) — captured once, same lifetime as `src_mtime`.
    // `$HOME` unset mirrors `ac_core::config::default_config_path`'s own
    // fallback: degrade to "." rather than panicking or leaving it absent.
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let config_path = ac_core::config::default_config_path();
    let pid = std::process::id();
    let started_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let spawn_mode = if auto_spawned { "auto" } else { "manual" }.to_string();

    let state = ServerState {
        cfg: Arc::new(Mutex::new(cfg)),
        cfg_error: Arc::new(Mutex::new(None)),
        workers: Arc::new(Mutex::new(HashMap::new())),
        pub_tx,
        src_mtime: crate::binary_mtime(),
        fake_audio,
        listen_mode: Arc::new(Mutex::new(listen_mode)),
        home,
        config_path,
        pid,
        started_at,
        spawn_mode,
        rebind_tx,
        ctrl_port,
        data_port,
        dut_reply_tx: Arc::new(Mutex::new(None)),
        cal_reply_tx: Arc::new(Mutex::new(None)),
        snapshot_ring: Arc::new(Mutex::new(None)),
        drive_state: Arc::new(Mutex::new(None)),
        relock_state: Arc::new(Mutex::new(None)),
        snapshot_spool: Arc::new(Mutex::new(HashMap::new())),
        playback_ports_cache: Arc::new(Mutex::new(None)),
        capture_ports_cache: Arc::new(Mutex::new(None)),
        analysis_mode: Arc::new(Mutex::new("fft".to_string())),
        cwt_sigma: Arc::new(Mutex::new(ac_core::visualize::cwt::DEFAULT_SIGMA)),
        cwt_n_scales: Arc::new(Mutex::new(ac_core::visualize::cwt::DEFAULT_N_SCALES)),
        ioct_bpo: Arc::new(Mutex::new(None)),
        band_weighting: Arc::new(Mutex::new("off".to_string())),
        time_integration_mode: Arc::new(Mutex::new("off".to_string())),
        leq_reset_request: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        loudness_reset_request: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        mic_correction_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        monitor_params: Arc::new(Mutex::new(MonitorParams::default())),
    };

    let mut items = [ctrl.as_poll_item(zmq::POLLIN)];
    let mut backlog_warned = false;
    // Keepalive cadence — clients use the monotonically-increasing `seq`
    // to detect a stalled or restarted daemon. 1 Hz is plenty and costs
    // one tiny PUB frame per second.
    let keepalive_interval = std::time::Duration::from_secs(1);
    let mut last_keepalive = std::time::Instant::now();
    let mut keepalive_seq: u64 = 0;
    // Idle-timeout tracking for auto-disable of public bind. Updated on every
    // CTRL recv; the keepalive tick checks elapsed vs. configured timeout.
    let mut last_ctrl_activity = std::time::Instant::now();

    loop {
        // Drain any pending DATA frames first
        if pub_rx.len() > PUB_BACKLOG_WARN && !backlog_warned {
            eprintln!(
                "ac-daemon: PUB backlog {} pending frames — subscriber is lagging",
                pub_rx.len()
            );
            backlog_warned = true;
        } else if pub_rx.is_empty() {
            backlog_warned = false;
        }
        while let Ok(frame) = pub_rx.try_recv() {
            data.send(frame, 0).ok();
        }

        // Reap finished workers
        {
            let mut workers = state.workers.lock().unwrap();
            workers.retain(|_, w| match &w.thread {
                Some(t) => !t.is_finished(),
                None => false,
            });
        }

        if last_keepalive.elapsed() >= keepalive_interval {
            last_keepalive = std::time::Instant::now();
            keepalive_seq = keepalive_seq.wrapping_add(1);
            let ts_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            let busy = !state.workers.lock().unwrap().is_empty();
            let payload = serde_json::to_string(&json!({
                "type":      "keepalive",
                "seq":       keepalive_seq,
                "timestamp": ts_ns,
                "busy":      busy,
            }))
            .unwrap_or_else(|_| "{}".to_string());
            let frame = format!("keepalive {payload}").into_bytes();
            data.send(frame, 0).ok();

            // Idle-timeout: while public-bound and not running any worker,
            // auto-fold back to localhost if we've been silent long enough.
            // Queues a rebind via the same channel server_disable uses; the
            // actual socket rebind happens on the next CTRL round.
            if !busy {
                let timeout_secs = state.cfg.lock().unwrap().server_idle_timeout_secs;
                let is_public = state.listen_mode.lock().unwrap().as_str() == "public";
                if is_public {
                    if let Some(secs) = timeout_secs {
                        if last_ctrl_activity.elapsed() >= std::time::Duration::from_secs(secs) {
                            eprintln!(
                                "ac-daemon: idle timeout {secs}s reached — auto-disabling public bind"
                            );
                            handlers::server_disable(&state);
                            last_ctrl_activity = std::time::Instant::now();
                        }
                    }
                }
            }
        }

        zmq::poll(&mut items, 10).ok(); // 10 ms timeout

        if items[0].is_readable() {
            let msg = ctrl.recv_bytes(0).context("CTRL recv")?;
            last_ctrl_activity = std::time::Instant::now();
            let reply = dispatch(&msg, &state, &pub_rx, &data);
            let reply_bytes = serde_json::to_vec(&reply).unwrap_or_else(|_| b"{}".to_vec());

            let should_quit = reply.get("_quit").and_then(Value::as_bool).unwrap_or(false);

            // Flush DATA frames that arrived during dispatch
            while let Ok(frame) = pub_rx.try_recv() {
                data.send(frame, 0).ok();
            }

            ctrl.send(reply_bytes, 0).context("CTRL send")?;

            if should_quit {
                eprintln!("ac-daemon: quit received, shutting down");
                break;
            }

            // Rebind AFTER the reply is sent (per ZMQ.md spec)
            apply_pending_rebind(
                &rebind_rx,
                &ctrl,
                &data,
                &mut bind_host,
                ctrl_port,
                data_port,
            );
        }

        // Also drain rebinds scheduled outside the CTRL round — e.g. when the
        // keepalive tick auto-disables the public bind on idle timeout. Without
        // this, the rebind would sit in the channel until the next client
        // connected and leave the public socket live in the meantime.
        apply_pending_rebind(
            &rebind_rx,
            &ctrl,
            &data,
            &mut bind_host,
            ctrl_port,
            data_port,
        );
    }

    Ok(())
}

/// On a CTRL bind failure (port already in use), probe whatever is already
/// listening on `ctrl_port` via a short-lived `status` request and print its
/// identity to stderr before this process gives up — or admit we couldn't
/// identify it, rather than guessing (#385). 500 ms timeout, matching
/// `ac-cli/src/spawn.rs`'s `wait_for_server` per-attempt timeout.
///
/// Not called on a DATA-only bind failure: at that point CTRL has already
/// bound successfully in this process, so probing `ctrl_port` would reach
/// our own socket, not an incumbent's — see the DATA-bind-failure arm in
/// `run()` for the honest message used there instead (#396 QA correctness
/// #2).
fn report_bind_conflict(ctrl_port: u16) {
    let cant_identify = || {
        eprintln!("ac-daemon: could not identify existing listener (no response to status query)");
    };

    let ctx = zmq::Context::new();
    let sock = match ctx.socket(zmq::REQ) {
        Ok(s) => s,
        Err(_) => return cant_identify(),
    };
    sock.set_linger(0).ok();
    sock.set_rcvtimeo(500).ok();
    sock.set_sndtimeo(500).ok();
    let addr = format!("tcp://127.0.0.1:{ctrl_port}");
    if sock.connect(&addr).is_err() || sock.send(br#"{"cmd":"status"}"#.as_ref(), 0).is_err() {
        return cant_identify();
    }
    let Ok(bytes) = sock.recv_bytes(0) else {
        return cant_identify();
    };
    let Ok(reply) = serde_json::from_slice::<Value>(&bytes) else {
        return cant_identify();
    };
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        return cant_identify();
    }
    let home = reply.get("home").and_then(Value::as_str).unwrap_or("?");
    let pid = reply.get("pid").and_then(Value::as_u64).unwrap_or(0);
    let spawn_label = match reply.get("spawn_mode").and_then(Value::as_str) {
        Some("auto") => "auto-spawned",
        Some("manual") => "manual",
        _ => "?",
    };
    let started_at = reply
        .get("started_at")
        .and_then(Value::as_str)
        .unwrap_or("?");
    eprintln!("ac-daemon: existing listener: home {home}, pid {pid}, {spawn_label} {started_at}");
}

fn apply_pending_rebind(
    rebind_rx: &Receiver<String>,
    ctrl: &zmq::Socket,
    data: &zmq::Socket,
    bind_host: &mut String,
    ctrl_port: u16,
    data_port: u16,
) {
    while let Ok(new_host) = rebind_rx.try_recv() {
        if new_host == *bind_host {
            continue;
        }
        let old_ctrl = format!("tcp://{bind_host}:{ctrl_port}");
        let old_data = format!("tcp://{bind_host}:{data_port}");
        let new_ctrl = format!("tcp://{new_host}:{ctrl_port}");
        let new_data = format!("tcp://{new_host}:{data_port}");

        ctrl.unbind(&old_ctrl).ok();
        data.unbind(&old_data).ok();

        std::thread::sleep(std::time::Duration::from_millis(150));

        match (ctrl.bind(&new_ctrl), data.bind(&new_data)) {
            (Ok(_), Ok(_)) => {
                eprintln!("ac-daemon: rebound → CTRL {new_ctrl}  DATA {new_data}");
                *bind_host = new_host;
            }
            (Err(e), _) => eprintln!("ac-daemon: rebind CTRL {new_ctrl}: {e}"),
            (_, Err(e)) => eprintln!("ac-daemon: rebind DATA {new_data}: {e}"),
        }
    }
}

fn dispatch(
    raw: &[u8],
    state: &ServerState,
    pub_rx: &Receiver<Vec<u8>>,
    data_sock: &zmq::Socket,
) -> Value {
    while let Ok(frame) = pub_rx.try_recv() {
        data_sock.send(frame, 0).ok();
    }

    // #370: reload config.json on every request, before it reaches a
    // handler. An auto-spawned daemon outlives the `ac` invocation that
    // spawned it, so without this a config edit made between two `ac`
    // commands never reaches an already-running daemon — it keeps serving
    // the config it started with, silently. Routing fields are already
    // re-resolved per request via `resolve_input`/`resolve_output`, so
    // reloading here completes that design rather than fighting it.
    // Single-threaded REP loop: no race with a handler's `cfg.lock().clone()`.
    match ac_core::config::load(None) {
        Ok(cfg) => {
            *state.cfg.lock().unwrap() = cfg;
            *state.cfg_error.lock().unwrap() = None;
        }
        Err(e) => {
            // Keep the last-good in-memory config; record the failure so
            // routing-affecting handlers can refuse via `cfg_guard!`
            // instead of silently serving against config.json's last-known
            // state. `status`/`quit`/etc. are unaffected — the operator can
            // still reach the daemon to find out what's wrong.
            *state.cfg_error.lock().unwrap() = Some(format!("{e:#}"));
        }
    }

    let cmd: Value = match serde_json::from_slice(raw) {
        Ok(v) => v,
        Err(_) => return json!({"ok": false, "error": "invalid JSON"}),
    };

    let name = match cmd.get("cmd").and_then(Value::as_str) {
        Some(n) => n,
        None => return json!({"ok": false, "error": "missing 'cmd' field"}),
    };

    match name {
        "status" => handlers::status(state),
        "quit" => handlers::quit(state),
        "stop" => handlers::stop(state, &cmd),
        "devices" => handlers::devices(state),
        "setup" => handlers::setup(state, &cmd),
        "get_calibration" => handlers::get_calibration(state, &cmd),
        "list_calibrations" => handlers::list_calibrations(state),
        "sweep_level" => handlers::sweep_level(state, &cmd),
        "sweep_frequency" => handlers::sweep_frequency(state, &cmd),
        "plot" => handlers::plot(state, &cmd),
        "plot_level" => handlers::plot_level(state, &cmd),
        "plot_ir" => handlers::plot_ir(state, &cmd),
        "monitor_spectrum" => handlers::monitor_spectrum(state, &cmd),
        "set_analysis_mode" => handlers::set_analysis_mode(state, &cmd),
        "get_analysis_mode" => handlers::get_analysis_mode(state),
        "set_ioct_bpo" => handlers::set_ioct_bpo(state, &cmd),
        "set_band_weighting" => handlers::set_band_weighting(state, &cmd),
        "get_band_weighting" => handlers::get_band_weighting(state),
        "set_time_integration" => handlers::set_time_integration(state, &cmd),
        "get_time_integration" => handlers::get_time_integration(state),
        "reset_leq" => handlers::reset_leq(state),
        "reset_loudness" => handlers::reset_loudness(state),
        "set_monitor_params" => handlers::set_monitor_params(state, &cmd),
        "generate" => handlers::generate(state, &cmd),
        "generate_pink" => handlers::generate_pink(state, &cmd),
        "calibrate" => handlers::calibrate(state, &cmd),
        "calibrate_spl" => handlers::calibrate_spl(state, &cmd),
        "calibrate_mic_curve" => handlers::calibrate_mic_curve(state, &cmd),
        "set_mic_correction_enabled" => handlers::set_mic_correction_enabled(state, &cmd),
        "cal_reply" => handlers::cal_reply(state, &cmd),
        "dmm_read" => handlers::dmm_read(state),
        "server_enable" => handlers::server_enable(state),
        "server_disable" => handlers::server_disable(state),
        "server_connections" => handlers::server_connections(state),
        "transfer_stream" => handlers::transfer_stream(state, &cmd),
        "set_drive" => handlers::set_drive(state, &cmd),
        "relock" => handlers::relock(state, &cmd),
        "snapshot" => handlers::snapshot(state, &cmd),
        "snapshot_fetch" => handlers::snapshot_fetch(state, &cmd),
        "snapshot_list" => handlers::snapshot_list(state, &cmd),
        "snapshot_delete" => handlers::snapshot_delete(state, &cmd),
        "probe" => handlers::probe(state, &cmd),
        "test_software" => handlers::test_software(state),
        "test_hardware" => handlers::test_hardware(state, &cmd),
        "test_dut" => handlers::test_dut(state, &cmd),
        "dut_reply" => handlers::dut_reply(state),
        other => json!({"ok": false, "error": format!("unknown command: '{other}'")}),
    }
}
