//! Multi-run verification (#398): the statistics a set of `plot ir` runs
//! supports and no single run does — gain spread across a drive series,
//! tonal stability against the series mean, harmonic drive-tracking, and
//! which runs are fit to aggregate at all.
//!
//! Numbers only. [`verify`] turns archived [`MeasurementReport`]s (plus an
//! optional microphone curve applied post-hoc) into a [`VerificationSet`]
//! or refuses the set with a [`VerificationRefusal`]; what each figure
//! *says* is `report_layout::verification`'s job, and the terminal, HTML
//! and PDF all read that one layout. A renderer never sees a report, so it
//! cannot compute a second, drifting copy of a statistic.
//!
//! Every response statistic is taken on 1/6-octave power means of the
//! gated response, inside a named band normalised against that band's own
//! median. A maximum over raw FFT bins grows with the bin count — with the
//! gate length — while a maximum over a fixed ~10 cells per band does not.
//! The grid is built here rather than borrowed from
//! `visualize::smoothing`: a Tier 1 verdict must not depend on a Tier 2
//! display helper.
//!
//! What this module does not do: attribute a feature to a cause (no
//! "crossover", no "floor reflection"), compare absolute level across
//! runs (the interface output volume is recorded nowhere, so gain spread
//! assumes it did not change), or re-derive a tail-decay verdict for a
//! pre-v13 archive (the tail it needs was never stored).
//!
//! [`MeasurementReport`]: crate::measurement::report::MeasurementReport

mod set;
mod stats;

pub use set::{
    verify, CurveIdentity, Exclusion, FieldValue, MicProvenance, Mismatch, Run, RunInput, RunMic,
    RunStatus, SetField, SweepSummary, VerificationRefusal, VerificationSet,
};
pub use stats::{
    Band, Figure, Flatness, HarmonicReading, HarmonicRow, NotComputed, RunSeries, Verdict,
};

/// Resolution of the statistics grid: 1/6-octave cells centred on
/// `1000·2^(k/6)` Hz. Provenance: assumed (#398 architect decision 5).
pub const STATS_GRID_BPO: u32 = 6;

/// Fixed lower edge of every band. Below it an in-room response at 0.5 m
/// is the room, not the loudspeaker. A constant, not derived from
/// `position`. Provenance: assumed (#398, 2026-08-28 geometry).
pub const BAND_LOWER_EDGE_HZ: f64 = 1_500.0;

/// Band the per-run gain is read in. Provenance: assumed (#398 design
/// point 3).
pub const GAIN_BAND_HZ: (f64, f64) = (BAND_LOWER_EDGE_HZ, 16_000.0);

/// Bands tonal stability and flatness are read in, each normalised
/// against its own median. Provenance: assumed (#398 design point 3).
pub const TONAL_BANDS_HZ: [(f64, f64); 2] = [(BAND_LOWER_EDGE_HZ, 5_000.0), (5_000.0, 16_000.0)];

/// Gain spread limit, dB. Provenance: assumed; the #398 rig check
/// falsifies it if repeat runs exceed half of it.
pub const GAIN_SPREAD_LIMIT_DB: f64 = 0.5;

/// Tonal stability limit, ±dB. Provenance: assumed, as
/// [`GAIN_SPREAD_LIMIT_DB`].
pub const TONAL_STABILITY_LIMIT_DB: f64 = 0.5;

/// How far a harmonic's slope may sit from its order's expectation and
/// still read as tracking drive, dB per 10 dB. A gate, not a readout.
/// Provenance: assumed.
pub const HARMONIC_TRACK_TOLERANCE_DB: f64 = 3.0;

/// Smallest drive span a harmonic slope is fitted over, dB. Provenance:
/// assumed.
pub const MIN_DRIVE_SPAN_DB: f64 = 10.0;

/// Fewest used runs any set statistic needs: a spread needs two.
pub const MIN_USED_RUNS: usize = 2;

/// ISO 18233 §6.3.2's required tail decay, dB, as the run table states it
/// when no run carries a recorded requirement. Must equal what
/// `check_tail_decay` writes into `TailDecayCheck::required_db`; a test
/// holds the two together.
pub const TAIL_DECAY_REQUIRED_DB: f64 = 30.0;

#[cfg(test)]
pub(crate) mod testkit {
    //! Synthetic `plot ir` reports for the verification tests.

    use crate::measurement::report::{
        GateParams, GatedFrequencyResponsePoint, IntegrationParams, MeasurementData,
        MeasurementMethod, MeasurementPayload, MeasurementReport, ProcessingChain, StimulusParams,
        TailDecayRecord, SCHEMA_VERSION,
    };
    use crate::measurement::sweep::{HarmonicIr, TailDecayCheck};

    pub const SR: u32 = 48_000;
    pub const IR_LEN: usize = 1024;

    /// A gated-response bin grid at `df` Hz spacing up to 24 kHz.
    pub fn bins(df: f64) -> Vec<f64> {
        (1..)
            .map(|k| k as f64 * df)
            .take_while(|f| *f <= 24_000.0)
            .collect()
    }

    /// A passed v13 tail-decay record.
    pub fn tail_passed() -> TailDecayRecord {
        TailDecayRecord::Checked(TailDecayCheck {
            bpo: 3,
            worst_band_hz: 12_500.0,
            worst_decay_db: 41.2,
            required_db: 30.0,
            passed: true,
            bands_settled: 30,
            bands_total: 30,
        })
    }

    /// A failed v13 tail-decay record: the truncated-sweep case.
    pub fn tail_failed() -> TailDecayRecord {
        TailDecayRecord::Checked(TailDecayCheck {
            bpo: 3,
            worst_band_hz: 16_000.0,
            worst_decay_db: 11.6,
            required_db: 30.0,
            passed: false,
            bands_settled: 30,
            bands_total: 30,
        })
    }

    /// A clean `plot ir` report: unit impulse at the window centre over a
    /// silent pre-impulse floor, harmonic `order` IRs of amplitude
    /// `harmonics[i].1` relative to it, and a gated response of
    /// `response(f)` dB at every bin in `freqs`.
    pub fn run(
        timestamp: &str,
        level_dbfs: f64,
        freqs: &[f64],
        response: impl Fn(f64) -> f64,
        harmonics: &[(u32, f64)],
    ) -> MeasurementReport {
        let mut linear_ir = vec![0.0; IR_LEN];
        linear_ir[IR_LEN / 2] = 1.0;
        let harmonics = harmonics
            .iter()
            .map(|&(order, amp)| {
                let mut samples = vec![0.0; IR_LEN / 4];
                samples[IR_LEN / 8] = amp;
                HarmonicIr { order, samples }
            })
            .collect();
        let points = freqs
            .iter()
            .map(|&f| GatedFrequencyResponsePoint {
                freq_hz: f,
                magnitude_db: response(f),
                phase_deg: 0.0,
            })
            .collect();
        let gate_length_s = 0.005;
        MeasurementReport {
            schema_version: SCHEMA_VERSION,
            ac_version: "0.2.0".into(),
            timestamp_utc: timestamp.into(),
            backend: Some("fake".into()),
            method: MeasurementMethod::SweptSine {
                f1_hz: 20.0,
                f2_hz: 20_000.0,
                duration_s: 4.0,
            },
            stimulus: StimulusParams {
                sample_rate_hz: SR,
                f_start_hz: 20.0,
                f_stop_hz: 20_000.0,
                level_dbfs,
                n_points: 0,
            },
            integration: IntegrationParams {
                duration_s: 4.0,
                window: "farina-inverse".into(),
                n_averages: None,
            },
            calibration: None,
            position: None,
            interface_latency: None,
            reference_latency: None,
            reference_stored_latency: None,
            inter_pair_offset: None,
            tail_decay: Some(tail_passed()),
            data: vec![
                MeasurementPayload {
                    data: MeasurementData::ImpulseResponse {
                        sample_rate_hz: SR,
                        f1_hz: 20.0,
                        f2_hz: 20_000.0,
                        duration_s: 4.0,
                        linear_ir,
                        harmonics,
                        noise_tail_start_s: None,
                    },
                    standard: vec![],
                    gate: None,
                },
                MeasurementPayload {
                    data: MeasurementData::GatedFrequencyResponse { points },
                    standard: vec![],
                    gate: Some(GateParams {
                        gate_start_s: 0.0,
                        gate_length_s,
                        window_kind: "tukey0.25".into(),
                        f_low_hz: 1.0 / gate_length_s,
                    }),
                },
            ],
            notes: None,
            processing_chain: ProcessingChain::default(),
        }
    }

    /// A deterministic, roughly Gaussian (sum of twelve uniforms) noise
    /// sequence with unit standard deviation.
    pub struct Noise(u64);

    impl Noise {
        pub fn new(seed: u64) -> Self {
            Self(seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1))
        }

        fn uniform(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }

        pub fn next(&mut self) -> f64 {
            (0..12).map(|_| self.uniform()).sum::<f64>() - 6.0
        }
    }
}
