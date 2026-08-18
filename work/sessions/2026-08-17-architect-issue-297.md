<!-- architect session issue-297 — 2026-08-17T10:30+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-17-architect-issue-297.jsonl -->
<!-- resume: claude --resume f8162ccb-8e53-4b88-922d-e5780a82ca6d -->

Done. #297: posted design decision, labels swapped `needs-design` → `ready-to-implement`.

**Decision:** full `tau_history` array over wire (Option A), not daemon-reduced `latest_tau`+count. Reasons — `mic_response` already sets precedent of full-data-over-reduced in same reply; no consumer exists yet to justify centralizing selection logic (grep confirms `ac-view` doesn't touch either command); UX comment explicitly left "which entry" open, baking it into wire schema would foreclose that early.

CLI picks newest by `measured_at` string compare (same idiom `tau_for` already uses), renders per UX's spec block. New `age_from_iso8601` helper goes in `ac-core/shared/time.rs` (Tier 0 timestamp module), not inline in CLI. No `needs-ux` added — UX comment already covers the render surface and said its spec doesn't change based on this wire choice.
