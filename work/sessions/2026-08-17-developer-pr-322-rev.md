<!-- developer session pr-322-rev — 2026-08-17T00:57+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-17-developer-pr-322-rev.jsonl -->
<!-- resume: claude --resume 1b223e8a-e3da-460d-ab54-4adb57ec9a37 -->

Fixed both correctness bugs, added tests, pushed to `issue-308-sweep-ir-loader`, commented on PR #322.

- **Issue 1** (arrival marker leaks uncorrected distance): now gated through `report.ir_arrival_distance()` — metres only when τ measured, ms tau-corrected when it is.
- **Issue 2** (gate ± re-derived not stored): header now reads `gate.gate_start_s` direct, not `gate_window_s/2`.
- Added 4 new tests QA named missing (nonzero-delay fixture, τ-correction, asymmetric gate, degenerate single-sample span). Updated 1 existing test that pinned old buggy behavior.
- `cargo test --workspace` / clippy -D warnings / fmt --check all clean.
- One point I pushed back on in comment: AC1 "unreachable from keybinding" — QA itself flagged that as design-sanctioned (#256 scope), not a defect. Left as-is, said so.

No label change, no new PR — same branch, same PR #322.
