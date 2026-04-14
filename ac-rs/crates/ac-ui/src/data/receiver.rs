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

pub fn spawn(endpoint: String, mut input: Input<SpectrumFrame>) -> ReceiverHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let status = Arc::new(ReceiverStatus::new());
    let stop_c = stop.clone();
    let status_c = status.clone();
    let join = thread::spawn(move || {
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
                    let frame: SpectrumFrame = match serde_json::from_str(body) {
                        Ok(f) => f,
                        Err(_) => continue,
                    };
                    if frame.spectrum.is_empty() {
                        continue;
                    }
                    status_c.connected.store(true, Ordering::Relaxed);
                    let ns = start.elapsed().as_nanos() as u64;
                    status_c.last_frame_ns.store(ns, Ordering::Relaxed);
                    input.write(frame);
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

fn mark_stale(status: &ReceiverStatus, start: &Instant) {
    let last = status.last_frame_ns.load(Ordering::Relaxed);
    let now = start.elapsed().as_nanos() as u64;
    if last == 0 || now.saturating_sub(last) > 2_000_000_000 {
        status.connected.store(false, Ordering::Relaxed);
    }
}
