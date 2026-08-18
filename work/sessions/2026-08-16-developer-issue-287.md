<!-- developer session issue-287 — 2026-08-16T08:45+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-16-developer-issue-287.jsonl -->
<!-- resume: claude --resume 0e271002-40a3-4d3a-89db-3b52469199a1 -->

PR #307 open, closes #287. Done.

**Summary:** most acceptance criteria already satisfied by #282/#284's own PRs — verified line-by-line, no diff needed. Fixed remaining stale `play_and_capture` claim (3 sites: `ARCHITECTURE.md`, `ZMQ.md`, `plot.rs` doc comment — moved from `sweep.rs` by #282) since both JACK/CPAL backends implement it now. Added §6.2 note in `acoustic-analyze-v1.md` about IR-arrival retiring the phase-slope estimator per #284, without implementing it. `cargo test --workspace` green except one pre-existing worktree-env failure unrelated to this change; clippy clean; fmt clean.
