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
//! | both legs live, estimator refusing | [`Fault::LostLock`] / [`Fault::NoLock`] | delay fault |
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
//! since the threshold is the estimator's to own." Warmup and refusal are
//! separated by [`FaultInput::settled`] — an observed property of the frame —
//! rather than by reading prominence null-versus-present. Silently gating on
//! a value the estimator owns is how the two ends drift apart.
//!
//! # Warmup must not cry wolf
//!
//! `delay_locked: false` is also what a pair publishes while warming up. A
//! fault indicator that fires on every healthy startup gets ignored, which
//! defeats the point of having one, so every lock-derived state waits for the
//! ladder to settle. Settling is *observed* — the frame's `mtw` columns are
//! absent until every rung holds a full N blocks — not timed from session
//! start, so it cannot drift from what the display is actually showing.

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
/// Recorded because [`PERSISTENT_REFUSAL_S`] is reasoned from it. It is not
/// used as a timer — see [`FaultInput::settled`].
pub const LADDER_SETTLE_S: f64 = 2.560;

/// A refusal still standing this many seconds **after the ladder settles** is
/// persistent rather than transient.
///
/// #227 retries at 1 Hz, so this is roughly ten retries past the first point
/// at which a lock was even possible, and anything past a handful of retries
/// is genuinely persistent. The clock starts at settle, not at session start:
/// a timer started at t=0 would fire on healthy sessions, since warmup is
/// indistinguishable from refusal on the wire.
///
/// The number is arguable — argue it against the rig. What is not open is
/// leaving it unset, which decides it silently and re-litigates it later.
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
    /// The estimator is refusing to lock, and has been for less than
    /// [`PERSISTENT_REFUSAL_S`]. Retries are still plausibly going to
    /// succeed, so this says what is happening and no more.
    LostLock,
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
            Fault::NoLock => "NO LOCK",
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
    /// a full N blocks. Before this, `delay_locked: false` is warmup and
    /// nothing lock-derived may paint.
    pub settled: bool,
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
        })
    }
}

/// Everything one frame contributes to the indicator.
pub struct FaultInput<'a> {
    /// `None` disables the indicator entirely.
    pub frame: Option<FaultFrame>,
    pub meas_peak_dbfs: Option<f64>,
    pub ref_peak_dbfs: Option<f64>,
    /// The display's own columns. Empty before the ladder settles.
    pub coherence: &'a [f64],
}

impl<'a> FaultInput<'a> {
    /// Read one live frame. The coherence columns come from `mtw` — the
    /// display's source — and their presence is what marks the ladder
    /// settled, so both fall out of one lookup.
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
        let refusing = frame.settled && frame.delay_locked == Some(false);
        if refusing {
            self.refusing_since_s.get_or_insert(now_s);
        } else {
            self.refusing_since_s = None;
        }
        if frame.delay_locked == Some(true) && self.prev_locked == Some(false) {
            self.acquired_at_s = Some(now_s);
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
            return Some(if now_s - since >= PERSISTENT_REFUSAL_S {
                Fault::NoLock
            } else {
                Fault::LostLock
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
            }),
            ..healthy(&coh)
        };
        assert_eq!(st.update(&inp, 0.0), Some(Fault::LostLock));
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
        assert_eq!(st.update(&refusing, 0.0), Some(Fault::LostLock));
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

    /// The whole reason lock states wait for the ladder: `delay_locked:
    /// false` during warmup is indistinguishable from a refusal, and an
    /// indicator that fires on every healthy startup gets ignored.
    #[test]
    fn warmup_does_not_paint_a_lock_fault() {
        let mut st = FaultState::default();
        let warming = FaultInput {
            frame: Some(FaultFrame {
                delay_locked: Some(false),
                settled: false,
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

    /// And the refusal clock starts at settle, not at session start: a timer
    /// from t=0 would fire NO LOCK the instant a slow-settling session
    /// produced its first columns.
    #[test]
    fn the_refusal_clock_starts_at_settle_not_at_session_start() {
        let coh = [0.755, 0.92];
        let mut st = FaultState::default();
        let warming = FaultInput {
            frame: Some(FaultFrame {
                delay_locked: Some(false),
                settled: false,
                ..driving()
            }),
            coherence: &[],
            ..healthy(&[])
        };
        for t in 0..30 {
            assert_eq!(st.update(&warming, t as f64), None);
        }
        // The first settled frame, at t=30, is where the clock starts.
        let refusing = FaultInput {
            frame: Some(FaultFrame {
                delay_locked: Some(false),
                ..driving()
            }),
            ..healthy(&coh)
        };
        assert_eq!(st.update(&refusing, 30.0), Some(Fault::LostLock));
        assert_eq!(st.update(&refusing, 39.9), Some(Fault::LostLock));
        assert_eq!(st.update(&refusing, 40.0), Some(Fault::NoLock));
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
        assert_eq!(st.update(&refusing, 0.0), Some(Fault::LostLock));
        assert_eq!(
            st.update(&refusing, PERSISTENT_REFUSAL_S),
            Some(Fault::NoLock)
        );
        // The transient one deliberately carries no instruction; the
        // persistent one carries the one the operator needs.
        assert_eq!(Fault::LostLock.detail(), None);
        assert!(Fault::NoLock.detail().is_some());
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
        assert_eq!(st.update(&refusing, 0.0), Some(Fault::LostLock));
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
        assert_eq!(st.update(&refusing, 0.0), Some(Fault::LostLock));
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
        assert_eq!(st.update(&inp, 0.0), Some(Fault::LostLock));
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
        assert_eq!(st.update(&inp, 0.0), Some(Fault::LostLock));
        assert_eq!(
            st.update(&inp, PERSISTENT_REFUSAL_S),
            Some(Fault::NoLock),
            "a refusal standing 10 s past settle is persistent"
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
