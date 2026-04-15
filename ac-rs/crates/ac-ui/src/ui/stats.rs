//! Rolling frame-time stats for the timing overlay and benchmark harness.
//!
//! Keeps a fixed-capacity ring buffer of frame samples (CPU prep + total
//! frame wall time) plus the most recent GPU pass timings from the
//! query-set readback. Snapshots are computed on demand and contain
//! mean / p50 / p95 / p99 over the buffered window.

use std::time::Duration;

use crate::render::timing::PassTimings;

const CAPACITY: usize = 240; // ~4 s @ 60 fps

#[derive(Clone, Copy, Debug, Default)]
struct Sample {
    cpu_ms:   f32,
    frame_ms: f32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StatsSnapshot {
    pub samples:        usize,
    pub fps:            f32,
    pub frame_mean_ms:  f32,
    pub frame_p50_ms:   f32,
    pub frame_p95_ms:   f32,
    pub frame_p99_ms:   f32,
    pub cpu_mean_ms:    f32,
    pub gpu:            PassTimings,
}

pub struct TimingStats {
    samples:  Vec<Sample>,
    head:     usize,
    filled:   usize,
    last_gpu: PassTimings,
}

impl TimingStats {
    pub fn new() -> Self {
        Self {
            samples:  vec![Sample::default(); CAPACITY],
            head:     0,
            filled:   0,
            last_gpu: PassTimings::default(),
        }
    }

    pub fn push(&mut self, cpu: Duration, frame: Duration, gpu: PassTimings) {
        self.samples[self.head] = Sample {
            cpu_ms:   cpu.as_secs_f32() * 1_000.0,
            frame_ms: frame.as_secs_f32() * 1_000.0,
        };
        self.head = (self.head + 1) % CAPACITY;
        if self.filled < CAPACITY { self.filled += 1; }
        // Hold the latest non-zero GPU sample so the overlay does not
        // flicker between filled and empty values when readback misses
        // a frame (the timing module returns zeros until the first
        // resolve completes).
        if gpu.gpu_ms > 0.0 { self.last_gpu = gpu; }
    }

    pub fn snapshot(&self) -> StatsSnapshot {
        if self.filled == 0 {
            return StatsSnapshot::default();
        }
        let mut frame_ms: Vec<f32> = self.samples[..self.filled].iter().map(|s| s.frame_ms).collect();
        let cpu_sum: f32 = self.samples[..self.filled].iter().map(|s| s.cpu_ms).sum();
        let frame_sum: f32 = frame_ms.iter().sum();
        let frame_mean = frame_sum / self.filled as f32;
        frame_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let percentile = |p: f32| {
            let idx = ((self.filled as f32 - 1.0) * p).round() as usize;
            frame_ms[idx.min(self.filled - 1)]
        };
        let fps = if frame_mean > 0.0 { 1_000.0 / frame_mean } else { 0.0 };
        StatsSnapshot {
            samples:       self.filled,
            fps,
            frame_mean_ms: frame_mean,
            frame_p50_ms:  percentile(0.50),
            frame_p95_ms:  percentile(0.95),
            frame_p99_ms:  percentile(0.99),
            cpu_mean_ms:   cpu_sum / self.filled as f32,
            gpu:           self.last_gpu,
        }
    }
}

impl Default for TimingStats {
    fn default() -> Self { Self::new() }
}
