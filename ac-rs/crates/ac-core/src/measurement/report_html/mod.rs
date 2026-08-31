//! Render a [`MeasurementReport`] as a self-contained HTML document.
//!
//! No external CSS / JS / images — the entire report fits in one file
//! the user can email, commit, or open in any browser. Plots are
//! embedded as inline SVG. The styling is minimal-opinionated: a
//! readable monospace-for-data, sans-serif-for-prose layout that prints
//! cleanly to PDF via the browser's built-in "save as PDF" flow.
//!
//! Intentionally not loaded: chart libraries, MathJax, any network
//! asset. Everything you see is in the file.
//!
//! What each section *says* lives in `report_layout`, shared with the
//! PDF backend; this module only decides how to paint it.

mod emit;
mod plot;

use std::fmt::Write as _;

use crate::measurement::report::{MeasurementData, MeasurementPayload, MeasurementReport};
use crate::measurement::report_layout::{self as layout, Body};

const CSS: &str = r#"
body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
       max-width: 1000px; margin: 2em auto; padding: 0 1em; color: #222; }
h1 { border-bottom: 2px solid #333; padding-bottom: 0.2em; }
h2 { margin-top: 1.8em; color: #444; }
table { border-collapse: collapse; margin: 0.6em 0; font-size: 0.92em; }
th, td { border: 1px solid #ccc; padding: 3px 10px; text-align: right; }
th { background: #eee; font-weight: 600; }
td.label, th.label { text-align: left; font-family: ui-monospace, "SF Mono", Consolas, monospace; }
.meta dt { font-weight: 600; float: left; width: 11em; clear: left; }
.meta dd { margin: 0 0 0.2em 11em; font-family: ui-monospace, "SF Mono", Consolas, monospace; }
.note { color: #666; font-size: 0.9em; }
svg { display: block; margin: 1em 0; background: #fafafa; border: 1px solid #ccc; }
svg .axis { stroke: #888; stroke-width: 1; fill: none; }
svg .grid { stroke: #ddd; stroke-width: 1; fill: none; }
svg text { font-family: ui-monospace, Consolas, monospace; font-size: 10px; fill: #333; }
svg .trace { fill: none; stroke: #1f77b4; stroke-width: 1.6; }
svg .trace-phase { fill: none; stroke: #999; stroke-width: 1.0; }
"#;

/// Render `report` as a self-contained HTML document.
pub fn render_html(report: &MeasurementReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "<!DOCTYPE html>");
    let _ = writeln!(out, "<html lang=\"en\"><head>");
    let _ = writeln!(out, "<meta charset=\"UTF-8\">");
    let _ = writeln!(out, "<title>ac \u{2014} MeasurementReport</title>");
    let _ = writeln!(out, "<style>{CSS}</style>");
    let _ = writeln!(out, "</head><body>");

    let _ = writeln!(out, "<h1>ac MeasurementReport</h1>");
    emit::write_rows(&mut out, &layout::header_rows(report));

    for section in layout::sections(report) {
        let _ = writeln!(out, "<h2>{}</h2>", html_escape(section.title));
        match &section.body {
            Body::Rows(rows) => emit::write_rows(&mut out, rows),
            Body::Note(note) => {
                let _ = writeln!(out, "<p class=\"note\">{}</p>", html_escape(note));
            }
        }
    }

    for payload in &report.data {
        write_payload(&mut out, payload);
    }

    if let Some(notes) = &report.notes {
        let _ = writeln!(out, "<h2>Notes</h2><pre>{}</pre>", html_escape(notes));
    }

    let _ = writeln!(out, "</body></html>");
    out
}

/// One payload: heading, its citation(s) and gate block (when present),
/// then the data-specific body.
fn write_payload(out: &mut String, payload: &MeasurementPayload) {
    let _ = writeln!(
        out,
        "<h2>{}</h2>",
        html_escape(layout::payload_title(&payload.data))
    );
    emit::write_rows(out, &layout::payload_meta_rows(payload));
    write_payload_body(out, &payload.data);
}

fn write_payload_body(out: &mut String, d: &MeasurementData) {
    match d {
        MeasurementData::FrequencyResponse { points } => {
            out.push_str(&plot::magnitude(
                "Frequency response",
                360.0,
                &layout::frequency_response_series(points),
            ));
            emit::write_table(
                out,
                layout::frequency_response_columns(),
                &layout::frequency_response_cells(points),
            );
        }
        MeasurementData::SpectrumBands {
            bpo,
            class,
            centres_hz,
            levels_dbfs,
        } => {
            emit::write_rows(out, &layout::spectrum_bands_rows(*bpo, class));
            emit::write_table(
                out,
                layout::spectrum_columns(),
                &layout::spectrum_cells(centres_hz, levels_dbfs),
            );
        }
        MeasurementData::ImpulseResponse {
            sample_rate_hz,
            f1_hz,
            f2_hz,
            duration_s,
            linear_ir,
            harmonics,
            noise_tail_start_s,
        } => {
            let orders: Vec<u32> = harmonics.iter().map(|h| h.order).collect();
            emit::write_rows(
                out,
                &layout::impulse_response_rows(
                    *sample_rate_hz,
                    *f1_hz,
                    *f2_hz,
                    *duration_s,
                    linear_ir.len(),
                    &orders,
                    *noise_tail_start_s,
                ),
            );
        }
        MeasurementData::NoiseResult {
            sample_rate_hz,
            duration_s,
            unweighted_dbfs,
            a_weighted_dbfs,
            ccir_weighted_dbfs,
        } => {
            emit::write_rows(
                out,
                &layout::noise_result_rows(
                    *sample_rate_hz,
                    *duration_s,
                    *unweighted_dbfs,
                    *a_weighted_dbfs,
                    *ccir_weighted_dbfs,
                ),
            );
        }
        MeasurementData::GatedFrequencyResponse { points } => {
            // Magnitude carries full visual weight as the primary
            // result; phase rides underneath in its own panel (#284).
            out.push_str(&plot::magnitude(
                "Gated frequency response magnitude",
                300.0,
                &layout::gated_magnitude_series(points),
            ));
            out.push_str(&plot::phase(&layout::gated_phase_series(points)));
            emit::write_table(out, layout::gated_columns(), &layout::gated_cells(points));
        }
    }
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement::report::{
        FrequencyResponsePoint, GateParams, GatedFrequencyResponsePoint, IntegrationParams,
        MeasurementData, MeasurementMethod, MeasurementPayload, MeasurementReport,
        PositionSnapshot, StandardsCitation, StimulusParams, SCHEMA_VERSION,
    };

    fn sample_fr_report() -> MeasurementReport {
        MeasurementReport {
            schema_version: SCHEMA_VERSION,
            ac_version: "0.1.0".into(),
            timestamp_utc: "2026-04-22T12:00:00Z".into(),
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
            data: vec![MeasurementPayload {
                data: MeasurementData::FrequencyResponse {
                    points: vec![
                        FrequencyResponsePoint {
                            freq_hz: 100.0,
                            fundamental_dbfs: -20.5,
                            thd_pct: 0.005,
                            thdn_pct: 0.012,
                            noise_floor_dbfs: -120.0,
                            linear_rms: 0.0707,
                            clipping: false,
                            ac_coupled: false,
                        },
                        FrequencyResponsePoint {
                            freq_hz: 1_000.0,
                            fundamental_dbfs: -20.0,
                            thd_pct: 0.003,
                            thdn_pct: 0.009,
                            noise_floor_dbfs: -121.3,
                            linear_rms: 0.0707,
                            clipping: false,
                            ac_coupled: false,
                        },
                        FrequencyResponsePoint {
                            freq_hz: 10_000.0,
                            fundamental_dbfs: -21.2,
                            thd_pct: 0.008,
                            thdn_pct: 0.015,
                            noise_floor_dbfs: -119.5,
                            linear_rms: 0.0706,
                            clipping: true,
                            ac_coupled: false,
                        },
                    ],
                },
                standard: vec![StandardsCitation {
                    standard: "IEC 60268-3:2018".into(),
                    clause: "§15.12.3".into(),
                    verified: false,
                }],
                gate: None,
            }],
            notes: Some("bench run 2026-04-22".into()),
            processing_chain: crate::measurement::report::ProcessingChain::default(),
        }
    }

    #[test]
    fn renders_valid_html_skeleton() {
        let html = render_html(&sample_fr_report());
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<html"));
        assert!(html.contains("</html>"));
        assert!(html.contains("<title>ac"));
    }

    #[test]
    fn includes_method_and_payload_standard() {
        let html = render_html(&sample_fr_report());
        assert!(html.contains("stepped_sine"));
        assert!(html.contains("IEC 60268-3:2018"));
    }

    /// The slice of `html` between one `<h2>` heading and the next.
    fn section_of<'a>(html: &'a str, title: &str) -> &'a str {
        let head = format!("<h2>{title}</h2>");
        let start = html
            .find(&head)
            .unwrap_or_else(|| panic!("no section {title:?} in:\n{html}"))
            + head.len();
        let rest = &html[start..];
        match rest.find("<h2>") {
            Some(end) => &rest[..end],
            None => rest,
        }
    }

    #[test]
    fn method_section_no_longer_carries_standard_line() {
        // #280: the citation belongs to the payload, not the stimulus
        // method. The report used here *does* carry a citation, so this
        // fails if the row leaks back up into Method rather than merely
        // passing because nothing was cited.
        let html = render_html(&sample_fr_report());
        assert!(
            html.contains("<dt>standard</dt>"),
            "fixture lost its citation"
        );
        assert!(
            !section_of(&html, "Method").contains("<dt>standard</dt>"),
            "{html}"
        );
    }

    #[test]
    fn frequency_response_has_svg_and_table() {
        let html = render_html(&sample_fr_report());
        assert!(html.contains("<svg"));
        assert!(html.contains("</svg>"));
        assert!(html.contains("freq (Hz)"));
        // Table rows: verify a data point appears.
        assert!(html.contains("1000.00")); // 1 kHz freq
                                           // Clipping flag surfaces.
        assert!(html.contains("clip"));
    }

    #[test]
    fn notes_are_rendered() {
        let html = render_html(&sample_fr_report());
        assert!(html.contains("bench run 2026-04-22"));
    }

    #[test]
    fn html_escaping_prevents_injection() {
        let mut r = sample_fr_report();
        r.notes = Some("<script>alert(1)</script>".into());
        let html = render_html(&r);
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn spectrum_bands_renders_table() {
        let mut r = sample_fr_report();
        r.data = vec![MeasurementPayload {
            data: MeasurementData::SpectrumBands {
                bpo: 3,
                class: "Class 1".into(),
                centres_hz: vec![100.0, 125.0, 160.0],
                levels_dbfs: vec![-30.0, -25.0, -28.0],
            },
            standard: vec![],
            gate: None,
        }];
        let html = render_html(&r);
        assert!(html.contains("Spectrum Bands"));
        assert!(html.contains("Class 1"));
        assert!(html.contains("125.00"));
    }

    #[test]
    fn payload_without_standard_omits_standard_line() {
        let mut r = sample_fr_report();
        r.data = vec![MeasurementPayload {
            data: MeasurementData::SpectrumBands {
                bpo: 3,
                class: "Class 1".into(),
                centres_hz: vec![100.0],
                levels_dbfs: vec![-30.0],
            },
            standard: vec![],
            gate: None,
        }];
        let html = render_html(&r);
        assert!(!html.contains("<dt>standard</dt>"), "{html}");
    }

    #[test]
    fn payload_with_multiple_standards_renders_each() {
        let mut r = sample_fr_report();
        r.data[0].standard = vec![
            StandardsCitation {
                standard: "ISO 18233:2006".into(),
                clause: "§9(c)".into(),
                verified: false,
            },
            StandardsCitation {
                standard: "ISO 3382-2:2008".into(),
                clause: "§9.2".into(),
                verified: false,
            },
        ];
        let html = render_html(&r);
        assert!(html.contains("ISO 18233:2006"));
        assert!(html.contains("ISO 3382-2:2008"));
        assert_eq!(html.matches("<dt>standard</dt>").count(), 2);
    }

    #[test]
    fn payload_with_gate_renders_gate_block_and_stored_f_low() {
        let mut r = sample_fr_report();
        r.data[0].gate = Some(GateParams {
            gate_start_s: 0.0029,
            gate_length_s: 0.020,
            window_kind: "tukey0.25".into(),
            f_low_hz: 50.0,
        });
        let html = render_html(&r);
        assert!(html.contains("<dt>gate</dt>"), "{html}");
        assert!(html.contains("tukey0.25"));
        assert!(html.contains("50.0 Hz (= 1 / gate length)"), "{html}");
    }

    #[test]
    fn averages_line_omitted_when_absent_and_rendered_when_set() {
        let html = render_html(&sample_fr_report());
        assert!(
            !section_of(&html, "Method").contains("<dt>averages</dt>"),
            "{html}"
        );

        let mut r = sample_fr_report();
        r.integration.n_averages = Some(8);
        let html = render_html(&r);
        assert!(html.contains("<dt>averages</dt><dd>8</dd>"), "{html}");
    }

    #[test]
    fn environment_section_omitted_when_position_absent() {
        let html = render_html(&sample_fr_report());
        assert!(!html.contains("Environment &amp; Geometry"));
    }

    #[test]
    fn environment_section_renders_temperature_geometry_and_speed_of_sound() {
        let mut r = sample_fr_report();
        r.position = Some(PositionSnapshot {
            temperature_c: Some(21.3),
            relative_humidity_pct: Some(45.0),
            source_height_m: Some(1.2),
            receiver_height_m: Some(1.1),
            distance_m: Some(1.0),
        });
        let html = render_html(&r);
        assert!(html.contains("Environment &amp; Geometry"));
        assert!(html.contains("21.3 °C"));
        assert!(html.contains("45 %"));
        assert!(html.contains("height 1.10 m, distance 1.00 m from source"));
        assert!(html.contains("speed of sound"));
    }

    #[test]
    fn processing_section_collapses_to_raw_when_chain_is_default() {
        // Default chain (all-off + uncorrected) renders the one-line
        // summary instead of a key/value table — keeps simple reports
        // tidy.
        let html = render_html(&sample_fr_report());
        assert!(
            html.contains("<h2>Processing</h2>"),
            "section heading missing"
        );
        assert!(
            html.contains("raw — no smoothing"),
            "default-chain summary missing: {html}"
        );
    }

    #[test]
    fn processing_section_renders_active_state() {
        use crate::measurement::report::ProcessingChain;
        let mut r = sample_fr_report();
        r.processing_chain = ProcessingChain {
            weighting: "a".into(),
            smoothing_bpo: Some(6),
            time_integration: "fast".into(),
            mic_correction_applied: true,
        };
        let html = render_html(&r);
        assert!(
            html.contains("<dt>weighting</dt><dd>a</dd>"),
            "weighting row missing: {html}"
        );
        assert!(
            html.contains("<dt>smoothing</dt><dd>1/6 octave</dd>"),
            "smoothing row missing: {html}"
        );
        assert!(
            html.contains("<dt>time integration</dt><dd>fast</dd>"),
            "time-integration row missing: {html}"
        );
        assert!(
            html.contains("<dt>mic correction</dt><dd>applied</dd>"),
            "mic correction row missing: {html}"
        );
    }

    #[test]
    fn calibration_section_renders_all_three_layers_when_present() {
        use crate::measurement::report::{CalibrationSnapshot, MicResponseRef};
        let mut r = sample_fr_report();
        r.calibration = Some(CalibrationSnapshot {
            output_channel: 0,
            input_channel: 0,
            vrms_at_0dbfs_out: Some(1.0),
            vrms_at_0dbfs_in: Some(0.5),
            ref_freq_hz: 1000.0,
            ref_level_dbfs: -10.0,
            mic_sensitivity_dbfs_at_94db_spl: Some(-32.0),
            mic_response: Some(MicResponseRef {
                n_points: 157,
                source_path: Some("/tmp/umik.frd".into()),
                imported_at: "2026-04-15T12:00:00Z".into(),
            }),
        });
        let html = render_html(&r);
        // Voltage cal still rendered.
        assert!(
            html.contains("V<sub>RMS</sub>@0dBFS in"),
            "voltage missing: {html}"
        );
        // SPL pistonphone reference + computed offset (94 - (-32) = 126).
        assert!(html.contains("94 dB SPL"), "SPL ref label missing: {html}");
        assert!(
            html.contains("-32.00 dBFS"),
            "captured dBFS missing: {html}"
        );
        assert!(
            html.contains("+126.00 dB"),
            "offset missing or wrong: {html}"
        );
        // Mic-curve provenance.
        assert!(html.contains("/tmp/umik.frd"), "curve path missing: {html}");
        assert!(html.contains("157 points"), "n_points missing: {html}");
        assert!(
            html.contains("2026-04-15T12:00:00Z"),
            "imported_at missing: {html}"
        );
    }

    #[test]
    fn calibration_section_says_uncorrected_when_absent() {
        use crate::measurement::report::CalibrationSnapshot;
        let mut r = sample_fr_report();
        r.calibration = Some(CalibrationSnapshot {
            output_channel: 0,
            input_channel: 0,
            vrms_at_0dbfs_out: None,
            vrms_at_0dbfs_in: None,
            ref_freq_hz: 1000.0,
            ref_level_dbfs: -10.0,
            mic_sensitivity_dbfs_at_94db_spl: None,
            mic_response: None,
        });
        let html = render_html(&r);
        assert!(html.contains("not calibrated"), "SPL stub missing: {html}");
        assert!(html.contains("uncorrected"), "mic stub missing: {html}");
    }

    #[test]
    fn noise_result_renders_numbers() {
        let mut r = sample_fr_report();
        r.data = vec![MeasurementPayload {
            data: MeasurementData::NoiseResult {
                sample_rate_hz: 48_000,
                duration_s: 0.9,
                unweighted_dbfs: -98.4,
                a_weighted_dbfs: -103.1,
                ccir_weighted_dbfs: None,
            },
            standard: vec![],
            gate: None,
        }];
        let html = render_html(&r);
        assert!(html.contains("Idle-channel Noise"));
        assert!(html.contains("-98.40 dBFS"));
        assert!(html.contains("-103.10 dBFS"));
        // CCIR field omitted when None.
        assert!(!html.contains("CCIR-468"));
    }

    // ─── GatedFrequencyResponse / noise_tail_start_s (#284) ──────────────

    fn sample_gated_points() -> Vec<GatedFrequencyResponsePoint> {
        vec![
            GatedFrequencyResponsePoint {
                freq_hz: 0.0,
                magnitude_db: -0.1,
                phase_deg: 0.0,
            },
            GatedFrequencyResponsePoint {
                freq_hz: 100.0,
                magnitude_db: -0.5,
                phase_deg: 142.68,
            },
            GatedFrequencyResponsePoint {
                freq_hz: 1_000.0,
                magnitude_db: -0.2,
                phase_deg: -18.44,
            },
            GatedFrequencyResponsePoint {
                freq_hz: 10_000.0,
                magnitude_db: -4.9,
                phase_deg: -171.19,
            },
        ]
    }

    #[test]
    fn gated_frequency_response_renders_title_gate_magnitude_phase_and_table() {
        let mut r = sample_fr_report();
        r.data = vec![MeasurementPayload {
            data: MeasurementData::GatedFrequencyResponse {
                points: sample_gated_points(),
            },
            standard: vec![
                StandardsCitation {
                    standard: "AES17-2020".into(),
                    clause: "Annex A.4.5".into(),
                    verified: false,
                },
                StandardsCitation {
                    standard: "Farina, AES 108th Convention preprint #5093 (2000)".into(),
                    clause: "§2".into(),
                    verified: false,
                },
            ],
            gate: Some(GateParams {
                gate_start_s: 0.0029,
                gate_length_s: 0.020,
                window_kind: "tukey0.25".into(),
                f_low_hz: 50.0,
            }),
        }];
        let html = render_html(&r);
        assert!(html.contains("Frequency Response (gated)"), "{html}");
        assert!(html.contains("<dt>gate</dt>"), "{html}");
        assert!(html.contains("tukey0.25"), "{html}");
        assert!(html.contains("50.0 Hz (= 1 / gate length)"), "{html}");
        // Two stacked SVGs: magnitude (full weight) then phase (thinner).
        assert_eq!(html.matches("<svg").count(), 2, "{html}");
        assert!(html.contains("trace-phase"), "{html}");
        // Table carries all three columns.
        assert!(html.contains("magnitude (dB)"), "{html}");
        assert!(html.contains("phase (°)"), "{html}");
        assert!(html.contains("142.68"), "{html}");
    }

    #[test]
    fn noise_tail_line_omitted_when_absent_and_rendered_when_set() {
        let mut r = sample_fr_report();
        r.data = vec![MeasurementPayload {
            data: MeasurementData::ImpulseResponse {
                sample_rate_hz: 48_000,
                f1_hz: 20.0,
                f2_hz: 20_000.0,
                duration_s: 3.0,
                linear_ir: vec![0.0, 1.0, 0.0],
                harmonics: vec![],
                noise_tail_start_s: None,
            },
            standard: vec![],
            gate: None,
        }];
        let html = render_html(&r);
        assert!(!html.contains("noise tail begins"), "{html}");

        r.data[0].data = MeasurementData::ImpulseResponse {
            sample_rate_hz: 48_000,
            f1_hz: 20.0,
            f2_hz: 20_000.0,
            duration_s: 3.0,
            linear_ir: vec![0.0, 1.0, 0.0],
            harmonics: vec![],
            noise_tail_start_s: Some(3.0),
        };
        let html = render_html(&r);
        assert!(html.contains("noise tail begins"), "{html}");
        assert!(html.contains("3000.0 ms after peak"), "{html}");
    }
}
