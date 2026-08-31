//! Derived read-out quantities for an impulse-response payload, and the
//! verdict on whether its peak is a trustworthy deconvolution result
//! (#376). Computed once here so `ac-cli`'s text read-out and
//! `ac-scene`'s sweep-IR panel cannot disagree about a capture.

use super::{GateParams, MeasurementData, MeasurementReport};

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
        // would peak at — so the peak's offset from the window centre
        // *is* the measured round-trip delay in samples.
        let centre = window_len / 2;
        let delay_samples = peak_index as i64 - centre as i64;
        let arrival_s = delay_samples as f64 / *sample_rate_hz as f64;

        let pre_region = pre_impulse_region(linear_ir, peak_index);
        let pre_impulse_snr_db = pre_impulse_snr_db(pre_region, peak_magnitude);
        let (gate_window_s, gate_f_low_hz, gate_window_kind) =
            resolve_gate(payload.gate.as_ref(), window_len, *sample_rate_hz);
        let verdict = ir_verdict(peak_magnitude, pre_region, pre_impulse_snr_db);

        Some(IrStats {
            sample_rate_hz: *sample_rate_hz,
            window_len,
            peak_index,
            peak_magnitude,
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
    /// Index of the peak-magnitude sample within the gated IR.
    pub peak_index: usize,
    /// `|linear_ir[peak_index]|`.
    pub peak_magnitude: f64,
    /// `peak_index - window_len / 2` — signed offset of the peak from the
    /// gate centre, in samples. Positive means the response arrived after
    /// the zero-delay reference position.
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
}
