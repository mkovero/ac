//! The transfer view's fault indicator (#228) — a pure function of the
//! frame plus a little carried time state.
//!
//! # What this is for
//!
//! Distinct failures otherwise present identically: as "the top end looks
//! wrong". A dead reference leg cost a whole rig session with nothing on
//! screen saying so. Each state here names one cause with one action —
//! "something is wrong" would leave the operator guessing between them.
//!
//! | condition | state | cause |
//! |---|---|---|
//! | not driving, legs quiet | *(nothing)* | idle, expected |
//! | driving, reference leg at floor | [`Fault::NoReference`] | #225, misrouted or unpatched |
//! | driving, measurement leg at floor | [`Fault::NoSignal`] | mic unplugged, DUT off, wrong input |
//! | both legs live, coherence low everywhere | [`Fault::CheckRouting`] | legs carry different sources |
//!
//! # Drive state gates the level rows and nothing else
//!
//! Only the two level rows read the drive: a leg at the floor is a fault
//! only when something should have been reaching it, which is why the first
//! row exists and why a session that never drives reports neither of them.
//! Two legs above the floor are carrying signal whoever put it there, so
//! [`Fault::CheckRouting`] fires on a passive external-DUT session too.
//!
//! # There is no delay fault (#669)
//!
//! The daemon finds the delay as the peak of the unaligned live impulse
//! response — Smaart's Delay Finder rule — and the operator owns it from
//! there. Two live legs always have a highest IR peak, so there is no
//! refusal to report: the only pair left unaligned after warmup is one with
//! a digitally silent leg, which the level rows already name. The rows that
//! reported a cross-correlation estimator refusing (`NO LOCK`, `LOST LOCK`)
//! went with the estimator.
//!
//! A wrong delay still shows, where it always did: in coherence. Two
//! unrelated legs now get a ladder (their IR has a peak, so they get a
//! delay), so [`Fault::CheckRouting`] fires on exactly the session that used
//! to reach the operator only as `NO LOCK`.
//!
//! # `CHECK ROUTING` reads the ladder, never the Welch array
//!
//! The ladder's columns are what the display draws. The frame's Welch
//! `coherence` is a different measurement — a single path with a different
//! bin count and a different bias floor — and [`coherence_dead`]'s threshold
//! was measured against the ladder's columns; applied to the other array it
//! is a different test wearing the same name. Before the ladder settles
//! there are no columns, and the row stays dark.

use crate::transfer::COHERENCE_THRESHOLD;
use ac_core::wire::TransferFrame;

use crate::transfer::displayed_mtw;

/// "At the floor", in dBFS. Absolute and generous: far below any usable
/// measurement, so it will not fire on a quiet but valid session.
///
/// Deliberately **not** relative to the other leg. Levels legitimately
/// differ — by 15 dB on the rig that found this, a mic at −30 dBFS peak
/// against a reference at −14.5 dBFS — so a relative test would misfire on a
/// perfectly good session.
pub const SIGNAL_FLOOR_DBFS: f64 = ac_core::visualize::protection::REFERENCE_FLOOR_DBFS;

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
}

impl Fault {
    /// The banner text. `ac-view` draws this verbatim and must never
    /// reformat it.
    pub fn label(&self) -> &'static str {
        match self {
            Fault::NoReference => "NO REFERENCE",
            Fault::NoSignal => "NO SIGNAL",
            Fault::CheckRouting => "CHECK ROUTING",
        }
    }

    /// The action, where the label alone does not imply one. `None` where
    /// it does, or where there is nothing for the operator to do yet.
    ///
    /// **A detail may name what to check; it may not assert a cause.** The
    /// level rows can afford to be specific — the frame says which leg is at
    /// the floor, and the table's causes for that leg are established.
    pub fn detail(&self) -> Option<&'static str> {
        match self {
            Fault::NoReference => Some("reference leg silent — check the output patch"),
            Fault::NoSignal => {
                Some("measurement leg silent — check the mic, the DUT, and the input")
            }
            Fault::CheckRouting => Some("the two legs carry unrelated sources"),
        }
    }

    /// Every row is a fault since #256 dropped `DELAY FOUND`, the one
    /// confirmation; [`Severity::Confirmation`] stays for the renderer's
    /// contract.
    pub fn severity(&self) -> Severity {
        Severity::Fault
    }
}

/// The [`ac_core::wire::WireDrive`] fields this module actually uses.
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
    /// Whether the pair holds a delay (`delay_locked`). Carried for the
    /// frame's completeness; no row reads it since #256 dropped the
    /// `DELAY FOUND` confirmation (the operator reads the delay readout).
    pub delay_locked: Option<bool>,
}

impl FaultFrame {
    /// `None` when the daemon does not report its drive state — see
    /// [`TransferFrame::drive`].
    pub fn from_wire_frame(frame: &TransferFrame) -> Option<FaultFrame> {
        let drive = frame.drive.as_ref()?;
        Some(FaultFrame {
            drive: DriveState {
                on: drive.on,
                drivable: drive.drivable,
            },
            delay_locked: frame.delay_locked,
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
    /// Empty before it settles.
    ///
    /// Do not fall back to [`TransferFrame::coherence`] to fill it. See the
    /// module's `CHECK ROUTING` section: the Welch array is a different
    /// measurement with a different bin count and bias floor, and
    /// [`coherence_dead`]'s threshold was measured against the ladder's 504
    /// columns. Substituting the other array keeps the name and changes the
    /// test. `no_welch_fallback_fills_the_coherence_columns` pins this.
    pub coherence: &'a [f64],
}

impl<'a> FaultInput<'a> {
    /// Read one live frame. The coherence columns come from `mtw` — the
    /// display's source.
    ///
    /// `mtw` absent means no columns, full stop. The frame's own `coherence`
    /// is deliberately not consulted here — see [`Self::coherence`].
    pub fn from_wire_frame(frame: &'a TransferFrame) -> FaultInput<'a> {
        FaultInput {
            frame: FaultFrame::from_wire_frame(frame),
            meas_peak_dbfs: frame.meas_peak_dbfs,
            ref_peak_dbfs: frame.ref_peak_dbfs,
            coherence: displayed_mtw(frame)
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

/// The two level rows, for a frame with at least one leg at the floor.
///
/// A quiet leg is only a fault when something should be reaching it. Not
/// driving: idle and expected — and for a session that is not drivable at
/// all, daemon silence says nothing about the inputs, so there is nothing to
/// report either way.
///
/// **This is the only place the drive is read.** Two legs both above the
/// floor are carrying signal whoever put it there, so an external-DUT session
/// still gets [`Fault::CheckRouting`].
fn level_row(frame: FaultFrame, ref_dead: bool) -> Option<Fault> {
    if !frame.drive.on {
        return None;
    }
    // Both dead reports the reference. It is the daemon's own leg and its
    // failure explains the other; naming the measurement leg first would
    // send the operator to the mic for a patching fault.
    Some(if ref_dead {
        Fault::NoReference
    } else {
        Fault::NoSignal
    })
}

/// The time-dependent part of the indicator, carried across frames the same
/// way [`crate::transfer::MeterState`] carries the meter hold and clip latch.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FaultState {}

impl FaultState {
    /// Fold one frame in at scene time `now_s` and read the indicator out.
    /// `None` is "show nothing", which is the correct display for an idle
    /// session and for a warming-up one.
    ///
    /// Monotone in `now_s`; callers pass the scene clock, not wall time.
    ///
    /// # Order of the rows
    ///
    /// Level first, then coherence — the order the causes chain in. A dead
    /// reference leg makes everything downstream of it look wrong, so
    /// reporting the coherence it destroys instead would name the symptom
    /// while the cause sits one row up.
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
        if ref_dead || meas_dead {
            return level_row(frame, ref_dead);
        }
        // Both legs live from here.
        let _ = now_s;
        coherence_dead(input.coherence).then_some(Fault::CheckRouting)
    }

    fn reset(&mut self) {
        *self = FaultState::default();
    }
}

#[cfg(test)]
#[path = "fault_tests.rs"]
mod tests;
