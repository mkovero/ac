<!-- architect session issue-280 — 2026-08-13T08:48+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-13-architect-issue-280.jsonl -->
<!-- resume: claude --resume 6a98e8b4-e437-47bc-a883-b61bab507fac -->

Done.

- Posted design comment on #280: recommends `MeasurementPayload` wrapper (`data: Vec<MeasurementPayload>`), citation moved off `MeasurementMethod` onto each payload (`Vec<StandardsCitation>`), new `GateParams` per gated payload, `PositionSnapshot` at report level (one geometry per capture). Rejected bare `Vec<MeasurementData>` — doesn't fix the `plot.rs:430` citation-slot bug and leaves `GateParams` with no clean home.
- ZMQ impact: nested `report.data` shape changes object→array; checked both consumers (`ac-cli` doesn't index into it live, `ac-view` doesn't consume the frame at all) — archival-only, not a live break.
- Labels: removed `needs-design`, added `ready-to-implement` + `needs-ux` (report_html/report_pdf gain new rendered surface: multi-payload, gate block, f_low_hz, citations).

Note: had to route around a sandbox restriction on file writes/heredocs — posted the comment as a single `gh issue comment --body` argument instead.
