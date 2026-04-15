use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use triple_buffer::Input;

use super::types::SpectrumFrame;

pub struct ReceiverStatus {
    pub connected: AtomicBool,
    pub last_frame_ns: AtomicU64,
}

impl ReceiverStatus {
    pub fn new() -> Self {
        Self {
            connected: AtomicBool::new(false),
            last_frame_ns: AtomicU64::new(0),
        }
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

pub fn spawn(endpoint: String, inputs: Vec<Input<SpectrumFrame>>) -> ReceiverHandle {
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

        let ctx = zmq::Context::new();
        let sub = match ctx.socket(zmq::SUB) {
            Ok(s) => s,
            Err(e) => {
                log::error!("zmq socket: {e}");
                return;
            }
        };
        if let Err(e) = sub.set_subscribe(b"data") {
            log::error!("zmq subscribe: {e}");
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
                    let body = match msg.split_once(' ') {
                        Some(("data", body)) => body,
                        _ => continue,
                    };
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

fn mark_stale(status: &ReceiverStatus, start: &Instant) {
    let last = status.last_frame_ns.load(Ordering::Relaxed);
    let now = start.elapsed().as_nanos() as u64;
    if last == 0 || now.saturating_sub(last) > 2_000_000_000 {
        status.connected.store(false, Ordering::Relaxed);
    }
}
