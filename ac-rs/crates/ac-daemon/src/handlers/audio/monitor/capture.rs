//! Pulling audio out of the engine: the per-tick drain budget, the
//! ring-buffered capture the CWT / CQT / reassigned modes share, and how a
//! capture failure is reported.

use serde_json::json;

use ac_core::measurement::loudness::LoudnessState;
use ac_core::shared::mic_curve_filter::MicCurveFir;

use crate::handlers::send_pub;

use super::channel::{ChannelState, RingKind};
use super::frames::{emit_scope_frame, TickCtx};

/// Samples to pull from the capture ring per tick, per channel.
///
/// **This must never be less than what arrives in the same wall-clock
/// interval.** The ring fills at `sr` samples/second regardless of what the
/// analysis does with them; if a tick drains less than `per_ch_budget · sr`,
/// the shortfall accumulates in the JACK ring on every single tick and the
/// displayed spectrum falls progressively further behind realtime — for as
/// long as the monitor runs.
///
/// That is exactly what issue #208 was. The budget used to be
/// `clamp(128, fft_n)`, and the upper clamp bit whenever
/// `per_ch_budget · sr > fft_n` — which is the *default* configuration:
/// `interval` 0.2 s at 96 kHz is 19 200 samples arriving per tick against an
/// `fft_n` of 8192 drained, so the monitor consumed audio at 42.7% of
/// realtime. Measured on hardware: 500 ms bursts emitted 10 s apart appeared
/// in the published frames 23.2 s apart, at lags of 12.3 s, 25.3 s and
/// 38.8 s — growing without bound. The reported symptom (a stimulus
/// reappearing seconds later, then repeating with nothing happening in the
/// room) was the daemon playing out that backlog.
///
/// It bit at 48 kHz too — 9600 arriving against 8192 drained, a lag growing
/// at 0.147 s per second, so 3–5 s of lag after 20–34 s of monitoring. That
/// matches the originally reported "three to five seconds" and is why the
/// symptom survived the `ac-ui` → `ac-view` rewrite: it was never in either
/// UI.
///
/// There is deliberately **no upper bound**. Reading more than `fft_n` is
/// harmless — the sliding ring pops the excess straight back off — whereas
/// any ceiling below the arrival rate reintroduces the defect. The 128-sample
/// floor stays so a very short `interval` still gives JACK something to hand
/// back.
pub(super) fn capture_budget_samples(per_ch_budget_secs: f64, sr: u32) -> usize {
    ((per_ch_budget_secs * sr as f64) as usize).max(128)
}

/// Push captured samples to the loudness state, optionally filtering
/// through the per-channel mic-curve FIR first (#104). When the FIR
/// is bypassed (toggle off, or no curve loaded), pushes the raw
/// samples — preserves the existing dBTP / LKFS path so a channel
/// without a curve sees no behavioural change.
///
/// The FIR's delay-line state persists across calls so block boundaries
/// are seamless. Toggling the global enable flag mid-stream causes a
/// brief discontinuity (one FIR-length of stale history); document'd
/// in the wire frame's `mic_correction` field flipping `"on"` → `"off"`.
pub(super) fn push_loudness_with_optional_fir(
    loudness: &mut LoudnessState,
    fir: &mut Option<MicCurveFir>,
    mic_corr_enabled: bool,
    samples: &[f32],
) {
    if let (true, Some(fir)) = (mic_corr_enabled, fir.as_mut()) {
        let mut filtered = samples.to_vec();
        fir.process_inplace(&mut filtered);
        let _ = loudness.push(&[&filtered]);
    } else {
        let _ = loudness.push(&[samples]);
    }
}

/// Unwrap a capture result, publishing a `capture error on chN` frame and
/// returning `None` when the engine failed.
///
/// A capture failure is terminal for the worker — there is no partial
/// buffer to fall back on — so every caller must `return` on `None`. The
/// `let ... else { return; }` is left at the call site rather than hidden
/// in here, so the control flow stays visible where it happens.
pub(super) fn capture_or_report<T, E: std::fmt::Display>(
    result: Result<T, E>,
    pub_tx: &crossbeam_channel::Sender<Vec<u8>>,
    channel: u32,
) -> Option<T> {
    match result {
        Ok(v) => Some(v),
        Err(e) => {
            send_pub(
                pub_tx,
                "error",
                &json!({
                    "cmd":     "monitor_spectrum",
                    "message": format!("capture error on ch{channel}: {e}"),
                }),
            );
            None
        }
    }
}

/// Minimum CWT ring fill before a column is emitted. `morlet_cwt_into`
/// asserts on fewer than 256 samples.
pub(super) const CWT_MIN_FILL: usize = 256;

/// Outcome of one ring-buffered capture.
pub(super) enum RingTick {
    /// The ring holds enough samples for a valid column. Carries the
    /// engine's cumulative xrun count, read once here so the frames built
    /// from this capture all quote the same number.
    Ready { xruns: u32 },
    /// Not enough samples yet — the caller should skip this channel.
    NotReady,
    /// Capture failed and has been reported; the caller must return.
    Failed,
}

/// Capture one paced block for `ch`, push it through the loudness meter,
/// emit its scope frame, and append it to the mode's ring, trimmed to
/// `ring_cap` from the front.
///
/// This is the half of a CWT / CQT / reassigned tick that does not depend
/// on which transform runs: the three modes differ only in which ring
/// they fill, how full it must be, and what they then compute from it.
pub(super) fn capture_into_ring(
    eng: &mut dyn crate::audio::AudioEngine,
    ch: &mut ChannelState,
    ctx: &TickCtx,
    kind: RingKind,
    ring_cap: usize,
    min_fill: usize,
) -> RingTick {
    // Pace the capture tick to the UI's requested interval, clamped to
    // [16 ms, 100 ms] by the caller. Pre-#109 this was hardcoded 20 ms
    // regardless of `--max-fps`, so CWT emitted at 50 fps even when the
    // UI was capped at 30 — wasted work on both sides.
    let tick_secs = ctx.tick_secs;
    let Some(samples) = capture_or_report(eng.capture_block(tick_secs), ctx.pub_tx, ch.channel)
    else {
        return RingTick::Failed;
    };
    // `eng.xruns()` is already a cumulative count for this engine session
    // (see `jack_backend.rs`'s `SharedState::xruns`), so this assigns
    // rather than accumulates — summing it across per-tick, per-channel
    // reads would multiply a handful of real xruns into thousands over a
    // long monitor session.
    let xruns = eng.xruns();
    // Feed the raw capture into the loudness meter before any downstream
    // consumer touches it.
    push_loudness_with_optional_fir(
        &mut ch.loudness,
        &mut ch.loudness_fir,
        ctx.mic_corr_enabled,
        &samples,
    );
    emit_scope_frame(ch, ctx, &samples, xruns);
    let ring = ch.ring_mut(kind);
    ring.extend(samples.iter());
    while ring.len() > ring_cap {
        ring.pop_front();
    }
    if ring.len() < min_fill {
        return RingTick::NotReady;
    }
    RingTick::Ready { xruns }
}

/// Periodic timing line for a ring-buffered transform — one line every
/// 50 ticks so a slow transform is visible without flooding the log.
pub(super) fn log_transform_time(
    counter: &mut u32,
    label: &str,
    channel: u32,
    t0: std::time::Instant,
    ring_len: usize,
    n_out: usize,
) {
    *counter += 1;
    if *counter % 50 == 1 {
        eprintln!(
            "{label} ch{channel}: {:.1}ms, ring={ring_len}, out={n_out}",
            t0.elapsed().as_secs_f64() * 1000.0,
        );
    }
}

#[cfg(test)]
mod drain_budget_tests {
    use super::capture_budget_samples;

    /// **The invariant that issue #208 violated.** A tick must drain at least
    /// as many samples as arrive during the same interval. Anything less
    /// accumulates in the JACK ring every tick, so the spectrum falls
    /// progressively behind realtime and the daemon ends up replaying old
    /// audio — the reported "response reappears with no stimulus present".
    ///
    /// Swept across every sample rate and interval the daemon accepts, and
    /// across the full legal `fft_n` range, because the old bug was precisely
    /// a ceiling that depended on `fft_n`.
    #[test]
    fn budget_never_drains_slower_than_the_ring_fills() {
        for sr in [44_100u32, 48_000, 88_200, 96_000, 176_400, 192_000] {
            for interval in [0.002f64, 0.01, 0.05, 0.1, 0.2, 0.5, 1.0] {
                for n_ch in [1usize, 2, 4, 8] {
                    let per_ch = (interval / n_ch as f64).max(0.002);
                    let arriving = (per_ch * sr as f64) as usize;
                    let budget = capture_budget_samples(per_ch, sr);
                    assert!(
                        budget >= arriving,
                        "sr={sr} interval={interval} n_ch={n_ch}: drains {budget}                          but {arriving} arrive — backlog grows {} samples/tick",
                        arriving - budget
                    );
                }
            }
        }
    }

    /// The exact configuration that shipped the defect: daemon defaults
    /// (`interval` 0.2, `fft_n` 8192), single channel, at both rig rates.
    /// The old `clamp(128, fft_n)` returned 8192 in both rows.
    #[test]
    fn default_monitor_config_keeps_up_at_both_rig_rates() {
        // 96 kHz: 19 200 arrive per 0.2 s tick. Old budget 8192 = 42.7%.
        assert_eq!(capture_budget_samples(0.2, 96_000), 19_200);
        // 48 kHz: 9600 arrive. Old budget 8192 = 85.3%, a lag growing at
        // 0.147 s/s — 3-5 s behind after 20-34 s.
        assert_eq!(capture_budget_samples(0.2, 48_000), 9_600);
    }

    /// The floor survives: a very short interval must still ask JACK for
    /// enough to be worth a round trip.
    #[test]
    fn short_interval_keeps_the_floor() {
        assert_eq!(capture_budget_samples(0.002, 44_100), 128);
        assert!(capture_budget_samples(0.0, 96_000) >= 128);
    }
}
