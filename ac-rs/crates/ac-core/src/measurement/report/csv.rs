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
