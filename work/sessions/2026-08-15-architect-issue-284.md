<!-- architect session issue-284 — 2026-08-15T15:32+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-15-architect-issue-284.jsonl -->
<!-- resume: claude --resume d84bd6ba-e464-4f13-88d4-b865c99987d4 -->

Design comment posted, labels swapped (`needs-design` off, `ready-to-implement` + `needs-ux` on).

Core decision, short version:

**New `MeasurementData::GatedFrequencyResponse` variant** (magnitude+phase points), not reuse of `FrequencyResponse` — that struct's THD/clipping fields would sit at false 0 for a producer that never measures them. Fifth variant, same pattern as `SpectrumBands`/`NoiseResult`.

Pinned four sub-decisions so implementer has zero open questions:
- **Window:** Tukey α=0.25 (new fn, `gate()` in sweep.rs stays rectangular — that's for harmonic isolation, don't touch it).
- **`gate_start_s`:** anchored at the linear-IR peak (arrival), not buffer origin — only stable zero-reference across window-length changes.
- **Noise-tail boundary:** derivable as `duration_s` past the peak (inverse-filter kernel length), capped by captured tail. One shared fn in `sweep.rs`, used by both #283's print path and this issue's report render. Marked as text in the existing `ImpulseResponse` metadata block, not a waveform shade (no IR plot exists in the static renderer — that's #286).
- **Distortion-vs-frequency:** explicit deferral to a follow-up — it's a genuinely separate derivation (needs the sweep's time↔frequency map applied per harmonic IR), not shared code with the gate+FFT path here.

No ZMQ schema/version bump needed — additive fields only, same trick `NoiseResult.ccir_weighted_dbfs` used. Flagged `needs-ux` too: #280's earlier UX mockup covered magnitude display but not phase or the noise-tail line, so that's new reader-facing surface.
