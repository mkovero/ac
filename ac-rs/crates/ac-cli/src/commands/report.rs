use std::path::{Path, PathBuf};
use std::process;

use ac_core::measurement::report::MeasurementReport;
use ac_core::measurement::report_html::render_html;

pub fn run(path: &str) {
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
    let html = render_html(&report);
    let out: PathBuf = input.with_extension("html");
    if let Err(e) = std::fs::write(&out, html) {
        eprintln!("  error: cannot write {}: {e}", out.display());
        process::exit(1);
    }
    println!("  wrote {}", out.display());
}
