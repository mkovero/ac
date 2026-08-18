<!-- developer session issue-330 — 2026-08-17T22:50+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/src/ac-wt/log/2026-08-17-developer-issue-330.jsonl -->
<!-- resume: claude --resume 59a99bec-a8af-42ff-963e-5635bdb29723 -->

PR #335 open, closes #330.

- `.agents/rig.md` new role: pre-flight, interlocks (hard constraints), record format, decline-to-conclude, work/rig/ expiry rules.
- `.agents/AGENTS.md`: role table row + invocation example.
- `.agents/bin/rig-run.sh`: new, mirrors audit-run.sh.

No ac-rs code touched, no gate needed beyond fmt sanity (clean). PR: https://github.com/mkovero/ac/pull/335
