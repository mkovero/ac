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
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Group {
    Output,
    Input,
    Exclusive,
}

pub fn cmd_group(name: &str) -> Option<Group> {
    match name {
        "sweep_level" | "sweep_frequency" | "generate" | "generate_pink" => Some(Group::Output),
        "monitor_spectrum" => Some(Group::Input),
        "plot" | "plot_level" | "calibrate" | "transfer" | "probe"
        | "test_hardware" | "test_dut" => Some(Group::Exclusive),
        _ => None,
    }
}
