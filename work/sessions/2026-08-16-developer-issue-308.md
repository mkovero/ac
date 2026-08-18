<!-- developer session issue-308 — 2026-08-16T21:46+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-16-developer-issue-308.jsonl -->
<!-- resume: claude --resume 40ee6c11-b8c3-4305-949c-a0f1de396133 -->

PR #322 is up, closes #308.

**Summary (caveman mode, PR/commit text stayed normal per rules):**

Built the architect-picked "second loader" (option A) for Frame C:

- `ac-scene::sweep_ir` — new module. `SweepIrScene::from_report()` turns a `MeasurementReport` into trace+axis+arrival+header, or `SweepIrFault` (two named failure modes, UX text verbatim). Gate-absent check reads `payload.gate.is_some()` directly, not through `ir_stats()`'s legacy fallback (architect's explicit risk note).
- `ac-scene::readout::format_sweep_ir_header` — dynamic header string.
- `ac-view::report_flow` — `open_local` + `open_sweep_ir`, mirrors existing `snapshot_flow::open_local` shape, no daemon needed.
- `ac-view::view::draw_sweep_ir_panel` — pub paint fn, refactored shared trace/arrival drawing out of existing `draw_ir_panel`.
- New tests: 9 ac-scene unit, 1 readout, 3 report_flow, 3 `egui_kittest` paint-path integration.

Deliberately did NOT wire a keybinding/ViewKind/Action — issue's own out-of-scope section + architect comment both defer the file-open UI to #256, matching the existing `Action::OpenSnapshot` no-op precedent in this repo. Flagged as open question in PR for reviewer to override if wanted.

Gates: `cargo test --workspace` all green (0 failed anywhere), `cargo clippy --workspace --all-targets -- -D warnings` clean, `cargo fmt --check` clean.
