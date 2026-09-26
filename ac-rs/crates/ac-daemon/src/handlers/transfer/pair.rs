//! Per-pair state, split by lifetime: [`PairCtx`] is fixed for the session,
//! [`PairState`] is what the loop maintains, and [`Lock`] is the held delay
//! estimate with the provenance a drive edge reads (#226).

use ac_core::shared::calibration::{Calibration, LayerVerdict};

/// A pair's held delay (#226, #669). `operator` is true when a client set
/// it with `set_delay`; the daemon never replaces an operator-set delay.
/// `driving` records, for a delay the daemon found, whether the drive
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
    pub(super) operator: bool,
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
    /// Each leg's calibration as applied: a voltage scale the session
    /// check refused is withheld here (#466).
    pub(super) meas_cal: Option<Calibration>,
    pub(super) ref_cal: Option<Calibration>,
    /// The session check's recorded voltage verdict per leg, decided once at
    /// session start (#466). `Some` exactly when that leg's **stored**
    /// calibration has `vrms_at_0dbfs_in` — the presence rule of
    /// `cal_tags.*.voltage_check`.
    pub(super) meas_voltage_check: Option<LayerVerdict>,
    pub(super) ref_voltage_check: Option<LayerVerdict>,
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
    /// The held delay: found once at start from the unaligned live IR's
    /// peak, or set by the operator (#669). ref↔meas propagation is
    /// constant on a fixed path, so the daemon does not move it by itself.
    pub(super) delay: Option<Lock>,
    /// A pair whose start-up Find had no peak (a silent leg) stays
    /// unaligned and is retried, because the cause is usually one the
    /// operator then fixes: an unpatched reference or a muted source.
    pub(super) next_attempt: Option<std::time::Instant>,
    /// How many start-up Finds this pair has completed, with or without a
    /// peak. Published as `delay_attempts` (#238).
    ///
    /// This is the only thing on the wire that separates "warming up"
    /// from "refusing": both publish `delay_locked: false`, and until an
    /// attempt has run there is no statement to make about the pair at
    /// all. The consumer that needs it is the fault indicator, which may
    /// not paint `LOST LOCK` on a session that has simply not been asked
    /// a question yet — see `ac-scene::fault`.
    ///
    /// A count, not a verdict. It says the estimator ran; it says nothing
    /// about what the result was.
    ///
    /// MONOTONE for the life of the session — never reset, including by
    /// a re-find (`set_delay` with `samples: null`). Resetting it would make a pair that locked and
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
            attempts: 0,
            spl_integ,
            spl_last: None,
        }
    }

    /// Discard this pair's held lock and its `ladder`, and clear the
    /// retry timer so the next tick attempts acquisition immediately
    /// rather than waiting out `FIND_RETRY`. Leaves `attempts` untouched —
    /// it must stay monotone (a reset would make a found-then-silent pair
    /// read as one never asked).
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
