//! Sample reports shared by the test modules under `report`. They live
//! in one module because several groups build the same shapes — a report
//! with an IR payload is needed by `ir_stats`, by `csv`, and by the
//! schema round-trips — and a second copy of one is a second thing to
//! keep in step with `SCHEMA_VERSION`.

use super::*;

/// A calibration snapshot whose voltage layer the session check refused
/// (#466): the scale is withheld, the verdict says why.
pub(super) fn refused_calibration() -> CalibrationSnapshot {
    use crate::shared::calibration::session::{CheckSource, Evidence, VerdictUnit};
    CalibrationSnapshot {
        output_channel: 1,
        input_channel: 1,
        vrms_at_0dbfs_out: None,
        vrms_at_0dbfs_in: None,
        ref_freq_hz: 1000.0,
        ref_level_dbfs: -40.0,
        mic_sensitivity_dbfs_at_94db_spl: None,
        mic_response: None,
        voltage_check: Some(crate::shared::calibration::LayerVerdict::Refused {
            evidence: Evidence {
                measured: 2.42,
                stored: -0.60,
                delta: 3.02,
                tolerance: 0.10,
                unit: VerdictUnit::Db,
                stored_at: "2026-09-15T23:43:04Z".into(),
                checked_at: "2026-09-16T14:02:11.000Z".into(),
                source: CheckSource::Probe,
            },
            via: None,
            delta_bound: None,
        }),
    }
}

pub(super) fn sample_report() -> MeasurementReport {
    MeasurementReport {
        schema_version: SCHEMA_VERSION,
        ac_version: "0.1.0".into(),
        timestamp_utc: "2026-04-21T20:00:00Z".into(),
        backend: Some("fake".into()),
        method: MeasurementMethod::SteppedSine { n_points: 3 },
        stimulus: StimulusParams {
            sample_rate_hz: 48_000,
            f_start_hz: 100.0,
            f_stop_hz: 10_000.0,
            level_dbfs: -20.0,
            n_points: 3,
        },
        integration: IntegrationParams {
            duration_s: 1.0,
            window: "hann".into(),
            n_averages: None,
        },
        calibration: None,
        position: None,
        interface_latency: None,
        reference_latency: None,
        reference_stored_latency: None,
        inter_pair_offset: None,
        tail_decay: None,
        data: vec![MeasurementPayload {
            data: MeasurementData::FrequencyResponse {
                points: vec![
                    FrequencyResponsePoint {
                        freq_hz: 100.0,
                        fundamental_dbfs: -20.1,
                        thd_pct: 0.005,
                        thdn_pct: 0.012,
                        noise_floor_dbfs: -120.0,
                        linear_rms: 0.0707,
                        clipping: false,
                        ac_coupled: false,
                    },
                    FrequencyResponsePoint {
                        freq_hz: 1_000.0,
                        fundamental_dbfs: -20.05,
                        thd_pct: 0.003,
                        thdn_pct: 0.009,
                        noise_floor_dbfs: -121.3,
                        linear_rms: 0.0707,
                        clipping: false,
                        ac_coupled: false,
                    },
                    FrequencyResponsePoint {
                        freq_hz: 10_000.0,
                        fundamental_dbfs: -20.2,
                        thd_pct: 0.008,
                        thdn_pct: 0.015,
                        noise_floor_dbfs: -119.5,
                        linear_rms: 0.0706,
                        clipping: false,
                        ac_coupled: false,
                    },
                ],
            },
            standard: vec![crate::measurement::thd::citation()],
            gate: None,
        }],
        notes: None,
        processing_chain: ProcessingChain::default(),
    }
}

pub(super) fn sample_spectrum_bands_report() -> MeasurementReport {
    MeasurementReport {
        schema_version: SCHEMA_VERSION,
        ac_version: "0.1.0".into(),
        timestamp_utc: "2026-04-22T12:00:00Z".into(),
        backend: None,
        method: MeasurementMethod::SteppedSine { n_points: 0 },
        stimulus: StimulusParams {
            sample_rate_hz: 48_000,
            f_start_hz: 100.0,
            f_stop_hz: 1000.0,
            level_dbfs: -20.0,
            n_points: 0,
        },
        integration: IntegrationParams {
            duration_s: 1.0,
            window: "none".into(),
            n_averages: None,
        },
        calibration: None,
        position: None,
        interface_latency: None,
        reference_latency: None,
        reference_stored_latency: None,
        inter_pair_offset: None,
        tail_decay: None,
        data: vec![MeasurementPayload {
            data: MeasurementData::SpectrumBands {
                bpo: 3,
                class: "Class 1".into(),
                centres_hz: vec![100.0, 125.893, 158.489],
                levels_dbfs: vec![-30.0, -20.0, -40.0],
            },
            standard: vec![crate::measurement::filterbank::Filterbank::citation()],
            gate: None,
        }],
        notes: None,
        processing_chain: ProcessingChain::default(),
    }
}

pub(super) fn sample_impulse_response_report() -> MeasurementReport {
    use crate::measurement::sweep::HarmonicIr;
    MeasurementReport {
        schema_version: SCHEMA_VERSION,
        ac_version: "0.1.0".into(),
        timestamp_utc: "2026-04-22T12:00:00Z".into(),
        backend: None,
        method: MeasurementMethod::SweptSine {
            f1_hz: 20.0,
            f2_hz: 20_000.0,
            duration_s: 1.0,
        },
        stimulus: StimulusParams {
            sample_rate_hz: 48_000,
            f_start_hz: 20.0,
            f_stop_hz: 20_000.0,
            level_dbfs: -6.0,
            n_points: 0,
        },
        integration: IntegrationParams {
            duration_s: 1.0,
            window: "none".into(),
            n_averages: None,
        },
        calibration: None,
        position: None,
        interface_latency: None,
        reference_latency: None,
        reference_stored_latency: None,
        inter_pair_offset: None,
        tail_decay: None,
        data: vec![MeasurementPayload {
            data: MeasurementData::ImpulseResponse {
                sample_rate_hz: 48_000,
                f1_hz: 20.0,
                f2_hz: 20_000.0,
                duration_s: 1.0,
                linear_ir: vec![0.0, 0.5, 1.0, 0.25, 0.0],
                noise_tail_start_s: None,
                harmonics: vec![HarmonicIr {
                    order: 2,
                    samples: vec![0.0, 0.1, 0.2, 0.05, 0.0],
                }],
            },
            standard: vec![crate::measurement::sweep::citation()],
            gate: None,
        }],
        notes: None,
        processing_chain: ProcessingChain::default(),
    }
}

/// Build an IR report with `window_len` samples, an impulse of
/// `peak_mag` at `peak_index`, and `noise` amplitude everywhere else
/// — enough signal shape to exercise `ir_stats` deterministically.
/// Carries no `gate`, so it also covers the legacy fallback path.
pub(super) fn ir_report_with_peak(
    window_len: usize,
    peak_index: usize,
    peak_mag: f64,
    noise: f64,
    sample_rate_hz: u32,
) -> MeasurementReport {
    let mut r = sample_impulse_response_report();
    let mut ir = vec![noise; window_len];
    ir[peak_index] = peak_mag;
    r.data = vec![MeasurementPayload {
        data: MeasurementData::ImpulseResponse {
            sample_rate_hz,
            f1_hz: 20.0,
            f2_hz: 20_000.0,
            duration_s: 1.0,
            linear_ir: ir,
            noise_tail_start_s: None,
            harmonics: vec![],
        },
        standard: Vec::new(),
        gate: None,
    }];
    r
}

/// Companion to [`ir_report_with_peak`] for tests (#346) that need a
/// hand-built `linear_ir` shape rather than a single spike over flat
/// noise — e.g. sustained onset energy distinct from the peak.
pub(super) fn ir_report_with_custom_ir(
    linear_ir: Vec<f64>,
    sample_rate_hz: u32,
) -> MeasurementReport {
    ir_report_with_custom_ir_band(linear_ir, sample_rate_hz, 20_000.0)
}

/// [`ir_report_with_custom_ir`] with the payload's sweep top `f2_hz` set —
/// the band-limited arrival (#537) reads it to decide whether the band
/// reaches an octave above its corner.
pub(super) fn ir_report_with_custom_ir_band(
    linear_ir: Vec<f64>,
    sample_rate_hz: u32,
    f2_hz: f64,
) -> MeasurementReport {
    let mut r = sample_impulse_response_report();
    r.data = vec![MeasurementPayload {
        data: MeasurementData::ImpulseResponse {
            sample_rate_hz,
            f1_hz: 20.0,
            f2_hz,
            duration_s: 1.0,
            linear_ir,
            noise_tail_start_s: None,
            harmonics: vec![],
        },
        standard: Vec::new(),
        gate: None,
    }];
    r
}

/// White noise, uniform in `±amplitude`, from a fixed hash of the index and
/// `seed` so a fixture is reproducible.
pub(super) fn hashed_uniform_noise(len: usize, amplitude: f64, seed: u64) -> Vec<f64> {
    (0..len as u64)
        .map(|i| {
            let mut s =
                (i ^ seed.wrapping_mul(0xD1B5_4A32_D192_ED03)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            s ^= s >> 31;
            s = s.wrapping_mul(0xBF58_476D_1CE4_E5B9);
            s ^= s >> 29;
            let u = (s >> 11) as f64 / (1u64 << 53) as f64;
            (2.0 * u - 1.0) * amplitude
        })
        .collect()
}

/// #537's acceptance case, at 96 kHz with a 20 Hz–20 kHz payload band and
/// a 0.4 s window (the `plot_ir` default length): a direct broadband
/// impulse at `t0` (2 m of flight after the gate centre) and a 55 Hz room
/// mode whose first large swing, `mode_peak`, lands 15 ms after it at
/// 21.9 dB above the direct sound. The mode builds up over 10 ms under a
/// raised-cosine envelope, so its onset carries no edge the high-pass could
/// read as an arrival, and decays with a 50 ms time constant. The envelope
/// is flat and the cosine at its crest on `mode_peak`, so the broadband
/// maximum sits exactly there. The noise floor (±1e-7) is too small to move
/// that maximum by a sample.
pub(super) struct RoomModeCapture {
    pub(super) report: MeasurementReport,
    pub(super) t0: usize,
    pub(super) mode_peak: usize,
}

pub(super) fn direct_plus_room_mode_report() -> RoomModeCapture {
    const SR: u32 = 96_000;
    const WINDOW_LEN: usize = 38_400;
    const DIRECT: f64 = 0.08;
    const MODE: f64 = 1.0;
    const MODE_HZ: f64 = 55.0;
    const RISE_S: f64 = 0.010;
    const DECAY_S: f64 = 0.050;
    let sr = SR as f64;
    let centre = WINDOW_LEN / 2;
    let t0 = centre + 598;
    let mode_peak = t0 + (0.015 * sr) as usize;
    let mut ir = hashed_uniform_noise(WINDOW_LEN, 1e-7, 537);
    ir[t0] += DIRECT;
    for (n, v) in ir.iter_mut().enumerate() {
        let t = (n as f64 - mode_peak as f64) / sr;
        let envelope = if t < -RISE_S {
            0.0
        } else if t <= 0.0 {
            0.5 * (1.0 - (std::f64::consts::PI * (t + RISE_S) / RISE_S).cos())
        } else {
            (-t / DECAY_S).exp()
        };
        *v += MODE * envelope * (2.0 * std::f64::consts::PI * MODE_HZ * t).cos();
    }
    RoomModeCapture {
        report: ir_report_with_custom_ir(ir, SR),
        t0,
        mode_peak,
    }
}

/// A stored τ for the capture pair, resolved in the same enumeration epoch
/// as the capture (#461) — the ordinary, unflagged case.
pub(super) fn measured_tau(tau_s: f64) -> InterfaceLatency {
    measured_tau_with_check(
        tau_s,
        Some(crate::shared::calibration::EnumerationCheck::Same),
    )
}

/// [`measured_tau`] with an explicit frozen enumeration check; `None` is the
/// v9 shape.
pub(super) fn measured_tau_with_check(
    tau_s: f64,
    enumeration: Option<crate::shared::calibration::EnumerationCheck>,
) -> InterfaceLatency {
    InterfaceLatency::Measured(MeasuredLatency {
        tau_s,
        measured_at: "2026-08-15T00:00:00Z".into(),
        method: "farina_short_ess".into(),
        backend: "fake".into(),
        sample_rate_hz: 48_000,
        period_size: Some(1024),
        output_port: "out1".into(),
        input_port: "in1".into(),
        enumeration,
        session_check: None,
        session_check_loopback: None,
    })
}

/// Give `r` a live latency basis of `tau_s` (#544): a same-capture
/// reference reading of `tau_s` and the capture pair being the reference
/// pair, so the flight time is `arrival − tau_s`. What the pre-#544
/// fixtures got from a stored `interface_latency`.
pub(super) fn with_live_latency(r: &mut MeasurementReport, tau_s: f64) {
    r.reference_latency = Some(measured_reference(tau_s));
    r.inter_pair_offset = Some(InterPairOffset::Identity);
}

/// A measured inter-pair offset of `offset_s` (#544) for [`measured_tau`]'s
/// capture pair against [`measured_reference`]'s ports, measured in the
/// capture's own enumeration epoch.
pub(super) fn measured_offset(offset_s: f64) -> InterPairOffset {
    InterPairOffset::Measured(MeasuredInterPairOffset {
        offset_s,
        tau_s: 0.0178 + offset_s,
        reference_tau_s: 0.0178,
        measured_at: "2026-09-21T16:05:40Z".into(),
        output_port: "out1".into(),
        input_port: "in1".into(),
        reference_output_port: "ref_out".into(),
        reference_input_port: "ref_in".into(),
        sample_rate_hz: 48_000,
        period_size: Some(1024),
        enumeration: crate::shared::calibration::EnumerationCheck::Same,
    })
}

/// Same-capture reference latency (#460) at `tau_s`, on nominal reference
/// ports distinct from [`measured_tau`]'s capture pair.
pub(super) fn measured_reference(tau_s: f64) -> ReferenceLatency {
    ReferenceLatency::Measured(MeasuredReferenceLatency {
        tau_s,
        pre_impulse_snr_db: Some(60.0),
        pre_impulse_snr_floor_db: Some(63.0),
        method: "farina_same_capture_reference_v1".into(),
        output_port: "ref_out".into(),
        input_port: "ref_in".into(),
    })
}

/// Stored τ for the reference pair (#359, schema v9), analogous to
/// [`measured_tau`] but with the reference pair's own port names and a
/// settable `period_size` — the `arrival_check` tests need both `Some` and
/// `None` on the stored side, since a `None` period size must never let a
/// disagreement read as a period shift.
pub(super) fn stored_reference_tau(tau_s: f64, period_size: Option<u32>) -> InterfaceLatency {
    InterfaceLatency::Measured(MeasuredLatency {
        tau_s,
        measured_at: "2026-08-15T00:00:00Z".into(),
        method: "farina_short_ess".into(),
        backend: "fake".into(),
        sample_rate_hz: 48_000,
        period_size,
        output_port: "ref_out".into(),
        input_port: "ref_in".into(),
        enumeration: Some(crate::shared::calibration::EnumerationCheck::Same),
        session_check: None,
        session_check_loopback: None,
    })
}

pub(super) fn sample_noise_report() -> MeasurementReport {
    MeasurementReport {
        schema_version: SCHEMA_VERSION,
        ac_version: "0.1.0".into(),
        timestamp_utc: "2026-04-22T12:00:00Z".into(),
        backend: None,
        method: MeasurementMethod::SteppedSine { n_points: 0 },
        stimulus: StimulusParams {
            sample_rate_hz: 48_000,
            f_start_hz: 0.0,
            f_stop_hz: 0.0,
            level_dbfs: 0.0,
            n_points: 0,
        },
        integration: IntegrationParams {
            duration_s: 1.0,
            window: "none".into(),
            n_averages: None,
        },
        calibration: None,
        position: None,
        interface_latency: None,
        reference_latency: None,
        reference_stored_latency: None,
        inter_pair_offset: None,
        tail_decay: None,
        data: vec![MeasurementPayload {
            data: MeasurementData::NoiseResult {
                sample_rate_hz: 48_000,
                duration_s: 0.9,
                unweighted_dbfs: -98.4,
                a_weighted_dbfs: -103.1,
                ccir_weighted_dbfs: None,
            },
            standard: vec![crate::measurement::noise::citation()],
            gate: None,
        }],
        notes: None,
        processing_chain: ProcessingChain::default(),
    }
}
