<!-- developer session issue-327 — 2026-08-17T21:54+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/src/ac-wt/log/2026-08-17-developer-issue-327.jsonl -->
<!-- resume: claude --resume 0fb9b5ab-6d93-4430-8a3f-248dbc020a39 -->

PR #332 opened, closes #327. Doc-only change: `.agents/qa.md` step 3 gains reachability/ignore/fake-mock checks, both `qa.md` and `developer.md` gain `$AC_HOME` scratch-space rule. Full workspace test green, clippy clean, fmt clean. Scope: exactly 2 files, matches spec's likely-affected list minus `.agents/bin/*.sh` (read-only per issue).
