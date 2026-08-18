<!-- developer session issue-193 — 2026-08-13T19:16+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-13-developer-issue-193.jsonl -->
<!-- resume: claude --resume bf3f2070-0c0a-4618-93e5-739212e7aa03 -->

PR #301 open. https://github.com/mkovero/ac/pull/301 — closes #193.

Summary:
- `app.rs` gains `frame_parse_failures` streak + `first_malformed_since` clock, `MALFORMED_GRACE` (10s) constant, `ingest_raw_frame`/`malformed_active`/`status_for_state` methods.
- Status line: `live` → `malformed — host:port — N consecutive frames dropped, not rendering` once streak clears grace window; resets on good parse; single glitch doesn't flicker.
- Test seam fixed per UX's flagged gap: raw-JSON entry (`ingest_raw_for_test`) drives actual `serde_json::from_value` boundary, not the pre-parsed `ingest_frame_for_test` hook.
- 5 new tests, all pass. `cargo test --workspace` clean except pre-existing unrelated `it_live_end_to_end` env failure (hardcoded path ignores this worktree's `CARGO_TARGET_DIR`, reproduces on `main` too). clippy + fmt clean.
