use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process;

use ac_core::measurement::report::{MeasurementReport, ReportReadError};
use ac_core::measurement::report_html::render_html;
use ac_core::measurement::report_pdf::render_pdf;

use crate::parse::ReportFormat;

pub fn run(path: &str, format: ReportFormat) {
    let input = Path::new(path);
    let json = match std::fs::read_to_string(input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  error: cannot read {path}: {e}");
            process::exit(1);
        }
    };
    let report = match MeasurementReport::from_json(&json) {
        Ok(r) => r,
        Err(ReportReadError::UnsupportedSchema { found, supported }) => {
            for line in unsupported_schema_lines(path, found, &supported) {
                eprintln!("{line}");
            }
            process::exit(1);
        }
        Err(ReportReadError::Malformed(msg)) => {
            eprintln!("  error: invalid MeasurementReport JSON: {msg}");
            process::exit(1);
        }
    };
    let (ext, bytes): (&str, Vec<u8>) = match format {
        ReportFormat::Html => ("html", render_html(&report).into_bytes()),
        ReportFormat::Pdf => match render_pdf(&report) {
            Ok(b) => ("pdf", b),
            Err(e) => {
                eprintln!("  error: PDF render failed: {e:#}");
                process::exit(1);
            }
        },
    };
    let out: PathBuf = input.with_extension(ext);
    if let Err(e) = std::fs::write(&out, bytes) {
        eprintln!("  error: cannot write {}: {e}", out.display());
        process::exit(1);
    }
    println!("  wrote {}", out.display());
}

/// The refusal block for a report whose `schema_version` this build does
/// not read (#429): names compatibility, not syntax, as the fault, and
/// says no HTML/PDF was produced. `ac report verify` prints the same block
/// for any one of its inputs.
pub(crate) fn unsupported_schema_lines(
    path: &str,
    found: u64,
    supported: &RangeInclusive<u32>,
) -> [String; 5] {
    [
        "  error: unsupported measurement report schema".to_string(),
        format!("         file       {path}"),
        format!("         found      v{found}"),
        format!(
            "         supported  v{}–v{}",
            supported.start(),
            supported.end()
        ),
        "         output     not written".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::measurement::report::{MIN_SCHEMA_VERSION, SCHEMA_VERSION};

    #[test]
    fn unsupported_schema_block_names_file_found_supported_and_no_output() {
        let lines = unsupported_schema_lines(
            "/home/rig/reports/session-42.json",
            999,
            &(MIN_SCHEMA_VERSION..=SCHEMA_VERSION),
        );
        assert_eq!(
            lines,
            [
                "  error: unsupported measurement report schema".to_string(),
                "         file       /home/rig/reports/session-42.json".to_string(),
                "         found      v999".to_string(),
                format!("         supported  v1–v{SCHEMA_VERSION}"),
                "         output     not written".to_string(),
            ]
        );
    }
}
