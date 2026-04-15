//! ZMQ REP+PUB event loop.
//!
//! The main thread owns both sockets.  Worker threads push DATA frames into a
//! `crossbeam_channel::Receiver`; the main loop drains it between REP rounds.

use std::collections::HashMap;
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
    pub cfg:         Arc<Mutex<ac_core::config::Config>>,
    pub workers:     Arc<Mutex<HashMap<String, WorkerHandle>>>,
    /// Worker threads → main thread → PUB socket.
    pub pub_tx:      Sender<Vec<u8>>,
    pub src_mtime:   f64,
    pub fake_audio:  bool,
    /// Human-readable mode string for `status` / `server_connections` replies.
    pub listen_mode: Arc<Mutex<String>>,
    /// Signal the main loop to rebind: send the new bind host ("*" or "127.0.0.1").
    /// The rebind happens AFTER the current CTRL reply is sent (per ZMQ.md spec).
    pub rebind_tx:   Sender<String>,
    /// Ports, so handlers can report correct endpoints.
    pub ctrl_port:   u16,
    pub data_port:   u16,
    /// Optional channel to signal the running test_dut worker (compare-mode hand-off).
    pub dut_reply_tx: Arc<Mutex<Option<Sender<()>>>>,
    /// Optional channel to signal the running calibrate worker.
    /// Sends Option<f64>: Some(vrms) = user reading, None = skip.
    pub cal_reply_tx: Arc<Mutex<Option<Sender<Option<f64>>>>>,
    /// Cached port lists. JACK port queries open a fresh probe client every
    /// call, so before this cache `test_hardware` would build 4+ probe clients
    /// per invocation just to resolve sticky port names. Populated lazily and
    /// refreshed by the `devices` command.
    pub playback_ports_cache: Arc<Mutex<Option<Vec<String>>>>,
    pub capture_ports_cache:  Arc<Mutex<Option<Vec<String>>>>,
}

pub fn run(ctrl_port: u16, data_port: u16, local_only: bool, fake_audio: bool) -> Result<()> {
    let ctx = zmq::Context::new();

    let ctrl = ctx.socket(zmq::REP).context("CTRL socket")?;
    let data = ctx.socket(zmq::PUB).context("DATA socket")?;
    data.set_sndhwm(PUB_HWM).context("set PUB sndhwm")?;

    let mut bind_host = if local_only { "127.0.0.1" } else { "*" }.to_string();
    ctrl.bind(&format!("tcp://{bind_host}:{ctrl_port}"))
        .with_context(|| format!("bind CTRL tcp://{bind_host}:{ctrl_port}"))?;
    data.bind(&format!("tcp://{bind_host}:{data_port}"))
        .with_context(|| format!("bind DATA tcp://{bind_host}:{data_port}"))?;

    eprintln!("ac-daemon: CTRL tcp://{bind_host}:{ctrl_port}  DATA tcp://{bind_host}:{data_port}");

    let (pub_tx,    pub_rx):    (Sender<Vec<u8>>, Receiver<Vec<u8>>) = crossbeam_channel::unbounded();
    let (rebind_tx, rebind_rx): (Sender<String>,  Receiver<String>)  = crossbeam_channel::unbounded();

    let cfg = ac_core::config::load(None).unwrap_or_default();
    let listen_mode = if local_only { "local" } else { "public" }.to_string();

    let state = ServerState {
        cfg:          Arc::new(Mutex::new(cfg)),
        workers:      Arc::new(Mutex::new(HashMap::new())),
        pub_tx,
        src_mtime:    crate::binary_mtime(),
        fake_audio,
        listen_mode:  Arc::new(Mutex::new(listen_mode)),
        rebind_tx,
        ctrl_port,
        data_port,
        dut_reply_tx: Arc::new(Mutex::new(None)),
        cal_reply_tx: Arc::new(Mutex::new(None)),
        playback_ports_cache: Arc::new(Mutex::new(None)),
        capture_ports_cache:  Arc::new(Mutex::new(None)),
    };

    let mut items = [ctrl.as_poll_item(zmq::POLLIN)];
    let mut backlog_warned = false;

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
            workers.retain(|_, w| {
                match &w.thread {
                    Some(t) => !t.is_finished(),
                    None    => false,
                }
            });
        }

        zmq::poll(&mut items, 10).ok(); // 10 ms timeout

        if items[0].is_readable() {
            let msg = ctrl.recv_bytes(0).context("CTRL recv")?;
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
            if let Ok(new_host) = rebind_rx.try_recv() {
                if new_host != bind_host {
                    let old_ctrl = format!("tcp://{bind_host}:{ctrl_port}");
                    let old_data = format!("tcp://{bind_host}:{data_port}");
                    let new_ctrl = format!("tcp://{new_host}:{ctrl_port}");
                    let new_data = format!("tcp://{new_host}:{data_port}");

                    ctrl.unbind(&old_ctrl).ok();
                    data.unbind(&old_data).ok();

                    // Give the OS a moment to release the ports before rebinding.
                    std::thread::sleep(std::time::Duration::from_millis(150));

                    match (ctrl.bind(&new_ctrl), data.bind(&new_data)) {
                        (Ok(_), Ok(_)) => {
                            eprintln!("ac-daemon: rebound → CTRL {new_ctrl}  DATA {new_data}");
                            bind_host = new_host;
                        }
                        (Err(e), _) => eprintln!("ac-daemon: rebind CTRL {new_ctrl}: {e}"),
                        (_, Err(e)) => eprintln!("ac-daemon: rebind DATA {new_data}: {e}"),
                    }
                }
            }
        }
    }

    Ok(())
}

fn dispatch(raw: &[u8], state: &ServerState, pub_rx: &Receiver<Vec<u8>>, data_sock: &zmq::Socket) -> Value {
    while let Ok(frame) = pub_rx.try_recv() {
        data_sock.send(frame, 0).ok();
    }

    let cmd: Value = match serde_json::from_slice(raw) {
        Ok(v)  => v,
        Err(_) => return json!({"ok": false, "error": "invalid JSON"}),
    };

    let name = match cmd.get("cmd").and_then(Value::as_str) {
        Some(n) => n,
        None    => return json!({"ok": false, "error": "missing 'cmd' field"}),
    };

    match name {
        "status"              => handlers::status(state),
        "quit"                => handlers::quit(state),
        "stop"                => handlers::stop(state, &cmd),
        "devices"             => handlers::devices(state),
        "setup"               => handlers::setup(state, &cmd),
        "get_calibration"     => handlers::get_calibration(state, &cmd),
        "list_calibrations"   => handlers::list_calibrations(state),
        "sweep_level"         => handlers::sweep_level(state, &cmd),
        "sweep_frequency"     => handlers::sweep_frequency(state, &cmd),
        "plot"                => handlers::plot(state, &cmd),
        "plot_level"          => handlers::plot_level(state, &cmd),
        "monitor_spectrum"    => handlers::monitor_spectrum(state, &cmd),
        "generate"            => handlers::generate(state, &cmd),
        "generate_pink"       => handlers::generate_pink(state, &cmd),
        "calibrate"           => handlers::calibrate(state, &cmd),
        "cal_reply"           => handlers::cal_reply(state, &cmd),
        "dmm_read"            => handlers::dmm_read(state),
        "server_enable"       => handlers::server_enable(state),
        "server_disable"      => handlers::server_disable(state),
        "server_connections"  => handlers::server_connections(state),
        "transfer"            => handlers::transfer(state, &cmd),
        "transfer_stream"     => handlers::transfer_stream(state, &cmd),
        "probe"               => handlers::probe(state, &cmd),
        "test_hardware"       => handlers::test_hardware(state, &cmd),
        "test_dut"            => handlers::test_dut(state, &cmd),
        "dut_reply"           => handlers::dut_reply(state),
        other => json!({"ok": false, "error": format!("unknown command: '{other}'")}),
    }
}
