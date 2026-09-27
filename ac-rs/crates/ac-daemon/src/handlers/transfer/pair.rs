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
    /// Stream position (in samples since session start) where driven audio
    /// begins, after a drive off→on edge (#226, #669). The Find waits until
    /// the analysis ring starts at or after it: a Find over a ring still
    /// holding the pre-drive silence takes a noise peak, and with no
    /// refusal any more that peak would be held as "found while driving"
    /// and never re-found.
    pub(super) find_from: Option<u64>,
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
    /// Delay tracking switched on for this pair (#687).
    pub(super) tracking: bool,
    /// The residual tracking is waiting to see confirmed (#687).
    pub(super) track_candidate: Option<TrackCandidate>,
}

/// One analysis window's residual, held until an independent window
/// confirms or replaces it (#687).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TrackCandidate {
    /// The live IR's residual against `delay`, samples.
    pub(super) residual: i64,
    /// The held delay the residual was measured against.
    pub(super) delay: i64,
    /// One past the last ring sample the window covered, in the rings'
    /// coordinate (the one `SessionState::dropped` counts in).
    pub(super) ring_end: u64,
}

/// One analysis window as tracking sees it.
#[derive(Debug, Clone, Copy)]
pub(super) struct TrackObservation {
    pub(super) residual: Option<i64>,
    pub(super) delay: i64,
    pub(super) ring_start: u64,
    pub(super) ring_end: u64,
}

/// How far two windows' residuals may differ and still agree, samples. A
/// peak can land on either of two adjacent samples when the true arrival
/// falls between them; two samples of scatter is a different arrival.
pub(super) const TRACK_AGREEMENT_SAMPLES: i64 = 1;

/// The tracking rule (#687): the step to apply to the held delay, or
/// `None`. Moves only when the residual of a window that shares **no
/// samples** with the candidate's window agrees with it within
/// [`TRACK_AGREEMENT_SAMPLES`], both measured against the same held delay.
///
/// Consecutive ticks share almost the whole 2.5 s ring, so agreement
/// between them is the same data read twice, not a confirmation: a single
/// noise or reflection peak would pass it. A residual of 0, no residual (a
/// silent leg), or a different held delay clears the candidate.
pub(super) fn track_step(
    candidate: &mut Option<TrackCandidate>,
    obs: TrackObservation,
) -> Option<i64> {
    let residual = match obs.residual {
        Some(r) if r != 0 => r,
        _ => {
            *candidate = None;
            return None;
        }
    };
    let fresh = TrackCandidate {
        residual,
        delay: obs.delay,
        ring_end: obs.ring_end,
    };
    match *candidate {
        Some(c) if c.delay != obs.delay => {
            *candidate = Some(fresh);
            None
        }
        // Still overlapping the candidate's window: no new evidence yet.
        Some(c) if obs.ring_start < c.ring_end => None,
        Some(c) if (residual - c.residual).abs() <= TRACK_AGREEMENT_SAMPLES => {
            *candidate = None;
            Some(residual)
        }
        _ => {
            *candidate = Some(fresh);
            None
        }
    }
}

impl PairState {
    pub(super) fn new(
        spl_integ: Option<ac_core::visualize::time_integration::EmaIntegrator>,
    ) -> Self {
        Self {
            delay: None,
            next_attempt: None,
            find_from: None,
            attempts: 0,
            spl_integ,
            spl_last: None,
            tracking: false,
            track_candidate: None,
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
        self.track_candidate = None;
        *ladder = None;
    }
}

#[cfg(test)]
mod track_tests {
    use super::*;

    const RING: u64 = 120_000; // 2.5 s at 48 kHz
    const HOP: u64 = 24_000; // 0.5 s

    fn obs(residual: Option<i64>, delay: i64, start: u64) -> TrackObservation {
        TrackObservation {
            residual,
            delay,
            ring_start: start,
            ring_end: start + RING,
        }
    }

    /// The rejected rule, computed here: move when two consecutive
    /// analyses agree, whatever their windows share.
    fn consecutive_rule(prev: Option<i64>, now: Option<i64>) -> Option<i64> {
        match (prev, now) {
            (Some(a), Some(b)) if b != 0 && (a - b).abs() <= TRACK_AGREEMENT_SAMPLES => Some(b),
            _ => None,
        }
    }

    /// Agreement on overlapping windows is the same audio read twice: the
    /// consecutive rule moves on it, this rule waits for a window that
    /// shares no samples.
    #[test]
    fn overlapping_windows_never_confirm() {
        let mut c = None;
        assert_eq!(track_step(&mut c, obs(Some(20), 460, 0)), None);
        for k in 1..5 {
            let start = k * HOP; // still inside the first window
            assert_eq!(track_step(&mut c, obs(Some(20), 460, start)), None);
            assert_eq!(
                consecutive_rule(Some(20), Some(20)),
                Some(20),
                "the rejected rule would have moved at hop {k}"
            );
        }
    }

    #[test]
    fn an_independent_window_that_agrees_moves_the_delay() {
        let mut c = None;
        track_step(&mut c, obs(Some(20), 460, 0));
        assert_eq!(track_step(&mut c, obs(Some(21), 460, RING)), Some(21));
        assert_eq!(c, None, "a move starts the evidence afresh");
    }

    #[test]
    fn a_disagreeing_window_becomes_the_new_candidate() {
        let mut c = None;
        track_step(&mut c, obs(Some(20), 460, 0));
        assert_eq!(track_step(&mut c, obs(Some(35), 460, RING)), None);
        assert_eq!(c.map(|c| c.residual), Some(35));
        assert_eq!(track_step(&mut c, obs(Some(35), 460, 2 * RING)), Some(35));
    }

    #[test]
    fn zero_or_no_residual_clears_the_candidate() {
        for clear in [Some(0), None] {
            let mut c = None;
            track_step(&mut c, obs(Some(20), 460, 0));
            assert_eq!(track_step(&mut c, obs(clear, 460, HOP)), None);
            assert_eq!(c, None);
            assert_eq!(
                track_step(&mut c, obs(Some(20), 460, RING)),
                None,
                "{clear:?}"
            );
        }
    }

    /// A residual measured against another held delay says nothing about
    /// this one.
    #[test]
    fn a_changed_held_delay_restarts_the_evidence() {
        let mut c = None;
        track_step(&mut c, obs(Some(20), 460, 0));
        assert_eq!(track_step(&mut c, obs(Some(20), 450, RING)), None);
        assert_eq!(c.map(|c| c.delay), Some(450));
    }
}
