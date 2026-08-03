# handoff-issue-strategy — order of work, and why

Read with `STATE-live-spectrum.md` and `handoff-lock-and-smoothing.md`.
This says what to do next and in what order; those say what the work is.

---

## Where the goal actually stands

The live transfer view works. It works **if** JACK is hand-patched to feed the
reference leg, and **if** the delay estimate happens to land on the direct
sound. Neither is something a second person could be expected to know.

So the remaining gap between "works for Markus who knows the workarounds" and
"works" is one cluster: **#225–#228**. Everything else open is polish or debt,
and none of it is on that path.

That is the sequencing principle below. Not urgency, not size — whether it
closes the gap.

---

## First: close #216

Its cheap half shipped in PR #217 and was confirmed on hardware (occupancy
equal across all rings, `delay_ms` reading 0.0 on a digital loopback where the
skew previously gave −200 ms).

Its general half — coherence lost to non-overlapping content at any DUT delay
— is addressed by the alignment in #218 and was confirmed in Run 3: top stage
0.755 with a real acoustic delay, against 0.05 when the lock is wrong. That
comparison is the evidence, because it separates alignment working from the
room being live.

Both halves are done. Close it.

---

## The cluster, in order

### 1. #225 — reference output leg

`resolve_ref_output` resolves the reference **output** from
`cfg.reference_channel`, which is the reference **input** index everywhere
else. `Config` has no reference-output field at all.

Small, cause known, nothing depends on it. Until it lands, every session needs
manual JACK patching — and patching *after* session start produces the garbage
lock that cost a whole rig session.

Can run in parallel with anything.

### 2. #228 — the six-state indicator

**Most value per unit of work in the whole set.** It fixes nothing; it makes
four invisible failures visible.

A session was lost to a dead reference leg with nothing on screen saying so.
`NO REFERENCE` is the difference between that session and a ten-second fix.
The same applies to `NO SIGNAL`, `CHECK ROUTING` and `LOST LOCK` — each names
a distinct cause with a distinct action, where today all four present as
"the top end looks wrong".

It also builds the signal-presence gates that #226 needs, which is the second
reason it comes before it rather than after.

Thresholds are specified in `handoff-lock-and-smoothing.md`: absolute
−80 dBFS floor, never relative between legs, and no coherence threshold
derived from an electrical loopback.

### 3. #227 — peak picking

Reduces how often a bad lock happens at all, rather than detecting or
recovering from one.

Independent of #226 and #228 — different crate, different code — so it can run
in parallel with them.

**It needs new fixtures before it needs a fix.** Every existing headless test
feeds one unambiguous correlation peak, so none of them can fail on this. At
minimum: a direct peak plus reflections, and an input with no correlated
content at all (Run 5's pair 1 locked confidently to 494 ms on one).

Acceptance is sub-millisecond, measured at 625 µs against 616 µs derived.
Falsify any fix against Run 1's data: it must turn the 22.8 / 30.3 / 30.4 ms
locks into either a correct lock or a refusal.

### 4. #226 — maintained lock

Largest of the four, and it consumes #228's gates, so it goes last in the
cluster. Recovery also matters less once detection exists and bad locks are
rarer — which is what 2 and 3 deliver.

**This is the one most likely to sprawl.** Re-lock interval, hysteresis, rate
limiting, flush semantics, state transitions, what happens when a re-lock
itself fails, whether a failed re-lock retries or latches — each is a
reasonable question, and together they are a month.

**Route to architect for scope before implementation starts**, with tight
acceptance written up front rather than discovered during. Given this
project's history, that is the specific risk worth spending a gate on.

---

## After the cluster

**#224** — per-band Δf and settling labels. UX flagged it should land before
the ladder is used to tune a real system: resolution and settling vary 24×
across one screen with nothing saying so, and "the bottom lags" is the kind of
report that sticks.

**#229** — fractional-octave smoothing. Fills the already-declared
`smoothing_bpo`. Decide base-2 versus the conformant `G_OCTAVE` helper
explicitly and write down which; do not inherit it.

**#221** (snapshot parity) and **#219 Part B** (injection seam) are debt.
Schedule when they start hurting, not before.

**#230** — correct the `((W−D)/W)²` model in `handoff-mtw-live-spectrum.md:239`
and `qa-brief-218-222.md:51`. Ten minutes. Do it opportunistically, but do it
**before anyone sets a delay tolerance from the wrong model** — it
under-states high-frequency sensitivity by roughly an order of magnitude.

---

## Parallelism

Disjoint code, safe to run at once:

- **#225** — daemon routing
- **#227** — `ac-core` estimator
- **#229** — `ac-core` + `ac-scene`
- **#224** — `ac-scene`

**#226 and #228 are not independent.** Same gates, same indicator, same state.
Either one work item or strictly sequential — #228 first. Splitting them
across agents means two things reading the same signals and disagreeing.

---

## Unfiled observations — below the cluster, deliberately

Three things were seen during the 2026-07-28 acoustic session that are
unexplained and not filed. None blocks the cluster; recorded so they are not
re-discovered, and because two of them are the same instrumented session.

**Desk work, no rig:**

- **Frame cadence contradicts `ZMQ.md`.** Doc says one frame per iteration,
  ≈2.5 s at 48 kHz. Measured ~18 frames/s per pair, inter-frame gaps 12–50 ms.
  Two orders out. Either the doc is stale or the worker publishes far more
  often than one frame per capture window. Read the publish path and correct
  whichever is wrong — but note that if the cadence is real, every "per frame"
  cost in the design docs is understated by the same factor.

**One instrumented session covers both:**

- **Per-pair settling offset.** Same session, same rings, same iteration: pair
  0 settled at 0.079 / 0.828 / 2.532 s, pair 1 at 0.574 / 1.317 / 3.027 s — a
  near-constant 0.5 s later on every rung.
- **`mtw: null` asymmetric between pairs** early in a session: within one
  iteration, meas=1 frames carried an `mtw` object while meas=0 frames did
  not. Plausibly the same cause.

Both are multi-pair-only, which is why the single-pair headless tests are
silent on them. Worth folding into whatever fixture work #227 needs rather
than scheduling on their own.

**Why they sit here and not in the cluster:** neither affects a single-pair
session, which is what the display is used for today, and the settling offset
costs half a second on a rung that already takes 2.5 s. They become
interesting when multi-pair fan-out is a supported workflow rather than a
capability.

## What would change this order

If #227 turns out to make bad locks rare enough that they stop mattering in
practice, #226's automatic refresh loses most of its value and could drop to
manual-key-only. Worth re-checking after #227 lands rather than committing to
the full #226 scope now.

Conversely if #225 alone makes the reference reliable and locks stop failing,
the whole cluster shrinks. Neither is likely, but both are cheap to check and
would save the largest item in the set.
