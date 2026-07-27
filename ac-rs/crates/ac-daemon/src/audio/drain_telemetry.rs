//! Consumer-side drain telemetry for the streaming capture path (#208, D1).
//!
//! The leading hypothesis for the triple recurrence is that the transfer
//! worker's sliding window advances at a fraction of realtime — the consumer
//! not keeping up, backlog accumulating in the capture rings, and Welch
//! re-reporting the same impulse once per segment position as it crawls
//! through the window. That is a number, not an opinion: samples popped per
//! tick against wall-clock elapsed. This module records it.
//!
//! **Instrumentation only.** Nothing here changes what is captured, analysed
//! or published; it is off unless `AC_DRAIN_TELEMETRY` is set, and when off
//! the per-tick cost is one `bool` test.
//!
//! Two lines are emitted rather than one summary, deliberately. The summary
//! is convenient, but a derived figure that only the implementation can
//! reproduce is not evidence — QA has to be able to re-derive the drain rate
//! from the raw per-tick record (`dt_us`, `n`) without trusting the division
//! done here. So every tick is logged raw, and the window summary is a
//! convenience on top of it.
//!
//! Not real-time code: this runs on the worker thread, alongside the drain it
//! measures.

use std::time::{Duration, Instant};

/// Default seconds between window summaries. Raw per-tick lines are not
/// affected by this — they are emitted every tick.
const DEFAULT_REPORT_SECS: f64 = 1.0;

/// What one tick produced: the raw record, plus a window summary when the
/// report interval has elapsed.
pub(crate) struct DrainTick {
    /// Raw per-tick line. Always present.
    pub(crate) raw: String,
    /// Window summary, present on the tick that closes a report interval.
    pub(crate) summary: Option<String>,
}

/// Accumulates per-tick drain figures and formats them.
///
/// Constructed disabled unless the environment asks for it; a disabled
/// instance returns `None` from [`Self::tick`] and touches nothing else.
pub(crate) struct DrainTelemetry {
    enabled: bool,
    sr: f64,
    report_every: Duration,

    seq: u64,
    prev_tick: Option<Instant>,

    /// Wall-clock span the current window has accumulated. Summed from the
    /// per-tick intervals rather than taken as `now - window_start` so that
    /// the summary is derivable from exactly the numbers the raw lines carry.
    win_elapsed: Duration,
    win_ticks: u64,
    win_samples: u64,
    win_zero_ticks: u64,
    win_min_n: usize,
    win_max_n: usize,
    win_max_occupied: usize,

    /// Consecutive zero-length pops, and the longest such run seen this
    /// session with the wall-clock time it covered. A stalled `min_occupied()`
    /// freezes the analysis window while the loop keeps emitting frames, which
    /// looks identical to a slow drain in the display but is a different
    /// defect.
    zero_run: u64,
    zero_run_elapsed: Duration,
    max_zero_run: u64,
    max_zero_run_elapsed: Duration,

    tot_ticks: u64,
    tot_samples: u64,
    tot_elapsed: Duration,
}

impl DrainTelemetry {
    /// Enabled by `AC_DRAIN_TELEMETRY` in {`1`, `true`, `yes`}; the report
    /// interval is overridable with `AC_DRAIN_TELEMETRY_SECS`.
    pub(crate) fn from_env(sr: u32) -> Self {
        let enabled = std::env::var("AC_DRAIN_TELEMETRY")
            .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
            .unwrap_or(false);
        let secs = std::env::var("AC_DRAIN_TELEMETRY_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|s| *s > 0.0)
            .unwrap_or(DEFAULT_REPORT_SECS);
        Self::new(enabled, sr, Duration::from_secs_f64(secs))
    }

    pub(crate) fn new(enabled: bool, sr: u32, report_every: Duration) -> Self {
        Self {
            enabled,
            sr: (sr as f64).max(1.0),
            report_every,
            seq: 0,
            prev_tick: None,
            win_elapsed: Duration::ZERO,
            win_ticks: 0,
            win_samples: 0,
            win_zero_ticks: 0,
            win_min_n: usize::MAX,
            win_max_n: 0,
            win_max_occupied: 0,
            zero_run: 0,
            zero_run_elapsed: Duration::ZERO,
            max_zero_run: 0,
            max_zero_run_elapsed: Duration::ZERO,
            tot_ticks: 0,
            tot_samples: 0,
            tot_elapsed: Duration::ZERO,
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// Record one drain.
    ///
    /// `n` is the samples popped this tick (the length of the returned
    /// measurement block), `occupancy` the per-ring occupancy sampled inside
    /// the drain immediately before the pop, and `discarded` the backend's
    /// cumulative counted-discard figure.
    ///
    /// `now` is injected so the arithmetic is testable without sleeping.
    pub(crate) fn tick(
        &mut self,
        n: usize,
        occupancy: &[usize],
        discarded: u64,
        now: Instant,
    ) -> Option<DrainTick> {
        if !self.enabled {
            return None;
        }
        let dt = self.prev_tick.map(|p| now.saturating_duration_since(p));
        self.prev_tick = Some(now);
        self.seq += 1;

        let occ_max = occupancy.iter().copied().max().unwrap_or(0);
        let occ_min = occupancy.iter().copied().min().unwrap_or(0);

        // The first tick has no interval behind it, so it contributes its
        // sample count to nothing — a rate needs two timestamps. It is still
        // logged raw, since its `n` says how much backlog the warmup left.
        if let Some(dt) = dt {
            self.win_ticks += 1;
            self.win_samples += n as u64;
            self.win_elapsed += dt;
            self.win_min_n = self.win_min_n.min(n);
            self.win_max_n = self.win_max_n.max(n);
            self.win_max_occupied = self.win_max_occupied.max(occ_max);
            self.tot_ticks += 1;
            self.tot_samples += n as u64;
            self.tot_elapsed += dt;
            if n == 0 {
                self.win_zero_ticks += 1;
                self.zero_run += 1;
                self.zero_run_elapsed += dt;
                if self.zero_run > self.max_zero_run {
                    self.max_zero_run = self.zero_run;
                    self.max_zero_run_elapsed = self.zero_run_elapsed;
                }
            } else {
                self.zero_run = 0;
                self.zero_run_elapsed = Duration::ZERO;
            }
        }

        let raw = format!(
            "drain-tick seq={} dt_us={} n={} occ_min={} occ_max={} occ={:?} discarded={}",
            self.seq,
            dt.map(|d| d.as_micros().to_string())
                .unwrap_or_else(|| "-".to_string()),
            n,
            occ_min,
            occ_max,
            occupancy,
            discarded,
        );

        let summary = if dt.is_some() && self.win_elapsed >= self.report_every {
            let s = self.format_window();
            self.reset_window();
            Some(s)
        } else {
            None
        };

        Some(DrainTick { raw, summary })
    }

    /// Samples drained per second of wall clock, as a fraction of realtime.
    /// 1.0 means the consumer is keeping up exactly; below 1.0 means backlog
    /// is accumulating in the rings at that rate.
    fn rate(&self, samples: u64, elapsed: Duration) -> f64 {
        let secs = elapsed.as_secs_f64();
        if secs <= 0.0 {
            return f64::NAN;
        }
        samples as f64 / (self.sr * secs)
    }

    fn format_window(&self) -> String {
        let win_rate = self.rate(self.win_samples, self.win_elapsed);
        let tot_rate = self.rate(self.tot_samples, self.tot_elapsed);
        let mean_dt_us = if self.win_ticks > 0 {
            self.win_elapsed.as_micros() as f64 / self.win_ticks as f64
        } else {
            f64::NAN
        };
        format!(
            "drain-window sr={} ticks={} elapsed_s={:.4} samples={} \
             rate={:.4}x mean_dt_us={:.0} n_min={} n_max={} occ_max={} \
             zero_ticks={} max_zero_run={} max_zero_run_s={:.3} \
             session_ticks={} session_elapsed_s={:.3} session_rate={:.4}x",
            self.sr as u64,
            self.win_ticks,
            self.win_elapsed.as_secs_f64(),
            self.win_samples,
            win_rate,
            mean_dt_us,
            if self.win_min_n == usize::MAX {
                0
            } else {
                self.win_min_n
            },
            self.win_max_n,
            self.win_max_occupied,
            self.win_zero_ticks,
            self.max_zero_run,
            self.max_zero_run_elapsed.as_secs_f64(),
            self.tot_ticks,
            self.tot_elapsed.as_secs_f64(),
            tot_rate,
        )
    }

    fn reset_window(&mut self) {
        self.win_elapsed = Duration::ZERO;
        self.win_ticks = 0;
        self.win_samples = 0;
        self.win_zero_ticks = 0;
        self.win_min_n = usize::MAX;
        self.win_max_n = 0;
        self.win_max_occupied = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn disabled_by_default_construction_emits_nothing() {
        let mut t = DrainTelemetry::new(false, 48_000, Duration::from_secs(1));
        assert!(!t.enabled());
        assert!(t.tick(2400, &[2400], 0, Instant::now()).is_none());
    }

    /// A consumer keeping up: 50 ms ticks each popping 50 ms of audio.
    #[test]
    fn realtime_drain_reports_unity_rate() {
        let base = Instant::now();
        let mut t = DrainTelemetry::new(true, 48_000, Duration::from_secs(1));
        let mut last = None;
        for i in 0..21 {
            let r = t.tick(2400, &[2400, 2400], 0, at(base, i * 50)).unwrap();
            if r.summary.is_some() {
                last = r.summary;
            }
        }
        let s = last.expect("a window closes within 21 ticks at 50 ms");
        assert!(s.contains("rate=1.0000x"), "{s}");
        assert!(s.contains("mean_dt_us=50000"), "{s}");
        assert!(s.contains("zero_ticks=0"), "{s}");
    }

    /// The shape the hypothesis predicts: the loop takes longer per tick than
    /// the audio it drains, so backlog accumulates and the rate sits below 1.
    #[test]
    fn slow_consumer_reports_sub_realtime_rate() {
        let base = Instant::now();
        let mut t = DrainTelemetry::new(true, 48_000, Duration::from_secs(1));
        // 250 ms of wall clock per tick, 50 ms of audio drained: 0.2x.
        let mut last = None;
        for i in 0..10 {
            let r = t.tick(2400, &[2400], 0, at(base, i * 250)).unwrap();
            if r.summary.is_some() {
                last = r.summary;
            }
        }
        let s = last.expect("a window closes within 10 ticks at 250 ms");
        assert!(s.contains("rate=0.2000x"), "{s}");
    }

    /// Zero-length pops are counted and their longest run timed: a frozen
    /// `min_occupied()` is a different defect from a slow drain and must not
    /// be reported as one.
    #[test]
    fn zero_pops_are_counted_and_the_longest_run_timed() {
        let base = Instant::now();
        let mut t = DrainTelemetry::new(true, 48_000, Duration::from_secs(10));
        t.tick(2400, &[2400], 0, at(base, 0)).unwrap();
        for i in 1..=4 {
            t.tick(0, &[0], 0, at(base, i * 50)).unwrap();
        }
        t.tick(2400, &[2400], 0, at(base, 250)).unwrap();
        t.tick(0, &[0], 0, at(base, 300)).unwrap();
        assert_eq!(t.max_zero_run, 4);
        assert_eq!(t.max_zero_run_elapsed, Duration::from_millis(200));
        assert_eq!(t.win_zero_ticks, 5);
    }

    /// The raw line must carry `dt_us` and `n` so the rate can be re-derived
    /// without trusting `format_window` (#208 acceptance criterion 1).
    #[test]
    fn raw_line_carries_the_inputs_the_rate_is_derived_from() {
        let base = Instant::now();
        let mut t = DrainTelemetry::new(true, 48_000, Duration::from_secs(1));
        let first = t.tick(2400, &[2400, 2401], 0, at(base, 0)).unwrap();
        assert!(first.raw.contains("dt_us=-"), "{}", first.raw);
        assert!(first.summary.is_none());

        let second = t.tick(2400, &[2400, 2401], 17, at(base, 50)).unwrap();
        assert!(second.raw.contains("dt_us=50000"), "{}", second.raw);
        assert!(second.raw.contains("n=2400"), "{}", second.raw);
        assert!(second.raw.contains("occ_min=2400"), "{}", second.raw);
        assert!(second.raw.contains("occ_max=2401"), "{}", second.raw);
        assert!(second.raw.contains("discarded=17"), "{}", second.raw);
    }

    /// The window summary's own numbers must be self-consistent: the rate it
    /// prints has to equal samples / (sr · elapsed) from the same line.
    #[test]
    fn window_rate_matches_its_own_samples_and_elapsed() {
        let base = Instant::now();
        let mut t = DrainTelemetry::new(true, 48_000, Duration::from_millis(200));
        for i in 0..6 {
            t.tick(1200, &[1200], 0, at(base, i * 50));
        }
        // 5 intervals of 50 ms = 250 ms elapsed, 6000 samples drained.
        // 6000 / (48000 · 0.25) = 0.5. The 200 ms report interval closes on
        // the fourth of those, so one interval has accumulated into the fresh
        // window by the end — the session totals still see all five.
        assert_eq!(t.win_ticks, 1, "the window resets after reporting");
        assert_eq!(t.tot_samples, 6000);
        assert_eq!(t.tot_elapsed, Duration::from_millis(250));
        assert!((t.rate(6000, Duration::from_millis(250)) - 0.5).abs() < 1e-12);
    }
}
