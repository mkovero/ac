# The three silence ceilings are one statistic, under-sampled

Reconciling the numbers that would set the admission constant in #246.

| source | attempts | worst prominence |
|---|---|---|
| `cand-silence` (this session) | 19 | 5.37 |
| the floor-rule scoring (3 baselines) | 66 | 6.75 |
| session 2, `NOISE_FLOOR_PROMINENCE`'s derivation | 40 pairs | 7.73 |

## They are the same statistic

Two checks, both on tonight's data.

**The "non-peak candidate" figure is not a different quantity.** In silence
the strongest non-peak candidate sits **0.2%** below the peak (5.37 against a
prominence of 5.38 in the same 19 attempts) — noise peaks cluster, so the
runner-up is the peak for all practical purposes. Reporting one rather than
the other changes nothing.

**The spread is sample size.** Pooling every silent attempt measured tonight
(85, three positions, both ends of the evening):

| attempts pooled | max |
|---|---|
| 19 | 3.33 |
| 66 | 5.00 |
| **85** | **6.75** |

median 4.32, p90 5.38, p99 6.75. This is a maximum of a noise distribution:
it grows with the number of draws, by construction, and does not converge.
**5.37 was an under-sampled max, not a lower ceiling.** Session 2's 7.73 over
40 independent uncorrelated pairs is the same statistic again, and being the
largest sample of the three in effective terms it is the one to design
against.

## What that does to the interlock

Admission at prominence `P` puts the fixed selection cut at `P/2 x median`:

| admission | selection cut | vs 6.75 | vs 7.73 |
|---|---|---|---|
| 12 | 6.0 | −11% | −22% |
| 14 | 7.0 | +4% | −9% |
| **16** | **8.0** | **+19%** | **+3.5%** |
| 18 | 9.0 | +33% | +16% |

The interlock argument survives — admission still sets selection's cut, and no
second constant is needed — but **12 is the wrong admission constant for it**.
The margin computed at 12 was measured against the smallest sample.

## What 16 costs, measured

Sessions still lock. A session locks on its *first* qualifying attempt, so a
lower per-attempt yield costs time, not measurements:

| admission | A 3.000 m | B 3.2 m | first qualifying attempt (median / worst) |
|---|---|---|---|
| 12 | 8/8 | 8/8 | 2 / 3 |
| 14 | 8/8 | 8/8 | 2 / 5 |
| **16** | **8/8** | **8/8** | **3–4 / 16–18** |
| 18 | 6/8 | 7/8 | 3–7 / 20–23 |

Attempts run about 1 s apart, so admission 16 keeps every session that 12
keeps at both distant positions, at a worst case of ~18 s to lock instead of
~3 s. Admission 18 starts losing sessions outright.

Selection accuracy at 3.000 m, fixed 6 dB window, per admitted attempt:
12 → 11/12 correct, 14 → 3/3, 16 → 1/1.

## Why agreement-across-attempts does not substitute

Tempting, since a noise-driven lock should not repeat: require two attempts a
full ring apart to agree within ±120 samples before caching. Scored:

| | fires | correct |
|---|---|---|
| A 3.000 m | 8/8 sessions | **2/8** |
| B 3.2 m | 8/8 sessions | 5/8 |
| silence ×2 | **0/2** | — |

**It is an excellent noise filter and useless as a correctness test.** Silence
never fires. But the competing reflection at 3 m sits **93 samples** from the
direct arrival — *inside* the ±120 tolerance the offline scoring derived — so
two attempts "agree" while landing on different arrivals. Same blind spot the
near-wall position exposed: agreement measures whether the room is stable, not
which peak is the direct one.

It is revisited at the end of this note, once the admission question is
settled by measurement rather than by ceiling-chasing.

## You cannot design against a maximum — so measure the conjunction instead

3.33 → 5.00 → 6.75 is not converging, and 7.73 will be exceeded by the next
dataset. Admission 16 buys 3.5% against a figure that is not stationary.

A noise ripple only causes harm under a **conjunction**: it must clear the
selection cut *and* sit earlier than the direct arrival, because the rule
takes the earliest qualifying candidate. A late ripple loses to the arrival; an
early one below the cut is not a candidate. That conjunction is the thing to
measure.

**Measured directly (`early_ripple.py`) — candidates earlier than the arrival
and above the cut:**

| capture | attempts | at `0.5·peak` | at admission 12 | at admission 16 |
|---|---|---|---|---|
| A 3.000 m | 19 | **0** | 0 | 0 |
| A ~1.10 m | 2 | 0 | 0 | 0 |
| A 1.8 m | 2 | 0 | 0 | 0 |
| A+B 1.8/2.5 m | 2 | 0 | 0 | 0 |
| A 2.4 m near wall | 24 | **0** | 0 | 0 |

Zero in every capture that has candidate lists — 49 attempts. On its own that
bounds the rate at ~6% per attempt (rule of three), which is not yet an
argument.

**The silence capture makes it one.** The harmful region is not the whole
scan: the arrival sits at lag 400–950, so only lags below it can do damage.
Restricting the noise ceiling to that window:

| | max value/median |
|---|---|
| over all causal lags | 5.38 |
| **over lags 0–1000 only** | **4.32** |

Only 8.9% of silent causal candidates land below lag 1000 at all (30 of 337,
median 0 per attempt). The early window is a small fraction of the search, so
its maximum is far below the full-scan maximum — which is the quantitative
form of why no rule ever picked an early ripple.

The reported candidate set cannot hide a stronger one: candidates are
everything within 12 dB of the peak, capped at 32 *by rank*, so anything
unreported is weaker than the weakest reported — about 1.0–1.35× median in
these captures, nowhere near the cut.

## Recommendation, revised

**Admission 12, fixed 6 dB selection window, `MIN_PROMINENCE` deleted.**

Admission 12 puts the selection cut at 6.0× median. Against the statistic that
actually governs harm — the noise ceiling *inside the early window*, 4.32 —
that is **+39%**, not the −11% the full-scan maximum suggested. And the full-scan
maximum is the wrong comparison anyway: a ripple at lag 40000 clearing 6.75×
median cannot be selected over an arrival at lag 947.

16 was chasing a number that does not converge, at a real cost: worst-case
time-to-lock of 16–18 attempts against 3 at admission 12.

## The #239 interaction, which decides this independently

`PERSISTENT_REFUSAL_S` = 10.0 s, anchored on the estimator's *first refusal*,
and it renders "check mic placement and routing".

| admission | worst first qualifying attempt | ~time to lock | fires #239's advice first? |
|---|---|---|---|
| **12** | 3 | **~3 s** | no |
| 14 | 5 | ~5 s | no |
| 16 | 16–18 | **~18 s** | **yes** |
| 18 | 20–23 | ~23 s | yes |

At admission 16 a perfectly good 3 m measurement would tell the operator to
move the mic, then lock correctly eight seconds later — worse than the blank
screen #228 was written to end. Admission 12 lands well inside the threshold.

Two consequences worth carrying:

1. Any future admission raise must be re-checked against
   `PERSISTENT_REFUSAL_S`, which is not currently a documented coupling.
2. The threshold is expressed in seconds but what varies is **attempts** — the
   retry interval is `RELOCK_RETRY` = 1 s today, and any change to it silently
   moves the fault text relative to the estimator's progress. Attempts are the
   natural unit. Filed separately.

## The agreement guard: recorded dead, so nobody proposes it again

Two independent confirmations now:

- **At 3 m** the competing reflection sits 93 samples from the direct arrival,
  inside the ±120 tolerance — fires 8/8 sessions, correct 2/8.
- **Near the wall** every candidate is 0.5–0.8 m long and successive
  independent estimates agree to p90 9 samples around the wrong one.

It never fires in silence (0/2), so it is a sound **noise** filter and nothing
more. It must never be described as a correctness check: agreement measures
whether the room is stable, not which peak is the direct arrival. Only the
geometry model separates those.
