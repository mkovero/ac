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

### 1. #225 — reference output leg — **LANDED** (#233, `a14ee4a`)

`resolve_ref_output` resolves the reference **output** from
`cfg.reference_channel`, which is the reference **input** index everywhere
else. `Config` has no reference-output field at all.

Small, cause known, nothing depends on it. Until it lands, every session needs
manual JACK patching — and patching *after* session start produces the garbage
lock that cost a whole rig session.

Can run in parallel with anything.

### 2. #228 — the six-state indicator — **IMPLEMENTED, GATED** (PR #234)

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

**Built. Do not merge #234 ahead of #232** — the gate is stated under item 3
and the design decisions are in `state-live-spectrum.md`, "The fault
indicator (#228), as built". The short version: `LOST LOCK` reads
`delay_locked` rather than top-stage coherence, so the issue's own
0.715/0.05 discriminator is superseded as a threshold while surviving as
the evidence that motivated the issue.

### 3. #227 — peak picking

Reduces how often a bad lock happens at all, rather than detecting or
recovering from one.

~~Independent of #226 and #228 — different crate, different code — so it can
run in parallel with them.~~

**Superseded 2026-08-03. #228 waits on #227 landing.** The implementation and
the rebase can proceed in parallel — and did — but the *merge* cannot. #227
converts silent wrong locks into refusals, and a refusal is invisible without
#228: `h1_estimate` falls back to unaligned zero, which collapses HF exactly
like a bad lock did. A refusing session presents as a blank top end, arguably
worse for an operator than the confident wrong answer it replaced. #228 is
what makes #227's improvement legible, so it must be on screen first.

This reverses nothing else in this document: #228 still comes before #226, and
#227 is still independent of #226.

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

**#230** — ~~correct the `((W−D)/W)²` model in
`handoff-mtw-live-spectrum.md:239` and `qa-brief-218-222.md:51`.~~ **Closed
2026-08-06 as done in place.** The tracked occurrence,
`handoff-rig-findings.md:71`, already carries a `[revised]` correction naming
intra-band phase dispersion as the dominant HF mechanism. The two files cited
above exist only as untracked working copies — a clone has neither — and the
criteria they carry are criterion 6/7 for #218 and #222, both merged, so no QA
gate is still checking against the wrong ceiling.

The substance is still worth knowing and is why this was filed: the model
**under-states high-frequency sensitivity by roughly an order of magnitude**,
so do not set a delay tolerance from it.

---

## Parallelism

Disjoint code, safe to run at once:

- **#225** — daemon routing *(landed, #233)*
- **#227** — `ac-core` estimator
- **#229** — `ac-core` + `ac-scene`
- **#224** — `ac-scene`

Parallel *work* is not parallel *merging*: #227 and #228 touch different
crates and were built at the same time, but #228 must reach main second. See
item 3.

**#229 and #224 now collide with #228 in `ac-scene`.** #234 adds a `fault`
module, one field to `TransferInput`, one to `TransferScene`, and one argument
to `TransferScene::from_input`. Nothing in the mask, the ticks, or the
column path moved, so the collision is textual rather than semantic — but
whichever of the three lands second rebases.

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

> **Amended 2026-08-04, after #239 (issue #238).** "May not be worth building"
> was framed on measurement quality alone, and that was too casual: it missed
> that #226 is also the **producer** two fault states need. `LOST LOCK` and
> `CHECK ROUTING` both require a pair to return from locked to unlocked —
> today's daemon estimates a delay once and caches it, so `delay_locked` is
> monotone false→true for a session's life and neither state has a live path.
>
> This does not settle the scope question, and it is not an argument for the
> automatic half specifically: **the manual re-lock key produces the same
> transition**, so both states become reachable with manual-key-only. What
> changes is that the key is no longer an optional convenience — it is the
> minimum #226 must ship for the fault table to be testable at all, and a
> manual-only #226 has to be chosen knowing that, not by dropping the automatic
> half and assuming the rest is a feature preference.
>
> **Narrowed 2026-08-06 — the two states separate, and only one of them needs
> the key.** `LOCK ACQUIRED` is already reachable with no operator action:
> session 2's Run B induced it by enabling drive on a session that came up
> silent, `delay_locked` went false→true, and the transient painted. (Run B's
> induce column said "re-lock", which was the wrong word — no manual path
> exists; corrected in `18993a7`.) So the fault table's *confirmation* half is
> testable today.
>
> `LOST LOCK` is the half that is genuinely stuck: it needs a held lock to be
> invalidated, and `pair_delays` is never cleared (`handlers/transfer.rs:810`
> sets it; `:796`, `:847`, `:856`, `:913` read it; nothing resets it, on
> `set_drive` or otherwise). A key is one of the few things that would produce
> that transition. **Read the priority bump above as applying to `LOST LOCK`
> only** — manual-only #226 is still legitimate, but the reason is that it
> gives `LOST LOCK` a producer, not that it makes the fault table testable in
> general.
>
> Whichever shape it takes, `delay_attempts` must stay monotone across a
> re-lock. A count that resets puts a locked-then-refusing pair back into
> "warming up", which paints nothing — the exact defect #239 closed.

Conversely if #225 alone makes the reference reliable and locks stop failing,
the whole cluster shrinks. Neither is likely, but both are cheap to check and
would save the largest item in the set.
