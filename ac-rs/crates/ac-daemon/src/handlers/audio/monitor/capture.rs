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

/// Capture one paced, non-clearing block for `ch`, push it through the
/// loudness meter, emit its scope frame, and append it to the mode's ring,
/// trimmed to `ring_cap` from the front.
///
/// The drain is `capture_contiguous`, not `capture_block` (#210): the ring
/// is a sliding window analysed as one continuous stretch of time, so a
/// pre-wait `clear()` would discard the audio that arrived while the
/// previous tick was being processed and splice the fragments together.
/// `capture_contiguous` still waits for `tick_secs` of audio, which is what
/// paces these modes, and returns everything buffered, so it never drains
/// slower than the ring fills (#208).
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
    let Some(samples) =
        capture_or_report(eng.capture_contiguous(tick_secs), ctx.pub_tx, ch.channel)
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
mod ring_contiguity_tests {
    use std::cell::Cell;

    use super::{capture_into_ring, RingTick};
    use crate::audio::fake::FakeEngine;
    use crate::audio::AudioEngine;
    use crate::handlers::audio::monitor::channel::{ChannelState, RingCaps, RingKind};
    use crate::handlers::audio::monitor::frames::TickCtx;

    /// The rig #207 was measured on: RME Babyface Pro, 96 kHz, quantum 1024.
    const SR: u32 = 96_000;
    const PERIOD: usize = 1024;
    /// Not a multiple of `SR / PERIOD` = 93.75 Hz (161.07 cycles per period),
    /// so every discarded whole period costs it phase — a splice is visible.
    /// A commensurate tone such as 15 000 Hz would pass even with the
    /// clearing drain (`audio/contiguity.rs`,
    /// `period_quantisation_decides_which_frequencies_expose_the_splice`).
    const TONE_HZ: f64 = 15_100.0;
    const AMPLITUDE: f64 = 0.5;
    /// Per-tick consumer processing time the fake ring accrues between
    /// drains. 20 ms is 1920 samples, more than one period, so a clearing
    /// drain discards at least one whole period on every tick and every
    /// junction inside the ring is a splice.
    const PROCESS_SECS: f64 = 0.02;
    /// Monitor tick: the bottom of the handler's [16 ms, 100 ms] clamp.
    ///
    /// Must be the short end. At 96 kHz a 16 ms tick is 1536 samples, below
    /// every ring cap including reassigned's 4096, so each ring spans several
    /// drains and therefore several junctions. At 50 ms (4800 samples) the
    /// reassigned ring would hold the tail of a single block, contain no
    /// junction, and pass even with the clearing drain restored.
    const TICK_SECS: f64 = 0.016;

    /// `y[n] = x[n+1] + x[n−1] − 2cos(ω)·x[n]`: zero for any pure sinusoid
    /// at `ω`, whatever its amplitude and phase.
    fn annihilate(x: &[f64], omega: f64) -> Vec<f64> {
        let k = 2.0 * omega.cos();
        x.windows(3).map(|w| w[2] + w[0] - k * w[1]).collect()
    }

    /// Largest residual left after annihilating both components the fake
    /// tone contains: the fundamental at `ω` and the 1 % second harmonic at
    /// `2ω` that `fake/stimulus.rs` adds to every tone.
    ///
    /// Zero for a contiguous stream, needing neither amplitude nor phase.
    /// Across a splice junction the phase jumps and the residual is of order
    /// the amplitude.
    fn max_recurrence_residual(x: &[f32], omega: f64) -> f64 {
        let x: Vec<f64> = x.iter().map(|&v| v as f64).collect();
        annihilate(&annihilate(&x, omega), 2.0 * omega)
            .into_iter()
            .map(f64::abs)
            .fold(0.0, f64::max)
    }

    /// Fill one ring-mode ring at its real cap from a ring-backed fake engine
    /// and return `(ring contents, samples discarded)`.
    fn fill_ring(label: &str, kind: RingKind, ring_cap: usize) -> (Vec<f32>, u64) {
        let mut eng = FakeEngine::new();
        eng.set_sample_rate(SR);
        eng.enable_ring_mode(PROCESS_SECS, 0, PERIOD);
        eng.set_external_tones(&[(TONE_HZ, AMPLITUDE)]);

        let caps = RingCaps {
            cwt: ring_cap,
            cqt: ring_cap,
            reass: ring_cap,
        };
        let mut ch = ChannelState::new(0, "fake:in".into(), None, None, SR, TONE_HZ, &caps);
        let (pub_tx, _pub_rx) = crossbeam_channel::unbounded();
        let scope_frame_idx = Cell::new(0);
        let ctx = TickCtx {
            pub_tx: &pub_tx,
            n_channels: 1,
            sr: SR,
            backend: "fake",
            scope_frame_idx: &scope_frame_idx,
            mic_corr_enabled: false,
            tick_secs: TICK_SECS,
        };

        // Enough ticks to fill the ring and then slide it a few more times,
        // so its contents are made of several drains' worth of junctions.
        let per_tick = (TICK_SECS * SR as f64) as usize;
        let ticks = ring_cap.div_ceil(per_tick) + 4;
        for _ in 0..ticks {
            match capture_into_ring(&mut eng, &mut ch, &ctx, kind, ring_cap, 0) {
                RingTick::Ready { .. } | RingTick::NotReady => {}
                RingTick::Failed => panic!("fake capture failed"),
            }
        }
        let ring: Vec<f32> = ch.ring_mut(kind).iter().copied().collect();
        assert_eq!(ring.len(), ring_cap, "{label}: ring must be full");
        (ring, eng.discarded_samples())
    }

    /// Issue #210's guard: each of the three sliding rings — CWT at 0.15 s,
    /// CQT at 1.0 s, reassigned at `DEFAULT_N` — receives an unspliced stream.
    ///
    /// The three transforms are not what is under test, and one shared capture
    /// function feeds all three rings, so the check is on the samples handed
    /// to each ring rather than a spectral assertion on its transform: the
    /// pure-sinusoid recurrence residual, which a single splice junction
    /// raises to the order of the amplitude.
    ///
    /// Mutation check: with `capture_block(tick_secs)` restored in
    /// `capture_into_ring`, both assertions fail for all three rings.
    #[test]
    fn ring_modes_receive_contiguous_audio() {
        let omega = 2.0 * std::f64::consts::PI * TONE_HZ / SR as f64;
        // Provenance: derived. f32 rounding of a unit-scale sine is ~6e-8 ·
        // amplitude, and the two cascaded annihilators amplify it at most
        // 4 × 4 = 16×, so a contiguous ring leaves ~1e-6 · amplitude; a splice
        // leaves one of order the amplitude. 1e-4 sits orders of magnitude
        // from both.
        let bound = 1e-4 * AMPLITUDE;
        // Every ring and both checks are evaluated before failing, so one run
        // shows which of the three paths — and which check — went red.
        let mut failures = Vec::new();
        for (label, kind, cap) in [
            ("cwt", RingKind::Cwt, (SR as f64 * 0.15).ceil() as usize),
            ("cqt", RingKind::Cqt, SR as usize),
            (
                "reassigned",
                RingKind::Reassigned,
                ac_core::visualize::reassigned::DEFAULT_N,
            ),
        ] {
            let (ring, discarded) = fill_ring(label, kind, cap);
            if discarded != 0 {
                failures.push(format!(
                    "{label}: the capture discarded {discarded} samples — a clearing \
                     drain is feeding a sliding ring"
                ));
            }
            let residual = max_recurrence_residual(&ring, omega);
            if residual >= bound {
                failures.push(format!(
                    "{label}: recurrence residual {residual:.3e} ≥ {bound:.1e} — the ring \
                     holds non-contiguous fragments of a {TONE_HZ} Hz tone"
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
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
