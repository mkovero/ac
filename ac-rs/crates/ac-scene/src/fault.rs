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
//! question: all but a handful of columns below the display's own mask
//! threshold, which is two legs carrying unrelated sources rather than a
//! misaligned one. A bad lock does not trip it — stage 2 reads 0.93 either
//! way. "All but a handful" and not "every": unrelated legs still put 22 of
//! 504 columns over the mask at low frequency, and demanding every one made
//! the state unreachable. See [`COHERENCE_ALIVE_FRACTION`].
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
//! The same count carries the escalation, not just the gate: `NO LOCK` is the
//! later of [`PERSISTENT_REFUSAL_S`] and [`PERSISTENT_REFUSAL_ATTEMPTS`],
//! because seconds are paced by a daemon constant this crate cannot see
//! (#247). What that threshold is coupled to — the estimator's admission
//! constant, in another crate — is at [`PERSISTENT_REFUSAL_S`].
//!
//! **Both halves fire now.** #226 gives the lock a producer: a manual re-lock
//! key and the drive off→on edge both flush a pair's held delay, so
//! `delay_locked` can return to false after having been true, and
//! [`Fault::LostLock`] and the `settled` half of this gate — dormant while
//! the daemon only ever estimated a pair's delay once and cached it — are
//! reachable today, not merely written ahead of their producer. #238's
//! contribution stands unchanged: the half that needed no producer, a pair
//! that never locks.
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
///
/// # It is the *later* of two thresholds (#247)
///
/// Seconds alone is the wrong unit. What decides whether the estimator has had
/// a fair chance is **attempts**, and attempts are paced by `RELOCK_RETRY`
/// (1 s, `handlers/transfer.rs`) — a daemon-side constant this crate cannot
/// see. At 1 Hz the two coincide, which is the only reason a seconds threshold
/// has been correct so far. Re-pace the retry, or pace it by ring refill
/// instead of a timer, and the advice moves relative to the estimator's
/// progress with no edit here and no test failure.
///
/// So escalation now requires both this and
/// [`PERSISTENT_REFUSAL_ATTEMPTS`] — whichever lands later wins. Each covers
/// the case the other cannot: fast retries must not let ten attempts fly past
/// in a second and advise the operator instantly, and slow retries must not
/// let ten seconds elapse on two attempts.
///
/// # What else moves this number
///
/// The estimator's admission constant, [`ac_core::visualize::transfer::NOISE_FLOOR_PROMINENCE`],
/// decides how many attempts a **correct** measurement needs before it locks.
/// Rig session 3, first attempt clearing each candidate admission threshold at
/// the two distant positions, attempts ~1 s apart:
///
/// | admission | worst first qualifying attempt | advice fires first? |
/// |---|---|---|
/// | **12 (shipped)** | **3** | no |
/// | 14 | 5 | no |
/// | 16 | 16–18 | **yes** |
/// | 18 | 20–23 | yes |
///
/// At admission 16 a correct 3 m measurement is told to move the microphone
/// and then locks correctly eight seconds later — worse than the blank window
/// #228 exists to end, because a confident wrong instruction sends the
/// operator to re-rig a working setup. The two constants live in different
/// crates and are set by different reasoning, and one decides whether the
/// other is correct. `the_admission_constant_leaves_room_before_the_advice_fires`
/// fails when either side moves.
pub const PERSISTENT_REFUSAL_S: f64 = 10.0;

/// Refused attempts, counting the one that started the refusal, after which
/// the refusal is persistent rather than transient (#247).
///
/// This is the unit the message means: "ten refusals, and it is still
/// refusing". It is **not** [`PERSISTENT_REFUSAL_S`] converted — a conversion
/// would bake in `RELOCK_RETRY` = 1 s, which is the coincidence this constant
/// exists to stop relying on.
///
/// Derived from the estimator instead. At the shipped admission constant of 12
/// the worst first qualifying attempt measured on the rig is **3** (table
/// above), so ten leaves a factor of 3.3 between "a correct distant position
/// is still working on it" and "tell the operator to check the rig". Below
/// about 5 the margin is thin enough that a slightly worse position than any
/// yet measured would be advised while it was about to succeed; far above 10
/// the operator stares at an unexplained blank window, which is #228's
/// original defect.
pub const PERSISTENT_REFUSAL_ATTEMPTS: u32 = 10;

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
    /// Both legs live, and almost no column clears the display's coherence
    /// mask — the legs are carrying unrelated sources. See
    /// [`COHERENCE_ALIVE_FRACTION`] for how much "almost" is.
    CheckRouting,
    /// The estimator is refusing to lock **after having held a lock**, and
    /// has been for less than [`PERSISTENT_REFUSAL_S`]. Retries are still
    /// plausibly going to succeed, so this says what is happening and no
    /// more.
    ///
    /// Requires a lock to have existed. A pair that has never locked gets
    /// [`Fault::NoLockYet`] instead — see its docs.
    ///
    /// Reachable since #226: a manual re-lock or the drive's off→on edge
    /// flushes a held delay lock, so `delay_locked` can return to false
    /// after having been true.
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
    /// How many delay estimates the estimator has completed on this pair,
    /// accepted or refused (`delay_attempts`).
    ///
    /// Non-zero is the refusing pair's equivalent of [`Self::settled`]: it is
    /// what makes `delay_locked: false` mean "asked and refused" rather than
    /// "not asked yet" — see [`Self::estimator_attempted`]. Zero on a daemon
    /// predating #238, which leaves such a daemon's refusals as unreachable as
    /// they were — absence is not evidence that the estimator ran.
    ///
    /// The count itself, not merely its zero-ness, because escalation is
    /// measured in attempts (#247): seconds are paced by a daemon constant
    /// this crate cannot see. See [`PERSISTENT_REFUSAL_ATTEMPTS`].
    ///
    /// Monotone for the session (`ZMQ.md`), and #226's re-locking must not
    /// change that. A count that reset would take a locked-then-refusing pair
    /// back to "warming up", and warmup paints nothing — and it would also
    /// restart the refusal's attempt count, deferring the advice forever.
    pub delay_attempts: u32,
}

impl FaultFrame {
    /// Whether the estimator has completed at least one estimate on this pair.
    ///
    /// Derived rather than stored, so it cannot disagree with
    /// [`Self::delay_attempts`].
    pub fn estimator_attempted(&self) -> bool {
        self.delay_attempts > 0
    }

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
            // The display's own selection, so "settled" and "there are
            // columns on screen" cannot disagree — see
            // [`WireFrame::displayed_mtw`].
            settled: frame.displayed_mtw().is_some(),
            delay_attempts: frame.delay_attempts,
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
                .displayed_mtw()
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

/// Fraction of columns that may clear [`COHERENCE_THRESHOLD`] while the
/// display is still, in substance, drawing nothing.
///
/// The original test demanded that **not one** of the columns clear the mask,
/// and that turns out to be unreachable: on the genuinely unrelated legs of
/// rig session 2, 22 of 504 columns cleared it (max 0.844, at 37–71 Hz), so
/// `CHECK ROUTING` never fired on the exact condition it was written for. Low
/// frequencies are where a room, a mains hum, or a shared noise source will
/// correlate two otherwise unrelated legs, and one narrow band of accidental
/// agreement is not a measurement.
///
/// 10% sits between the two measured cases with room on both sides: unrelated
/// legs put 4.4% over the line, and a healthy acoustic measurement clears it
/// nearly everywhere (0.715–0.755 on stage 0, 0.92+ below).
///
/// # What this costs
///
/// The columns are `mtw::ladder::P_REF` = 48 per octave, so 10% of a 504-
/// column frame is **about one octave**. A measurement that is genuinely
/// coherent over less than an octave and incoherent everywhere else — a
/// narrow bandpass DUT, a driver measured well outside its passband — reads
/// as `CHECK ROUTING`. That is a real false positive and it is the price of
/// the state firing at all; the strict rule had the opposite failure and was
/// worse, because it was silent.
///
/// It is also the shape a fraction cannot distinguish: 50 coherent columns
/// are 50 coherent columns whether they are one contiguous passband or
/// scattered accidents. If the rig produces the narrow-passband false
/// positive, contiguity is the discriminator to reach for, not a smaller
/// fraction.
const COHERENCE_ALIVE_FRACTION: f64 = 0.10;

/// Almost no column clears the display's coherence mask.
///
/// Reuses [`COHERENCE_THRESHOLD`] rather than introducing a second, tunable
/// number: the condition being named is "the display can draw nothing at
/// all", so the threshold that decides what is drawable is the one that
/// belongs here. It is also not loopback-derived — a healthy acoustic
/// measurement sits at 0.715-0.755 on stage 0 and 0.92+ below, an order of
/// magnitude clear.
///
/// "Almost", not "none": see [`COHERENCE_ALIVE_FRACTION`] for why the strict
/// reading made this structurally unreachable.
fn coherence_dead(coherence: &[f64]) -> bool {
    if coherence.is_empty() {
        return false;
    }
    let alive = coherence
        .iter()
        .filter(|c| **c >= COHERENCE_THRESHOLD)
        .count();
    (alive as f64) < COHERENCE_ALIVE_FRACTION * coherence.len() as f64
}

/// The origin of one unbroken run of refusals, in both units escalation is
/// measured in (#247).
///
/// One value rather than two parallel `Option`s. Both anchors are set by the
/// same frame and cleared by the same lock, and there is no state in which one
/// is meaningful without the other — a clock anchor without its attempt anchor
/// would escalate on seconds alone, which is the coupling
/// [`PERSISTENT_REFUSAL_ATTEMPTS`] exists to break. Making them one field
/// means the skew cannot be written, rather than merely not being written
/// today.
#[derive(Debug, Clone, Copy, PartialEq)]
struct RefusalRun {
    /// Scene time of the frame that began the run.
    since_s: f64,
    /// `delay_attempts` as it stood on that frame — the attempt that refused
    /// is itself counted, so the run has made `delay_attempts - this + 1`
    /// attempts.
    ///
    /// Zero means the producer does not report attempts (a daemon predating
    /// #238); see [`FaultState::update`] for why that falls back to the clock
    /// rather than never escalating.
    since_attempts: u32,
}

impl RefusalRun {
    /// Whether the estimator has had [`PERSISTENT_REFUSAL_ATTEMPTS`] chances
    /// on this run.
    ///
    /// A producer reporting no attempts (pre-#238, anchor 0) leaves this
    /// unobservable, and an unobservable condition must not be read as unmet:
    /// that would make `NO LOCK` unreachable for exactly the daemons whose
    /// refusals #238 was written to surface. Absent evidence defers to the
    /// clock.
    fn attempts_elapsed(&self, delay_attempts: u32) -> bool {
        self.since_attempts == 0
            || delay_attempts.saturating_sub(self.since_attempts) + 1 >= PERSISTENT_REFUSAL_ATTEMPTS
    }
}

/// The time-dependent part of the indicator, carried across frames the same
/// way [`crate::transfer::MeterState`] carries the meter hold and clip latch.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FaultState {
    /// The current unbroken run of settled refusals. Cleared by a lock, and
    /// by falling back out of settled.
    refusing: Option<RefusalRun>,
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
        // Both gates fire today: #226 gave the lock a producer, so a
        // settled pair can lose its lock and report a refusal too — see
        // the module docs.
        let asked = frame.settled || frame.estimator_attempted();
        let refusing = asked && frame.delay_locked == Some(false);
        if refusing {
            self.refusing.get_or_insert(RefusalRun {
                since_s: now_s,
                since_attempts: frame.delay_attempts,
            });
        } else {
            self.refusing = None;
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

        if let Some(run) = self.refusing {
            // Escalation takes the *later* of the two thresholds (#247).
            // Seconds bound how long the operator stares at a transient row;
            // attempts bound how many chances the estimator has actually had,
            // which is what the advice claims to know. They coincide only
            // while `RELOCK_RETRY` is 1 s — a daemon constant this crate
            // cannot see, so it must not be the thing being relied on.
            // See [`RefusalRun::attempts_elapsed`] for the pre-#238 producer.
            if now_s - run.since_s >= PERSISTENT_REFUSAL_S
                && run.attempts_elapsed(frame.delay_attempts)
            {
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
#[path = "fault_tests.rs"]
mod tests;
