<!-- developer session issue-286 — 2026-08-16T15:11+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-16-developer-issue-286.jsonl -->
<!-- resume: claude --resume 176bd636-d5cd-445d-b890-cc1366e16163 -->

Done. PR #309 open, closes #286.

Summary:
- Filed follow-up #308 (Frame C, sweep-derived IR — architect said file before code, none existed)
- `ac-scene`: new `ir.rs` module — `IrInput`/`IrScene`, mirrors `TransferInput`/`TransferScene` split. Both producers (live wire sidecar + `.acsnap` `PairDerivation`) funnel through one path. Reuses `format_delay_readout` verbatim for arrival marker, per design.
- `ac-scene/wire.rs`: new `IrWireFrame` deser type for `visualize/ir` (already-documented frame, zero schema change)
- `ac-scene/ticks.rs`: new `time_to_x`/`time_axis` for linear ms axis
- `ac-view/session.rs`: `poll_frame` now returns `PolledFrame::{Transfer,Ir}` instead of silently discarding the sidecar (fixed the #219 story properly for a second frame type)
- `ac-view/keys.rs`: `H` bound, Scope::Transfer, ledger updated
- `ac-view/view.rs` + `app.rs`: IR panel toggle, replaces mag/phase panes when open, `computes_nothing` stays green (all strings/coords from ac-scene verbatim)

Tests: workspace green, clippy clean, fmt clean. +32 new tests across ac-scene/ac-view.

PR: https://github.com/mkovero/ac/pull/309
