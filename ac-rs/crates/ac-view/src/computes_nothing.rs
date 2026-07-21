//! AC1's enforcement mechanism: this crate computes nothing, checked
//! by scanning its own `src/` for the forbidden tokens rather than
//! trusting review memory — the same "test in the crate itself" pattern
//! `ac-scene` used for its own dependency-freedom claim (M2's AC6).

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    /// Every `.rs` file under `src/`, this file's own `tests` module
    /// excluded (a false positive here — this string list — would be
    /// self-defeating).
    fn source_files() -> Vec<(String, String)> {
        let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out = Vec::new();
        for entry in fs::read_dir(&src_dir).expect("read src/") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("rs")
                && path.file_name().and_then(|n| n.to_str()) != Some("computes_nothing.rs")
            {
                let content = fs::read_to_string(&path).expect("read source file");
                out.push((path.display().to_string(), content));
            }
        }
        out
    }

    #[test]
    fn no_log_arithmetic_in_crate_sources() {
        // Forbidden: any dB/log-domain conversion. `ac-scene` owns the
        // single conversion site (M2); this crate must never contain a
        // second one.
        for (path, content) in source_files() {
            for token in ["log10(", "ln(", ".powf(", "log2("] {
                assert!(
                    !content.contains(token),
                    "{path} contains forbidden log/pow arithmetic: {token} \
                     (ac-view computes nothing — this belongs in ac-scene)"
                );
            }
        }
    }

    #[test]
    fn no_format_macro_used_to_render_measurement_numbers() {
        // format!/println!/write! ARE used in this crate for
        // non-measurement purposes (error messages, URLs, key-binding
        // help text) — those are fine. What's forbidden is formatting
        // a measurement *value* (a level, a frequency) with a numeric
        // format spec, which would mean this crate re-implemented a
        // formatting rule ac-scene::readout already owns. Checked here
        // as a targeted grep for numeric format specifiers combined
        // with unit-like literal suffixes, which is what a
        // reintroduced measurement-formatting call would look like.
        let suspicious_units = [" Hz", " dB", "dBFS", "dBu", "Vrms"];
        for (path, content) in source_files() {
            for line in content.lines() {
                let has_numeric_spec = line.contains(":.0}")
                    || line.contains(":.1}")
                    || line.contains(":.2}")
                    || line.contains(":.3}");
                let has_unit_literal = suspicious_units.iter().any(|u| line.contains(u));
                assert!(
                    !(has_numeric_spec && has_unit_literal),
                    "{path} looks like it formats a measurement value directly: {line} \
                     (ac-view computes nothing — add the string to ac-scene::readout instead)"
                );
            }
        }
    }
}
