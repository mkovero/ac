<!-- developer session issue-280 — 2026-08-13T09:09+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-13-developer-issue-280.jsonl -->
<!-- resume: claude --resume 11a81e79-ff7d-4425-aa6e-7a3ca8b53286 -->

PR #299 open, closes #280. Summary:

- `report.rs`: `data` → `Vec<MeasurementPayload>`, `SCHEMA_VERSION` 4, new `GateParams`/`PositionSnapshot`, legacy v1/v2/v3 decode shim, `to_csv` per-payload blocks. `MeasurementMethod` loses `standard` (fixes the `plot.rs:430` bug).
- `report_html.rs`/`report_pdf.rs`: render each payload's own citation(s)+gate, new Environment & Geometry section.
- `plot.rs` (daemon): builds payloads instead of bare data, flows `cfg.temperature_c` into report.
- `it_protocol.rs`: two tests updated for new shape/version.

Tests: full green except one pre-existing infra failure (verified via stash — not caused by this change). Clippy clean, fmt clean.
