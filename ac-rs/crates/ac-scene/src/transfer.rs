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
//! `delay_samples` once and freezes it (D4). So [`ac_core::wire::TransferFrame`]'s
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
use ac_core::wire::{MtwColumns, MtwStage, TransferFrame};

/// Columns below this coherence are masked out of both panes by default,
/// and — fixed, whatever the display uses — what the fault indicator's
/// `CHECK ROUTING` rule reads (#670: the operator's mask is display policy;
/// the fault rule does not move with it).
pub const COHERENCE_THRESHOLD: f64 = 0.5;

/// The display mask settings `B` cycles through (#670, Smaart's coherence
/// blanking; the guide states no default, `ac` keeps 0.5).
pub const COHERENCE_MASK_STEPS: [f64; 4] = [0.3, 0.5, 0.7, 0.9];

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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayModes {
    /// Which delay the phase pane is de-rotated by (D3).
    pub derot: DerotMode,
    /// Fractional-octave smoothing of both panes (#229).
    pub smoothing: Smoothing,
    /// Columns below this coherence are not drawn (#670). Display policy
    /// only — the fault indicator keeps [`COHERENCE_THRESHOLD`].
    pub coherence_mask: f64,
}

impl DisplayModes {
    /// The defaults an opening session gets: session de-rotation, no
    /// smoothing, the default coherence mask.
    pub fn new(derot: DerotMode, smoothing: Smoothing) -> DisplayModes {
        DisplayModes {
            derot,
            smoothing,
            coherence_mask: COHERENCE_THRESHOLD,
        }
    }
}

/// Not derived: a derived default would mask at coherence 0.0 — nothing.
impl Default for DisplayModes {
    fn default() -> Self {
        DisplayModes::new(DerotMode::default(), Smoothing::default())
    }
}

impl DisplayModes {
    /// The same modes with a different coherence mask (#670).
    pub fn with_coherence_mask(self, coherence_mask: f64) -> DisplayModes {
        DisplayModes {
            coherence_mask,
            ..self
        }
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

/// Everything [`format_delay_readout`] produces.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DelayReadout {
    /// `"4.08 ms"`.
    pub delay_readout: String,
}

/// Builds [`DelayReadout`] — milliseconds only (#391 removed the ms → m
/// conversion this used to also produce, and the calibration/plausibility
/// lines that came with it).
pub fn format_delay_readout(delay_ms: f64) -> DelayReadout {
    DelayReadout {
        delay_readout: format!("{delay_ms:.2} ms"),
    }
}

/// The operator's delay control on a live frame (#669): the setting, the
/// live IR's residual against it (Smaart's Delta Delay), and who set it.
///
/// `None` on [`TransferInput::delay_control`] for a snapshot derivation, a
/// daemon predating #669, or a pair with no delay yet — there is nothing to
/// insert or nudge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelayControl {
    /// The held delay, in samples (`delay_samples`).
    pub samples: i64,
    /// Peak of the live IR as an offset from [`Self::samples`]
    /// (`delay_residual`); `None` on a frame that carries none.
    pub residual: Option<i64>,
    /// Whether [`Self::samples`] was set by the operator (`delay_operator`).
    pub operator: bool,
}

impl DelayControl {
    pub fn from_wire_frame(frame: &TransferFrame) -> Option<DelayControl> {
        (frame.delay_locked == Some(true)).then_some(DelayControl {
            samples: frame.delay_samples,
            residual: frame.delay_residual,
            operator: frame.delay_operator,
        })
    }

    /// What "Insert" sends as `set_delay`: the absolute IR-peak arrival.
    pub fn insert_samples(&self) -> Option<i64> {
        self.residual.map(|r| self.samples + r)
    }

    /// `"set · find +12 smp (+0.25 ms)"` — where the delay came from, and
    /// what Find reads against it right now.
    pub fn readout(&self, sr: u32) -> String {
        let source = if self.operator { "set" } else { "found" };
        match self.residual {
            Some(r) if sr > 0 => {
                let ms = r as f64 * 1000.0 / sr as f64;
                format!("{source} · find {r:+} smp ({ms:+.2} ms)")
            }
            Some(r) => format!("{source} · find {r:+} smp"),
            None => format!("{source} · find —"),
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

/// `"H₁ Welch {Δf} Hz flat — not the live ladder"` (#221 UX): the statement
/// a trace carries when it was derived by the full-rate Welch H₁ rather than
/// the ladder the live view runs. `Δf = sr / nperseg`, to [`band_figure`]'s
/// precision so it reads in the same register as the live band labels
/// (`1.00`, not `1`). `nperseg` is the segment length the derivation used,
/// never an assumed `sr`. A zero `sr` or `nperseg` has no resolution to state
/// and prints none.
pub fn format_estimator_readout(sr: u32, nperseg: usize) -> String {
    if sr == 0 || nperseg == 0 {
        return "H\u{2081} Welch \u{2014} not the live ladder".to_string();
    }
    let df = f64::from(sr) / nperseg as f64;
    format!(
        "H\u{2081} Welch {} Hz flat \u{2014} not the live ladder",
        band_figure(df)
    )
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
    /// `"2.50 ms"` — τ_sess, milliseconds only (#391).
    pub delay_readout: String,
    /// [`DelayControl::readout`] on a live frame with a delay; `None`
    /// otherwise (#669).
    pub delay_control_readout: Option<String>,
    /// The held delay in samples, for a ±1 nudge; `None` without a
    /// [`DelayControl`].
    pub delay_samples: Option<i64>,
    /// What "Insert" sends ([`DelayControl::insert_samples`]).
    pub delay_insert_samples: Option<i64>,
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
    /// `"coherence mask 0.70"` when the display mask is not the default
    /// (#670); `None` at the default.
    pub coherence_mask_readout: Option<String>,
    /// What data protection is doing (#670): `"paused: no reference"`,
    /// `"12 clipped buffers dropped"`, `"8 columns held (weak reference)"`,
    /// joined with `" · "`; `None` when there is nothing to say. Set by
    /// [`TransferScene::set_protection`] from the live frame.
    pub protection_readout: Option<String>,
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
    /// `"H₁ Welch 1.00 Hz flat — not the live ladder"` when this trace was
    /// derived by a different estimator than the live view (#221), `None`
    /// when it is the live ladder's. Keyed on [`TransferInput::estimator`],
    /// not on [`Source`]: a snapshot whose ladder was replayed carries
    /// `None`, one without recorded ladder provenance carries the string.
    /// The renderer draws it verbatim.
    pub estimator_readout: Option<String>,
    pub meas_meter: Meter,
    pub ref_meter: Meter,
    /// The fault indicator (#228), or `None` for "show nothing" — which is
    /// the correct display for an idle session, a warming-up one, and a
    /// healthy one alike. See [`crate::fault`] for the table.
    pub fault: Option<Fault>,
    /// Whether the voltage scale behind the spectra was verified by a
    /// session check (#466), or `None` when nothing is scaled. The session
    /// applies its gate once, at start, so this is static for the session.
    pub calibration_readout: Option<CalibrationReadout>,
}

/// The session-check state of a readout. The renderer picks the weight from
/// this, never from [`CalibrationReadout::text`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalibrationState {
    Verified,
    Unverified,
    Refused,
}

/// `voltage verified <time>`, `voltage unverified` or `voltage refused —
/// dBFS` (#466 UX), with the state that selects its weight.
#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationReadout {
    pub text: String,
    pub state: CalibrationState,
}

impl CalibrationReadout {
    /// The readout for a frame's `cal_tags` (#466 scene rule), over both
    /// legs; the stronger state wins (refused > unverified > verified).
    ///
    /// - no `cal_tags` → `None`;
    /// - a leg with `voltage: "none"` and no `voltage_check` → skipped
    ///   (nothing is scaled);
    /// - `verified` → `Verified`, with its check time;
    /// - `refused` → `Refused`;
    /// - `unverified`, an absent `voltage_check` under `voltage: "on"`, or a
    ///   `voltage_check` that does not parse → `Unverified`. A malformed
    ///   verdict never reads as verified.
    pub fn from_cal_tags(tags: Option<&serde_json::Value>) -> Option<CalibrationReadout> {
        use ac_core::shared::calibration::session::whole_seconds;
        use ac_core::shared::calibration::LayerVerdict;
        let tags = tags.filter(|t| t.is_object())?;
        let mut state: Option<CalibrationState> = None;
        let mut checked_at: Option<String> = None;
        for leg in ["meas", "ref"] {
            let Some(tag) = tags.get(leg) else {
                continue;
            };
            let applied = tag.get("voltage").and_then(|v| v.as_str()) == Some("on");
            let check = tag.get("voltage_check").filter(|v| !v.is_null());
            let leg_state = match check {
                None if !applied => continue,
                None => CalibrationState::Unverified,
                Some(v) => match serde_json::from_value::<LayerVerdict>(v.clone()) {
                    Ok(LayerVerdict::Verified(e)) => {
                        if checked_at.as_ref().is_none_or(|t| e.checked_at < *t) {
                            checked_at = Some(e.checked_at);
                        }
                        CalibrationState::Verified
                    }
                    Ok(LayerVerdict::Refused { .. }) => CalibrationState::Refused,
                    Ok(LayerVerdict::Unverified { .. }) | Err(_) => CalibrationState::Unverified,
                },
            };
            state = Some(match (state, leg_state) {
                (Some(CalibrationState::Refused), _) | (_, CalibrationState::Refused) => {
                    CalibrationState::Refused
                }
                (Some(CalibrationState::Unverified), _) | (_, CalibrationState::Unverified) => {
                    CalibrationState::Unverified
                }
                _ => CalibrationState::Verified,
            });
        }
        let state = state?;
        let text = match state {
            CalibrationState::Verified => format!(
                "voltage verified {}",
                whole_seconds(checked_at.as_deref().unwrap_or("?"))
            ),
            CalibrationState::Unverified => "voltage unverified".to_string(),
            CalibrationState::Refused => "voltage refused \u{2014} dBFS".to_string(),
        };
        Some(CalibrationReadout { text, state })
    }
}

/// Which estimator produced a [`TransferInput`]'s arrays (#221).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Estimator {
    /// The multi-time-window ladder — what the live view draws, whether
    /// from a live frame or replayed from a snapshot.
    Ladder,
    /// The full-rate Welch H₁ at one flat resolution, with the segment
    /// length it used. A snapshot without recorded ladder provenance.
    Welch { nperseg: usize },
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
    /// [`TransferFrame::delay_locked`]'s three-way meaning. Consumed by
    /// [`crate::fault`], not by the delay readout itself (#391).
    pub delay_locked: Option<bool>,
    /// The operator's delay control (#669); `None` off the live path.
    pub delay_control: Option<DelayControl>,
    /// This pair's channel numbers — distinct from [`Self::channel_role`],
    /// which is a display label, not a wire identity.
    pub meas_channel: i64,
    pub ref_channel: i64,
    pub meas_peak_dbfs: Option<f64>,
    pub ref_peak_dbfs: Option<f64>,
    pub channel_role: String,
    pub source: Source,
    pub sr: u32,
    /// Per-column provenance, present on the three-stage ladder path — a
    /// live frame, or a snapshot whose ladder was replayed. Empty for a
    /// Welch derivation (a pre-v3 snapshot, or a pair with no recorded
    /// ladder; #221), which has one resolution across the axis.
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
    /// lifetime and cannot shift frame to frame. Empty for a Welch
    /// derivation, which has no ladder (#221).
    pub stages: Vec<MtwStage>,
    /// Which estimator produced these arrays (#221). Decides
    /// [`TransferScene::estimator_readout`].
    pub estimator: Estimator,
    /// The fault indicator's frame-derived inputs (#228). `None` disables
    /// the indicator: a snapshot derivation has no live drive or lock state
    /// to report, and neither does a daemon predating the field.
    pub fault: Option<FaultFrame>,
    /// The session check's verdict on the applied voltage scale (#466).
    /// `None` for a snapshot derivation and for a frame with nothing scaled.
    pub calibration: Option<CalibrationReadout>,
}

/// The ladder columns **as the display uses them**: present only when
/// [`MtwColumns::lengths_agree`], because a mismatched frame draws nothing.
///
/// One function rather than the same `filter` at each call site. The
/// selection is an invariant shared across modules, not a local
/// convenience: [`crate::fault::FaultFrame::settled`] means "there are
/// columns on screen", and it can only mean that while it and
/// [`TransferInput::from_wire_frame`] make the identical choice. Duplicating
/// the filter let the two drift apart with nothing failing.
///
/// A display-selection rule, so it lives here beside its callers rather than
/// on the shared wire type (#112).
pub fn displayed_mtw(frame: &TransferFrame) -> Option<&MtwColumns> {
    frame.mtw.as_ref().filter(|m| m.lengths_agree())
}

/// A ladder's columns as the transfer input's parallel arrays, plus its
/// stage description. Shared by the live and the replayed-snapshot adapters
/// (#221), so the two draw the same columns the same way.
struct LadderArrays {
    freqs: Vec<f64>,
    magnitude_db: Vec<f64>,
    phase_deg: Vec<f64>,
    coherence: Vec<f64>,
    column_df: Vec<f64>,
    column_window_s: Vec<f64>,
    column_n: Vec<f64>,
    column_bins: Vec<usize>,
    stages: Vec<MtwStage>,
}

impl LadderArrays {
    /// `None` input (no columns, or columns whose lengths disagree) yields
    /// empty arrays and no stages: a mismatched ladder draws nothing, and
    /// labelling the resolution of a curve that is not on screen would
    /// describe a measurement the operator cannot see.
    fn from_columns(mtw: Option<&MtwColumns>) -> LadderArrays {
        match mtw.filter(|m| m.lengths_agree()) {
            Some(m) => LadderArrays {
                freqs: m.freqs.clone(),
                magnitude_db: m.magnitude_db.clone(),
                phase_deg: m.phase_deg.clone(),
                coherence: m.coherence.clone(),
                column_df: m.df.clone(),
                column_window_s: m.window_s.clone(),
                // An integer on the wire; the display's per-column inputs
                // are uniformly `f64`.
                column_n: m.n.iter().map(|&n| n as f64).collect(),
                column_bins: m.bins.clone(),
                stages: m.stages.clone(),
            },
            None => LadderArrays {
                freqs: Vec::new(),
                magnitude_db: Vec::new(),
                phase_deg: Vec::new(),
                coherence: Vec::new(),
                column_df: Vec::new(),
                column_window_s: Vec::new(),
                column_n: Vec::new(),
                column_bins: Vec::new(),
                stages: Vec::new(),
            },
        }
    }
}

impl TransferScene {
    /// Fill [`Self::protection_readout`] from a live frame's protection
    /// state (#670).
    pub fn set_protection(&mut self, p: Option<&ac_core::wire::WireProtection>) {
        let Some(p) = p else {
            self.protection_readout = None;
            return;
        };
        let mut parts = Vec::new();
        if p.reference_absent {
            parts.push("paused: no reference".to_string());
        }
        if p.clipped_buffers > 0 {
            parts.push(format!("{} clipped buffers dropped", p.clipped_buffers));
        }
        if p.held_columns > 0 {
            parts.push(format!("{} columns held (weak reference)", p.held_columns));
        }
        self.protection_readout = (!parts.is_empty()).then(|| parts.join(" \u{b7} "));
    }
}

impl TransferInput {
    /// Adapt a live `transfer_stream` frame. `phase_deg` is carried
    /// through as-is — it is already session-compensated (see the module
    /// doc), and `delay_ms` is τ_sess for this session.
    pub fn from_wire_frame(frame: &TransferFrame) -> TransferInput {
        // The three-stage columns are the display's source. The frame still
        // carries the full-rate Welch arrays, and this deliberately does not
        // read them: they are a different measurement (1 Hz flat, sliding
        // re-segmentation, uniform density with interpolation below 69 Hz),
        // and falling back to them when the ladder is not yet warm would
        // change the display's resolution and settling mid-session without
        // saying so. No trace is the honest state for the ~2.56 s the bottom
        // rung takes to settle; the meters and delay readout stay live
        // throughout, which is what gain staging needs.
        //
        // The stages are carried from the same filtered `mtw`
        // (`displayed_mtw`, the selection `FaultFrame::settled` shares).
        let LadderArrays {
            freqs,
            magnitude_db,
            phase_deg,
            coherence,
            column_df,
            column_window_s,
            column_n,
            column_bins,
            stages,
        } = LadderArrays::from_columns(displayed_mtw(frame));
        TransferInput {
            freqs,
            magnitude_db,
            phase_deg,
            coherence,
            delay_ms: frame.delay_ms,
            delay_locked: frame.delay_locked,
            delay_control: DelayControl::from_wire_frame(frame),
            meas_channel: frame.meas_channel,
            ref_channel: frame.ref_channel,
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
            // A live frame is the ladder by definition; while it warms it
            // draws nothing rather than falling back to Welch.
            estimator: Estimator::Ladder,
            fault: FaultFrame::from_wire_frame(frame),
            calibration: CalibrationReadout::from_cal_tags(frame.cal_tags.as_ref()),
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
    ///
    /// When the derivation replayed the live ladder (`d.mtw`, #221) the
    /// trace is the ladder's columns, drawn exactly as a live frame's, with
    /// its per-column provenance and stages. Otherwise — a pre-v3 file, a
    /// pair with no recorded ladder, a sub-window derivation — it is the
    /// Welch H₁ arrays, tagged with the segment length the derivation used,
    /// and the scene states that it is not the live ladder.
    /// The trace as CSV (#256, `C`): a few `#` header lines naming what it
    /// is, then `freq_hz,magnitude_db,phase_deg,coherence`, one row per
    /// column. Unsmoothed, as measured: smoothing is a display choice.
    /// Phase is as drawn with the trace's own delay removed (the `# delay_ms`
    /// line), wrapped to ±180°.
    pub fn to_csv(&self, name: &str) -> String {
        let mut out = String::new();
        out.push_str(&format!("# ac transfer trace: {name}\n"));
        out.push_str(&format!("# channel: {}\n", self.channel_role));
        out.push_str(&format!("# sample_rate_hz: {}\n", self.sr));
        out.push_str(&format!("# delay_ms: {:.6}\n", self.delay_ms));
        out.push_str(
            "# phase_deg: measured, with delay_ms removed; not the display's de-rotation mode\n",
        );
        out.push_str("freq_hz,magnitude_db,phase_deg,coherence\n");
        for i in 0..self.freqs.len() {
            let get = |v: &[f64]| v.get(i).copied().unwrap_or(f64::NAN);
            out.push_str(&format!(
                "{:.4},{:.4},{:.3},{:.5}\n",
                self.freqs[i],
                get(&self.magnitude_db),
                get(&self.phase_deg),
                get(&self.coherence),
            ));
        }
        out
    }

    /// Move a stored run's delay by `samples` (#256, `←`/`→` on a slot):
    /// the delay a derivation removes is a pure phase term
    /// (`exp(+j·2π·f·D/sr)` on `Gxy`), so changing it by Δ rotates the
    /// phase by `360·f·Δ/sr` degrees and leaves magnitude and coherence
    /// alone. Exact for the Welch estimate; for replayed ladder columns it
    /// is the same phase, without re-running the ladder's alignment. The
    /// delay readout follows (`delay_ms`).
    pub fn shift_delay(&mut self, samples: i64) {
        if samples == 0 || self.sr == 0 {
            return;
        }
        let dt = samples as f64 / self.sr as f64;
        for (phi, f) in self.phase_deg.iter_mut().zip(&self.freqs) {
            *phi = wrap_deg(*phi + 360.0 * f * dt);
        }
        self.delay_ms += dt * 1000.0;
    }

    pub fn from_pair_derivation(d: &PairDerivation, channel_role: &str, sr: u32) -> TransferInput {
        let ladder = d.mtw.as_ref().filter(|m| m.lengths_agree());
        let (arrays, estimator) = match ladder {
            Some(m) => (LadderArrays::from_columns(Some(m)), Estimator::Ladder),
            None => (
                // A Welch derivation has one resolution and one settling
                // time across the whole axis: no per-column provenance and
                // no ladder, so no per-band labels — three labels claiming
                // otherwise would misdescribe the trace.
                LadderArrays {
                    freqs: d.h1.freqs.clone(),
                    magnitude_db: d.h1.magnitude_db.clone(),
                    phase_deg: d.h1.phase_deg.clone(),
                    coherence: d.h1.coherence.clone(),
                    column_df: Vec::new(),
                    column_window_s: Vec::new(),
                    column_n: Vec::new(),
                    column_bins: Vec::new(),
                    stages: Vec::new(),
                },
                Estimator::Welch {
                    nperseg: d.welch_nperseg,
                },
            ),
        };
        let LadderArrays {
            freqs,
            magnitude_db,
            phase_deg,
            coherence,
            column_df,
            column_window_s,
            column_n,
            column_bins,
            stages,
        } = arrays;
        TransferInput {
            freqs,
            magnitude_db,
            phase_deg,
            coherence,
            delay_ms: d.h1.delay_ms,
            // A `PairDerivation` records no lock verdict — `derive_pair`
            // takes a `delay_samples` it is handed and asks no questions, so
            // a snapshot of a session that never locked carries the same
            // `0` as one that did. `None` is the honest value: an offline
            // derivation states the delay it was built with and makes no
            // claim about whether it was a measured lock.
            delay_locked: None,
            delay_control: None,
            // `PairDerivation` carries no wire channel identity — a
            // `channel_role` label is all the caller has (see above). `-1`
            // is never a real channel number.
            meas_channel: -1,
            ref_channel: -1,
            meas_peak_dbfs: None,
            ref_peak_dbfs: None,
            channel_role: channel_role.to_string(),
            source: Source::Snapshot,
            sr,
            column_df,
            column_window_s,
            column_n,
            column_bins,
            stages,
            estimator,
            // A snapshot is a static capture. There is no drive to observe
            // and no lock being maintained, so there is nothing for the
            // indicator to say — the same reason its meters are `None`.
            fault: None,
            // A snapshot replay carries no live session check to report.
            calibration: None,
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
        // by design (TransferFrame is `#[serde(default)]`-lenient), so this
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
                        .map(|&c| c >= modes.coherence_mask)
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
                split_on_mask(&input.coherence, modes.coherence_mask, mag_points),
                split_on_mask(&input.coherence, modes.coherence_mask, phase_points),
            )
        } else {
            (Vec::new(), Vec::new())
        };

        let delay = format_delay_readout(input.delay_ms);

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
            delay_control_readout: input.delay_control.map(|c| c.readout(input.sr)),
            delay_samples: input.delay_control.map(|c| c.samples),
            delay_insert_samples: input.delay_control.and_then(|c| c.insert_samples()),
            smoothing_readout: modes.smoothing.label(),
            coherence_mask_readout: (modes.coherence_mask != COHERENCE_THRESHOLD)
                .then(|| format!("coherence mask {:.2}", modes.coherence_mask)),
            protection_readout: None,
            calibration_readout: input.calibration.clone(),
            // Derived from the ladder alone, never from the frame's columns:
            // the same session yields the same labels on every frame, so
            // they sit still while the curve moves.
            band_labels: band_labels(&input.stages, f_min, f_max),
            estimator_readout: match input.estimator {
                Estimator::Ladder => None,
                Estimator::Welch { nperseg } => Some(format_estimator_readout(input.sr, nperseg)),
            },
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
fn split_on_mask(
    coherence: &[f64],
    mask: f64,
    point: impl Fn(usize) -> (f64, f64),
) -> Vec<Vec<(f64, f64)>> {
    let mut segments: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut current: Vec<(f64, f64)> = Vec::new();

    for (i, &c) in coherence.iter().enumerate() {
        if c < mask {
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
    // Session-check readout (#466): one test per scene-rule branch
    // ---------------------------------------------------------------

    fn verdict(state: &str, checked_at: &str) -> serde_json::Value {
        serde_json::json!({
            "state": state, "measured": -0.61, "stored": -0.60, "delta": -0.01,
            "tolerance": 0.10, "unit": "dB", "stored_at": "2026-09-15T23:43:04Z",
            "checked_at": checked_at, "source": "probe",
        })
    }

    fn readout(tags: serde_json::Value) -> Option<CalibrationReadout> {
        CalibrationReadout::from_cal_tags(Some(&tags))
    }

    #[test]
    fn calibration_readout_absent_without_cal_tags() {
        assert_eq!(CalibrationReadout::from_cal_tags(None), None);
        assert_eq!(
            CalibrationReadout::from_cal_tags(Some(&serde_json::Value::Null)),
            None
        );
    }

    #[test]
    fn calibration_readout_absent_when_nothing_is_scaled() {
        assert_eq!(
            readout(serde_json::json!({
                "meas": {"voltage": "none", "spl": "on"},
                "ref": {"voltage": "none"},
            })),
            None
        );
    }

    #[test]
    fn calibration_readout_verified_carries_its_check_time() {
        let r = readout(serde_json::json!({
            "meas": {"voltage": "on", "voltage_check": verdict("verified", "2026-09-16T14:02:11.345Z")},
        }))
        .unwrap();
        assert_eq!(r.state, CalibrationState::Verified);
        assert_eq!(r.text, "voltage verified 2026-09-16T14:02:11Z");
    }

    #[test]
    fn calibration_readout_refused_names_the_unit_on_screen() {
        let r = readout(serde_json::json!({
            "meas": {"voltage": "none", "voltage_check": verdict("refused", "x")},
            "ref": {"voltage": "on", "voltage_check": verdict("verified", "x")},
        }))
        .unwrap();
        assert_eq!(r.state, CalibrationState::Refused);
        assert_eq!(r.text, "voltage refused \u{2014} dBFS");
    }

    #[test]
    fn calibration_readout_unverified_forms() {
        let unverified = serde_json::json!({"state": "unverified", "cause": "no_loopback",
            "reason": "no reference loopback configured"});
        for tags in [
            serde_json::json!({"meas": {"voltage": "on", "voltage_check": unverified}}),
            // An applied scale with no verdict (an older daemon).
            serde_json::json!({"meas": {"voltage": "on"}}),
            // A verdict this build cannot parse never reads as verified.
            serde_json::json!({"meas": {"voltage": "on", "voltage_check": {"state": "verified"}}}),
            serde_json::json!({"meas": {"voltage": "on", "voltage_check": "garbage"}}),
            // One unverified leg outweighs a verified one.
            serde_json::json!({
                "meas": {"voltage": "on", "voltage_check": verdict("verified", "x")},
                "ref": {"voltage": "on"},
            }),
        ] {
            let r = readout(tags.clone()).unwrap_or_else(|| panic!("{tags}"));
            assert_eq!(r.state, CalibrationState::Unverified, "{tags}");
            assert_eq!(r.text, "voltage unverified");
        }
    }

    /// A malformed `cal_tags` never drops the frame.
    #[test]
    fn a_malformed_cal_tags_still_parses_the_frame() {
        let frame = serde_json::json!({
            "sr": 48000, "meas_channel": 0, "ref_channel": 1,
            "spec_freqs": [], "meas_spectrum": [], "ref_spectrum": [],
            "spl": null, "spl_weighting": "Z", "spl_integration": "fast",
            "cal_tags": {"meas": {"voltage": 7, "voltage_check": [1]}},
        });
        let wire: TransferFrame = serde_json::from_value(frame).expect("frame still parses");
        let input = TransferInput::from_wire_frame(&wire);
        assert_eq!(
            input.calibration.map(|c| c.state),
            Some(CalibrationState::Unverified)
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
                f_top: s.f_top,
                blend_top: s.blend_top,
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
                delay_control: None,
                meas_channel: 0,
                ref_channel: 1,
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
                estimator: Estimator::Ladder,
                fault: None,
                calibration: None,
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
    // ─── delay readout — ms only (#391) ─────────────────────────────────

    // ─── operator delay control (#669) ─────────────────────────────────

    #[test]
    fn delay_control_reads_source_and_residual() {
        let c = DelayControl {
            samples: 470,
            residual: Some(12),
            operator: true,
        };
        assert_eq!(c.readout(48_000), "set · find +12 smp (+0.25 ms)");
        assert_eq!(c.insert_samples(), Some(482));
        let found = DelayControl {
            operator: false,
            residual: Some(-3),
            ..c
        };
        assert_eq!(found.readout(48_000), "found · find -3 smp (-0.06 ms)");
        let none = DelayControl {
            residual: None,
            ..c
        };
        assert_eq!(none.readout(48_000), "set · find —");
        assert_eq!(none.insert_samples(), None);
    }

    /// Nothing to insert or nudge on a pair without a delay.
    #[test]
    fn delay_control_is_absent_without_a_held_delay() {
        let mut f: TransferFrame = serde_json::from_str(
            r#"{"type":"transfer_stream","delay_samples":400,"delay_residual":0,
                "delay_operator":false,"delay_locked":false,
                "meas_channel":0,"ref_channel":1,"sr":48000,
                "spec_freqs":[],"meas_spectrum":[],"ref_spectrum":[],"spl":null,
                "spl_weighting":"Z","spl_integration":"fast"}"#,
        )
        .unwrap();
        assert_eq!(DelayControl::from_wire_frame(&f), None);
        f.delay_locked = Some(true);
        assert_eq!(
            DelayControl::from_wire_frame(&f),
            Some(DelayControl {
                samples: 400,
                residual: Some(0),
                operator: false
            })
        );
    }

    #[test]
    fn protection_readout_says_what_is_held_back() {
        let frame: TransferFrame = serde_json::from_value(serde_json::json!({
            "type": "transfer_stream", "meas_channel": 0, "ref_channel": 1, "sr": 48000,
            "spec_freqs": [], "meas_spectrum": [], "ref_spectrum": [], "spl": null,
            "spl_weighting": "Z", "spl_integration": "fast"
        }))
        .unwrap();
        let mut scene = TransferScene::from_input(
            &TransferInput::from_wire_frame(&frame),
            DisplayModes::default(),
            (20.0, 20_000.0),
            (-40.0, 20.0),
            &mut (MeterState::default(), MeterState::default()),
            &mut FaultState::default(),
            0.0,
        );
        scene.set_protection(Some(&ac_core::wire::WireProtection {
            clipped_buffers: 3,
            reference_absent: true,
            held_columns: 8,
        }));
        assert_eq!(
            scene.protection_readout.as_deref(),
            Some("paused: no reference \u{b7} 3 clipped buffers dropped \u{b7} 8 columns held (weak reference)")
        );
        scene.set_protection(Some(&Default::default()));
        assert_eq!(scene.protection_readout, None);
    }

    /// The operator's mask moves the drawing, not the fault rule (#670):
    /// columns at 0.6 coherence vanish under a 0.7 mask, and CHECK ROUTING
    /// (fixed 0.5) stays dark.
    #[test]
    fn the_display_mask_does_not_move_the_fault_rule() {
        let n = 20;
        let freqs: Vec<f64> = (0..n).map(|i| 100.0 * 1.3f64.powi(i)).collect();
        let frame: TransferFrame = serde_json::from_value(serde_json::json!({
            "type": "transfer_stream", "meas_channel": 0, "ref_channel": 1, "sr": 48000,
            "spec_freqs": [], "meas_spectrum": [], "ref_spectrum": [], "spl": null,
            "spl_weighting": "Z", "spl_integration": "fast",
            "meas_peak_dbfs": -20.0, "ref_peak_dbfs": -20.0,
            "delay_locked": true,
            "drive": {"on": true, "level_dbfs": -30.0, "drivable": true},
            "mtw": {"freqs": freqs, "magnitude_db": vec![0.0; n as usize],
                    "phase_deg": vec![0.0; n as usize], "coherence": vec![0.6; n as usize]}
        }))
        .unwrap();
        let input = TransferInput::from_wire_frame(&frame);
        let build = |mask: f64| {
            TransferScene::from_input(
                &input,
                DisplayModes::new(DerotMode::Session, Smoothing::Off).with_coherence_mask(mask),
                (20.0, 20_000.0),
                (-40.0, 20.0),
                &mut (MeterState::default(), MeterState::default()),
                &mut FaultState::default(),
                0.0,
            )
        };
        let drawn = build(0.5);
        let masked = build(0.7);
        assert!(
            !drawn.magnitude.segments.is_empty(),
            "test setup: drawn at 0.5"
        );
        assert!(
            masked.magnitude.segments.is_empty(),
            "0.7 mask left columns at 0.6"
        );
        assert_eq!(masked.fault, None, "the display mask moved the fault rule");
        assert_eq!(
            masked.coherence_mask_readout.as_deref(),
            Some("coherence mask 0.70")
        );
        assert_eq!(drawn.coherence_mask_readout, None);
    }

    #[test]
    fn csv_has_a_header_and_one_row_per_column() {
        let input = TransferInput {
            freqs: vec![100.0, 1000.0],
            magnitude_db: vec![-1.5, 0.25],
            phase_deg: vec![10.0, -170.0],
            coherence: vec![0.9, 0.99],
            delay_ms: 3.3958,
            delay_locked: Some(true),
            delay_control: None,
            meas_channel: 0,
            ref_channel: 1,
            meas_peak_dbfs: None,
            ref_peak_dbfs: None,
            channel_role: "meas_0".into(),
            source: crate::scene::Source::Live,
            sr: 96_000,
            column_df: Vec::new(),
            column_window_s: Vec::new(),
            column_n: Vec::new(),
            column_bins: Vec::new(),
            stages: Vec::new(),
            estimator: Estimator::Ladder,
            fault: None,
            calibration: None,
        };
        let csv = input.to_csv("slot 3");
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], "# ac transfer trace: slot 3");
        assert_eq!(lines[3], "# delay_ms: 3.395800");
        assert!(lines[4].starts_with("# phase_deg: measured, with delay_ms removed"));
        assert_eq!(lines[5], "freq_hz,magnitude_db,phase_deg,coherence");
        assert_eq!(lines[6], "100.0000,-1.5000,10.000,0.90000");
        assert_eq!(lines[7], "1000.0000,0.2500,-170.000,0.99000");
        assert_eq!(lines.len(), 8);
    }

    /// `shift_delay` against the rejected route (a fresh derivation at the
    /// shifted delay): same phase, same magnitude, readout moved.
    #[test]
    fn shift_delay_matches_a_derivation_at_the_shifted_delay() {
        use ac_core::visualize::pair_derivation::derive_pair;
        use ac_core::visualize::weighting_curves::WeightingCurve;
        let sr = 48_000u32;
        let n = 3 * sr as usize;
        let mut x = 1u32;
        let sig: Vec<f32> = (0..n)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (x >> 8) as f32 / (1 << 23) as f32 - 1.0
            })
            .collect();
        let meas: Vec<f32> = (0..n)
            .map(|i| if i >= 100 { sig[i - 100] } else { 0.0 })
            .collect();
        let at = |d| derive_pair(&sig, &meas, sr, d, None, None, WeightingCurve::Z);
        let mut shifted = TransferInput::from_pair_derivation(&at(100), "m", sr);
        shifted.shift_delay(7);
        let direct = TransferInput::from_pair_derivation(&at(107), "m", sr);
        assert!((shifted.delay_ms - direct.delay_ms).abs() < 1e-9);
        for (i, (a, b)) in shifted.phase_deg.iter().zip(&direct.phase_deg).enumerate() {
            let d = wrap_deg(a - b).abs();
            assert!(d < 1e-6, "bin {i}: {a} vs {b}");
        }
        for (a, b) in shifted.magnitude_db.iter().zip(&direct.magnitude_db) {
            assert!((a - b).abs() < 1e-9, "magnitude moved: {a} vs {b}");
        }
    }

    #[test]
    fn delay_readout_is_ms_only_regardless_of_lock_state() {
        // #391 removed the ms → m conversion entirely — the readout is
        // just the frame's own `delay_ms`, unconditioned on lock, sign, or
        // any stored calibration (there is none left to store).
        assert_eq!(format_delay_readout(2.5).delay_readout, "2.50 ms");
        assert_eq!(format_delay_readout(0.0).delay_readout, "0.00 ms");
        assert_eq!(format_delay_readout(-0.5).delay_readout, "-0.50 ms");
        assert_eq!(format_delay_readout(f64::NAN).delay_readout, "NaN ms");
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
            delay_control: None,
            meas_channel: 0,
            ref_channel: 1,
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
            estimator: Estimator::Ladder,
            fault: None,
            calibration: None,
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
    // keypress-adjacent, and TransferFrame parses partial frames by design.
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
            delay_control: None,
            meas_channel: 0,
            ref_channel: 1,
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
            estimator: Estimator::Ladder,
            fault: None,
            calibration: None,
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
        let seg = split_on_mask(&coherence, COHERENCE_THRESHOLD, |i| (i as f64, 0.0));
        // One interior segment of the two live columns; no leading or
        // trailing empty segment.
        assert_eq!(seg.len(), 1);
        assert_eq!(seg[0].len(), 2);
        assert_eq!(seg[0][0].0, 1.0);
        assert_eq!(seg[0][1].0, 2.0);
    }

    // ---- #221: which estimator a trace came from ----

    fn scene_of(inp: &TransferInput) -> TransferScene {
        let mut meters = (MeterState::default(), MeterState::default());
        TransferScene::from_input(
            inp,
            DisplayModes::new(DerotMode::Session, Smoothing::Off),
            (20.0, 20_000.0),
            (-80.0, 20.0),
            &mut meters,
            &mut FaultState::default(),
            0.0,
        )
    }

    /// A Welch derivation of `sr` Hz audio, as `derive_pair` returns it for
    /// a file with no recorded ladder.
    fn welch_derivation(sr: u32) -> PairDerivation {
        let sig: Vec<f32> = (0..sr as usize * 2)
            .map(|i| ((i as f64 * 0.37).sin() * 0.3) as f32)
            .collect();
        ac_core::visualize::pair_derivation::derive_pair(
            &sig,
            &sig,
            sr,
            0,
            None,
            None,
            ac_core::visualize::weighting_curves::WeightingCurve::Z,
        )
    }

    /// Three ladder columns at 48 kHz, lengths agreeing, with the stages.
    fn ladder_columns() -> MtwColumns {
        MtwColumns {
            freqs: vec![100.0, 1_000.0, 10_000.0],
            f_lo: vec![95.0, 950.0, 9_500.0],
            f_hi: vec![105.0, 1_050.0, 10_500.0],
            magnitude_db: vec![-6.0; 3],
            phase_deg: vec![0.0; 3],
            coherence: vec![0.9; 3],
            df: vec![0.98, 2.93, 11.7],
            window_s: vec![1.02, 0.34, 0.085],
            n: vec![4; 3],
            stage: vec![2, 1, 0],
            blend: vec![0.0; 3],
            bins: vec![1; 3],
            ppo: 48.0,
            n_blocks: 4,
            settled_stages: vec![true; 3],
            stages: wire_stages(48_000),
        }
    }

    /// ux's test bullet: the statement is present and verbatim on a Welch
    /// snapshot scene and absent on a live one. Asserting the exact `Some`
    /// fails on a dropped field as well as a misspelt one.
    #[test]
    fn a_welch_snapshot_states_it_is_not_the_live_ladder_and_a_live_frame_does_not() {
        let d = welch_derivation(48_000);
        assert!(d.mtw.is_none());
        let inp = TransferInput::from_pair_derivation(&d, "meas_0", 48_000);
        assert_eq!(inp.estimator, Estimator::Welch { nperseg: 48_000 });
        assert_eq!(
            scene_of(&inp).estimator_readout.as_deref(),
            Some("H\u{2081} Welch 1.00 Hz flat \u{2014} not the live ladder")
        );

        let frame: TransferFrame = serde_json::from_value(serde_json::json!({
            "type": "transfer_stream",
            "cmd": "transfer_stream",
            "sr": 48_000,
            "meas_channel": 0,
            "ref_channel": 1,
            "spec_freqs": [],
            "meas_spectrum": [],
            "ref_spectrum": [],
            "spl": null,
            "spl_weighting": "Z",
            "spl_integration": "fast",
            "mtw": ladder_columns(),
        }))
        .expect("minimal live frame");
        let live = TransferInput::from_wire_frame(&frame);
        assert_eq!(live.estimator, Estimator::Ladder);
        assert_eq!(scene_of(&live).estimator_readout, None);
    }

    /// The figure is `sr / nperseg` from the derivation, not an assumed 1 Hz.
    #[test]
    fn the_readout_figure_is_the_segment_the_derivation_used() {
        assert_eq!(
            format_estimator_readout(48_000, 96_000),
            "H\u{2081} Welch 0.50 Hz flat \u{2014} not the live ladder"
        );
        assert_eq!(
            format_estimator_readout(96_000, 4_096),
            "H\u{2081} Welch 23.4 Hz flat \u{2014} not the live ladder"
        );
        let mut d = welch_derivation(48_000);
        d.welch_nperseg = 24_000;
        let inp = TransferInput::from_pair_derivation(&d, "meas_0", 48_000);
        assert_eq!(
            scene_of(&inp).estimator_readout.as_deref(),
            Some("H\u{2081} Welch 2.00 Hz flat \u{2014} not the live ladder")
        );
    }

    /// A snapshot whose ladder was replayed draws the ladder: its columns,
    /// their provenance and the stages, and no "not the live ladder"
    /// statement — the estimator decides, not the source.
    #[test]
    fn a_replayed_snapshot_draws_the_ladder_with_no_statement() {
        let mut d = welch_derivation(48_000);
        d.mtw = Some(ladder_columns());
        let inp = TransferInput::from_pair_derivation(&d, "meas_0", 48_000);
        assert_eq!(inp.source, Source::Snapshot);
        assert_eq!(inp.estimator, Estimator::Ladder);
        assert_eq!(inp.freqs, ladder_columns().freqs);
        assert!(!inp.stages.is_empty());
        assert!(!inp.column_df.is_empty() && !inp.column_window_s.is_empty());
        assert!(!inp.column_n.is_empty() && !inp.column_bins.is_empty());
        let scene = scene_of(&inp);
        assert_eq!(scene.estimator_readout, None);
        assert!(!scene.band_labels.is_empty());

        // Columns whose lengths disagree draw nothing as a ladder; the
        // derivation falls back to its Welch arrays and says so.
        let mut bad = ladder_columns();
        bad.coherence.pop();
        d.mtw = Some(bad);
        let inp = TransferInput::from_pair_derivation(&d, "meas_0", 48_000);
        assert_eq!(inp.estimator, Estimator::Welch { nperseg: 48_000 });
        assert!(scene_of(&inp).estimator_readout.is_some());
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
            mtw: None,
            welch_nperseg: ac_core::visualize::transfer::h1_nperseg(sr),
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
