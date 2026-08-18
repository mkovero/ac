<!-- developer session issue-284 — 2026-08-15T16:00+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-15-developer-issue-284.jsonl -->
<!-- resume: claude --resume be272cc1-483d-4b6e-aadb-5685c258904f -->

PR #305 opened, closes #284. Branch `issue-284` pushed, all gates green (workspace tests, clippy `-D warnings`, `fmt --check` on touched files) except one pre-existing environment-only `ac-view` test unrelated to this diff.

Summary of what shipped:
- `sweep.rs`: `tukey_window`, `gated_frequency_response`/`GatedResponsePoint`, `noise_tail_start_s`, `gated_response_citation` — plus hand-derived synthetic tests (flat system, single-pole closed-form DTFT, gate-length→f_low arithmetic, gated-vs-ungated reflection mutation test).
- `report.rs`: new `MeasurementData::GatedFrequencyResponse` variant (option A from the architect review) and `ImpulseResponse.noise_tail_start_s: Option<f64>` (additive, no schema bump).
- `report_html.rs`/`report_pdf.rs`: render the gated payload (HTML: two-panel SVG + table; PDF: magnitude plot + 3-column table, per the UX page-budget call) and the new noise-tail line.
- `ac-daemon/handlers/audio/plot.rs`: `plot_ir` now emits the gated-response payload alongside the impulse response, citing both Farina/ISO 18233 and a new AES17-2015 Annex A.4 citation.
- `ZMQ.md` updated for the new `data.kind` and field.

Distortion-vs-frequency stayed explicitly deferred per the architect's decision #4 — no follow-up issue opened since nothing blocks it landing independently later.
