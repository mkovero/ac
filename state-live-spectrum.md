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
- **PR #222 — merged.** Switches `ac-view` to draw the three-stage columns.
- **PR #218 — merged**, after rework to revision 3 and then to fill downward.
  Bottom stage 4000 Hz, plain average of the last 4 blocks, N uniform, fixed
  block boundaries. No `τ`, no `α`, no `n_eff` left in the tree.
- **#208 — closed.** Cause: analysis blocks are cut from the head of a sliding
  buffer, so a transient gets re-analysed at a shifting weighting. Criterion 10
  run on the rig 2026-07-28: no recurrence on the new build, and none on the
  pre-#218 control either. Operator agreed closed. **The A/B had no positive
  control** — the 6 s level step is longer than the analysis window, so its
  edge gives a monotone ramp on both builds and could not excite the symptom.
  Closed on other evidence; the gap is recorded rather than assumed discharged.
- **#216 — closed.** Both halves done. Cheap half landed in #217
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
- **PR #234 — merged**, behind #232 as the gate required. #228, the fault
  indicator. New `ac-scene::fault` module holds the six-state table;
  `TransferScene` carries the row and `ac-view` draws it verbatim.
- **PR #232 — merged.** #227, earliest prominent correlation peak instead of
  the global maximum. #227 is closed.
- **Since, and not covered below** (all merged): #237 causal-only delay search
  with captures that reproduce their own decision; #239, which made
  `LOST LOCK` / `NO LOCK` reachable by publishing `delay_attempts` (#238);
  #250, admit on the noise floor and select in a fixed 6 dB window (#246);
  #253, escalate on attempts as well as seconds (#247); #240 smoothing; #242
  per-band resolution and settling labels; #252 banner/tick overlap (#245);
  #248, which made the metres readout conditional on a measured lock and put
  the speed of sound behind `ac setup temp`.

> **Updated 2026-08-06.** Everything above through #234 is `main`. The section
> below (**"What #218 changed in the rework"** onward) describes the state at
> the time of writing and has **not** been re-verified against `main` —
> read it as history unless it is one of the paragraphs the "Corrected, and
> worth not re-deriving" section marks as durable.

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

1. ~~**Frame cadence contradicts `ZMQ.md`.**~~ **Resolved 2026-08-06 by
   measurement — the doc was wrong, the worker was right.** `ZMQ.md` gave the
   H1 *window* (`capture_duration(4, sr)`, 2.5 s at 48 kHz) as if it were the
   frame interval. The publish interval is the loop tick: `chunk_secs` = 0.05 s
   plus per-tick processing. Measured **16.6 frames/s per pair** on
   `--fake-audio` at 48 kHz over 30 s, median gap 60.3 ms, which is consistent
   with the rig's 17.5–18/s at 96 kHz. `ZMQ.md` and the `transfer.rs` comment
   (which still claimed ~10 Hz from when `chunk_secs` was 0.2) are both
   corrected.

   Two things this turned up on the way:

   - The `≈2.5 s` sentence **is** in `ZMQ.md` — `handoff-doc-maintenance.md`
     says it is not and calls the attribution here a doc error. That part of
     the handoff is itself wrong; the attribution was correct and the source
     was the thing at fault.
   - **`--fake-audio` cannot run a transfer session over more than two
     distinct channels, and fails silently.** `pairs: [[0,1],[2,3]]` replies
     `ok: true` and then publishes **nothing at all** — no frames, no error
     frame, indefinitely. Three or more distinct channels is the trigger; pair
     count is not (`[[0,1],[1,0]]` streams fine). `FakeEngine::capture_multi`
     returns exactly two buffers via `capture_stereo` regardless of what the
     session asked for. Not filed yet; it is why session 3's `pairs=[[3,3],
     [0,3]]` worked, and it means any desk check of a genuinely multi-channel
     session is silently untestable today. Related to but distinct from #204.
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

**Rewritten 2026-08-06 against `main`.** The #225–#228 cluster is closed:
#225 landed in #233, #227 in #232, #228 in #234, and #226 is the only member
still open. Items 1, 2 and 4 of the previous list are done and are kept below
under "Discharged" for their reasoning, not as work.

1. **#226 — the last of the cluster.** `transfer_stream` locks delay against
   silence at warmup and never re-estimates. Session 3 narrowed what it is
   for: **the automatic-refresh half is not needed for lock stability** —
   88 sessions produced zero unstable locks, and every repeat at a fixed
   position agreed to the sample, *including the one wrong lock*. What #226 is
   still needed for is the **stimulus-before-session ordering**, which remains
   a real trap.
2. **The gate is the open question, and it is no longer one-directional.**
   Session 3 produced both sides: prominence 24 is too high for a clean 3 m
   measurement (refused 8/8 with `peak_lag` right to 3 cm) and *only just*
   high enough to exclude a near-wall one (7/8 refused, one accepted at 24.15
   and 52 cm wrong). A single threshold is being asked to separate two
   situations that differ in **where the peaks are**, not in how prominent
   they are. #251 is the capture that scores the selection half.
3. **#221 — snapshot parity divergence, now real rather than latent.** The
   live display draws the `mtw` columns, so a snapshot no longer matches the
   screen. This was the trigger condition the previous entry named.
4. #219 Part B stays open: the deterministic drain test + injection seam. The
   seam must carry a **mixed** stream (`keepalive` + `transfer_stream` +
   `visualize/ir`) or it will not reproduce Part A's behaviour at all.
5. **Not filed, and should be: the discarded second arrival.** On a two-source
   measurement the estimator locks the nearest arrival — correctly and
   confidently — and never tells the operator a comparable second arrival
   1.4 ms later was passed over. Disclosure gap, not a correctness bug. The
   shape of a fix is in `rig-session-3-results.md`: **arrival clusters, not
   peak counts** (the count version is recorded there as a dead end, censored
   at `MAX_CANDIDATES`).

Emission rules unchanged: explicit per-run consent, and the daemon run from
an isolated `HOME` with a server-side `drive_max_dbfs` clamp. The −40 dBFS
electrical ceiling was raised to −30 dBFS **for the 2026-07-28 session only**,
by explicit operator decision, because the acoustic path needs the mic SNR.
That does not carry forward.

<details><summary>Discharged — kept for the reasoning</summary>

- **#218 and #222 merged** (`3c73af6`, `bd40ed4`). The live display draws the
  three-stage columns, which is what made #221 live and criterion 10
  checkable.
- **Criterion 10 done** (2026-07-28, acoustic rig). No recurrence on the new
  build; the pre-#218 control showed none either, so **the A/B had no positive
  control** — see the #208 entry above. #216 closed on that basis, with the
  gap recorded rather than assumed discharged. Run D is still the positive
  control that was never run.
- **#224** — per-band Δf/settling labels — landed in #242. The reason it
  mattered: resolution and settling vary 24x across the screen, and the 2.5 s
  low-frequency lag reads as a fault without a label saying otherwise.

</details>
