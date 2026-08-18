<!-- architect session issue-308 — 2026-08-16T21:26+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-16-architect-issue-308.jsonl -->
<!-- resume: claude --resume 9c28e1ce-6ea7-4ab0-bea4-c11ea55102ca -->

Posted. Design decision for #308:

**Option A wins** — `ac-view` gets a second loader for `MeasurementReport` JSON, not a `.acsnap`-compatible sidecar. Reasons:

- Report file already exists on disk (`<ISO8601>-plot_ir.json`, written by `ac plot ir` already) — zero daemon work.
- All Frame C math already lives in `ac-core`: `MeasurementReport::ir_stats()` computes arrival, gate window, `f_low_hz` already. `ac-view` just orchestrates, same as `.acsnap`'s `snapshot_flow.rs`.
- Sidecar option would smuggle a Farina-deconvolved `linear_ir` through `.acsnap`'s container, whose documented contract (`SNAPSHOT.md`) is *raw pre-DSP capture* — breaks that invariant or forks the extension into two silent meanings.
- No ZMQ/wire change either way.

Also resolved the carried-forward concurrency question: **no live+sweep overlay** — Frame C stays a dedicated stored-only panel, same boundary #286 already drew between live-arrival IR and gated IR (different kinds, must not visually conflate). Answers #321's single-trace form too.

Flagged one real risk for developer: `ir_stats()`'s legacy-report fallback is permissive (infers a gate for old reports) but this issue's spec wants a hard fail when gate is genuinely absent — loader must check `payload.gate.is_some()` directly, not route the fail decision through `ir_stats()`.

Labels: removed `needs-design`, added `ready-to-implement`. No `needs-ux` needed — UX already fully specified success/fault text; my concurrency answer only simplifies geometry (single panel), doesn't add new surface.
