//! Per-pair state, split by lifetime: [`PairCtx`] is fixed for the session,
//! [`PairState`] is what the loop maintains, and [`Lock`] is the held delay
//! estimate with the provenance a drive edge reads (#226).

use serde_json::Value;

use ac_core::shared::calibration::Calibration;

/// A pair's held delay lock (#226). `driving` records whether the drive
/// was on at the tick this lock was accepted — the qualifier the drive
/// off→on edge reads to decide whether this lock is stale by
/// construction (taken against silence) or survives (taken while
/// driving, so a dead-man drop and resume must not disturb it). Carried
/// inside the `Option` rather than beside it so a pair that is currently
/// unlocked cannot hold a stale, meaningless flag: provenance exists only
/// when a lock does.
#[derive(Debug, Clone, Copy)]
pub(super) struct Lock {
    pub(super) samples: i64,
    pub(super) driving: bool,
}

/// Everything about one pair that is fixed for the session: the channels
/// it names, where those channels sit in the capture buffers, and the
/// calibration each leg carries. Built once at launch, read-only after.
///
/// This replaces a `Vec` per field indexed by pair. That shape cost a
/// seven-deep `zip` at the per-pair fan-out — deep enough that
/// `delay_attempts` was read by index rather than joining it — and made
/// each vec an independent chance to index the wrong pair, with nothing
/// in the types saying they had to agree.
pub(super) struct PairCtx {
    /// Position in the launch `pairs` list. Frames publish in this order,
    /// and it is the index into the per-tick ladder column vectors.
    pub(super) pos: usize,
    pub(super) meas_ch: u32,
    pub(super) ref_ch: u32,
    /// Index of the measurement channel in the capture buffers / `rings`.
    pub(super) mi: usize,
    /// Index of the reference channel in the capture buffers / `rings`.
    pub(super) ri: usize,
    pub(super) meas_cal: Option<Calibration>,
    pub(super) ref_cal: Option<Calibration>,
    /// `meas_cal`'s mic-curve, lifted out because the mag/phase/re/im
    /// correction path takes it alone and must stay untouched
    /// (additive-only discipline). A ref-leg curve is refused at launch,
    /// so there is deliberately no `ref_curve` twin.
    pub(super) meas_curve: Option<ac_core::shared::calibration::MicResponse>,
}

/// Everything about one pair that the worker loop maintains across ticks.
///
/// Plain data on purpose: the per-pair fan-out takes `&PairState`, so
/// every field here has to be `Sync`. The ladder (`MtwPair`) is
/// deliberately *not* a field — it owns an FFT planner, and it is
/// consumed into `mtw_columns` before the fan-out rather than read
/// inside it, so it stays a separate vec alongside.
pub(super) struct PairState {
    /// Delay cache: ref↔meas propagation is constant during a streaming
    /// session (fixed hardware path), so we estimate once per pair on
    /// warmup and reuse the result. Skipping `estimate_delay` per tick
    /// (a 262 k-point FFT+IFFT at 2.5 s ring / 48 kHz) cuts the hot-loop
    /// work from ~17 ms → ~3 ms and takes the refresh rate from choppy
    /// ~8.5 Hz to the capture-interval-limited rate.
    ///
    /// That rate is ~16.6 Hz, not the ~10 Hz an older note claimed: the
    /// limit is `chunk_secs` (0.05 s) plus per-tick work, and
    /// `chunk_secs` was 0.2 when the ~10 Hz figure was written. Measured
    /// 2026-08-06 on `--fake-audio` at 48 kHz over 30 s, two pairs,
    /// median inter-frame gap 60.3 ms; the rig sees 17.5–18 Hz at 96 kHz.
    pub(super) delay: Option<Lock>,
    /// A pair whose delay estimate was *refused* (no prominent
    /// correlation peak — #227) stays unlocked and is retried, because
    /// the cause is usually transient from the software's point of view:
    /// an unpatched reference leg or a muted source that the operator
    /// then fixes. Retry is rate-limited because each attempt is the same
    /// full-ring FFT+IFFT the cache above exists to avoid, and the inputs
    /// it reads only turn over on the ring's own timescale.
    pub(super) next_attempt: Option<std::time::Instant>,
    /// Peak-to-median prominence from the most recent attempt, locked or
    /// refused. Published so a session that never locks still says how
    /// far short it fell — the estimator's one empirical constant is set
    /// from this distribution, and a bare "refused" would not measure it.
    pub(super) prominence: Option<Value>,
    /// How many delay estimates this pair has completed, accepted or
    /// refused. Published as `delay_attempts` (#238).
    ///
    /// This is the only thing on the wire that separates "warming up"
    /// from "refusing": both publish `delay_locked: false`, and until an
    /// attempt has run there is no statement to make about the pair at
    /// all. The consumer that needs it is the fault indicator, which may
    /// not paint `LOST LOCK` on a session that has simply not been asked
    /// a question yet — see `ac-scene::fault`.
    ///
    /// A count, not a verdict. It says the estimator ran; it says nothing
    /// about how close the result came, which is the estimator's own
    /// business (`delay_evidence`, diagnostic-only).
    ///
    /// MONOTONE for the life of the session — never reset, including by
    /// #226's re-locking. Resetting it would make a pair that locked and
    /// then started refusing read as one that has not been asked yet, and
    /// the fault indicator answers "nothing to report" to that.
    pub(super) attempts: u32,
    /// Per-pair `spl` time-integration state (F/S EMA, n_bands=1 —
    /// handoff: transfer-frame-v2 M0). `None` for a pair whose meas
    /// channel has no SPL calibration layer; `spl` stays `null` for that
    /// pair's whole session, matching `spl_offsets` in `monitor.rs`.
    /// Session-static per D10, so decided once at construction rather
    /// than re-checked per tick.
    pub(super) spl_integ: Option<ac_core::visualize::time_integration::EmaIntegrator>,
    /// Timestamp of the last `spl` integration step, for its `dt`.
    pub(super) spl_last: Option<std::time::Instant>,
}

impl PairState {
    pub(super) fn new(
        spl_integ: Option<ac_core::visualize::time_integration::EmaIntegrator>,
    ) -> Self {
        Self {
            delay: None,
            next_attempt: None,
            prominence: None,
            attempts: 0,
            spl_integ,
            spl_last: None,
        }
    }

    /// Discard this pair's held lock and its `ladder`, and clear the
    /// retry timer so the next tick attempts acquisition immediately
    /// rather than waiting out `RELOCK_RETRY`. Leaves `attempts` and
    /// `prominence` untouched — the first must stay monotone (a reset
    /// would make a locked-then-refusing pair read as one never asked),
    /// and the second is last-attempt evidence that the next attempt
    /// overwrites on its own.
    ///
    /// Takes the ladder slot as an argument because `MtwPair` cannot live
    /// in `PairState` (see the type's note), but a flush that dropped the
    /// lock without the ladder would leave a ladder aligned to an offset
    /// no longer held.
    pub(super) fn flush(&mut self, ladder: &mut Option<ac_core::visualize::mtw::MtwPair>) {
        self.delay = None;
        self.next_attempt = None;
        *ladder = None;
    }
}
