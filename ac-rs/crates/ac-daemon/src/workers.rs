//! Worker thread management.
//!
//! Each audio command spawns a worker thread that owns the audio engine.
//! The main thread can stop a worker by setting its stop flag.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

pub struct WorkerHandle {
    pub stop_flag: Arc<AtomicBool>,
    pub thread:    Option<JoinHandle<()>>,
}

impl WorkerHandle {
    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::Relaxed);
    }

    pub fn join(&mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.stop();
        self.join();
    }
}

/// Concurrency groups for the busy guard (mirrors Python engine.py).
///
/// * `Output` — drives the soundcard output (tone/pink/sweep).
/// * `Input`  — drains capture into a spectrum stream.
/// * `Transfer` — passive H1 estimator on independent JACK capture clients;
///   one at a time, but coexists with `Input` and `Output` because each
///   worker owns its own `AudioEngine` with its own capture ring.
/// * `Exclusive` — monopolises the engine (calibration, full-sweep plots,
///   hardware/DUT probes).
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Group {
    Output,
    Input,
    Transfer,
    Exclusive,
}

pub fn cmd_group(name: &str) -> Option<Group> {
    match name {
        "sweep_level" | "sweep_frequency" | "generate" | "generate_pink" => Some(Group::Output),
        "monitor_spectrum" => Some(Group::Input),
        "transfer_stream"  => Some(Group::Transfer),
        "plot" | "plot_level" | "calibrate" | "transfer"
        | "probe" | "test_hardware" | "test_dut" => Some(Group::Exclusive),
        _ => None,
    }
}
