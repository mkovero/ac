# STATE — live transfer display

Read this first after a gap. Update when something lands.

## The point

The live display has never been usable. Everything below serves that. If
something stops serving it, cut it.

## Where it stands

- **PR #215 — merged** (`25d078f`). Drain telemetry, gated behind
  `AC_DRAIN_TELEMETRY=1`.
- **PR #217 — merged** (`6038a9f`). #216 cheap half: warmup used a call that
  clears the measurement ring only, leaving every reference 0.2 s ahead for
  the whole session. Now always uses the call that clears both. Also wired
  `last_drain_occupancy` into the fake ring backend — it had been inheriting
  the empty default, so `AC_DRAIN_TELEMETRY` reported `occ=[]` in the one
  mode built to reproduce ring defects.
- **PR #220 — merged** (`cda40ef`). #219 Part A: `ac-view`'s drain loop
  stopped at the first non-`transfer_stream` frame, so it surfaced one frame
  out of 75 after a 2 s stall. Cause was the type filter, not libzmq and not
  EAGAIN — falsified against a real daemon.
- **PR #222 — open, stacked on #218.** Switches `ac-view` to draw the
  three-stage columns. Until it lands, #218 changes nothing on screen.
- **PR #218 — open, reworked to revision 3, then to fill downward.** Bottom stage 4000 Hz, plain
  average of the last 4 blocks, N uniform, fixed block boundaries. No `τ`,
  no `α`, no `n_eff` left in the tree. Workspace green, 758 tests.
- **#208 — closed.** Cause: analysis blocks are cut from the head of a sliding
  buffer, so a transient gets re-analysed at a shifting weighting. Criterion 10
  run on the rig 2026-07-28: no recurrence on the new build, and none on the
  pre-#218 control either. Operator agreed closed. **The A/B had no positive
  control** — the 6 s level step is longer than the analysis window, so its
  edge gives a monotone ramp on both builds and could not excite the symptom.
  Closed on other evidence; the gap is recorded rather than assumed discharged.
- **#216 — both halves done, ready to close.** Cheap half landed in #217
  (occupancy equal across rings, `delay_ms` 0.0 on a digital loopback where the
  skew gave −200 ms). General half confirmed in Run 3 of the acoustic session:
  top stage 0.755 against a real 5.9 ms acoustic delay, versus 0.05 when the
  lock is wrong — that comparison separates alignment working from the room
  being live.
- **#225–#230 filed** from the 2026-07-28 acoustic session. #225–#228 are the
  cluster standing between "works with workarounds" and "works"; see
  `handoff-issue-strategy.md` for order.
- **PR #233 — merged** (`a14ee4a`). #225, the reference output leg. Resolves it
  from its own playback index instead of `reference_channel`, an input index.
  Sessions no longer need hand-patching, and the launch reply carries a
  migration warning for configs written against the old meaning.
- **PR #234 — open.** #228, the fault indicator. **Must not merge ahead of
  #232** — see the gate below. New `ac-scene::fault` module holds the
  six-state table; `TransferScene` carries the row and `ac-view` draws it
  verbatim. Workspace green, 826 tests.

## What #218 changed in the rework

| | was (rev 2) | now (rev 3) |
|---|---|---|
| averaging | exponential, uniform τ | plain average of last 4 |
| block boundaries | fixed already | fixed, and now tested directly |
| bottom stage | 3000 Hz, 1.365 s | 4000 Hz, 1.024 s |
| LF settling | 4.9 s | 2.56 s (= today) |
| frame field | `n_eff` (float) | `n` (integer, = 4 everywhere) |
| request param | `mtw_n_target` | `mtw_n_blocks` |

Decimation factors: 48 kHz → 1/4/12, 96 kHz → 1/8/24, 192 kHz → 1/16/48,
44.1 kHz → 1/4/11 (4009 Hz, 0.23% off).

The uniform-N reasoning survives unchanged — it is what revision 3 ratified.
What changed is the shape of the averaging window.

Kept as-is: crossovers anchored to P_REF = 48; one `PairDecimator` with a
single phase counter; independent chains, no cascade; measured Kaiser lengths;
fallible `Vec<Stage>`. `aggregate.rs` untouched.

Criterion 5b now has a direct test: the same audio delivered in ragged chunks
(1, 4801, 97, 12000, 331, 2048 samples) must produce bit-identical columns.
Head-relative segmentation could not pass it.

`design-mtw-ladder.md` updated to revision 3 — it previously carried the stale
`τ`/`α`/3000 Hz markers.

## The design, in one paragraph

Three analysis stages instead of one: top at full sample rate, middle at
12000 Hz, bottom at 4000 Hz. Decimation factors derived from the sample rate,
so behaviour is identical at 48 and 96 kHz. Block boundaries fixed, each block
of audio analysed once — that is the #208 fix. Plain average of the last four
completed blocks, four at every stage, so coherence bias is the same
everywhere and there is no step at a crossover. No column interpolated: where
the display asks for more detail than the data supports, it gets coarser.
Reference alignment is one signed whole-sample offset per pair, applied at
full rate before decimation. SPL stays on the full-rate path, untouched.

## Scope

**In:** transfer magnitude, phase, coherence. The live tuning display, which
is what Smaart's live side does.

**Out by decision, not deferral:** the per-channel level curves
(`meas_spectrum`, `ref_spectrum`). Not displayed by this slice.

**Deferred:** bench mode; snapshot parity implementation (option (a) ratified,
interim consequence is that snapshots will not match the live view — **filed as
#221**); Tier 1 and the IEC 61260-1 filterbank; `reconnect_input` clearing the
measurement ring only (named in PR #217, not folded in).

## Verified on hardware (192.168.9.25, 96 kHz, 2026-07-27)

- **#216 both halves.** Ring occupancy `occ=[5120, 5120]` on every tick across
  five runs (was `[5120, 24320]`). With the digital loopback: `delay_samples=0`,
  coherence 1.0, |H1| 0.0 dB, phase 0.0 deg — where the skew gave -19200 /
  -200 ms / 0.64.
- **#218's ladder, everything except criterion 10.** Rungs derive correctly at
  96 kHz (stage 0 `df` 23.4375 / window 42.7 ms; bottom rung `df` 0.9766 /
  window 1.024 s), `n=4` uniform, `bins` never zero, 504 columns. Settling
  matching `W + hop*(N-1)`. **Realtime holds with the ladder running**:
  `session_rate=1.0000x` over 456 ticks, `zero_ticks=0`, no backlog — the one
  risk the headless tests could not show.
- **The display fills downward** (re-measured after the progressive-fill
  change): first columns at **+0.070 s** (219 columns above 2064.5 Hz), 363
  above 258.0 Hz at +0.824 s, all 504 at +2.541 s. Against the analytic
  0.107 / 0.853 / 2.560 s — each within one 53.3 ms drain tick. This
  supersedes the earlier "first frame at 2.56 s" measurement, which was
  correct for the code as it then stood.

## Verified on hardware (192.168.9.25, 96 kHz, 2026-07-28 — acoustic)

First session with a real acoustic path: mic on IN1, electrical loopback as
reference, speakers on ADAT. Full record in `rig-session-results.md`.

- **Delay tracks distance.** 4.5938 ms → 5.44/5.92 ms for a 34 cm axial move,
  against a 1.00 ms prediction. Baseline repeatable to **one sample** over
  eight sessions when the stimulus is present at session start.
- **Alignment holds on a real delay** (Run 3). Top stage 0.755 median / 0.715
  min over 539 frames. Its window is 42.7 ms, so this is alignment working,
  not a coincidence.
- **Settling matches the electrical figures** (Run 6): 0.079 / 0.828 / 2.532 s
  acoustic against 0.070 / 0.824 / 2.541 s electrical, all within 9 ms.
- **Two pairs stay independent** (Run 5): different delays, different
  coherences, magnitudes 45 dB apart. No coupling, no leakage.
- **Stage 0's 0.755 is reverberation-limited, not noise-limited** — flat to
  0.006 across 20 dB of input gain (Run 7). A healthy acoustic measurement in
  a live room legitimately sits well below 1.0. Do not set a coherence
  threshold from an electrical loopback.
- **The upper crossover step is present and stationary** (+0.05…+0.09, stable
  to ±0.003 over 40 s). The lower-crossover "drift" seen in one run did not
  survive a 65 s repeat — sign flips between sessions, and `n_blocks` is fixed
  at 4 so there is no 1/N convergence to drift with. Metric artifact, not a
  finding.

## Observed but not filed

Noticed during the acoustic session, unexplained, none blocking:

1. **Frame cadence contradicts `ZMQ.md`.** The doc says one frame per
   iteration, ≈2.5 s at 48 kHz. Measured ~18 frames/s per pair — 901 frames in
   25 s across two pairs, inter-frame gaps 12–50 ms. Two orders out. Either
   the doc is stale or the worker publishes far more often than one frame per
   capture window. Desk check; no rig needed.
2. **Per-pair settling offset.** Run 6, same session, same rings, same
   iteration: pair 0 settled at 0.079 / 0.828 / 2.532 s, pair 1 at 0.574 /
   1.317 / 3.027 s — a near-constant 0.5 s later on all three rungs. Only pair
   0's figures are quoted above, because those are the ones that match the
   electrical reference. A constant per-pair offset is not something the design
   predicts.
3. **`mtw: null` asymmetric between pairs** early in a session — within one
   iteration, meas=1 frames carried an `mtw` object while meas=0 frames did
   not. Possibly the same cause as (2).

## Corrected, and worth not re-deriving

The coherence floor on uncorrelated inputs is **1/N** (nominal blocks), not
1/3.2. Welch's rho = 1/6 corrects power-spectrum *variance*, not MSC *bias*;
applying it was wrong and it reached the code before being measured. Removed
from `ac-core` and `ac-scene`; **#223 withdrawn, not deferred.**

Effective depth also depends on bins per column, sublinearly, and that count
drops at every crossover by construction (crossovers sit where one bin fills
one column). A residual coherence step of ~0.05 at 1623 Hz is **accepted and
documented**, not engineered away — moving crossovers cannot help.

**Ship the inputs, not a derived depth.** The frame carries blocks-held and
bins-per-column. Two models of their combination have been wrong. Tables and
reasoning: `design-mtw-ladder.md`, "coherence depth — measured, not modelled".

## The fault indicator (#228), as built

PR #234. `ac-scene::fault` holds the table as a pure state machine plus a
little carried time state; `TransferScene.fault` carries the row out and
`ac-view` draws label and detail verbatim, so `computes_nothing` still holds.

**Merge order is a gate, not a preference. #234 must not land before #232.**
#227 converts silent wrong locks into refusals, and a refusal is invisible
without #228: `h1_estimate` falls back to unaligned zero, which collapses HF
exactly like a bad lock did. Landing #227 alone turns a confident wrong answer
into a blank top end, which is arguably worse for an operator. #234 is built
on `main`, not on #232 — only the order is gated.

**A lock fault is read, not inferred.** The issue's own discriminator — "both
legs live, HF collapsed, LF fine", stage 0 at 0.05 against 0.715–0.755 —
is **superseded**, along with those figures as thresholds. They were derived
when refusal did not exist and the only evidence of a bad lock was its
downstream effect. #227 makes the estimator say so itself, so the indicator
reads `delay_locked`: coherence is the symptom, the flag is the cause. This
also removes the hardest threshold in the set, since stage 0 sits at 0.755
legitimately in a live room (Run 7).

The figures stay valid as *evidence* — they are what established that a bad
lock is diagnosable at all. They are no longer what the code keys on.

**The dead-DUT ambiguity dissolves rather than being solved.** The issue asks
how to separate "your alignment is wrong" from "your device has no high end",
and proposes re-estimating the delay. Reading `delay_locked` sidesteps it
entirely: a low-passed DUT still produces a prominent correlation peak, so it
locks, so no lock row fires. Nothing in #228 needs the re-estimate, which
leaves it to #226 to build for its own reasons.

| quantity | value | why |
|---|---|---|
| "at the floor" | −80 dBFS, absolute | Never relative — the rig's own legs differ by 15 dB on a valid session. |
| "coherence dead" | every column < 0.5 | Reuses the display's own `COHERENCE_THRESHOLD` (D5). Not loopback-derived, not a second tunable. |
| ladder settled | observed | `mtw` presence, not a timer. 2.560 s is recorded as the design figure only. |
| persistent refusal | 10 s past settle | ~10 of #227's 1 Hz retries. Desk number, **not yet on the rig**. |
| lock-acquired hold | 3 s | Matches the clip latch's existing hold. |

**Drive state gates the level rows and nothing else.** `NO REFERENCE` and
`NO SIGNAL` need to know signal should be arriving; the lock rows do not. Two
legs above the floor are carrying signal whoever put it there, so a refusal on
a passive external-DUT session (`drivable: false`) is as real as one on a
driving session and *less* recoverable — the operator cannot resolve it by
starting the stimulus. The table's first row is about a drivable session
sitting silent, not about non-drivable sessions generally.

**A detail may name what to check; it may not assert a cause.** A refusal
means no sufficiently prominent peak, which is equally consistent with an
off-axis mic, with unrelated sources, and with a path that has nothing to
correlate. `NO LOCK` therefore reads "check mic placement and routing", not
"move the mic closer". This matters more after #227 lands, because unrelated
sources will then arrive through `NO LOCK` rather than through `CHECK ROUTING`
— which is correct (there genuinely is nothing to correlate) but makes a
diagnosis-shaped message wrong for that case.

**`CHECK ROUTING` narrows once #232 merges**, to "valid lock, dead coherence
everywhere". Accepted, not a defect. Before then it is the only routing
indicator, since `delay_locked` is absent.

**Nothing gates on `delay_prominence`.** `ZMQ.md` reserves its threshold to
the estimator. Warmup and refusal are separated by observed settling instead,
which is also why a timer from t=0 was rejected: it would fire the persistent
message on healthy sessions.

**`delay_locked` is `Option` on the consumer side.** It exists only on #232's
branch — `ZMQ.md` on `main` does not document it, and a search of `main` finds
it only in `brief-228-fault-indicator.md`. #234 reads it as an optional field,
so the lock rows stay dark on today's daemon and light up when #232 merges,
with no follow-up change. An absent `drive` object disables the indicator
outright rather than defaulting to "not driving": a daemon that does not
report its own drive gives no ground for any claim about whether signal should
be present, and it also predates the capture peaks, whose absence would
otherwise read as silence.

## Next

1. **#218 and #222 are MERGED** (`3c73af6`, `bd40ed4`). Main green at 780
   tests, fmt and clippy clean. The live display now draws the three-stage
   columns — #221's snapshot divergence is live from here, and criterion 10
   is finally checkable.
2. **Criterion 10 is done** (2026-07-28, acoustic rig). No recurrence on the
   new build; the pre-#218 control showed none either, so the A/B had no
   positive control — see the #208 entry above. **Close #216.**

   The remaining gap to "works for someone who is not Markus" is the
   **#225–#228 cluster**, and nothing else open is on that path. Order and
   reasoning: `handoff-issue-strategy.md`. In short: #225 (reference output
   leg) and #227 (peak picking) run in parallel; #228 before #226 because it
   builds the gates #226 consumes.

   **#225 has landed** (#233). **#228 is implemented** (#234) and gated on
   #227 landing first — see below. #226 and #227 remain.

   Emission rules unchanged: explicit per-run consent, and the daemon run from
   an isolated `HOME` with a server-side `drive_max_dbfs` clamp. The −40 dBFS
   electrical ceiling was raised to −30 dBFS **for that session only**, by
   explicit operator decision, because the acoustic path needs the mic SNR.
   That does not carry forward.
3. #219 Part B stays open: the deterministic drain test + injection seam. The
   seam must carry a **mixed** stream (`keepalive` + `transfer_stream` +
   `visualize/ir`) or it will not reproduce Part A's behaviour at all.
4. #224 — per-band Δf/settling labels on the transfer view. UX design is
   settled; implementation is hours. **Should land before the ladder is used
   to tune a real system**: resolution and settling vary 24x across the screen
   and the 2.5 s low-frequency lag reads as a fault otherwise.
5. #221 — snapshot parity divergence. Not a blocker for #218; **it is a blocker
   for the UX switch to the `mtw` columns**, which is the point at which a
   snapshot stops matching the screen. Latent until then: the frame is
   additive, `ac-view` still draws the Welch arrays, and the existing parity
   test still passes.
