use std::time::Duration;

use crate::data::receiver::{ReceiverHandle, ReceiverStatus};
use crate::data::synthetic::SyntheticHandle;

/// How long a notification string stays visible in the overlay. Also gates
/// the continuous-repaint window: while a notification is live we repaint at
/// ~60 Hz so the fade / pop-in feels right; after it expires we drop back to
/// event-driven idle. Was previously a 1200 ms magic literal at the single
/// overlay-display site; lifted so `about_to_wait` can clear `self.notification`
/// at the same boundary instead of leaking state forever.
pub const NOTIFICATION_TTL: Duration = Duration::from_millis(1200);

/// Frame cap for continuous repaint windows (notification fade, benchmark,
/// data flow). Default 16 ms (≈ 60 Hz) — matches typical display refresh.
/// With the skip-when-unchanged gate in `loop_directive` (#109) the cap
/// is a ceiling, not a target: at 30 Hz daemon data the loop paints
/// 30 fps regardless because there's nothing new to draw between data
/// ticks. The cap binds only on cases that genuinely benefit from 60 Hz
/// — drag/zoom feedback, peak-hold decay, multi-channel rapid mixes.
/// Override at runtime via `--max-fps` / `AC_UI_MAX_FPS` (e.g. drop to
/// 30 on stacks where each `present()` is unusually expensive — see
/// #109's NVIDIA+Vulkan+X11 thread).
pub const CONTINUOUS_REPAINT_INTERVAL_DEFAULT: Duration = Duration::from_millis(16);

/// Lowest `--max-fps` value the parser accepts. 5 Hz is the minimum cadence
/// at which peak-hold decay and waterfall scroll still read as motion
/// rather than stepped state changes; below that we'd be misadvertising
/// "continuous" repaint.
pub const MAX_FPS_MIN: u32 = 5;
/// Upper end. 240 Hz covers gaming-grade displays; beyond that the GPU
/// is the bottleneck anyway and we should stop forcing more presents.
pub const MAX_FPS_MAX: u32 = 240;

/// How recently a frame must have arrived for the loop to stay in
/// continuous-repaint mode. Lets the UI render at vsync between data
/// ticks (so waterfall scroll, peak ramps, hover labels feel smooth at
/// 60 fps even when the daemon emits at 30 Hz) without the #108
/// regression: when monitoring stops, `last_data_arrival` ages past
/// this window within ~half a second and the loop falls through to
/// `Wait`. 500 ms covers a single missed tick at the slowest
/// auto-picked monitor cadence (~33 ms) and short pauses on JACK
/// xruns; longer than that and the user is no longer watching live
/// data, so an idle UI is correct.
pub const DATA_LIVELINESS_WINDOW: Duration = Duration::from_millis(500);

/// Left/Right arrow tunes FFT monitor refresh rate in 1 ms steps (Left =
/// slower, Right = faster). Clamped to [`MONITOR_INTERVAL_MIN_MS`,
/// `MONITOR_INTERVAL_MAX_MS`]. The FLOOR/CEIL below bracket what the eye
/// perceives as "live": 33 ms (30 Hz) is the smoothness floor; we keep the
/// ceiling at the same value so the auto-pick never drops below 30 Hz at
/// huge N (#108 follow-up — the previous 50 ms ceiling pinned fft_n≥32k
/// monitors at 20 fps, which read as visibly stepped after the idle
/// 60 Hz polling redraw was removed).
pub const MONITOR_INTERVAL_MIN_MS: u32 = 1;
pub const MONITOR_INTERVAL_MAX_MS: u32 = 1000;
pub const MONITOR_INTERVAL_FLOOR_MS: u32 = 33;
pub const MONITOR_INTERVAL_CEIL_MS: u32 = 33;

/// Pick a smooth monitor tick for a given FFT size + sample rate. Targets
/// ~window/8 (87.5% overlap) so consecutive frames share most of their
/// input and motion reads as continuous even when N is large; with the
/// FLOOR == CEIL band collapsed to 30 Hz, the result is always 33 ms —
/// large N just gets denser overlap (~97 % at fft_n=65k) rather than a
/// slower visible cadence. Arrow keys still let the user push outside
/// this band manually (down to MONITOR_INTERVAL_MIN_MS or up to
/// MONITOR_INTERVAL_MAX_MS).
pub fn auto_monitor_interval_ms(fft_n: u32, sr_hz: u32) -> u32 {
    let sr = sr_hz.max(1) as f32;
    let window_ms = (fft_n as f32 * 1000.0) / sr;
    let target = (window_ms / 8.0).round().max(1.0) as u32;
    target.clamp(MONITOR_INTERVAL_FLOOR_MS, MONITOR_INTERVAL_CEIL_MS)
}

/// Rolling window for the waterfall row-period estimator. Median over the
/// last N row-to-row dt samples; bigger → more stable axis labels, smaller
/// → faster tracking if the producer cadence genuinely changes. 16 at 10 Hz
/// means the axis responds to real cadence shifts within ~1.6 s while
/// single-frame jitter is absorbed by the median.
pub const WATERFALL_ROW_DT_WINDOW: usize = 16;

/// Peak-hold release: how long the held value sits unchanged before it
/// starts falling toward the live trace. A standard audio-meter "attack-0,
/// release-after-hold" behaviour — a transient pins the trace for 3 s, then
/// the line glides back down at a bounded rate instead of snapping.
pub const PEAK_HOLD_DECAY: Duration = Duration::from_secs(1);

/// Fall rate once release kicks in. 20 dB/s matches the perceived cadence of
/// analogue peak-program meters — fast enough to track genuine level drops,
/// slow enough that the user can still read the number on the way down. Also
/// drives min-hold's symmetric rise toward live.
pub const PEAK_RELEASE_DB_PER_SEC: f32 = 20.0;

/// Need at least this many dt samples in the window before we trust the
/// median enough to replace the 0.1 s default. Below this we keep the
/// default so the first couple of frames don't set a wildly wrong period.
pub const WATERFALL_ROW_DT_MIN: usize = 5;
/// Relative-change gate: only repaint the row-period when the new median
/// differs by more than this fraction from the current value. Kills label
/// flipping caused by micro-jitter in the median without blocking real
/// cadence shifts.
pub const WATERFALL_ROW_DT_HYSTERESIS: f32 = 0.03;

/// Median of an f32 slice, ignoring NaN. Returns `None` if empty.
pub fn median_f32(samples: &[f32]) -> Option<f32> {
    let mut v: Vec<f32> = samples.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    })
}

/// Up/Down arrow tunes FFT size (bin count) through this ladder. Up → larger
/// N (finer resolution), Down → smaller N (coarser but faster capture).
/// Protocol rejects anything outside [256, 131072] or non-pow2.
pub const MONITOR_FFT_N_LADDER: &[u32] =
    &[1024, 2048, 4096, 8192, 16384, 32768, 65536, 131072];

/// Step a ladder: find `current`'s index, move by `delta`, clamp to bounds.
/// Returns the new value, or `current` if it wasn't on the ladder (keeps the
/// UI coherent when the daemon default drifts from the UI default).
pub fn step_ladder(ladder: &[u32], current: u32, delta: i32) -> u32 {
    let Some(idx) = ladder.iter().position(|&v| v == current) else {
        return current;
    };
    let new_idx = (idx as i32 + delta).clamp(0, ladder.len() as i32 - 1) as usize;
    ladder[new_idx]
}

/// Pure-state result of the `about_to_wait` decision, extracted so the same
/// logic can be unit-tested without a winit event loop. Translating this to
/// winit calls is the only thing `about_to_wait` does on top of calling
/// `App::loop_directive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopDirective {
    /// Redraw now, then keep the loop running at ~60 Hz until next tick
    /// (notification fade / benchmark).
    RedrawContinuous,
    /// Redraw now, then block on events (data wake-ups, OS input).
    RedrawIdle,
    /// Don't redraw, wait indefinitely for the next event.
    Idle,
}

pub enum DataSource {
    // Retained: handle owns the synthetic worker thread; dropping it stops the thread.
    Synthetic(#[allow(dead_code)] SyntheticHandle),
    Receiver(ReceiverHandle),
}

impl DataSource {
    pub(super) fn connected(&self) -> bool {
        match self {
            DataSource::Synthetic(_) => true,
            DataSource::Receiver(h) => h.status.connected.load(std::sync::atomic::Ordering::Relaxed),
        }
    }
    pub(super) fn status(&self) -> Option<&ReceiverStatus> {
        match self {
            DataSource::Receiver(h) => Some(&h.status),
            _ => None,
        }
    }
}

pub enum SourceKind {
    Synthetic,
    Daemon,
}

#[cfg(test)]
mod ladder_tests {
    use super::*;

    #[test]
    fn step_ladder_walks_within_bounds() {
        assert_eq!(step_ladder(MONITOR_FFT_N_LADDER, 8192, 0), 8192);
        assert_eq!(step_ladder(MONITOR_FFT_N_LADDER, 8192, -1), 4096);
        assert_eq!(step_ladder(MONITOR_FFT_N_LADDER, 8192, 1), 16384);
    }

    #[test]
    fn step_ladder_clamps_at_edges() {
        assert_eq!(step_ladder(MONITOR_FFT_N_LADDER, 1024, -5), 1024);
        assert_eq!(step_ladder(MONITOR_FFT_N_LADDER, 131072, 5), 131072);
    }

    #[test]
    fn step_ladder_leaves_off_ladder_value_unchanged() {
        assert_eq!(step_ladder(MONITOR_FFT_N_LADDER, 12345, 1), 12345);
    }

    #[test]
    fn fft_n_ladder_entries_are_pow2_in_protocol_range() {
        for &n in MONITOR_FFT_N_LADDER {
            assert!(n.is_power_of_two(), "ladder entry {n} not pow2");
            assert!((256..=131_072).contains(&n), "ladder entry {n} out of protocol range");
        }
    }

    #[test]
    fn auto_interval_floors_for_small_n() {
        // Tiny windows never drop below the display-refresh floor — no
        // reason to tick faster than the eye can track.
        assert_eq!(auto_monitor_interval_ms(1024, 48_000), MONITOR_INTERVAL_FLOOR_MS);
        assert_eq!(auto_monitor_interval_ms(4096, 48_000), MONITOR_INTERVAL_FLOOR_MS);
    }

    #[test]
    fn auto_interval_ceils_for_huge_n() {
        // Huge windows cap at the "still feels live" ceiling even though
        // window/8 would suggest much slower ticks. With FLOOR==CEIL=33ms
        // (#108 follow-up) every fft_n≥8192 lands at 33ms = 30 Hz.
        assert_eq!(auto_monitor_interval_ms(32768, 48_000), MONITOR_INTERVAL_CEIL_MS);
        assert_eq!(auto_monitor_interval_ms(65536, 48_000), MONITOR_INTERVAL_CEIL_MS);
        assert_eq!(auto_monitor_interval_ms(131_072, 48_000), MONITOR_INTERVAL_CEIL_MS);
    }

    /// Specific regression guard for the post-#108 fps drop: fft_n≥65k
    /// must auto-tick at ≤ 33 ms (≥ 30 fps). Before this change the
    /// auto-pick clamped to 50 ms and the UI fell to ~20 fps once the
    /// 60 Hz idle polling redraw was removed.
    #[test]
    fn auto_interval_at_or_below_30hz_floor_for_huge_n() {
        for &n in &[32_768u32, 65_536, 131_072] {
            let tick = auto_monitor_interval_ms(n, 48_000);
            assert!(
                tick <= 33,
                "fft_n={n} auto-tick {tick} ms > 33 ms — regression of #108 follow-up",
            );
        }
    }

    #[test]
    fn auto_interval_scales_with_sample_rate() {
        // Double the sample rate halves the window duration → tick shrinks
        // (until it hits the floor).
        let at_48k = auto_monitor_interval_ms(16384, 48_000);
        let at_96k = auto_monitor_interval_ms(16384, 96_000);
        assert!(at_96k <= at_48k, "{at_96k} ms should be ≤ {at_48k} ms");
    }

    #[test]
    fn auto_interval_stays_within_clamp_band() {
        for &n in MONITOR_FFT_N_LADDER {
            for &sr in &[44_100u32, 48_000, 88_200, 96_000, 192_000] {
                let tick = auto_monitor_interval_ms(n, sr);
                assert!(
                    (MONITOR_INTERVAL_FLOOR_MS..=MONITOR_INTERVAL_CEIL_MS).contains(&tick),
                    "tick {tick} outside band for N={n} sr={sr}"
                );
            }
        }
    }

    #[test]
    fn median_of_odd_slice_picks_middle() {
        assert_eq!(median_f32(&[0.09, 0.11, 0.10]), Some(0.10));
    }

    #[test]
    fn median_of_even_slice_averages_middle_pair() {
        let m = median_f32(&[0.1, 0.2, 0.3, 0.4]).unwrap();
        assert!((m - 0.25).abs() < 1e-6);
    }

    #[test]
    fn median_rejects_nan_samples() {
        // A single spurious NaN in the ring shouldn't poison the estimate.
        let m = median_f32(&[0.10, f32::NAN, 0.11, 0.10]).unwrap();
        assert!((m - 0.10).abs() < 1e-6);
    }

    #[test]
    fn median_empty_returns_none() {
        assert_eq!(median_f32(&[]), None);
    }

    #[test]
    fn median_absorbs_single_stall() {
        // Producer running at ~10 Hz with one 500 ms stall — the median
        // should stay near 0.1 s instead of jumping halfway to 0.5 s the way
        // a 15% EMA would.
        let mut samples = vec![0.10_f32; 15];
        samples.push(0.50);
        let m = median_f32(&samples).unwrap();
        assert!((m - 0.10).abs() < 0.01, "median {m} pulled too far by stall");
    }
}
