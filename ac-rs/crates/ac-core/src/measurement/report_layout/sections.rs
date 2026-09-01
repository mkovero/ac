//! Report-level sections: everything above the payloads.

use super::{Row, Section};
use crate::measurement::report::{
    CalibrationSnapshot, MeasurementMethod, MeasurementReport, PositionSnapshot, ProcessingChain,
};
use crate::shared::conversions::speed_of_sound_at;

/// Document-identity rows, rendered under the title rather than in a
/// section of their own.
pub fn header_rows(r: &MeasurementReport) -> Vec<Row> {
    vec![
        Row::new("schema", format!("v{}", r.schema_version)),
        Row::new("ac version", r.ac_version.clone()),
        Row::new("timestamp", r.timestamp_utc.clone()),
    ]
}

/// Every section, in render order. `Environment & Geometry` is present
/// only when the report captured a position.
pub fn sections(r: &MeasurementReport) -> Vec<Section> {
    let mut out = vec![
        Section::rows("Method", method_rows(r)),
        Section::rows("Stimulus", stimulus_rows(r)),
        calibration_section(r.calibration.as_ref()),
    ];
    if let Some(pos) = r.position.as_ref() {
        out.push(Section::rows(
            "Environment & Geometry",
            environment_rows(pos),
        ));
    }
    out.push(processing_section(&r.processing_chain));
    out
}

/// Method describes only the stimulus shape (what was played), never
/// what a derived payload means. Citations live on each payload
/// instead (#280) — a report-level `standard` row could show at most
/// one while the report may carry several.
fn method_rows(r: &MeasurementReport) -> Vec<Row> {
    let kind = match &r.method {
        MeasurementMethod::SteppedSine { n_points } => {
            format!("stepped_sine ({n_points} points)")
        }
        MeasurementMethod::SweptSine {
            f1_hz,
            f2_hz,
            duration_s,
        } => format!("swept_sine ({f1_hz:.1} Hz \u{2192} {f2_hz:.1} Hz, {duration_s:.3} s)"),
    };
    let mut rows = vec![
        Row::new("kind", kind),
        Row::new(
            "integration",
            format!(
                "{:.3} s, window={}",
                r.integration.duration_s, r.integration.window
            ),
        ),
    ];
    if let Some(n) = r.integration.n_averages {
        rows.push(Row::new("averages", n.to_string()));
    }
    rows
}

fn stimulus_rows(r: &MeasurementReport) -> Vec<Row> {
    vec![
        Row::new("sample rate", format!("{} Hz", r.stimulus.sample_rate_hz)),
        Row::new(
            "range",
            format!(
                "{:.1} Hz \u{2192} {:.1} Hz",
                r.stimulus.f_start_hz, r.stimulus.f_stop_hz
            ),
        ),
        Row::new("level", format!("{:.2} dBFS", r.stimulus.level_dbfs)),
        Row::new("points", r.stimulus.n_points.to_string()),
    ]
}

/// The three orthogonal calibration layers (voltage / SPL pistonphone /
/// mic frequency-response curve), so a printed report carries the cal
/// context its values were captured under. See #102.
///
/// An absent snapshot renders as a stated "uncalibrated" rather than as
/// a missing section: dropping the section entirely (what the HTML
/// backend used to do) leaves a reader unable to tell an uncalibrated
/// measurement from a report written before the field existed.
fn calibration_section(cal: Option<&CalibrationSnapshot>) -> Section {
    let Some(c) = cal else {
        return Section::note(
            "Calibration",
            "uncalibrated — no calibration snapshot was captured with this measurement.",
        );
    };
    let mut rows = vec![
        Row::new("output ch", c.output_channel.to_string()),
        Row::new("input ch", c.input_channel.to_string()),
    ];
    if let Some(v) = c.vrms_at_0dbfs_out {
        rows.push(Row::html_label(
            "V_RMS@0dBFS out",
            "V<sub>RMS</sub>@0dBFS out",
            format!("{v:.6} V"),
        ));
    }
    if let Some(v) = c.vrms_at_0dbfs_in {
        rows.push(Row::html_label(
            "V_RMS@0dBFS in",
            "V<sub>RMS</sub>@0dBFS in",
            format!("{v:.6} V"),
        ));
    }
    rows.push(Row::new(
        "reference",
        format!("{:.2} Hz @ {:.2} dBFS", c.ref_freq_hz, c.ref_level_dbfs),
    ));
    // SPL pistonphone reference (#94 / #102): when set, downstream
    // readings convert to dB SPL via `dbspl = dbfs + (94 - mic_sens)`.
    rows.push(match c.mic_sensitivity_dbfs_at_94db_spl {
        Some(sens) => Row::new(
            "SPL reference",
            format!(
                "94 dB SPL @ {sens:.2} dBFS captured (offset {:+.2} dB)",
                94.0 - sens
            ),
        ),
        None => Row::new("SPL reference", "not calibrated"),
    });
    // Mic frequency-response correction provenance (#92 / #102).
    rows.push(match &c.mic_response {
        Some(mic) => Row::new(
            "mic response",
            format!(
                "{} ({} points, imported {})",
                mic.source_path.as_deref().unwrap_or("(no path recorded)"),
                mic.n_points,
                mic.imported_at,
            ),
        ),
        None => Row::new("mic response", "not loaded (uncorrected)"),
    });
    Section::rows("Calibration", rows)
}

/// The knowable subset of ISO 3382-1 §9.2 / 3382-2 §9.2 — temperature,
/// relative humidity, source/receiver height, distance (#280).
///
/// Speed of sound is derived from temperature at render time and shown
/// so a reader can sanity-check a gate start against distance without a
/// calculator; it is not stored in the report itself.
fn environment_rows(pos: &PositionSnapshot) -> Vec<Row> {
    let mut rows = Vec::new();
    if let Some(t) = pos.temperature_c {
        rows.push(Row::new("temperature", format!("{t:.1} \u{b0}C")));
    }
    if let Some(h) = pos.relative_humidity_pct {
        rows.push(Row::new("relative humidity", format!("{h:.0} %")));
    }
    if let Some(sh) = pos.source_height_m {
        rows.push(Row::new("source position", format!("height {sh:.2} m")));
    }
    match (pos.receiver_height_m, pos.distance_m) {
        (Some(rh), Some(d)) => rows.push(Row::new(
            "receiver position",
            format!("height {rh:.2} m, distance {d:.2} m from source"),
        )),
        (Some(rh), None) => rows.push(Row::new("receiver position", format!("height {rh:.2} m"))),
        (None, Some(d)) => rows.push(Row::new(
            "receiver distance",
            format!("{d:.2} m from source"),
        )),
        (None, None) => {}
    }
    if let Some(t) = pos.temperature_c {
        rows.push(Row::new(
            "speed of sound",
            format!("{:.1} m/s (c = 331.3 + 0.606\u{b7}T)", speed_of_sound_at(t)),
        ));
    }
    rows
}

/// Active overlay / processing state captured with the report (#105).
/// An all-off, uncorrected chain — the default, and what legacy v1/v2
/// reports deserialize to — collapses to one line so simple reports
/// stay tidy.
fn processing_section(chain: &ProcessingChain) -> Section {
    let is_default = chain.weighting == "off"
        && chain.smoothing_bpo.is_none()
        && chain.time_integration == "off"
        && !chain.mic_correction_applied;
    if is_default {
        return Section::note(
            "Processing",
            "raw — no smoothing, weighting, time integration, or mic-curve correction applied.",
        );
    }
    Section::rows(
        "Processing",
        vec![
            Row::new("weighting", chain.weighting.clone()),
            Row::new(
                "smoothing",
                match chain.smoothing_bpo {
                    Some(n) => format!("1/{n} octave"),
                    None => "off".to_string(),
                },
            ),
            Row::new("time integration", chain.time_integration.clone()),
            Row::new(
                "mic correction",
                if chain.mic_correction_applied {
                    "applied"
                } else {
                    "not applied"
                },
            ),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::super::Body;
    use super::*;
    use crate::measurement::report::MicResponseRef;

    fn find<'a>(rows: &'a [Row], label: &str) -> Option<&'a Row> {
        rows.iter().find(|r| r.label == label)
    }

    fn rows_of(s: &Section) -> &[Row] {
        match &s.body {
            Body::Rows(r) => r,
            Body::Note(n) => panic!("expected rows, got note: {n}"),
        }
    }

    fn full_calibration() -> CalibrationSnapshot {
        CalibrationSnapshot {
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
        }
    }

    #[test]
    fn calibration_carries_both_voltage_directions() {
        // The PDF backend used to render only the input side, so an
        // output-referred calibration silently vanished from a printed
        // report while the HTML showed it.
        let rows = rows_of(&calibration_section(Some(&full_calibration()))).to_vec();
        assert_eq!(
            find(&rows, "V_RMS@0dBFS out").map(|r| r.value.as_str()),
            Some("1.000000 V"),
            "{rows:?}"
        );
        assert_eq!(
            find(&rows, "V_RMS@0dBFS in").map(|r| r.value.as_str()),
            Some("0.500000 V"),
            "{rows:?}"
        );
    }

    #[test]
    fn absent_calibration_states_uncalibrated_instead_of_vanishing() {
        let s = calibration_section(None);
        assert_eq!(s.title, "Calibration");
        match s.body {
            Body::Note(n) => assert!(n.contains("uncalibrated"), "{n}"),
            Body::Rows(r) => panic!("expected a stated note, got rows: {r:?}"),
        }
    }

    #[test]
    fn spl_offset_is_94_minus_sensitivity() {
        let rows = rows_of(&calibration_section(Some(&full_calibration()))).to_vec();
        let v = &find(&rows, "SPL reference").expect("row").value;
        assert!(v.contains("-32.00 dBFS"), "{v}");
        assert!(v.contains("+126.00 dB"), "{v}");
    }

    #[test]
    fn environment_omits_rows_for_fields_that_were_not_captured() {
        let rows = environment_rows(&PositionSnapshot {
            temperature_c: None,
            relative_humidity_pct: None,
            source_height_m: None,
            receiver_height_m: None,
            distance_m: Some(2.5),
        });
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "receiver distance");
        assert!(find(&rows, "speed of sound").is_none());
    }

    #[test]
    fn speed_of_sound_appears_only_with_a_temperature() {
        let with_t = environment_rows(&PositionSnapshot {
            temperature_c: Some(21.3),
            relative_humidity_pct: None,
            source_height_m: None,
            receiver_height_m: None,
            distance_m: None,
        });
        assert!(find(&with_t, "speed of sound").is_some(), "{with_t:?}");
    }

    #[test]
    fn processing_collapses_only_for_a_fully_default_chain() {
        assert!(matches!(
            processing_section(&ProcessingChain::default()).body,
            Body::Note(_)
        ));
        let active = ProcessingChain {
            weighting: "a".into(),
            smoothing_bpo: Some(6),
            time_integration: "fast".into(),
            mic_correction_applied: true,
        };
        let rows = rows_of(&processing_section(&active)).to_vec();
        assert_eq!(
            find(&rows, "smoothing").map(|r| r.value.as_str()),
            Some("1/6 octave")
        );
        assert_eq!(
            find(&rows, "mic correction").map(|r| r.value.as_str()),
            Some("applied")
        );
    }

    #[test]
    fn method_never_carries_a_standard_row() {
        // #280: the citation belongs to the payload, not the stimulus.
        let r = crate::measurement::report_layout::tests_support::minimal_report();
        assert!(find(&method_rows(&r), "standard").is_none());
    }
}
