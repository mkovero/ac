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
//! to "closest". Among several exact matches the lookup prefers one from the
//! current device-enumeration epoch, then the newest (#461); the epoch is
//! compared and reported, never used to refuse (see [`super::epoch`]).

use serde::{Deserialize, Serialize};

use super::epoch::{DeviceEpoch, EnumerationCheck};
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
/// silently replaces a good one; [`Calibration::tau_for`] filters them by
/// exact condition match and picks among the matches by epoch, then age.
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
    /// What the graph declared its own path latency to be, in frames, while
    /// this entry was measured (#363). The second, structurally different
    /// account of the same path: the readings are what `ac` measured, this is
    /// what the graph asserted about itself.
    ///
    /// **Not a τ, and never subtracted from one.** It carries the driver's
    /// claim plus `jackd`'s user-supplied `-I`/`-O` arguments, neither of
    /// which is validated. `None` on an entry written before this field
    /// existed *and* on a backend that declares nothing — on disk those two
    /// are indistinguishable, so a reader must not infer a backend
    /// limitation from its absence (the wire can tell them apart; see
    /// ZMQ.md's null-versus-absent rule).
    #[serde(default)]
    pub declared_latency_frames: Option<u32>,
    /// Wall-clock seconds between the two lifecycles' captures (#363).
    ///
    /// This is the number that says what the agreement is worth. The failure
    /// this field exists for is a graph-buffering state that persists over
    /// *seconds*: two readings about a second apart land in the same state
    /// and agree, which is why 42 of 97 rig runs stored a value one period
    /// short while reporting that two readings agreed. A reader compares this
    /// against how long the state is believed to persist; the instrument
    /// cannot do that comparison for them. `None` on entries written before
    /// this field existed.
    #[serde(default)]
    pub reading_separation_s: Option<f64>,
    /// The device-enumeration epoch this entry was measured in (#461).
    /// `None` on an entry written before this field existed, which a lookup
    /// reports as [`EnumerationCheck::NotRecorded`] — never as current.
    #[serde(default)]
    pub enumeration: Option<DeviceEpoch>,
    /// Opaque identifier of the daemon session that measured this entry,
    /// `"<pid>@<daemon started_at>"` (#461). JSON only; never printed.
    /// `None` on entries written before this field existed.
    #[serde(default)]
    pub session: Option<String>,
    /// The reference loopback's τ, read from the reference leg of the same
    /// captures this entry's τ was read from (#544). `Some` only when both
    /// lifecycles' reference readings passed calibrate's own gates and
    /// agreed to the whole sample; `None` on an entry written before this
    /// field existed, on a run with no reference configured, and whenever
    /// the reference reading was refused. `None` means "no inter-pair offset
    /// from this entry" — never an offset of zero.
    ///
    /// The inter-pair offset is `tau_s − reference.tau_s`
    /// ([`ResolvedPairOffset::offset_s`]). Both terms come from one capture,
    /// so any converter, transport or graph state is common to them by
    /// construction; an offset built from two entries measured at different
    /// times would not have that property.
    #[serde(default)]
    pub reference: Option<TauReferenceLeg>,
}

/// The reference loopback's reading taken in the same captures as a
/// [`TauEntry`]'s own τ (#544). The ports are part of the offset's topology
/// key: an offset measured against one loopback never applies against
/// another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TauReferenceLeg {
    pub output_port: String,
    pub input_port: String,
    pub tau_s: f64,
}

/// An inter-pair offset resolved for a capture pair against a reference
/// loopback (#544): the τ entry that carries it, and how that entry's
/// device-enumeration epoch relates to the current one. A non-`Same` check
/// is a flag, never a refusal — the premise that the offset survives
/// re-enumeration is what #544's rig check tests.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedPairOffset<'a> {
    pub entry: &'a TauEntry,
    pub reference: &'a TauReferenceLeg,
    pub check: EnumerationCheck,
}

impl ResolvedPairOffset<'_> {
    /// `τ(this pair) − τ(reference pair)`, both from one capture. Positive
    /// when this pair's path is longer than the reference's.
    pub fn offset_s(&self) -> f64 {
        self.entry.tau_s - self.reference.tau_s
    }
}

/// Why [`Calibration::pair_offset_for`] found no inter-pair offset (#544).
/// Refuses rather than falling back to the stored absolute τ — that is the
/// model #544 removes — and names what differs, the way [`TauRefusal`]
/// does.
#[derive(Debug, Clone, PartialEq)]
pub struct PairOffsetRefusal {
    pub requested: TauConditions,
    pub reference_output_port: String,
    pub reference_input_port: String,
    pub miss: PairOffsetMiss,
}

/// The three ways an offset lookup misses.
#[derive(Debug, Clone, PartialEq)]
pub enum PairOffsetMiss {
    /// No τ entry, with or without a reference leg, is close enough to
    /// name: nothing on file matches these conditions and no entry carries
    /// a reference leg.
    NoTau {
        /// Whether any entry exists for this pair's own ports (under other
        /// conditions).
        pair_on_file: bool,
    },
    /// τ entries exist for these exact conditions, but none was measured
    /// with a reference leg — the common case on first upgrade.
    TauWithoutReference,
    /// The nearest entry that carries a reference leg, by fewest differing
    /// fields (conditions plus reference ports), ties broken by newest.
    Differs {
        nearest: TauEntry,
        /// One rendered line per differing field, in tuple order.
        lines: Vec<String>,
    },
}

impl PairOffsetRefusal {
    /// One observation per line: what is on file, never a cause.
    pub fn lines(&self) -> Vec<String> {
        match &self.miss {
            PairOffsetMiss::NoTau {
                pair_on_file: false,
            } => vec!["no \u{3c4} on file for this pair".to_string()],
            PairOffsetMiss::NoTau { pair_on_file: true } => {
                vec!["no \u{3c4} on file for this pair at these conditions".to_string()]
            }
            PairOffsetMiss::TauWithoutReference => {
                vec!["\u{3c4} on file, reference leg not measured with it".to_string()]
            }
            PairOffsetMiss::Differs { lines, .. } => lines.clone(),
        }
    }
}

/// A τ entry that matched the requested conditions exactly, and how its
/// device-enumeration epoch relates to the current one (#461). A non-`Same`
/// check flags the value; it never refuses it.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTau<'a> {
    pub entry: &'a TauEntry,
    pub check: EnumerationCheck,
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
    /// Short operator-facing name, as in "entry measured at period 512".
    label: &'static str,
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
        ($( $field:ident, $label:literal => $render:expr ),* $(,)?) => {{
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
                        label: $label,
                        requested: render(a),
                        stored: render(b),
                    });
                }
            )*
            out
        }};
    }
    deltas! {
        device, "device" => device.to_string(),
        backend, "backend" => backend.clone(),
        sample_rate, "rate" => format!("{sample_rate} Hz"),
        period_size, "period" => period_size
            .map(|p| p.to_string())
            .unwrap_or_else(|| "n/a".to_string()),
        output_port, "output" => output_port.clone(),
        input_port, "input" => input_port.clone(),
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
    ///
    /// Among several exact matches (#461): the newest entry whose epoch is
    /// [`EnumerationCheck::Same`] as `current` wins; failing that, the newest
    /// entry, carrying its non-`Same` check. "Newest" is `measured_at`, ties
    /// broken by append order. The epoch never causes a refusal.
    pub fn tau_for(
        &self,
        cond: &TauConditions,
        current: &DeviceEpoch,
    ) -> Result<ResolvedTau<'_>, Box<TauRefusal>> {
        let matches: Vec<ResolvedTau<'_>> = self
            .tau_history
            .iter()
            .filter(|e| &e.conditions == cond)
            .map(|entry| ResolvedTau {
                entry,
                check: EnumerationCheck::of(entry.enumeration.as_ref(), current),
            })
            .collect();
        // `max_by` keeps the last of equal elements, so append order breaks
        // a `measured_at` tie in favour of the later entry.
        fn newest(candidates: Vec<ResolvedTau<'_>>) -> Option<ResolvedTau<'_>> {
            candidates
                .into_iter()
                .max_by(|a, b| a.entry.measured_at.cmp(&b.entry.measured_at))
        }
        let (same, other): (Vec<_>, Vec<_>) = matches.into_iter().partition(|r| r.check.is_same());
        if let Some(hit) = newest(same).or_else(|| newest(other)) {
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

    /// Exact-match inter-pair offset lookup (#544): the τ entry for `cond`
    /// whose reference leg was read on `reference_output_port` →
    /// `reference_input_port` in the same captures.
    ///
    /// The topology key is every [`TauConditions`] field **plus** both
    /// reference ports, all exact. Among several matches the same rule as
    /// [`Self::tau_for`]: newest same-epoch entry, then newest entry, with
    /// its non-`Same` check carried as a flag. An entry without a reference
    /// leg never matches, and nothing falls back to the absolute τ.
    pub fn pair_offset_for(
        &self,
        cond: &TauConditions,
        reference_output_port: &str,
        reference_input_port: &str,
        current: &DeviceEpoch,
    ) -> Result<ResolvedPairOffset<'_>, Box<PairOffsetRefusal>> {
        let same_ports = |r: &TauReferenceLeg| {
            r.output_port == reference_output_port && r.input_port == reference_input_port
        };
        let matches: Vec<ResolvedPairOffset<'_>> = self
            .tau_history
            .iter()
            .filter(|e| &e.conditions == cond)
            .filter_map(|entry| {
                let reference = entry.reference.as_ref().filter(|r| same_ports(r))?;
                Some(ResolvedPairOffset {
                    entry,
                    reference,
                    check: EnumerationCheck::of(entry.enumeration.as_ref(), current),
                })
            })
            .collect();
        fn newest(candidates: Vec<ResolvedPairOffset<'_>>) -> Option<ResolvedPairOffset<'_>> {
            candidates
                .into_iter()
                .max_by(|a, b| a.entry.measured_at.cmp(&b.entry.measured_at))
        }
        let (same, other): (Vec<_>, Vec<_>) = matches.into_iter().partition(|r| r.check.is_same());
        if let Some(hit) = newest(same).or_else(|| newest(other)) {
            return Ok(hit);
        }

        let exact_without_reference = self
            .tau_history
            .iter()
            .any(|e| &e.conditions == cond && e.reference.is_none());
        let with_reference_exact = self
            .tau_history
            .iter()
            .any(|e| &e.conditions == cond && e.reference.is_some());
        let miss = if exact_without_reference && !with_reference_exact {
            PairOffsetMiss::TauWithoutReference
        } else {
            let mut nearest: Option<&TauEntry> = None;
            let mut best_diff = usize::MAX;
            for e in &self.tau_history {
                let Some(r) = e.reference.as_ref() else {
                    continue;
                };
                let n_diff =
                    tau_diff_fields(cond, &e.conditions).len() + usize::from(!same_ports(r));
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
            match nearest {
                Some(n) => {
                    let r = n
                        .reference
                        .as_ref()
                        .expect("nearest is drawn from entries with a reference leg");
                    let mut lines: Vec<String> = tau_deltas(cond, &n.conditions)
                        .into_iter()
                        .map(|d| {
                            format!(
                                "entry measured at {} {}, this capture {} {}",
                                d.label, d.stored, d.label, d.requested
                            )
                        })
                        .collect();
                    if !same_ports(r) {
                        lines.push(format!(
                            "entry measured against ref {} \u{2192} {}, this capture ref {} \
                             \u{2192} {}",
                            r.output_port,
                            r.input_port,
                            reference_output_port,
                            reference_input_port
                        ));
                    }
                    PairOffsetMiss::Differs {
                        nearest: n.clone(),
                        lines,
                    }
                }
                None => PairOffsetMiss::NoTau {
                    pair_on_file: self.tau_history.iter().any(|e| {
                        e.conditions.output_port == cond.output_port
                            && e.conditions.input_port == cond.input_port
                    }),
                },
            }
        };
        Err(Box::new(PairOffsetRefusal {
            requested: cond.clone(),
            reference_output_port: reference_output_port.to_string(),
            reference_input_port: reference_input_port.to_string(),
            miss,
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
            declared_latency_frames: None,
            reading_separation_s: None,
            enumeration: None,
            session: None,
            reference: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::epoch::fixtures::observed;
    use super::fixtures::{dummy_conditions, dummy_tau_entry};
    use super::*;

    /// An epoch for tests that do not care which one it is.
    fn any_epoch() -> DeviceEpoch {
        observed("boot-a", &[("/dev/fw1", "2026-09-15T08:00:05Z")])
    }

    #[test]
    fn tau_for_exact_match_hits() {
        let mut cal = Calibration::new(0, 0);
        let cond = dummy_conditions();
        cal.tau_history
            .push(dummy_tau_entry(cond.clone(), 0.0011931));
        let hit = cal
            .tau_for(&cond, &any_epoch())
            .expect("exact match should hit");
        assert!((hit.entry.tau_s - 0.0011931).abs() < 1e-12);
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
            .tau_for(&requested, &any_epoch())
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
        let refusal = cal.tau_for(&dummy_conditions(), &any_epoch()).unwrap_err();
        assert!(refusal.nearest.is_none());
        assert!(refusal.differing_fields.is_empty());
        assert!(refusal.message().contains("no \u{3c4} history"));
    }

    /// #461 AC6, written against the rejected behaviour. Two entries under
    /// identical conditions: the older measured in epoch E1 (1711 samples),
    /// the newer in E2 (1743 samples) — pupu's test A values.
    ///
    /// The lookup this replaced was `tau_history.iter().find(..)`: it takes
    /// the first-appended match, E1, whatever epoch is current. That is why
    /// re-running `ac calibrate` after a reboot changed nothing `plot ir`
    /// subtracted. The rejected pick is computed here and shown to be E1.
    #[test]
    fn tau_for_prefers_the_current_epoch_and_flags_a_crossed_one() {
        let sr = 96_000.0;
        let e1 = observed("boot-1", &[("/dev/fw2", "2026-09-15T08:00:05Z")]);
        let e2 = observed("boot-2", &[("/dev/fw1", "2026-09-15T13:41:50Z")]);
        let e3 = observed("boot-3", &[("/dev/fw1", "2026-09-16T00:08:31Z")]);
        let cond = dummy_conditions();

        let mut cal = Calibration::new(0, 0);
        let mut older = dummy_tau_entry(cond.clone(), 1711.0 / sr);
        older.measured_at = "2026-09-15T09:00:00Z".to_string();
        older.enumeration = Some(e1.clone());
        let mut newer = dummy_tau_entry(cond.clone(), 1743.0 / sr);
        newer.measured_at = "2026-09-15T14:00:00Z".to_string();
        newer.enumeration = Some(e2.clone());
        cal.tau_history.push(older);
        cal.tau_history.push(newer);

        let rejected = cal
            .tau_history
            .iter()
            .find(|e| e.conditions == cond)
            .unwrap();
        assert_eq!(rejected.enumeration.as_ref(), Some(&e1));

        let now_e2 = cal.tau_for(&cond, &e2).unwrap();
        assert_eq!(now_e2.check, EnumerationCheck::Same);
        assert_eq!(now_e2.entry.enumeration.as_ref(), Some(&e2));
        assert_ne!(now_e2.entry, rejected, "first-match would have used E1");

        // Current epoch E3: nothing matches it, so the newest entry resolves
        // and carries the crossed boundary — never `Same`.
        let now_e3 = cal.tau_for(&cond, &e3).unwrap();
        assert_eq!(now_e3.entry.enumeration.as_ref(), Some(&e2));
        assert!(
            matches!(now_e3.check, EnumerationCheck::Crossed { .. }),
            "a stored τ from another epoch must not resolve silently: {:?}",
            now_e3.check
        );

        // Current epoch E1: the older, same-epoch entry wins over the newer.
        let now_e1 = cal.tau_for(&cond, &e1).unwrap();
        assert_eq!(now_e1.check, EnumerationCheck::Same);
        assert_eq!(now_e1.entry.enumeration.as_ref(), Some(&e1));
    }

    /// #461 AC7: within one epoch a τ resolves as before, with no flag; two
    /// same-epoch entries resolve to the newer, and an equal `measured_at`
    /// goes to the later-appended one.
    #[test]
    fn tau_for_within_one_epoch_is_same_and_picks_the_newest() {
        let epoch = any_epoch();
        let cond = dummy_conditions();
        let mut cal = Calibration::new(0, 0);
        for tau_s in [0.001, 0.002, 0.003] {
            let mut e = dummy_tau_entry(cond.clone(), tau_s);
            e.enumeration = Some(epoch.clone());
            cal.tau_history.push(e);
        }
        cal.tau_history[0].measured_at = "2026-09-16T10:00:00Z".to_string();
        let hit = cal.tau_for(&cond, &epoch).unwrap();
        assert_eq!(hit.check, EnumerationCheck::Same);
        assert_eq!(hit.entry.tau_s, 0.001, "newest measured_at wins");

        cal.tau_history[0].measured_at = "2026-01-01T00:00:00Z".to_string();
        let hit = cal.tau_for(&cond, &epoch).unwrap();
        assert_eq!(hit.entry.tau_s, 0.003, "a tie goes to the later append");
    }

    /// #461 (d): an entry stored before tracking has no epoch and must never
    /// read as current.
    #[test]
    fn tau_for_an_entry_without_an_epoch_is_not_recorded() {
        let cond = dummy_conditions();
        let mut cal = Calibration::new(0, 0);
        cal.tau_history.push(dummy_tau_entry(cond.clone(), 0.001));
        let hit = cal.tau_for(&cond, &any_epoch()).unwrap();
        assert_eq!(hit.check, EnumerationCheck::NotRecorded);
    }

    /// A `cal.json` entry written before #461 has neither new key and still
    /// loads.
    #[test]
    fn a_pre_461_entry_deserializes_with_no_epoch_or_session() {
        let raw = r#"{
            "conditions": {"device": 0, "backend": "jack", "sample_rate": 96000,
                           "period_size": 256, "output_port": "a", "input_port": "b"},
            "tau_s": 0.0178229, "measured_at": "2026-09-15T23:43:04Z",
            "method": "farina_short_ess_v2", "agreement_count": 2
        }"#;
        let e: TauEntry = serde_json::from_str(raw).unwrap();
        assert_eq!(e.enumeration, None);
        assert_eq!(e.session, None);
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
        let refusal = cal
            .tau_for(&wanted, &any_epoch())
            .expect_err("period size differs");
        let msg = refusal.message();
        assert!(
            msg.contains("period_size (requested 64, stored 1024)"),
            "message did not render both sides: {msg}"
        );
    }

    // ─── #544: inter-pair offset ─────────────────────────────────────────

    const REF_OUT: &str = "fake:playback_1";
    const REF_IN: &str = "fake:capture_1";

    fn with_reference(mut e: TauEntry, ref_tau_s: f64) -> TauEntry {
        e.reference = Some(TauReferenceLeg {
            output_port: REF_OUT.to_string(),
            input_port: REF_IN.to_string(),
            tau_s: ref_tau_s,
        });
        e
    }

    /// The offset is this pair's τ minus the reference's, both from one
    /// entry, and a non-zero one keeps its sign: 1757 − 1711 = +46.
    #[test]
    fn pair_offset_for_takes_the_entry_carrying_the_reference_leg() {
        let sr = 96_000.0;
        let cond = dummy_conditions();
        let mut cal = Calibration::new(0, 0);
        cal.tau_history.push(with_reference(
            dummy_tau_entry(cond.clone(), 1757.0 / sr),
            1711.0 / sr,
        ));
        let hit = cal
            .pair_offset_for(&cond, REF_OUT, REF_IN, &any_epoch())
            .unwrap();
        assert_eq!(((hit.offset_s()) * sr).round(), 46.0);
        assert_eq!(hit.check, EnumerationCheck::NotRecorded);
    }

    /// An entry with no reference leg never yields an offset — not zero,
    /// and not the absolute τ — and the refusal says τ is on file.
    #[test]
    fn pair_offset_for_refuses_a_tau_without_a_reference_leg() {
        let cond = dummy_conditions();
        let mut cal = Calibration::new(0, 0);
        cal.tau_history.push(dummy_tau_entry(cond.clone(), 0.0178));
        assert!(cal.tau_for(&cond, &any_epoch()).is_ok(), "test setup");
        let refusal = cal
            .pair_offset_for(&cond, REF_OUT, REF_IN, &any_epoch())
            .unwrap_err();
        assert_eq!(refusal.miss, PairOffsetMiss::TauWithoutReference);
        assert_eq!(
            refusal.lines(),
            vec!["\u{3c4} on file, reference leg not measured with it"]
        );
    }

    /// A reference leg against another loopback, or at another period, is a
    /// different topology: refused, each differing field on its own line.
    #[test]
    fn pair_offset_for_refuses_another_topology_and_names_the_fields() {
        let cond = dummy_conditions();
        let mut cal = Calibration::new(0, 0);
        cal.tau_history
            .push(with_reference(dummy_tau_entry(cond.clone(), 0.02), 0.02));

        let refusal = cal
            .pair_offset_for(&cond, "fake:playback_7", REF_IN, &any_epoch())
            .unwrap_err();
        assert_eq!(
            refusal.lines(),
            vec![format!(
                "entry measured against ref {REF_OUT} \u{2192} {REF_IN}, this capture ref \
                 fake:playback_7 \u{2192} {REF_IN}"
            )]
        );

        let mut other = cond.clone();
        other.period_size = Some(256);
        let refusal = cal
            .pair_offset_for(&other, REF_OUT, REF_IN, &any_epoch())
            .unwrap_err();
        assert_eq!(
            refusal.lines(),
            vec!["entry measured at period 1024, this capture period 256"]
        );
    }

    /// Nothing on file at all names the pair as having no τ.
    #[test]
    fn pair_offset_for_with_no_history_says_no_tau() {
        let cal = Calibration::new(0, 0);
        let refusal = cal
            .pair_offset_for(&dummy_conditions(), REF_OUT, REF_IN, &any_epoch())
            .unwrap_err();
        assert_eq!(refusal.lines(), vec!["no \u{3c4} on file for this pair"]);
    }

    /// Same epoch first, then newest — [`Calibration::tau_for`]'s rule.
    #[test]
    fn pair_offset_for_prefers_the_current_epoch() {
        let e1 = observed("boot-1", &[("/dev/fw2", "2026-09-15T08:00:05Z")]);
        let e2 = observed("boot-2", &[("/dev/fw1", "2026-09-15T13:41:50Z")]);
        let cond = dummy_conditions();
        let mut cal = Calibration::new(0, 0);
        let mut older = with_reference(dummy_tau_entry(cond.clone(), 0.010), 0.010);
        older.measured_at = "2026-09-15T09:00:00Z".to_string();
        older.enumeration = Some(e1.clone());
        let mut newer = with_reference(dummy_tau_entry(cond.clone(), 0.011), 0.010);
        newer.measured_at = "2026-09-15T14:00:00Z".to_string();
        newer.enumeration = Some(e2.clone());
        cal.tau_history.push(older);
        cal.tau_history.push(newer);
        let hit = cal.pair_offset_for(&cond, REF_OUT, REF_IN, &e1).unwrap();
        assert_eq!(hit.check, EnumerationCheck::Same);
        assert_eq!(hit.entry.tau_s, 0.010);
        let hit = cal.pair_offset_for(&cond, REF_OUT, REF_IN, &e2).unwrap();
        assert_eq!(hit.entry.tau_s, 0.011);
    }

    /// An entry written before #544 decodes with no reference leg.
    #[test]
    fn a_pre_544_entry_deserializes_with_no_reference_leg() {
        let raw = r#"{
            "conditions": {"device": 0, "backend": "jack", "sample_rate": 96000,
                           "period_size": 256, "output_port": "a", "input_port": "b"},
            "tau_s": 0.0178229, "measured_at": "2026-09-15T23:43:04Z",
            "method": "farina_short_ess_v2", "agreement_count": 2
        }"#;
        let e: TauEntry = serde_json::from_str(raw).unwrap();
        assert_eq!(e.reference, None);
    }
}
