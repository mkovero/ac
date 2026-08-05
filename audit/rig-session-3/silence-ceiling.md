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

So it can be added as a guard against the noise tail that admission 16 is
otherwise buying, but it cannot replace the admission constant, and it must
not be described as a correctness check.

## Recommendation

**Admission 16, fixed 6 dB selection window, `MIN_PROMINENCE` deleted.** That
clears every measured silence ceiling including session 2's, keeps 8/8 of the
sessions the shipped gate refuses at both distant positions, and costs
time-to-lock rather than measurements.
