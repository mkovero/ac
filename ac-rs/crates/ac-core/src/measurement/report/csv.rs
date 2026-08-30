//! Flat-CSV rendering of a report's payloads, for spreadsheet tools. The
//! JSON in [`super::MeasurementReport::to_json`] is the archival form;
//! this is the lossy convenience one.

use std::fmt::Write as _;

use super::{MeasurementData, MeasurementReport};

impl MeasurementReport {
    /// Flat CSV of the report's data payloads. One block per payload,
    /// separated by a blank line and a `# payload N: <kind>` comment
    /// row (with the gate summary appended when the payload carries
    /// one) — two payloads never collapse into one table, and tooling
    /// that wants a single clean table can `grep -v '^#'` or split on
    /// the blank line. The header and column set within a block depend
    /// on that payload's `MeasurementData` variant.
    ///
    /// Infallible: formatting into a `String` cannot fail, so there is
    /// no encode error for a caller to handle — matching
    /// [`super::report_html::render_html`].
    pub fn to_csv(&self) -> String {
        let mut s = String::new();
        for (i, payload) in self.data.iter().enumerate() {
            if i > 0 {
                let _ = writeln!(s);
            }
            let _ = match &payload.gate {
                Some(g) => writeln!(
                    s,
                    "# payload {}: {}  gate={:.1}ms+{:.1}ms {} f_low={:.1}Hz",
                    i + 1,
                    payload.data.kind_label(),
                    g.gate_start_s * 1000.0,
                    g.gate_length_s * 1000.0,
                    g.window_kind,
                    g.f_low_hz,
                ),
                None => writeln!(s, "# payload {}: {}", i + 1, payload.data.kind_label()),
            };
            payload.data.write_csv(&mut s);
        }
        s
    }
}

impl MeasurementData {
    fn write_csv(&self, s: &mut String) {
        match self {
            MeasurementData::FrequencyResponse { points } => {
                let _ = writeln!(
                    s,
                    "freq_hz,fundamental_dbfs,thd_pct,thdn_pct,noise_floor_dbfs,linear_rms,clipping,ac_coupled"
                );
                for p in points {
                    let _ = writeln!(
                        s,
                        "{:.6},{:.6},{:.6},{:.6},{:.6},{:.9},{},{}",
                        p.freq_hz,
                        p.fundamental_dbfs,
                        p.thd_pct,
                        p.thdn_pct,
                        p.noise_floor_dbfs,
                        p.linear_rms,
                        p.clipping,
                        p.ac_coupled,
                    );
                }
            }
            MeasurementData::SpectrumBands {
                bpo,
                class,
                centres_hz,
                levels_dbfs,
            } => {
                let _ = writeln!(s, "centre_hz,level_dbfs,bpo,class");
                for (c, l) in centres_hz.iter().zip(levels_dbfs.iter()) {
                    let _ = writeln!(s, "{:.6},{:.6},{},{}", c, l, bpo, class);
                }
            }
            MeasurementData::ImpulseResponse {
                sample_rate_hz,
                linear_ir,
                harmonics,
                ..
            } => {
                let _ = writeln!(s, "sample_idx,time_s,order,amplitude");
                let fs = *sample_rate_hz as f64;
                for (i, v) in linear_ir.iter().enumerate() {
                    let _ = writeln!(s, "{},{:.9},1,{:.9}", i, i as f64 / fs, v);
                }
                for h in harmonics {
                    for (i, v) in h.samples.iter().enumerate() {
                        let _ = writeln!(s, "{},{:.9},{},{:.9}", i, i as f64 / fs, h.order, v);
                    }
                }
            }
            MeasurementData::NoiseResult {
                sample_rate_hz,
                duration_s,
                unweighted_dbfs,
                a_weighted_dbfs,
                ccir_weighted_dbfs,
            } => {
                let _ = writeln!(
                    s,
                    "sample_rate_hz,duration_s,unweighted_dbfs,a_weighted_dbfs,ccir_weighted_dbfs"
                );
                let ccir = ccir_weighted_dbfs
                    .map(|v| format!("{v:.6}"))
                    .unwrap_or_default();
                let _ = writeln!(
                    s,
                    "{},{:.6},{:.6},{:.6},{}",
                    sample_rate_hz, duration_s, unweighted_dbfs, a_weighted_dbfs, ccir,
                );
            }
            MeasurementData::GatedFrequencyResponse { points } => {
                let _ = writeln!(s, "freq_hz,magnitude_db,phase_deg");
                for p in points {
                    let _ = writeln!(
                        s,
                        "{:.6},{:.6},{:.4}",
                        p.freq_hz, p.magnitude_db, p.phase_deg
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::*;
    use super::super::*;

    #[test]
    fn report_csv_is_stable() {
        let r = sample_report();
        let a = r.to_csv();
        let b = r.to_csv();
        assert_eq!(a, b);
        // Payload comment + header + 3 data lines.
        assert_eq!(a.lines().count(), 5);
        assert!(a.starts_with("# payload 1: frequency_response"));
        assert!(a.contains("freq_hz,fundamental_dbfs,"));
    }

    #[test]
    fn spectrum_bands_csv_shape() {
        let r = sample_spectrum_bands_report();
        let csv = r.to_csv();
        assert!(csv.starts_with("# payload 1: spectrum_bands"));
        assert!(csv.contains("centre_hz,level_dbfs,bpo,class"));
        assert_eq!(csv.lines().count(), 5);
    }

    #[test]
    fn impulse_response_csv_shape() {
        let r = sample_impulse_response_report();
        let csv = r.to_csv();
        assert!(csv.starts_with("# payload 1: impulse_response"));
        assert!(csv.contains("sample_idx,time_s,order,amplitude"));
        // Payload comment + header + 5 linear rows + 5 harmonic rows.
        assert_eq!(csv.lines().count(), 12);
    }

    // ─── `ir_stats` (#283) ────────────────────────────────────────────

    #[test]
    fn noise_result_csv_shape() {
        let r = sample_noise_report();
        let csv = r.to_csv();
        assert!(csv.starts_with("# payload 1: noise_result"));
        assert!(csv.contains("sample_rate_hz,duration_s,unweighted_dbfs,"));
        assert_eq!(csv.lines().count(), 3);
    }

    #[test]
    fn multi_payload_csv_does_not_collapse_into_one_table() {
        let mut r = sample_impulse_response_report();
        r.data.push(MeasurementPayload {
            data: MeasurementData::SpectrumBands {
                bpo: 3,
                class: "Class 1".into(),
                centres_hz: vec![100.0],
                levels_dbfs: vec![-30.0],
            },
            standard: vec![],
            gate: Some(GateParams {
                gate_start_s: 0.0,
                gate_length_s: 0.02,
                window_kind: "hann".into(),
                f_low_hz: 50.0,
            }),
        });
        let csv = r.to_csv();
        assert!(csv.contains("# payload 1: impulse_response"));
        assert!(csv.contains("# payload 2: spectrum_bands  gate=0.0ms+20.0ms hann f_low=50.0Hz"));
        assert!(csv.contains("sample_idx,time_s,order,amplitude"));
        assert!(csv.contains("centre_hz,level_dbfs,bpo,class"));
    }

    #[test]
    fn gated_frequency_response_csv_shape() {
        let mut r = sample_impulse_response_report();
        r.data = vec![MeasurementPayload {
            data: MeasurementData::GatedFrequencyResponse {
                points: vec![
                    GatedFrequencyResponsePoint {
                        freq_hz: 100.0,
                        magnitude_db: -0.5,
                        phase_deg: 12.3,
                    },
                    GatedFrequencyResponsePoint {
                        freq_hz: 1_000.0,
                        magnitude_db: -1.2,
                        phase_deg: -145.0,
                    },
                ],
            },
            standard: vec![],
            gate: None,
        }];
        let csv = r.to_csv();
        assert!(csv.starts_with("# payload 1: gated_frequency_response"));
        assert!(csv.contains("freq_hz,magnitude_db,phase_deg"));
        // Payload comment + header + 2 data lines.
        assert_eq!(csv.lines().count(), 4);
    }
}
