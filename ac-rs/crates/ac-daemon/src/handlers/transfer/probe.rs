//! `probe` — the channel-discovery sweep. Shares this module tree with
//! `transfer_stream` because both need engine port routing, and nothing
//! else.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::shared::emission_level::{MAX_EMISSION_DBFS, SELF_TEST_LEVEL_DBFS};

use crate::handlers::{
    busy_guard, check_emission_or_refuse, make_engine_for_state, read_dmm_vrms, send_pub,
    spawn_worker,
};
use crate::server::ServerState;

const LOOPBACK_GATE_DBFS: f64 = SELF_TEST_LEVEL_DBFS - 20.0;

fn output_result_frame(
    channel: usize,
    port: &str,
    vrms: Option<f64>,
    analog: bool,
    backend: &str,
) -> Value {
    json!({
        "cmd": "probe", "phase": "output",
        "channel": channel, "port": port,
        "vrms": vrms, "analog": analog,
        "backend": backend,
    })
}

pub fn probe(state: &ServerState, _cmd: &Value) -> Value {
    busy_guard!(state, "probe");
    let cfg = state.cfg.lock().unwrap().clone();
    if let Err(reply) = check_emission_or_refuse(state, &cfg, SELF_TEST_LEVEL_DBFS) {
        return reply;
    }

    let mut eng = match make_engine_for_state(state) {
        Ok(eng) => eng,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let backend = eng.backend_name();
    let pub_tx = state.pub_tx.clone();
    let dmm_host = cfg.dmm_host.clone();

    let (playback, capture) = (
        crate::handlers::cached_playback_ports(state),
        crate::handlers::cached_capture_ports(state),
    );
    let n_play = playback.len();
    let n_cap = capture.len();

    let worker = spawn_worker(state, "probe", move |stop| {
        let threshold_rms: f64 = 0.010 / (2.0f64.sqrt()); // 10 mVrms ≈ this linear RMS

        let freq = 1000.0;
        let amplitude = ac_core::shared::generator::dbfs_to_amplitude(SELF_TEST_LEVEL_DBFS);

        if !eng.supports_routing() {
            send_pub(
                &pub_tx,
                "error",
                &json!({
                    "cmd":     "probe",
                    "message": format!("{} backend does not support port routing", eng.backend_name()),
                }),
            );
            return;
        }
        if playback.is_empty() {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"probe","message":"no playback ports"}),
            );
            return;
        }

        if let Err(e) = eng.start(&[playback[0].clone()], None) {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"probe","message":format!("{e}")}),
            );
            return;
        }
        eng.set_tone(freq, amplitude);
        eng.disconnect_output(&playback[0]);

        // Phase 1: DMM output scan
        let mut analog_channels: Vec<usize> = Vec::new();
        if let Some(ref host) = dmm_host {
            send_pub(
                &pub_tx,
                "data",
                &json!({
                    "cmd": "probe", "phase": "output_start", "n_ports": n_play
                    , "backend": backend
                }),
            );
            for (i, port) in playback.iter().enumerate() {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                eng.connect_output(port).ok();
                std::thread::sleep(std::time::Duration::from_millis(400));
                let vrms = read_dmm_vrms(host, 3);
                eng.disconnect_output(port);
                let is_analog = vrms.map(|v| v > threshold_rms).unwrap_or(false);
                if is_analog {
                    analog_channels.push(i);
                }
                send_pub(
                    &pub_tx,
                    "data",
                    &output_result_frame(i, port, vrms, is_analog, backend),
                );
            }
        } else {
            send_pub(
                &pub_tx,
                "data",
                &json!({
                    "cmd": "probe", "phase": "output_skip",
                    "message": "no DMM configured — skipping output scan",
                    "backend": backend,
                }),
            );
            analog_channels = (0..n_play).collect();
        }

        // Phase 2: Loopback detection
        if !stop.load(Ordering::Relaxed) {
            send_pub(
                &pub_tx,
                "data",
                &json!({
                    "cmd": "probe", "phase": "loopback_start",
                    "n_outputs": analog_channels.len(), "n_inputs": n_cap,
                    "backend": backend,
                }),
            );
        }

        if let Some(cap0) = capture.first() {
            eng.reconnect_input(cap0).ok();
        }

        for &out_idx in &analog_channels {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            eng.connect_output(&playback[out_idx]).ok();
            std::thread::sleep(std::time::Duration::from_millis(150));

            for (j, cap_port) in capture.iter().enumerate() {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                eng.reconnect_input(cap_port).ok();
                eng.flush_capture();
                std::thread::sleep(std::time::Duration::from_millis(50));
                let level_dbfs = match eng.capture_block(0.05) {
                    Ok(data) => {
                        let rms = (data.iter().map(|&x| (x as f64).powi(2)).sum::<f64>()
                            / data.len().max(1) as f64)
                            .sqrt();
                        20.0 * rms.max(1e-12).log10()
                    }
                    Err(_) => -120.0,
                };
                if level_dbfs > LOOPBACK_GATE_DBFS {
                    send_pub(
                        &pub_tx,
                        "data",
                        &json!({
                            "cmd": "probe", "phase": "loopback",
                            "out_ch": out_idx, "out_port": &playback[out_idx],
                            "in_ch": j, "in_port": cap_port,
                            "level_dbfs": (level_dbfs * 10.0).round() / 10.0,
                            "backend": backend,
                        }),
                    );
                }
            }
            eng.disconnect_output(&playback[out_idx]);
        }

        eng.set_silence();
        eng.stop();
        send_pub(
            &pub_tx,
            "done",
            &json!({
                "cmd": "probe",
                "analog_channels": analog_channels,
                "backend": backend,
            }),
        );
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("probe".to_string(), worker);
    }
    json!({
        "ok": true,
        "n_playback": n_play,
        "n_capture": n_cap,
        "level_dbfs": SELF_TEST_LEVEL_DBFS,
        "max_dbfs": MAX_EMISSION_DBFS,
        "backend": backend,
    })
}

#[cfg(test)]
mod tests {
    use super::{output_result_frame, LOOPBACK_GATE_DBFS};

    #[test]
    fn output_result_identifies_live_backend() {
        let frame = output_result_frame(2, "system:playback_3", Some(0.125), true, "fake");

        assert_eq!(frame["backend"], "fake");
    }

    #[test]
    fn loopback_gate_stays_twenty_db_below_the_probe_tone() {
        assert_eq!(
            LOOPBACK_GATE_DBFS,
            ac_core::shared::emission_level::SELF_TEST_LEVEL_DBFS - 20.0
        );
    }
}
