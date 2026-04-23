use std::path::{Path, PathBuf};
use std::process;

use ac_core::measurement::report::MeasurementReport;
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
    let report: MeasurementReport = match serde_json::from_str(&json) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  error: invalid MeasurementReport JSON: {e}");
            process::exit(1);
        }
    };
    let (ext, bytes): (&str, Vec<u8>) = match format {
        ReportFormat::Html => ("html", render_html(&report).into_bytes()),
        ReportFormat::Pdf  => match render_pdf(&report) {
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
