# handoff-desk-work — what can progress before the next rig visit

Companion to `work/handoff/handover.md`. That document describes the state; this one lists
the work that needs **no hardware**, in priority order.

What genuinely waits for the rig: finding 4's resolution, the per-speaker
geometry measurement, and any further Run C positions. Everything below does
not.

---

## 1. Finding 3 — make the fault states reachable

> **DONE in part — #239 (issue #238) merged 2026-08-04, `0a4d033`.** A pair
> that never locks now paints `NO LOCK` from its first second and escalates
> with "check mic placement and routing" at 10 s. That is the case the rig
> actually hit, so the blank window is gone and the next rig visit is worth
> what this item said it would be.
>
> **Two things below are NOT done, and both wait on #226.** The daemon caches a
> pair's delay (`pair_delays[i].is_some() → continue`), so `delay_locked` is
> monotone false→true for a session's life. `LOST LOCK` therefore has no live
> path — it is a dormant row with tests written against #226's producer — and
> `CHECK ROUTING` is still unverifiable, because a refusing pair still gets no
> ladder and so no coherence columns. The "one fix unblocks both" claim below
> was half right: one fix unblocked the visible refusal, and #226 is what
> unblocks the other two. Section kept as the record of what was diagnosed and
> what it turned out to cover.

### The defect

`refusing = frame.settled && delay_locked == Some(false)`, and `settled` is
`frame.mtw.is_some()`. But the daemon builds the ladder only *after* a lock,
and `delay_locked` never returns to false once set. So:

- before a lock — no `mtw`, so not settled, so not refusing;
- after a lock — `delay_locked` is true forever, so not refusing.

Unreachable in both directions.

### It blocks two states, not one

`CHECK ROUTING` cannot be verified for the same reason: unrelated legs refuse →
no lock → no ladder → no coherence columns → `coherence_dead` has nothing to
evaluate. One fix unblocks both, and makes the `CHECK ROUTING` threshold
testable on the rig for the first time.

### Constraint that shaped the original design

`settled` was chosen over a timer specifically to avoid gating on
`delay_prominence`, which `ac-rs/ZMQ.md` documents as diagnostic-only — nothing
downstream may branch on it, including reading null-versus-present to separate
warmup from refusal. **That constraint still holds.** Whatever replaces the
current gate must not reach for prominence.

Two directions, architect's call:

- the daemon publishes a settling signal that exists independently of the
  ladder, so "warming up" and "refusing" are distinguishable without a lock;
- or `settled` stops being the gate and something else carries the same
  information.

**Resolved as the first.** The daemon publishes `delay_attempts`, a monotone
count of completed estimates, and the gate became `settled ||
estimator_attempted`. A count carries no threshold, so it does not gate on the
estimator's internals — the constraint above is respected. `ac-rs/ZMQ.md` carries the
argument next to the rule it has to survive.

### Also still true, and must not regress

- `delay_locked: false` is what a pair publishes **while warming up**, not only
  on refusal. Warmup must not present as a fault.
- Persistent refusal needs different words from a transient one — 10 s past
  settle, counted from after the ladder settles, not from session start. A mic
  that will never lock needs "check mic placement and routing", not a blank
  display and not a message that reads as a passing glitch.
  **Amended by #238:** the anchor is now the first *refused attempt*, because
  "after the ladder settles" is undefined for a pair that never builds one. The
  10 s is unchanged; for a never-locked pair it fires 2.56 s earlier than this
  text implied. `LOST LOCK` also narrowed to a pair that held a lock and lost
  it — a pair that never locked reads `NO LOCK` throughout, since nothing was
  lost.

---

## 2. Offline analysis of the rig captures

**The highest-value desk work available**, because it tests two open proposals
against real data without hardware.

`audit/rig-verify-125/` holds 2.1 MB of captures — `delay_evidence` plus
scalars, slimmed from 657 MB. Enough for gate-rule work; transfer curves would
need a re-run.

Every capture now reproduces its own decision (370/370 locked frames, 2069
refusing, zero failures), so a candidate rule can be replayed against recorded
evidence and scored.

### What to test

**Repeatability as a discriminator.** `peak_lag` was 1045 samples in seven of
eight sessions, to the sample, across 2069 frames. Noise wanders across
thousands of lags; a real arrival does not. Score a rule of the form "accept
when N successive independent estimates agree within k samples" against the
captures, and report how it separates the sessions the current gate refused.

**The negative-lag floor.** `negative_lag_median` is published on every frame.
Across eleven refusing sessions the two floors agree within 7%; the one session
that locked had the *weakest* arrival (`peak_value` 0.092 against ~0.18) and
locked anyway because `median_value` collapsed 4× while `negative_lag_median`
held — all-lag prominence 30.93, negative-lag 8.80. Recompute prominence
against the negative-lag floor for every captured session and report whether
the ranking changes.

### What this cannot settle

One position only, and the speaker configuration during those captures is
unrecorded. **Do not conclude the gate's correct value from this.** The
deliverable is a ranked comparison of candidate rules against real data, with
the confounds stated — not a new constant.

---

## 3. #229 — fractional-octave smoothing

**The largest genuinely independent piece of work in the backlog.** Touches
nothing in flight.

Half of it already exists: `ProcessingChain.smoothing_bpo: Option<u32>` is
carried through daemon provenance, serialised into the frame, and rendered by
both report writers as `"1/6 octave"` or `"off"`. Every producer sets it to
`None` and nothing smooths. This fills a declared hole rather than adding a
concept.

Designators 1/1, 1/3, 1/6, 1/12, 1/24, plus off.

**Rules that are not negotiable** (from `work/handoff/handoff-lock-and-smoothing.md`,
decision 4):

- **Smooth magnitude in dB.** Smoothing complex H1 reintroduces delay
  sensitivity — real and imaginary parts cancel where phase rotates.
- **Unwrap phase before smoothing**, or the average crosses wraps.
- **Coherence is not smoothed**, or only with a visible label. It is the trust
  indicator, and smoothing makes a bad measurement look good.

**Decide the octave convention explicitly before starting.** `ioct_band_centers`
and `ioct_band_edges` in `fractional_octave.rs` sit in the path of the open
`G_OCTAVE` work — base-2 against IEC 61260-1's 10^(3/10). Display smoothing is
Tier 2 and does not need conformant band edges, but it must not silently share
a constant that is about to change meaning underneath it. Write down which
geometry it uses and why. This is the same trap that has already appeared with
the ladder's decimation and with the crossover frequencies.

---

## 4. #230 — correct the delay-tolerance model

Ten minutes. `((W−D)/W)²` appears in `work/handoff/handoff-mtw-live-spectrum.md:239` and
`work/qa/qa-brief-218-222.md:51`, and QA's criterion 6 tells a reviewer the top stage
should "collapse toward" that ceiling under mutation.

It collapses much further. The dominant high-frequency term is phase rotation
across a display column's bandwidth, not loss of window overlap — measured
625 µs against 616 µs derived, tracking `sinc²(τ·BW)`. The window model
understates HF delay sensitivity by roughly an order of magnitude.

Keep the window form for low frequency; add the dispersion term. **Do this
before anyone sets a delay tolerance from the current text.**

---

## 5. #224 — per-band Δf and settling labels

UX has already designed it: labels at each band's geometric centre
(63.7 / 573.8 / 5697 Hz, computed not eyeballed, separation holding at all four
rates), dim structural grey, settling rather than raw window (the raw window
understates the wait by about 2.5×), strings `ac-scene`-owned and drawn
verbatim, session-static so built once.

The per-column `window_s` and `settling_s` fields are already on the wire.

UX flagged it should land before the ladder is used to tune a real system:
resolution and settling vary 24× across one screen with nothing saying so, and
"the bottom lags" is the kind of report that sticks.

---

## 6. `install.sh` does not ship `ac-view`

Has bitten twice; copied by hand both times. Worth its own issue and a fix.

While in there, the install path has produced two distinct stale-binary traps
worth defending against: once where size and mtime both passed on a differing
binary, and once where `sudo cp` partially succeeded — `ac-daemon` failed
`Text file busy` under the running daemon while the other two went through.
"Did the install command run" passes in both cases. **Verify by sha256 after
install, and stop the daemon first.**

---

## Also open, lower priority

- **#226** — maintained lock. Needs an architect scope gate before anyone
  starts, and **may change shape depending on how finding 4 resolves**: if the
  gate turns out to be the problem rather than lock stability, the automatic
  re-refresh half may not be worth building.
  Weigh that against what it now also carries: it is the only issue that gives
  `LOST LOCK` and `CHECK ROUTING` a producer at all (item 1), since both need a
  pair to return from locked to unlocked and nothing does that today.
  **The manual re-lock key is enough for that** — it makes the same transition,
  so a manual-key-only #226 still unblocks both states. So the automatic half
  remains a measurement-quality question, as originally framed; what is *not*
  optional is the key. Scoping #226 down to nothing, or deferring it entirely,
  is what would strand two of the six states, and that should be chosen
  deliberately rather than fall out of "bad locks turned out to be rare".
  Either shape must keep `delay_attempts` monotone; a count that resets on
  re-lock puts a locked-then-refusing pair back into "warming up", which paints
  nothing.
- **#221** — snapshot parity. Real now that the live view runs the ladder.
- **#219 Part B** — injection seam; the mixed-stream requirement is on the
  issue.
- **#201** — no CI. Every merge gate is currently one person's local run.
- **#214** — drive-path health. Conflicts with what has landed (`ac-rs/ZMQ.md`,
  `handlers/transfer.rs`, `wire.rs`), and raises a two-indicators UX question
  when it merges: two operator-facing elements would report the same physical
  fault in different vocabulary, and they will eventually disagree, since one
  reads graph topology and the other reads signal levels.

---

## Do not attempt without the rig

- Finding 4's resolution — the gate's correct value.
- Per-speaker geometry (loopback for converter latency, tape measure per
  speaker, one source energised at a time).
- Run C positions 1, 2, 4, 5.
- `CHECK ROUTING` verification — and **#239 did not unblock it**. Still no
  ladder for a refusing pair, so still no coherence columns. Blocked on #226
  first, then on rig access; do not queue it for a visit before #226 lands.
- Run D, #208's positive control. Two obstacles found while scoping: the daemon
  has no burst/gating primitive, so `set_drive` over ZMQ cannot approach 50 ms;
  and the recurrence lives in a response that mostly does not exist while
  sessions refuse.
