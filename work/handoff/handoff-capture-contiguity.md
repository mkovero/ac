# handoff-capture-contiguity — HF spectrum replication: instrument, reproduce, then fix

Parent: none (standing defect, predates the `ac-ui` → `ac-scene`/`ac-view`
rewrite). Base: `main` post-M3.

**This is an investigation handoff, not a fix handoff.** The deliverable is
*instrumented evidence* plus a falsifiable test. The fix is explicitly gated
on the verdict — see §Routing. Falsification-first: no code change to the
capture layer lands on the strength of the reasoning in this document.

**Sequencing:** D1–D3 are headless and need neither hardware nor a working
non-TUI `ac monitor`. They can proceed during the current development cycle.
D4 is hardware-gated and deferred until the cycle stabilises.

## Symptom (reporter: Markus)

With HF stimulus, the displayed spectrum shows the response **three times**.
Gain is not lost across the copies. The response shape itself appears
correct — it just repeats. Observed in `ac-view` today; the same symptom was
observed under the old `ac-ui`.

The cross-era persistence is the load-bearing clue: `ac-ui` consumed
`monitor_spectrum`, `ac-view` consumes `transfer_stream`. Whatever this is,
it is upstream of both, or in code both share.

## Already ruled out (do not re-tread)

Each of these was checked directly against the current tree. Recorded so the
investigation starts from a narrowed surface, not from scratch.

- **`spectrum_to_columns` does not replicate.** Ported to Python with f32
  semantics and run with a single HF tone through both live grids — transfer
  (`b = 24001`, `df = 1 Hz`, 491 columns) and monitor (`fft_n = 32768`, 4096
  columns). Exactly one peak each, at 15010 Hz and 15001 Hz respectively.
  No tiling, no wrap; the `k` cursor is monotone and never resets.
- **The `meas_amp` frequency axis is consistent.** `h1_estimate_core` uses
  `nperseg = sr`, so `meas_amp.len() = sr/2 + 1` and the implicit axis
  `spectrum_to_columns_wire` reconstructs (`df = sr / (2·(b−1)) = 1 Hz`)
  matches `freqs[k] = k·sr/nperseg` exactly. No decimation-factor mismatch
  between the H1 grid and the spectrum grid.
- **`welch_all`** accumulates and averages with no index arithmetic capable
  of wrapping.
- **`ac-scene::to_points`** is a `zip`; **`ac-view::scene_to_screen`** is one
  affine map. Neither can produce copies. Display-truth discipline holds.
- **`generate_sine_1s`** is cycle-aligned at exactly `sr` samples;
  `fill_tone` wraps on that boundary. The stimulus buffer is not the source.

Conclusion carried into this work: **no path in the display chain
manufactures equal-gain spectral copies.** Either the buffer handed to the
FFT is not the contiguous signal the FFT assumes, or the copies are real on
the wire.

## Hypotheses, ranked

**H1 — spliced capture windows (reporter's own suspicion; primary).**
`capture_multi` (transfer worker, `chunk_secs = 0.05`) and `capture_block`
(multi-channel monitor) both call `ring_cons.clear()` *before* `wait_ring`.
Every tick discards whatever accumulated during the previous tick's
processing, then pops a fresh chunk. `rings[i]` then concatenates those
chunks into the 2.5 s Welch window, so the FFT is handed ~50 non-contiguous
50 ms fragments presented as continuous time.

Why it fits: the phase error introduced per splice scales with frequency — a
10 ms gap is ~1 cycle at 100 Hz and ~150 cycles (fully randomised) at
15 kHz, so the artifact is inherently HF-specific. It is also
energy-conserving, matching "does not lose gain". And it predicts the
cross-era persistence exactly: single-channel monitor uses
`capture_available` (non-clearing, contiguous) and should be **clean**, while
multi-channel monitor and the transfer worker both splice.

Not yet explained by H1: why *three* discrete copies rather than a dense
sideband cluster at the ~20 Hz chunk rate. Splice-induced replicas should sit
at multiples of `sr/L`. **This gap is the reason D4 exists** — if the
observed copies are widely separated, H1 is incomplete even if splicing is
independently confirmed. Do not let a confirmed splice close the issue
without accounting for the spacing.

**H2 — CPAL rate mismatch (secondary; separable defect either way).**
`cpal_backend::start()` sets `self.sample_rate` from
`default_output_config()`, then opens the input stream with
`in_dev.default_input_config()` and never reconciles the two. All analysis
assumes the output rate. This scales the entire frequency axis by the rate
ratio — one displaced copy, not three — so it is unlikely to be *this* bug,
but it is a real defect and should be filed separately regardless of the
verdict here.

**H3 — ring backlog (note only).** `RING_CAPACITY = 16·192_000` (~64 s at
48 k) with a partial per-tick drain lets the ring back up without bound if
the daemon falls behind realtime. Flagged as a candidate for the open LF
~10 s anomaly, not for this one. Out of scope here; cross-reference only.

## Deliverables

### 1. Fake backend gains real ring semantics (the unblocker)

`FakeEngine::make_samples_for` synthesises samples on demand from a
phase-continuous per-channel cursor. There is **no ring, and nothing is ever
cleared or discarded**. The fake backend is therefore *structurally
incapable* of reproducing any splice, drop, or backlog defect — which is
almost certainly why this bug, and the LF ~10 s anomaly, have never
reproduced headlessly.

- Add an opt-in fake mode in which capture is served from a **real ring**
  fed on a wall-clock (or synthetically advanced) schedule, so that
  `clear()`, partial drain, and overrun all have the same observable
  consequences they have on JACK.
- **Additive and default-unchanged**: the existing on-demand generator stays
  the default; the ring-backed mode is selected explicitly. Follow the
  established fake-only knob pattern — request param on the relevant
  command, guarded by `state.fake_audio`, mirroring `fake_tones` /
  `fake_correlated_pair`. No config key.
- Prefer a **synthetically advanced** clock over wall-clock if it can be made
  to work: it makes drain behaviour deterministic and testable, and it is the
  same seam already wanted for the open ZMQ-drain determinism item. If the
  two seams can share one mechanism, say so and route to architect rather
  than building two.

Zero edits to existing assertions. The existing suite proves the default
path is byte-identical.

### 2. Wire-tap on the exact buffer handed to the estimator

- Dump `rings[i]` — the precise slice passed to `h1_estimate_with_delay`,
  post-assembly, pre-analysis — to WAV or FLAC behind an explicit
  debug flag.
- Splices are directly visible and measurable in the waveform; this is the
  instrument that converts H1 from a reading of the code into evidence.
- Emit alongside it a per-tick **discarded-sample count** at each `clear()`
  site. Internal engine counter mirroring the existing `xruns()` shape — an
  additive wire field would be more convenient for the UI but changes the
  frame contract, so that is architect's call, not the implementer's.
  Default to the internal counter.

### 3. Guard test — HF single-tone replication

- Under the D1 ring-backed fake mode, with a single HF tone through the
  transfer path: assert the number of aggregated columns above
  `peak − 40 dB`.
- Per house convention, this lands as a **guard test that passes while the
  bug is observably present**, and is inverted to the correct assertion
  (exactly one peak) as part of the eventual fix.
- Mutation-verify at birth: the test must fail when the `clear()`-before-
  `wait_ring` ordering is reverted in isolation. A test that cannot
  distinguish spliced from contiguous input is not evidence and does not
  satisfy this deliverable.
- Record the *measured* replica spacing in the test's comment. Spacing is
  the quantity that discriminates the mechanisms (see D4) — capture it while
  the harness is in hand.

### 4. Hardware A/B (DEFERRED — gated on the development cycle)

Blocked on: non-TUI `ac monitor` working, and a stable tree. Do not attempt
before both hold.

- **The three frequencies.** Cursor-read the copies in `ac-view` and record
  them. This alone picks the mechanism:
  - *geometric* (`f`, `f·r`, `f·r²`) → sample-rate / resampling mismatch (H2
    territory, not H1);
  - *linear-symmetric* (`f`, `F−f`, `F+f`) → aliasing images from
    zero-stuffing or unfiltered decimation — a mechanism **not currently in
    any hypothesis**, and if this is what turns up, re-open the analysis
    rather than forcing it into H1;
  - *tight cluster at ~20 Hz spacing* → H1 confirmed.
- **Single vs multi channel, same tone:**
  ```
  ac generate sine 15khz -20db
  ac monitor 0            # capture_available — contiguous
  ac monitor 0-1          # capture_block — clears ring every tick
  ```
  Replicas absent in the first and present in the second is the decisive
  discriminator for H1.
- **Backend name** from the daemon banner. If `cpal`, additionally record
  `default_input_config().sample_rate()` alongside
  `default_output_config().sample_rate()` — that settles H2 in one reading.

## Acceptance criteria (falsifiable)

1. Full workspace green; zero edits to pre-existing assertions.
2. D1's ring-backed fake mode is opt-in; default fake behaviour unchanged,
   proven by the existing suite passing unmodified.
3. D3's guard test is mutation-verified at birth: demonstrated failing when
   the `clear()` ordering is reverted, demonstrated passing on current
   `main`. Both runs recorded in the PR.
4. The wire-tap produces a buffer in which splice boundaries are either
   **visible and located** (H1 supported) or **demonstrably absent** (H1
   falsified). "Inconclusive" is a legitimate outcome and must be reported as
   such — not resolved by argument.
5. The measured replica spacing is recorded as a number, and reconciled
   against the spacing H1 predicts (`sr/L`). An unreconciled discrepancy
   keeps the issue open regardless of what else is confirmed.
6. No change to `spectrum_to_columns`, `h1_estimate_core`, or any display-path
   code lands in this slice. The evidence arrives first.

## Out of scope (hard fence)

- **Any fix to the capture layer.** The `clear()`-before-`wait_ring` ordering
  is *not* to be changed in this slice, however obvious it looks once the
  wire-tap is in hand. It is a real-time path with `reconnect_input`
  interactions (the existing comment — "multi-channel must use block capture
  because `reconnect_input` clears the ring on every switch" — is a
  constraint, not an excuse, and the fix must be designed against it).
  Separate PR, architect-gated.
- `ac-scene`, `ac-view`, and all display-path code. Ruled out above; touching
  it here means the investigation went sideways.
- Wire-protocol changes. The discarded-sample counter stays internal unless
  architect rules otherwise (D2).
- The LF ~10 s anomaly and the ZMQ-drain determinism item. Cross-reference
  only — except where D1's seam can serve both, which is a routing question,
  not a scope expansion.
- H2 (CPAL rate reconciliation). File separately; do not fix here.
- `RING_CAPACITY` / backlog behaviour (H3).

## Routing

- **Architect:** D1's mode-selection surface and the shared-seam question
  (D1 vs the open ZMQ-drain item); D2's counter-placement decision (internal
  vs additive wire field). Both before implementation.
- **QA:** D3's mutation verification, and criterion 5 — QA independently
  re-derives the predicted splice spacing rather than re-reading it from the
  implementer's comment.
- **UX:** no gate. Nothing here reaches a value display.
- **Fix PR:** gated on the D4 verdict, routed to architect on open, and
  `drive-path` does not apply (capture side, not stimulus).

## Open questions (Markus only)

1. Does D4's hardware A/B wait for the current cycle to complete, or is a
   partial run (transfer path only, no `ac monitor`) acceptable earlier? The
   single-vs-multi-channel discriminator is the strongest single piece of
   evidence available and it needs `ac monitor` non-TUI.
2. Should D1's ring-backed fake mode be built to also serve the ZMQ-drain
   determinism seam, or kept deliberately narrow? Sharing is cheaper now and
   couples two open investigations; narrow is more reversible.
