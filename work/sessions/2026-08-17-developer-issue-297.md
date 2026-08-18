<!-- developer session issue-297 — 2026-08-17T10:41+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-17-developer-issue-297.jsonl -->
<!-- resume: claude --resume f6297fe6-2b68-4b94-a906-6d37a19537b1 -->

PR #324 open, closes #297. Full workspace test/clippy/fmt all clean.

Summary:
- `admin.rs`: `get_calibration`/`list_calibrations` now carry `tau_history` (always array, `[]` unmeasured).
- `time.rs`: new `age_from_iso8601` helper, tested.
- `calibrate.rs`: `run_show` gains `Delay:` leg — pure `render_tau_history_leg` fn (testable) picks newest entry, shows value/age/conditions/ports/`+N more`, matches UX mock exactly.
- `it_protocol.rs`: wire test, present + absent history.
- `ZMQ.md`: documented new field both replies.

PR: https://github.com/mkovero/ac/pull/324
