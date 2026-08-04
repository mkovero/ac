//! The transfer view's fault indicator (#228) — the six-state table from
//! `handoff-lock-and-smoothing.md`, as a pure function of the frame plus a
//! little carried time state.
//!
//! # What this is for
//!
//! Four distinct failures currently present identically: as "the top end
//! looks wrong". A dead reference leg cost a whole rig session with nothing
//! on screen saying so. Each state here names one cause with one action —
//! "something is wrong" would leave the operator guessing between them.
//!
//! | condition | state | cause |
//! |---|---|---|
//! | not driving, legs quiet | *(nothing)* | idle, expected |
//! | driving, reference leg at floor | [`Fault::NoReference`] | #225, misrouted or unpatched |
//! | driving, measurement leg at floor | [`Fault::NoSignal`] | mic unplugged, DUT off, wrong input |
//! | both legs live, estimator refusing, lock held earlier | [`Fault::LostLock`] | delay fault |
//! | both legs live, estimator refusing, never locked | [`Fault::NoLockYet`] / [`Fault::NoLock`] | delay fault |
//! | both legs live, coherence low everywhere | [`Fault::CheckRouting`] | legs carry different sources |
//! | after a successful lock | [`Fault::LockAcquired`] | transient confirmation |
//!
//! # Drive state gates the level rows and nothing else
//!
//! Only the two level rows read the drive: a leg at the floor is a fault
//! only when something should have been reaching it, which is why the first
//! row exists and why a session that never drives reports neither of them.
//!
//! The lock rows do not read it. Two legs above the floor are carrying
//! signal whoever put it there, so a refusal on a fully passive external-DUT
//! session is as real as one on a driving session — and less recoverable,
//! since the operator cannot resolve it by starting the stimulus. Row 1's
//! "idle, expected" is about a drivable session sitting silent, not about
//! non-drivable sessions in general.
//!
//! # A lock fault is read, not inferred
//!
//! The original table discriminated a bad lock by "both legs live, HF
//! collapsed, LF fine" — stage 0 coherence at 0.05 against 0.715-0.755 for a
//! good lock. That was designed when a refusal did not exist and the only
//! evidence of a bad lock was its downstream effect. #227 makes the estimator
//! say so itself, so this module reads [`WireFrame::delay_locked`]: coherence
//! is the symptom, the flag is the cause.
//!
//! Dropping the coherence discriminator also removes the hardest threshold in
//! the set. Stage 0 sits at 0.755 legitimately in a live room — flat to 0.006
//! across 20 dB of input gain, so it is reverberation-limited, not
//! noise-limited — and any threshold set from an electrical loopback would
//! flag a healthy acoustic measurement as faulty.
//!
//! [`Fault::CheckRouting`] still reads coherence, but on a different
//! question: *every* column below the display's own mask threshold, which is
//! two legs carrying unrelated sources rather than a misaligned one. A bad
//! lock does not trip it — stage 2 reads 0.93 either way.
//!
//! # Nothing here gates on `delay_prominence`
//!
//! `ZMQ.md` documents it "Diagnostic only: nothing downstream may gate on it,
//! since the threshold is the estimator's to own." Silently gating on a value
//! the estimator owns — including reading its null-versus-present to separate
//! warmup from refusal — is how the two ends drift apart.
//!
//! [`WireFrame::delay_attempts`] is the field this module reads instead, and
//! the distinction is worth stating because it is not self-evident. A count
//! of completed estimates carries no threshold: it says the estimator has
//! answered, not how close the answer came. The estimator can change
//! `NOISE_FLOOR_PROMINENCE`, its search range, its peak rule, or refuse for a
//! reason not yet invented, and every statement this module makes stays true.
//! Inferring the same thing from `delay_evidence` being non-null would couple
//! the indicator to *when the estimator chooses to publish evidence*, which is
//! exactly the internal the rule protects. See `ZMQ.md`'s `delay_attempts`
//! entry, which carries the same argument next to the rule itself.
//!
//! # Warmup must not cry wolf
//!
//! `delay_locked: false` is also what a pair publishes while warming up. A
//! fault indicator that fires on every healthy startup gets ignored, which
//! defeats the point of having one, so no lock-derived state paints until the
//! pair has been asked the question at least once.
//!
//! Originally that gate was [`FaultFrame::settled`] alone — the frame's `mtw`
//! columns, absent until every rung holds a full N blocks. **That made both
//! refusal states unreachable** (#238): the daemon builds the ladder only
//! *after* a lock, so a refusing pair never settles, and a locked pair never
//! returns `delay_locked: false`. A refusing session rendered a blank window
//! with no indicator at all — the exact failure the module exists to prevent,
//! and it survived because "settled" was assumed to be a property of the
//! session rather than of the lock.
//!
//! So the gate is now "settled **or** the estimator has completed an attempt".
//! Both are observed, neither is timed from session start:
//!
//! - a locked pair settles when its ladder does, unchanged;
//! - a refusing pair is gated by [`FaultFrame::estimator_attempted`], which
//!   turns true on the first completed estimate. That estimate runs only once
//!   the rings hold a full Welch segment, so the refusal clock still starts
//!   from the first moment a lock was *possible*, not from t=0.
//!
//! # `CHECK ROUTING` remains a post-lock state, deliberately
//!
//! Unrelated legs make the estimator refuse, so they never produce a ladder,
//! so [`Fault::CheckRouting`] cannot fire on the session that most obviously
//! deserves it. That is a known gap, not an oversight — and it is **not**
//! closed by falling back to the frame's Welch `coherence` when `mtw` is
//! absent. Those two arrays are different measurements: three ladder stages
//! with per-stage block counts against a single Welch path with a different
//! bin count and a different bias floor. [`coherence_dead`]'s threshold was
//! measured against the ladder's columns; applied to the other array it is a
//! different test wearing the same name, and a silently-selected source for a
//! displayed quantity is the shape that let the original display defect
//! survive a rewrite.
//!
//! A refusing session reaches the operator through [`Fault::NoLock`], whose
//! instruction is "check mic placement and routing" — which sends them to the
//! same place. Closing the gap properly means validating the threshold against
//! both sources, and that is a rig measurement, not a code change.
//!
//! # Why the daemon does not simply build the ladder unaligned
//!
//! Recorded so it is not revisited as the obvious simplification: building the
//! ladder at delay 0 before a lock would make `settled` true for a refusing
//! pair with no new wire field. It would also publish a transfer curve
//! computed against an unaligned reference, which at any real acoustic delay
//! collapses the top end to nothing. That is a confident wrong display — the
//! failure mode three rig sessions have gone into removing — and a blank
//! window under a `NO LOCK` banner is the honest alternative.

use crate::transfer::COHERENCE_THRESHOLD;
use crate::wire::WireFrame;

/// "At the floor", in dBFS. Absolute and generous: far below any usable
/// measurement, so it will not fire on a quiet but valid session.
///
/// Deliberately **not** relative to the other leg. Levels legitimately
/// differ — by 15 dB on the rig that found this, a mic at −30 dBFS peak
/// against a reference at −14.5 dBFS — so a relative test would misfire on a
/// perfectly good session.
pub const SIGNAL_FLOOR_DBFS: f64 = -80.0;

/// How long the ladder takes to settle, in seconds: four independent 1.024 s
/// windows at the bottom rung (`design-mtw-ladder.md`, stage 2).
///
/// Recorded because [`PERSISTENT_REFUSAL_S`]'s original anchor was reasoned
/// from it, and because the size of the anchor change is this number. Not
/// used as a timer — see [`FaultFrame::settled`].
pub const LADDER_SETTLE_S: f64 = 2.560;

/// A refusal still standing this many seconds **after the estimator first
/// refused** is persistent rather than transient.
///
/// #227 retries at 1 Hz, so this is roughly ten retries past the first point
/// at which a lock was even possible, and anything past a handful of retries
/// is genuinely persistent. The clock does not start at session start: a timer
/// from t=0 would fire on healthy sessions, since warmup is indistinguishable
/// from refusal on the wire.
///
/// # The anchor moved, deliberately (#238)
///
/// The original wording was "10 s **after the ladder settles**". That anchor
/// is undefined in the case it was written for: a pair that never locks never
/// builds a ladder, which is the defect #238 fixes. It survived review because
/// settle was the only observable that stood in for "a lock was possible by
/// now".
///
/// [`WireFrame::delay_attempts`] observes that directly, so the clock is now
/// anchored on the first refused attempt. The consequences, both accepted:
///
/// - **A pair that never locks** reaches [`Fault::NoLock`] at 10 s from its
///   first refused attempt — [`LADDER_SETTLE_S`] earlier than the settle-
///   anchored text implied, since the first attempt lands on the first
///   published frame and settle would have been 2.56 s after it. Ten retries
///   past the first possible lock is what the paragraph above actually asks
///   for, and this delivers it; the settle anchor was the proxy, not the
///   intent.
/// - **A pair that locks, settles, then refuses** is anchored on that
///   refusal, which is later than either reading of the original text and is
///   the only sensible origin for a lock that was held and lost.
///
/// The number is arguable — argue it against the rig. What is not open is
/// leaving either the number or the anchor unset, which decides them silently
/// and re-litigates them later.
pub const PERSISTENT_REFUSAL_S: f64 = 10.0;

/// How long [`Fault::LockAcquired`] stays up, in scene seconds. It is a
/// transient confirmation, not a state.
pub const LOCK_ACQUIRED_HOLD_S: f64 = 3.0;

/// Whether a state is a problem or a confirmation. The renderer picks a
/// colour from this rather than matching on the variant, so a state added
/// later cannot be drawn in the wrong register by omission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Something is wrong and the operator must act.
    Fault,
    /// Something went right. Transient.
    Confirmation,
}

/// One row of the indicator table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// Driving, and the reference leg is at the floor.
    NoReference,
    /// Driving, and the measurement leg is at the floor.
    NoSignal,
    /// Both legs live, and not one column clears the display's coherence
    /// mask — the legs are carrying unrelated sources.
    CheckRouting,
    /// The estimator is refusing to lock **after having held a lock**, and
    /// has been for less than [`PERSISTENT_REFUSAL_S`]. Retries are still
    /// plausibly going to succeed, so this says what is happening and no
    /// more.
    ///
    /// Requires a lock to have existed. A pair that has never locked gets
    /// [`Fault::NoLockYet`] instead — see its docs.
    LostLock,
    /// The estimator has refused every attempt so far, and the session has
    /// never held a lock, for less than [`PERSISTENT_REFUSAL_S`].
    ///
    /// Exists because [`Fault::LostLock`] asserts something untrue about a
    /// pair that never locked: nothing was lost. It is a transient row like
    /// `LostLock` — retries may still succeed — so it carries no instruction,
    /// and it shares the `NO LOCK` label with [`Fault::NoLock`] deliberately.
    /// The two rows make the same true statement about the pair; what
    /// escalation adds is the instruction, not a different claim.
    ///
    /// The words are confirmed operator-side (Markus, 2026-08-04): `NO LOCK`
    /// states both the status and the target, and is what a refusing session
    /// should read from its first second. Not open unless the rig says
    /// otherwise.
    NoLockYet,
    /// The estimator has been refusing for longer than
    /// [`PERSISTENT_REFUSAL_S`]. A mic at 3 m off-axis may never lock, so
    /// the operator needs somewhere to go rather than a message that reads
    /// as a passing glitch. See [`Fault::detail`] for why that is a list of
    /// things to check rather than a diagnosis.
    NoLock,
    /// A lock was just acquired. Held for [`LOCK_ACQUIRED_HOLD_S`].
    LockAcquired,
}

impl Fault {
    /// The banner text. `ac-view` draws this verbatim and must never
    /// reformat it.
    pub fn label(&self) -> &'static str {
        match self {
            Fault::NoReference => "NO REFERENCE",
            Fault::NoSignal => "NO SIGNAL",
            Fault::CheckRouting => "CHECK ROUTING",
            Fault::LostLock => "LOST LOCK",
            // Same words as `NoLock`, on purpose: both say the pair has no
            // lock, which is true of each. The escalation carries an
            // instruction, not a different claim.
            Fault::NoLockYet | Fault::NoLock => "NO LOCK",
            Fault::LockAcquired => "LOCK ACQUIRED",
        }
    }

    /// The action, where the label alone does not imply one. `None` where
    /// it does, or where there is nothing for the operator to do yet.
    ///
    /// **A detail may name what to check; it may not assert a cause.** The
    /// level rows can afford to be specific — the frame says which leg is at
    /// the floor, and the table's causes for that leg are established. The
    /// refusal rows cannot: a refusal means the estimator found no
    /// sufficiently prominent peak, which is equally consistent with a mic
    /// too far off-axis, with legs carrying unrelated sources, and with a
    /// path that genuinely has nothing to correlate. Naming one of those
    /// would send the operator to the wrong end of the room with the
    /// display's authority behind it.
    pub fn detail(&self) -> Option<&'static str> {
        match self {
            Fault::NoReference => Some("reference leg silent — check the output patch"),
            Fault::NoSignal => {
                Some("measurement leg silent — check the mic, the DUT, and the input")
            }
            Fault::CheckRouting => Some("the two legs carry unrelated sources"),
            // Transient by construction: #227 is still retrying, and sending
            // the operator to check something that is about to lock on its
            // own is worse than saying nothing. `NoLock` is where the
            // instruction lives.
            Fault::LostLock => None,
            // Transient for the same reason `LostLock` is: #227 is still
            // retrying, and the first seconds of a session that has not
            // locked yet are not evidence that it never will.
            Fault::NoLockYet => None,
            Fault::NoLock => Some("check mic placement and routing"),
            Fault::LockAcquired => None,
        }
    }

    pub fn severity(&self) -> Severity {
        match self {
            Fault::LockAcquired => Severity::Confirmation,
            _ => Severity::Fault,
        }
    }
}

/// The [`crate::wire::WireDrive`] fields this module actually uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriveState {
    pub on: bool,
    /// Carried for completeness of the "could drive and is not" versus
    /// "never drives" distinction. The level rows key on [`Self::on`],
    /// which is already `false` for a session that is not drivable.
    pub drivable: bool,
}

/// The frame-derived indicator inputs that [`crate::TransferInput`] does not
/// already carry, bundled so the display intermediate grows one field rather
/// than three.
///
/// Its presence is itself the top-level gate: a snapshot derivation and a
/// daemon predating #228 both produce `None`, and the indicator stays silent
/// for both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultFrame {
    pub drive: DriveState,
    /// `Some(false)` is a positive refusal-or-warmup statement; `None` is a
    /// daemon predating #227, which permits no lock state at all.
    pub delay_locked: Option<bool>,
    /// Whether the ladder has settled, observed from the frame rather than
    /// timed: the daemon withholds the `mtw` columns until every rung holds
    /// a full N blocks.
    ///
    /// True only after a lock, since the daemon builds the ladder from the
    /// alignment offset — which is why this cannot be the only warmup gate.
    /// See [`Self::estimator_attempted`].
    pub settled: bool,
    /// Whether the estimator has completed at least one delay estimate on
    /// this pair, accepted or refused (`delay_attempts > 0`).
    ///
    /// The refusing pair's equivalent of [`Self::settled`]: it is what makes
    /// `delay_locked: false` mean "asked and refused" rather than "not asked
    /// yet". False on a daemon predating #238, which leaves such a daemon's
    /// refusals as unreachable as they were — absence is not evidence that
    /// the estimator ran.
    ///
    /// Once true, true for the session: the count behind it is monotone
    /// (`ZMQ.md`), and #226's re-locking must not change that. A count that
    /// reset would take a locked-then-refusing pair back to "warming up",
    /// and warmup paints nothing.
    pub estimator_attempted: bool,
}

impl FaultFrame {
    /// `None` when the daemon does not report its drive state — see
    /// [`WireFrame::drive`].
    pub fn from_wire_frame(frame: &WireFrame) -> Option<FaultFrame> {
        let drive = frame.drive.as_ref()?;
        Some(FaultFrame {
            drive: DriveState {
                on: drive.on,
                drivable: drive.drivable,
            },
            delay_locked: frame.delay_locked,
            // Same `lengths_agree` filter the display applies, so "settled"
            // and "there are columns on screen" cannot disagree.
            settled: frame.mtw.as_ref().filter(|m| m.lengths_agree()).is_some(),
            estimator_attempted: frame.delay_attempts > 0,
        })
    }
}

/// Everything one frame contributes to the indicator.
pub struct FaultInput<'a> {
    /// `None` disables the indicator entirely.
    pub frame: Option<FaultFrame>,
    pub meas_peak_dbfs: Option<f64>,
    pub ref_peak_dbfs: Option<f64>,
    /// The display's own columns — the ladder's, and **only** the ladder's.
    /// Empty before it settles, and empty for a pair that never locks.
    ///
    /// Do not fall back to [`WireFrame::coherence`] to fill it. See the
    /// module's `CHECK ROUTING` section: the Welch array is a different
    /// measurement with a different bin count and bias floor, and
    /// [`coherence_dead`]'s threshold was measured against the ladder's 504
    /// columns. Substituting the other array keeps the name and changes the
    /// test. `no_welch_fallback_fills_the_coherence_columns` pins this.
    pub coherence: &'a [f64],
}

impl<'a> FaultInput<'a> {
    /// Read one live frame. The coherence columns come from `mtw` — the
    /// display's source — and their presence is what marks the ladder
    /// settled, so both fall out of one lookup.
    ///
    /// `mtw` absent means no columns, full stop. The frame's own `coherence`
    /// is deliberately not consulted here — see [`Self::coherence`].
    pub fn from_wire_frame(frame: &'a WireFrame) -> FaultInput<'a> {
        FaultInput {
            frame: FaultFrame::from_wire_frame(frame),
            meas_peak_dbfs: frame.meas_peak_dbfs,
            ref_peak_dbfs: frame.ref_peak_dbfs,
            coherence: frame
                .mtw
                .as_ref()
                .filter(|m| m.lengths_agree())
                .map(|m| m.coherence.as_slice())
                .unwrap_or(&[]),
        }
    }
}

/// A capture peak is at the floor.
///
/// `None` is wire `null`, which is digital silence (−inf, which JSON cannot
/// represent) — at the floor. A NaN from a non-conforming producer is *not*
/// treated as silence: fabricating a fault out of a malformed frame is the
/// same class of error as drawing a trace from one.
fn at_floor(peak_dbfs: Option<f64>) -> bool {
    match peak_dbfs {
        None => true,
        Some(p) if p.is_nan() => false,
        Some(p) => p <= SIGNAL_FLOOR_DBFS,
    }
}

/// Not one column clears the display's coherence mask.
///
/// Reuses [`COHERENCE_THRESHOLD`] rather than introducing a second, tunable
/// number: the condition being named is "the display can draw nothing at
/// all", so the threshold that decides what is drawable is the one that
/// belongs here. It is also not loopback-derived — a healthy acoustic
/// measurement sits at 0.715-0.755 on stage 0 and 0.92+ below, an order of
/// magnitude clear.
fn coherence_dead(coherence: &[f64]) -> bool {
    !coherence.is_empty() && coherence.iter().all(|c| *c < COHERENCE_THRESHOLD)
}

/// The time-dependent part of the indicator, carried across frames the same
/// way [`crate::transfer::MeterState`] carries the meter hold and clip latch.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FaultState {
    /// When the current unbroken run of settled refusals began. Cleared by a
    /// lock, and by falling back out of settled.
    refusing_since_s: Option<f64>,
    /// When the last false→true lock transition happened.
    acquired_at_s: Option<f64>,
    /// Last observed `delay_locked`, to detect that transition.
    prev_locked: Option<bool>,
    /// Whether this pair has ever reported a lock. Picks the words for a
    /// refusal: nothing was *lost* by a pair that never had one.
    ever_locked: bool,
}

impl FaultState {
    /// Fold one frame in at scene time `now_s` and read the indicator out.
    /// `None` is "show nothing", which is the correct display for an idle
    /// session and for a warming-up one.
    ///
    /// Monotone in `now_s`; callers pass the scene clock, not wall time.
    ///
    /// # Order of the rows
    ///
    /// Level first, then lock, then coherence — the order the causes chain
    /// in. A dead reference leg makes everything downstream of it look
    /// wrong, so reporting the coherence it destroys instead would name the
    /// symptom while the cause sits one row up. For the same reason a
    /// refusal outranks [`Fault::CheckRouting`]: with #227 present, unrelated
    /// sources make the estimator refuse, and the flag is the more direct
    /// statement of it.
    pub fn update(&mut self, input: &FaultInput, now_s: f64) -> Option<Fault> {
        // A daemon that does not report its own drive gives no ground for
        // any claim about whether signal should be present. It also predates
        // the capture peaks, so treating its absent levels as silence would
        // paint NO SIGNAL on every frame it sends.
        let Some(frame) = input.frame else {
            self.reset();
            return None;
        };

        let ref_dead = at_floor(input.ref_peak_dbfs);
        let meas_dead = at_floor(input.meas_peak_dbfs);

        // Lock bookkeeping runs on every frame, whatever is reported. A
        // refusal that spans a period of silence is still one unbroken
        // refusal, and the acquisition transient must not be missed because
        // a louder row was showing when it happened.
        // Either gate is enough, and each covers a case the other cannot:
        // `settled` is the only one a locked-then-lost pair can satisfy (the
        // ladder outlives the lock), and `estimator_attempted` is the only one
        // a pair that has never locked can satisfy (there is no ladder without
        // an offset). Requiring both is what made this unreachable — #238.
        let asked = frame.settled || frame.estimator_attempted;
        let refusing = asked && frame.delay_locked == Some(false);
        if refusing {
            self.refusing_since_s.get_or_insert(now_s);
        } else {
            self.refusing_since_s = None;
        }
        if frame.delay_locked == Some(true) {
            self.ever_locked = true;
            if self.prev_locked == Some(false) {
                self.acquired_at_s = Some(now_s);
            }
        }
        if frame.delay_locked.is_some() {
            self.prev_locked = frame.delay_locked;
        }

        if ref_dead || meas_dead {
            // A quiet leg is only a fault when something should be reaching
            // it. Not driving: idle and expected — and for a session that is
            // not drivable at all, daemon silence says nothing about the
            // inputs, so there is nothing to report either way.
            //
            // This gate covers the two level rows and **only** them. Drive
            // state is not evidence about the lock: two legs both above the
            // floor are carrying signal whoever put it there, so an
            // external-DUT session below still gets its lock rows. Arguably
            // it needs them more than a driving one does, since the operator
            // cannot resolve it by starting the stimulus.
            if !frame.drive.on {
                return None;
            }
            // Both dead reports the reference. It is the daemon's own leg
            // and its failure explains the other; naming the measurement leg
            // first would send the operator to the mic for a patching fault.
            return Some(if ref_dead {
                Fault::NoReference
            } else {
                Fault::NoSignal
            });
        }

        // Both legs live from here.

        if let Some(since) = self.refusing_since_s {
            if now_s - since >= PERSISTENT_REFUSAL_S {
                return Some(Fault::NoLock);
            }
            // The transient row's words follow the history, not the clock.
            // `LOST LOCK` on a pair that never locked asserts something
            // untrue, and the whole premise of #228 is that the words name
            // the fault.
            return Some(if self.ever_locked {
                Fault::LostLock
            } else {
                Fault::NoLockYet
            });
        }

        if let Some(at) = self.acquired_at_s {
            if now_s - at < LOCK_ACQUIRED_HOLD_S {
                return Some(Fault::LockAcquired);
            }
        }

        if coherence_dead(input.coherence) {
            return Some(Fault::CheckRouting);
        }

        None
    }

    fn reset(&mut self) {
        *self = FaultState::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn driving() -> FaultFrame {
        FaultFrame {
            drive: DriveState {
                on: true,
                drivable: true,
            },
            delay_locked: Some(true),
            settled: true,
            estimator_attempted: true,
        }
    }

    /// A driving session with both legs live, settled, and locked — the
    /// healthy baseline every case below perturbs one field of. The peaks
    /// are the rig's own, 15 dB apart.
    fn healthy(coherence: &[f64]) -> FaultInput<'_> {
        FaultInput {
            frame: Some(driving()),
            meas_peak_dbfs: Some(-30.0),
            ref_peak_dbfs: Some(-14.5),
            coherence,
        }
    }

    #[test]
    fn a_healthy_settled_session_shows_nothing() {
        // The rig's real figures: stage 0 reverberation-limited at 0.755,
        // the lower rungs well above it.
        let coh = [0.755, 0.92, 0.93];
        let mut st = FaultState::default();
        assert_eq!(st.update(&healthy(&coh), 0.0), None);
    }

    #[test]
    fn a_daemon_without_drive_state_shows_nothing_at_all() {
        let mut st = FaultState::default();
        let inp = FaultInput {
            frame: None,
            // An older daemon has no capture peaks either, which would read
            // as silence on both legs if the indicator ran at all.
            meas_peak_dbfs: None,
            ref_peak_dbfs: None,
            coherence: &[],
        };
        assert_eq!(st.update(&inp, 0.0), None);
    }

    #[test]
    fn a_dead_reference_leg_while_driving_is_no_reference() {
        let coh = [0.755, 0.92];
        let mut st = FaultState::default();
        let inp = FaultInput {
            ref_peak_dbfs: None, // digital silence
            ..healthy(&coh)
        };
        assert_eq!(st.update(&inp, 0.0), Some(Fault::NoReference));
    }

    #[test]
    fn a_dead_measurement_leg_while_driving_is_no_signal() {
        let coh = [0.755, 0.92];
        let mut st = FaultState::default();
        let inp = FaultInput {
            meas_peak_dbfs: Some(-95.0),
            ..healthy(&coh)
        };
        assert_eq!(st.update(&inp, 0.0), Some(Fault::NoSignal));
    }

    #[test]
    fn both_legs_dead_names_the_reference_not_the_mic() {
        let coh = [0.755, 0.92];
        let mut st = FaultState::default();
        let inp = FaultInput {
            meas_peak_dbfs: None,
            ref_peak_dbfs: None,
            ..healthy(&coh)
        };
        assert_eq!(st.update(&inp, 0.0), Some(Fault::NoReference));
    }

    /// The floor is absolute, never relative. The rig's own 15 dB leg
    /// imbalance is a valid session and must show nothing.
    #[test]
    fn a_fifteen_db_leg_imbalance_is_not_a_fault() {
        let coh = [0.755, 0.92];
        let mut st = FaultState::default();
        assert_eq!(st.update(&healthy(&coh), 0.0), None);
    }

    /// A quiet but valid session. −79 dBFS is unusable in practice but it is
    /// above the floor, and the floor's job is to catch *nothing at all*.
    #[test]
    fn a_very_quiet_but_present_leg_is_not_at_the_floor() {
        let coh = [0.755];
        let mut st = FaultState::default();
        let inp = FaultInput {
            meas_peak_dbfs: Some(-79.0),
            ..healthy(&coh)
        };
        assert_eq!(st.update(&inp, 0.0), None);
        // And the boundary itself is inclusive.
        let inp = FaultInput {
            meas_peak_dbfs: Some(SIGNAL_FLOOR_DBFS),
            ..healthy(&coh)
        };
        assert_eq!(st.update(&inp, 0.0), Some(Fault::NoSignal));
    }

    #[test]
    fn silence_on_an_idle_session_shows_nothing() {
        let mut st = FaultState::default();
        let inp = FaultInput {
            frame: Some(FaultFrame {
                drive: DriveState {
                    on: false,
                    drivable: true,
                },
                delay_locked: None,
                settled: false,
                estimator_attempted: false,
            }),
            meas_peak_dbfs: None,
            ref_peak_dbfs: None,
            coherence: &[],
        };
        assert_eq!(st.update(&inp, 0.0), None);
    }

    /// A fully passive external-DUT session never drives, so silence on its
    /// inputs says nothing and neither level row may fire.
    #[test]
    fn a_non_drivable_session_gets_no_level_row() {
        let mut st = FaultState::default();
        let inp = FaultInput {
            frame: Some(FaultFrame {
                drive: DriveState {
                    on: false,
                    drivable: false,
                },
                delay_locked: Some(false),
                settled: false,
                // The estimator has run and refused, so the suppression
                // below is the drive gate doing its job, not the warmup
                // gate hiding the case by accident.
                estimator_attempted: true,
            }),
            meas_peak_dbfs: None,
            ref_peak_dbfs: None,
            coherence: &[],
        };
        assert_eq!(st.update(&inp, 0.0), None);
    }

    /// But the lock rows are not about driving. An external-DUT session with
    /// both legs live and the estimator refusing is a real fault, and one the
    /// operator cannot resolve by starting the stimulus — so suppressing it
    /// on `drivable: false` would hide the case that needs it most.
    #[test]
    fn a_non_drivable_session_still_gets_its_lock_rows() {
        let coh = [0.755, 0.92];
        let mut st = FaultState::default();
        let inp = FaultInput {
            frame: Some(FaultFrame {
                drive: DriveState {
                    on: false,
                    drivable: false,
                },
                delay_locked: Some(false),
                settled: true,
                estimator_attempted: true,
            }),
            ..healthy(&coh)
        };
        assert_eq!(st.update(&inp, 0.0), Some(Fault::NoLockYet));
        assert_eq!(st.update(&inp, PERSISTENT_REFUSAL_S), Some(Fault::NoLock));
    }

    /// The same for the other two both-legs-live rows: neither reads drive.
    #[test]
    fn a_non_drivable_session_still_gets_check_routing_and_the_confirmation() {
        let dead = [0.1, 0.08];
        let passive = |delay_locked| FaultFrame {
            drive: DriveState {
                on: false,
                drivable: false,
            },
            delay_locked,
            settled: true,
            estimator_attempted: true,
        };
        let mut st = FaultState::default();
        let inp = FaultInput {
            frame: Some(passive(Some(true))),
            ..healthy(&dead)
        };
        assert_eq!(st.update(&inp, 0.0), Some(Fault::CheckRouting));

        let coh = [0.755, 0.92];
        let mut st = FaultState::default();
        let refusing = FaultInput {
            frame: Some(passive(Some(false))),
            ..healthy(&coh)
        };
        let locked = FaultInput {
            frame: Some(passive(Some(true))),
            ..healthy(&coh)
        };
        assert_eq!(st.update(&refusing, 0.0), Some(Fault::NoLockYet));
        assert_eq!(st.update(&locked, 1.0), Some(Fault::LockAcquired));
    }

    /// The refusal rows must not name a cause. A refusal is equally
    /// consistent with an off-axis mic, with unrelated sources, and with a
    /// path that has nothing to correlate — which matters more once #227
    /// lands and unrelated sources reach the operator through `NO LOCK`
    /// rather than through `CHECK ROUTING`.
    #[test]
    fn the_refusal_rows_name_what_to_check_not_why() {
        let detail = Fault::NoLock.detail().expect("has an instruction");
        for asserted in ["closer", "on-axis", "unrelated", "too far", "unplugged"] {
            assert!(
                !detail.contains(asserted),
                "NO LOCK's detail asserts a cause it cannot know: {detail:?}"
            );
        }
        // It still has to send the operator somewhere.
        assert!(detail.contains("mic") && detail.contains("routing"));
        assert_eq!(Fault::LostLock.detail(), None);
    }

    /// The whole reason lock states wait: `delay_locked: false` before the
    /// estimator has run is warmup, indistinguishable from a refusal on the
    /// flag alone, and an indicator that fires on every healthy startup gets
    /// ignored.
    #[test]
    fn warmup_does_not_paint_a_lock_fault() {
        let mut st = FaultState::default();
        let warming = FaultInput {
            frame: Some(FaultFrame {
                delay_locked: Some(false),
                settled: false,
                // Rings not yet full: no estimate has been attempted, so
                // there is nothing to call a refusal.
                estimator_attempted: false,
                ..driving()
            }),
            coherence: &[],
            ..healthy(&[])
        };
        // Well past PERSISTENT_REFUSAL_S in scene time — still nothing,
        // because the clock has not started.
        assert_eq!(st.update(&warming, 0.0), None);
        assert_eq!(st.update(&warming, 30.0), None);
    }

    /// #238's regression test. A pair that never locks never gets a ladder,
    /// so `settled` is false forever — and gating on it alone left `LOST
    /// LOCK` and `NO LOCK` unreachable, which is what put a blank window in
    /// front of an operator for a whole rig session.
    #[test]
    fn a_refusal_that_never_settles_still_paints() {
        let mut st = FaultState::default();
        let refusing = FaultInput {
            frame: Some(FaultFrame {
                delay_locked: Some(false),
                // No lock, so no ladder, so no columns — for the whole
                // session.
                settled: false,
                estimator_attempted: true,
                ..driving()
            }),
            coherence: &[],
            ..healthy(&[])
        };
        assert_eq!(st.update(&refusing, 0.0), Some(Fault::NoLockYet));
        assert_eq!(
            st.update(&refusing, PERSISTENT_REFUSAL_S),
            Some(Fault::NoLock)
        );
    }

    /// A daemon predating #238 publishes no attempt count, and absence is not
    /// evidence that the estimator ran. Such a session stays as silent as it
    /// was — a wrong banner on an old daemon would be worse than the gap.
    #[test]
    fn a_daemon_without_the_attempt_count_paints_no_refusal() {
        let mut st = FaultState::default();
        let old = FaultInput {
            frame: Some(FaultFrame {
                delay_locked: Some(false),
                settled: false,
                estimator_attempted: false,
                ..driving()
            }),
            coherence: &[],
            ..healthy(&[])
        };
        assert_eq!(st.update(&old, 0.0), None);
        assert_eq!(st.update(&old, 100.0), None);
    }

    /// And the refusal clock starts when the estimator first answers, not at
    /// session start: a timer from t=0 would fire NO LOCK on a session that
    /// was still filling its rings.
    #[test]
    fn the_refusal_clock_starts_at_the_first_attempt_not_at_session_start() {
        let coh = [0.755, 0.92];
        let mut st = FaultState::default();
        let warming = FaultInput {
            frame: Some(FaultFrame {
                delay_locked: Some(false),
                settled: false,
                estimator_attempted: false,
                ..driving()
            }),
            coherence: &[],
            ..healthy(&[])
        };
        for t in 0..30 {
            assert_eq!(st.update(&warming, t as f64), None);
        }
        // The first frame carrying a completed attempt, at t=30, is where
        // the clock starts — here on a pair that goes on to lock and settle.
        let refusing = FaultInput {
            frame: Some(FaultFrame {
                delay_locked: Some(false),
                ..driving()
            }),
            ..healthy(&coh)
        };
        assert_eq!(st.update(&refusing, 30.0), Some(Fault::NoLockYet));
        assert_eq!(st.update(&refusing, 39.9), Some(Fault::NoLockYet));
        assert_eq!(st.update(&refusing, 40.0), Some(Fault::NoLock));
    }

    /// The transient row's words follow the history. `LOST LOCK` on a pair
    /// that never locked asserts something untrue, and a fault indicator
    /// whose words are wrong is what #228 exists to replace.
    #[test]
    fn a_pair_that_never_locked_is_not_told_it_lost_a_lock() {
        let coh = [0.755, 0.92];
        let refusing = |st: &mut FaultState, t: f64| {
            let inp = FaultInput {
                frame: Some(FaultFrame {
                    delay_locked: Some(false),
                    ..driving()
                }),
                ..healthy(&coh)
            };
            st.update(&inp, t)
        };

        // Never locked: the transient row says NO LOCK, without the
        // instruction the persistent row carries.
        let mut fresh = FaultState::default();
        assert_eq!(refusing(&mut fresh, 0.0), Some(Fault::NoLockYet));
        assert_eq!(Fault::NoLockYet.label(), "NO LOCK");
        assert_eq!(Fault::NoLockYet.detail(), None);
        assert_eq!(
            refusing(&mut fresh, PERSISTENT_REFUSAL_S),
            Some(Fault::NoLock)
        );

        // Locked earlier, refusing now: LOST LOCK is true, and only here.
        let mut held = FaultState::default();
        assert_eq!(held.update(&healthy(&coh), 0.0), None);
        assert_eq!(refusing(&mut held, 1.0), Some(Fault::LostLock));
        assert_eq!(Fault::LostLock.label(), "LOST LOCK");
    }

    /// A lock earlier in the session is what licenses `LOST LOCK` later. The
    /// history is carried, not re-read from the current frame.
    #[test]
    fn the_words_follow_the_history_not_the_current_frame() {
        let coh = [0.755, 0.92];
        let mut st = FaultState::default();
        let refusing = FaultInput {
            frame: Some(FaultFrame {
                delay_locked: Some(false),
                ..driving()
            }),
            ..healthy(&coh)
        };
        assert_eq!(st.update(&refusing, 0.0), Some(Fault::NoLockYet));
        assert_eq!(st.update(&healthy(&coh), 1.0), Some(Fault::LockAcquired));
        assert_eq!(st.update(&refusing, 5.0), Some(Fault::LostLock));
    }

    /// The settled gate still stands on its own, for the case the attempt
    /// count cannot cover: a pair that locked, settled, and then lost the
    /// lock keeps its ladder, and its refusal must still paint.
    #[test]
    fn a_settled_pair_that_loses_its_lock_still_paints() {
        let coh = [0.755, 0.92];
        let mut st = FaultState::default();
        assert_eq!(st.update(&healthy(&coh), 0.0), None);
        let lost = FaultInput {
            frame: Some(FaultFrame {
                delay_locked: Some(false),
                settled: true,
                estimator_attempted: false,
                ..driving()
            }),
            ..healthy(&coh)
        };
        assert_eq!(st.update(&lost, 1.0), Some(Fault::LostLock));
    }

    #[test]
    fn a_persistent_refusal_gets_different_words_than_a_transient_one() {
        let coh = [0.755, 0.92];
        let mut st = FaultState::default();
        let refusing = FaultInput {
            frame: Some(FaultFrame {
                delay_locked: Some(false),
                ..driving()
            }),
            ..healthy(&coh)
        };
        assert_eq!(st.update(&refusing, 0.0), Some(Fault::NoLockYet));
        assert_eq!(
            st.update(&refusing, PERSISTENT_REFUSAL_S),
            Some(Fault::NoLock)
        );
        // The transient one deliberately carries no instruction; the
        // persistent one carries the one the operator needs.
        assert_eq!(Fault::NoLockYet.detail(), None);
        assert!(Fault::NoLock.detail().is_some());
        // The escalation adds the instruction; it does not change the claim,
        // so the two rows share their label on purpose.
        assert_eq!(Fault::NoLockYet.label(), Fault::NoLock.label());
        assert_ne!(Fault::LostLock.label(), Fault::NoLock.label());
    }

    /// A refusal that spans a stretch of silence is one unbroken refusal —
    /// the clock must not restart when a louder row takes the screen.
    #[test]
    fn a_louder_row_does_not_restart_the_refusal_clock() {
        let coh = [0.755, 0.92];
        let mut st = FaultState::default();
        let refusing = FaultInput {
            frame: Some(FaultFrame {
                delay_locked: Some(false),
                ..driving()
            }),
            ..healthy(&coh)
        };
        assert_eq!(st.update(&refusing, 0.0), Some(Fault::NoLockYet));
        // Reference leg drops out, then comes back.
        let dead_ref = FaultInput {
            ref_peak_dbfs: None,
            frame: Some(FaultFrame {
                delay_locked: Some(false),
                ..driving()
            }),
            ..healthy(&coh)
        };
        assert_eq!(st.update(&dead_ref, 2.0), Some(Fault::NoReference));
        assert_eq!(st.update(&dead_ref, 5.0), Some(Fault::NoReference));
        // Back on the original clock, not a fresh one.
        assert_eq!(st.update(&refusing, 10.0), Some(Fault::NoLock));
    }

    #[test]
    fn a_successful_lock_shows_a_transient_confirmation() {
        let coh = [0.755, 0.92];
        let mut st = FaultState::default();
        let refusing = FaultInput {
            frame: Some(FaultFrame {
                delay_locked: Some(false),
                ..driving()
            }),
            ..healthy(&coh)
        };
        assert_eq!(st.update(&refusing, 0.0), Some(Fault::NoLockYet));
        assert_eq!(st.update(&healthy(&coh), 1.0), Some(Fault::LockAcquired));
        assert_eq!(
            st.update(&healthy(&coh), 1.0 + LOCK_ACQUIRED_HOLD_S - 0.1),
            Some(Fault::LockAcquired)
        );
        assert_eq!(st.update(&healthy(&coh), 1.0 + LOCK_ACQUIRED_HOLD_S), None);
        assert_eq!(Fault::LockAcquired.severity(), Severity::Confirmation);
        assert_eq!(Fault::NoLock.severity(), Severity::Fault);
    }

    /// A session that locks on its first settled frame never refused, so
    /// there is nothing to confirm.
    #[test]
    fn locking_without_a_prior_refusal_is_not_an_acquisition() {
        let coh = [0.755, 0.92];
        let mut st = FaultState::default();
        assert_eq!(st.update(&healthy(&coh), 0.0), None);
        assert_eq!(st.update(&healthy(&coh), 1.0), None);
    }

    #[test]
    fn legs_carrying_unrelated_sources_is_check_routing() {
        // Every column below the display's mask — nothing is drawable.
        let coh = [0.1, 0.08, 0.12];
        let mut st = FaultState::default();
        assert_eq!(st.update(&healthy(&coh), 0.0), Some(Fault::CheckRouting));
    }

    /// The rig's measured bad-lock shape: stage 0 collapsed, stage 2 intact.
    /// That is a delay fault, not a routing one, and the coherence row must
    /// not claim it. This is also why the row cannot be the lock
    /// discriminator it was originally written as.
    #[test]
    fn a_bad_lock_shape_is_not_check_routing() {
        let coh = [0.054, 0.77, 0.93];
        let mut st = FaultState::default();
        assert_eq!(st.update(&healthy(&coh), 0.0), None);
    }

    /// With #227 present, unrelated sources make the estimator refuse, and
    /// the flag is the more direct statement of the same fact.
    #[test]
    fn a_refusal_outranks_the_coherence_row() {
        let coh = [0.1, 0.08, 0.12];
        let mut st = FaultState::default();
        let inp = FaultInput {
            frame: Some(FaultFrame {
                delay_locked: Some(false),
                ..driving()
            }),
            ..healthy(&coh)
        };
        assert_eq!(st.update(&inp, 0.0), Some(Fault::NoLockYet));
    }

    /// `CHECK ROUTING` is a post-lock state, and #238 does not change that:
    /// a refusing pair has no ladder, so it has no coherence columns to
    /// evaluate, and the Welch array on the frame is a different measurement
    /// with a different bias floor — reading the threshold against it would
    /// be a different test under the same name. The operator still gets
    /// routing in `NO LOCK`'s instruction.
    #[test]
    fn a_refusing_session_reaches_routing_through_no_lock() {
        let mut st = FaultState::default();
        let refusing = FaultInput {
            frame: Some(FaultFrame {
                delay_locked: Some(false),
                settled: false,
                estimator_attempted: true,
                ..driving()
            }),
            // No ladder, so no columns, whatever the legs are carrying.
            coherence: &[],
            ..healthy(&[])
        };
        assert_eq!(st.update(&refusing, 0.0), Some(Fault::NoLockYet));
        assert_eq!(
            st.update(&refusing, PERSISTENT_REFUSAL_S),
            Some(Fault::NoLock)
        );
        assert!(Fault::NoLock
            .detail()
            .is_some_and(|d| d.contains("routing")));
    }

    /// Before #227 lands, `delay_locked` is absent. Every other row still
    /// works; no lock state may paint from absence.
    #[test]
    fn a_daemon_without_delay_locked_reports_no_lock_state() {
        let dead = [0.1, 0.08];
        let mut st = FaultState::default();
        let inp = FaultInput {
            frame: Some(FaultFrame {
                delay_locked: None,
                ..driving()
            }),
            ..healthy(&dead)
        };
        // The coherence row still fires...
        assert_eq!(st.update(&inp, 0.0), Some(Fault::CheckRouting));
        // ...and no length of time turns absence into a refusal.
        let coh = [0.755, 0.92];
        let inp = FaultInput {
            frame: Some(FaultFrame {
                delay_locked: None,
                ..driving()
            }),
            ..healthy(&coh)
        };
        assert_eq!(st.update(&inp, 100.0), None);
    }

    /// A malformed frame must not fabricate a fault, the same way a
    /// malformed frame draws no trace rather than a guessed one.
    #[test]
    fn a_nan_peak_is_not_read_as_silence() {
        let coh = [0.755, 0.92];
        let mut st = FaultState::default();
        let inp = FaultInput {
            meas_peak_dbfs: Some(f64::NAN),
            ..healthy(&coh)
        };
        assert_eq!(st.update(&inp, 0.0), None);
    }

    #[test]
    fn negative_infinity_is_at_the_floor() {
        assert!(at_floor(Some(f64::NEG_INFINITY)));
        assert!(at_floor(None));
        assert!(!at_floor(Some(-79.999)));
    }

    /// An empty coherence array is an unsettled ladder, not a dead one.
    #[test]
    fn no_columns_is_not_check_routing() {
        assert!(!coherence_dead(&[]));
    }

    /// The decision that `CHECK ROUTING` stays post-lock, pinned in code
    /// rather than in a PR body. A frame with no ladder contributes no
    /// coherence columns even when it carries a full Welch array — the
    /// threshold was measured against the ladder's columns, and quietly
    /// feeding it the other array would keep the name while changing the
    /// test.
    #[test]
    fn no_welch_fallback_fills_the_coherence_columns() {
        let json = r#"{
            "type": "transfer_stream",
            "delay_locked": false,
            "delay_attempts": 3,
            "meas_peak_dbfs": -30.0,
            "ref_peak_dbfs": -14.5,
            "meas_channel": 0,
            "ref_channel": 1,
            "sr": 48000,
            "coherence": [0.02, 0.03, 0.01, 0.04],
            "spec_freqs": [],
            "meas_spectrum": [],
            "ref_spectrum": [],
            "spl": null,
            "spl_weighting": "Z",
            "spl_integration": "fast",
            "drive": {"on": true, "level_dbfs": -30.0, "drivable": true}
        }"#;
        let frame: WireFrame = serde_json::from_str(json).expect("deserialize");
        assert_eq!(frame.coherence.len(), 4, "the Welch array is on the frame");
        let inp = FaultInput::from_wire_frame(&frame);
        assert!(
            inp.coherence.is_empty(),
            "the Welch array reached the indicator: {:?}",
            inp.coherence
        );

        // And the refusal is what the operator gets, not CHECK ROUTING.
        let mut st = FaultState::default();
        assert_eq!(st.update(&inp, 0.0), Some(Fault::NoLockYet));
        assert_eq!(st.update(&inp, PERSISTENT_REFUSAL_S), Some(Fault::NoLock));
    }

    /// #226 will add re-locking. If a re-lock ever resets `delay_attempts`,
    /// a pair that locked and then started refusing reads as one that has
    /// not been asked yet — and warmup paints nothing, which is the blank
    /// window this issue removed.
    #[test]
    fn a_relocking_pair_never_falls_back_to_warmup() {
        let coh = [0.755, 0.92];
        let mut st = FaultState::default();
        assert_eq!(st.update(&healthy(&coh), 0.0), None);
        // The wire contract says the count is monotone, so a pair that has
        // locked can only report more attempts, never zero.
        let refusing_after_lock = FaultInput {
            frame: Some(FaultFrame {
                delay_locked: Some(false),
                settled: false,
                estimator_attempted: true,
                ..driving()
            }),
            coherence: &[],
            ..healthy(&[])
        };
        assert_eq!(st.update(&refusing_after_lock, 1.0), Some(Fault::LostLock));
    }

    #[test]
    fn reads_a_live_frame_end_to_end() {
        let json = r#"{
            "type": "transfer_stream",
            "delay_ms": 5.9,
            "delay_locked": false,
            "meas_peak_dbfs": -30.0,
            "ref_peak_dbfs": -14.5,
            "meas_channel": 0,
            "ref_channel": 1,
            "sr": 48000,
            "spec_freqs": [],
            "meas_spectrum": [],
            "ref_spectrum": [],
            "spl": null,
            "spl_weighting": "Z",
            "spl_integration": "fast",
            "drive": {"on": true, "level_dbfs": -30.0, "drivable": true},
            "mtw": {
                "freqs": [100.0, 1000.0, 10000.0],
                "magnitude_db": [0.0, 0.0, 0.0],
                "phase_deg": [0.0, 0.0, 0.0],
                "coherence": [0.93, 0.77, 0.054]
            }
        }"#;
        let frame: WireFrame = serde_json::from_str(json).expect("deserialize");
        let inp = FaultInput::from_wire_frame(&frame);
        let f = inp.frame.expect("drive state present");
        assert!(f.settled, "mtw columns present means settled");
        assert_eq!(f.delay_locked, Some(false));
        assert!(f.drive.on);
        assert_eq!(inp.coherence.len(), 3);
        let mut st = FaultState::default();
        assert_eq!(st.update(&inp, 0.0), Some(Fault::NoLockYet));
        assert_eq!(
            st.update(&inp, PERSISTENT_REFUSAL_S),
            Some(Fault::NoLock),
            "a refusal standing 10 s past the first refused attempt is persistent"
        );
    }

    /// A frame from today's daemon — no `delay_locked`, no `drive` — must
    /// leave the indicator silent rather than paint from defaults.
    #[test]
    fn a_pre_228_frame_leaves_the_indicator_silent() {
        let json = r#"{
            "type": "transfer_stream",
            "delay_ms": 5.9,
            "meas_channel": 0,
            "ref_channel": 1,
            "sr": 48000,
            "spec_freqs": [],
            "meas_spectrum": [],
            "ref_spectrum": [],
            "spl": null,
            "spl_weighting": "Z",
            "spl_integration": "fast"
        }"#;
        let frame: WireFrame = serde_json::from_str(json).expect("deserialize");
        assert!(frame.drive.is_none());
        assert!(frame.delay_locked.is_none());
        let inp = FaultInput::from_wire_frame(&frame);
        assert!(inp.frame.is_none());
        let mut st = FaultState::default();
        assert_eq!(st.update(&inp, 0.0), None);
    }
}
