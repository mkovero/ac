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

use crate::scene::{Provenance, Source, Trace};
use crate::ticks::freq_to_x;

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
    /// `"2.50 ms  (0.86 m)"`.
    pub delay_readout: String,
    pub meas_meter: Meter,
    pub ref_meter: Meter,
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
}

impl TransferScene {
    /// Build the scene. `derot` selects the phase mode; `meters` carries
    /// the cross-frame hold/latch state; `now_s` is scene time.
    pub fn from_input(
        input: &TransferInput,
        derot: DerotMode,
        freq_range: (f64, f64),
        db_range: (f64, f64),
        meters: &mut (MeterState, MeterState),
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

        let mag_points = |i: usize| {
            (
                freq_to_x(input.freqs[i], f_min, f_max),
                ((input.magnitude_db[i] - db_min) / (db_max - db_min)).clamp(0.0, 1.0),
            )
        };
        let phase_points = |i: usize| {
            let phi = derotate_deg(input.phase_deg[i], input.freqs[i], tau);
            (
                freq_to_x(input.freqs[i], f_min, f_max),
                (phi + 180.0) / 360.0,
            )
        };

        TransferScene {
            magnitude: Trace {
                segments: split_on_mask(&input.coherence, mag_points),
                provenance: provenance.clone(),
            },
            phase: Trace {
                segments: split_on_mask(&input.coherence, phase_points),
                provenance,
            },
            delay_readout: format_delay_readout(input.delay_ms),
            meas_meter: meters.0.update(input.meas_peak_dbfs, now_s),
            ref_meter: meters.1.update(input.ref_peak_dbfs, now_s),
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
}
