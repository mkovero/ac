//! Per-payload layout: title, citation/gate metadata, and the tabular
//! or key-value body each [`MeasurementData`](crate::measurement::report::MeasurementData)
//! variant carries.

use super::Row;
use crate::measurement::report::{
    FrequencyResponsePoint, GatedFrequencyResponsePoint, MeasurementPayload,
};

/// One table column, named for both backends and sized for the fixed-
/// width one. Keeping the two spellings adjacent stops the column sets
/// drifting apart the way the header lists used to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Column {
    /// Header for a flowing layout, where a full word fits.
    pub html: &'static str,
    /// Header for a fixed-width layout, where it does not.
    pub plain: &'static str,
    pub width_mm: f32,
    /// Left-aligned identifier column rather than a right-aligned
    /// number.
    pub label: bool,
}

const fn col(html: &'static str, plain: &'static str, width_mm: f32, label: bool) -> Column {
    Column {
        html,
        plain,
        width_mm,
        label,
    }
}

/// Format a measured value, marking a non-finite one rather than
/// printing `NaN` into a report someone will read as data.
pub fn fmt_f(v: f64, decimals: usize) -> String {
    if !v.is_finite() {
        return "\u{2013}".into();
    }
    format!("{v:.decimals$}")
}

/// Citation(s) and gate for one payload. A payload with no citation
/// yields no `standard` row at all — omission reads as "not
/// applicable", not as an error state (#280).
pub fn payload_meta_rows(p: &MeasurementPayload) -> Vec<Row> {
    let mut rows = Vec::new();
    for s in &p.standard {
        rows.push(Row::new(
            "standard",
            format!(
                "{} \u{2014} {}{}",
                s.standard,
                s.clause,
                if s.verified { " \u{2713} verified" } else { "" }
            ),
        ));
    }
    if let Some(g) = &p.gate {
        rows.push(Row::new(
            "gate",
            format!(
                "{:.1} ms \u{2192} {:.1} ms ({:.1} ms window, {})",
                g.gate_start_s * 1000.0,
                (g.gate_start_s + g.gate_length_s) * 1000.0,
                g.gate_length_s * 1000.0,
                g.window_kind,
            ),
        ));
        rows.push(Row::new(
            "f_low",
            format!("{:.1} Hz (= 1 / gate length)", g.f_low_hz),
        ));
    }
    rows
}

// ---------------------------------------------------------------------------
// Frequency response
// ---------------------------------------------------------------------------

const FR_COLUMNS: &[Column] = &[
    col("freq (Hz)", "freq_Hz", 24.0, true),
    col("fundamental (dBFS)", "fund_dBFS", 26.0, false),
    col("THD (%)", "THD_%", 24.0, false),
    col("THD+N (%)", "THD+N_%", 24.0, false),
    col("noise (dBFS)", "noise_dBFS", 28.0, false),
    col("flags", "flags", 20.0, true),
];

pub fn frequency_response_columns() -> &'static [Column] {
    FR_COLUMNS
}

/// One string per cell, in `FR_COLUMNS` order.
///
/// `thd_pct` and `thdn_pct` are already percentages — `thd::analyze`
/// divides each residual by the total output and multiplies by 100 before
/// storing them. The PDF backend used to scale them by a further 100, printing
/// distortion a hundredfold high under a `%` header. Formatting them
/// here means there is one place left to get that wrong.
pub fn frequency_response_cells(points: &[FrequencyResponsePoint]) -> Vec<Vec<String>> {
    points
        .iter()
        .map(|p| {
            let mut flags = Vec::new();
            if p.clipping {
                flags.push("clip");
            }
            if p.ac_coupled {
                flags.push("ac");
            }
            vec![
                fmt_f(p.freq_hz, 2),
                fmt_f(p.fundamental_dbfs, 2),
                fmt_f(p.thd_pct, 4),
                fmt_f(p.thdn_pct, 4),
                fmt_f(p.noise_floor_dbfs, 2),
                flags.join(", "),
            ]
        })
        .collect()
}

/// `(frequency, level)` pairs for the magnitude trace, DC excluded.
pub fn frequency_response_series(points: &[FrequencyResponsePoint]) -> Vec<(f64, f64)> {
    points
        .iter()
        .filter(|p| p.freq_hz > 0.0)
        .map(|p| (p.freq_hz, p.fundamental_dbfs))
        .collect()
}

// ---------------------------------------------------------------------------
// Gated frequency response
// ---------------------------------------------------------------------------

const GATED_COLUMNS: &[Column] = &[
    col("freq (Hz)", "freq_Hz", 26.0, true),
    col("magnitude (dB)", "mag_dB", 30.0, false),
    col("phase (\u{b0})", "phase_deg", 26.0, false),
];

pub fn gated_columns() -> &'static [Column] {
    GATED_COLUMNS
}

pub fn gated_cells(points: &[GatedFrequencyResponsePoint]) -> Vec<Vec<String>> {
    points
        .iter()
        .map(|p| {
            vec![
                fmt_f(p.freq_hz, 2),
                fmt_f(p.magnitude_db, 2),
                fmt_f(p.phase_deg, 2),
            ]
        })
        .collect()
}

pub fn gated_magnitude_series(points: &[GatedFrequencyResponsePoint]) -> Vec<(f64, f64)> {
    points
        .iter()
        .filter(|p| p.freq_hz > 0.0)
        .map(|p| (p.freq_hz, p.magnitude_db))
        .collect()
}

pub fn gated_phase_series(points: &[GatedFrequencyResponsePoint]) -> Vec<(f64, f64)> {
    points
        .iter()
        .filter(|p| p.freq_hz > 0.0)
        .map(|p| (p.freq_hz, p.phase_deg))
        .collect()
}

// ---------------------------------------------------------------------------
// Spectrum bands
// ---------------------------------------------------------------------------

const SPECTRUM_COLUMNS: &[Column] = &[
    col("centre (Hz)", "centre_Hz", 30.0, true),
    col("level (dBFS)", "level_dBFS", 30.0, false),
];

pub fn spectrum_columns() -> &'static [Column] {
    SPECTRUM_COLUMNS
}

pub fn spectrum_bands_rows(bpo: u32, class: &str) -> Vec<Row> {
    vec![
        Row::new("bands", format!("1/{bpo} octave")),
        Row::new("class", class.to_string()),
    ]
}

pub fn spectrum_cells(centres_hz: &[f64], levels_dbfs: &[f64]) -> Vec<Vec<String>> {
    centres_hz
        .iter()
        .zip(levels_dbfs.iter())
        .map(|(c, l)| vec![fmt_f(*c, 2), fmt_f(*l, 2)])
        .collect()
}

// ---------------------------------------------------------------------------
// Key-value payloads
// ---------------------------------------------------------------------------

/// Sweep provenance for a Farina impulse response. The harmonic orders
/// are listed rather than counted: "3" does not tell a reader whether
/// the 2nd was captured.
pub fn impulse_response_rows(
    sample_rate_hz: u32,
    f1_hz: f64,
    f2_hz: f64,
    duration_s: f64,
    linear_ir_len: usize,
    harmonic_orders: &[u32],
    noise_tail_start_s: Option<f64>,
) -> Vec<Row> {
    let mut rows = vec![
        Row::new("sample rate", format!("{sample_rate_hz} Hz")),
        Row::new(
            "sweep",
            format!("{f1_hz:.1} Hz \u{2192} {f2_hz:.1} Hz over {duration_s:.3} s"),
        ),
        Row::new("linear IR length", format!("{linear_ir_len} samples")),
        Row::new(
            "harmonic IRs",
            if harmonic_orders.is_empty() {
                "0".to_string()
            } else {
                format!(
                    "{} (orders {})",
                    harmonic_orders.len(),
                    harmonic_orders
                        .iter()
                        .map(|o| o.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            },
        ),
    ];
    if let Some(t) = noise_tail_start_s {
        rows.push(Row::new(
            "noise tail begins",
            format!(
                "{:.1} ms after peak (sweep duration \u{2014} beyond this, `full` is \
                 convolution-smeared noise, not system response)",
                t * 1000.0
            ),
        ));
    }
    rows
}

pub fn noise_result_rows(
    sample_rate_hz: u32,
    duration_s: f64,
    unweighted_dbfs: f64,
    a_weighted_dbfs: f64,
    ccir_weighted_dbfs: Option<f64>,
) -> Vec<Row> {
    let mut rows = vec![
        Row::new("sample rate", format!("{sample_rate_hz} Hz")),
        Row::new("duration", format!("{duration_s:.3} s")),
        Row::new("unweighted", format!("{unweighted_dbfs:.2} dBFS")),
        Row::new("A-weighted", format!("{a_weighted_dbfs:.2} dBFS")),
    ];
    if let Some(c) = ccir_weighted_dbfs {
        rows.push(Row::new("CCIR-468", format!("{c:.2} dBFS (quasi-peak)")));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement::report::{GateParams, MeasurementData, StandardsCitation};

    fn point(freq_hz: f64, thd_pct: f64) -> FrequencyResponsePoint {
        FrequencyResponsePoint {
            freq_hz,
            fundamental_dbfs: -20.0,
            thd_pct,
            thdn_pct: 0.02,
            noise_floor_dbfs: -120.0,
            linear_rms: 0.0707,
            clipping: false,
            ac_coupled: false,
        }
    }

    #[test]
    fn thd_is_rendered_as_stored_because_it_is_already_a_percentage() {
        // `thd::analyze` stores `residual / fundamental * 100`, so a
        // 0.5 % reading is stored as 0.5. Scaling it again on the way
        // out — which the PDF backend did — prints 50 %.
        let cells = frequency_response_cells(&[point(1_000.0, 0.5)]);
        assert_eq!(cells[0][2], "0.5000", "{cells:?}");
        assert_ne!(cells[0][2], "50.0000");
    }

    #[test]
    fn every_row_has_one_cell_per_column() {
        let cells = frequency_response_cells(&[point(1_000.0, 0.5)]);
        assert!(cells.iter().all(|r| r.len() == FR_COLUMNS.len()));
        let gated = gated_cells(&[GatedFrequencyResponsePoint {
            freq_hz: 100.0,
            magnitude_db: -0.5,
            phase_deg: 142.68,
        }]);
        assert!(gated.iter().all(|r| r.len() == GATED_COLUMNS.len()));
        let spectrum = spectrum_cells(&[100.0, 125.0], &[-30.0, -25.0]);
        assert!(spectrum.iter().all(|r| r.len() == SPECTRUM_COLUMNS.len()));
    }

    #[test]
    fn fixed_width_columns_fit_the_printable_width() {
        // 210 mm A4 less two 15 mm margins. A column set that overflows
        // silently overprints the next column in the PDF backend.
        for cols in [FR_COLUMNS, GATED_COLUMNS, SPECTRUM_COLUMNS] {
            let total: f32 = cols.iter().map(|c| c.width_mm).sum();
            assert!(total <= 180.0, "{total} mm over 180 mm: {cols:?}");
        }
    }

    #[test]
    fn series_drop_dc_so_a_log_axis_stays_finite() {
        let pts = vec![
            GatedFrequencyResponsePoint {
                freq_hz: 0.0,
                magnitude_db: -0.1,
                phase_deg: 0.0,
            },
            GatedFrequencyResponsePoint {
                freq_hz: 100.0,
                magnitude_db: -0.5,
                phase_deg: 10.0,
            },
        ];
        assert_eq!(gated_magnitude_series(&pts).len(), 1);
        assert_eq!(gated_phase_series(&pts).len(), 1);
        // The table still shows DC — it is real data, just unplottable.
        assert_eq!(gated_cells(&pts).len(), 2);
        assert_eq!(frequency_response_series(&[point(0.0, 0.1)]).len(), 0);
    }

    #[test]
    fn non_finite_measurements_are_marked_not_printed_as_nan() {
        let cells = frequency_response_cells(&[point(1_000.0, f64::NAN)]);
        assert_eq!(cells[0][2], "\u{2013}");
    }

    #[test]
    fn harmonic_orders_are_listed_not_merely_counted() {
        let rows = impulse_response_rows(48_000, 20.0, 20_000.0, 3.0, 144_000, &[2, 3, 5], None);
        let v = &rows
            .iter()
            .find(|r| r.label == "harmonic IRs")
            .unwrap()
            .value;
        assert!(v.contains("orders 2, 3, 5"), "{v}");
    }

    #[test]
    fn noise_tail_row_appears_only_when_measured() {
        let without = impulse_response_rows(48_000, 20.0, 20_000.0, 3.0, 10, &[], None);
        assert!(without.iter().all(|r| r.label != "noise tail begins"));
        let with = impulse_response_rows(48_000, 20.0, 20_000.0, 3.0, 10, &[], Some(0.25));
        let v = &with
            .iter()
            .find(|r| r.label == "noise tail begins")
            .unwrap()
            .value;
        assert!(v.starts_with("250.0 ms"), "{v}");
    }

    #[test]
    fn ccir_row_appears_only_when_measured() {
        assert!(noise_result_rows(48_000, 1.0, -98.0, -103.0, None)
            .iter()
            .all(|r| r.label != "CCIR-468"));
        let rows = noise_result_rows(48_000, 1.0, -98.0, -103.0, Some(-95.0));
        assert_eq!(
            rows.iter().find(|r| r.label == "CCIR-468").unwrap().value,
            "-95.00 dBFS (quasi-peak)"
        );
    }

    #[test]
    fn payload_without_a_citation_yields_no_standard_row() {
        let p = MeasurementPayload {
            data: MeasurementData::FrequencyResponse { points: vec![] },
            standard: vec![],
            gate: None,
        };
        assert!(payload_meta_rows(&p).is_empty());
    }

    #[test]
    fn every_citation_gets_its_own_row_and_a_gate_adds_f_low() {
        let p = MeasurementPayload {
            data: MeasurementData::FrequencyResponse { points: vec![] },
            standard: vec![
                StandardsCitation {
                    standard: "ISO 18233:2006".into(),
                    clause: "\u{a7}9(c)".into(),
                    verified: false,
                },
                StandardsCitation {
                    standard: "AES17-2020".into(),
                    clause: "Annex A.4.5".into(),
                    verified: true,
                },
            ],
            gate: Some(GateParams {
                gate_start_s: 0.0029,
                gate_length_s: 0.020,
                window_kind: "tukey0.25".into(),
                f_low_hz: 50.0,
            }),
        };
        let rows = payload_meta_rows(&p);
        assert_eq!(rows.iter().filter(|r| r.label == "standard").count(), 2);
        assert!(rows[1].value.contains("\u{2713} verified"), "{rows:?}");
        assert!(rows.iter().any(|r| r.label == "gate"));
        assert_eq!(
            rows.iter().find(|r| r.label == "f_low").unwrap().value,
            "50.0 Hz (= 1 / gate length)"
        );
    }
}
