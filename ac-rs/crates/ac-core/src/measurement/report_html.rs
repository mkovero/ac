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

use std::fmt::Write as _;

use crate::measurement::report::{
    CalibrationSnapshot, FrequencyResponsePoint, MeasurementData, MeasurementMethod,
    ProcessingChain,
    MeasurementReport,
};

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
"#;

/// Render `report` as a self-contained HTML document.
pub fn render_html(report: &MeasurementReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "<!DOCTYPE html>");
    let _ = writeln!(out, "<html lang=\"en\"><head>");
    let _ = writeln!(out, "<meta charset=\"UTF-8\">");
    let _ = writeln!(out, "<title>ac — MeasurementReport</title>");
    let _ = writeln!(out, "<style>{}</style>", CSS);
    let _ = writeln!(out, "</head><body>");

    write_header(&mut out, report);
    write_method(&mut out, report);
    write_stimulus(&mut out, report);
    if let Some(cal) = &report.calibration {
        write_calibration(&mut out, cal);
    }
    write_processing_chain(&mut out, &report.processing_chain);
    write_data(&mut out, &report.data);
    if let Some(notes) = &report.notes {
        let _ = writeln!(out, "<h2>Notes</h2><pre>{}</pre>", html_escape(notes));
    }

    let _ = writeln!(out, "</body></html>");
    out
}

fn write_header(out: &mut String, r: &MeasurementReport) {
    let _ = writeln!(out, "<h1>ac MeasurementReport</h1>");
    let _ = writeln!(out, "<dl class=\"meta\">");
    let _ = writeln!(
        out,
        "<dt>schema</dt><dd>v{}</dd>",
        r.schema_version
    );
    let _ = writeln!(
        out,
        "<dt>ac version</dt><dd>{}</dd>",
        html_escape(&r.ac_version)
    );
    let _ = writeln!(
        out,
        "<dt>timestamp</dt><dd>{}</dd>",
        html_escape(&r.timestamp_utc)
    );
    let _ = writeln!(out, "</dl>");
}

fn write_method(out: &mut String, r: &MeasurementReport) {
    let _ = writeln!(out, "<h2>Method</h2>");
    let _ = writeln!(out, "<dl class=\"meta\">");
    match &r.method {
        MeasurementMethod::SteppedSine { n_points, standard } => {
            let _ = writeln!(
                out,
                "<dt>kind</dt><dd>stepped_sine ({} points)</dd>",
                n_points
            );
            if let Some(s) = standard {
                let _ = writeln!(
                    out,
                    "<dt>standard</dt><dd>{} — {}{}</dd>",
                    html_escape(&s.standard),
                    html_escape(&s.clause),
                    if s.verified { " ✓ verified" } else { "" }
                );
            }
        }
        MeasurementMethod::SweptSine { f1_hz, f2_hz, duration_s, standard } => {
            let _ = writeln!(
                out,
                "<dt>kind</dt><dd>swept_sine ({:.1} Hz → {:.1} Hz, {:.3} s)</dd>",
                f1_hz, f2_hz, duration_s
            );
            if let Some(s) = standard {
                let _ = writeln!(
                    out,
                    "<dt>standard</dt><dd>{} — {}{}</dd>",
                    html_escape(&s.standard),
                    html_escape(&s.clause),
                    if s.verified { " ✓ verified" } else { "" }
                );
            }
        }
    }
    let _ = writeln!(
        out,
        "<dt>integration</dt><dd>{:.3} s, window={}</dd>",
        r.integration.duration_s,
        html_escape(&r.integration.window)
    );
    let _ = writeln!(out, "</dl>");
}

fn write_stimulus(out: &mut String, r: &MeasurementReport) {
    let _ = writeln!(out, "<h2>Stimulus</h2>");
    let _ = writeln!(out, "<dl class=\"meta\">");
    let _ = writeln!(
        out,
        "<dt>sample rate</dt><dd>{} Hz</dd>",
        r.stimulus.sample_rate_hz
    );
    let _ = writeln!(
        out,
        "<dt>range</dt><dd>{:.1} Hz → {:.1} Hz</dd>",
        r.stimulus.f_start_hz, r.stimulus.f_stop_hz
    );
    let _ = writeln!(
        out,
        "<dt>level</dt><dd>{:.2} dBFS</dd>",
        r.stimulus.level_dbfs
    );
    let _ = writeln!(
        out,
        "<dt>points</dt><dd>{}</dd>",
        r.stimulus.n_points
    );
    let _ = writeln!(out, "</dl>");
}

fn write_calibration(out: &mut String, c: &CalibrationSnapshot) {
    let _ = writeln!(out, "<h2>Calibration</h2>");
    let _ = writeln!(out, "<dl class=\"meta\">");
    let _ = writeln!(
        out,
        "<dt>output ch</dt><dd>{}</dd><dt>input ch</dt><dd>{}</dd>",
        c.output_channel, c.input_channel
    );
    if let Some(v) = c.vrms_at_0dbfs_out {
        let _ = writeln!(
            out,
            "<dt>V<sub>RMS</sub>@0dBFS out</dt><dd>{:.6} V</dd>",
            v
        );
    }
    if let Some(v) = c.vrms_at_0dbfs_in {
        let _ = writeln!(
            out,
            "<dt>V<sub>RMS</sub>@0dBFS in</dt><dd>{:.6} V</dd>",
            v
        );
    }
    let _ = writeln!(
        out,
        "<dt>reference</dt><dd>{:.2} Hz @ {:.2} dBFS</dd>",
        c.ref_freq_hz, c.ref_level_dbfs
    );
    // SPL pistonphone reference (#94 / #102): when set, downstream
    // readings convert to dB SPL via `dbspl = dbfs + (94 − mic_sens)`.
    if let Some(mic_sens) = c.mic_sensitivity_dbfs_at_94db_spl {
        let offset = 94.0 - mic_sens;
        let _ = writeln!(
            out,
            "<dt>SPL reference</dt>\
             <dd>94 dB SPL @ {mic_sens:.2} dBFS captured (offset {offset:+.2} dB)</dd>",
        );
    } else {
        let _ = writeln!(out, "<dt>SPL reference</dt><dd>not calibrated</dd>");
    }
    // Mic frequency-response correction provenance (#92 / #102).
    if let Some(mic) = &c.mic_response {
        let path = mic.source_path.as_deref().unwrap_or("(no path recorded)");
        let _ = writeln!(
            out,
            "<dt>mic response</dt>\
             <dd>{} ({} points, imported {})</dd>",
            html_escape(path), mic.n_points, html_escape(&mic.imported_at),
        );
    } else {
        let _ = writeln!(out, "<dt>mic response</dt><dd>not loaded (uncorrected)</dd>");
    }
    let _ = writeln!(out, "</dl>");
}

/// Render the active overlay / processing state captured with the
/// report. When the chain is "all-off + uncorrected" (default for
/// reports built from `ProcessingChain::default()` or legacy v1/v2
/// reports without the field), the section collapses to a one-line
/// "Processing: raw" summary so simple reports stay tidy.
fn write_processing_chain(out: &mut String, chain: &ProcessingChain) {
    let is_default = chain.weighting == "off"
        && chain.smoothing_bpo.is_none()
        && chain.time_integration == "off"
        && !chain.mic_correction_applied;
    if is_default {
        let _ = writeln!(out, "<h2>Processing</h2>");
        let _ = writeln!(out, "<p>raw — no smoothing, weighting, time integration, or mic-curve correction applied.</p>");
        return;
    }
    let _ = writeln!(out, "<h2>Processing</h2>");
    let _ = writeln!(out, "<dl class=\"meta\">");
    let _ = writeln!(
        out,
        "<dt>weighting</dt><dd>{}</dd>",
        html_escape(&chain.weighting),
    );
    match chain.smoothing_bpo {
        Some(n) => {
            let _ = writeln!(out, "<dt>smoothing</dt><dd>1/{n} octave</dd>");
        }
        None => {
            let _ = writeln!(out, "<dt>smoothing</dt><dd>off</dd>");
        }
    }
    let _ = writeln!(
        out,
        "<dt>time integration</dt><dd>{}</dd>",
        html_escape(&chain.time_integration),
    );
    let _ = writeln!(
        out,
        "<dt>mic correction</dt><dd>{}</dd>",
        if chain.mic_correction_applied { "applied" } else { "not applied" },
    );
    let _ = writeln!(out, "</dl>");
}

fn write_data(out: &mut String, d: &MeasurementData) {
    match d {
        MeasurementData::FrequencyResponse { points } => {
            let _ = writeln!(out, "<h2>Frequency Response</h2>");
            out.push_str(&render_frequency_response_svg(points));
            write_frequency_response_table(out, points);
        }
        MeasurementData::SpectrumBands {
            bpo,
            class,
            centres_hz,
            levels_dbfs,
        } => {
            let _ = writeln!(out, "<h2>Spectrum Bands</h2>");
            let _ = writeln!(
                out,
                "<p class=\"note\">{} — 1/{} octave bands</p>",
                html_escape(class),
                bpo
            );
            let _ = writeln!(
                out,
                "<table><thead><tr><th class=\"label\">centre (Hz)</th><th>level (dBFS)</th></tr></thead><tbody>"
            );
            for (c, l) in centres_hz.iter().zip(levels_dbfs.iter()) {
                let _ = writeln!(
                    out,
                    "<tr><td class=\"label\">{:.2}</td><td>{:.2}</td></tr>",
                    c, l
                );
            }
            let _ = writeln!(out, "</tbody></table>");
        }
        MeasurementData::ImpulseResponse {
            sample_rate_hz,
            f1_hz,
            f2_hz,
            duration_s,
            linear_ir,
            harmonics,
        } => {
            let _ = writeln!(out, "<h2>Impulse Response (Farina log sweep)</h2>");
            let _ = writeln!(out, "<dl class=\"meta\">");
            let _ = writeln!(
                out,
                "<dt>sample rate</dt><dd>{} Hz</dd><dt>sweep</dt><dd>{:.1} Hz → {:.1} Hz over {:.3} s</dd>",
                sample_rate_hz, f1_hz, f2_hz, duration_s
            );
            let _ = writeln!(
                out,
                "<dt>linear IR length</dt><dd>{} samples</dd>",
                linear_ir.len()
            );
            let _ = writeln!(
                out,
                "<dt>harmonic IRs</dt><dd>{} (orders {})</dd>",
                harmonics.len(),
                harmonics
                    .iter()
                    .map(|h| h.order.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let _ = writeln!(out, "</dl>");
        }
        MeasurementData::NoiseResult {
            sample_rate_hz,
            duration_s,
            unweighted_dbfs,
            a_weighted_dbfs,
            ccir_weighted_dbfs,
        } => {
            let _ = writeln!(out, "<h2>Idle-channel Noise (AES17)</h2>");
            let _ = writeln!(out, "<dl class=\"meta\">");
            let _ = writeln!(
                out,
                "<dt>sample rate</dt><dd>{} Hz</dd><dt>duration</dt><dd>{:.3} s</dd>",
                sample_rate_hz, duration_s
            );
            let _ = writeln!(
                out,
                "<dt>unweighted</dt><dd>{:.2} dBFS</dd>",
                unweighted_dbfs
            );
            let _ = writeln!(
                out,
                "<dt>A-weighted</dt><dd>{:.2} dBFS</dd>",
                a_weighted_dbfs
            );
            if let Some(c) = ccir_weighted_dbfs {
                let _ = writeln!(out, "<dt>CCIR-468</dt><dd>{:.2} dBFS</dd>", c);
            }
            let _ = writeln!(out, "</dl>");
        }
    }
}

fn write_frequency_response_table(out: &mut String, points: &[FrequencyResponsePoint]) {
    let _ = writeln!(
        out,
        "<table><thead><tr>\
         <th class=\"label\">freq (Hz)</th>\
         <th>fundamental (dBFS)</th>\
         <th>THD (%)</th>\
         <th>THD+N (%)</th>\
         <th>noise (dBFS)</th>\
         <th class=\"label\">flags</th>\
         </tr></thead><tbody>"
    );
    for p in points {
        let mut flags = Vec::new();
        if p.clipping {
            flags.push("clip");
        }
        if p.ac_coupled {
            flags.push("ac");
        }
        let flag_s = flags.join(", ");
        let _ = writeln!(
            out,
            "<tr>\
             <td class=\"label\">{:.2}</td>\
             <td>{:.2}</td>\
             <td>{:.4}</td>\
             <td>{:.4}</td>\
             <td>{:.2}</td>\
             <td class=\"label\">{}</td>\
             </tr>",
            p.freq_hz, p.fundamental_dbfs, p.thd_pct, p.thdn_pct, p.noise_floor_dbfs, flag_s
        );
    }
    let _ = writeln!(out, "</tbody></table>");
}

/// Inline SVG with log-frequency x-axis and dB y-axis. Returns an empty
/// string if there are fewer than two points (nothing to plot).
fn render_frequency_response_svg(points: &[FrequencyResponsePoint]) -> String {
    if points.len() < 2 {
        return String::new();
    }
    let w = 900.0_f64;
    let h = 360.0_f64;
    let pad_l = 60.0_f64;
    let pad_r = 20.0_f64;
    let pad_t = 20.0_f64;
    let pad_b = 40.0_f64;

    let f_min = points.iter().map(|p| p.freq_hz).fold(f64::INFINITY, f64::min);
    let f_max = points
        .iter()
        .map(|p| p.freq_hz)
        .fold(f64::NEG_INFINITY, f64::max);
    let (db_min_raw, db_max_raw) = points.iter().fold(
        (f64::INFINITY, f64::NEG_INFINITY),
        |(lo, hi), p| (lo.min(p.fundamental_dbfs), hi.max(p.fundamental_dbfs)),
    );
    // Pad the dB range so the trace isn't flush with the border.
    let mut db_min = db_min_raw.floor() - 1.0;
    let mut db_max = db_max_raw.ceil() + 1.0;
    if (db_max - db_min) < 6.0 {
        db_min -= 3.0;
        db_max += 3.0;
    }

    let x = |f: f64| {
        pad_l + (f.log10() - f_min.log10()) / (f_max.log10() - f_min.log10()) * (w - pad_l - pad_r)
    };
    let y = |db: f64| pad_t + (db_max - db) / (db_max - db_min) * (h - pad_t - pad_b);

    let mut s = String::new();
    let _ = writeln!(
        s,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {} {}\" width=\"{}\" height=\"{}\" role=\"img\" aria-label=\"Frequency response\">",
        w as i64, h as i64, w as i64, h as i64
    );

    // Log-f gridlines at decade markers.
    let mut decade = 10f64.powf(f_min.log10().floor());
    while decade <= f_max {
        if decade >= f_min {
            let xp = x(decade);
            let _ = writeln!(
                s,
                "<line class=\"grid\" x1=\"{:.1}\" y1=\"{}\" x2=\"{:.1}\" y2=\"{}\" />",
                xp,
                pad_t as i64,
                xp,
                (h - pad_b) as i64
            );
            let _ = writeln!(
                s,
                "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\">{} Hz</text>",
                xp,
                h - pad_b + 14.0,
                format_freq(decade)
            );
        }
        decade *= 10.0;
    }

    // dB gridlines at 10 dB intervals.
    let db_step = pick_db_step(db_max - db_min);
    let mut db = (db_min / db_step).ceil() * db_step;
    while db <= db_max {
        let yp = y(db);
        let _ = writeln!(
            s,
            "<line class=\"grid\" x1=\"{}\" y1=\"{:.1}\" x2=\"{}\" y2=\"{:.1}\" />",
            pad_l as i64,
            yp,
            (w - pad_r) as i64,
            yp
        );
        let _ = writeln!(
            s,
            "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"end\">{:.0} dB</text>",
            pad_l - 6.0,
            yp + 3.5,
            db
        );
        db += db_step;
    }

    // Axes.
    let _ = writeln!(
        s,
        "<rect class=\"axis\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" />",
        pad_l as i64,
        pad_t as i64,
        (w - pad_l - pad_r) as i64,
        (h - pad_t - pad_b) as i64
    );

    // Trace.
    let mut d = String::new();
    for (i, p) in points.iter().enumerate() {
        let prefix = if i == 0 { 'M' } else { 'L' };
        let _ = write!(d, "{}{:.2} {:.2} ", prefix, x(p.freq_hz), y(p.fundamental_dbfs));
    }
    let _ = writeln!(s, "<path class=\"trace\" d=\"{}\" />", d);

    let _ = writeln!(s, "</svg>");
    s
}

fn pick_db_step(span: f64) -> f64 {
    if span > 80.0 {
        20.0
    } else if span > 40.0 {
        10.0
    } else if span > 16.0 {
        5.0
    } else {
        2.0
    }
}

fn format_freq(f: f64) -> String {
    if f >= 1000.0 {
        format!("{:.0}k", f / 1000.0)
    } else {
        format!("{:.0}", f)
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
        FrequencyResponsePoint, IntegrationParams, MeasurementData, MeasurementMethod,
        MeasurementReport, StandardsCitation, StimulusParams, SCHEMA_VERSION,
    };

    fn sample_fr_report() -> MeasurementReport {
        MeasurementReport {
            schema_version: SCHEMA_VERSION,
            ac_version: "0.1.0".into(),
            timestamp_utc: "2026-04-22T12:00:00Z".into(),
            method: MeasurementMethod::SteppedSine {
                n_points: 3,
                standard: Some(StandardsCitation {
                    standard: "IEC 60268-3:2018".into(),
                    clause: "§15.12.3".into(),
                    verified: false,
                }),
            },
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
            },
            calibration: None,
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
    fn includes_method_and_standard() {
        let html = render_html(&sample_fr_report());
        assert!(html.contains("stepped_sine"));
        assert!(html.contains("IEC 60268-3:2018"));
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
        r.data = MeasurementData::SpectrumBands {
            bpo: 3,
            class: "Class 1".into(),
            centres_hz: vec![100.0, 125.0, 160.0],
            levels_dbfs: vec![-30.0, -25.0, -28.0],
        };
        let html = render_html(&r);
        assert!(html.contains("Spectrum Bands"));
        assert!(html.contains("Class 1"));
        assert!(html.contains("125.00"));
    }

    #[test]
    fn processing_section_collapses_to_raw_when_chain_is_default() {
        // Default chain (all-off + uncorrected) renders the one-line
        // summary instead of a key/value table — keeps simple reports
        // tidy.
        let html = render_html(&sample_fr_report());
        assert!(html.contains("<h2>Processing</h2>"), "section heading missing");
        assert!(html.contains("raw — no smoothing"),
            "default-chain summary missing: {html}");
    }

    #[test]
    fn processing_section_renders_active_state() {
        use crate::measurement::report::ProcessingChain;
        let mut r = sample_fr_report();
        r.processing_chain = ProcessingChain {
            weighting:              "a".into(),
            smoothing_bpo:          Some(6),
            time_integration:       "fast".into(),
            mic_correction_applied: true,
        };
        let html = render_html(&r);
        assert!(html.contains("<dt>weighting</dt><dd>a</dd>"),
            "weighting row missing: {html}");
        assert!(html.contains("<dt>smoothing</dt><dd>1/6 octave</dd>"),
            "smoothing row missing: {html}");
        assert!(html.contains("<dt>time integration</dt><dd>fast</dd>"),
            "time-integration row missing: {html}");
        assert!(html.contains("<dt>mic correction</dt><dd>applied</dd>"),
            "mic correction row missing: {html}");
    }

    #[test]
    fn calibration_section_renders_all_three_layers_when_present() {
        use crate::measurement::report::{CalibrationSnapshot, MicResponseRef};
        let mut r = sample_fr_report();
        r.calibration = Some(CalibrationSnapshot {
            output_channel:    0,
            input_channel:     0,
            vrms_at_0dbfs_out: Some(1.0),
            vrms_at_0dbfs_in:  Some(0.5),
            ref_freq_hz:       1000.0,
            ref_level_dbfs:    -10.0,
            mic_sensitivity_dbfs_at_94db_spl: Some(-32.0),
            mic_response: Some(MicResponseRef {
                n_points:    157,
                source_path: Some("/tmp/umik.frd".into()),
                imported_at: "2026-04-15T12:00:00Z".into(),
            }),
        });
        let html = render_html(&r);
        // Voltage cal still rendered.
        assert!(html.contains("V<sub>RMS</sub>@0dBFS in"), "voltage missing: {html}");
        // SPL pistonphone reference + computed offset (94 - (-32) = 126).
        assert!(html.contains("94 dB SPL"), "SPL ref label missing: {html}");
        assert!(html.contains("-32.00 dBFS"), "captured dBFS missing: {html}");
        assert!(html.contains("+126.00 dB"), "offset missing or wrong: {html}");
        // Mic-curve provenance.
        assert!(html.contains("/tmp/umik.frd"), "curve path missing: {html}");
        assert!(html.contains("157 points"), "n_points missing: {html}");
        assert!(html.contains("2026-04-15T12:00:00Z"), "imported_at missing: {html}");
    }

    #[test]
    fn calibration_section_says_uncorrected_when_absent() {
        use crate::measurement::report::CalibrationSnapshot;
        let mut r = sample_fr_report();
        r.calibration = Some(CalibrationSnapshot {
            output_channel:    0,
            input_channel:     0,
            vrms_at_0dbfs_out: None,
            vrms_at_0dbfs_in:  None,
            ref_freq_hz:       1000.0,
            ref_level_dbfs:    -10.0,
            mic_sensitivity_dbfs_at_94db_spl: None,
            mic_response: None,
        });
        let html = render_html(&r);
        assert!(html.contains("not calibrated"), "SPL stub missing: {html}");
        assert!(html.contains("uncorrected"),    "mic stub missing: {html}");
    }

    #[test]
    fn noise_result_renders_numbers() {
        let mut r = sample_fr_report();
        r.data = MeasurementData::NoiseResult {
            sample_rate_hz: 48_000,
            duration_s: 0.9,
            unweighted_dbfs: -98.4,
            a_weighted_dbfs: -103.1,
            ccir_weighted_dbfs: None,
        };
        let html = render_html(&r);
        assert!(html.contains("Idle-channel Noise"));
        assert!(html.contains("-98.40 dBFS"));
        assert!(html.contains("-103.10 dBFS"));
        // CCIR field omitted when None.
        assert!(!html.contains("CCIR-468"));
    }
}
