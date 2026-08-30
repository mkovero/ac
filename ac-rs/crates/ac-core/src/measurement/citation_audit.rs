//! Repo-hygiene guards over the `StandardsCitation`s this workspace
//! emits (#72, #313): every Tier 1 measurement module must cite a
//! populated standard and clause, and every edition named must resolve
//! to a document actually held under `stddocs/`.
//!
//! Test-only module. It lives beside the measurement modules it audits
//! rather than inside `report.rs`, whose tests cover the archival schema
//! — these guards are about the citations themselves and about paths
//! cited in repo documentation, neither of which is a report concern.

use std::fs;
use std::path::Path;

use super::report::StandardsCitation;

/// Every citation this workspace emits, in one place. Both citation
/// guards below read this list: kept separate, a new measurement
/// module added to one and forgotten in the other leaves that guard
/// green while silently covering less.
pub(crate) fn every_citation() -> [StandardsCitation; 9] {
    [
        crate::measurement::thd::citation(),
        crate::measurement::filterbank::Filterbank::citation(),
        crate::measurement::noise::citation(),
        crate::measurement::weighting::WeightingFilter::citation(),
        crate::measurement::sweep::citation(),
        crate::measurement::sweep::farina_citation(),
        crate::measurement::sweep::gated_response_citation(),
        crate::measurement::ccir468::citation(),
        crate::shared::reference_levels::citation(),
    ]
}

/// Every Tier 1 measurement module must emit a populated
/// `StandardsCitation` — non-empty `standard` and `clause`. See #72 for
/// the audit workflow. That such a citation survives a report round-trip
/// is `report.rs`'s `citations_round_trip_through_a_report`.
#[test]
fn every_measurement_module_emits_populated_citation() {
    for c in &every_citation() {
        assert!(!c.standard.is_empty(), "empty standard in {c:?}");
        assert!(!c.clause.is_empty(), "empty clause in {c:?}");
    }
}

/// Maps one edition string (as it appears in a `citation().standard`
/// field, or one `; `-separated half of one) to the `stddocs/`-relative
/// path of the document it names. Single place this mapping lives —
/// see #313: it must not be re-derived per call site.
///
/// Matches by prefix (not equality) because a `standard` field may
/// carry trailing qualifiers the citation owns (e.g. sweep.rs's
/// combined `citation()` appends "; ISO 18233:2006 Annex B
/// (normative)"). Returns `None` for any edition this map does not
/// recognise — that is the failure this guard exists to catch: a
/// citation naming an edition nobody holds.
fn standard_edition_to_stddocs_path(edition: &str) -> Option<&'static str> {
    const KNOWN: &[(&str, &str)] = &[
        ("IEC 61672-1:2013", "iec-full/IEC61672-1.pdf"),
        ("IEC 60268-3:2018", "iec-full/IEC60268-3.pdf"),
        ("IEC 61260-1:2014", "iec-full/IEC61260-1.pdf"),
        ("ITU-R BS.468-4", "ITU-R BS.468-4.pdf"),
        ("ITU-R BS.1770-5", "ITU-R BS.1770-5.pdf"),
        (
            "AES17-2020",
            "iec-full/aes17_2020_aes_standard_method_for_digital_audio_engineering_measurement.pdf",
        ),
        (
            "Farina, AES 108th Convention preprint #5093 (2000)",
            "iec-full/Simultaneous_Measurement_of_Impulse_Response_and_D.pdf",
        ),
        ("ISO 18233:2006", "iso-full/ISO18233.pdf"),
        ("ISO 3382-1:2009", "iso-full/ISO3382-1.pdf"),
        ("ISO 3382-2:2008", "iso-full/ISO3382-2.pdf"),
    ];
    KNOWN
        .iter()
        .find(|(key, _)| edition.starts_with(key))
        .map(|(_, path)| *path)
}

/// A citation's `standard` field resolves only if every `; `-separated
/// half of it maps to a file that actually exists under `stddocs_root`.
fn citation_standard_resolves(standard: &str, stddocs_root: &Path) -> bool {
    standard.split("; ").all(|edition| {
        standard_edition_to_stddocs_path(edition)
            .map(|rel| stddocs_root.join(rel).is_file())
            .unwrap_or(false)
    })
}

/// Pulls every backtick-delimited `stddocs/...pdf` path reference out
/// of a markdown document. Used to walk the normative-standards table
/// path columns in `.agents/qa.md` and `ARCHITECTURE.md`.
fn extract_stddocs_pdf_paths(markdown: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut rest = markdown;
    while let Some(start) = rest.find("stddocs/") {
        let after = &rest[start..];
        let Some(end) = after.find('`') else {
            break;
        };
        let candidate = &after[..end];
        // Skip bare directory references like `stddocs/` or
        // `stddocs/iec-full/` — only file references matter here.
        if candidate.ends_with(".pdf") {
            paths.push(candidate.to_string());
        }
        rest = &after[end..];
    }
    paths
}

/// Regression guard for #313. `every_measurement_module_emits_populated_citation`
/// only checks that `standard`/`clause` are non-empty — a well-formed
/// lie passes it just as well as the truth. That is exactly what
/// shipped: `gated_response_citation()` cited `AES17-2015` while only
/// AES17-2020 was ever held in `stddocs/` (#312), and the existing
/// guard could not go red for it. This test additionally resolves each
/// citation's `standard` to a document actually present in `stddocs/`,
/// and separately walks the `.agents/qa.md` and `ARCHITECTURE.md`
/// standards tables for stale path references (#291's own acceptance
/// criterion: the table and the emitting fns must never disagree).
///
/// `stddocs/` is gitignored — held PDFs are licensed and exist only in
/// the local main tree, not in worktrees (see repo `CLAUDE.md`). This
/// test skips, visibly, rather than failing when the directory is
/// absent, so the suite stays runnable where most agent work happens.
#[test]
fn every_citation_resolves_to_a_held_document() {
    let stddocs_root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../stddocs"));
    if !stddocs_root.is_dir() {
        eprintln!(
            "SKIP every_citation_resolves_to_a_held_document: {} not present \
             (stddocs/ is gitignored, main-tree only — see CLAUDE.md)",
            stddocs_root.display()
        );
        return;
    }
    let repo_root = stddocs_root.parent().expect("stddocs_root has a parent");

    let citations = every_citation();
    for c in &citations {
        assert!(
            citation_standard_resolves(&c.standard, stddocs_root),
            "citation names an edition nobody holds under stddocs/: {c:?}"
        );
    }

    for (label, path) in [
        (".agents/qa.md", repo_root.join(".agents/qa.md")),
        ("ARCHITECTURE.md", repo_root.join("ARCHITECTURE.md")),
    ] {
        let doc =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        for rel in extract_stddocs_pdf_paths(&doc) {
            let full = repo_root.join(&rel);
            assert!(
                full.is_file(),
                "{label} cites `{rel}` which does not exist on disk"
            );
        }
    }
}
