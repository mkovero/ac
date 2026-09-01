//! Interface-latency (τ) calibration layer — issue #281 / #347.
//!
//! τ is a property of *(device, backend, sample rate, period size, port
//! pair)*, not of the electrical or acoustic calibration, so nothing in
//! this module takes a voltage or SPL field to produce a τ, and nothing
//! takes a [`TauEntry`] to produce a Vrms / dBu / dB SPL value. See the
//! parent module's "third parallel layer" doc for why that separation is
//! load-bearing; `tau_history_does_not_affect_voltage_or_spl_derivations`
//! there is its parity test.
//!
//! History is append-only and looked up by exact condition match
//! ([`Calibration::tau_for`]) — never averaged, interpolated, or degraded
//! to "closest".

use serde::{Deserialize, Serialize};

use super::Calibration;

/// Conditions τ (interface round-trip latency) was measured under. τ is a
/// property of this whole tuple, not of the interface alone — a period-size
/// change alone can move it by milliseconds — so lookup
/// ([`Calibration::tau_for`]) is exact-match on every field, never
/// nearest-neighbour or interpolated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TauConditions {
    pub device: u32,
    pub backend: String,
    pub sample_rate: u32,
    /// `None` means "not applicable to this backend" (it cannot report a
    /// period/buffer size at all), not "unknown" — see
    /// `AudioEngine::period_size`. Two runs on such a backend at different
    /// real buffer sizes will spuriously exact-match; this is a documented
    /// limitation of that backend, not new to this field.
    pub period_size: Option<u32>,
    pub output_port: String,
    pub input_port: String,
}

/// One τ measurement: the conditions it was taken under, the value, when,
/// and how. Stored in [`Calibration::tau_history`] as an append-only list —
/// entries are never overwritten or removed, so a stale value never
/// silently replaces a good one; [`Calibration::tau_for`] picks among them
/// by exact condition match.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TauEntry {
    pub conditions: TauConditions,
    pub tau_s: f64,
    /// RFC3339 timestamp of the measurement.
    pub measured_at: String,
    /// Free-text description of the method, e.g. `"farina_short_ess"`.
    pub method: String,
    /// How many independently-lifecycled readings agreed before this entry
    /// was stored (#347). `0` on any entry written before this field
    /// existed — `#[serde(default)]` so those deserialize to `0` rather
    /// than being indistinguishable from a corroborated one. A caller that
    /// writes this field must never write `1`: since #347, a lone reading
    /// is no longer a storable outcome — corroborated entries store `>= 2`.
    #[serde(default)]
    pub agreement_count: u32,
}

/// Why an exact-match τ lookup missed. Names the delta to the nearest
/// stored entry rather than silently interpolating, falling back to
/// "closest", or proceeding uncorrected — see the acceptance criteria on
/// issue #281.
#[derive(Debug, Clone, PartialEq)]
pub struct TauRefusal {
    pub requested: TauConditions,
    /// Nearest entry by fewest differing condition fields, ties broken by
    /// most recent `measured_at`. `None` when no entry exists at all for
    /// this calibration key.
    pub nearest: Option<TauEntry>,
    /// Condition field names (see [`TauConditions`]) that differ between
    /// `requested` and `nearest`, in tuple order. Empty when `nearest` is
    /// `None`.
    pub differing_fields: Vec<&'static str>,
}

impl TauRefusal {
    /// Diagnostic message naming the delta — the point of refusing instead
    /// of guessing is that a reader can see *why* in one line, without
    /// opening `cal.json` by hand.
    pub fn message(&self) -> String {
        match &self.nearest {
            None => format!(
                "no \u{3c4} history recorded for device {} / {} backend yet \u{2014} run `ac \
                 calibrate` with loopback patched to measure one",
                self.requested.device, self.requested.backend
            ),
            Some(nearest) => {
                let deltas: Vec<String> = tau_deltas(&self.requested, &nearest.conditions)
                    .into_iter()
                    .map(|d| {
                        format!(
                            "{} (requested {}, stored {})",
                            d.field, d.requested, d.stored
                        )
                    })
                    .collect();
                format!(
                    "no \u{3c4} entry for these exact conditions; nearest stored entry \
                     (measured {}) differs in {}",
                    nearest.measured_at,
                    deltas.join(", ")
                )
            }
        }
    }
}

/// One condition field that differs between a requested and a stored
/// [`TauConditions`], with both sides already rendered for a message.
struct TauFieldDelta {
    field: &'static str,
    requested: String,
    stored: String,
}

/// Condition fields that differ between `a` and `b`, in declaration order,
/// each rendered for both sides.
///
/// The field name, the equality test and the rendering all come from one
/// line per field. That is the point: the previous shape was a pair of
/// functions — one listing names, one mapping a name back to a value —
/// that had to be kept in sync by hand, with a `_ => "?"` arm that turned
/// a missed field into a `?` printed inside the very diagnostic whose job
/// is to name the delta. Adding a field to [`TauConditions`] now stops the
/// crate compiling until it is listed here, via the exhaustive destructure
/// below — a pattern with no `..` is an error when a field is unbound.
fn tau_deltas(a: &TauConditions, b: &TauConditions) -> Vec<TauFieldDelta> {
    let TauConditions {
        device: _,
        backend: _,
        sample_rate: _,
        period_size: _,
        output_port: _,
        input_port: _,
    } = a;
    macro_rules! deltas {
        ($( $field:ident => $render:expr ),* $(,)?) => {{
            let mut out: Vec<TauFieldDelta> = Vec::new();
            $(
                if a.$field != b.$field {
                    #[allow(clippy::redundant_closure_call)]
                    let render = |c: &TauConditions| {
                        let $field = &c.$field;
                        $render
                    };
                    out.push(TauFieldDelta {
                        field: stringify!($field),
                        requested: render(a),
                        stored: render(b),
                    });
                }
            )*
            out
        }};
    }
    deltas! {
        device => device.to_string(),
        backend => backend.clone(),
        sample_rate => format!("{sample_rate} Hz"),
        period_size => period_size
            .map(|p| p.to_string())
            .unwrap_or_else(|| "n/a".to_string()),
        output_port => output_port.clone(),
        input_port => input_port.clone(),
    }
}

/// Names of the condition fields that differ between `a` and `b`.
fn tau_diff_fields(a: &TauConditions, b: &TauConditions) -> Vec<&'static str> {
    tau_deltas(a, b).into_iter().map(|d| d.field).collect()
}

/// Outcome of comparing two independently-lifecycled τ readings (#347). A
/// single reading is not a measurement of τ on this stack — a
/// graph-buffering shift of exactly one period is invisible within one
/// client lifetime and stable to 0.001 frames within it, so nothing short
/// of a second, separately-lifecycled reading can catch it (see the
/// module-level "third parallel layer" doc). [`compare_tau_readings`]
/// decides whether two such readings corroborate each other.
#[derive(Debug, Clone, PartialEq)]
pub enum TauComparison {
    /// The two readings match to the whole sample
    /// (`round((b - a) * sample_rate) == 0`).
    Agree,
    /// The two readings disagree and neither may be stored.
    /// [`TauDisagreement::periods`] tells a period-shift (software, #347's
    /// own root cause) apart from any other mismatch (a different fault).
    Disagree(TauDisagreement),
}

/// The delta between two disagreeing τ readings, in whole samples, plus
/// enough of the two raw readings to name in a diagnostic message — see
/// [`TauDisagreement::message`].
#[derive(Debug, Clone, PartialEq)]
pub struct TauDisagreement {
    pub reading1_s: f64,
    pub reading2_s: f64,
    /// `round((reading2_s - reading1_s) * sample_rate)`. Always nonzero —
    /// a zero delta is [`TauComparison::Agree`], not a `TauDisagreement`.
    pub delta_samples: i64,
    pub sample_rate: u32,
    pub period_size: Option<u32>,
    /// `Some(n)` (`n != 0`) when `delta_samples` is an exact multiple of
    /// `period_size` — a graph-buffering shift, not hardware drift.
    /// `None` when it isn't, or `period_size` is unknown for this backend:
    /// a different fault class, per #347's acceptance criteria.
    pub periods: Option<i64>,
}

impl TauDisagreement {
    /// Diagnostic message naming the delta in both samples (the causal,
    /// period-quantized unit) and milliseconds (what an operator holds in
    /// their head) — see #347's acceptance criteria: a message that only
    /// says "readings differ" would pass on ordinary jitter and miss the
    /// point.
    pub fn message(&self) -> String {
        let delta_ms = self.delta_samples as f64 / self.sample_rate as f64 * 1000.0;
        match self.periods {
            Some(n) => {
                let period = self
                    .period_size
                    .expect("periods is Some only when period_size is Some");
                format!(
                    "\u{3c4} readings disagree by exactly {} period{} of {period} samples \
                     ({:.3} samples \u{2192} {:.3} samples, \u{394} {} samples = {delta_ms:.4} \
                     ms at {} Hz) \u{2014} a graph-buffering shift, not hardware drift",
                    n.unsigned_abs(),
                    if n.unsigned_abs() == 1 { "" } else { "s" },
                    self.reading1_s * self.sample_rate as f64,
                    self.reading2_s * self.sample_rate as f64,
                    self.delta_samples,
                    self.sample_rate,
                )
            }
            None => format!(
                "\u{3c4} readings disagree, not a period multiple ({:.3} samples \u{2192} \
                 {:.3} samples, \u{394} {} samples = {delta_ms:.4} ms at {} Hz)",
                self.reading1_s * self.sample_rate as f64,
                self.reading2_s * self.sample_rate as f64,
                self.delta_samples,
                self.sample_rate,
            ),
        }
    }
}

/// Compare two independently-lifecycled τ readings (#347) and classify the
/// result. Works in whole samples, derived directly from the issue's own
/// rig data (`+1024.000` exact, fractional part unchanged across the
/// jump): `delta_samples = round((reading2_s - reading1_s) * sample_rate)`.
pub fn compare_tau_readings(
    reading1_s: f64,
    reading2_s: f64,
    sample_rate: u32,
    period_size: Option<u32>,
) -> TauComparison {
    let delta_samples = ((reading2_s - reading1_s) * sample_rate as f64).round() as i64;
    if delta_samples == 0 {
        return TauComparison::Agree;
    }
    let periods = period_size.and_then(|p| {
        let p = p as i64;
        (p != 0 && delta_samples % p == 0).then_some(delta_samples / p)
    });
    TauComparison::Disagree(TauDisagreement {
        reading1_s,
        reading2_s,
        delta_samples,
        sample_rate,
        period_size,
        periods,
    })
}

impl Calibration {
    /// Exact-match τ lookup. Refuses rather than interpolating or falling
    /// back to "closest" — a stale τ is a silent-wrongness bug (issue
    /// #281), so a miss must say so and name the delta, not degrade.
    pub fn tau_for(&self, cond: &TauConditions) -> Result<&TauEntry, Box<TauRefusal>> {
        if let Some(hit) = self.tau_history.iter().find(|e| &e.conditions == cond) {
            return Ok(hit);
        }
        let mut nearest: Option<&TauEntry> = None;
        let mut best_diff = usize::MAX;
        for e in &self.tau_history {
            let n_diff = tau_diff_fields(cond, &e.conditions).len();
            let better = n_diff < best_diff
                || (n_diff == best_diff
                    && nearest
                        .map(|n| e.measured_at > n.measured_at)
                        .unwrap_or(true));
            if better {
                best_diff = n_diff;
                nearest = Some(e);
            }
        }
        let differing_fields = nearest
            .map(|n| tau_diff_fields(cond, &n.conditions))
            .unwrap_or_default();
        Err(Box::new(TauRefusal {
            requested: cond.clone(),
            nearest: nearest.cloned(),
            differing_fields,
        }))
    }
}

/// Fixtures shared with the persistence tests in [`super::store`], which
/// round-trip a τ entry through disk alongside the voltage and SPL fields.
#[cfg(test)]
pub(super) mod fixtures {
    use super::{TauConditions, TauEntry};

    pub(crate) fn dummy_conditions() -> TauConditions {
        TauConditions {
            device: 0,
            backend: "fake".to_string(),
            sample_rate: 48_000,
            period_size: Some(1024),
            output_port: "fake:playback_0".to_string(),
            input_port: "fake:capture_0".to_string(),
        }
    }

    pub(crate) fn dummy_tau_entry(cond: TauConditions, tau_s: f64) -> TauEntry {
        TauEntry {
            conditions: cond,
            tau_s,
            measured_at: "2026-01-01T00:00:00Z".to_string(),
            method: "farina_short_ess".to_string(),
            agreement_count: 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{dummy_conditions, dummy_tau_entry};
    use super::*;

    #[test]
    fn tau_for_exact_match_hits() {
        let mut cal = Calibration::new(0, 0);
        let cond = dummy_conditions();
        cal.tau_history
            .push(dummy_tau_entry(cond.clone(), 0.0011931));
        let hit = cal.tau_for(&cond).expect("exact match should hit");
        assert!((hit.tau_s - 0.0011931).abs() < 1e-12);
    }

    #[test]
    fn tau_for_refuses_on_period_size_change_and_names_the_delta() {
        // #281 acceptance criterion: "a synthetic entry recorded at one
        // period size is refused at another, with the delta in the
        // message" — τ moves by milliseconds on a period-size change, so
        // this must never silently degrade to the stored value.
        let mut cal = Calibration::new(0, 0);
        let stored = dummy_conditions();
        cal.tau_history
            .push(dummy_tau_entry(stored.clone(), 0.0011931));

        let mut requested = stored.clone();
        requested.period_size = Some(256);

        let refusal = cal
            .tau_for(&requested)
            .expect_err("period-size mismatch must refuse, not degrade");
        assert_eq!(refusal.differing_fields, vec!["period_size"]);
        assert_eq!(refusal.nearest.as_ref().unwrap().tau_s, 0.0011931);
        let msg = refusal.message();
        assert!(
            msg.contains("period_size"),
            "message must name the differing field: {msg}"
        );
        assert!(
            msg.contains("256"),
            "message must name the requested value: {msg}"
        );
        assert!(
            msg.contains("1024"),
            "message must name the stored value: {msg}"
        );
    }

    #[test]
    fn tau_for_refuses_with_no_nearest_when_history_is_empty() {
        let cal = Calibration::new(0, 0);
        let refusal = cal.tau_for(&dummy_conditions()).unwrap_err();
        assert!(refusal.nearest.is_none());
        assert!(refusal.differing_fields.is_empty());
        assert!(refusal.message().contains("no \u{3c4} history"));
    }

    #[test]
    fn compare_tau_readings_exact_match_agrees() {
        let cmp = compare_tau_readings(0.001, 0.001, 48_000, Some(1024));
        assert_eq!(cmp, TauComparison::Agree);
    }

    #[test]
    fn compare_tau_readings_sub_sample_jitter_still_agrees() {
        // #347 acceptance: within-lifecycle stability is 0.001 frame; the
        // comparator must not flag that as a disagreement.
        let cmp = compare_tau_readings(0.001, 0.001 + 1e-9, 48_000, Some(1024));
        assert_eq!(cmp, TauComparison::Agree);
    }

    #[test]
    fn compare_tau_readings_refuses_on_period_shift_and_names_the_period() {
        // #347's own rig data: 4262.064 frames -> 5286.064 frames at
        // 96 kHz, exactly +1024.000 samples = one period, fractional part
        // unchanged. A test that only checks "readings differ" would pass
        // on noise and miss the point — assert the period is *named*.
        let sr = 96_000;
        let period = 1024u32;
        let reading1_s = 4262.064 / sr as f64;
        let reading2_s = 5286.064 / sr as f64;
        let cmp = compare_tau_readings(reading1_s, reading2_s, sr, Some(period));
        let TauComparison::Disagree(d) = cmp else {
            panic!("expected a period-shift disagreement, got {cmp:?}");
        };
        assert_eq!(d.delta_samples, 1024);
        assert_eq!(d.periods, Some(1));
        let msg = d.message();
        assert!(
            msg.contains("1 period"),
            "message must name the period count: {msg}"
        );
        assert!(
            msg.contains("1024"),
            "message must name the period size: {msg}"
        );
        assert!(
            msg.contains("10.6667 ms"),
            "message must name the delta in ms: {msg}"
        );
    }

    #[test]
    fn compare_tau_readings_multi_period_shift_names_the_count() {
        let sr = 48_000;
        let period = 512u32;
        let reading1_s = 1000.0 / sr as f64;
        let reading2_s = (1000.0 + 1536.0) / sr as f64; // 3 periods
        let cmp = compare_tau_readings(reading1_s, reading2_s, sr, Some(period));
        let TauComparison::Disagree(d) = cmp else {
            panic!("expected a period-shift disagreement, got {cmp:?}");
        };
        assert_eq!(d.periods, Some(3));
        assert!(d.message().contains("3 periods"), "got {}", d.message());
    }

    #[test]
    fn compare_tau_readings_non_period_delta_is_a_different_fault() {
        // #347 acceptance: "a disagreement that is not a multiple of the
        // period is a different fault and should say so" — not laundered
        // through the same message as a period-shift.
        let sr = 96_000;
        let reading1_s = 4262.064 / sr as f64;
        let reading2_s = 4290.500 / sr as f64; // delta 28.436 -> rounds to 28
        let cmp = compare_tau_readings(reading1_s, reading2_s, sr, Some(1024));
        let TauComparison::Disagree(d) = cmp else {
            panic!("expected a disagreement, got {cmp:?}");
        };
        assert_eq!(d.periods, None);
        let msg = d.message();
        assert!(
            msg.contains("not a period multiple"),
            "message must say this is a different fault class: {msg}"
        );
    }

    #[test]
    fn compare_tau_readings_unknown_period_size_is_never_a_period_shift() {
        // A backend that can't report a period size (AudioEngine::
        // period_size -> None) can never corroborate the period-shift
        // classification, even if the delta happens to look tidy.
        let cmp = compare_tau_readings(0.0, 1024.0 / 48_000.0, 48_000, None);
        let TauComparison::Disagree(d) = cmp else {
            panic!("expected a disagreement, got {cmp:?}");
        };
        assert_eq!(d.periods, None);
    }

    /// Every condition field must be able to appear in a delta, with both
    /// sides rendered. A field the delta table forgets would otherwise be
    /// invisible in the one message whose job is to name what differs —
    /// and `tau_for`'s nearest-entry ranking, which counts differing
    /// fields, would score a genuinely different entry as an exact match.
    #[test]
    fn tau_deltas_name_every_condition_field() {
        let base = dummy_conditions();
        let mutations: Vec<(&str, TauConditions, &str)> = vec![
            (
                "device",
                TauConditions {
                    device: 7,
                    ..base.clone()
                },
                "7",
            ),
            (
                "backend",
                TauConditions {
                    backend: "jack".to_string(),
                    ..base.clone()
                },
                "jack",
            ),
            (
                "sample_rate",
                TauConditions {
                    sample_rate: 96_000,
                    ..base.clone()
                },
                "96000 Hz",
            ),
            (
                "period_size",
                TauConditions {
                    period_size: None,
                    ..base.clone()
                },
                "n/a",
            ),
            (
                "output_port",
                TauConditions {
                    output_port: "other:playback_1".to_string(),
                    ..base.clone()
                },
                "other:playback_1",
            ),
            (
                "input_port",
                TauConditions {
                    input_port: "other:capture_1".to_string(),
                    ..base.clone()
                },
                "other:capture_1",
            ),
        ];
        for (field, changed, rendered) in mutations {
            let deltas = tau_deltas(&base, &changed);
            assert_eq!(
                deltas.iter().map(|d| d.field).collect::<Vec<_>>(),
                vec![field],
                "changing {field} alone should name exactly that field"
            );
            assert_eq!(
                deltas[0].stored, rendered,
                "{field} rendered as {:?}, expected {rendered:?}",
                deltas[0].stored
            );
        }
    }

    /// A refusal message must carry the rendered values, not just the
    /// names — the whole point of #281's refusal is that the reader does
    /// not have to open cal.json to see what moved.
    #[test]
    fn refusal_message_renders_both_sides_of_a_delta() {
        let mut cal = Calibration::new(0, 0);
        cal.tau_history
            .push(dummy_tau_entry(dummy_conditions(), 0.001));
        let mut wanted = dummy_conditions();
        wanted.period_size = Some(64);
        let refusal = cal.tau_for(&wanted).expect_err("period size differs");
        let msg = refusal.message();
        assert!(
            msg.contains("period_size (requested 64, stored 1024)"),
            "message did not render both sides: {msg}"
        );
    }
}
