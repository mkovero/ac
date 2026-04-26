use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use triple_buffer::Input;
use winit::event_loop::EventLoopProxy;

use super::store::{LoudnessStore, SweepStore, TransferStore, VirtualChannelStore};
use super::types::{
    CwtFrame, LoudnessReadout, SpectrumFrame, SweepDone, SweepPoint, TransferFrame, TransferPair,
};

pub struct ReceiverStatus {
    pub connected: AtomicBool,
    pub last_frame_ns: AtomicU64,
    /// Latest unconsumed worker-error message published by the daemon on the
    /// `error` PUB topic. App drains this every frame to raise a notification.
    pub last_error: Mutex<Option<String>>,
}

impl ReceiverStatus {
    pub fn new() -> Self {
        Self {
            connected: AtomicBool::new(false),
            last_frame_ns: AtomicU64::new(0),
            last_error: Mutex::new(None),
        }
    }

    pub fn take_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|mut g| g.take())
    }
}

pub struct ReceiverHandle {
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
    pub status: Arc<ReceiverStatus>,
}

impl Drop for ReceiverHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

pub fn spawn(
    endpoint: String,
    inputs: Vec<Input<SpectrumFrame>>,
    transfer: TransferStore,
    virtual_channels: VirtualChannelStore,
    sweep: SweepStore,
    loudness: LoudnessStore,
    wake: Option<EventLoopProxy<()>>,
) -> ReceiverHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let status = Arc::new(ReceiverStatus::new());
    let stop_c = stop.clone();
    let status_c = status.clone();
    let join = thread::spawn(move || {
        let mut inputs = inputs;
        let n_slots = inputs.len();
        let mut channel_map: Vec<Option<u32>> = vec![None; n_slots];
        let mut slot_seq: Vec<u64> = vec![0; n_slots];
        let mut warned_overflow = false;
        let notify = || {
            if let Some(ref p) = wake {
                let _ = p.send_event(());
            }
        };

        let ctx = zmq::Context::new();
        let sub = match ctx.socket(zmq::SUB) {
            Ok(s) => s,
            Err(e) => {
                log::error!("zmq socket: {e}");
                return;
            }
        };
        if let Err(e) = sub.set_subscribe(b"data") {
            log::error!("zmq subscribe data: {e}");
            return;
        }
        // Also receive worker error frames (`send_pub("error", …)`) so
        // silent-failing workers surface in the UI.
        if let Err(e) = sub.set_subscribe(b"error") {
            log::error!("zmq subscribe error: {e}");
            return;
        }
        if let Err(e) = sub.set_subscribe(b"done") {
            log::error!("zmq subscribe done: {e}");
            return;
        }
        if let Err(e) = sub.set_rcvtimeo(200) {
            log::error!("zmq rcvtimeo: {e}");
            return;
        }
        if let Err(e) = sub.connect(&endpoint) {
            log::error!("zmq connect {endpoint}: {e}");
            return;
        }
        log::info!("receiver connected to {endpoint}");

        let start = Instant::now();
        while !stop_c.load(Ordering::Relaxed) {
            match sub.recv_string(0) {
                Ok(Ok(msg)) => {
                    let (topic, body) = match msg.split_once(' ') {
                        Some((t, b)) => (t, b),
                        None => continue,
                    };
                    if topic == "error" {
                        let text = serde_json::from_str::<serde_json::Value>(body)
                            .ok()
                            .and_then(|v| {
                                let cmd = v.get("cmd").and_then(|c| c.as_str()).map(str::to_string);
                                let msg = v.get("message").and_then(|m| m.as_str()).map(str::to_string);
                                match (cmd, msg) {
                                    (Some(c), Some(m)) => Some(format!("{c}: {m}")),
                                    (None, Some(m))    => Some(m),
                                    (Some(c), None)    => Some(c),
                                    _ => None,
                                }
                            })
                            .unwrap_or_else(|| body.to_string());
                        log::warn!("daemon error: {text}");
                        if let Ok(mut g) = status_c.last_error.lock() {
                            *g = Some(text);
                        }
                        continue;
                    }
                    if topic == "done" {
                        if let Ok(done) = serde_json::from_str::<SweepDone>(body) {
                            if done.cmd == "plot" || done.cmd == "plot_level" {
                                log::info!(
                                    "sweep done: cmd={} n_points={} xruns={}",
                                    done.cmd, done.n_points, done.xruns,
                                );
                                sweep.set_done(done);
                                status_c.connected.store(true, Ordering::Relaxed);
                                let ns = start.elapsed().as_nanos() as u64;
                                status_c.last_frame_ns.store(ns, Ordering::Relaxed);
                                notify();
                            }
                        }
                        continue;
                    }
                    if topic != "data" {
                        continue;
                    }
                    // Multiplexed "data" topic: spectrum frames and
                    // transfer_stream frames share the channel but have
                    // disjoint shapes. Peek at `type` first — `transfer_stream`
                    // goes to the TransferStore, everything else defaults to
                    // the legacy SpectrumFrame deserializer.
                    let type_tag = serde_json::from_str::<serde_json::Value>(body)
                        .ok()
                        .as_ref()
                        .and_then(|v| v.get("type"))
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string());
                    if matches!(
                        type_tag.as_deref(),
                        Some("visualize/cwt")
                            | Some("visualize/cqt")
                            | Some("visualize/reassigned")
                    ) {
                        // CWT / CQT / reassigned column: magnitudes are
                        // already dBFS and frequencies are log-spaced.
                        // Repackage as a SpectrumFrame so the existing
                        // display pipeline (triple-buffer → waterfall)
                        // consumes it unchanged. The waterfall auto-detects
                        // log spacing from the freqs step ratio (see
                        // app.rs log_spaced detection). All three frame
                        // shapes are wire-identical; one parser covers all.
                        let cf: CwtFrame = match serde_json::from_str(body) {
                            Ok(f) => f,
                            Err(e) => {
                                log::warn!("cwt/cqt/reassigned parse failed: {e}");
                                continue;
                            }
                        };
                        if cf.magnitudes.is_empty() {
                            continue;
                        }
                        let slot = route_slot(cf.channel, &mut channel_map);
                        let Some(slot) = slot else {
                            if !warned_overflow {
                                log::warn!(
                                    "receiver: cwt/cqt/reassigned frame for channel {:?} exceeds {} preallocated slots; dropping",
                                    cf.channel,
                                    n_slots
                                );
                                warned_overflow = true;
                            }
                            continue;
                        };
                        status_c.connected.store(true, Ordering::Relaxed);
                        let ns = start.elapsed().as_nanos() as u64;
                        status_c.last_frame_ns.store(ns, Ordering::Relaxed);
                        notify();
                        slot_seq[slot] += 1;
                        let frame = SpectrumFrame {
                            freqs: cf.frequencies,
                            spectrum: cf.magnitudes,
                            sr: cf.sr,
                            channel: cf.channel,
                            n_channels: cf.n_channels,
                            spl_offset_db: cf.spl_offset_db,
                            mic_correction: cf.mic_correction,
                            frame_id: slot_seq[slot],
                            ..SpectrumFrame::default()
                        };
                        inputs[slot].write(frame);
                        continue;
                    }
                    if type_tag.as_deref() == Some("visualize/fractional_octave") {
                        // 1/N-octave aggregation of the CWT column. Daemon
                        // emits this in addition to the `cwt` frame each
                        // tick when `set_ioct_bpo` has enabled it. Schema
                        // matches SpectrumFrame (`freqs` + `spectrum`), so
                        // we deserialise straight into one. Writing it to
                        // the same triple-buffer slot as the preceding
                        // `cwt` frame means the renderer naturally shows
                        // the band view whenever ioct is on; the AppState's
                        // `ioct_bpo` provides the overlay label context so
                        // the user knows the trace isn't raw CWT.
                        let mut sf: SpectrumFrame = match serde_json::from_str(body) {
                            Ok(f) => f,
                            Err(e) => {
                                log::warn!("fractional_octave parse failed: {e}");
                                continue;
                            }
                        };
                        if sf.spectrum.is_empty() {
                            continue;
                        }
                        let slot = route_slot(sf.channel, &mut channel_map);
                        let Some(slot) = slot else {
                            if !warned_overflow {
                                log::warn!(
                                    "receiver: fractional_octave frame for channel {:?} exceeds {} preallocated slots; dropping",
                                    sf.channel,
                                    n_slots
                                );
                                warned_overflow = true;
                            }
                            continue;
                        };
                        status_c.connected.store(true, Ordering::Relaxed);
                        let ns = start.elapsed().as_nanos() as u64;
                        status_c.last_frame_ns.store(ns, Ordering::Relaxed);
                        notify();
                        slot_seq[slot] += 1;
                        sf.frame_id = slot_seq[slot];
                        inputs[slot].write(sf);
                        continue;
                    }
                    if type_tag.as_deref() == Some("visualize/fractional_octave_leq") {
                        // Time-integrated sidecar to `fractional_octave` (see
                        // ZMQ.md § time-integration). Publishers emit this
                        // immediately after the corresponding `fractional_octave`
                        // frame; overwriting the same triple-buffer slot means
                        // consumers paint the integrated trace rather than the
                        // raw one whenever integration is active. Mode label is
                        // surfaced by the overlay via `leq_duration_s` — `NaN`
                        // for fast/slow (duration is irrelevant), real seconds
                        // for Leq.
                        #[derive(serde::Deserialize)]
                        struct LeqFrame {
                            freqs: Vec<f32>,
                            spectrum: Vec<f32>,
                            sr: u32,
                            #[serde(default)]
                            channel: Option<u32>,
                            #[serde(default)]
                            n_channels: Option<u32>,
                            #[serde(default)]
                            mode: Option<String>,
                            #[serde(default)]
                            duration_s: Option<f64>,
                            #[serde(default)]
                            xruns: u32,
                        }
                        let lf: LeqFrame = match serde_json::from_str(body) {
                            Ok(f) => f,
                            Err(e) => {
                                log::warn!("fractional_octave_leq parse failed: {e}");
                                continue;
                            }
                        };
                        if lf.spectrum.is_empty() {
                            continue;
                        }
                        let slot = route_slot(lf.channel, &mut channel_map);
                        let Some(slot) = slot else {
                            if !warned_overflow {
                                log::warn!(
                                    "receiver: fractional_octave_leq frame for channel {:?} exceeds {} preallocated slots; dropping",
                                    lf.channel,
                                    n_slots
                                );
                                warned_overflow = true;
                            }
                            continue;
                        };
                        status_c.connected.store(true, Ordering::Relaxed);
                        let ns = start.elapsed().as_nanos() as u64;
                        status_c.last_frame_ns.store(ns, Ordering::Relaxed);
                        notify();
                        slot_seq[slot] += 1;
                        let leq_duration_s = match lf.mode.as_deref() {
                            Some("leq") => lf.duration_s.or(Some(0.0)),
                            Some("fast") | Some("slow") => Some(f64::NAN),
                            _ => lf.duration_s,
                        };
                        let sf = SpectrumFrame {
                            freqs: lf.freqs,
                            spectrum: lf.spectrum,
                            sr: lf.sr,
                            channel: lf.channel,
                            n_channels: lf.n_channels,
                            xruns: lf.xruns,
                            frame_id: slot_seq[slot],
                            leq_duration_s,
                            ..SpectrumFrame::default()
                        };
                        inputs[slot].write(sf);
                        continue;
                    }
                    if type_tag.as_deref() == Some("measurement/loudness") {
                        // Per-channel BS.1770-5 / R128 meter readout (see
                        // ZMQ.md § loudness). The daemon emits `channel` as
                        // a physical capture index (e.g. 10 for
                        // `ac monitor 10`), so we route through the same
                        // `channel_map` spectrum/cwt frames use and key the
                        // store by UI slot — the overlay reads by
                        // `active_channel` which is a slot, not a physical
                        // index. Silence / pre-gate fields arrive as
                        // `null` and stay `None`.
                        #[derive(serde::Deserialize)]
                        struct LoudnessFrame {
                            channel: Option<u32>,
                            #[serde(default)]
                            momentary_lkfs: Option<f64>,
                            #[serde(default)]
                            short_term_lkfs: Option<f64>,
                            #[serde(default)]
                            integrated_lkfs: Option<f64>,
                            #[serde(default)]
                            lra_lu: f64,
                            #[serde(default)]
                            true_peak_dbtp: Option<f64>,
                            #[serde(default)]
                            gated_duration_s: f64,
                            #[serde(default)]
                            spl_offset_db: Option<f64>,
                        }
                        match serde_json::from_str::<LoudnessFrame>(body) {
                            Ok(lf) => {
                                let slot = route_slot(lf.channel, &mut channel_map);
                                let Some(slot) = slot else {
                                    if !warned_overflow {
                                        log::warn!(
                                            "receiver: loudness frame for channel {:?} exceeds {} preallocated slots; dropping",
                                            lf.channel,
                                            n_slots,
                                        );
                                        warned_overflow = true;
                                    }
                                    continue;
                                };
                                loudness.write(
                                    slot as u32,
                                    LoudnessReadout {
                                        momentary_lkfs: lf.momentary_lkfs,
                                        short_term_lkfs: lf.short_term_lkfs,
                                        integrated_lkfs: lf.integrated_lkfs,
                                        lra_lu: lf.lra_lu,
                                        true_peak_dbtp: lf.true_peak_dbtp,
                                        gated_duration_s: lf.gated_duration_s,
                                        spl_offset_db: lf.spl_offset_db,
                                    },
                                );
                                status_c.connected.store(true, Ordering::Relaxed);
                                let ns = start.elapsed().as_nanos() as u64;
                                status_c.last_frame_ns.store(ns, Ordering::Relaxed);
                                notify();
                            }
                            Err(e) => {
                                log::warn!("measurement/loudness parse failed: {e}");
                            }
                        }
                        continue;
                    }
                    if type_tag.as_deref() == Some("transfer_stream") {
                        match serde_json::from_str::<TransferFrame>(body) {
                            Ok(tf) => {
                                log::info!(
                                    "transfer_stream frame: bins={} delay_ms={:.2} meas={} ref={}",
                                    tf.freqs.len(),
                                    tf.delay_ms,
                                    tf.meas_channel,
                                    tf.ref_channel,
                                );
                                let pair = TransferPair {
                                    meas: tf.meas_channel,
                                    ref_ch: tf.ref_channel,
                                };
                                virtual_channels.write(pair, tf.clone());
                                transfer.write(tf);
                                status_c.connected.store(true, Ordering::Relaxed);
                                let ns = start.elapsed().as_nanos() as u64;
                                status_c.last_frame_ns.store(ns, Ordering::Relaxed);
                                notify();
                            }
                            Err(e) => {
                                log::warn!(
                                    "transfer_stream parse failed: {e} — body head: {}",
                                    &body[..body.len().min(200)]
                                );
                            }
                        }
                        continue;
                    }
                    if type_tag.as_deref() == Some("measurement/frequency_response/point") {
                        match serde_json::from_str::<SweepPoint>(body) {
                            Ok(pt) => {
                                log::debug!(
                                    "sweep_point n={} fund_hz={:.1} thd={:.3}%",
                                    pt.n, pt.fundamental_hz, pt.thd_pct,
                                );
                                sweep.push(pt);
                                status_c.connected.store(true, Ordering::Relaxed);
                                let ns = start.elapsed().as_nanos() as u64;
                                status_c.last_frame_ns.store(ns, Ordering::Relaxed);
                                notify();
                            }
                            Err(e) => {
                                log::warn!("sweep_point parse failed: {e}");
                            }
                        }
                        continue;
                    }
                    let mut frame: SpectrumFrame = match serde_json::from_str(body) {
                        Ok(f) => f,
                        Err(_) => continue,
                    };
                    if frame.spectrum.is_empty() {
                        continue;
                    }
                    // Daemon publishes a linear amplitude spectrum (|FFT|/N/wc
                    // in [0, ~1]). The UI pipeline — auto-init dB window,
                    // colormap mapping, hover readout — all assume dBFS. Match
                    // `ac/ui/spectrum.py:131` which does the same conversion.
                    for v in frame.spectrum.iter_mut() {
                        *v = 20.0 * v.max(1e-12).log10();
                    }
                    let slot = route_slot(frame.channel, &mut channel_map);
                    let Some(slot) = slot else {
                        if !warned_overflow {
                            log::warn!(
                                "receiver: frame for channel {:?} exceeds {} preallocated slots; dropping",
                                frame.channel,
                                n_slots
                            );
                            warned_overflow = true;
                        }
                        continue;
                    };
                    status_c.connected.store(true, Ordering::Relaxed);
                    let ns = start.elapsed().as_nanos() as u64;
                    status_c.last_frame_ns.store(ns, Ordering::Relaxed);
                    notify();
                    slot_seq[slot] += 1;
                    frame.frame_id = slot_seq[slot];
                    inputs[slot].write(frame);
                }
                Ok(Err(_)) => continue,
                Err(zmq::Error::EAGAIN) => {
                    mark_stale(&status_c, &start);
                    continue;
                }
                Err(e) => {
                    log::warn!("recv error: {e}");
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
    });
    ReceiverHandle {
        stop,
        join: Some(join),
        status,
    }
}

/// Map an incoming `channel` id onto a preallocated slot index.
///
/// First-come-first-served: the first unseen `channel` value claims the first
/// free slot, subsequent frames for the same channel reuse it. Frames with no
/// channel field always map to slot 0. Returns `None` when all slots are
/// already claimed and the channel hasn't been seen — the caller drops the
/// frame.
fn route_slot(channel: Option<u32>, map: &mut [Option<u32>]) -> Option<usize> {
    if map.is_empty() {
        return None;
    }
    let Some(ch) = channel else {
        return Some(0);
    };
    if let Some(idx) = map.iter().position(|slot| *slot == Some(ch)) {
        return Some(idx);
    }
    if let Some(idx) = map.iter().position(Option::is_none) {
        map[idx] = Some(ch);
        return Some(idx);
    }
    None
}

/// Liveness window must cover the slowest legitimate producer. `monitor_spectrum`
/// publishes every ~200 ms; `transfer_stream` publishes one H1 estimate per
/// ~4 s capture block. Anything shorter than the transfer cadence would flip
/// the status to "disconnected" between every transfer frame.
const STALE_THRESHOLD_NS: u64 = 6_000_000_000;

fn mark_stale(status: &ReceiverStatus, start: &Instant) {
    let last = status.last_frame_ns.load(Ordering::Relaxed);
    let now = start.elapsed().as_nanos() as u64;
    if last == 0 || now.saturating_sub(last) > STALE_THRESHOLD_NS {
        status.connected.store(false, Ordering::Relaxed);
    }
}
