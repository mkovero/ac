//! Report flow (#308): open a `MeasurementReport` JSON file from disk —
//! the "second loader" the architect review chose (option A) over an
//! `.acsnap`-compatible sidecar. No daemon connection needed, the same
//! D8 shape [`crate::snapshot_flow::open_local`] already has for
//! `.acsnap`: this is a file-open flow, not a wire command, so neither
//! the ZMQ control nor data socket is touched. This module never
//! computes a level, a string, or a trace itself — `ac_scene::sweep_ir`
//! does that.

use std::path::Path;

use ac_core::measurement::report::MeasurementReport;
use ac_scene::{SweepIrFault, SweepIrScene};
use anyhow::{Context, Result};

/// Read and decode a local report JSON file. Mirrors
/// [`crate::snapshot_flow::open_local`]'s shape exactly — read bytes,
/// decode, hand back the parsed value; the caller decides what a
/// decode failure means for display (see [`open_sweep_ir`]).
pub fn open_local(path: &Path) -> Result<MeasurementReport> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("parse {} as a MeasurementReport", path.display()))
}

/// Open a local report file and build the Frame C scene in one call —
/// the orchestration a future file-open UI (#256) will call once it
/// exists. A read/decode failure and a decoded-but-wrong-shape report
/// both collapse to [`SweepIrFault::NotASweepDerivedIr`]: the UX
/// comment groups "unparseable" and "parses as a different shape" as
/// one failure mode, since neither can be told apart from the other
/// without asserting a cause this loader doesn't have.
pub fn open_sweep_ir(path: &Path) -> Result<SweepIrScene, SweepIrFault> {
    let report = open_local(path).map_err(|_| SweepIrFault::NotASweepDerivedIr)?;
    SweepIrScene::from_report(&report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_local_rejects_missing_file() {
        let result = open_local(Path::new("/nonexistent/path/does-not-exist.json"));
        assert!(result.is_err());
    }

    #[test]
    fn open_sweep_ir_reports_not_a_sweep_derived_ir_for_a_missing_file() {
        let result = open_sweep_ir(Path::new("/nonexistent/path/does-not-exist.json"));
        assert_eq!(result, Err(SweepIrFault::NotASweepDerivedIr));
    }

    #[test]
    fn open_sweep_ir_reports_not_a_sweep_derived_ir_for_non_json() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "ac-view-report-flow-test-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, b"not json at all").expect("write scratch file");
        let result = open_sweep_ir(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(result, Err(SweepIrFault::NotASweepDerivedIr));
    }
}
