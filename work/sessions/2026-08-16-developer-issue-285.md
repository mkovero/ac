<!-- developer session issue-285 — 2026-08-16T07:36+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-16-developer-issue-285.jsonl -->
<!-- resume: claude --resume 0461aab4-ebf2-478a-8af9-505d93f04d47 -->

PR #306 open, closes #285. Branch pushed, tests green (workspace, 0 failed), clippy clean, fmt clean.

Summary:
- `plot_ir` now applies mic-curve correction to the **gated derived spectrum** only (frequency-domain, reused `apply_mic_curve_inplace_f64` route) — IR itself, arrival, gate untouched.
- `mic_correction_applied` reports truth instead of hardcoded `false`.
- New `mic::apply_mic_curve_to_gated_response` + load-bearing test `mic_curve_correction_does_not_move_ir_peak`: builds synthetic IR w/ known peak, proves peak stays put under real impl, and computes what `MicCurveFir` time-domain convolution (rejected impl) would've done — group-delay shift — to show test catches that bug.
- SPL: left as-is, riding on existing `calibration.mic_sensitivity_dbfs_at_94db_spl` field already in the report (matches codebase-wide precedent — SPL never baked per-bin into any spectrum array anywhere, incl. `plot`'s own). Flagged as judgment call in PR open questions.
- 2 files touched, no schema change, no new deps.
