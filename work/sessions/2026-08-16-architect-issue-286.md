<!-- architect session issue-286 — 2026-08-16T10:11+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-16-architect-issue-286.jsonl -->
<!-- resume: claude --resume 6f235213-a38f-48c1-8d19-ec21f15c2df8 -->

Posted design comment on #286, labels now `ready-to-implement` + `needs-ux` (needs-design dropped).

Core finding: real gap bigger than UX's flagged one. `#284`/`#305` shipped gated-IR only inside Tier 1 report pipeline (`MeasurementPayload`/`GateParams`), not in `.acsnap`/`PairDerivation` — `ac-view`'s snapshot loader never touches that format at all. So Frame C (sweep-derived) has no path into `ac-view` today; that's a separate file-format decision, not fillable inline.

Decision: build Frame A/B now (live sidecar — data already on wire, `TransferResult.re/im` already on `PairDerivation` too, `impulse_response_from_h` reused for both). File Frame C as its own follow-up issue needing its own architect pass. Keybinding `H`, bound `Scope::Transfer` (toggle, not new `ViewId` — views fixed at launch per `keys.rs:263`). Flagged `needs-ux` since display consequence follows from this boundary call.
