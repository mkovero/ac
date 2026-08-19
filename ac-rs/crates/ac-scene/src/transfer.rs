//! Transfer-view display truth: de-rotated phase, the coherence mask,
//! the delay readout, and the input-level meter model (M4a, #180).
//!
//! # The de-rotation mapping (corrected §6, R1 ratified)
//!
//! Sign convention, stated once: a physical delay τ > 0 (later arrival)
//! produces measured phase φ(f) = −360·f·τ (degrees, f in Hz, τ in
//! seconds). De-rotation therefore ADDS:
//!
//! ```text
//! φ'(f) = wrap±180( φ(f) + 360·f·τ_derot )
//! ```
//!
//! **The wire does not carry raw phase.** `ac-core`'s
//! `visualize::transfer::h1_estimate_core` multiplies `Gxy` by
//! `exp(+j·2π·f·delay_samples/sr)` before forming H1, and takes
//! `phase_deg = h1.arg()` after that; the streaming worker estimates
//! `delay_samples` once and freezes it (D4). So [`crate::wire::WireFrame`]'s
//! `phase_deg` is
//!
//! ```text
//! φ_wire(f) = φ_raw(f) + 360·f·τ_sess
//! ```
//!
//! and the τ_derot each display mode must supply is measured *from
//! there*, not from raw phase:
//!
//! | [`DerotMode`] | τ_derot | shows |
//! |---|---|---|
//! | `Session`  | `0`               | φ_wire as-is — already session-compensated |
//! | `Raw`      | `−τ_sess`         | φ_raw |
//! | `Snapshot` | `τ_snap − τ_sess` | φ_raw + 360·f·τ_snap |
//!
//! The overlay workflow (tops snapshot vs live subs) is why `Snapshot`
//! exists: the snapshot trace is drawn as-is — it is already compensated
//! by its own τ_snap, so its τ_derot is 0 — and the live trace takes
//! τ_snap − τ_sess. Both then sit on φ_raw + 360·f·τ_snap, a common
//! reference, so a DSP delay change tilts one against the other instead
//! of moving both.
//!
//! D4 survives unchanged: the session estimate stays frozen, so operator
//! DSP-delay changes appear as phase tilt rather than being silently
//! tracked out.
//!
//! Reading `phase_deg` as raw phase and de-rotating by `+τ_sess` — the
//! literal pre-correction §6 — double-compensates, producing a tilt of
//! the wrong sign at exactly the magnitude the operator is trying to
//! null. Fixtures F1′/F1″/F2′ (`tests/it_transfer.rs`) are built from
//! daemon-shaped frames specifically to catch that.

use ac_core::visualize::pair_derivation::PairDerivation;
use ac_core::visualize::smoothing::{smooth_db, smooth_unwrapped_phase_deg};

use crate::fault::{Fault, FaultFrame, FaultInput, FaultState};
use crate::scene::{Provenance, Source, Trace};
use crate::ticks::{db_to_y, freq_to_x, phase_to_y};
use crate::wire::{MtwStage, WireDistanceCal, WireFrame};

/// Columns below this coherence are masked out of both panes (D5 —
/// fixed threshold, no tuning UI).
pub const COHERENCE_THRESHOLD: f64 = 0.5;

/// Speed of sound for the delay readout's metres conversion when the frame
/// carries none — 343 m/s, the conventional 20 °C figure.
///
/// D2 originally fixed this "exactly 343 m/s, not a temperature-dependent
/// estimate". #243 reverses that half of the decision: the rooms this
/// instrument measures in run 24–26 °C, where c ≈ 346 m/s, and over 1 m the
/// disagreement is larger than one sample at 96 kHz — so the constant was
/// wrong by more than the estimator could resolve, which is the bar for
/// whether a physical constant may be baked in at all. The speed is now a
/// parameter, derived daemon-side from a configured room temperature and
/// carried on [`WireFrame::speed_of_sound_m_s`].
///
/// This value survives as the fallback for a frame that names no speed —
/// an older daemon, or a room whose temperature nobody recorded — where it
/// reproduces the previous behaviour exactly rather than inventing one.
pub const SPEED_OF_SOUND_DEFAULT_M_S: f64 =
    ac_core::shared::conversions::SPEED_OF_SOUND_DEFAULT_M_S;

/// Meter floor: −60 dBFS maps to a zero-height bar (§6).
pub const METER_FLOOR_DBFS: f64 = -60.0;

/// At or above this level the clip latch sets (§6).
pub const CLIP_DBFS: f64 = -0.1;

/// How long a set clip latch stays visible, in scene seconds (§6: "at
/// least 3 s").
pub const CLIP_LATCH_HOLD_S: f64 = 3.0;

/// Peak-hold tick decay, in scene seconds (§6: "~1.5 s").
pub const PEAK_HOLD_S: f64 = 1.5;

/// Which delay the phase pane is de-rotated by (D3). The variants name
/// what the operator selects; [`DerotMode::tau_derot_ms`] converts that
/// choice into the τ_derot the maths needs, given a wire that is already
/// session-compensated.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum DerotMode {
    /// This session's delay. τ_derot = 0 — the wire already is this. The
    /// default: a session opens showing its own compensation, which is the
    /// only reference it has measured for itself.
    #[default]
    Session,
    /// Undo the session's compensation and show measured phase.
    Raw,
    /// An open snapshot's stored delay, so live and snapshot share a
    /// reference. Carries the snapshot's own τ, in ms.
    Snapshot { snapshot_delay_ms: f64 },
}

impl DerotMode {
    /// τ_derot in ms, to be applied on top of `session_delay_ms`
    /// (τ_sess) — the frame's own `delay_ms`.
    pub fn tau_derot_ms(&self, session_delay_ms: f64) -> f64 {
        match *self {
            DerotMode::Session => 0.0,
            DerotMode::Raw => -session_delay_ms,
            DerotMode::Snapshot { snapshot_delay_ms } => snapshot_delay_ms - session_delay_ms,
        }
    }
}

/// Fractional-octave smoothing of the displayed transfer curve (#229).
///
/// The designator set is closed on purpose — 1/1, 1/3, 1/6, 1/12, 1/24 and
/// off, the standard ladder, in the same vocabulary
/// `ProcessingChain.smoothing_bpo` and both report writers already use. A
/// free `u32` would let the view ask for 1/7 octave, which no operator means
/// and no report renders.
///
/// What this is **not**: a column-density control. Points-per-octave stays
/// fixed at 48 (`design-mtw-ladder.md`, decision 3) because coarser columns
/// tighten the delay tolerance eightfold. Smoothing runs after coherence is
/// formed and therefore cannot do that, however heavy it is set.
///
/// Coherence is never smoothed: the mask that gaps the trace is computed from
/// the raw `coherence` array either way, so a smoothed magnitude cannot make
/// an untrusted column look trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Smoothing {
    #[default]
    Off,
    Oct1,
    Oct3,
    Oct6,
    Oct12,
    Oct24,
}

impl Smoothing {
    /// Bands per octave, or `None` for off — the shape
    /// `ProcessingChain.smoothing_bpo` is declared in.
    pub fn bpo(self) -> Option<u32> {
        match self {
            Smoothing::Off => None,
            Smoothing::Oct1 => Some(1),
            Smoothing::Oct3 => Some(3),
            Smoothing::Oct6 => Some(6),
            Smoothing::Oct12 => Some(12),
            Smoothing::Oct24 => Some(24),
        }
    }

    /// The operator-facing string, or `None` when nothing is applied.
    ///
    /// `None` rather than `"off"`: the readout exists to say that the trace
    /// has been altered, and an unaltered trace is the resting state of the
    /// instrument. Wording matches the report writers' `"1/6 octave"` so a
    /// screenshot and a report name the same setting the same way.
    pub fn label(self) -> Option<&'static str> {
        match self {
            Smoothing::Off => None,
            Smoothing::Oct1 => Some("smoothing 1/1 octave"),
            Smoothing::Oct3 => Some("smoothing 1/3 octave"),
            Smoothing::Oct6 => Some("smoothing 1/6 octave"),
            Smoothing::Oct12 => Some("smoothing 1/12 octave"),
            Smoothing::Oct24 => Some("smoothing 1/24 octave"),
        }
    }

    /// Cycle order: off, then narrowest to widest, then back to off.
    ///
    /// Successive presses smooth *more*, so the key reads as one direction of
    /// travel rather than as a menu, and the state after a full cycle is the
    /// unaltered trace rather than the heaviest setting.
    pub fn next(self) -> Smoothing {
        match self {
            Smoothing::Off => Smoothing::Oct24,
            Smoothing::Oct24 => Smoothing::Oct12,
            Smoothing::Oct12 => Smoothing::Oct6,
            Smoothing::Oct6 => Smoothing::Oct3,
            Smoothing::Oct3 => Smoothing::Oct1,
            Smoothing::Oct1 => Smoothing::Off,
        }
    }
}

/// The operator's display-only choices, grouped because that is what they
/// have in common: neither changes the measurement, only how it is drawn.
///
/// They travel together into [`TransferScene::from_input`] so that adding the
/// next one is not another positional argument on a constructor that already
/// carries the ranges, the cross-frame state and the clock.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DisplayModes {
    /// Which delay the phase pane is de-rotated by (D3).
    pub derot: DerotMode,
    /// Fractional-octave smoothing of both panes (#229).
    pub smoothing: Smoothing,
}

impl DisplayModes {
    /// The defaults an opening session gets: session de-rotation, no
    /// smoothing.
    pub fn new(derot: DerotMode, smoothing: Smoothing) -> DisplayModes {
        DisplayModes { derot, smoothing }
    }
}

/// Wrap to **(−180, +180]** — the range of `Complex::arg`, which is what
/// produced `phase_deg` upstream (`h1.arg().to_degrees()`).
///
/// The interval is not a free choice: scene values must agree with wire
/// values at the boundary. Note the strict `>` — the idiomatic
/// `rem_euclid`-then-shift with `>=` yields the other half-open
/// interval, [−180, +180), which returns −180 exactly where this returns
/// +180 and leaves every interior column looking correct.
pub fn wrap_deg(deg: f64) -> f64 {
    let y = deg.rem_euclid(360.0);
    if y > 180.0 {
        y - 360.0
    } else {
        y
    }
}

/// φ'(f) = wrap±180( φ_wire(f) + 360·f·τ_derot ), τ_derot in ms.
pub fn derotate_deg(phase_wire_deg: f64, freq_hz: f64, tau_derot_ms: f64) -> f64 {
    wrap_deg(phase_wire_deg + 360.0 * freq_hz * tau_derot_ms / 1000.0)
}

/// The distance a locked delay corresponds to, in metres.
///
/// `constant_ms`, when given, is subtracted from `delay_ms` before the
/// conversion — the delay this pair's lock includes that never travelled
/// through air (converter/DUT latency, loudspeaker DSP, mic geometry). It
/// comes from a stored [`DistanceCalibration`] (#243); `None` performs the
/// pre-#243 conversion of the whole delay, unchanged.
///
/// Under the supported wiring both legs traverse the same converter, so
/// everything ahead of the converter's analogue output is common-mode and
/// cancels in the correlation, and what survives is only what the acoustic
/// branch genuinely adds — amplifier, speaker DSP, driver origin, air,
/// microphone. Rig measurement (#243) showed that residual is *not*
/// converter latency alone — it is dominated by loudspeaker and microphone
/// geometry — so it is a property of the pair's acoustic setup, not of the
/// interface, and is looked up accordingly. See `README.md`'s "Reference
/// wiring" for the topology this depends on, and what the readout means
/// without it.
pub fn distance_m(delay_ms: f64, speed_of_sound_m_s: f64, constant_ms: Option<f64>) -> f64 {
    (delay_ms - constant_ms.unwrap_or(0.0)) / 1000.0 * speed_of_sound_m_s
}

/// The pre-#243 "`{ms} ms`" / "`{ms} ms  ({m} m)`" readout, unconditioned on
/// a stored [`DistanceCalibration`] — kept for the two IR-panel marker call
/// sites (`ir.rs`, `sweep_ir.rs`) whose delay is not this issue's
/// per-`(meas_channel, ref_channel)` constant: the live `visualize/ir`
/// sidecar carries no distance-calibration wire field yet, and the
/// sweep-gate panel's `ArrivalDistance::Known` case (#283) already applies
/// its own, unrelated τ correction before this is called. [`format_delay_readout`]
/// is for [`crate::transfer::TransferScene`] alone; this is everything else
/// that prints a delay-as-distance string.
// `!(delay_ms >= 0.0)` rather than `delay_ms < 0.0` is the NaN-aware guard —
// see `format_delay_readout`'s own copy of this comment.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn format_delay_readout_plain(
    delay_ms: f64,
    delay_locked: Option<bool>,
    speed_of_sound_m_s: f64,
) -> String {
    if delay_locked != Some(true) || !(delay_ms >= 0.0) {
        return format!("{delay_ms:.2} ms");
    }
    let metres = distance_m(delay_ms, speed_of_sound_m_s, None);
    format!("{delay_ms:.2} ms  ({metres:.2} m)")
}

/// A pair's stored distance-calibration constant (#243), adapted from
/// [`crate::wire::WireDistanceCal`] for [`format_delay_readout`].
#[derive(Debug, Clone, PartialEq)]
pub struct DistanceCalibration {
    /// The flight-time offset to subtract from `delay_ms` before the ms → m
    /// conversion, in milliseconds.
    pub constant_ms: f64,
    /// The acoustic setup this constant was captured under — shown so a
    /// figure read later can be told from an uncalibrated one, and so a
    /// patched-in channel wearing someone else's calibration is visible.
    pub setup_id: String,
    /// RFC3339 timestamp of the capture. Only the date portion (up to a
    /// `T`, if present) is shown.
    pub captured_at: String,
}

impl DistanceCalibration {
    fn captured_date(&self) -> &str {
        self.captured_at
            .split('T')
            .next()
            .unwrap_or(&self.captured_at)
    }

    fn from_wire(w: &WireDistanceCal) -> DistanceCalibration {
        DistanceCalibration {
            constant_ms: w.constant_ms,
            setup_id: w.setup_id.clone(),
            captured_at: w.captured_at.clone(),
        }
    }
}

/// Everything [`format_delay_readout`] produces: the primary reading, and
/// two receding-weight lines that say whether it can be trusted (#243).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DelayReadout {
    /// `"4.08 ms"`, or `"4.08 ms    0.999 m"` when a stored calibration
    /// earns a metres figure.
    pub delay_readout: String,
    /// `"cal 2026-08-18 · mic1 · pair meas0↔ref3"` when calibrated, or
    /// `"not calibrated — pair meas0↔ref3"` when locked but not. `None`
    /// when the pair is not locked — nothing here has an opinion on a
    /// delay the estimator has not produced.
    pub delay_calibration: Option<String>,
    /// Set only when a calibrated reading exceeds `plausible_max_m`. Own
    /// line, not appended to the value, and never colour-only: a warning
    /// piped to a log or read on a colour-disabled terminal must still say
    /// what it says.
    pub delay_warning: Option<String>,
}

/// Builds [`DelayReadout`] — or milliseconds alone when the delay is not a
/// measured lock (#243).
///
/// `speed_of_sound_m_s` comes from [`WireFrame::speed_of_sound_m_s`],
/// falling back to [`SPEED_OF_SOUND_DEFAULT_M_S`]. `distance_cal` comes from
/// a stored [`DistanceCalibration`] for this `(meas_channel, ref_channel)`
/// pair, or `None` when the session named no `distance_setup_id` or none was
/// found. `plausible_max_m` is an operator-supplied sanity ceiling (#243's
/// plausibility check); `None` disables the check rather than guessing a
/// room size.
///
/// # Why the lock gates the metres and not the milliseconds
///
/// `delay_locked` follows [`WireFrame::delay_locked`]'s three-way meaning,
/// and only `Some(true)` earns metres. Before a lock the frame's `delay_ms`
/// is `0.0` — a placeholder, not a measurement — and the readout used to
/// paint `"0.00 ms  (0.00 m)"` from it: a distance claim about a room,
/// stated in the unit an operator acts on, derived from a delay the
/// estimator had not yet produced. That is on screen for the first seconds
/// of every session and for the whole of a refusing one.
///
/// Milliseconds stay because they are the frame's own field reported back
/// unchanged, and `0.00 ms` alongside a `NO LOCK` indicator reads as the
/// placeholder it is. Metres go because the conversion is an inference about
/// physical space, and an inference from a placeholder is worse than
/// silence. An unlocked pair is already named by [`crate::fault`]'s
/// `NO LOCK` / `LOST LOCK` states, so nothing new has to say why — and for
/// the same reason, no calibration line is shown either: the pair's
/// calibration status is moot until it has a delay to apply it to.
///
/// # Metres require a stored constant, not just a lock (#243)
///
/// A locked pair with no `distance_cal` gets milliseconds only, plus a
/// `"not calibrated"` line naming the pair. Converting the whole locked
/// delay — converter/DUT latency, loudspeaker DSP, air, mic, as if it were
/// flight time alone — is the defect #243 was filed on: it read 1.40 m at a
/// taped 1.000 m, 40% high. A missing calibration is not defaulted to zero
/// offset; it suppresses the metres figure entirely.
///
/// # The non-negative guard is dormant, deliberately
///
/// A negative delay would mean the microphone heard the source before the
/// reference did, which is the sign-reversed misroute. It cannot arrive from
/// today's estimator: `estimate_delay` searches `zero_idx..` and selects
/// `direct_idx ∈ zero_idx..=peak_idx`, so a lock is causal by construction
/// and that misroute surfaces as a refusal (`NO LOCK`) instead. The guard is
/// here so the conversion is total rather than because the branch is live —
/// the same standing [`crate::fault::Fault::LostLock`] has until #226 — and
/// it is written and tested now so that relaxing the causal rule does not
/// also have to invent the display state for what it lets through.
// `!(delay_ms >= 0.0)` rather than `delay_ms < 0.0` is the NaN-aware guard,
// as in `ticks::freq_axis` and `band_labels`: a NaN delay must take the
// no-metres branch too, and `<` would let it through into `format!`.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn format_delay_readout(
    delay_ms: f64,
    delay_locked: Option<bool>,
    speed_of_sound_m_s: f64,
    meas_channel: i64,
    ref_channel: i64,
    distance_cal: Option<&DistanceCalibration>,
    plausible_max_m: Option<f64>,
) -> DelayReadout {
    if delay_locked != Some(true) || !(delay_ms >= 0.0) {
        return DelayReadout {
            delay_readout: format!("{delay_ms:.2} ms"),
            delay_calibration: None,
            delay_warning: None,
        };
    }
    let pair_tag = format!("pair meas{meas_channel}↔ref{ref_channel}");
    match distance_cal {
        None => DelayReadout {
            delay_readout: format!("{delay_ms:.2} ms"),
            delay_calibration: Some(format!("not calibrated — {pair_tag}")),
            delay_warning: None,
        },
        Some(cal) => {
            let metres = distance_m(delay_ms, speed_of_sound_m_s, Some(cal.constant_ms));
            let delay_warning = plausible_max_m.filter(|&max| metres > max).map(|_| {
                "readout exceeds plausible bound for this pair — check wiring or re-calibrate"
                    .to_string()
            });
            DelayReadout {
                delay_readout: format!("{delay_ms:.2} ms    {metres:.3} m"),
                delay_calibration: Some(format!(
                    "cal {} · {} · {pair_tag}",
                    cal.captured_date(),
                    cal.setup_id
                )),
                delay_warning,
            }
        }
    }
}

/// One band's resolution-and-settling label (#224): where it sits on the
/// shared log-frequency axis, and the exact string to draw there.
///
/// Both fields are contract, as with [`crate::ticks::Tick`] — a renderer
/// positions and draws, and must never reformat the text or recompute the
/// position.
#[derive(Debug, Clone, PartialEq)]
pub struct BandLabel {
    /// Normalized `[0,1]` x — the geometric centre of the band's **visible**
    /// span, mapped by [`freq_to_x`], the same mapping the traces and the
    /// frequency ticks use.
    pub position: f64,
    /// `"0.98 Hz / 2.56 s"` — this band's bin width and settling time.
    pub text: String,
}

/// `"{Δf} Hz / {settling} s"`, both figures to [`band_figure`]'s precision.
///
/// The settling figure is the ladder's own `W + hop·(N−1)` (the wire's
/// `settling_s`), **not** the raw analysis window: at the bottom rung the
/// window is 1.02 s while the average does not fill for 2.56 s, so a
/// window-derived label would understate the wait by 2.5x — which is the
/// one number an operator acts on after an EQ change.
pub fn format_band_label(df_hz: f64, settling_s: f64) -> String {
    format!("{} Hz / {} s", band_figure(df_hz), band_figure(settling_s))
}

/// Two decimals below 10, one at or above it.
///
/// Fixed by the ratified label set rather than by a significant-figure
/// rule: `0.98`, `2.56`, `2.93`, `0.85`, `23.4`, `0.11` are the strings the
/// UX review drew, and a plain 2- or 3-significant-figure rule reproduces
/// neither the `23.4` nor the `0.98` end of that list. Both quantities are
/// context for reading the curve, not readings themselves, so the display
/// precision is capped here — the underlying `f64`s are untouched.
fn band_figure(v: f64) -> String {
    if v >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

/// The per-band labels for one ladder, over the caller's frequency axis.
///
/// A stage serves from its own validity edge up to the shallower stage's:
/// the ladder builds `f_top_i` as `f_valid_{i−1}`, so the band edges follow
/// from `f_valid` alone and no second wire field is needed. The deepest
/// stage runs down to the bottom of the axis — below its validity edge the
/// column grid is Δf-limited and thins out, but it is still that stage's
/// measurement at that stage's resolution — and the shallowest runs to the
/// top. Each label is placed at the **geometric** centre of what remains
/// after clamping to `[f_min, f_max]`, which is the midpoint of the span it
/// governs on a log axis.
///
/// Emits nothing rather than a placeholder for a stage the frame does not
/// describe: a `df` or `settling_s` the daemon did not send arrives as
/// `0.0`, and `"0.00 Hz / 0.00 s"` would be a claim about the measurement
/// that no field on the wire supports. A band clamped off the visible axis
/// is dropped for the same reason — it labels frequencies not on screen.
// Negated `>` comparisons are intentional NaN-aware guards, as in
// `ticks::freq_axis`: `!(f_min > 0.0)` is true for NaN as well as for zero
// and negative inputs, all of which must short-circuit.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn band_labels(stages: &[MtwStage], f_min: f64, f_max: f64) -> Vec<BandLabel> {
    if !(f_min > 0.0) || !(f_max > f_min) {
        return Vec::new();
    }
    // The band edges are read off neighbouring stages, so the order is load
    // bearing: shallowest first, `f_valid` strictly descending. A ladder that
    // arrived in any other order would make `hi <= lo` and the label would
    // vanish — a silent drop, which is the failure class this repo keeps
    // finding. Named here so a future producer change trips a debug build
    // instead of quietly emptying the row. Release builds keep the drop: a
    // missing label is survivable, a panicking display is not.
    debug_assert!(
        stages
            .windows(2)
            .all(|w| w[0].f_valid > w[1].f_valid || w[1].f_valid <= 0.0),
        "ladder stages must be shallowest-first with descending f_valid: {:?}",
        stages.iter().map(|s| s.f_valid).collect::<Vec<_>>()
    );
    let deepest = stages.len().saturating_sub(1);
    let mut out = Vec::new();
    for (i, stage) in stages.iter().enumerate() {
        if !(stage.df > 0.0) || !(stage.settling_s > 0.0) {
            continue;
        }
        let lo = if i == deepest { f_min } else { stage.f_valid };
        let hi = if i == 0 { f_max } else { stages[i - 1].f_valid };
        let (lo, hi) = (lo.max(f_min), hi.min(f_max));
        if !(lo > 0.0) || !(hi > lo) {
            continue;
        }
        out.push(BandLabel {
            position: freq_to_x((lo * hi).sqrt(), f_min, f_max),
            text: format_band_label(stage.df, stage.settling_s),
        });
    }
    out
}

/// One input-level meter's display state. Heights are normalized
/// `[0,1]`, ready for the affine viewport map and nothing else.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Meter {
    /// Bar height, `clamp((peak_dbfs + 60) / 60, 0, 1)`.
    pub height: f64,
    /// Peak-hold tick height — the decaying maximum of `height`.
    pub hold: f64,
    /// Set at or above −0.1 dBFS, held for at least 3 s of scene time.
    pub clip_latch: bool,
}

/// Normalized bar height for a raw capture peak.
///
/// The peak arrives **already in dBFS** — unlike `meas_spectrum`, which
/// is linear. This function must therefore never reach for
/// [`crate::dbfs::linear_to_dbfs`]: doing so would put a second `log10`
/// in the crate, which structural rule 1 exists to prevent. `None`
/// (wire `null`, or the field absent on an older daemon) is a zero bar
/// with no latch, indistinguishably.
pub fn meter_height(peak_dbfs: Option<f64>) -> f64 {
    match peak_dbfs {
        // A non-finite value from a non-conforming producer clamps to
        // the floor rather than propagating NaN into a bar height.
        Some(p) if p.is_finite() => ((p - METER_FLOOR_DBFS) / -METER_FLOOR_DBFS).clamp(0.0, 1.0),
        _ => 0.0,
    }
}

/// Per-channel meter state carried across frames — the hold tick and the
/// clip latch are the only time-dependent quantities in the scene, and
/// they live here rather than in the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MeterState {
    hold: f64,
    hold_set_at_s: f64,
    latched_at_s: Option<f64>,
}

impl MeterState {
    /// Fold one frame's peak in at scene time `now_s` and read the
    /// meter out. Monotone in `now_s`; callers pass the scene clock, not
    /// wall time.
    pub fn update(&mut self, peak_dbfs: Option<f64>, now_s: f64) -> Meter {
        let height = meter_height(peak_dbfs);

        if height >= self.hold || now_s - self.hold_set_at_s >= PEAK_HOLD_S {
            self.hold = height;
            self.hold_set_at_s = now_s;
        }

        let clipping = peak_dbfs.is_some_and(|p| p.is_finite() && p >= CLIP_DBFS);
        if clipping {
            self.latched_at_s = Some(now_s);
        }
        let clip_latch = self
            .latched_at_s
            .is_some_and(|t| now_s - t < CLIP_LATCH_HOLD_S);

        Meter {
            height,
            hold: self.hold,
            clip_latch,
        }
    }
}

/// Everything the transfer view draws, with no numeric work left for the
/// renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct TransferScene {
    /// `|H|` in dB, normalized against the caller's dB range.
    pub magnitude: Trace,
    /// De-rotated phase, normalized against a fixed ±180° pane.
    pub phase: Trace,
    /// Shared log-frequency axis for both panes (#194).
    pub freq_axis: crate::ticks::Axis,
    /// dB gridlines for the magnitude pane, over the caller's dB range.
    pub mag_axis: crate::ticks::Axis,
    /// Degrees gridlines for the phase pane — `{+180, +90, 0, −90}`, with
    /// no −180 line (matches the trace's `(−180, +180]` wrap boundary).
    pub phase_axis: crate::ticks::Axis,
    /// `"2.50 ms"`, or `"2.50 ms    0.860 m"` when a stored calibration
    /// earns a metres figure (#243).
    pub delay_readout: String,
    /// `"cal 2026-08-18 · mic1 · pair meas0↔ref3"` or `"not calibrated —
    /// pair meas0↔ref3"` — which stored constant (if any) the metres figure
    /// came from, so a figure read a year later can be told from an
    /// uncalibrated one (#243). `None` while unlocked; see
    /// [`format_delay_readout`].
    pub delay_calibration: Option<String>,
    /// Set only when a calibrated metres figure exceeds the session's
    /// plausibility ceiling (#243) — the case this issue was filed on: a
    /// readout larger than the true distance, positive and locked, which no
    /// other guard here catches.
    pub delay_warning: Option<String>,
    /// `"smoothing 1/6 octave"`, or `None` when the trace is unaltered
    /// (#229).
    ///
    /// Present whenever smoothing is on, and not optional chrome: a smoothed
    /// trace is smoother than the measurement, and a screenshot that does not
    /// say so is a claim about resolution the instrument did not make.
    ///
    /// Read together with [`Self::band_labels`], which state the *measurement's*
    /// resolution: those say what the analyser resolved, this says what is on
    /// screen, and this one is authoritative for the drawn trace.
    pub smoothing_readout: Option<&'static str>,
    /// Per-band resolution and settling labels for the top of the magnitude
    /// pane (#224). Empty when the frame carries no ladder description —
    /// resolution and settling vary 24x across one screen, and a screen that
    /// cannot say by how much says nothing at all rather than guessing.
    ///
    /// These describe the measurement, not the drawn curve: with smoothing on,
    /// [`Self::smoothing_readout`] is what the trace's resolution actually is.
    /// The renderer places the two adjacently so they are read as one
    /// statement.
    pub band_labels: Vec<BandLabel>,
    pub meas_meter: Meter,
    pub ref_meter: Meter,
    /// The fault indicator (#228), or `None` for "show nothing" — which is
    /// the correct display for an idle session, a warming-up one, and a
    /// healthy one alike. See [`crate::fault`] for the table.
    pub fault: Option<Fault>,
}

/// The transfer-view analogue of [`crate::scene::SceneInput`]: the
/// canonical intermediate both a live frame and a snapshot derivation
/// funnel through, so the two paths cannot drift.
pub struct TransferInput {
    pub freqs: Vec<f64>,
    pub magnitude_db: Vec<f64>,
    /// Wire phase — already session-compensated. See the module doc.
    pub phase_deg: Vec<f64>,
    pub coherence: Vec<f64>,
    /// τ_sess, this session's frozen estimate.
    pub delay_ms: f64,
    /// Whether [`Self::delay_ms`] is a measured lock, with
    /// [`WireFrame::delay_locked`]'s three-way meaning. Only `Some(true)`
    /// earns a metres figure in the delay readout — see
    /// [`format_delay_readout`].
    pub delay_locked: Option<bool>,
    /// Speed of sound for that conversion, in m/s. `None` falls back to
    /// [`SPEED_OF_SOUND_DEFAULT_M_S`].
    pub speed_of_sound_m_s: Option<f64>,
    /// This pair's channel numbers, named in the delay readout's
    /// calibration/warning lines (#243) — distinct from
    /// [`Self::channel_role`], which is a display label, not a wire
    /// identity.
    pub meas_channel: i64,
    pub ref_channel: i64,
    /// This pair's stored distance-calibration constant (#243), or `None`
    /// when the session named no `distance_setup_id` or none was found —
    /// either way the delay readout's metres figure is not shown.
    pub distance_cal: Option<DistanceCalibration>,
    /// Session-scoped plausibility ceiling for the delay readout's metres
    /// figure, in metres (#243). `None` disables the plausibility check.
    pub distance_plausible_max_m: Option<f64>,
    pub meas_peak_dbfs: Option<f64>,
    pub ref_peak_dbfs: Option<f64>,
    pub channel_role: String,
    pub source: Source,
    pub sr: u32,
    /// Per-column provenance, present only on the live three-stage path.
    /// Empty for a snapshot derivation, which is still Welch-derived (#221).
    pub column_df: Vec<f64>,
    pub column_window_s: Vec<f64>,
    /// Blocks averaged behind each column, and the source bins each column
    /// spans. Raw inputs, deliberately not combined into a single "effective
    /// depth": the coherence floor depends on both, sublinearly in bins, and
    /// no validated model exists. Two successive models were wrong, one of
    /// them shipped. See `design-mtw-ladder.md`.
    pub column_n: Vec<f64>,
    pub column_bins: Vec<usize>,
    /// The ladder description behind those columns, shallowest rung first.
    ///
    /// Session-static: it derives from `sr`, which does not change
    /// mid-session, so everything built from it is fixed for the session's
    /// lifetime and cannot shift frame to frame. Empty for a snapshot
    /// derivation, which is Welch-derived and has no ladder (#221).
    pub stages: Vec<MtwStage>,
    /// The fault indicator's frame-derived inputs (#228). `None` disables
    /// the indicator: a snapshot derivation has no live drive or lock state
    /// to report, and neither does a daemon predating the field.
    pub fault: Option<FaultFrame>,
}

impl TransferInput {
    /// Adapt a live `transfer_stream` frame. `phase_deg` is carried
    /// through as-is — it is already session-compensated (see the module
    /// doc), and `delay_ms` is τ_sess for this session.
    pub fn from_wire_frame(frame: &WireFrame) -> TransferInput {
        // The three-stage columns are the display's source. The frame still
        // carries the full-rate Welch arrays, and this deliberately does not
        // read them: they are a different measurement (1 Hz flat, sliding
        // re-segmentation, uniform density with interpolation below 69 Hz),
        // and falling back to them when the ladder is not yet warm would
        // change the display's resolution and settling mid-session without
        // saying so. No trace is the honest state for the ~2.56 s the bottom
        // rung takes to settle; the meters and delay readout stay live
        // throughout, which is what gain staging needs.
        let mtw = frame.mtw.as_ref().filter(|m| m.lengths_agree());
        let (
            freqs,
            magnitude_db,
            phase_deg,
            coherence,
            column_df,
            column_window_s,
            column_n,
            column_bins,
        ) = match mtw {
            Some(m) => (
                m.freqs.clone(),
                m.magnitude_db.clone(),
                m.phase_deg.clone(),
                m.coherence.clone(),
                m.df.clone(),
                m.window_s.clone(),
                m.n.clone(),
                m.bins.clone(),
            ),
            None => Default::default(),
        };
        // Carried from the same filtered `mtw`: a length-mismatched frame
        // draws no trace, and labelling the resolution of a curve that is
        // not on screen would describe a measurement the operator cannot
        // see.
        let stages = mtw.map(|m| m.stages.clone()).unwrap_or_default();
        TransferInput {
            freqs,
            magnitude_db,
            phase_deg,
            coherence,
            delay_ms: frame.delay_ms,
            delay_locked: frame.delay_locked,
            speed_of_sound_m_s: frame.speed_of_sound_m_s,
            meas_channel: frame.meas_channel,
            ref_channel: frame.ref_channel,
            distance_cal: frame
                .distance_cal
                .as_ref()
                .map(DistanceCalibration::from_wire),
            distance_plausible_max_m: frame.distance_plausible_max_m,
            meas_peak_dbfs: frame.meas_peak_dbfs,
            ref_peak_dbfs: frame.ref_peak_dbfs,
            channel_role: format!("meas_{}", frame.meas_channel),
            source: Source::Live,
            sr: frame.sr,
            column_df,
            column_window_s,
            column_n,
            column_bins,
            stages,
            fault: FaultFrame::from_wire_frame(frame),
        }
    }

    /// Adapt an offline snapshot derivation. This is deliverable 3's
    /// mechanism: [`stored_delay_ms`] reads the derivation's frozen delay
    /// (`d.h1.delay_ms`) back out, so a live frame can be de-rotated by a
    /// snapshot's delay — `DerotMode::Snapshot { snapshot_delay_ms:
    /// TransferInput::stored_delay_ms(d) }` — and the two land on a
    /// common reference (F2′). A snapshot has no input-level meters (it
    /// is a static capture, not a live gain-staging aid), so its peaks
    /// are `None`.
    pub fn from_pair_derivation(d: &PairDerivation, channel_role: &str, sr: u32) -> TransferInput {
        TransferInput {
            freqs: d.h1.freqs.clone(),
            magnitude_db: d.h1.magnitude_db.clone(),
            phase_deg: d.h1.phase_deg.clone(),
            coherence: d.h1.coherence.clone(),
            delay_ms: d.h1.delay_ms,
            // A `PairDerivation` records no lock verdict — `derive_pair`
            // takes a `delay_samples` it is handed and asks no questions, so
            // a snapshot of a session that never locked carries the same
            // `0` as one that did. `None` is the honest value, and it reads
            // through `format_delay_readout` as milliseconds without metres:
            // an offline derivation states the delay it was built with and
            // makes no claim about the room it came from.
            delay_locked: None,
            // A snapshot carries no room temperature either — it predates
            // the field or was captured elsewhere — so the readout falls
            // back to the conventional speed. Moot while the metres are
            // gated off above, and set explicitly so it stays correct if a
            // future snapshot format does record a lock.
            speed_of_sound_m_s: None,
            // `PairDerivation` carries no wire channel identity — a
            // `channel_role` label is all the caller has (see above). `-1`
            // is never a real channel number, and is moot regardless: with
            // `delay_locked: None` above, `format_delay_readout` never
            // reaches the branch that names a channel.
            meas_channel: -1,
            ref_channel: -1,
            // A snapshot carries no distance calibration either — same
            // reasoning as `speed_of_sound_m_s` above, and moot for the
            // same reason.
            distance_cal: None,
            distance_plausible_max_m: None,
            meas_peak_dbfs: None,
            ref_peak_dbfs: None,
            channel_role: channel_role.to_string(),
            source: Source::Snapshot,
            sr,
            // A snapshot is still derived by the full-rate Welch path, so it
            // has no per-column resolution or averaging depth to report. That
            // divergence from the live view is #221; this slice makes it
            // visible rather than fixing it.
            column_df: Vec::new(),
            column_window_s: Vec::new(),
            column_n: Vec::new(),
            column_bins: Vec::new(),
            // No ladder either, for the same reason — and so no per-band
            // labels: a Welch derivation has one resolution and one settling
            // time across the whole axis, and three labels claiming otherwise
            // would be the divergence in #221 dressed as a feature.
            stages: Vec::new(),
            // A snapshot is a static capture. There is no drive to observe
            // and no lock being maintained, so there is nothing for the
            // indicator to say — the same reason its meters are `None`.
            fault: None,
        }
    }

    /// A snapshot derivation's stored (frozen) delay in ms — the τ_snap
    /// a live trace is de-rotated by to overlay it (deliverable 3).
    pub fn stored_delay_ms(d: &PairDerivation) -> f64 {
        d.h1.delay_ms
    }
}

impl TransferScene {
    /// Build the scene. `derot` selects the phase mode; `meters` and `fault`
    /// carry the cross-frame state; `now_s` is scene time.
    pub fn from_input(
        input: &TransferInput,
        modes: DisplayModes,
        freq_range: (f64, f64),
        db_range: (f64, f64),
        meters: &mut (MeterState, MeterState),
        fault: &mut FaultState,
        now_s: f64,
    ) -> TransferScene {
        let (f_min, f_max) = freq_range;
        let (db_min, db_max) = db_range;
        let tau = modes.derot.tau_derot_ms(input.delay_ms);

        let provenance = Provenance {
            channel_role: input.channel_role.clone(),
            source: input.source,
            sr: input.sr,
        };

        // A conforming daemon sends the four transfer arrays equal-length.
        // If they disagree, the frame's producer is malformed — and the
        // four are independent JSON fields, so nothing guarantees the
        // prefixes are mutually aligned: a producer that dropped trailing
        // columns and one that misaligned the arrays entirely present the
        // same symptom. Truncating to the common length would draw the
        // second case as if it were truth, fabricating data at exactly the
        // moment the input is known-bad — the same display-truth argument
        // that rules out clamping. So a mismatched frame contributes NO
        // transfer traces (empty segments); the render path stays alive
        // and draws the next good frame. `ac-view` parses partial frames
        // by design (WireFrame is `#[serde(default)]`-lenient), so this
        // must never panic.
        let lengths_agree = input.freqs.len() == input.magnitude_db.len()
            && input.freqs.len() == input.phase_deg.len()
            && input.freqs.len() == input.coherence.len();

        let (mag_segments, phase_segments) = if lengths_agree {
            // De-rotate first, then smooth. The two orders do not commute,
            // and this one is right for a reason, not by accident:
            // de-rotation removes a linear phase ramp, so what smoothing then
            // averages is a curve that is already flat where the response is
            // flat, and its detail survives. Smoothing the wire phase first
            // would average across a steep 360·f·τ ramp — the ramp is not
            // constant across a window, so the average would flatten real
            // structure along with it, and the de-rotation applied afterwards
            // could not put back what the averaging had already removed.
            let phase_derot: Vec<f64> = (0..input.freqs.len())
                .map(|i| derotate_deg(input.phase_deg[i], input.freqs[i], tau))
                .collect();

            let (magnitude_db, phase_deg) = match modes.smoothing.bpo() {
                None => (input.magnitude_db.clone(), phase_derot),
                Some(bpo) => {
                    // The smoother's mask is the drawn mask: a column the
                    // display refuses to show may not move one it does show.
                    // `input.coherence` itself goes to `split_on_mask`
                    // untouched — smoothing it would let a heavy setting
                    // un-gap the trace, which is the one direction this
                    // control must not be able to fail in.
                    let valid: Vec<bool> = input
                        .coherence
                        .iter()
                        .map(|&c| c >= COHERENCE_THRESHOLD)
                        .collect();
                    (
                        smooth_db(&input.freqs, &input.magnitude_db, &valid, bpo),
                        // The smoother returns unwrapped degrees by
                        // contract; `wrap_deg` is this crate's one wrap site
                        // and stays the only one.
                        smooth_unwrapped_phase_deg(&input.freqs, &phase_derot, &valid, bpo)
                            .into_iter()
                            .map(wrap_deg)
                            .collect(),
                    )
                }
            };

            let mag_points = |i: usize| {
                // db_to_y is the crate's one dB→y mapping — do not
                // re-implement it, and do not clamp: an over-range
                // magnitude runs off-canvas (the viewport clips it), which
                // is honest. Pinning it to the pane border would fabricate
                // a value at exactly the overload moment the display must
                // not lie about.
                (
                    freq_to_x(input.freqs[i], f_min, f_max),
                    db_to_y(magnitude_db[i], db_min, db_max),
                )
            };
            let phase_points = |i: usize| {
                let phi = phase_deg[i];
                // phase_to_y is the crate's one phase→y mapping — the same
                // function the phase axis ticks use, so a gridline and a
                // trace point at the same degrees agree by construction
                // (the AC3 shared-mapping law, extended to the phase pane).
                (freq_to_x(input.freqs[i], f_min, f_max), phase_to_y(phi))
            };
            (
                split_on_mask(&input.coherence, mag_points),
                split_on_mask(&input.coherence, phase_points),
            )
        } else {
            (Vec::new(), Vec::new())
        };

        let delay = format_delay_readout(
            input.delay_ms,
            input.delay_locked,
            input
                .speed_of_sound_m_s
                .unwrap_or(SPEED_OF_SOUND_DEFAULT_M_S),
            input.meas_channel,
            input.ref_channel,
            input.distance_cal.as_ref(),
            input.distance_plausible_max_m,
        );

        TransferScene {
            magnitude: Trace {
                segments: mag_segments,
                provenance: provenance.clone(),
            },
            phase: Trace {
                segments: phase_segments,
                provenance,
            },
            freq_axis: crate::ticks::freq_axis(f_min, f_max),
            mag_axis: crate::ticks::db_axis(db_min, db_max),
            phase_axis: crate::ticks::phase_axis(),
            delay_readout: delay.delay_readout,
            delay_calibration: delay.delay_calibration,
            delay_warning: delay.delay_warning,
            smoothing_readout: modes.smoothing.label(),
            // Derived from the ladder alone, never from the frame's columns:
            // the same session yields the same labels on every frame, so
            // they sit still while the curve moves.
            band_labels: band_labels(&input.stages, f_min, f_max),
            meas_meter: meters.0.update(input.meas_peak_dbfs, now_s),
            ref_meter: meters.1.update(input.ref_peak_dbfs, now_s),
            // Reads `input.coherence` — the same columns the mask above
            // drew from, so `CHECK ROUTING` cannot claim the legs are
            // unrelated while the panes still show a trace. A
            // length-mismatched frame leaves that array empty, which the
            // indicator reads as an unsettled ladder rather than a dead
            // one: a malformed frame must no more fabricate a fault than
            // it may fabricate a trace.
            fault: fault.update(
                &FaultInput {
                    frame: input.fault,
                    meas_peak_dbfs: input.meas_peak_dbfs,
                    ref_peak_dbfs: input.ref_peak_dbfs,
                    coherence: if lengths_agree { &input.coherence } else { &[] },
                },
                now_s,
            ),
        }
    }
}

/// Emit `point(i)` for every unmasked column, splitting into a new
/// segment wherever the mask interrupts. Masked columns are **absent** —
/// never emitted at y=0, which would draw a line to the floor and read
/// as a real measurement (D5).
fn split_on_mask(coherence: &[f64], point: impl Fn(usize) -> (f64, f64)) -> Vec<Vec<(f64, f64)>> {
    let mut segments: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut current: Vec<(f64, f64)> = Vec::new();

    for (i, &c) in coherence.iter().enumerate() {
        if c < COHERENCE_THRESHOLD {
            if !current.is_empty() {
                segments.push(std::mem::take(&mut current));
            }
        } else {
            current.push(point(i));
        }
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_interval_is_open_at_minus_180_closed_at_plus_180() {
        // The four boundary mappings from #180's ruling. The rem_euclid
        // form with `>=` returns -180 for the first three.
        assert_eq!(wrap_deg(-900.0), 180.0);
        assert_eq!(wrap_deg(-180.0), 180.0);
        assert_eq!(wrap_deg(180.0), 180.0);
        assert_eq!(wrap_deg(181.0), -179.0);
    }

    #[test]
    fn derot_mode_maps_against_a_session_compensated_wire() {
        let tau_sess = 2.5;
        assert_eq!(DerotMode::Session.tau_derot_ms(tau_sess), 0.0);
        assert_eq!(DerotMode::Raw.tau_derot_ms(tau_sess), -2.5);
        assert_eq!(
            DerotMode::Snapshot {
                snapshot_delay_ms: 3.0
            }
            .tau_derot_ms(tau_sess),
            0.5
        );
    }

    // ---------------------------------------------------------------
    // Per-band resolution and settling labels (#224)
    // ---------------------------------------------------------------

    /// The wire's stage list for `sr`, as the daemon builds it — the same
    /// `settling_seconds(stage, N)` it puts on the wire, with `N = 4`
    /// (`mtw_n_blocks`, the ratified depth the 2.56 s bottom figure is
    /// derived from).
    fn wire_stages(sr: u32) -> Vec<MtwStage> {
        use ac_core::visualize::mtw::{ladder, settling_seconds};
        ladder::layout(sr)
            .expect("layout")
            .stages
            .iter()
            .map(|s| MtwStage {
                decim: s.decim,
                rate: s.rate,
                df: s.df,
                window_s: s.window_s,
                hop_s: s.hop_s,
                f_valid: s.f_valid,
                settling_s: settling_seconds(s, 4),
            })
            .collect()
    }

    // The ratified figures, end to end: the three labels at 96 kHz and the
    // three positions the UX review derived from the log axis. Positions
    // are hand-derived here (`ln(f/20)/ln(1000)` at the geometric centres
    // 63.7 / 573.8 / 5697 Hz) rather than recomputed with the
    // implementation's own expression, which would assert nothing.
    #[test]
    fn band_labels_carry_the_ratified_strings_and_positions_at_96k() {
        let labels = band_labels(&wire_stages(96_000), 20.0, 20_000.0);
        let got: Vec<(f64, &str)> = labels
            .iter()
            .map(|b| (b.position, b.text.as_str()))
            .collect();
        // Shallowest rung first, matching the ladder's own order: the top
        // band is the coarse-resolution/fast-settling one.
        assert_eq!(got.len(), 3, "{got:?}");
        assert_eq!(got[0].1, "23.4 Hz / 0.11 s");
        assert_eq!(got[1].1, "2.93 Hz / 0.85 s");
        assert_eq!(got[2].1, "0.98 Hz / 2.56 s");
        for (got, want) in got.iter().zip([0.818, 0.486, 0.168]) {
            assert!(
                (got.0 - want).abs() < 5e-4,
                "position {} for {}, want {want}",
                got.0,
                got.1
            );
        }
    }

    // The rejected implementation, computed inside the test: labelling the
    // raw analysis window instead of `W + hop·(N−1)`. At the bottom rung
    // the two differ by 2.5x, and the window is the one an operator would
    // wait out and conclude the instrument had stalled.
    #[test]
    fn settling_is_the_filled_average_not_the_analysis_window() {
        let stages = wire_stages(96_000);
        let bottom = stages.last().expect("three rungs");
        // The rejected figure, derived here rather than assumed: 1.02 s.
        let window_label = format_band_label(bottom.df, bottom.window_s);
        assert_eq!(window_label, "0.98 Hz / 1.02 s");
        let labels = band_labels(&stages, 20.0, 20_000.0);
        let bottom_label = &labels.last().expect("three labels").text;
        assert_ne!(bottom_label, &window_label);
        assert_eq!(bottom_label, "0.98 Hz / 2.56 s");
        // And the gap is the 2.5x the issue names, not a rounding
        // difference.
        assert!(bottom.settling_s / bottom.window_s > 2.4);
    }

    // Three labels at every supported rate, all distinct — the claim the
    // UX review makes about the whole rate set, not just 96 kHz. 44.1 kHz
    // is the rate where the deep rungs are 0.23% off target, so its
    // strings are checked explicitly.
    #[test]
    fn three_distinct_labels_at_every_supported_rate() {
        for sr in [44_100u32, 48_000, 96_000, 192_000] {
            let labels = band_labels(&wire_stages(sr), 20.0, 20_000.0);
            assert_eq!(labels.len(), 3, "sr {sr}: {labels:?}");
            let texts: Vec<&str> = labels.iter().map(|b| b.text.as_str()).collect();
            assert!(
                texts[0] != texts[1] && texts[1] != texts[2],
                "sr {sr}: adjacent bands read the same: {texts:?}"
            );
            // Positions stay in axis order and separated — the labels must
            // not stack up on one another at any rate.
            for w in labels.windows(2) {
                assert!(
                    w[0].position - w[1].position > 0.1,
                    "sr {sr}: bands {} and {} are {:.3} apart",
                    w[0].text,
                    w[1].text,
                    w[0].position - w[1].position
                );
            }
        }
        // The 44.1 kHz deep rungs run 0.23% slow, which the labels round
        // away at the bottom and show in the middle's settling figure.
        let at_44k = band_labels(&wire_stages(44_100), 20.0, 20_000.0);
        assert_eq!(at_44k[2].text, "0.98 Hz / 2.55 s");
    }

    // The label content is a function of the ladder alone. Two frames of
    // the same session differ in every column and in the fault state, and
    // must produce byte-identical labels — they describe the measurement's
    // geometry, not its values, and a label that twitched with the data
    // would read as one.
    #[test]
    fn labels_do_not_move_or_change_between_frames_of_one_session() {
        let stages = wire_stages(48_000);
        let scene_for = |mag: f64, coh: f64| {
            let inp = TransferInput {
                freqs: vec![100.0, 1_000.0, 10_000.0],
                magnitude_db: vec![mag; 3],
                phase_deg: vec![0.0; 3],
                coherence: vec![coh; 3],
                delay_ms: 0.0,
                delay_locked: Some(true),
                speed_of_sound_m_s: None,
                meas_channel: 0,
                ref_channel: 1,
                distance_cal: None,
                distance_plausible_max_m: None,
                meas_peak_dbfs: Some(-20.0),
                ref_peak_dbfs: Some(-20.0),
                channel_role: "meas_0".to_string(),
                source: Source::Live,
                sr: 48_000,
                column_df: Vec::new(),
                column_window_s: Vec::new(),
                column_n: Vec::new(),
                column_bins: Vec::new(),
                stages: stages.clone(),
                fault: None,
            };
            let mut meters = (MeterState::default(), MeterState::default());
            TransferScene::from_input(
                &inp,
                DisplayModes::new(DerotMode::Session, Smoothing::Off),
                (20.0, 20_000.0),
                (-80.0, 20.0),
                &mut meters,
                &mut FaultState::default(),
                0.0,
            )
            .band_labels
        };
        assert_eq!(scene_for(-6.0, 0.9), scene_for(40.0, 0.1));
        assert_eq!(scene_for(-6.0, 0.9).len(), 3);
    }

    // A frame with no ladder description labels nothing. That covers the
    // warm-up (no `mtw` yet), a snapshot derivation, and a daemon
    // predating the ladder — in all three the display has no per-band
    // resolution to report, and inventing one would be the failure the
    // labels exist to prevent.
    #[test]
    fn no_ladder_means_no_labels() {
        assert!(band_labels(&[], 20.0, 20_000.0).is_empty());
        // A stage the daemon described only partially — `df`/`settling_s`
        // absent arrive as 0.0 — is skipped rather than drawn as
        // "0.00 Hz / 0.00 s".
        let partial = vec![MtwStage {
            f_valid: 67.6,
            ..Default::default()
        }];
        assert!(band_labels(&partial, 20.0, 20_000.0).is_empty());
    }

    // Zoom is a display range, and a band that has left the visible axis
    // is dropped rather than pinned to the edge: its label would name
    // frequencies not on screen. The bands still on screen keep their
    // geometric centres over what remains visible.
    #[test]
    fn bands_clamped_off_a_zoomed_axis_are_dropped() {
        let stages = wire_stages(96_000);
        // Zoomed into the top band alone (stage 0 runs upward from its
        // 1623 Hz validity edge).
        let labels = band_labels(&stages, 2_000.0, 20_000.0);
        assert_eq!(labels.len(), 1, "{labels:?}");
        assert_eq!(labels[0].text, "23.4 Hz / 0.11 s");
        // sqrt(2000·20000) = 6324.6 Hz — the centre of what is visible,
        // not of the band's full span.
        assert!((labels[0].position - 0.5).abs() < 1e-9, "{labels:?}");
        // A degenerate range labels nothing rather than producing NaN
        // positions, as `ticks::freq_axis` does.
        assert!(band_labels(&stages, 0.0, 20_000.0).is_empty());
        assert!(band_labels(&stages, 20_000.0, 20.0).is_empty());
        assert!(band_labels(&stages, f64::NAN, 20_000.0).is_empty());
    }

    // The precision rule, pinned at both sides of its threshold — a
    // significant-figure rule would round 23.4375 to 23 and 0.9766 to
    // 0.98, and only one of those matches the ratified set.
    #[test]
    fn band_figures_are_two_decimals_below_ten_and_one_above() {
        assert_eq!(format_band_label(0.976_562_5, 2.56), "0.98 Hz / 2.56 s");
        assert_eq!(format_band_label(23.4375, 0.106_667), "23.4 Hz / 0.11 s");
        // Exactly at the threshold, and above it in both figures.
        assert_eq!(format_band_label(10.0, 9.99), "10.0 Hz / 9.99 s");
        assert_eq!(format_band_label(46.875, 12.34), "46.9 Hz / 12.3 s");
    }

    /// A stored constant with no plausibility ceiling, at the issue's own
    /// rig figures (`work/rig/rig-243-343-results.md`, 2026-08-18).
    fn dummy_distance_cal() -> DistanceCalibration {
        DistanceCalibration {
            constant_ms: 1.0615,
            setup_id: "mic1".to_string(),
            captured_at: "2026-08-18T11:35:14Z".to_string(),
        }
    }

    #[test]
    fn a_locked_delay_with_no_stored_constant_states_ms_only_and_says_why() {
        // #243's core defect: converting the whole locked delay — including
        // the part that never travelled through air — used to read
        // "(0.86 m)" here unconditionally. With no stored constant for this
        // pair, metres are withheld entirely rather than computed from a
        // zero offset, and the readout says why.
        let got = format_delay_readout(
            2.5,
            Some(true),
            SPEED_OF_SOUND_DEFAULT_M_S,
            0,
            3,
            None,
            None,
        );
        assert_eq!(got.delay_readout, "2.50 ms");
        assert_eq!(
            got.delay_calibration.as_deref(),
            Some("not calibrated — pair meas0↔ref3")
        );
        assert_eq!(got.delay_warning, None);
    }

    #[test]
    fn a_calibrated_delay_subtracts_the_constant_before_the_speed_conversion() {
        // The issue's own motivating figures: 4.08 ms locked, 1.0615 ms of
        // it not flight time, converted at the 26 \u{b0}C the rig ran
        // (347.056 m/s) — not the 1.40 m the uncorrected 343 m/s path
        // produces.
        let c = ac_core::shared::conversions::speed_of_sound_at(26.0);
        let cal = dummy_distance_cal();
        let got = format_delay_readout(4.08, Some(true), c, 0, 3, Some(&cal), None);
        let want_metres = (4.08 - 1.0615) / 1000.0 * c;
        assert_eq!(got.delay_readout, format!("4.08 ms    {want_metres:.3} m"));
        // Materially smaller than the uncorrected reading this issue was
        // filed on.
        assert!(
            want_metres < 1.2,
            "got {want_metres} m, expected well under the uncorrected 1.40 m"
        );
        assert_eq!(
            got.delay_calibration.as_deref(),
            Some("cal 2026-08-18 · mic1 · pair meas0↔ref3")
        );
    }

    #[test]
    fn the_room_temperature_still_moves_the_metres_once_calibrated() {
        // #243's second error, preserved alongside the first: with the
        // constant held at zero (isolating the temperature effect), the
        // same 4.08 ms lock reads 1.40 m at the hardcoded 343 and 1.41 m at
        // the 24 \u{b0}C the rig room ran.
        let zero_cal = DistanceCalibration {
            constant_ms: 0.0,
            ..dummy_distance_cal()
        };
        let warm = ac_core::shared::conversions::speed_of_sound_at(24.0);
        assert_eq!(
            format_delay_readout(
                4.08,
                Some(true),
                SPEED_OF_SOUND_DEFAULT_M_S,
                0,
                3,
                Some(&zero_cal),
                None
            )
            .delay_readout,
            "4.08 ms    1.399 m"
        );
        assert_eq!(
            format_delay_readout(4.08, Some(true), warm, 0, 3, Some(&zero_cal), None).delay_readout,
            "4.08 ms    1.411 m"
        );
    }

    #[test]
    fn metres_need_a_lock() {
        // The reachable defect: before a lock the frame's delay_ms is 0.0, a
        // placeholder, and the readout used to convert it into "(0.00 m)" —
        // a claim about the room from a delay the estimator had not made.
        // No calibration line either: the pair's calibration status is moot
        // until it has a delay to apply it to.
        let cal = dummy_distance_cal();
        for unlocked in [None, Some(false)] {
            let got = format_delay_readout(
                0.0,
                unlocked,
                SPEED_OF_SOUND_DEFAULT_M_S,
                0,
                3,
                Some(&cal),
                None,
            );
            assert_eq!(
                got.delay_readout, "0.00 ms",
                "an unlocked pair must not state a distance"
            );
            assert_eq!(got.delay_calibration, None);
        }
        // Milliseconds are still the frame's own field, reported unchanged.
        assert_eq!(
            format_delay_readout(
                4.08,
                Some(false),
                SPEED_OF_SOUND_DEFAULT_M_S,
                0,
                3,
                Some(&cal),
                None
            )
            .delay_readout,
            "4.08 ms"
        );
    }

    #[test]
    fn a_negative_or_nan_delay_states_no_distance() {
        // Dormant against today's causal-only estimator — see
        // `format_delay_readout`'s doc — so this is what keeps the branch
        // honest rather than a claim that it fires.
        let cal = dummy_distance_cal();
        assert_eq!(
            format_delay_readout(
                -0.5,
                Some(true),
                SPEED_OF_SOUND_DEFAULT_M_S,
                0,
                3,
                Some(&cal),
                None
            )
            .delay_readout,
            "-0.50 ms"
        );
        assert_eq!(
            format_delay_readout(
                f64::NAN,
                Some(true),
                SPEED_OF_SOUND_DEFAULT_M_S,
                0,
                3,
                Some(&cal),
                None
            )
            .delay_readout,
            "NaN ms"
        );
    }

    #[test]
    fn a_readout_over_the_plausible_ceiling_warns() {
        // The case this issue was filed on: a readout larger than the true
        // distance, positive and locked. A floor or negative-delay guard is
        // the wrong sign for this — only an explicit ceiling catches it.
        let zero_cal = DistanceCalibration {
            constant_ms: 0.0,
            ..dummy_distance_cal()
        };
        let got = format_delay_readout(
            4.08,
            Some(true),
            SPEED_OF_SOUND_DEFAULT_M_S,
            0,
            3,
            Some(&zero_cal),
            Some(1.2), // taped at 1.0 m; the 1.40 m reading is implausible
        );
        assert_eq!(got.delay_readout, "4.08 ms    1.399 m");
        assert_eq!(
            got.delay_warning.as_deref(),
            Some("readout exceeds plausible bound for this pair — check wiring or re-calibrate")
        );
    }

    #[test]
    fn a_readout_inside_the_plausible_ceiling_is_silent() {
        let cal = dummy_distance_cal();
        let c = ac_core::shared::conversions::speed_of_sound_at(26.0);
        let got = format_delay_readout(4.08, Some(true), c, 0, 3, Some(&cal), Some(1.5));
        assert_eq!(got.delay_warning, None);
    }

    #[test]
    fn no_plausible_ceiling_disables_the_check() {
        // No default — an unset ceiling must not guess a room size, so a
        // grossly implausible reading is silent rather than refused.
        let zero_cal = DistanceCalibration {
            constant_ms: 0.0,
            ..dummy_distance_cal()
        };
        let got = format_delay_readout(
            4.08,
            Some(true),
            SPEED_OF_SOUND_DEFAULT_M_S,
            0,
            3,
            Some(&zero_cal),
            None,
        );
        assert_eq!(got.delay_warning, None);
    }

    #[test]
    fn a_stored_constant_from_one_setup_does_not_silently_apply_to_another() {
        // This is a display-layer check, not a lookup — `format_delay_readout`
        // takes whatever `Option<&DistanceCalibration>` its caller resolved,
        // and the daemon-side exact-match-or-refuse lookup
        // (`Calibration::distance_cal_for`) is what keeps a mismatched
        // `setup_id` from ever reaching here as `Some`. What this asserts is
        // the half of that guarantee visible at this layer: the calibration
        // line always names the setup it actually used, so a reader can
        // catch a wrong one on sight rather than trusting it silently.
        let cal_a = DistanceCalibration {
            setup_id: "mic1".to_string(),
            ..dummy_distance_cal()
        };
        let cal_b = DistanceCalibration {
            setup_id: "mic2-moved".to_string(),
            ..dummy_distance_cal()
        };
        let got_a = format_delay_readout(
            4.08,
            Some(true),
            SPEED_OF_SOUND_DEFAULT_M_S,
            0,
            3,
            Some(&cal_a),
            None,
        );
        let got_b = format_delay_readout(
            4.08,
            Some(true),
            SPEED_OF_SOUND_DEFAULT_M_S,
            0,
            3,
            Some(&cal_b),
            None,
        );
        assert_ne!(got_a.delay_calibration, got_b.delay_calibration);
        assert!(got_a.delay_calibration.unwrap().contains("mic1"));
        assert!(got_b.delay_calibration.unwrap().contains("mic2-moved"));
    }

    #[test]
    fn the_fallback_speed_is_the_pre_243_constant() {
        // Not `speed_of_sound_at(20.0)` (343.42): a frame that names no
        // speed must reproduce the old behaviour exactly, not a nearby
        // derived value.
        assert!((SPEED_OF_SOUND_DEFAULT_M_S - 343.0).abs() < 1e-12);
        assert!(
            (SPEED_OF_SOUND_DEFAULT_M_S - ac_core::shared::conversions::speed_of_sound_at(20.0))
                .abs()
                > 0.4
        );
    }

    #[test]
    fn meter_height_floors_at_minus_60_and_saturates_at_zero() {
        assert!((meter_height(Some(-6.0206)) - 0.899_656_666_666_666_6).abs() < 1e-9);
        assert_eq!(meter_height(Some(0.0)), 1.0);
        assert_eq!(meter_height(Some(-60.0)), 0.0);
        assert_eq!(meter_height(Some(-90.0)), 0.0);
        assert_eq!(meter_height(None), 0.0);
        assert_eq!(meter_height(Some(f64::NEG_INFINITY)), 0.0);
        assert!(!meter_height(Some(f64::NAN)).is_nan());
    }

    #[test]
    fn clip_latch_holds_for_three_seconds_then_clears() {
        let mut st = MeterState::default();
        assert!(!st.update(Some(-6.0), 0.0).clip_latch);
        assert!(st.update(Some(0.0), 1.0).clip_latch);
        // Still latched 2.9 s later even though the level dropped.
        assert!(st.update(Some(-40.0), 3.9).clip_latch);
        assert!(!st.update(Some(-40.0), 4.1).clip_latch);
    }

    #[test]
    fn peak_hold_decays_after_about_one_and_a_half_seconds() {
        let mut st = MeterState::default();
        let m = st.update(Some(-6.0206), 0.0);
        assert!((m.hold - m.height).abs() < 1e-12);
        // Lower level, inside the hold window: tick stays up.
        let m = st.update(Some(-40.0), 1.0);
        assert!(m.hold > m.height);
        // Past the window: tick follows the level down.
        let m = st.update(Some(-40.0), 2.0);
        assert!((m.hold - m.height).abs() < 1e-12);
    }

    // QA gap: the hold-decay boundary itself. The implementation resets
    // the tick when `now - hold_set_at >= PEAK_HOLD_S`, so at exactly the
    // window the tick releases. Pins the `>=` convention.
    #[test]
    fn peak_hold_releases_at_exactly_the_window_boundary() {
        let mut st = MeterState::default();
        st.update(Some(-6.0206), 0.0);
        // Exactly PEAK_HOLD_S later, with a lower level: the `>=` means
        // the hold releases to the current height on this frame.
        let m = st.update(Some(-40.0), PEAK_HOLD_S);
        assert!((m.hold - m.height).abs() < 1e-12);
    }

    // QA gap: phase-pane normalization asserted directly, not through the
    // test-helper's inverse — a matched sign flip in mapping + helper
    // would cancel and hide itself. −180 → 0.0, 0 → 0.5, +180 → 1.0, read
    // straight off the segment.
    #[test]
    fn phase_pane_normalization_is_pinned_directly() {
        // Three columns whose wire phase de-rotates (session mode, τ=0)
        // to −180, 0, +180 — chosen as literal wire values so no derot
        // arithmetic is involved.
        let inp = TransferInput {
            freqs: vec![100.0, 200.0, 300.0],
            magnitude_db: vec![0.0; 3],
            phase_deg: vec![-180.0, 0.0, 180.0],
            coherence: vec![0.9; 3],
            delay_ms: 0.0,
            delay_locked: Some(true),
            speed_of_sound_m_s: None,
            meas_channel: 0,
            ref_channel: 1,
            distance_cal: None,
            distance_plausible_max_m: None,
            meas_peak_dbfs: None,
            ref_peak_dbfs: None,
            channel_role: "meas_0".to_string(),
            source: Source::Live,
            sr: 48_000,
            // Welch-derived fixture: no per-column provenance to carry.
            column_df: Vec::new(),
            column_window_s: Vec::new(),
            column_n: Vec::new(),
            column_bins: Vec::new(),
            stages: Vec::new(),
            fault: None,
        };
        let mut meters = (MeterState::default(), MeterState::default());
        let s = TransferScene::from_input(
            &inp,
            DisplayModes::new(DerotMode::Session, Smoothing::Off),
            (20.0, 20_000.0),
            (-80.0, 20.0),
            &mut meters,
            &mut FaultState::default(),
            0.0,
        );
        let ys: Vec<f64> = s.phase.segments[0].iter().map(|p| p.1).collect();
        // wrap((−180,+180]) sends −180 to +180, so BOTH ends map to 1.0
        // and the midpoint to 0.5 — the pane is single-valued at the
        // wrap seam, which is the intended behaviour, not a bug.
        assert!((ys[0] - 1.0).abs() < 1e-12, "−180 wire → {}", ys[0]);
        assert!((ys[1] - 0.5).abs() < 1e-12, "0 → {}", ys[1]);
        assert!((ys[2] - 1.0).abs() < 1e-12, "+180 → {}", ys[2]);
    }

    // QA issue 1 regression: a length-mismatched frame contributes NO
    // transfer traces — it is omitted, not truncated-and-drawn. The four
    // arrays are independent JSON fields with no prefix-alignment
    // guarantee, so drawing a common prefix would fabricate data from a
    // known-malformed producer. Absence is asserted, not truncated
    // presence. Must not panic — the render path is live and
    // keypress-adjacent, and WireFrame parses partial frames by design.
    #[test]
    fn length_mismatch_omits_transfer_traces_entirely() {
        let inp = TransferInput {
            freqs: vec![100.0, 200.0],
            magnitude_db: vec![0.0, 0.0],
            phase_deg: vec![0.0, 0.0],
            // Longer than the rest — a malformed frame.
            coherence: vec![0.9, 0.9, 0.9, 0.9],
            delay_ms: 0.0,
            delay_locked: Some(true),
            speed_of_sound_m_s: None,
            meas_channel: 0,
            ref_channel: 1,
            distance_cal: None,
            distance_plausible_max_m: None,
            meas_peak_dbfs: None,
            ref_peak_dbfs: None,
            channel_role: "meas_0".to_string(),
            source: Source::Live,
            sr: 48_000,
            // Welch-derived fixture: no per-column provenance to carry.
            column_df: Vec::new(),
            column_window_s: Vec::new(),
            column_n: Vec::new(),
            column_bins: Vec::new(),
            stages: Vec::new(),
            fault: None,
        };
        let mut meters = (MeterState::default(), MeterState::default());
        let s = TransferScene::from_input(
            &inp,
            DisplayModes::new(DerotMode::Session, Smoothing::Off),
            (20.0, 20_000.0),
            (-80.0, 20.0),
            &mut meters,
            &mut FaultState::default(),
            0.0,
        );
        // No segments on either pane — the frame drew nothing, no panic.
        assert!(s.magnitude.segments.is_empty(), "magnitude not omitted");
        assert!(s.phase.segments.is_empty(), "phase not omitted");
        // The meters still update — they are independent of the trace
        // arrays and come from the peak fields, which are not part of the
        // mismatch.
        let _ = s.meas_meter;
    }

    // QA gap: a coherence mask that touches the array ends. F3 masks an
    // interior run; a leading/trailing masked column would expose an
    // off-by-one that an interior-only test cannot.
    #[test]
    fn mask_at_both_ends_produces_no_empty_edge_segments() {
        let coherence = [0.3, 0.9, 0.9, 0.3];
        let seg = split_on_mask(&coherence, |i| (i as f64, 0.0));
        // One interior segment of the two live columns; no leading or
        // trailing empty segment.
        assert_eq!(seg.len(), 1);
        assert_eq!(seg[0].len(), 2);
        assert_eq!(seg[0][0].0, 1.0);
        assert_eq!(seg[0][1].0, 2.0);
    }

    // Deliverable 3: a snapshot derivation's stored delay is readable and
    // is exactly what a live frame de-rotates by to overlay it.
    #[test]
    fn snapshot_stored_delay_round_trips_into_the_derot_mode() {
        use ac_core::visualize::transfer::h1_estimate_with_delay;
        // Build a PairDerivation with a known frozen delay by running the
        // estimator with a caller-supplied delay, then wrap it.
        let sr = 48_000u32;
        let n = sr as usize;
        let r: Vec<f32> = (0..n).map(|i| ((i as f64 * 0.01).sin()) as f32).collect();
        let m = r.clone();
        let h1 = h1_estimate_with_delay(&r, &m, sr, 144); // 3.0 ms at 48 kHz
        let d = PairDerivation {
            h1,
            spec_freqs: vec![],
            meas_spectrum: vec![],
            ref_spectrum: vec![],
            spl: None,
            spl_weighting: ac_core::visualize::weighting_curves::WeightingCurve::Z,
        };
        let tau_snap = TransferInput::stored_delay_ms(&d);
        assert!((tau_snap - 3.0).abs() < 1e-9, "stored delay {tau_snap}");
        // And it feeds the snapshot derot mode as τ_snap.
        assert_eq!(
            DerotMode::Snapshot {
                snapshot_delay_ms: tau_snap
            }
            .tau_derot_ms(2.5),
            0.5
        );
    }
}
