# The negative-lag floor does not separate. Block 2, answered offline.

`$AC_HOME/rig-verify-queue.md` block 2 asked one question of session 3's
captures, and said a capture set that answers "no" is as valuable as one that
answers "yes":

> Does `peak_value / negative_lag_median` separate the valid locks (prominence
> 7.1–25.8 on the all-lag statistic) from the noise ceiling, where
> `peak_value / median_value` does not?

**No, and the reason closes the whole family rather than this one variant: the
premise is measurably false.** The all-lag floor is not contaminated — driven
captures sit **3.5%** above silent ones on `R = median_value /
negative_lag_median`, against **±17.5%** per-attempt spread on that same ratio.
Every "uncontaminated floor" proposal rests on there being contamination to
remove, and there is a fifth as much as the noise it would have to be read
through. This is the second measured refutation, after
`audit/rig-verify-125/gate-rules-offline.md` §2 (12 captures, ≤8%); this one is
70× the data with a tighter bound.

The specific variant then also underperforms — it narrows the margin against
silence from 1.37× to 1.04× and *promotes* the one wrong lock in the session —
but that is a consequence, not the finding. A statistic that merely
underperforms invites a better variant. A premise that is false does not.

Scored by `negative_lag_rule.py` over **843 attempts** — every capture in
`audit/rig-session-3/`, thinned to **one record per estimator attempt**. That
thinning is load-bearing rather than tidiness: `pair_prominence` is cached and
republished every frame, so scoring frames would have counted a single
attempt's evidence hundreds of times, weighted each run by how long it refused,
and produced a far tighter-looking result out of the same underlying decisions.
No rig time was used.

---

## 1. The premise is false — and the tree said so already, in the same file

**Before any of the measurement below: this was settleable by reading.**
`visualize/transfer.rs` has carried both the premise and its contradiction,
about 170 lines apart, for as long as the proposal has been open.

| line | what it says |
|---|---|
| `:319–332` — `negative_lag_median`'s doc comment | the premise: *"on a reverberant path most lags hold reverberation, so the statistic is contaminated by the thing it is meant to discriminate against"* |
| `:491` — the implementation comment on the floor itself | the refutation: *"the median is unmoved by the peak itself and by a reverberant tail, both of which occupy a small fraction of the lags"* |

They cannot both be right about the same quantity, and the second one is. A
claim the tree already contradicted survived two rig sessions and shaped a
capture plan, because nobody put the two comments side by side. That is the
same class as everything else this session produced: the information was
present, and the failure was in reading it rather than in collecting it.

### The measurement, which agrees with `:491`

`R = median_value / negative_lag_median` should be clearly above 1 on driven
captures and ~1 in silence if the premise holds.

| population | attempts | R median |
|---|---|---|
| driven (10 positions + the wall) | 777 | **1.035** |
| silent (3 baselines, both ends of the evening) | 66 | **0.999** |

**3.5%.** Not 3 dB, not 10 dB — 3.5%. A median over a 34 ms scan is not moved
by a room. **The reverberation argument should not be raised a third time.**

### This also dissolves the drift confound rather than answering it

The obvious objection to session 3's 1 m / 3 m inversion — 8/8 locks at
1.000 m, 0/8 at 3.000 m — is that the two positions were measured hours apart
while the room floor moved, so distance and drift are confounded. That
objection does not reach this result, and it is worth saying plainly because it
is the first one anyone will raise.

The refutation here is not that the statistic failed to separate two positions.
It is that **both floors measure the same quantity to 3.5%, on the same data, in
the same frame, at the same instant.** Every `R` above is a within-frame ratio.
Nothing about it can be confounded by drift between positions, by time of
evening, or by which position was measured first — a session-independent and
position-independent comparison, which is exactly what makes it decisive
against a proposal that was argued from session-to-session drift in the first
place.

## 2. The contamination is smaller than the noise on the statistic

Both floors are medians of the same magnitudes. The negative-lag one is taken
over half as many lags, so it is the noisier estimate of the same quantity —
and `R`'s spread is that noise, since a noiseless pair of estimators would put
`R` at a constant.

| | value |
|---|---|
| contamination to recover (driven R over silent R) | **+3.5%** |
| per-attempt noise on R (silence, p10–p90 half-width) | **±17.5%** |
| ratio | **0.20 : 1** |

The signal is a fifth of the noise it would have to be read through. This is
not a marginal result that more captures would resolve; more captures would
sharpen the medians and leave the per-attempt decision — which is what a gate
makes — exactly as noisy.

## 3. Separation gets worse, measured

Silence ceiling, 66 attempts pooled across three positions and both ends of the
evening:

| statistic | max | p90 | median |
|---|---|---|---|
| `peak/median` | **6.75** | 5.66 | 4.47 |
| `peak/negmedian` | **8.34** | 6.17 | 4.47 |

The 6.75 reproduces `silence-ceiling.md`'s 66-attempt figure exactly, by an
independent path — that is the pipeline check, not a coincidence worth
reporting on its own.

The two floors have the same median in silence (4.47 both), so the ceiling
rises purely because the negative-lag floor is the noisier estimator: same
distribution, fatter tail, higher maximum. Against the worst attempt that
landed on the true arrival:

| statistic | worst on-arrival attempt | silence max | margin |
|---|---|---|---|
| `peak/median` | 9.26 | 6.75 | **1.37×** |
| `peak/negmedian` | 8.71 | 8.34 | **1.04×** |

**The margin collapses from 1.37× to 1.04×.** Both numbers are maxima of a
noise distribution and grow with sample size — `silence-ceiling.md` is the
argument for not designing against either of them — but they are drawn from the
same 66 attempts, so the *comparison* is sound even though neither absolute
figure is stationary.

## 4. It promotes the wrong answer — which is the comparison that matters

Silence is not what a gate fails on. `runF-wall` is: 2.4 m with the capsule
28 cm from a wall, refused 7/8, **accepted once at prominence 24.15 and 52 cm
wrong**, with no candidate at that position corresponding to the direct arrival
at all.

| | `peak/median` | `peak/negmedian` |
|---|---|---|
| worst valid on-arrival attempt | 9.26 | 8.71 |
| worst (highest) wall attempt | 24.15 | **25.20** |
| headroom — valid over wall, >1 to be separable | 0.38× | **0.35×** |

Session-level, at the shipped admission of 24:

| statistic | valid sessions kept | silent admitted | **wall admitted** |
|---|---|---|---|
| `peak/median` | 53/69 | 0/6 | **1/8** |
| `peak/negmedian` | 54/69 | 0/6 | **3/8** |

Three times the wrong locks for one extra valid session. No threshold from 6 to
48 separates the wall from the valid captures on **either** statistic — which
is session 3's finding restated, not a new one: *24 is simultaneously too high
for a clean 3 m capture and only just high enough to exclude the wall, so it is
not a threshold problem.* A better floor does not fix a gate whose failure is
that the wrong peak is genuinely prominent. Only the geometry model separates
those.

## 5. What this does not touch

`gate-rules-offline.md` §2 killed the reverberation claim and kept a narrower
one: a correlation ring that **straddles the stimulus onset** is mostly
silence, so the all-lag floor collapses (there, `R = 0.364`) while the
negative-lag floor holds — and the all-lag statistic then scored the weakest
arrival in the set as the most prominent, which was the only lock the shipped
gate returned across twenty captures. That is a transient artefact, not a
reverberation one, and **nothing above bears on it.**

Session 3 cannot re-score it either way:

- The signature is **absent** from all 843 attempts — 0 below `R = 0.5`,
  lowest observed 0.720 — which is what a session that starts its stream after
  the stimulus is supposed to look like, and is therefore not evidence against
  the claim.
- Session 3's onset run (`run4`) recorded frame counters, not per-frame floors,
  so the one capture that could have carried the signature does not.

So: the general proposal is closed. The onset property is **open and
unaffected**, and it is a diagnostic about ring composition rather than a
replacement floor — a ring whose all-lag median has collapsed relative to its
negative-lag median is a ring that does not contain a steady stimulus, which is
a statement about the capture, not about the room.

### It has a home, and it is block 1, not this block

The onset guard was written and then **dropped before `rig2-fixes-125`
shipped**, for two stated reasons (block 1): no synthetic onset ring could be
built where the causal-only search still returned a wrong answer, so the guard
had nothing left to prevent; and it would fire indefinitely on a legitimately
gated stimulus — Run D is a 50 ms burst whose ring is silent for most of its
length — suppressing locking outright on that session.

**A measured ring-composition discriminator is precisely what that guard
lacked.** `R` says *this ring straddles an onset* from the ring's own contents,
without a rule that also condemns a gated stimulus: a gated burst puts the same
noise in the negative lags as in the positive ones between bursts, so its `R`
does not collapse the way an onset-straddling ring's does. Whether that holds is
a measurement, not an argument — and it is the second reason block 1's onset
run has to carry per-frame floors.

So this block does not leave the property as a remainder. It goes into block 1
as a capture requirement, written there: **whatever run reproduces the onset
case must record per-frame `median_value` and `negative_lag_median`, not frame
counters.** Session 3's `run4` kept counters, so the one capture that could
have carried the signature does not — a run structurally unable to see the
thing it was there to observe, which is the `#[ignore]`d-snapshot failure one
layer out. Unwritten, the next session repeats it.

## 6. Recommendation

- **Close the proposal to re-base `prominence` on `negative_lag_median`.**
  Answered "no" on 843 attempts, and the failure is structural — a noisier
  estimate of an uncontaminated floor.
- **Keep publishing the field.** It costs one number per attempt, and §5's
  onset case is the reason: `median_value / negative_lag_median` is the only
  statistic that identified the false-confidence lock, and it can only be read
  from captures that carry both floors. Its doc comment should say it is a
  ring-composition diagnostic rather than a candidate floor — the current text
  frames it as an open proposal, and it is not one any more.
- **`R < 0.5` is the onset signature** — 0.364 observed, 0.720 the lowest
  steady-state value across 843 attempts. Written into block 1 as a capture
  requirement, with Run D as its control, because it is the discriminator the
  dropped onset guard lacked. It is not a gate, and it never fires on the case
  a gate exists to catch.
- **The gate work is unchanged by this.** Block 2 could have redirected the
  next rig visit and does not: the near-wall failure remains a wrong-peak
  problem that no floor and no single threshold addresses.
