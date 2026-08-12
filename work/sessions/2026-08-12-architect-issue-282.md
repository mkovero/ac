<!-- architect session issue-282 — 2026-08-12T04:38+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-12-architect-issue-282.jsonl -->

Posted. Design comment on #282, `needs-design` off, `ready-to-implement` on.

Core call: rename `sweep_ir`→`plot_ir` on wire (only `ac-cli` consumes it, no `ac-view`/`ac-scene` hit), leave `sweep_level`/`sweep_frequency` wire names alone under new `generate level`/`generate frequency` nouns — precedent already exists (`generate sine`→`"generate"`). Flagged two implementation-level gotchas for the developer: no suffix convention yet for `n_harmonics`/`window_len` (bare int collides with dBFS default), and `tail_s`'s ISO 18233 basis can only be a post-hoc ≥30dB-decay check via existing `Filterbank`, not a pre-capture computed value — no RT60 estimator exists to compute one blind.
