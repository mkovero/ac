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

use crate::fault::{Fault, FaultFrame, FaultInput, FaultState};
use crate::scene::{Provenance, Source, Trace};
use crate::ticks::{db_to_y, freq_to_x, phase_to_y};
use crate::wire::WireFrame;

/// Columns below this coherence are masked out of both panes (D5 —
/// fixed threshold, no tuning UI).
pub const COHERENCE_THRESHOLD: f64 = 0.5;

/// Speed of sound used for the delay readout's metres conversion —
/// exactly 343 m/s (D2), not a temperature-dependent estimate.
pub const SPEED_OF_SOUND_M_S: f64 = 343.0;

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
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DerotMode {
    /// This session's delay. τ_derot = 0 — the wire already is this.
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

/// `"{delay_ms:.2} ms  ({metres:.2} m)"`, c = 343 m/s exactly (D2).
pub fn format_delay_readout(delay_ms: f64) -> String {
    let metres = delay_ms * SPEED_OF_SOUND_M_S / 1000.0;
    format!("{delay_ms:.2} ms  ({metres:.2} m)")
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
    /// `"2.50 ms  (0.86 m)"`.
    pub delay_readout: String,
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
        TransferInput {
            freqs,
            magnitude_db,
            phase_deg,
            coherence,
            delay_ms: frame.delay_ms,
            meas_peak_dbfs: frame.meas_peak_dbfs,
            ref_peak_dbfs: frame.ref_peak_dbfs,
            channel_role: format!("meas_{}", frame.meas_channel),
            source: Source::Live,
            sr: frame.sr,
            column_df,
            column_window_s,
            column_n,
            column_bins,
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
        derot: DerotMode,
        freq_range: (f64, f64),
        db_range: (f64, f64),
        meters: &mut (MeterState, MeterState),
        fault: &mut FaultState,
        now_s: f64,
    ) -> TransferScene {
        let (f_min, f_max) = freq_range;
        let (db_min, db_max) = db_range;
        let tau = derot.tau_derot_ms(input.delay_ms);

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
            let mag_points = |i: usize| {
                // db_to_y is the crate's one dB→y mapping — do not
                // re-implement it, and do not clamp: an over-range
                // magnitude runs off-canvas (the viewport clips it), which
                // is honest. Pinning it to the pane border would fabricate
                // a value at exactly the overload moment the display must
                // not lie about.
                (
                    freq_to_x(input.freqs[i], f_min, f_max),
                    db_to_y(input.magnitude_db[i], db_min, db_max),
                )
            };
            let phase_points = |i: usize| {
                let phi = derotate_deg(input.phase_deg[i], input.freqs[i], tau);
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
            delay_readout: format_delay_readout(input.delay_ms),
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

    #[test]
    fn delay_readout_uses_343_metres_per_second_exactly() {
        assert_eq!(format_delay_readout(2.5), "2.50 ms  (0.86 m)");
        assert_eq!(format_delay_readout(0.0), "0.00 ms  (0.00 m)");
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
            fault: None,
        };
        let mut meters = (MeterState::default(), MeterState::default());
        let s = TransferScene::from_input(
            &inp,
            DerotMode::Session,
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
            fault: None,
        };
        let mut meters = (MeterState::default(), MeterState::default());
        let s = TransferScene::from_input(
            &inp,
            DerotMode::Session,
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
