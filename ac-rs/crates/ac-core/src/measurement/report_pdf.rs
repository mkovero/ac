//! Render a [`MeasurementReport`] as a PDF.
//!
//! Sibling to `report_html`. Uses `printpdf` (pure Rust) so the build
//! stays hermetic — no `wkhtmltopdf` or Chromium dependency. The layout
//! mirrors the HTML version in content but is rendered with the 14
//! standard PDF core fonts only: no embedded fonts, no external assets.
//!
//! Single A4 portrait page today. If a `FrequencyResponse` exceeds what
//! fits (rough cutoff: ~35 rows past the plot), the table is truncated
//! with an ellipsis row — the JSON report stays authoritative.

use anyhow::{Context, Result};
use printpdf::{
    BuiltinFont, Color, IndirectFontRef, Line, Mm, PdfDocument, PdfLayerReference, Point, Rgb,
};

use crate::measurement::report::{
    FrequencyResponsePoint, MeasurementData, MeasurementMethod, MeasurementReport,
};

// Page geometry — A4 portrait.
const PAGE_W_MM: f32 = 210.0;
const PAGE_H_MM: f32 = 297.0;
const MARGIN_MM: f32 = 15.0;

// Typography.
const SIZE_TITLE: f32 = 18.0;
const SIZE_H2: f32    = 13.0;
const SIZE_BODY: f32  = 9.5;
const SIZE_SMALL: f32 = 8.0;

// Plot box.
const PLOT_H_MM: f32 = 75.0;

/// Render `report` as a PDF byte stream.
pub fn render_pdf(report: &MeasurementReport) -> Result<Vec<u8>> {
    let (doc, page, layer) =
        PdfDocument::new("ac — MeasurementReport", Mm(PAGE_W_MM), Mm(PAGE_H_MM), "Layer 1");
    let current = doc.get_page(page).get_layer(layer);

    let font      = doc.add_builtin_font(BuiltinFont::Helvetica)
        .context("add Helvetica")?;
    let font_bold = doc.add_builtin_font(BuiltinFont::HelveticaBold)
        .context("add Helvetica-Bold")?;
    let font_mono = doc.add_builtin_font(BuiltinFont::Courier)
        .context("add Courier")?;

    let mut y = PAGE_H_MM - MARGIN_MM;

    y = draw_title(&current, &font_bold, y, "ac MeasurementReport");
    y = draw_header(&current, &font, &font_mono, y, report);
    y = draw_method(&current, &font_bold, &font, &font_mono, y, report);
    y = draw_stimulus(&current, &font_bold, &font_mono, y, report);

    match &report.data {
        MeasurementData::FrequencyResponse { points } => {
            y = draw_freq_response(&current, &font_bold, &font_mono, y, points);
        }
        MeasurementData::SpectrumBands { bpo, class, centres_hz, levels_dbfs } => {
            y = draw_spectrum_bands(
                &current, &font_bold, &font_mono, y,
                *bpo, class, centres_hz, levels_dbfs,
            );
        }
        MeasurementData::ImpulseResponse { sample_rate_hz, f1_hz, f2_hz, duration_s, linear_ir, harmonics } => {
            y = draw_impulse_response(
                &current, &font_bold, &font_mono, y,
                *sample_rate_hz, *f1_hz, *f2_hz, *duration_s, linear_ir.len(), harmonics.len(),
            );
        }
        MeasurementData::NoiseResult { sample_rate_hz, duration_s, unweighted_dbfs, a_weighted_dbfs, ccir_weighted_dbfs } => {
            y = draw_noise_result(
                &current, &font_bold, &font_mono, y,
                *sample_rate_hz, *duration_s, *unweighted_dbfs, *a_weighted_dbfs, *ccir_weighted_dbfs,
            );
        }
    }

    if let Some(notes) = &report.notes {
        let _ = draw_notes(&current, &font_bold, &font, y, notes);
    }

    doc.save_to_bytes().context("serialize PDF")
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

fn draw_title(
    layer: &PdfLayerReference,
    font_bold: &IndirectFontRef,
    y: f32,
    text: &str,
) -> f32 {
    layer.use_text(text, SIZE_TITLE, Mm(MARGIN_MM), Mm(y - SIZE_TITLE * 0.35), font_bold);
    let after = y - SIZE_TITLE * 0.9;
    draw_hline(layer, after - 1.5, MARGIN_MM, PAGE_W_MM - MARGIN_MM, 0.7);
    after - 5.0
}

fn draw_header(
    layer: &PdfLayerReference,
    font: &IndirectFontRef,
    font_mono: &IndirectFontRef,
    y: f32,
    r: &MeasurementReport,
) -> f32 {
    let mut y = y;
    for (label, value) in [
        ("schema",     format!("v{}", r.schema_version)),
        ("ac version", r.ac_version.clone()),
        ("timestamp",  r.timestamp_utc.clone()),
    ] {
        y = kv_row(layer, font, font_mono, y, label, &value);
    }
    y - 2.0
}

fn draw_method(
    layer: &PdfLayerReference,
    font_bold: &IndirectFontRef,
    font: &IndirectFontRef,
    font_mono: &IndirectFontRef,
    y: f32,
    r: &MeasurementReport,
) -> f32 {
    let mut y = section_heading(layer, font_bold, y, "Method");
    match &r.method {
        MeasurementMethod::SteppedSine { n_points, standard } => {
            y = kv_row(layer, font, font_mono, y, "kind",
                       &format!("stepped_sine ({n_points} points)"));
            if let Some(s) = standard {
                y = kv_row(layer, font, font_mono, y, "standard",
                           &format!("{} — {}{}",
                                    s.standard, s.clause,
                                    if s.verified { " ✓ verified" } else { "" }));
            }
        }
    }
    y = kv_row(layer, font, font_mono, y, "integration",
               &format!("{:.3} s, window={}", r.integration.duration_s, r.integration.window));
    y - 2.0
}

fn draw_stimulus(
    layer: &PdfLayerReference,
    font_bold: &IndirectFontRef,
    font_mono: &IndirectFontRef,
    y: f32,
    r: &MeasurementReport,
) -> f32 {
    let mut y = section_heading(layer, font_bold, y, "Stimulus");
    for (label, value) in [
        ("sample rate",  format!("{} Hz", r.stimulus.sample_rate_hz)),
        ("f_start",      format!("{:.2} Hz", r.stimulus.f_start_hz)),
        ("f_stop",       format!("{:.2} Hz", r.stimulus.f_stop_hz)),
        ("level",        format!("{:.2} dBFS", r.stimulus.level_dbfs)),
        ("n_points",     r.stimulus.n_points.to_string()),
    ] {
        y = kv_row(layer, font_bold, font_mono, y, label, &value);
    }
    y - 2.0
}

fn draw_freq_response(
    layer: &PdfLayerReference,
    font_bold: &IndirectFontRef,
    font_mono: &IndirectFontRef,
    y: f32,
    points: &[FrequencyResponsePoint],
) -> f32 {
    let mut y = section_heading(layer, font_bold, y, "Frequency Response");
    if points.is_empty() {
        layer.use_text("(no points)", SIZE_BODY, Mm(MARGIN_MM), Mm(y - SIZE_BODY), font_mono);
        return y - SIZE_BODY - 2.0;
    }

    // Plot box.
    let plot_x0 = MARGIN_MM + 14.0;              // leave room for dB ticks
    let plot_x1 = PAGE_W_MM - MARGIN_MM;
    let plot_y1 = y - 2.0;
    let plot_y0 = plot_y1 - PLOT_H_MM;

    draw_rect_outline(layer, plot_x0, plot_y0, plot_x1, plot_y1, 0.4);

    // Axis domains.
    let (fmin, fmax) = log_x_domain(points);
    let (dmin, dmax) = db_y_domain(points);

    // Grid.
    draw_log_freq_grid(layer, font_mono, plot_x0, plot_y0, plot_x1, plot_y1, fmin, fmax);
    draw_db_grid(layer, font_mono, plot_x0, plot_y0, plot_x1, plot_y1, dmin, dmax);

    // Trace.
    let fspan = (fmax.log10() - fmin.log10()).max(1e-9);
    let dspan = (dmax - dmin).max(1e-9);
    let trace: Vec<(Point, bool)> = points.iter().map(|p| {
        let x = lerp(plot_x0, plot_x1,
                     ((p.freq_hz.max(fmin)).log10() - fmin.log10()) as f32 / fspan as f32);
        let yv = lerp(plot_y0, plot_y1,
                      (p.fundamental_dbfs - dmin) as f32 / dspan as f32);
        (Point::new(Mm(x), Mm(yv)), false)
    }).collect();
    if trace.len() >= 2 {
        layer.set_outline_color(Color::Rgb(Rgb::new(0.12, 0.47, 0.71, None)));
        layer.set_outline_thickness(0.6);
        layer.add_line(Line { points: trace, is_closed: false });
        layer.set_outline_color(Color::Rgb(Rgb::new(0.0, 0.0, 0.0, None)));
        layer.set_outline_thickness(0.3);
    }

    y = plot_y0 - 6.0;

    // Table.
    let widths = [24.0, 24.0, 24.0, 24.0, 28.0];
    y = table_header(
        layer, font_bold, y,
        &["freq_hz", "fund_dBFS", "THD_%", "THD+N_%", "noise_dBFS"],
        &widths,
    );
    let row_h = SIZE_BODY * 1.25;
    let max_rows = ((y - MARGIN_MM) / row_h).floor() as usize;
    let show = max_rows.saturating_sub(1).min(points.len());
    for p in &points[..show] {
        y = table_row(
            layer, font_mono, y,
            &[
                fmt_f(p.freq_hz, 1),
                fmt_f(p.fundamental_dbfs, 2),
                fmt_f(p.thd_pct * 100.0, 4),
                fmt_f(p.thdn_pct * 100.0, 4),
                fmt_f(p.noise_floor_dbfs, 2),
            ],
            &widths,
        );
    }
    if show < points.len() {
        layer.use_text(
            format!("… {} more rows — see JSON report for full data",
                    points.len() - show),
            SIZE_SMALL, Mm(MARGIN_MM), Mm(y - SIZE_SMALL), font_mono,
        );
        y -= SIZE_SMALL + 1.0;
    }
    y
}

fn draw_spectrum_bands(
    layer: &PdfLayerReference,
    font_bold: &IndirectFontRef,
    font_mono: &IndirectFontRef,
    y: f32,
    bpo: u32,
    class: &str,
    centres: &[f64],
    levels: &[f64],
) -> f32 {
    let mut y = section_heading(layer, font_bold, y, "Spectrum Bands");
    y = kv_row(layer, font_bold, font_mono, y, "bpo", &bpo.to_string());
    y = kv_row(layer, font_bold, font_mono, y, "class", class);
    y -= 2.0;
    let widths = [30.0, 30.0];
    y = table_header(layer, font_bold, y, &["centre_Hz", "level_dBFS"], &widths);
    for (c, l) in centres.iter().zip(levels.iter()) {
        if y < MARGIN_MM + SIZE_BODY { break; }
        y = table_row(layer, font_mono, y, &[fmt_f(*c, 2), fmt_f(*l, 2)], &widths);
    }
    y
}

#[allow(clippy::too_many_arguments)]
fn draw_impulse_response(
    layer: &PdfLayerReference,
    font_bold: &IndirectFontRef,
    font_mono: &IndirectFontRef,
    y: f32,
    sr_hz: u32,
    f1_hz: f64,
    f2_hz: f64,
    duration_s: f64,
    linear_len: usize,
    n_harmonics: usize,
) -> f32 {
    let mut y = section_heading(layer, font_bold, y, "Impulse Response (Farina log sweep)");
    for (label, value) in [
        ("sample rate",    format!("{sr_hz} Hz")),
        ("f1",             format!("{f1_hz:.2} Hz")),
        ("f2",             format!("{f2_hz:.2} Hz")),
        ("sweep duration", format!("{duration_s:.3} s")),
        ("linear IR len",  format!("{linear_len} samples")),
        ("harmonic IRs",   format!("{n_harmonics}")),
    ] {
        y = kv_row(layer, font_bold, font_mono, y, label, &value);
    }
    y - 2.0
}

#[allow(clippy::too_many_arguments)]
fn draw_noise_result(
    layer: &PdfLayerReference,
    font_bold: &IndirectFontRef,
    font_mono: &IndirectFontRef,
    y: f32,
    sr_hz: u32,
    duration_s: f64,
    unweighted_dbfs: f64,
    a_weighted_dbfs: f64,
    ccir_weighted_dbfs: Option<f64>,
) -> f32 {
    let mut y = section_heading(layer, font_bold, y, "Idle-channel Noise (AES17)");
    let mut rows: Vec<(&str, String)> = vec![
        ("sample rate", format!("{sr_hz} Hz")),
        ("duration",    format!("{duration_s:.3} s")),
        ("unweighted",  format!("{unweighted_dbfs:.2} dBFS")),
        ("A-weighted",  format!("{a_weighted_dbfs:.2} dBFS")),
    ];
    if let Some(c) = ccir_weighted_dbfs {
        rows.push(("CCIR-468", format!("{c:.2} dBFS (quasi-peak)")));
    }
    for (label, value) in rows {
        y = kv_row(layer, font_bold, font_mono, y, label, &value);
    }
    y - 2.0
}

fn draw_notes(
    layer: &PdfLayerReference,
    font_bold: &IndirectFontRef,
    font: &IndirectFontRef,
    y: f32,
    notes: &str,
) -> f32 {
    let mut y = section_heading(layer, font_bold, y, "Notes");
    for line in notes.lines().take(20) {
        if y < MARGIN_MM + SIZE_BODY { break; }
        layer.use_text(line, SIZE_BODY, Mm(MARGIN_MM), Mm(y - SIZE_BODY), font);
        y -= SIZE_BODY + 1.0;
    }
    y
}

// ---------------------------------------------------------------------------
// Low-level primitives
// ---------------------------------------------------------------------------

fn section_heading(
    layer: &PdfLayerReference,
    font_bold: &IndirectFontRef,
    y: f32,
    text: &str,
) -> f32 {
    let y = y - 3.0;
    layer.use_text(text, SIZE_H2, Mm(MARGIN_MM), Mm(y - SIZE_H2 * 0.3), font_bold);
    y - SIZE_H2 * 0.9 - 1.5
}

fn kv_row(
    layer: &PdfLayerReference,
    font_label: &IndirectFontRef,
    font_value: &IndirectFontRef,
    y: f32,
    label: &str,
    value: &str,
) -> f32 {
    let y_text = y - SIZE_BODY;
    layer.use_text(label, SIZE_BODY, Mm(MARGIN_MM), Mm(y_text), font_label);
    layer.use_text(value, SIZE_BODY, Mm(MARGIN_MM + 32.0), Mm(y_text), font_value);
    y_text - 1.5
}

fn table_header(
    layer: &PdfLayerReference,
    font_bold: &IndirectFontRef,
    y: f32,
    headers: &[&str],
    widths_mm: &[f32],
) -> f32 {
    let y_text = y - SIZE_BODY;
    let mut x = MARGIN_MM;
    for (h, w) in headers.iter().zip(widths_mm.iter()) {
        layer.use_text(*h, SIZE_BODY, Mm(x), Mm(y_text), font_bold);
        x += w;
    }
    draw_hline(layer, y_text - 0.8, MARGIN_MM, PAGE_W_MM - MARGIN_MM, 0.3);
    y_text - 2.0
}

fn table_row(
    layer: &PdfLayerReference,
    font_mono: &IndirectFontRef,
    y: f32,
    cells: &[String],
    widths_mm: &[f32],
) -> f32 {
    let y_text = y - SIZE_BODY;
    let mut x = MARGIN_MM;
    for (c, w) in cells.iter().zip(widths_mm.iter()) {
        layer.use_text(c, SIZE_BODY, Mm(x), Mm(y_text), font_mono);
        x += w;
    }
    y_text - 1.0
}

fn draw_hline(layer: &PdfLayerReference, y_mm: f32, x0_mm: f32, x1_mm: f32, thickness: f32) {
    layer.set_outline_thickness(thickness);
    layer.add_line(Line {
        points: vec![
            (Point::new(Mm(x0_mm), Mm(y_mm)), false),
            (Point::new(Mm(x1_mm), Mm(y_mm)), false),
        ],
        is_closed: false,
    });
}

fn draw_vline(layer: &PdfLayerReference, x_mm: f32, y0_mm: f32, y1_mm: f32, thickness: f32) {
    layer.set_outline_thickness(thickness);
    layer.add_line(Line {
        points: vec![
            (Point::new(Mm(x_mm), Mm(y0_mm)), false),
            (Point::new(Mm(x_mm), Mm(y1_mm)), false),
        ],
        is_closed: false,
    });
}

fn draw_rect_outline(
    layer: &PdfLayerReference,
    x0: f32, y0: f32, x1: f32, y1: f32,
    thickness: f32,
) {
    layer.set_outline_thickness(thickness);
    layer.add_line(Line {
        points: vec![
            (Point::new(Mm(x0), Mm(y0)), false),
            (Point::new(Mm(x1), Mm(y0)), false),
            (Point::new(Mm(x1), Mm(y1)), false),
            (Point::new(Mm(x0), Mm(y1)), false),
        ],
        is_closed: true,
    });
}

fn draw_log_freq_grid(
    layer: &PdfLayerReference,
    font_mono: &IndirectFontRef,
    x0: f32, y0: f32, x1: f32, y1: f32,
    fmin: f64, fmax: f64,
) {
    let lo = fmin.log10();
    let hi = fmax.log10();
    let span = (hi - lo).max(1e-9);
    let mut decade = lo.floor() as i32;
    while (decade as f64) <= hi {
        let f = 10f64.powi(decade);
        if f >= fmin && f <= fmax {
            let x = lerp(x0, x1, ((f.log10() - lo) / span) as f32);
            draw_vline(layer, x, y0, y1, 0.15);
            let label = if f >= 1000.0 { format!("{:.0}k", f / 1000.0) } else { format!("{:.0}", f) };
            layer.use_text(label, SIZE_SMALL, Mm(x - 2.5), Mm(y0 - SIZE_SMALL - 0.5), font_mono);
        }
        decade += 1;
    }
}

fn draw_db_grid(
    layer: &PdfLayerReference,
    font_mono: &IndirectFontRef,
    x0: f32, y0: f32, x1: f32, y1: f32,
    dmin: f64, dmax: f64,
) {
    let span = (dmax - dmin).max(1e-9);
    let step = nice_db_step(span);
    let start = (dmin / step).ceil() * step;
    let mut v = start;
    while v <= dmax {
        let y = lerp(y0, y1, ((v - dmin) / span) as f32);
        draw_hline(layer, y, x0, x1, 0.15);
        layer.use_text(format!("{v:.0}"), SIZE_SMALL, Mm(x0 - 10.0), Mm(y - SIZE_SMALL * 0.3), font_mono);
        v += step;
    }
}

// ---------------------------------------------------------------------------
// Axis helpers
// ---------------------------------------------------------------------------

fn log_x_domain(points: &[FrequencyResponsePoint]) -> (f64, f64) {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for p in points {
        if p.freq_hz > 0.0 {
            lo = lo.min(p.freq_hz);
            hi = hi.max(p.freq_hz);
        }
    }
    if !lo.is_finite() || !hi.is_finite() || lo >= hi {
        return (20.0, 20_000.0);
    }
    (lo, hi)
}

fn db_y_domain(points: &[FrequencyResponsePoint]) -> (f64, f64) {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for p in points {
        let v = p.fundamental_dbfs;
        if v.is_finite() {
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    if !lo.is_finite() || !hi.is_finite() {
        return (-60.0, 0.0);
    }
    let pad = ((hi - lo).abs() * 0.1).max(1.0);
    (lo - pad, hi + pad)
}

fn nice_db_step(span: f64) -> f64 {
    if span <= 5.0 { 1.0 }
    else if span <= 20.0 { 2.0 }
    else if span <= 50.0 { 5.0 }
    else if span <= 100.0 { 10.0 }
    else { 20.0 }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 { a + (b - a) * t.clamp(0.0, 1.0) }

fn fmt_f(v: f64, decimals: usize) -> String {
    if !v.is_finite() { return "–".into(); }
    format!("{v:.*}", decimals)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement::report::{
        FrequencyResponsePoint, IntegrationParams, MeasurementData, MeasurementMethod,
        MeasurementReport, StimulusParams, SCHEMA_VERSION,
    };

    fn sample_report() -> MeasurementReport {
        MeasurementReport {
            schema_version: SCHEMA_VERSION,
            ac_version: "0.2.0".into(),
            timestamp_utc: "2026-04-23T10:00:00Z".into(),
            method: MeasurementMethod::SteppedSine { n_points: 3, standard: None },
            stimulus: StimulusParams {
                sample_rate_hz: 48_000,
                f_start_hz: 100.0,
                f_stop_hz: 10_000.0,
                level_dbfs: -20.0,
                n_points: 3,
            },
            integration: IntegrationParams { duration_s: 1.0, window: "hann".into() },
            calibration: None,
            data: MeasurementData::FrequencyResponse {
                points: (0..3).map(|i| FrequencyResponsePoint {
                    freq_hz:          100.0 * 10f64.powi(i),
                    fundamental_dbfs: -20.0 - i as f64 * 0.1,
                    thd_pct:          0.001 * (i + 1) as f64,
                    thdn_pct:         0.002 * (i + 1) as f64,
                    noise_floor_dbfs: -120.0,
                    linear_rms:       0.0707,
                    clipping:         false,
                    ac_coupled:       false,
                }).collect(),
            },
            notes: Some("unit test".into()),
        }
    }

    #[test]
    fn render_pdf_produces_valid_header() {
        let pdf = render_pdf(&sample_report()).expect("render");
        assert!(pdf.starts_with(b"%PDF-"), "wrong magic: {:?}", &pdf[..6.min(pdf.len())]);
        assert!(pdf.windows(5).any(|w| w == b"%%EOF"), "missing EOF marker");
        assert!(pdf.len() > 1500, "pdf too small: {} bytes", pdf.len());
    }

    #[test]
    fn render_pdf_handles_all_data_variants() {
        let mut r = sample_report();
        r.data = MeasurementData::SpectrumBands {
            bpo: 3,
            class: "Class 1".into(),
            centres_hz: vec![100.0, 125.0, 160.0],
            levels_dbfs: vec![-40.0, -35.0, -38.0],
        };
        assert!(render_pdf(&r).is_ok());

        r.data = MeasurementData::NoiseResult {
            sample_rate_hz: 48_000,
            duration_s: 1.0,
            unweighted_dbfs: -98.0,
            a_weighted_dbfs: -103.0,
            ccir_weighted_dbfs: Some(-95.0),
        };
        assert!(render_pdf(&r).is_ok());
    }

    #[test]
    fn render_pdf_empty_points_does_not_crash() {
        let mut r = sample_report();
        r.data = MeasurementData::FrequencyResponse { points: vec![] };
        assert!(render_pdf(&r).is_ok());
    }
}
