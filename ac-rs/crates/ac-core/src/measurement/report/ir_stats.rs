//! Derived read-out quantities for an impulse-response payload, and the
//! verdict on whether its peak is a trustworthy deconvolution result
//! (#376). Computed once here so `ac-cli`'s text read-out and
//! `ac-scene`'s sweep-IR panel cannot disagree about a capture.

use super::{GateParams, InterfaceLatency, MeasurementData, MeasurementReport};

/// Minimum pre-impulse SNR, in dB, below which a deconvolution is
/// reported as failed rather than as a result (#376). Below this floor
/// the linear-IR peak is not reliably the system response — it can land
/// wherever the pre-impulse noise floor happens to be largest, producing
/// a plausible-looking arrival/distance from noise.
///
/// Value: 18.0 dB — the worst observed *bad* capture in the rig table in
/// the #376 issue body (−42 dBFS drive, pre-impulse SNR up to 16.5 dB with
/// a peak index far from the true arrival) plus a 1.5 dB margin. The raw
/// log and results doc the issue body itself cites for that table
/// (`audit/rig-353-2026-08-23/ladder-3m.log`,
/// `work/rig/rig-2026-08-23-onset-353-results.md`) never landed in this
/// repo — the table reproduced in the issue is the only source checked
/// here; do not add a citation to either path without confirming the file
/// exists first. The same table's worst observed *good* capture (−36 dBFS
/// drive) reaches down to 14.5 dB, so
/// no single threshold separates this dataset cleanly — 18.0 dB is set
/// at or above the worst bad case rather than at the overlap's midpoint,
/// so a false refusal (cheap: re-run) is preferred over a false accept
/// (expensive: a silently wrong logged distance). This also means some
/// borderline-good low-drive captures near the boundary will be
/// refused — low drive is the operator-encouraged *safe* choice under
/// the rig's emission consent rules, so that blind spot is real and
/// documented here rather than picked by eye.
pub const PRE_IMPULSE_SNR_MIN_DB: f64 = 18.0;

impl MeasurementReport {
    /// Derived read-out quantities for the report's first
    /// `ImpulseResponse` payload: arrival timing, peak magnitude,
    /// pre-impulse SNR, and the time gate's low-frequency limit. `None`
    /// when no payload carries an impulse response, or its linear IR is
    /// empty (see issue #283).
    ///
    /// Arrival (`delay_samples`/`arrival_s`) is derived from an onset
    /// estimate ([`crate::measurement::sweep::estimate_onset`]), not from
    /// the IR's magnitude peak — see [`IrStats::onset_index`]'s doc for why
    /// (#346). When this report carries both a measured interface latency
    /// and a recorded `position.distance_m`, the onset estimate is bound
    /// to reject any candidate earlier than pure flight time allows.
    pub fn ir_stats(&self) -> Option<IrStats> {
        let (payload, sample_rate_hz, linear_ir) =
            self.data.iter().find_map(|p| match &p.data {
                MeasurementData::ImpulseResponse {
                    sample_rate_hz,
                    linear_ir,
                    ..
                } => Some((p, sample_rate_hz, linear_ir)),
                _ => None,
            })?;
        if linear_ir.is_empty() || *sample_rate_hz == 0 {
            return None;
        }
        let window_len = linear_ir.len();
        let (peak_index, peak_magnitude) = ir_peak(linear_ir);

        // `extract_irs` (`measurement::sweep`) centres the gate at the
        // sweep endpoint — the position an identity (zero-delay) system
        // would peak at.
        let centre = window_len / 2;

        let pre_region = pre_impulse_region(linear_ir, peak_index);
        let pre_impulse_snr_db = pre_impulse_snr_db(pre_region, peak_magnitude);

        // The onset picker's validity gate runs off a *median* floor over
        // the same region, not off `pre_impulse_snr_db`'s RMS one — see
        // [`onset_floor`] for why the two coexist rather than one
        // replacing the other.
        let onset_floor = onset_floor(pre_region);

        // The earliest sample known geometry admits as an onset (pure
        // flight time, converted to a sample index) — only computable
        // when both a measured τ and a recorded distance are present for
        // this capture. `None` otherwise, and `estimate_onset`'s `rule`
        // says so (#346 architect review, option A).
        let min_admissible_index = match (
            &self.interface_latency,
            self.position.as_ref().and_then(|p| p.distance_m),
        ) {
            (Some(InterfaceLatency::Measured(tau)), Some(distance_m)) => {
                let c = crate::shared::conversions::speed_of_sound_from_config(
                    self.position.as_ref().and_then(|p| p.temperature_c),
                );
                let bound_offset = (tau.tau_s + distance_m / c) * *sample_rate_hz as f64;
                Some((centre as f64 + bound_offset).round().max(0.0) as usize)
            }
            _ => None,
        };

        let onset = crate::measurement::sweep::estimate_onset(
            linear_ir,
            peak_index,
            *sample_rate_hz,
            onset_floor,
            min_admissible_index,
        );
        let onset_index = onset.index;
        let onset_rule = onset.rule;

        // The onset's offset from the window centre is the arrival —
        // not `peak_index`'s (#346): on a multi-way loudspeaker the
        // largest sample sits at a fixed group-delay offset past the
        // wavefront that actually left the baffle first.
        let delay_samples = onset_index as i64 - centre as i64;
        let arrival_s = delay_samples as f64 / *sample_rate_hz as f64;
        let (gate_window_s, gate_f_low_hz, gate_window_kind) =
            resolve_gate(payload.gate.as_ref(), window_len, *sample_rate_hz);
        let verdict = ir_verdict(peak_magnitude, pre_region, pre_impulse_snr_db);

        Some(IrStats {
            sample_rate_hz: *sample_rate_hz,
            window_len,
            peak_index,
            peak_magnitude,
            onset_index,
            onset_rule,
            delay_samples,
            arrival_s,
            pre_impulse_snr_db,
            gate_window_s,
            gate_f_low_hz,
            gate_window_kind,
            verdict,
        })
    }
}

/// Index and magnitude of the largest-magnitude sample of a linear IR.
/// Ties keep the earliest index.
pub(super) fn ir_peak(linear_ir: &[f64]) -> (usize, f64) {
    linear_ir
        .iter()
        .enumerate()
        .fold((0usize, 0.0_f64), |acc, (i, &v)| {
            let m = v.abs();
            if m > acc.1 {
                (i, m)
            } else {
                acc
            }
        })
}

/// Pre-impulse noise floor region: everything strictly before the peak,
/// minus a small guard band so the peak's own skirt doesn't bias the
/// floor estimate upward. Empty when the guard band consumes the whole
/// pre-peak window — which [`ir_verdict`] treats as a failure, not as a
/// clean floor.
pub(super) fn pre_impulse_region(linear_ir: &[f64], peak_index: usize) -> &[f64] {
    let guard = (linear_ir.len() / 32).max(8);
    &linear_ir[..peak_index.saturating_sub(guard)]
}

/// `20·log10(peak / rms(pre_region))`. `+inf` for an empty region (nothing
/// to measure) and for a true-silent one (`rms == 0.0`); [`ir_verdict`] is
/// what separates those two cases, since only the first is a failure.
pub(super) fn pre_impulse_snr_db(pre_region: &[f64], peak_magnitude: f64) -> f64 {
    if pre_region.is_empty() {
        return f64::INFINITY;
    }
    let mean_sq = pre_region.iter().map(|v| v * v).sum::<f64>() / pre_region.len() as f64;
    let rms = mean_sq.sqrt();
    if rms > 0.0 {
        20.0 * (peak_magnitude / rms).log10()
    } else {
        f64::INFINITY
    }
}

/// Contamination-robust pre-impulse floor: the median absolute sample of
/// `pre_region`, scaled by the standard MAD-to-σ constant so it targets
/// the same quantity [`pre_impulse_snr_db`]'s RMS floor does on clean
/// noise. `0.0` for an empty region.
///
/// #353 (option A′), retained under #378 with a narrower job. The RMS
/// floor has a breakdown point of zero — a single sample of the peak's own
/// sustained energy bleeding into `pre_region` moves it, by an amount that
/// scales with that sample's amplitude. A rank statistic has a 50%
/// breakdown point: up to half of `pre_region` can be lobe by count and
/// the median still reads the noise floor.
///
/// #378 demoted it from `estimate_onset`'s threshold input to its validity
/// gate — the picker takes no threshold at all, and this floor now decides
/// only whether the search window holds anything above the floor, never
/// where inside it the onset is. Its 50% breakdown point is what makes
/// that gate trustworthy on a contaminated pre-impulse region.
/// [`pre_impulse_snr_db`] and its RMS floor stay exactly as they were —
/// this is a second floor for a second question, not a replacement (#346
/// architect review, #378 AC5).
pub(super) fn onset_floor(pre_region: &[f64]) -> f64 {
    /// Φ⁻¹(0.75) — the standard MAD-to-σ constant, not a tuned value.
    const MAD_TO_SIGMA: f64 = 0.6744897501960817;
    if pre_region.is_empty() {
        return 0.0;
    }
    let mut abs_vals: Vec<f64> = pre_region.iter().map(|v| v.abs()).collect();
    abs_vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = abs_vals.len();
    let median = if n % 2 == 1 {
        abs_vals[n / 2]
    } else {
        (abs_vals[n / 2 - 1] + abs_vals[n / 2]) / 2.0
    };
    median / MAD_TO_SIGMA
}

/// Gate duration, low-frequency limit and window shape for an IR payload.
///
/// Prefers the gate the producer actually applied. #280 stores `f_low_hz`
/// on the payload precisely so a reader does not recompute it; falling
/// back to `window_len / sample_rate_hz` only covers legacy (v1-v3)
/// reports, where no gate was recorded and the rectangular `extract_irs`
/// window is the only gate that could have produced this payload.
pub(super) fn resolve_gate(
    gate: Option<&GateParams>,
    window_len: usize,
    sample_rate_hz: u32,
) -> (f64, f64, String) {
    match gate {
        Some(g) => (g.gate_length_s, g.f_low_hz, g.window_kind.clone()),
        None => {
            let len_s = window_len as f64 / sample_rate_hz as f64;
            (len_s, 1.0 / len_s, "rectangular (not recorded)".to_string())
        }
    }
}

/// Whether a capture's peak is trustworthy enough to present as a result
/// (#376). Split out of [`MeasurementReport::ir_stats`] so this rule —
/// the one `ac-cli` and `ac-scene` both read through
/// [`IrStats::verdict`] — can be exercised directly, without assembling
/// a whole report around it.
///
/// `snr_db` goes to +inf two different ways, and only one of them is a
/// failure. A zero floor against a nonzero peak (`rms == 0.0`,
/// `pre_region` nonempty) is the *best* possible capture — infinite SNR,
/// not an unmeasurable one — and clears any finite threshold below, so it
/// falls through to the ordinary threshold comparison rather than being
/// special-cased out. What fails closed is the case with nothing to
/// measure at all: an empty `pre_region` (the guard band consumed the
/// whole pre-peak window) or a zero peak (nothing captured, so there is
/// no signal to compare a floor against either) — absence of proof of a
/// good floor is not the same as proof of one.
pub(super) fn ir_verdict(peak_magnitude: f64, pre_region: &[f64], snr_db: f64) -> IrVerdict {
    if peak_magnitude == 0.0 {
        IrVerdict::Failed {
            reason: "no signal captured (linear IR is all zero)".to_string(),
        }
    } else if pre_region.is_empty() {
        IrVerdict::Failed {
            reason: "no measurable pre-impulse floor (peak too close to \
                     the start of the gated window)"
                .to_string(),
        }
    } else if snr_db < PRE_IMPULSE_SNR_MIN_DB {
        IrVerdict::Failed {
            reason: "pre-impulse SNR below threshold".to_string(),
        }
    } else {
        IrVerdict::Ok
    }
}

/// See [`MeasurementReport::ir_stats`].
#[derive(Debug, Clone, PartialEq)]
pub struct IrStats {
    pub sample_rate_hz: u32,
    /// Length of the gated linear IR, in samples.
    pub window_len: usize,
    /// Index of the peak-magnitude sample within the gated IR. Kept as a
    /// diagnostic — since #346 this is **not** what `delay_samples` /
    /// `arrival_s` are derived from; on a multi-way loudspeaker the
    /// largest sample sits at a fixed group-delay offset past the actual
    /// wavefront (see [`crate::measurement::sweep::estimate_onset`]).
    pub peak_index: usize,
    /// `|linear_ir[peak_index]|`.
    pub peak_magnitude: f64,
    /// Index of the estimated onset within the gated IR — see
    /// [`crate::measurement::sweep::estimate_onset`]. `delay_samples` and
    /// `arrival_s` are derived from this, not from `peak_index` (#346).
    pub onset_index: usize,
    /// The rule that produced `onset_index`, from
    /// [`crate::measurement::sweep::OnsetEstimate::rule`] — states
    /// whether a causal bound (known geometry) was enforced. Travels with
    /// `arrival_s` so a persisted number can be told apart from a bare
    /// peak read a year later (#346 acceptance criterion 4).
    pub onset_rule: String,
    /// `onset_index - window_len / 2` — signed offset of the estimated
    /// onset from the gate centre, in samples. Positive means the
    /// response arrived after the zero-delay reference position.
    pub delay_samples: i64,
    /// `delay_samples / sample_rate_hz` — arrival time relative to the
    /// gate's zero-delay reference. This is **not** acoustic path delay:
    /// it still contains any uncorrected interface latency, which is why
    /// it must not be converted to a distance without a calibrated τ.
    pub arrival_s: f64,
    /// `20·log10(peak_magnitude / rms(pre-impulse region))`. `+inf` when
    /// no pre-impulse energy was measurable at all (silent floor).
    pub pre_impulse_snr_db: f64,
    /// Gate window duration, in seconds — the recorded
    /// [`GateParams::gate_length_s`] when the payload carries one.
    pub gate_window_s: f64,
    /// The lowest frequency for which one full period fits inside the
    /// gate window. Read from [`GateParams::f_low_hz`] when recorded;
    /// content below it is not reliably resolved by a gate this short.
    pub gate_f_low_hz: f64,
    /// Window shape the gate applied, from [`GateParams::window_kind`].
    /// `"rectangular (not recorded)"` for legacy reports that stored no
    /// gate — an inference from `extract_irs`, flagged as such so a
    /// reader does not mistake it for a recorded value.
    pub gate_window_kind: String,
    /// Whether this capture's peak is trustworthy enough to present as a
    /// result, per [`PRE_IMPULSE_SNR_MIN_DB`] (#376). Computed once here
    /// so `ac-cli`'s text read-out and `ac-scene`'s sweep-IR panel read
    /// the same verdict rather than each re-deriving their own rule from
    /// [`Self::pre_impulse_snr_db`].
    pub verdict: IrVerdict,
}

/// Verdict on whether an [`IrStats`] peak is a trustworthy deconvolution
/// result or noise-floor pickup masquerading as one (#376). `Failed`
/// never carries a computed arrival, distance, or peak-as-result — only
/// the reason, naming what to check without asserting a cause (drive
/// level, mic gain, distance, room noise are all plausible; the
/// instrument cannot tell which).
#[derive(Debug, Clone, PartialEq)]
pub enum IrVerdict {
    Ok,
    Failed { reason: String },
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::*;
    use super::super::*;
    use super::*;

    #[test]
    fn ir_stats_reports_delay_samples_relative_to_gate_centre() {
        // Peak 32 samples after the window centre — the fake backend's
        // fixed loopback delay (see `ac-daemon/src/audio/fake.rs`).
        let window_len = 1024;
        let centre = window_len / 2;
        let r = ir_report_with_peak(window_len, centre + 32, 1.0, 0.0, 48_000);
        let stats = r.ir_stats().expect("impulse response data present");
        assert_eq!(stats.delay_samples, 32);
        assert!((stats.arrival_s - 32.0 / 48_000.0).abs() < 1e-12);
        assert_eq!(stats.peak_index, centre + 32);
        assert_eq!(stats.peak_magnitude, 1.0);
    }

    #[test]
    fn ir_stats_delay_is_negative_when_peak_precedes_centre() {
        let window_len = 1024;
        let centre = window_len / 2;
        let r = ir_report_with_peak(window_len, centre - 10, 1.0, 0.0, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(stats.delay_samples, -10);
        assert!(stats.arrival_s < 0.0);
    }

    #[test]
    fn ir_stats_pre_impulse_snr_reflects_noise_floor() {
        let window_len = 1024;
        let centre = window_len / 2;
        // Peak of 1.0 against a 0.01 floor -> 20*log10(100) = 40 dB.
        let r = ir_report_with_peak(window_len, centre, 1.0, 0.01, 48_000);
        let stats = r.ir_stats().unwrap();
        assert!(
            (stats.pre_impulse_snr_db - 40.0).abs() < 0.5,
            "pre_impulse_snr_db = {}",
            stats.pre_impulse_snr_db
        );
    }

    #[test]
    fn ir_stats_snr_is_infinite_over_true_silence() {
        let window_len = 1024;
        let centre = window_len / 2;
        let r = ir_report_with_peak(window_len, centre, 1.0, 0.0, 48_000);
        let stats = r.ir_stats().unwrap();
        assert!(stats.pre_impulse_snr_db.is_infinite());
    }

    #[test]
    fn ir_stats_falls_back_to_window_duration_when_no_gate_recorded() {
        let r = ir_report_with_peak(4_800, 2_400, 1.0, 0.0, 48_000);
        let stats = r.ir_stats().unwrap();
        // 4800 samples @ 48 kHz = 100 ms window -> f_low = 10 Hz.
        assert!((stats.gate_window_s - 0.1).abs() < 1e-12);
        assert!((stats.gate_f_low_hz - 10.0).abs() < 1e-9);
        // A legacy report's gate is inferred, and must say so — a reader
        // must not take "rectangular" here for a recorded fact.
        assert!(
            stats.gate_window_kind.contains("not recorded"),
            "inferred gate must be flagged: {}",
            stats.gate_window_kind
        );
    }

    /// The recorded gate wins over the `window_len / sample_rate` guess.
    /// This is the case that separates the two: a gate whose recorded
    /// `f_low_hz` and length disagree with what the IR length implies
    /// (a half-length gate on a zero-padded payload) — if `ir_stats`
    /// recomputed instead of reading, it would report 10 Hz, not 20.
    #[test]
    fn ir_stats_prefers_the_recorded_gate_over_the_ir_length() {
        let mut r = ir_report_with_peak(4_800, 2_400, 1.0, 0.0, 48_000);
        r.data[0].gate = Some(GateParams {
            gate_start_s: 0.0,
            gate_length_s: 0.05,
            window_kind: "half-hann".into(),
            f_low_hz: 20.0,
        });
        let stats = r.ir_stats().unwrap();
        assert!((stats.gate_window_s - 0.05).abs() < 1e-12);
        assert!((stats.gate_f_low_hz - 20.0).abs() < 1e-9);
        assert_eq!(stats.gate_window_kind, "half-hann");
    }

    /// [`ir_verdict`] direct, without a report around it: the threshold is
    /// a `<` on `PRE_IMPULSE_SNR_MIN_DB`, so a capture sitting exactly on
    /// the floor passes and one a hair under it fails.
    #[test]
    fn ir_verdict_threshold_is_inclusive_at_the_floor() {
        let floor = [0.0, 0.0, 0.0, 0.0];
        assert_eq!(
            ir_verdict(1.0, &floor, PRE_IMPULSE_SNR_MIN_DB),
            IrVerdict::Ok
        );
        assert!(matches!(
            ir_verdict(1.0, &floor, PRE_IMPULSE_SNR_MIN_DB - 0.001),
            IrVerdict::Failed { .. }
        ));
    }

    /// The two ways `snr_db` reaches `+inf` are not the same verdict: a
    /// silent floor under a real peak is the best possible capture, while
    /// an empty pre-impulse region means nothing was measured at all.
    #[test]
    fn ir_verdict_separates_a_silent_floor_from_an_unmeasured_one() {
        assert_eq!(ir_verdict(1.0, &[0.0, 0.0], f64::INFINITY), IrVerdict::Ok);
        assert!(matches!(
            ir_verdict(1.0, &[], f64::INFINITY),
            IrVerdict::Failed { .. }
        ));
    }

    /// A zero peak fails ahead of every other branch — an all-zero IR has
    /// no signal to compare a floor against, however clean the floor looks.
    #[test]
    fn ir_verdict_fails_a_zero_peak_before_reading_the_snr() {
        assert!(matches!(
            ir_verdict(0.0, &[0.0, 0.0], f64::INFINITY),
            IrVerdict::Failed { .. }
        ));
    }

    #[test]
    fn ir_stats_verdict_ok_when_snr_clears_the_threshold() {
        let window_len = 1024;
        let centre = window_len / 2;
        // Peak 1.0 against a 0.1 floor -> 20*log10(10) = 20 dB, above the
        // 18.0 dB threshold.
        let r = ir_report_with_peak(window_len, centre, 1.0, 0.1, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(stats.verdict, IrVerdict::Ok);
    }

    #[test]
    fn ir_stats_verdict_failed_when_snr_is_below_the_threshold() {
        let window_len = 1024;
        let centre = window_len / 2;
        // Peak 1.0 against a 0.2 floor -> 20*log10(5) \u{2248} 14.0 dB,
        // below the 18.0 dB threshold — the #376 failure shape: a plausible
        // number, but a noise-floor-scale peak.
        let r = ir_report_with_peak(window_len, centre, 1.0, 0.2, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(
            stats.verdict,
            IrVerdict::Failed {
                reason: "pre-impulse SNR below threshold".to_string()
            }
        );
    }

    #[test]
    fn ir_stats_verdict_ok_on_a_perfectly_clean_capture() {
        // A zero floor against a nonzero peak is +inf SNR, but it is the
        // *best* possible capture, not an unmeasurable one — the floor was
        // measured, and it measured to exactly zero. This must not be
        // confused with a genuine failure (#387 QA correctness #1).
        let window_len = 1024;
        let centre = window_len / 2;
        let r = ir_report_with_peak(window_len, centre, 1.0, 0.0, 48_000);
        let stats = r.ir_stats().unwrap();
        assert!(stats.pre_impulse_snr_db.is_infinite());
        assert_eq!(stats.verdict, IrVerdict::Ok);
    }

    #[test]
    fn ir_stats_verdict_failed_when_nothing_was_captured() {
        // Peak magnitude itself is zero -> the whole linear IR is zero,
        // i.e. there is no signal to compare a floor against at all. This
        // is the genuine "no measurable floor" failure, distinct from the
        // clean-capture case above.
        let window_len = 1024;
        let r = ir_report_with_peak(window_len, window_len / 2, 0.0, 0.0, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(
            stats.verdict,
            IrVerdict::Failed {
                reason: "no signal captured (linear IR is all zero)".to_string()
            }
        );
    }

    #[test]
    fn ir_stats_verdict_failed_when_guard_band_consumes_the_whole_pre_region() {
        // Peak sits inside the guard band from the start of the window, so
        // `pre_region` is empty — there is no data at all to measure a
        // floor from, regardless of what the peak itself looks like.
        let window_len = 1024;
        let r = ir_report_with_peak(window_len, 3, 1.0, 0.1, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(
            stats.verdict,
            IrVerdict::Failed {
                reason: "no measurable pre-impulse floor (peak too close to \
                         the start of the gated window)"
                    .to_string()
            }
        );
    }

    #[test]
    fn ir_stats_none_for_non_impulse_response_report() {
        let r = sample_report(); // FrequencyResponse variant
        assert!(r.ir_stats().is_none());
    }

    /// A Farina capture emits several payloads; the impulse response is
    /// not necessarily first. `ir_stats` must find it rather than read
    /// `data[0]` and give up.
    #[test]
    fn ir_stats_finds_the_ir_payload_behind_another_payload() {
        let mut r = ir_report_with_peak(1_024, 1_024 / 2 + 32, 1.0, 0.0, 48_000);
        let ir_payload = r.data.remove(0);
        r.data = vec![
            MeasurementPayload {
                data: MeasurementData::FrequencyResponse { points: vec![] },
                standard: Vec::new(),
                gate: None,
            },
            ir_payload,
        ];
        assert_eq!(r.ir_stats().unwrap().delay_samples, 32);
    }

    #[test]
    fn ir_stats_none_for_empty_linear_ir() {
        let mut r = sample_impulse_response_report();
        r.data = vec![MeasurementPayload {
            data: MeasurementData::ImpulseResponse {
                sample_rate_hz: 48_000,
                f1_hz: 20.0,
                f2_hz: 20_000.0,
                duration_s: 1.0,
                linear_ir: vec![],
                noise_tail_start_s: None,
                harmonics: vec![],
            },
            standard: Vec::new(),
            gate: None,
        }];
        assert!(r.ir_stats().is_none());
    }

    // ─── interface latency (τ), archived alongside the arrival ─────────

    /// #346: `delay_samples`/`arrival_s` must be onset-derived, not
    /// peak-derived. Test against the rejected implementation — a
    /// synthetic multi-way-like IR where sustained onset energy sits well
    /// before a group-delay-inflated peak, with the peak position
    /// computed here directly (not just asserted "close to the true
    /// arrival", which the old behaviour would also pass).
    #[test]
    fn ir_stats_arrival_is_onset_derived_not_peak_derived() {
        let window_len = 1024;
        let centre = window_len / 2;
        let noise = 0.001;
        let peak_true = centre + 100;
        let onset_true = peak_true - 20; // within the guard band before the peak
        let mut ir: Vec<f64> = vec![noise; window_len];
        for v in ir.iter_mut().take(peak_true + 1).skip(onset_true) {
            *v = 0.3;
        }
        ir[peak_true] = 1.0;

        let peak_index = ir
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap()
            .0;
        assert_eq!(
            peak_index, peak_true,
            "test setup: peak must be at peak_true"
        );

        let r = ir_report_with_custom_ir(ir, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(stats.peak_index, peak_true, "peak_index stays diagnostic");
        assert_eq!(stats.onset_index, onset_true);
        assert_ne!(
            stats.delay_samples,
            peak_true as i64 - centre as i64,
            "arrival must not be the peak-derived delay — #346"
        );
        assert_eq!(stats.delay_samples, onset_true as i64 - centre as i64);
        assert!(stats.onset_rule.contains("no causal bound"));
    }

    /// #346: when a report carries both a measured interface latency and
    /// a recorded `position.distance_m`, `ir_stats` must convert them
    /// into a causal bound and enforce it — proven with a capture whose
    /// unbounded answer is non-causal, so the bound actively changes the
    /// result rather than agreeing with it by coincidence.
    ///
    /// #378 changed how: the bound is the search window's lower limit,
    /// so the non-causal candidate is outside the picker's reach rather
    /// than being clamped after the fact. What is asserted is the same
    /// requirement — a bounded capture never reads earlier than pure
    /// flight time allows — expressed against a window instead of a
    /// walk.
    #[test]
    fn ir_stats_wires_a_causal_bound_from_position_and_interface_latency() {
        let window_len = 1024;
        let sr = 48_000u32;
        let centre = window_len / 2;
        let peak_true = centre + 100;
        let bound_index = peak_true - 12;
        let wavefront = peak_true - 7; // inside the admissible window
        let pre_ring = peak_true - 52; // below it — non-causal
        let mut ir: Vec<f64> = (0..window_len)
            .map(|i| {
                let mut s = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                s ^= s >> 29;
                ((s >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 2e-4
            })
            .collect();
        for (i, v) in ir.iter_mut().enumerate() {
            if (pre_ring..wavefront).contains(&i) {
                *v += 0.02 * ((i - pre_ring) as f64 * 0.7).sin();
            } else if (wavefront..=peak_true).contains(&i) {
                *v += 0.3 * (i - wavefront + 1) as f64 / 8.0;
            }
        }
        ir[peak_true] = 1.0;

        let mut r = ir_report_with_custom_ir(ir, sr);
        let unbounded = r.ir_stats().unwrap();
        assert!(
            unbounded.onset_index < bound_index,
            "test setup: the unbounded pick must be non-causal, got {}",
            unbounded.onset_index
        );
        assert!(unbounded.onset_rule.contains("no causal bound"));

        let bound_offset_samples = (bound_index - centre) as f64;
        let c = crate::shared::conversions::speed_of_sound_from_config(Some(20.0));
        r.position = Some(PositionSnapshot {
            temperature_c: Some(20.0),
            distance_m: Some(bound_offset_samples / sr as f64 * c),
            ..Default::default()
        });
        r.interface_latency = Some(measured_tau(0.0));

        let bounded = r.ir_stats().unwrap();
        assert!(
            bounded.onset_index >= bound_index,
            "causal bound must exclude the non-causal candidate, got {}",
            bounded.onset_index
        );
        assert_ne!(bounded.onset_index, unbounded.onset_index);
        assert!(bounded.onset_rule.contains("causal bound enforced"));
        assert!(bounded
            .onset_rule
            .contains(&format!("window start at sample {bound_index}")));
    }

    /// #346 (QA on #352, correctness issue 2) / #353: `floor_rms`'s guard
    /// band (`(window_len / 32).max(8)`, pre-existing) is sized off
    /// `window_len` alone — nothing ties it to how wide the peak's own
    /// sustained-energy run actually is. When that run is wider than the
    /// guard, it bleeds into `pre_region` and inflates `floor_rms`. Before
    /// #353, `estimate_onset` thresholded directly off that inflated
    /// value and the backward search stopped at the peak instead of the
    /// true onset — silently reproducing the very peak-as-arrival bug
    /// #346 exists to fix, for a signal shape the guard band was not
    /// sized for.
    ///
    /// #353 (architect revision 2, option A′) fixes this: `estimate_onset`
    /// now thresholds against a median-based floor over the same
    /// `pre_region`, which a lobe this size (contaminating 52/152 ≈ 34% of
    /// `pre_region`, comfortably under the estimator's 50% breakdown
    /// point) does not move. This test — a small window (guard = 8,
    /// `window_len` = 256) with a sustained run wide enough to defeat the
    /// old RMS floor — now asserts the corrected behaviour.
    #[test]
    fn ir_stats_onset_threshold_is_coupled_to_the_guard_band_a_wide_lobe_can_break() {
        let window_len = 256;
        let sr = 48_000u32;
        let noise = 0.001;
        let peak_true = 160;
        let onset_true = 100; // 60 samples wide — wider than guard = 8
        let mut ir = vec![noise; window_len];
        for v in ir.iter_mut().take(peak_true + 1).skip(onset_true) {
            *v = 0.3;
        }
        ir[peak_true] = 1.0;

        let r = ir_report_with_custom_ir(ir, sr);
        let stats = r.ir_stats().unwrap();
        // The median floor (#353) is not moved by this lobe: the backward
        // walk now reaches the true onset instead of stopping at the peak.
        assert_ne!(
            stats.onset_index, peak_true,
            "if this now equals peak_true, the median-floor fix (#353) has \
             regressed to the old RMS-floor behaviour — update this test's \
             assertions and its doc comment"
        );
        assert_eq!(
            stats.onset_index, onset_true,
            "median floor (#353) has a 50% breakdown point; this lobe \
             contaminates only ~34% of pre_region, well under it, so the \
             backward walk must reach the true onset"
        );
    }

    /// The rule #378 rejects, computed inline so these tests measure it
    /// rather than asserting "the new answer is closer to truth": a
    /// threshold 12 dB above the supplied pre-impulse floor, walked
    /// backward from the peak. Nothing in `ac-core` implements this any
    /// more — it exists only here, as the comparison.
    fn rejected_level_crossing_rule(ir: &[f64], peak_index: usize, floor: f64) -> usize {
        let threshold = floor * 10f64.powf(12.0 / 20.0);
        let mut onset = peak_index;
        while onset > 0 && ir[onset - 1].abs() > threshold {
            onset -= 1;
        }
        onset
    }

    /// #353's median floor, recomputed here over an arbitrary region so
    /// the rejected rule can be driven by either floor.
    fn median_abs_over_phi_inv(region: &[f64]) -> f64 {
        if region.is_empty() {
            return 0.0;
        }
        let mut abs_vals: Vec<f64> = region.iter().map(|v| v.abs()).collect();
        abs_vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = abs_vals.len();
        let median = if n % 2 == 1 {
            abs_vals[n / 2]
        } else {
            (abs_vals[n / 2 - 1] + abs_vals[n / 2]) / 2.0
        };
        median / 0.674_489_750_196_081_7
    }

    /// #353 acceptance criterion 3, carried into #378: names the input
    /// that makes the *rejected* level-crossing rule go red under both
    /// the RMS floor and the median floor (option A′), per the
    /// architect's revision-2 probe table (2026-08-23) — bracketing the
    /// crossings, not locating them to the sample, exactly as that
    /// comment states. All cases share `window_len = 256`,
    /// `peak_true = 160`, `guard = 8` (so `pre_end = 152`); `width` is
    /// `peak_true - onset_true`, i.e. how far the sustained run reaches
    /// back from the peak.
    ///
    /// Both rejected rules are computed inline against each input rather
    /// than trusted from an earlier revision (`test-against-the-rejected-
    /// implementation`). #353's own evidence is kept intact — the RMS
    /// floor breaks first, the median floor survives to its 50%
    /// breakdown point — and #378's claim is added on top: the shipped
    /// picker takes no threshold from either floor, so contamination
    /// that moves both of them does not move its answer at all.
    #[test]
    fn ir_stats_onset_floor_breakdown_point_matches_the_measured_table() {
        let window_len = 256;
        let sr = 48_000u32;
        let noise = 0.001;
        let peak_true = 160;
        let guard = (window_len / 32).max(8); // 8

        // (lobe width, RMS-floor level rule reaches onset_true?,
        //  median-floor level rule reaches onset_true?)
        let cases = [
            (16, true, true),   // RMS: 5% contaminated, green
            (18, false, true),  // RMS: 6.6% contaminated, red; median: green
            (80, false, true),  // RMS: red; median: 47% contaminated, green
            (90, false, false), // median: 54% contaminated, past 50% — red
        ];

        for (width, rms_reaches_onset, median_reaches_onset) in cases {
            let onset_true = peak_true - width;
            let mut ir = vec![noise; window_len];
            for v in ir.iter_mut().take(peak_true + 1).skip(onset_true) {
                *v = 0.3;
            }
            ir[peak_true] = 1.0;

            let pre_end = peak_true.saturating_sub(guard);
            let pre_region = &ir[..pre_end];
            let rms_floor = {
                let mean_sq =
                    pre_region.iter().map(|v| v * v).sum::<f64>() / pre_region.len() as f64;
                mean_sq.sqrt()
            };
            let median_floor = median_abs_over_phi_inv(pre_region);

            assert_eq!(
                rejected_level_crossing_rule(&ir, peak_true, rms_floor) == onset_true,
                rms_reaches_onset,
                "width {width}: RMS-floor level rule onset = {}, expected \
                 reaches_onset={rms_reaches_onset}",
                rejected_level_crossing_rule(&ir, peak_true, rms_floor)
            );
            assert_eq!(
                rejected_level_crossing_rule(&ir, peak_true, median_floor) == onset_true,
                median_reaches_onset,
                "width {width}: median-floor level rule onset = {}, expected \
                 reaches_onset={median_reaches_onset}",
                rejected_level_crossing_rule(&ir, peak_true, median_floor)
            );

            let r = ir_report_with_custom_ir(ir, sr);
            let stats = r.ir_stats().unwrap();
            assert_eq!(
                stats.onset_index, onset_true,
                "width {width}: #378's picker takes no threshold from either \
                 floor, so guard-band contamination that moves both rejected \
                 rules must not move it"
            );
        }
    }

    /// #353 acceptance criterion 5, carried into #378: the guard stays
    /// fixed and content-independent (option A′ added a distribution
    /// property, not a second constant coupled to the first) — asserted
    /// as an estimator-vs-estimator agreement rather than a hidden
    /// tolerance between the two floors, per the architect's own
    /// instruction ("do not convert that into a dB tolerance on the two
    /// floors; that number has not been measured and would ship as
    /// `assumed`"). On a clean pre-impulse region the RMS floor and the
    /// median floor must send the rejected level rule to the *same*
    /// onset index — checked over many seeded noise realisations so this
    /// is a statement, not a coincidence (architect's own probe: 200
    /// seeds, `{0: 200}`).
    ///
    /// #378 adds the stronger claim on the same 200 captures: the
    /// shipped picker's answer is identical whichever floor is handed to
    /// it, because the floor is a validity gate now and not a threshold.
    #[test]
    fn ir_stats_onset_floor_agrees_with_rms_floor_on_clean_pre_impulse_noise() {
        use rand::rngs::StdRng;
        use rand::{Rng, SeedableRng};

        let window_len: usize = 1024;
        let sr = 48_000u32;
        let peak_true: usize = 700;
        let onset_true = peak_true - 20; // within the guard band before the peak
        let guard = (window_len / 32).max(8);
        assert!(
            onset_true >= peak_true.saturating_sub(guard),
            "test setup: run must stay inside the guard band so both floors \
             see clean noise only"
        );

        for seed in 0u64..200 {
            let mut rng = StdRng::seed_from_u64(0xB353_0000 + seed);
            let mut ir: Vec<f64> = (0..window_len)
                .map(|_| (rng.gen::<f64>() * 2.0 - 1.0) * 0.001)
                .collect();
            for v in ir.iter_mut().take(peak_true + 1).skip(onset_true) {
                *v = 0.3;
            }
            ir[peak_true] = 1.0;

            let pre_end = peak_true.saturating_sub(guard);
            let pre_region = &ir[..pre_end];
            let rms_floor = {
                let mean_sq =
                    pre_region.iter().map(|v| v * v).sum::<f64>() / pre_region.len() as f64;
                mean_sq.sqrt()
            };
            let median_floor = median_abs_over_phi_inv(pre_region);

            assert_eq!(
                rejected_level_crossing_rule(&ir, peak_true, rms_floor),
                rejected_level_crossing_rule(&ir, peak_true, median_floor),
                "seed {seed}: median floor (#353) must agree with the RMS \
                 floor on an uncontaminated pre-impulse region"
            );

            let picked_with_rms =
                crate::measurement::sweep::estimate_onset(&ir, peak_true, sr, rms_floor, None);
            let r = ir_report_with_custom_ir(ir, sr);
            let stats = r.ir_stats().unwrap();
            assert_eq!(
                stats.onset_index, picked_with_rms.index,
                "seed {seed}: #378's pick must not depend on which floor \
                 reaches the validity gate"
            );
        }
    }
}
