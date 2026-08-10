# HANDOVER — pick up here

Written 2026-08-04, after rig verification of the #237 fixes.
**Supersedes the previous `work/handoff/handover.md`** (written after rig session 2).

Read this first, then `work/rig/rig-verify-125-results.md` for the measurements.

---

## One-line state

The estimator now locates the arrival perfectly and repeatably. The gate in
front of it throws that measurement away — and as of #239 the display at least
says so, which leaves the gate (finding 4) as the one thing between here and a
working measurement.

## What is on main

`0a4d033` — the ladder (#218/#222), reference output fix (#233), peak picking
with refusal (#232), fault indicator (#234), the rig2 fixes (#237), and
finding 3's reachable half (#239 / issue #238).

---

## What the verification established

| fix | result |
|---|---|
| negative lags rejected | **pass** — 0 negative locks and 0 negative `peak_lag` in 5789 frames, including 360 deliberately straddling a stimulus onset |
| captures reproduce their own decision | **pass** — 370/370 locked frames, 2069 refusing, zero failures |
| `CHECK ROUTING` fires | **cannot be verified** — see below; #239 did not unblock it, #226 is what will |

The onset case — the one item the queue said needed hardware — passed. The
session was started before the stimulus, reproducing session 2's exact −826 ms
condition. The non-causal peak beat the causal one in 52–76 of 90 frames per
run, so the failure condition was present and published; it is simply no longer
selectable. The peak snaps to the arrival within 0.35 s of onset.

---

## The finding that matters most

**Run 1 got zero locks in eight sessions, and that is the result, not a gap.**

`peak_lag` was 1045 samples in seven of eight sessions — **to the sample,
across 2069 frames**. Prominence ran 14.7–17.0 against a gate of 24.

So: **localisation is solved, gating is broken.** Those two questions have been
tangled since #227 opened and are now cleanly separated. The peak picker finds
the arrival exactly and repeatably; the gate rejects it. Everything remaining
in finding 4 is about the gate.

Session 2 called this position a coin toss at median prominence 21.8. In the
honest causal definition it is a clear refusal, and the ~7-point drop is the
non-causal energy that used to inflate the numerator. **Session 2's prominence
numbers are in a different definition and are not comparable to these.**

### Two candidate discriminators, neither settled

**Repeatability across attempts.** Identical `peak_lag` over successive
independent estimates is strong evidence of a real arrival — noise wanders
across thousands of lags, and session 2's position 1 showed genuine ambiguity
as *alternation* between two candidates rather than agreement. It is free,
contemporaneous, and depends on no floor estimate at all.

**Negative-lag floor.** The one session that locked had the *weakest* arrival
of the four (`peak_value` 0.092 against ~0.18) and locked anyway, because
`median_value` collapsed 4× while `negative_lag_median` held. All-lag
prominence 30.93; negative-lag 8.80. The contaminated floor scored the worst
arrival as twice as prominent as the best ones — exactly the failure predicted.
Across the eleven refusing sessions the two floors agree within 7%, so this
rests on one data point, but the direction is now evidenced rather than merely
plausible.

These are complementary, not alternatives. Consider both.

---

## The acoustic setup was never recorded, and it confounds part of this

**Discovered after the session, by Markus:** the room has two speakers on the
right side and one at the back, stereo-summed. Which of them were energised has
**never been recorded in any session**, and it is not certain the same set was
on in session 2 as in this verification.

That is a larger hole than any software finding here, and it changes what the
measurements mean.

**What survives.** The 3 m result. `peak_lag` = 1045 to the sample across 2069
frames in seven of eight sessions is a single dominant arrival with no
ambiguity to confound it, and prominence 14.7–17.0 was still refused by a gate
of 24. Gating is too tight on a clean single-source case.

**What is confounded.** Session 2's position 1 — 1/12 locks with `peak_lag`
*alternating* between candidates is exactly what two comparable sources
produce. Low prominence in that situation is **correct**: the measurement
genuinely is ambiguous, and the estimator may have been right to refuse. This
also becomes a candidate explanation for the unexplained 1/12-versus-7/7
inversion — two comparable arrivals at one position, one dominant arrival at
the other, rather than distance or room drift.

**The reflection-structure reading below is withdrawn.** Three lags with two
equal gaps is weak evidence at the best of times, and against an unrecorded
variable it is worthless. The lags may simply be different speakers.

Related and still unexplained: session 2's position 1 reported a cluster at
30.39 ms only 1.31 dB below the direct. A 27 ms flight is a 9.4 m path, which
inverse-square puts roughly 19 dB down — so it fits neither a reflection nor a
distant second source on level, only on timing. Resolving it needs the physical
layout.

**Procedural consequence, and it is the important one:** a single-arrival
estimator measured against a stereo-summed multi-speaker system is being asked
a question with no single right answer. That is not a defect in the estimator —
it is the wrong measurement procedure. Live sound isolates the source, measures
it, then measures the sum deliberately if the sum is what is wanted.

**So: one source energised at a time, and the speaker state recorded as a
session variable.** Add "what is connected and switched on" to the pre-flight
alongside the clock and the binary hashes.

## The open measurement that would settle the tolerance

The handoff's pass criterion — locks at 11.34 ms ± a sample or two — was **not
met**: the lock came at 10.438 ms, 86 samples early. The failure criterion was
not met either.

**The criterion was circular and should not be used again.** It anchored on
session 2's 11.34 ms, which was produced by the estimator #237 replaces. Four
sessions agreeing is not independent confirmation when they share a systematic
bias.

The three candidate lags are evenly spaced:

| | samples | ms | implied distance |
|---|---|---|---|
| session 2 accepted | 1088.6 | 11.340 | 2.84 m |
| now `peak_lag` | 1045 | 10.885 | 2.69 m |
| now accepted | 1002 | 10.438 | 2.53 m |

43.6 and 43.0 samples apart — 15.5 cm each, to within a sample. **Do not read
this as a reflection structure** (see the section above): with the speaker
configuration unrecorded, different sources explain it as readily, and the
even spacing across only three points is not evidence either way.

**To settle it without depending on the estimator:** with **one named speaker
energised**, run the same stimulus through the electrical loopback
(playback_2 → capture_4) for converter latency at zero flight, then
tape-measure that speaker to the mic. Acoustic minus electrical, times 343,
against the tape. Ten minutes, and it is the only measurement in this thread
that does not assume the estimator is right.

Doing this per speaker also gives an expected arrival time for each, which
makes every future lock checkable against geometry instead of against a
previous session's estimate.

---

## Priority order

**1. Finding 3 — half closed by #239 (issue #238), 2026-08-04.** The half that
mattered is done: a pair that **never locks** now paints `NO LOCK` from its
first second, and escalates to `NO LOCK` with "check mic placement and routing"
at 10 s. That was the case Run 1 hit in eight of eight sessions, and the blank
window in front of the operator is gone.

The original diagnosis was `refusing = frame.settled && delay_locked ==
Some(false)`, with `settled` = `frame.mtw.is_some()`, unreachable in both
directions. The daemon now publishes `delay_attempts` (monotone count of
completed estimates) and the gate is `settled || estimator_attempted`.

*(The specification error was mine — I endorsed observed-settle over
timed-settle without checking that the ladder only exists after a lock.)*

**What #239 did not fix, and #226 is what wakes it.** `pair_delays[i].is_some()
→ continue` in `handlers/transfer.rs`: the delay is estimated once and cached,
so `delay_locked` is monotone false→true for the life of a session. Two
consequences, both now recorded in `ac-scene::fault` rather than left to be
rediscovered:

- **`LOST LOCK` is a dormant row.** It needs `delay_locked` to go true and then
  false, which no daemon produces today. The code and its tests are written
  against #226's producer; they are specifications, not evidence of a live
  path. A rig tester must not read the absence of `LOST LOCK` as a defect.
- **`CHECK ROUTING` verification is queued behind #226 too, not only behind rig
  access.** Same underlying cause, unchanged by #239: unrelated legs refuse →
  no lock → no ladder → no coherence columns → `coherence_dead` has nothing to
  evaluate. Falling back to the frame's Welch `coherence` was considered and
  rejected — different bin count, different bias floor, and
  `COHERENCE_ALIVE_FRACTION` was measured against the ladder's 504 columns, so
  it would be a different test wearing the same name. Closing it properly means
  validating the threshold against both sources, which is a rig measurement on
  top of #226.

So the six-state table has one dormant row and one unverifiable row, and #226
is the single thing standing in front of both — specifically its **manual
re-lock key**, which is all either state needs to get a producer. The automatic
refresh is a separate question and does not gate them.

**2. Finding 4 — the gate.** Blocked on nothing now that captures reproduce
their own decisions. The offline data is in `audit/rig-verify-125/` (2.1 MB,
slimmed to `delay_evidence` plus scalars). Enough for `DIRECT_PEAK_FRACTION`
tuning; transfer curves would need a re-run.

**3. The per-speaker distance measurement above.** Ten minutes per speaker,
settles the tolerance question permanently and gives a geometric expectation
for every future lock. Do this before any further Run C work — without it,
position data is not comparable across sessions.

**4. Everything else** — see the open-issues list below.

---

## Rig facts (cumulative — all cost time to learn)

- **Clock stays `AutoSync`** (`numid=320 = 0`). The external master clocks the
  card over ADAT, and ADAT carries playback_5, the stimulus leg. Setting
  Internal breaks the speaker path. Do not "fix" this.
- **Wiring:** loopback is **playback_2 (AN2) → capture_4 (IN4)**. capture_3 is
  digitally silent; `PCM-AN1→AN1` and `Mic-AN1→AN1` are muted at the mixer.
  Session config wants `reference_channel: 3`, `reference_output_channel: 1`.
- **Stop the daemon before installing, and verify all three binaries by
  sha256.** The stale-binary trap has now fired twice with different shapes:
  once where size and mtime both passed on a differing binary, and once where
  `sudo cp` partially succeeded — `ac-daemon` failed `Text file busy` under the
  running daemon while the other two went through. "Did the install command
  run" passes in both cases.
- `install.sh` still does not ship `ac-view`. Copy by hand. Worth its own
  issue; it has bitten twice.
- **The mic is fixed at ~3 m on axis** (session 2's position 3).
- **Two speakers on the right side, one at the back, stereo-summed.** Which are
  energised has never been recorded and may have differed between sessions.
  Record it every time; energise one at a time for delay work.
- **Room floor was stable this session** — 0.10 dB across the evening
  (−47.12 → −47.02), against session 2's 10 dB. That **weakens** the drift
  explanation for session 2's unexplained 1/12-versus-7/7 inversion between
  1 m and 3 m. Still unexplained.
- `~/.config/ac/config.json` is still wrong for this rig (`reference_channel:
  2`, pointing at silent capture_3). Anything run as the operator's own user
  reports `NO REFERENCE`. One-line fix, deliberately left to Markus.

## Still owed on hardware

- **Run C positions 1, 2, 4, 5** — blocked on the mic being movable. Finding 4
  has data from 3 m only.
- **1 m and 3 m back-to-back with contemporaneous baselines** — session 2
  measured them hours apart.
- **`CHECK ROUTING`** — blocked on **#226**, not on rig access and no longer on
  finding 3. #239 made the refusal *visible*; it did not give a refusing pair a
  ladder, so there are still no coherence columns to evaluate. Nothing on the
  rig can produce this state until the lock is a maintained quantity.
- **Run D** (#208's positive control) — not run, and two obstacles found while
  scoping it: the daemon has no burst/gating primitive, so `set_drive` over ZMQ
  cannot approach 50 ms; and the recurrence lives in a response that mostly
  does not exist here because sessions refuse. A one-sided run proves nothing
  without the `cda40ef` A/B.
- **`COHERENCE_ALIVE_FRACTION` false positive** — 10% of a 504-column frame is
  about one octave, so a DUT genuinely coherent over less than an octave reads
  `CHECK ROUTING`. Documented, not fixed. If the rig produces it, the
  discriminator to reach for is *contiguity* of coherent columns, not a smaller
  fraction.

## Open issues, untouched by any of this

#226 (maintained lock — needs an architect scope gate; may shrink or change
shape depending on how finding 4 resolves. **Now also the blocker for two fault
states**: `LOST LOCK` and `CHECK ROUTING` both need a pair to return from
locked to unlocked, and nothing produces that today. The **manual re-lock key
alone is enough** — the automatic-refresh half is still the measurement-quality
question it always was, but the key is not optional if the fault table is to be
testable. Landing either shape means the tests marked pre-#226 in
`ac-scene::fault` stop being specifications; both shapes must keep
`delay_attempts` monotone), #229 (fractional-octave smoothing —
biggest independent piece of work; needs the base-2 versus `G_OCTAVE` decision
made explicitly up front), #224 (per-band Δf and settling labels), #230 (ten
minutes of doc correction — do it before anyone sets a delay tolerance from the
`((W−D)/W)²` model, which understates HF sensitivity by about an order of
magnitude), #221 and #219B (debt), #201 (no CI — every merge gate is one
person's local run), #214 (drive-path health; conflicts with what has landed,
and raises a two-indicators UX question when it merges).
