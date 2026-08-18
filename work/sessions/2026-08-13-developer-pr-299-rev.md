<!-- developer session pr-299-rev — 2026-08-13T10:01+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-13-developer-pr-299-rev.jsonl -->
<!-- resume: claude --resume 681650fd-cb61-47e8-a66a-100a3ab8e62b -->

Done. Commit `6745f36` pushed to `issue-280`, reply posted: https://github.com/mkovero/ac/pull/299#issuecomment-5278846784

Summary:
- Blocking (no "number of averages" field) → fixed: `IntegrationParams::n_averages: Option<u32>`, all 9 call sites updated, rendered in HTML/PDF Method section when present, tested.
- Section-order nit → disagreed, said why, offered to fold in if wanted.
- stddocs clause-verify gap → can't close it here either (gitignored, absent in checkout), flagged not silently dropped.

Tests/clippy/fmt all clean, same pre-existing unrelated `it_live_end_to_end` failure as before. Label/PR untouched per instructions.
