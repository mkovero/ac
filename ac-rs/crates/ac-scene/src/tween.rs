//! Motion easing for the live transfer trace (#716): a new estimate is
//! approached over [`TWEEN_S`] rather than jumped to.
//!
//! The daemon publishes every ~50 ms and the view paints at the display's
//! rate, so each new estimate arrives as a step that lingers several screen
//! frames. Easing the drawn curve toward it over less than one publish
//! interval turns the steps into motion without inventing data:
//!
//! - Every eased value lies between two estimates the daemon actually
//!   made, and the curve reaches the newest one [`TWEEN_S`] after it
//!   arrives — before the next is due. It never runs ahead of a frame.
//! - Only magnitude and phase are eased. Coherence (and so the mask) is the
//!   latest estimate's: a column is gapped or drawn as the measurement says.
//! - Phase moves along the shorter arc, so a curve near ±180° does not
//!   sweep the whole pane.
//! - A change that makes two estimates incomparable — a different column
//!   set, delay or ladder — snaps straight to the new one.
//! - Readouts, exports and stored runs never pass through here; they read
//!   the frames themselves.
//!
//! Astra (gpt-6-astra) discussion, 2026-09-28: "enable modest animation
//! only after improving estimator cadence … reset animation across
//! discontinuities". The IR panel is not eased: easing a moving impulse
//! draws two fading peaks, not one arrival moving.

use crate::transfer::TransferInput;

/// Seconds a new estimate takes to be reached: under the ~50 ms publish
/// interval, so the curve is always at the newest estimate before the next.
pub const TWEEN_S: f64 = 0.04;

/// What the drawing about to use the eased curve needs from it.
#[derive(Debug, Clone, Copy)]
pub struct TweenOptions {
    /// Ease phase too. Only for the wrapped phase view: unwrapped phase and
    /// group delay are built across columns, and columns eased
    /// independently can straddle a wrap mid-way — a ~50 ms group-delay
    /// spike between two estimates that both read under 1 ms (Codex
    /// review). Those views take the new phase at once.
    pub ease_phase: bool,
    /// The coherence mask the trace is drawn under. A column the previous
    /// estimate had masked was not on screen: it appears at its new value
    /// rather than easing in from the rejected one (Codex review).
    pub coherence_mask: f64,
}

/// One live trace's easing state.
#[derive(Debug, Default, Clone)]
pub struct Tween {
    /// Where the curve starts from — what was drawn when the target changed.
    from_mag: Vec<f64>,
    from_phase: Vec<f64>,
    /// The estimate being approached.
    to: Option<TransferInput>,
    start_s: f64,
}

impl Tween {
    /// The input to draw at `now_s`: `latest`, or on its way there from the
    /// previous estimate. Call once per paint with the newest estimate.
    pub fn sample(
        &mut self,
        latest: &TransferInput,
        now_s: f64,
        opts: TweenOptions,
    ) -> TransferInput {
        let is_new = self.to.as_ref().is_none_or(|to| {
            to.magnitude_db != latest.magnitude_db
                || to.phase_deg != latest.phase_deg
                || to.coherence != latest.coherence
        });
        if is_new {
            let comparable = self.to.as_ref().is_some_and(|to| comparable(to, latest));
            if comparable {
                // From wherever the curve is drawn now — except a column the
                // previous estimate masked, which was not drawn at all.
                let (mut mag, mut phase) = self.at(now_s);
                let prev_coh = self
                    .to
                    .as_ref()
                    .map(|t| t.coherence.clone())
                    .unwrap_or_default();
                for (i, (m, p)) in mag.iter_mut().zip(phase.iter_mut()).enumerate() {
                    let was_drawn = prev_coh.get(i).is_some_and(|&c| c >= opts.coherence_mask);
                    if !was_drawn {
                        *m = latest.magnitude_db.get(i).copied().unwrap_or(f64::NAN);
                        *p = latest.phase_deg.get(i).copied().unwrap_or(f64::NAN);
                    }
                }
                self.from_mag = mag;
                self.from_phase = phase;
            } else {
                self.from_mag = latest.magnitude_db.clone();
                self.from_phase = latest.phase_deg.clone();
            }
            self.to = Some(latest.clone());
            self.start_s = now_s;
        }
        let (mag, phase) = self.at(now_s);
        let mut out = latest.clone();
        out.magnitude_db = mag;
        if opts.ease_phase {
            out.phase_deg = phase;
        }
        out
    }

    /// Forget the state: the next sample starts at its estimate.
    pub fn reset(&mut self) {
        *self = Tween::default();
    }

    /// The eased arrays at `now_s`.
    fn at(&self, now_s: f64) -> (Vec<f64>, Vec<f64>) {
        let Some(to) = &self.to else {
            return (Vec::new(), Vec::new());
        };
        let t = ((now_s - self.start_s) / TWEEN_S).clamp(0.0, 1.0);
        let mag = self
            .from_mag
            .iter()
            .zip(&to.magnitude_db)
            .map(|(&a, &b)| lerp(a, b, t))
            .collect();
        let phase = self
            .from_phase
            .iter()
            .zip(&to.phase_deg)
            .map(|(&a, &b)| {
                if !(a.is_finite() && b.is_finite()) {
                    return b;
                }
                // Shorter arc, rewrapped to (−180, 180].
                let d = (b - a + 180.0).rem_euclid(360.0) - 180.0;
                crate::transfer::wrap_deg(a + d * t)
            })
            .collect();
        (mag, phase)
    }
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    if a.is_finite() && b.is_finite() {
        a + (b - a) * t
    } else {
        b
    }
}

/// Two estimates of the same columns under the same alignment and ladder,
/// so that easing one into the other means something.
/// A malformed estimate (arrays of different lengths) is never comparable:
/// it snaps, and the scene handles it as it would unseen.
fn comparable(a: &TransferInput, b: &TransferInput) -> bool {
    let whole = |t: &TransferInput| {
        let n = t.freqs.len();
        t.magnitude_db.len() == n && t.phase_deg.len() == n && t.coherence.len() == n
    };
    whole(a) && whole(b) && a.freqs == b.freqs && a.delay_ms == b.delay_ms && a.stages == b.stages
}
