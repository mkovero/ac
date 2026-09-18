//! Worker thread management.
//!
//! Each audio command spawns a worker thread that owns the audio engine.
//! The main thread can stop a worker by setting its stop flag.

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Instant;

use crate::audio::AudioEngine;

/// Drive drops if no `set_drive` arrives within this window (§4.3).
pub const DRIVE_DEADMAN_MS: u64 = 1500;

/// Live stimulus state for a running `transfer_stream` worker.
///
/// Shared atomics rather than a channel or a mutex on the engine, for a
/// hard reason and not by preference: `AudioEngine::set_pink` takes
/// `&mut self` and the engine is owned by the worker thread, so the CTRL
/// thread cannot touch it at all. That is the same constraint that made
/// [`WorkerHandle::stop_flag`] an `Arc<AtomicBool>`.
///
/// # Clock
///
/// The keepalive timestamp is **monotonic**, measured from `baseline`,
/// never wall-clock epoch millis. An NTP step backwards would otherwise
/// extend the dead-man arbitrarily and a step forwards would trip it
/// instantly — both on the one path whose entire purpose is stopping an
/// unattended process from driving a loudspeaker. `Instant` is not
/// atomically storable, so both sides store/compare
/// `baseline.elapsed().as_millis()`; per-session millis cannot overflow
/// a `u64`.
pub struct DriveState {
    on: AtomicBool,
    /// `f64::to_bits` of the validated, applied level in dBFS.
    level_bits: AtomicU64,
    last_keepalive_ms: AtomicU64,
    /// The dead-man is armed by the first `set_drive`, not at worker
    /// start. A session launched with the legacy `drive: true` param and
    /// driven by a script sends no keepalives; arming at start would
    /// silence it after 1.5 s and break that path. Every UI-driven
    /// session goes through `set_drive`, so the UI is always covered.
    keepalive_armed: AtomicBool,
    baseline: Instant,
}

impl DriveState {
    /// Initial state from the session's launch params. `on` is the
    /// legacy `drive` param — `ac transfer` never sets it (§4.3).
    pub fn new(on: bool, level_dbfs: f64) -> DriveState {
        DriveState {
            on: AtomicBool::new(on),
            level_bits: AtomicU64::new(level_dbfs.to_bits()),
            last_keepalive_ms: AtomicU64::new(0),
            keepalive_armed: AtomicBool::new(false),
            baseline: Instant::now(),
        }
    }

    /// Apply a `set_drive` request. `level_dbfs` must already have passed
    /// the daemon's fixed-maximum refusal guard; this type stores exactly
    /// what was requested and applied, so the CTRL echo and engine agree.
    ///
    /// Every call refreshes the keepalive: there is no separate
    /// keepalive command, and an unchanged resend is the normal case.
    pub fn set(&self, on: bool, level_dbfs: f64) {
        self.level_bits
            .store(level_dbfs.to_bits(), Ordering::Relaxed);
        self.on.store(on, Ordering::Relaxed);
        self.keepalive_armed.store(true, Ordering::Relaxed);
        self.last_keepalive_ms
            .store(self.elapsed_ms(), Ordering::Relaxed);
    }

    pub fn on(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    pub fn level_dbfs(&self) -> f64 {
        f64::from_bits(self.level_bits.load(Ordering::Relaxed))
    }

    /// Drop drive if the keepalive has gone silent. Called by the worker
    /// on **every** poll, not only when a message arrives — silence is
    /// the condition being detected, so waiting for traffic to check
    /// would be waiting for the thing whose absence is the alarm.
    ///
    /// Returns `true` if this call dropped drive.
    pub fn expire_if_stale(&self, timeout_ms: u64) -> bool {
        if !self.on() || !self.keepalive_armed.load(Ordering::Relaxed) {
            return false;
        }
        let last = self.last_keepalive_ms.load(Ordering::Relaxed);
        if self.elapsed_ms().saturating_sub(last) > timeout_ms {
            self.on.store(false, Ordering::Relaxed);
            return true;
        }
        false
    }

    fn elapsed_ms(&self) -> u64 {
        self.baseline.elapsed().as_millis() as u64
    }
}

/// Session-wide re-lock request for a running `transfer_stream` worker
/// (#226). A monotone generation counter rather than a bool, so two
/// requests arriving inside one worker tick cannot be swallowed into one
/// and so the worker-side test is deterministic: the worker compares
/// against the last generation it consumed, not against a flag a second
/// press before the next poll would silently coalesce into the first.
pub struct RelockRequest {
    generation: AtomicU64,
}

impl RelockRequest {
    pub fn new() -> RelockRequest {
        RelockRequest {
            generation: AtomicU64::new(0),
        }
    }

    /// Ask for a re-lock. The worker's next poll sees a changed
    /// generation even if a prior request was already consumed this tick.
    pub fn request(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }
}

impl Default for RelockRequest {
    fn default() -> Self {
        Self::new()
    }
}

pub struct WorkerHandle {
    pub stop_flag: Arc<AtomicBool>,
    pub thread: Option<JoinHandle<()>>,
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

/// An audio engine that is stopped when it goes out of scope (#432).
///
/// A worker that owns its engine through this type stops it on every exit
/// once the engine exists — normal completion, an early `return` on a
/// setup failure, or a panic unwinding the worker thread — without each
/// exit having to remember to. `AudioEngine::stop` is safe to call more than
/// once on every backend, so an explicit stop ahead of the drop is harmless.
pub struct StoppingEngine(Box<dyn AudioEngine>);

impl StoppingEngine {
    pub fn new(engine: Box<dyn AudioEngine>) -> StoppingEngine {
        StoppingEngine(engine)
    }
}

impl Deref for StoppingEngine {
    type Target = dyn AudioEngine;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

impl DerefMut for StoppingEngine {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut()
    }
}

impl Drop for StoppingEngine {
    fn drop(&mut self) {
        self.0.stop();
    }
}

/// Lock `slot`, apply `f`, release the lock, then clear any poison.
///
/// For teardown code that runs from `Drop`, possibly while a worker thread
/// is unwinding (#432). A plain `lock().unwrap()` there would panic inside a
/// panic and abort the daemon; accepting the poisoned guard cannot. Clearing
/// the poison afterwards keeps a later CTRL handler's `lock().unwrap()` on
/// the same slot from panicking the CTRL thread over a worker's panic.
pub fn with_unpoisoned<T: ?Sized>(slot: &Mutex<T>, f: impl FnOnce(&mut T)) {
    {
        let mut guard = slot.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut guard);
    }
    slot.clear_poison();
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
        "transfer_stream" => Some(Group::Transfer),
        "plot" | "plot_level" | "plot_ir" | "calibrate" | "calibrate_spl" | "session_check"
        | "probe" | "test_hardware" | "test_dut" => Some(Group::Exclusive),
        _ => None,
    }
}
