<!-- developer session issue-314 — 2026-08-16T20:15+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-16-developer-issue-314.jsonl -->
<!-- resume: claude --resume 955ceb44-8345-4d97-95c7-50e9f5472361 -->

PR #318 open, closes #314. Fix: `ac_daemon_bin()` reads `CARGO_TARGET_DIR` first, falls back to hardcoded path. Verified both named tests pass under `bin/common.sh`'s actual target dir value, full workspace suite green unset, clippy+fmt clean.
