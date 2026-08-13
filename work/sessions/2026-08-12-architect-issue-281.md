<!-- architect session issue-281 — 2026-08-12T17:35+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-12-architect-issue-281.jsonl -->
<!-- resume: claude --resume b0a0f12f-8b4f-4237-b5cf-012173177636 -->

Done. Issue #281 review posted, labels updated.

**design comment posted** (comment link: github.com/mkovero/ac/issues/281#issuecomment-5270248019)

Core call: τ history nests inside existing `CalibrationEntry` as `tau_history: Vec<TauEntry>` (same `cal.json`, same `out{N}_in{M}` key), not a separate file. Reason: repo already has a tested pattern for this exact shape (`mic_response` addition), and #279 just paid down a bug in the "load-merge-preserve-save" discipline — a second file duplicates that risk with zero test coverage.

Flagged gap not in spec: `AudioEngine` trait exposes no `period_size` or device-identity accessor today. Resolved it — `device` reuses existing `Config::device`, new `period_size()` trait method (default `None`, JACK backend implements via `buffer_size()` on the underlying client, queried fresh each run since jackd can rebuffer mid-session).

Wire impact: `cal_done` gains `tau_state` + `tau_s`, no new prompt step (rides step 2's existing loopback check). `ac-cli` render path needs an arm for it; `ac-view` untouched (doesn't subscribe to calibrate topics).

Labels: removed `needs-design`, added `ready-to-implement` + `needs-ux` (new field reaches an operator-facing readout in `ac calibrate`'s terminal output).
