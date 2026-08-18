<!-- architect session issue-321 — 2026-08-17T11:13+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-17-architect-issue-321.jsonl -->
<!-- resume: claude --resume bfd18c33-fdbc-4cac-adf7-ffa1d75482e3 -->

Done. Issue #321 posted, labels updated: `needs-design` off, `ready-to-implement` + `needs-ux` + `agent:architect` on.

Design gist: recommend **option B** — keep `ac_scene::transfer::TransferScene` single-trace, unchanged. Multiplicity (N stored runs, each own `Smoothing`) built by `ac-view` calling existing `TransferScene::from_input` machinery repeatedly (deliverable 3's snapshot-overlay already proved this shape works). No wire change, no `ac-scene` type change. Real cost lands on `ac-view`: `TransferViewState` gets `Vec<LoadedRun>` + focus, `draw_view` signature changes from one `Option<&TransferScene>` to live+slice.

Flagged `needs-ux` because the design confirms UX's already-posted mockup (focus marker, per-row smoothing, dim-vs-bright not colour) is exactly what's implementable — worth a UX pass to confirm alignment, not reopen it.
