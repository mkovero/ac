# handoff-208-triple-recurrence — locate the three-times replay, on the rig

Issue: #208. Base: `main` post-#207. Rig: **192.168.9.25** (RME Fireface 400,
JACK). Daemon CTRL `:5556`, DATA `:5557`.

**This is an investigation handoff.** The deliverable is instrumented
evidence that localises the defect to one side of a single boundary. No fix
lands in this slice — see §Out of scope.

Two earlier framings of this bug were wrong. Both are corrected below, and
the corrections are the most important part of this document: do not
reconstruct either one from the older files, which are still in the tree and
still say the wrong thing.

---

## Symptom, as reported (authoritative — supersedes all prior text)

After a stimulus event, the response is displayed **three times**. Then it
stops.

- **Count is stable.** Three, consistently. Not "several", not unbounded.
- **Temporal, not spectral.** The recurrences are separated in *time*, at
  roughly 3–5 s. They are not three peaks on the frequency axis.
- **Not frequency-dependent.** Reproduces on broadband transient stimulus —
  a finger snap — as readily as on an HF tone.
- **Each recurrence looks the same**, decaying as the stimulus ends.
- **Not indefinite.** An earlier version of this document said the
  recurrence continued forever with no stimulus present. That was wrong.
  It terminates after the third.

### Corrections to the record

1. `work/handoff/handoff-capture-contiguity.md` reads the symptom as three copies on the
   **frequency axis** and its D4 asks for three frequencies to be classified
   as geometric or linear-symmetric. **Void.** The symptom is temporal.
2. `docs/design/design-capture-contiguity.md`'s scope-correction block and
   `audio/contiguity.rs`'s module header both describe the recurrence as
   continuing **indefinitely with no stimulus present**. **Void.** It stops
   after three. Everything else in those two files stands — the splice they
   characterise is real, confirmed on hardware, and separately fixed (#207).

Fix both files' framing as part of this slice. Leave their technical content
alone.

---

## Ruled out (do not re-tread)

Each checked directly against the tree; recorded so the search starts narrow.

- **Anything frequency-dependent.** A finger snap is a broadband delta. That
  retires the splice/phase mechanism (H1), aliasing images, and the
  cycle-alignment trap `contiguity.rs` documents — all of them need the
  artifact to scale with frequency.
- **`spectrum_to_columns`.** Ported to Python with f32 semantics, run with a
  single tone through both live grids (transfer: `b = 24001`, `df = 1 Hz`,
  491 columns; monitor: `fft_n = 32768`, 4096 columns). Exactly one peak
  each. The `k` cursor is monotone and never resets.
- **The `meas_amp` frequency axis.** `h1_estimate_core` uses `nperseg = sr`,
  so the implicit axis `spectrum_to_columns_wire` reconstructs
  (`df = sr/(2·(b−1)) = 1 Hz`) matches `freqs[k] = k·sr/nperseg` exactly.
- **`ac-scene::to_points`** is a `zip`; **`ac-view::scene_to_screen`** is one
  affine map. Neither can duplicate anything.
- **UI-side replay.** `ac-view` holds no history, ember, or waterfall buffer
  — it is stateless per frame by construction (display-truth discipline).
  There is nothing there to loop.

---

## Leading hypothesis: the analysis window crawls, and Welch reports the
## same impulse once per segment position

The transfer worker (`handlers/transfer.rs:463–466`) runs
`nperseg = sr`, `step = nperseg/2`, `n_averages = 4`, so
`target_total = 2.5·sr` — a 2.5 s sliding window holding **four** Welch
segments at 50% overlap, with **three** internal segment boundaries.

An impulse deposited in that window is not consumed; it is re-analysed on
every tick until it slides out. If the window advanced at realtime it would
traverse in 2.5 s and the recurrences would blur into one continuous 2.5 s
event. For three *discrete* recurrences spaced 3–5 s, the window must be
advancing at roughly a fifth of realtime — i.e. the consumer is not keeping
up and the capture ring is accumulating backlog. That is H3 from the
original handoff, and it is the reading the reporter has favoured throughout.

Two arithmetic coincidences worth testing rather than trusting:

- `n_averages = 4` gives exactly **three** internal segment boundaries. A
  count of three that never varies wants a structural explanation, and this
  is the only "3" in the hot loop.
- Residence in frames is `2.5·sr / A` where `A` is samples popped per tick.
  Wall-clock span is that times the tick interval. **Both are directly
  measurable** — see D1. Do not assert this hypothesis; measure `A` and see
  whether the numbers land.

### Secondary: ref-ring saturation (weaker, but nearly free to exclude)

`REF_RING_CAPACITY = 4 · 192_000` and `RING_CAPACITY = 16 · 192_000` are
fixed **sample** counts, so their duration is rate-dependent:

| sr | ref ring | meas ring |
|---|---|---|
| 48 k | 16.0 s | 64.0 s |
| 96 k | 8.0 s | 32.0 s |
| 176.4 k | 4.35 s | 17.4 s |
| 192 k | 4.00 s | 16.0 s |

At 176.4/192 k the ref ring is 4–4.35 s, inside the reported spacing.
`push_slice`'s return is discarded at `jack_backend.rs:205,207`, so a full
ring drops silently. **Excluded by running the same session at 48 kHz:** if
the spacing changes with sample rate it is ring-capacity-bound; if it does
not, capacity is exonerated and this line of enquiry closes.

### Structural defect found while reading, independent of the above

`capture_multi_contiguous` pops `min_occupied()`. That equalises
**throughput, not phase** — every ring pops the same `n`, so differences
between ring occupancies are invariant under the drain. Any offset present
when streaming begins is permanent for the session.

The offset is nonzero by construction: refs are registered at
`transfer.rs:407`, **after** `eng.start()` at `:398`; the warmup flush at
`:504` calls `capture_block`, which is `CaptureRings::capture_block` →
`clear_meas()` — **measurement ring only**. It re-zeroes meas while leaving
every ref ring holding whatever accrued since it joined. Nothing in the
streaming path clears a ref ring again (`flush_capture` is not called in the
loop; `reconnect_input` clears meas only).

Before #207 the per-tick `clear_meas_and_refs()` destroyed that offset every
tick. The clear was doing two jobs — bounding latency *and* keeping meas/ref
phase-aligned — and #207 replaced only the first. Report this whether or not
it turns out to be #208; it is a regression either way.

---

## Deliverables

### D1. Tick telemetry (do this first — it is one line and may end the job)

In the worker loop, per tick, record: samples popped `n` (the length of
`bufs[0]`), `min_occupied()` before the pop, each ring's occupancy, and the
wall-clock interval since the previous tick.

From that, derive and report the **effective drain rate as a fraction of
realtime**. If it is materially below 1.0, backlog is accumulating and the
leading hypothesis is confirmed without further work. If it sits at 1.0,
the hypothesis is dead and the window is advancing normally — say so
plainly and stop, rather than looking for a way to keep it alive.

Also report whether `n` is ever 0, and for how long. A stalled
`min_occupied()` freezes `rings` while the loop keeps emitting.

### D2. Snapshot provenance test (the boundary discriminator)

`transfer.rs:540` pushes the exact `bufs` the analysis consumes into the
snapshot ring, one statement before the `rings` extend — 30 s retention,
FLAC inside the `.acsnap`. The instrument already exists; do not build a
second audio sink.

On the rig: snap once, let all three recurrences play out, take a snapshot,
open the audio.

- Impulse present **once** → capture is clean; the recurrence is
  manufactured at or after window assembly. Everything upstream of `rings`
  is exonerated.
- Impulse present **three times** → the capture ring re-served the audio and
  the analysis is faithfully reporting what it was given. The whole display
  chain is exonerated.

This is binary, frequency-agnostic, and halves the search space. It is the
single highest-value measurement in this handoff.

### D3. Producer-side drop counter

`discarded_samples` counts consumer-side `clear()` only, and deliberately
excludes uncounted clears. **It cannot see this failure** — both candidate
mechanisms are producer-side drops or zero-length pops. Add a counter in the
RT process callback comparing `push_slice`'s return against the input slice
length, per ring, cumulative.

Producer side is real-time: increment a preallocated atomic, allocate
nothing, lock nothing, format nothing. If that cannot be done cleanly,
say so and stop rather than putting a mutex in the callback.

### D4. Sample-rate exclusion for the secondary hypothesis

Run the same stimulus at 48 kHz and at the rig's current rate. Report the
recurrence spacing at each, and the rate the session actually ran at
(`eng.sample_rate()`, not what was requested).

### D5. Correct the two stale framings

`work/handoff/handoff-capture-contiguity.md` (frequency-axis reading, D4's
classification list) and the scope-correction blocks in
`docs/design/design-capture-contiguity.md` and `audio/contiguity.rs` (the "indefinitely,
no stimulus" claim). Framing only — their technical content is sound and
stays.

---

## Acceptance criteria (falsifiable)

1. Effective drain rate reported as a number, with the tick interval and
   per-tick `n` it was derived from. Not "the loop seems to keep up".
2. The D2 snapshot verdict stated as one of the two branches above, with the
   snapshot retained as an artifact on the issue. "Inconclusive" is a
   legitimate outcome and must be reported as such, not argued away.
3. Recurrence spacing measured at two sample rates, with the rate the
   session actually ran at recorded for each.
4. The count of three is either explained by a mechanism that predicts
   exactly three, or explicitly recorded as unexplained. A hypothesis that
   accounts for recurrence but not for the number is not a closed
   investigation — the invariance of that count is the sharpest clue in the
   report and must not be rounded off to "a few".
5. Full workspace green; zero edits to pre-existing assertions.
6. No behavioural change to the capture or display path in this slice
   (D3's counter is additive instrumentation; D5 is prose).

---

## Out of scope (hard fence)

- **Any fix.** Including the meas/ref offset defect above, however obvious it
  looks once D1 is in hand. Separate PR, architect-gated.
- `ac-scene`, `ac-view`, and the display path. Ruled out above; touching it
  means the investigation went sideways.
- Wire-protocol changes. D3's counter stays internal.
- Re-litigating the splice (#207). It is fixed, confirmed, and not this bug.
- `RING_CAPACITY` / `REF_RING_CAPACITY` retuning. D4 measures them; it does
  not change them.
- The LF ~10 s anomaly and the ZMQ-drain determinism item. Cross-reference
  only — though if D1 shows the transfer consumer running below realtime,
  say so on the LF issue, because that would be a shared root cause.

---

## Rig notes

- Rig at **192.168.9.25**; daemon CTRL `:5556`, DATA `:5557`.
- Hardware is a shared resource. Confirm nothing else is mid-measurement
  before driving outputs, and leave the drive off when idle — the dead-man is
  1500 ms (`workers.rs:12`) and the client keepalive 250 ms.
- Finger snap is a good probe and should stay the primary stimulus: it is a
  delta in time, so its position in a buffer is unambiguous in a way a steady
  tone never is. Prefer it to a generated tone for D2.
- `tests/it_loopback_ir.rs` is `#[ignore]`d and needs a real adapter. The
  headless suite cannot cover the JACK drain, so any claim about drain
  behaviour must come from the rig, not from the fake backend.
- The ring-backed fake (`fake_ring`, `process_secs`) models splices. It does
  **not** model producer-side overflow, so it cannot reproduce this defect.
  Do not treat a green headless run as evidence of absence.

## Routing

- **Architect:** D3's counter placement (RT-side, so the constraint is real);
  any proposal to change drain semantics arising from D1.
- **QA:** criteria 1, 3, 4 — independently re-derive the drain rate from the
  logged numbers rather than reading the implementer's computed figure.
- **UX:** no gate.

## Open question (Markus only)

What sample rate is the rig actually running at? It decides whether the
secondary hypothesis is live at all, and it is one line of output.
