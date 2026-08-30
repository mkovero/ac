//! AC1's enforcement mechanism: this crate computes nothing, checked
//! by scanning its own `src/` for the forbidden tokens rather than
//! trusting review memory — the same "test in the crate itself" pattern
//! `ac-scene` used for its own dependency-freedom claim (M2's AC6).

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    /// Every `.rs` file under `src/`, **recursively**, this file's own
    /// `tests` module excluded (a false positive here — this string
    /// list — would be self-defeating).
    ///
    /// Recursive on purpose. A flat `read_dir` was silently
    /// scope-limited: the day any module here is split into a
    /// `src/<mod>/` directory, its files stop being scanned and the
    /// forbidden-token guards below keep passing while covering less
    /// and less of the crate. That failure is invisible — no error, no
    /// skipped-test line, just a green run over a shrinking share of
    /// the sources — so the walk has to reach the whole tree rather
    /// than the top of it. `no_source_file_escapes_the_scan` below is
    /// what keeps this honest.
    fn source_files() -> Vec<(String, String)> {
        let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out = Vec::new();
        collect_rs_files(&src_dir, &mut out);
        out
    }

    /// Depth-first walk collecting `(path, content)` for every `.rs`
    /// file, skipping only this file itself.
    fn collect_rs_files(dir: &Path, out: &mut Vec<(String, String)>) {
        for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                collect_rs_files(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs")
                && path.file_name().and_then(|n| n.to_str()) != Some("computes_nothing.rs")
            {
                let content = fs::read_to_string(&path).expect("read source file");
                out.push((path.display().to_string(), content));
            }
        }
    }

    /// The guard on the guard: the scan must see every `.rs` file in
    /// the crate except this one. Counted against an independent walk
    /// so that a scan which quietly stops descending — the exact
    /// regression the flat `read_dir` shipped — fails here loudly
    /// instead of thinning out the three checks below in silence.
    #[test]
    fn no_source_file_escapes_the_scan() {
        let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut expected = Vec::new();
        let mut stack = vec![src_dir.clone()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).expect("read dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().and_then(|e| e.to_str()) == Some("rs")
                    && path.file_name().and_then(|n| n.to_str()) != Some("computes_nothing.rs")
                {
                    expected.push(path.display().to_string());
                }
            }
        }
        let mut scanned: Vec<String> = source_files().into_iter().map(|(p, _)| p).collect();
        scanned.sort();
        expected.sort();
        assert_eq!(
            scanned, expected,
            "the forbidden-token scan is not covering every source file — \
             a file under src/ is invisible to the checks below"
        );
        assert!(
            !expected.is_empty(),
            "scanned nothing at all — the guards below would pass vacuously"
        );
    }

    #[test]
    fn no_trig_in_crate_sources() {
        // Forbidden: any trigonometry. De-rotation (M4a) is phase
        // arithmetic — display truth — and lives in
        // `ac_scene::transfer`. A `sin`/`cos` appearing here would mean
        // the renderer had started computing phase rather than mapping
        // an already-computed normalized coordinate, which is the
        // failure mode this crate's whole contract exists to prevent.
        for (path, content) in source_files() {
            for token in [
                ".sin(",
                ".cos(",
                ".tan(",
                ".atan(",
                ".atan2(",
                ".asin(",
                ".acos(",
                ".to_radians(",
                ".to_degrees(",
            ] {
                assert!(
                    !content.contains(token),
                    "{path} contains forbidden trigonometry: {token} \
                     (ac-view computes nothing — de-rotation belongs in ac-scene::transfer)"
                );
            }
        }
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
