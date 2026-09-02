//! Backend-agnostic layout for a [`MeasurementReport`] — *what* each
//! section says, in what order, formatted how.
//!
//! `report_html` and `report_pdf` render the same report and used to
//! decide all of that twice, once each. They drifted: the PDF omitted
//! the output-side voltage calibration, spelled the CCIR row
//! differently, listed a harmonic count where the HTML listed the
//! orders, and plotted on a different dB grid. This module owns the
//! decisions so the two backends only own *painting* — a divergence of
//! that class now needs an edit here, where one edit reaches both.
//!
//! Values arrive pre-formatted as plain text. HTML escapes them on the
//! way out; nothing in here may emit markup.

pub mod axis;
mod payload;
mod sections;

pub use payload::{
    fmt_f, frequency_response_cells, frequency_response_columns, frequency_response_series,
    gated_cells, gated_columns, gated_magnitude_series, gated_phase_series, impulse_response_rows,
    noise_result_rows, payload_meta_rows, spectrum_bands_rows, spectrum_cells, spectrum_columns,
    Column,
};
pub use sections::{header_rows, sections};

/// One label/value line.
///
/// `label_html` exists only for labels that need markup a plain string
/// cannot carry (subscripts). Everything else — including every value —
/// is plain text in both backends.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub label: &'static str,
    pub label_html: Option<&'static str>,
    pub value: String,
}

impl Row {
    pub fn new(label: &'static str, value: impl Into<String>) -> Self {
        Self {
            label,
            label_html: None,
            value: value.into(),
        }
    }

    /// A row whose label needs HTML markup; `label` stays the plain
    /// form the PDF draws.
    pub fn html_label(
        label: &'static str,
        label_html: &'static str,
        value: impl Into<String>,
    ) -> Self {
        Self {
            label,
            label_html: Some(label_html),
            value: value.into(),
        }
    }
}

/// A section is either a key/value table or a single explanatory
/// sentence (the collapsed "nothing to report" form).
#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Rows(Vec<Row>),
    Note(&'static str),
}

/// A `<h2>`-level block. `title` is plain text; HTML escapes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub title: &'static str,
    pub body: Body,
}

impl Section {
    pub fn rows(title: &'static str, rows: Vec<Row>) -> Self {
        Self {
            title,
            body: Body::Rows(rows),
        }
    }

    pub fn note(title: &'static str, note: &'static str) -> Self {
        Self {
            title,
            body: Body::Note(note),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests_support {
    use crate::measurement::report::{
        IntegrationParams, MeasurementData, MeasurementMethod, MeasurementPayload,
        MeasurementReport, ProcessingChain, StimulusParams, SCHEMA_VERSION,
    };

    /// The smallest report that round-trips: no calibration, no
    /// position, a default processing chain, one empty payload.
    pub fn minimal_report() -> MeasurementReport {
        MeasurementReport {
            schema_version: SCHEMA_VERSION,
            ac_version: "0.2.0".into(),
            timestamp_utc: "2026-04-23T10:00:00Z".into(),
            backend: None,
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
                data: MeasurementData::FrequencyResponse { points: vec![] },
                standard: vec![],
                gate: None,
            }],
            notes: None,
            processing_chain: ProcessingChain::default(),
        }
    }
}
