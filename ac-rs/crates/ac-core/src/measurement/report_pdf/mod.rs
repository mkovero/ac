//! Render a [`MeasurementReport`] as a PDF.
//!
//! Sibling to `report_html`. Uses `printpdf` (pure Rust) so the build
//! stays hermetic — no `wkhtmltopdf` or Chromium dependency. The 14
//! standard PDF core fonts only: no embedded fonts, no external assets.
//!
//! What each section *says* lives in `report_layout`, shared with the
//! HTML backend; this module only decides how to paint it. Page
//! geometry and the cursor that grows a document across pages live in
//! [`cursor`] — see its module docs for the points-versus-millimetres
//! trap that used to push most of a report off the page.
//!
//! Content grows onto as many A4 portrait pages as it needs and every
//! measured row is printed. The single-page version elided rows it
//! could not fit; a report that silently drops data is worse than a
//! long one, and the JSON report is no longer the only place the
//! numbers survive.

mod cursor;
mod metrics;
mod plot;

use anyhow::{Context, Result};
use printpdf::{BuiltinFont, Mm, PdfDocument};

use cursor::{Cursor, Fonts, PAGE_H_MM, PAGE_W_MM};

use crate::measurement::report::{MeasurementData, MeasurementPayload, MeasurementReport};
use crate::measurement::report_layout::{self as layout, Body};

/// Render `report` as a PDF byte stream.
pub fn render_pdf(report: &MeasurementReport) -> Result<Vec<u8>> {
    let (doc, page, layer) = PdfDocument::new(
        "ac \u{2014} MeasurementReport",
        Mm(PAGE_W_MM),
        Mm(PAGE_H_MM),
        "Layer 1",
    );
    let first = doc.get_page(page).get_layer(layer);

    let fonts = Fonts {
        regular: doc
            .add_builtin_font(BuiltinFont::Helvetica)
            .context("add Helvetica")?,
        bold: doc
            .add_builtin_font(BuiltinFont::HelveticaBold)
            .context("add Helvetica-Bold")?,
        mono: doc
            .add_builtin_font(BuiltinFont::Courier)
            .context("add Courier")?,
    };

    let mut cur = Cursor::new(&doc, &fonts, first);

    cur.title("ac MeasurementReport");
    for row in layout::header_rows(report) {
        cur.kv(row.label, &row.value);
    }

    for section in layout::sections(report) {
        cur.heading(section.title);
        match &section.body {
            Body::Rows(rows) => {
                for row in rows {
                    cur.kv(row.label, &row.value);
                }
            }
            Body::Note(note) => cur.note(note),
        }
    }

    for payload in &report.data {
        draw_payload(&mut cur, payload);
    }

    if let Some(notes) = &report.notes {
        cur.heading("Notes");
        for line in notes.lines() {
            cur.note(line);
        }
    }

    doc.save_to_bytes().context("serialize PDF")
}

/// One payload: heading, its citation(s) and gate block (when present),
/// then the data-specific body.
fn draw_payload(cur: &mut Cursor, payload: &MeasurementPayload) {
    cur.heading(payload.data.display_title());
    for row in layout::payload_meta_rows(payload) {
        cur.kv(row.label, &row.value);
    }
    draw_payload_body(cur, &payload.data);
}

fn draw_payload_body(cur: &mut Cursor, d: &MeasurementData) {
    match d {
        MeasurementData::FrequencyResponse { points } => {
            if points.is_empty() {
                cur.note("(no points)");
                return;
            }
            plot::draw(cur, &layout::frequency_response_series(points));
            cur.table(
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
            for row in layout::spectrum_bands_rows(*bpo, class) {
                cur.kv(row.label, &row.value);
            }
            cur.table(
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
            for row in layout::impulse_response_rows(
                *sample_rate_hz,
                *f1_hz,
                *f2_hz,
                *duration_s,
                linear_ir.len(),
                &orders,
                *noise_tail_start_s,
            ) {
                cur.kv(row.label, &row.value);
            }
        }
        MeasurementData::NoiseResult {
            sample_rate_hz,
            duration_s,
            unweighted_dbfs,
            a_weighted_dbfs,
            ccir_weighted_dbfs,
        } => {
            for row in layout::noise_result_rows(
                *sample_rate_hz,
                *duration_s,
                *unweighted_dbfs,
                *a_weighted_dbfs,
                *ccir_weighted_dbfs,
            ) {
                cur.kv(row.label, &row.value);
            }
        }
        MeasurementData::GatedFrequencyResponse { points } => {
            if points.is_empty() {
                cur.note("(no points)");
                return;
            }
            // Magnitude only. A fixed page pushes real table rows off
            // before a phase panel earns its space (#284's UX review);
            // phase rides as a third table column instead.
            plot::draw(cur, &layout::gated_magnitude_series(points));
            cur.table(layout::gated_columns(), &layout::gated_cells(points));
        }
    }
}

/// Text a `printpdf` page carries, for tests that need to see what
/// actually landed on the page rather than only that bytes were
/// produced.
#[cfg(test)]
fn page_texts(pdf: &[u8]) -> Vec<Vec<String>> {
    let doc = printpdf::lopdf::Document::load_mem(pdf).expect("parse pdf");
    let mut pages: Vec<Vec<String>> = Vec::new();
    let mut numbers: Vec<u32> = doc.get_pages().keys().copied().collect();
    numbers.sort_unstable();
    for n in numbers {
        let text = doc.extract_text(&[n]).unwrap_or_default();
        pages.push(text.lines().map(|l| l.trim().to_string()).collect());
    }
    pages
}

/// Every line of text in the document, in page order.
#[cfg(test)]
fn all_text(pdf: &[u8]) -> String {
    page_texts(pdf).concat().join("\n")
}
#[cfg(test)]
mod tests {
    use super::cursor::winansi_char;
    use super::metrics::{text_mm, Face};
    use super::*;
    use crate::measurement::report::{
        FrequencyResponsePoint, GateParams, GatedFrequencyResponsePoint, IntegrationParams,
        MeasurementData, MeasurementMethod, MeasurementPayload, MeasurementReport,
        PositionSnapshot, StimulusParams, SCHEMA_VERSION,
    };

    fn sample_report() -> MeasurementReport {
        MeasurementReport {
            schema_version: SCHEMA_VERSION,
            ac_version: "0.2.0".into(),
            timestamp_utc: "2026-04-23T10:00:00Z".into(),
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
                    points: (0..3)
                        .map(|i| FrequencyResponsePoint {
                            freq_hz: 100.0 * 10f64.powi(i),
                            fundamental_dbfs: -20.0 - i as f64 * 0.1,
                            thd_pct: 0.001 * (i + 1) as f64,
                            thdn_pct: 0.002 * (i + 1) as f64,
                            noise_floor_dbfs: -120.0,
                            linear_rms: 0.0707,
                            clipping: false,
                            ac_coupled: false,
                        })
                        .collect(),
                },
                standard: vec![],
                gate: None,
            }],
            notes: Some("unit test".into()),
            processing_chain: crate::measurement::report::ProcessingChain::default(),
        }
    }

    #[test]
    fn render_pdf_produces_valid_header() {
        let pdf = render_pdf(&sample_report()).expect("render");
        assert!(
            pdf.starts_with(b"%PDF-"),
            "wrong magic: {:?}",
            &pdf[..6.min(pdf.len())]
        );
        assert!(pdf.windows(5).any(|w| w == b"%%EOF"), "missing EOF marker");
        assert!(pdf.len() > 1500, "pdf too small: {} bytes", pdf.len());
    }

    #[test]
    fn render_pdf_handles_all_data_variants() {
        let mut r = sample_report();
        r.data = vec![MeasurementPayload {
            data: MeasurementData::SpectrumBands {
                bpo: 3,
                class: "Class 1".into(),
                centres_hz: vec![100.0, 125.0, 160.0],
                levels_dbfs: vec![-40.0, -35.0, -38.0],
            },
            standard: vec![],
            gate: None,
        }];
        assert!(render_pdf(&r).is_ok());

        r.data = vec![MeasurementPayload {
            data: MeasurementData::NoiseResult {
                sample_rate_hz: 48_000,
                duration_s: 1.0,
                unweighted_dbfs: -98.0,
                a_weighted_dbfs: -103.0,
                ccir_weighted_dbfs: Some(-95.0),
            },
            standard: vec![],
            gate: None,
        }];
        assert!(render_pdf(&r).is_ok());
    }

    #[test]
    fn render_pdf_empty_points_does_not_crash() {
        let mut r = sample_report();
        r.data = vec![MeasurementPayload {
            data: MeasurementData::FrequencyResponse { points: vec![] },
            standard: vec![],
            gate: None,
        }];
        assert!(render_pdf(&r).is_ok());
    }

    #[test]
    fn render_pdf_handles_multi_payload_and_gate_and_position() {
        let mut r = sample_report();
        r.position = Some(PositionSnapshot {
            temperature_c: Some(21.3),
            relative_humidity_pct: Some(45.0),
            source_height_m: Some(1.2),
            receiver_height_m: Some(1.1),
            distance_m: Some(1.0),
        });
        r.data.push(MeasurementPayload {
            data: MeasurementData::SpectrumBands {
                bpo: 3,
                class: "Class 1".into(),
                centres_hz: vec![100.0],
                levels_dbfs: vec![-30.0],
            },
            standard: vec![],
            gate: Some(GateParams {
                gate_start_s: 0.0029,
                gate_length_s: 0.020,
                window_kind: "tukey0.25".into(),
                f_low_hz: 50.0,
            }),
        });
        let pdf = render_pdf(&r).expect("render");
        assert!(pdf.starts_with(b"%PDF-"));
    }

    // ─── GatedFrequencyResponse / noise_tail_start_s (#284) ──────────────

    #[test]
    fn render_pdf_handles_gated_frequency_response_payload() {
        let mut r = sample_report();
        r.data = vec![MeasurementPayload {
            data: MeasurementData::GatedFrequencyResponse {
                points: vec![
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
                ],
            },
            standard: vec![],
            gate: Some(GateParams {
                gate_start_s: 0.0,
                gate_length_s: 0.020,
                window_kind: "tukey0.25".into(),
                f_low_hz: 50.0,
            }),
        }];
        let pdf = render_pdf(&r).expect("render");
        assert!(pdf.starts_with(b"%PDF-"));
    }

    #[test]
    fn render_pdf_gated_frequency_response_empty_points_does_not_crash() {
        let mut r = sample_report();
        r.data = vec![MeasurementPayload {
            data: MeasurementData::GatedFrequencyResponse { points: vec![] },
            standard: vec![],
            gate: None,
        }];
        assert!(render_pdf(&r).is_ok());
    }

    #[test]
    fn render_pdf_handles_impulse_response_with_noise_tail() {
        let mut r = sample_report();
        r.data = vec![MeasurementPayload {
            data: MeasurementData::ImpulseResponse {
                sample_rate_hz: 48_000,
                f1_hz: 20.0,
                f2_hz: 20_000.0,
                duration_s: 3.0,
                linear_ir: vec![0.0, 1.0, 0.0],
                harmonics: vec![],
                noise_tail_start_s: Some(3.0),
            },
            standard: vec![],
            gate: None,
        }];
        assert!(render_pdf(&r).is_ok());
    }

    // ─── Placement (#P0: content used to land off the page) ──────────────

    /// Every `Td` text-positioning operator in the document, as
    /// `(page, x_mm, y_mm)`.
    ///
    /// Text extraction alone cannot catch the bug this guards: a run
    /// placed at a negative `Mm` is still in the content stream and
    /// still extracts, it simply never appears on paper. Only the
    /// coordinates say whether a reader will see it.
    fn text_positions(pdf: &[u8]) -> Vec<(u32, f32, f32)> {
        use printpdf::lopdf::content::Content;
        use printpdf::lopdf::{Document, Object};

        fn num(o: &Object) -> f32 {
            match o {
                Object::Real(v) => *v,
                Object::Integer(v) => *v as f32,
                other => panic!("non-numeric Td operand: {other:?}"),
            }
        }

        const PT_PER_MM: f32 = 72.0 / 25.4;
        let doc = Document::load_mem(pdf).expect("parse pdf");
        let mut out = Vec::new();
        let mut pages: Vec<(u32, _)> = doc.get_pages().into_iter().collect();
        pages.sort_by_key(|(n, _)| *n);
        for (number, id) in pages {
            let data = doc.get_page_content(id).expect("page content");
            for op in Content::decode(&data).expect("decode content").operations {
                if op.operator == "Td" {
                    assert_eq!(op.operands.len(), 2, "Td arity");
                    out.push((
                        number,
                        num(&op.operands[0]) / PT_PER_MM,
                        num(&op.operands[1]) / PT_PER_MM,
                    ));
                }
            }
        }
        out
    }

    /// Every text run in the document, as `(page, x_mm, width_mm, text)`.
    ///
    /// [`text_positions`] reads where a run *starts*. That cannot see a
    /// line whose start is inside the margin and whose glyphs are not:
    /// the width depends on the font the run was set in, so the font
    /// has to be read out of the page resources and the run measured
    /// against its own metrics.
    fn text_runs(pdf: &[u8]) -> Vec<(u32, f32, f32, String)> {
        use printpdf::lopdf::content::Content;
        use printpdf::lopdf::{Document, Object};

        const PT_PER_MM: f32 = 72.0 / 25.4;

        fn num(o: &Object) -> f32 {
            match o {
                Object::Real(v) => *v,
                Object::Integer(v) => *v as f32,
                other => panic!("non-numeric operand: {other:?}"),
            }
        }

        let doc = Document::load_mem(pdf).expect("parse pdf");
        let mut out = Vec::new();
        let mut pages: Vec<(u32, _)> = doc.get_pages().into_iter().collect();
        pages.sort_by_key(|(n, _)| *n);
        for (number, id) in pages {
            let data = doc.get_page_content(id).expect("page content");
            let (mut face, mut size, mut x) = (Face::Regular, 0.0f32, 0.0f32);
            for op in Content::decode(&data).expect("decode content").operations {
                match op.operator.as_str() {
                    "Tf" => {
                        // `printpdf` names a core font's resource after
                        // the font itself, so the operand is the face.
                        face = match op.operands[0].as_name().expect("font name") {
                            b"Helvetica" => Face::Regular,
                            b"Helvetica-Bold" => Face::Bold,
                            b"Courier" => Face::Mono,
                            other => panic!("unmetered font {}", String::from_utf8_lossy(other)),
                        };
                        size = num(&op.operands[1]);
                    }
                    "Td" => x = num(&op.operands[0]) / PT_PER_MM,
                    "Tj" => {
                        let bytes = op.operands[0].as_str().expect("string operand");
                        let text: String = bytes.iter().map(|b| winansi_char(*b)).collect();
                        out.push((number, x, text_mm(&text, size, face), text));
                    }
                    _ => {}
                }
            }
        }
        out
    }

    fn page_count(pdf: &[u8]) -> usize {
        printpdf::lopdf::Document::load_mem(pdf)
            .expect("parse pdf")
            .get_pages()
            .len()
    }

    /// A report with every optional section populated and a sweep long
    /// enough to need more than one page.
    fn full_report(n_points: usize) -> MeasurementReport {
        use crate::measurement::report::{CalibrationSnapshot, MicResponseRef, StandardsCitation};
        let mut r = sample_report();
        r.calibration = Some(CalibrationSnapshot {
            output_channel: 0,
            input_channel: 1,
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
        r.position = Some(PositionSnapshot {
            temperature_c: Some(21.3),
            relative_humidity_pct: Some(45.0),
            source_height_m: Some(1.2),
            receiver_height_m: Some(1.1),
            distance_m: Some(1.0),
        });
        r.data = vec![MeasurementPayload {
            data: MeasurementData::FrequencyResponse {
                points: (0..n_points)
                    .map(|i| FrequencyResponsePoint {
                        freq_hz: 20.0 * 1.05f64.powi(i as i32),
                        fundamental_dbfs: -20.0,
                        thd_pct: 0.5,
                        thdn_pct: 0.9,
                        noise_floor_dbfs: -120.0,
                        linear_rms: 0.0707,
                        clipping: false,
                        ac_coupled: false,
                    })
                    .collect(),
            },
            standard: vec![StandardsCitation {
                standard: "IEC 60268-3:2018".into(),
                clause: "\u{a7}15.12.3".into(),
                verified: false,
            }],
            gate: None,
        }];
        r
    }

    #[test]
    fn every_text_run_lands_inside_the_page_margins() {
        // The regression this exists for: font sizes are points, page
        // positions are millimetres, and subtracting one from the other
        // walked the cursor off the bottom of the page. Everything from
        // Environment onward used to sit at a negative Mm — present in
        // the file, absent from the paper.
        for pdf in [
            render_pdf(&sample_report()).expect("render"),
            render_pdf(&full_report(120)).expect("render"),
        ] {
            let positions = text_positions(&pdf);
            assert!(!positions.is_empty(), "no text placed at all");
            for (page, x, y) in positions {
                assert!(
                    (15.0..=297.0 - 15.0).contains(&y),
                    "page {page}: y={y:.1} mm is outside the 15 mm margins"
                );
                assert!(
                    (15.0..=210.0 - 15.0).contains(&x),
                    "page {page}: x={x:.1} mm is outside the 15 mm margins"
                );
            }
        }
    }

    #[test]
    fn the_frequency_response_table_actually_prints_its_rows() {
        // The single-page renderer computed a negative row budget,
        // saturated it to zero, and printed "... 3 more rows" in place
        // of every row it had.
        let pdf = render_pdf(&sample_report()).expect("render");
        let text = all_text(&pdf);
        for cell in ["100.00", "1000.00", "10000.00"] {
            assert!(text.contains(cell), "missing table cell {cell} in:\n{text}");
        }
        assert!(
            !text.contains("more rows"),
            "rows were elided on a report that fits:\n{text}"
        );
    }

    #[test]
    fn thd_is_printed_as_stored_not_scaled_a_second_time() {
        // `thd_pct` is already a percentage; this backend multiplied it
        // by 100 again, printing 0.5 % as 50 %.
        let pdf = render_pdf(&full_report(3)).expect("render");
        let text = all_text(&pdf);
        assert!(text.contains("0.5000"), "expected 0.5000 in:\n{text}");
        assert!(!text.contains("50.0000"), "THD scaled twice in:\n{text}");
    }

    #[test]
    fn a_long_sweep_paginates_rather_than_dropping_rows() {
        let pdf = render_pdf(&full_report(240)).expect("render");
        assert!(
            page_count(&pdf) > 1,
            "240 points must not claim to fit one page"
        );
        let text = all_text(&pdf);
        // First and last point both survive to paper.
        assert!(text.contains("20.00"), "first point missing");
        assert!(!text.contains("more rows"), "rows elided instead of paged");
    }

    #[test]
    fn optional_sections_reach_the_page_instead_of_running_off_it() {
        let pdf = render_pdf(&full_report(3)).expect("render");
        let text = all_text(&pdf);
        for expected in [
            "Calibration",
            "Environment",
            "Processing",
            "Frequency Response",
            "speed of sound",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
    }

    #[test]
    fn characters_the_core_fonts_lack_reach_the_page_as_ascii() {
        // `printpdf` drops an unencodable glyph without a word, so
        // "20.0 Hz -> 20000.0 Hz" used to print as "20.0 Hz 20000.0 Hz".
        let mut r = sample_report();
        r.method = MeasurementMethod::SweptSine {
            f1_hz: 20.0,
            f2_hz: 20_000.0,
            duration_s: 3.0,
        };
        let text = all_text(&render_pdf(&r).expect("render"));
        assert!(
            text.contains("20.0 Hz -> 20000.0 Hz"),
            "range arrow lost in:\n{text}"
        );
    }

    #[test]
    fn an_uncalibrated_report_says_so() {
        let pdf = render_pdf(&sample_report()).expect("render");
        let text = all_text(&pdf);
        assert!(text.contains("Calibration"), "{text}");
        assert!(text.contains("uncalibrated"), "{text}");
    }

    #[test]
    fn no_text_run_is_drawn_past_the_right_margin() {
        // `note` wrapped its lines to a Courier character budget and
        // drew them in Helvetica, so a note of capitals or hex ran off
        // the right edge — placed inside the margin, painted outside
        // it, and silent either way. Measuring each run in the font it
        // was actually set in is the only way to see that.
        let mut wide = full_report(120);
        wide.notes = Some(
            "MEASUREMENT ABORTED: CHECK ROUTING, CLOCK AND GAIN BEFORE RETRYING THIS SWEEP\n\
             WWW-MMM-WWW-MMM-WWW-MMM-WWW-MMM-WWW-MMM-WWW-MMM-WWW-MMM-WWW-MMM-WWW-MMM-WWW-MMM-WWW-MMM"
                .into(),
        );
        for pdf in [
            render_pdf(&sample_report()).expect("render"),
            render_pdf(&wide).expect("render"),
        ] {
            let runs = text_runs(&pdf);
            assert!(!runs.is_empty(), "no text placed at all");
            for (page, x, width, text) in runs {
                // A tick label clamped flush to the margin lands on it
                // to within f32 rounding; a micrometre is not an
                // overrun.
                let end = x + width;
                assert!(
                    end <= 210.0 - 15.0 + 0.01,
                    "page {page}: run ends at {end:.1} mm, past the 195 mm margin: {text:?}"
                );
            }
        }
    }

    #[test]
    fn both_voltage_calibration_directions_are_printed() {
        // The input side alone used to be rendered, so an
        // output-referred calibration silently vanished from print.
        let pdf = render_pdf(&full_report(3)).expect("render");
        let text = all_text(&pdf);
        assert!(
            text.contains("0dBFS out"),
            "output-side row missing:\n{text}"
        );
        assert!(text.contains("0dBFS in"), "input-side row missing:\n{text}");
    }
}
