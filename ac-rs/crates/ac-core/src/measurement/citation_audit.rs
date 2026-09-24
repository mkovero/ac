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
use std::path::{Path, PathBuf};

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
/// carry trailing qualifiers the citation owns (e.g. `sweep/mod.rs`'s
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
/// of a markdown document. Used by `unresolved_doc_references` on every
/// live doc it walks — chiefly the document map in
/// `docs/architecture/standards.md`, but any live doc citing a held PDF.
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

/// Repo-root-relative directories whose `*.md` files are scanned without
/// recursing. Together with `RECURSIVE_DOC_ROOT` this is the whole scan
/// scope of `unresolved_doc_references`.
const FLAT_DOC_ROOTS: &[&str] = &["", ".agents", "ac-rs"];

/// Scanned recursively, minus `EXCLUDED_DOC_DIRS`.
const RECURSIVE_DOC_ROOT: &str = "docs";

/// Dead-but-kept plans: a path they cite describes the tree as it stood
/// when they were written, so it is not held to today's `stddocs/`.
/// Same exclusion `bin/stale_names.sh` uses. (`audit/`, `work/` and
/// `tests/fixtures/` are excluded by not being roots at all.)
const EXCLUDED_DOC_DIRS: &[&str] = &["docs/superseded"];

/// The standards document map. Pinned by name so that moving or emptying
/// it fails the real-tree test instead of letting the walk pass
/// vacuously — the failure #410 reports.
const STANDARDS_MAP: &str = "docs/architecture/standards.md";

/// `*.md` files directly in `dir`. `is_file()` follows symlinks and is
/// false for a dangling one, so a missing symlink target is skipped.
fn markdown_files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "md") && p.is_file())
        .collect()
}

/// Every subdirectory under `dir` (inclusive), skipping any whose
/// repo-relative path is in `EXCLUDED_DOC_DIRS`.
fn doc_dirs_under(repo_root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let rel = dir.strip_prefix(repo_root).unwrap_or(dir);
    if EXCLUDED_DOC_DIRS.iter().any(|x| rel == Path::new(x)) {
        return;
    }
    out.push(dir.to_path_buf());
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            doc_dirs_under(repo_root, &path, out);
        }
    }
}

/// Every backticked `stddocs/...pdf` reference in a live doc under
/// `repo_root` that names a file not on disk, as (repo-relative doc
/// path, cited path) pairs, sorted so failures print in a fixed order.
/// Live docs are the `*.md` files in `FLAT_DOC_ROOTS` plus those under
/// `RECURSIVE_DOC_ROOT`, minus `EXCLUDED_DOC_DIRS`.
fn unresolved_doc_references(repo_root: &Path) -> Vec<(String, String)> {
    let mut dirs: Vec<PathBuf> = FLAT_DOC_ROOTS.iter().map(|d| repo_root.join(d)).collect();
    doc_dirs_under(repo_root, &repo_root.join(RECURSIVE_DOC_ROOT), &mut dirs);

    let mut unresolved = Vec::new();
    for dir in &dirs {
        for path in markdown_files_in(dir) {
            let doc = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
            let label = path
                .strip_prefix(repo_root)
                .unwrap_or(&path)
                .display()
                .to_string();
            for rel in extract_stddocs_pdf_paths(&doc) {
                if !repo_root.join(&rel).is_file() {
                    unresolved.push((label.clone(), rel));
                }
            }
        }
    }
    unresolved.sort();
    unresolved
}

/// Regression guard for #313. `every_measurement_module_emits_populated_citation`
/// only checks that `standard`/`clause` are non-empty — a well-formed
/// lie passes it just as well as the truth. That is exactly what
/// shipped: `gated_response_citation()` cited `AES17-2015` while only
/// AES17-2020 was ever held in `stddocs/` (#312), and the existing
/// guard could not go red for it. This test additionally resolves each
/// citation's `standard` to a document actually present in `stddocs/`,
/// and separately checks every `stddocs/...pdf` path cited in live repo
/// docs (#291's own acceptance criterion: the table and the emitting fns
/// must never disagree).
///
/// The standards document map lives in `docs/architecture/standards.md`
/// (moved there from `.agents/qa.md` on 2026-08-29). Naming scanned
/// files one by one is what let that move silence this guard (#410), so
/// the doc half walks bounded roots instead — the repo root, `.agents/`
/// and `ac-rs/` flat, `docs/` recursively minus `docs/superseded/` —
/// and pins the map itself: it must exist and cite at least one PDF.
///
/// `stddocs/` is gitignored — held PDFs are licensed and exist only in
/// the local main tree, not in worktrees (see repo `CLAUDE.md`). This
/// test skips, visibly, rather than failing when the directory is
/// absent, so the suite stays runnable where most agent work happens.
/// The walk itself is proven on fixtures by the `doc_walk_*` tests,
/// which need no real `stddocs/` and never skip.
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

    let map_path = repo_root.join(STANDARDS_MAP);
    let map = fs::read_to_string(&map_path).unwrap_or_else(|e| {
        panic!(
            "reading the standards map {}: {e} — if it moved, update STANDARDS_MAP",
            map_path.display()
        )
    });
    assert!(
        !extract_stddocs_pdf_paths(&map).is_empty(),
        "{STANDARDS_MAP} cites no `stddocs/...pdf` path — the map was emptied or moved"
    );

    let unresolved = unresolved_doc_references(repo_root);
    assert!(
        unresolved.is_empty(),
        "docs cite stddocs/ paths that do not exist on disk:\n{}",
        unresolved
            .iter()
            .map(|(doc, rel)| format!("  {doc} cites `{rel}`"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Fixture repo root with `stddocs/held.pdf` present and `body` written
/// at `doc_rel`.
fn fixture_repo(doc_rel: &str, body: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(root.path().join("stddocs")).unwrap();
    fs::write(root.path().join("stddocs/held.pdf"), b"").unwrap();
    let doc = root.path().join(doc_rel);
    fs::create_dir_all(doc.parent().unwrap()).unwrap();
    fs::write(doc, body).unwrap();
    root
}

#[test]
fn doc_walk_reports_a_missing_pdf_in_the_standards_map() {
    let root = fixture_repo(STANDARDS_MAP, "| x | `stddocs/missing.pdf` |\n");
    assert_eq!(
        unresolved_doc_references(root.path()),
        vec![(STANDARDS_MAP.to_string(), "stddocs/missing.pdf".to_string())]
    );
}

#[test]
fn doc_walk_passes_once_the_standards_map_reference_is_corrected() {
    let root = fixture_repo(STANDARDS_MAP, "| x | `stddocs/held.pdf` |\n");
    assert!(unresolved_doc_references(root.path()).is_empty());
}

#[test]
fn doc_walk_skips_superseded_docs() {
    let root = fixture_repo("docs/superseded/x.md", "`stddocs/missing.pdf`\n");
    assert!(unresolved_doc_references(root.path()).is_empty());
}
