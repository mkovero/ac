# Candidate gate rules, scored against the rig-verify-125 captures

> **Superseded in part, 2026-08-06 — read "What would settle it" as done.**
>
> Rig session 3 (2026-08-04) constructed the ambiguous case this file calls
> for. See `rig-session-3-results.md`, Run 2, third position: A at 1.8 m and
> B at ~2.5 m, 1.1 dB apart at the capsule, 134 samples of separation.
>
> | condition | locked | lag | median prominence |
> |---|---|---|---|
> | A alone | 6/6 | 628 | 26.75 |
> | B alone | 6/6 | 762 | 25.77 |
> | A + B | 8/8 | 628 | **28.07** |
>
> Prominence **rises** when the case turns ambiguous: a second correlated
> source moves the median slower than it moves the peak. Neither candidate
> rule refuses it, and **no threshold on that statistic can** — the direction
> is wrong, not the value. The candidate-count alternative is separately dead:
> 23 → 32, censored at `MAX_CANDIDATES`.
>
> Session 3 also disposed of the repeatability rule on its own terms: at the
> near-wall position, successive independent estimates agreed to 9 samples
> (3.2 cm) around an answer 52 cm from the truth. Agreement measures whether
> the room is stable, not whether the answer is right.
>
> **The scoring below stands and is not superseded.** The two rules still
> differ on the single-source data, which is where the choice between them has
> to be made, and this is the only record of that ranking. What is superseded
> is the closing section's call for a trip that has now happened.

Desk work item 2 (`handover-desk-work.md`). Reproduce with
`python3 gate_rules.py` in this directory; `--csv` adds machine-readable rows.

The captures themselves (`*-evidence.pkl.gz`, 2.1 MB, slimmed to
`delay_evidence` plus scalars) are **not committed** — the script expects them
beside it in `audit/rig-verify-125/`. Every number below comes from them, so a
checkout without them reproduces nothing; ask for the directory rather than
re-running the script against different data.

**This is a ranked comparison of rules, not a proposed constant.** One
microphone position, and the speaker configuration during the captures was
never recorded. The confounds section says what that costs. Nothing here sets
`NOISE_FLOOR_PROMINENCE` or any replacement for it, and the data cannot.

---

## What was scored, and against what

Twenty captures, labelled from the **rig setup** rather than from the
measurement — what was physically emitting, not what the estimator concluded:

| capture | n | label | condition |
|---|---|---|---|
| `run1-s1..s8` | 8 | pos | 3 m on-axis, pink into speaker leg + ref loopback |
| `run4-healthy` | 1 | pos | both legs driven, Run 1 configuration |
| `onset{1..4}-post` | 4 | pos | stimulus running |
| `run4-unrelated` | 1 | neg | pink into the ref loopback only; mic hears room noise |
| `baseline_before/after` | 2 | neg | ref leg at −95 dBFS, nothing to correlate |
| `onset{1..4}-pre` | 4 | neg | session open, stimulus not yet started |

13 positives, 7 negatives. "Positive" means a path existed for the estimator
to find. It does **not** mean the estimator's lag was right — the true flight
time is still open (10.438 / 10.885 / 11.340 ms), so every rule here is scored
on *accept versus refuse*, never on correctness of the lag.

### Two method choices that change the numbers

**Attempts, not frames.** Evidence is republished verbatim on every frame
(~10 Hz) while retries run at 1 Hz, so frame-level counting inflates any
"successive estimates agree" rule by about 10×. Everything below is scored on
attempts, deduplicated by evidence identity: 14 per Run 1 session, 19–21 for
the longer captures, 5 for the pre-onset windows.

**Any-attempt, not session median.** The daemon accepts on the *first* attempt
that clears the gate and caches it for the session. A session-median score
answers a different question than the shipped code asks, and the gap is not
academic: `onset4-post` has median prominence 18.0 and locked anyway, because
one attempt reached 30.93. Both scorings are reported where they differ.

---

## The captures

| session | label | attempts | locked frames | prom (all-lag) | prom (neg-lag) | modal lag | modal frac | distinct lags |
|---|---|---|---|---|---|---|---|---|
| run1-s1 | pos | 14 | 0 | 16.30 | 16.27 | 1045 | 1.00 | 1 |
| run1-s2 | pos | 14 | 0 | 15.04 | 15.81 | 1045 | 0.57 | 5 |
| run1-s3 | pos | 14 | 0 | 15.65 | 16.38 | 1045 | 0.57 | 4 |
| run1-s4 | pos | 14 | 0 | 15.65 | 16.12 | 946 | 0.43 | 3 |
| run1-s5 | pos | 14 | 0 | 15.17 | 15.83 | 1045 | 0.71 | 3 |
| run1-s6 | pos | 14 | 0 | 14.39 | 14.87 | 1045 | 0.50 | 4 |
| run1-s7 | pos | 14 | 0 | 16.64 | 17.07 | 1045 | 0.57 | 3 |
| run1-s8 | pos | 14 | 0 | 14.73 | 15.78 | 1045 | 1.00 | 1 |
| run4-healthy | pos | 19 | 0 | 14.44 | 14.77 | 1045 | 0.58 | 3 |
| onset1-post | pos | 21 | 0 | 15.06 | 14.55 | 1045 | 0.71 | 6 |
| onset2-post | pos | 21 | 0 | 14.04 | 14.49 | 1045 | 0.71 | 4 |
| onset3-post | pos | 21 | 0 | 14.84 | 14.95 | 1045 | 0.62 | 4 |
| **onset4-post** | pos | 2 | **370** | 18.02 | **6.79** | 4717 | 0.50 | 2 |
| run4-unrelated | neg | 19 | 0 | 4.67 | 4.63 | 1066 | 0.05 | 19 |
| baseline_before | neg | 21 | 0 | 4.69 | 4.53 | 12732 | 0.05 | 21 |
| baseline_after | neg | 21 | 0 | 5.30 | 5.25 | 36313 | 0.05 | 21 |
| onset1-pre | neg | 5 | 0 | 5.58 | 5.68 | 420 | 0.20 | 5 |
| onset2-pre | neg | 5 | 0 | 5.27 | 5.13 | 1298 | 0.20 | 5 |
| onset3-pre | neg | 5 | 0 | 4.38 | 3.90 | 14940 | 0.20 | 5 |
| onset4-pre | neg | 5 | 0 | 5.12 | 5.20 | 20518 | 0.20 | 5 |

Eleven of the thirteen positives put their modal peak at **1045 samples**, and
their non-modal attempts land on 945/946 or 1054/1055 — ±100 samples, ±1.0 ms,
±36 cm. `run1-s4`'s mode is 946, one step down that same ladder, on a 43/57
split. `onset4-post`'s modal 4717 is an artifact of having only two attempts,
one of them taken before the stimulus started; its post-onset attempt reports
1045 like the rest. Negatives never repeat a lag at all: 19 distinct lags in 19
attempts, 21 in 21, spread across the whole ±1 s search range.

---

## Ranked comparison

Ranked by how cleanly each rule separates the labels, and by how much of that
separation survives the caveats.

### 1. Repeatability — agreement between successive estimates

Accept when N successive attempts agree within k samples.

| N | k | positives accepted | negatives accepted |
|---|---|---|---|
| 2 | 0–16 | 12/13 | 0/7 |
| 2 | 4–16 | 12/13 | 2/7 |
| **3** | **0–16** | **12/13** | **0/7** |
| 4 | 0–8 | 8/13 | 0/7 |
| 5 | 0–8 | 7/13 | 0/7 |
| 8 | 0–8 | 4/13 | 0/7 |

Best separation of anything tested, and it separates without a threshold on a
signal-like quantity: it asks whether the estimates agree, not how large they
are. The single positive it misses is `onset4-post`, which locked on its second
attempt and therefore has only two attempts on record — the rule was never
given three. That is a structural artifact of scoring a rule against captures
made under a different rule, not a failure of the rule.

**But the attempts are not independent, and that is the finding that matters.**
The H1 ring is 2.5 s (`nperseg + step·(n_averages−1)`) and retries run at
1.014 s median spacing, so successive attempts share about 60% of their
samples. Rescored on attempts spaced at least one full ring apart:

| N | k | positives accepted | negatives accepted |
|---|---|---|---|
| 2 | 0–16 | 12/13 | 0/7 |
| 2 | 120 | 12/13 | 1/7 |
| 3 | 0–2 | **7/13** | 0/7 |
| 3 | 16 | 9/13 | 0/7 |
| 3 | 120 | 12/13 | 0/7 |
| 4 | 0–2 | 5/13 | 0/7 |
| 4 | 120 | 12/13 | 0/7 |

N=3 at k=2 falls from 12/13 to 7/13 once the windows stop overlapping. To keep
12/13 on genuinely independent estimates the tolerance has to reach k≈120
samples (±1.25 ms, ±0.43 m) — which is the ±100-sample structure the modal
table already shows, now load-bearing rather than incidental. Negatives stay at
0/7 throughout, including at k=120 for N≥3.

So the rule survives, but not in the form the handover sketched: "N successive
independent estimates agree **to the sample**" is not what this data supports.
Either the retries must be spaced by at least a ring, or the tolerance must be
wide enough to hold the ±100-sample spread — and those are different rules with
different costs (N seconds of delay versus 0.43 m of ambiguity).

### The independence problem generalises, and it changes what the rule claims

Any rule of the form "successive estimates agree" measures buffer overlap as
well as reproducibility, whenever the estimates are drawn faster than the
window turns over. Here that is 60% of the samples shared. The correction is
not a tweak to N or k — it is to state which quantity is being claimed, because
the honest version of the rule is a different claim from the sketched one:

- **not** "the estimate is exact and repeatable";
- **but** "the estimate is stable to about half a metre" (k≈120 samples,
  ±1.25 ms, ±0.43 m at 96 kHz).

**Which use it is proposed for.** As a *gate* — deciding whether to lock, align
the ladder, and let the display draw — 0.43 m is fine: the alternative is the
current behaviour, which refuses every genuine 3 m measurement in this corpus.
As a *reported delay*, it is not fine, and nothing here licenses printing a
distance derived from an estimate qualified this way. The gate's tolerance and
the readout's accuracy are separate numbers, and this rule sets only the first.
The per-speaker geometry measurement is what would bound the second.

### 2. Prominence against the negative-lag floor

`peak_value / negative_lag_median`, in place of `peak_value / median_value`.

| threshold | median form | | any-attempt form | |
|---|---|---|---|---|
| | pos | neg | pos | neg |
| 4 | 13/13 | 6/7 | 13/13 | 7/7 |
| 6 | 13/13 | 0/7 | 13/13 | 4/7 |
| 8 | 12/13 | 0/7 | 13/13 | 0/7 |
| 10–12 | 12/13 | 0/7 | 12/13 | 0/7 |
| 16 | 4/13 | 0/7 | 12/13 | 0/7 |
| 20 | 0/13 | 0/7 | 5/13 | 0/7 |
| 24 | 0/13 | 0/7 | 0/13 | 0/7 |

Clean separation exists — but so does it for the all-lag floor, and that is the
result. Side by side, `median_value / negative_lag_median` per session:

| condition | ratio |
|---|---|
| run1-s1..s8 | 0.999 – 1.071 |
| run4-healthy | 1.057 |
| run4-unrelated | 0.974 |
| baselines, onset-pre | 0.904 – 1.182 |
| **onset4-post** | **0.364** |

On every steady-state capture the two floors agree within about 8%, so
recomputing prominence against the negative-lag median changes almost nothing.
The one capture where they diverge by 2.7× is the one whose ring straddled the
stimulus onset.

**Recorded as a negative result: the negative-lag floor does not do what it was
proposed for.** The proposal was that reverberation contaminates the all-lag
median, since most lags on a reverberant path hold reverberation. At 3 m in
this room that contamination is ≤8% — measured, on twelve steady-state
captures, and too small to move any decision. The reverberation argument should
not be repeated.

**It survives for a different reason.** What it catches is a
**transient-deflated** floor: a ring that is mostly silence has a collapsed
all-lag median and therefore an inflated prominence, while the negative-lag
median does not collapse with it. That is a narrower property than claimed and
a real one — it is the exact mechanism behind the false accept in section 4,
and the only observed case where the two floors disagree at all.

### 3. Both rules together

N=3, k=2 on all attempts, plus a negative-lag threshold: 12/13 positives and
0/7 negatives for every threshold from 4 to 12. The conjunction adds nothing
over repeatability alone on this data — the same 12 sessions, the same miss.

### 4. The shipped gate, for reference

All-lag prominence ≥ 24: **0/13** positives on session medians, **1/13** on the
any-attempt form the daemon actually uses. Zero false accepts.

The one session it accepts is `onset4-post`. It has only two attempts on
record — one before the stimulus, one that locked — so its session medians in
the table above are meaningless and the accepting attempt has to be read
directly:

| | accepting attempt | other positives |
|---|---|---|
| `peak_value` | 0.0923 | ~0.18 |
| `median_value` (all-lag floor) | 0.0030 | ~0.012 |
| `negative_lag_median` | 0.0105 | ~0.012 |
| prominence, all-lag | **30.93** | 14.0–16.6 |
| prominence, negative-lag | **8.79** | 14.5–17.1 |
| `peak_lag` | 1045 | 1045 |
| accepted lag | **1002** | — |

So the weakest arrival in the entire set — half the correlation peak of every
other positive — produced the highest prominence in the entire set, and it is
the only lock the shipped gate returned across twenty captures. The mechanism
is visible in the two floors: the all-lag median collapsed 4× because the ring
was mostly pre-stimulus silence, while the negative-lag median held. Against
the uncontaminated floor the same attempt scores 8.79, below every other
positive (14.5–17.1) and above every negative (≤5.68) — still separated, but
ranked last among the positives rather than first overall.

Its `peak_lag` was 1045, agreeing with everything else. The **accepted** lag
was 1002, 43 samples earlier: the earliest-peak rule took a candidate within
6 dB of the 1045 peak. That is the rule operating as designed on a capture
whose floor should not have let it operate at all, and it is why the accept is
worth reading as a false accept rather than a lucky one.

---

## Ranking, with the caveats attached

1. **Repeatability** — best separation, no threshold on a signal-like
   quantity, and the only rule whose false-accept rate does not grow with
   session length. Costs: needs its independence assumption enforced
   explicitly (spacing or tolerance), cannot be scored on captures that locked
   early, and delays a lock by N retries. Scope of the claim: **a gate
   tolerance, not a readout accuracy** — see the independence section.
2. **Negative-lag floor** — a real correction, but for a narrower fault than it
   was proposed for: onset/transient deflation, not reverberation. Keeps a
   tunable constant, and the any-attempt form's false-accept probability still
   grows with session length.
3. **Both** — no measurable gain over repeatability alone here.
4. **All-lag prominence ≥ 24** — refuses every genuine 3 m measurement and
   accepts exactly one session, the one with the deflated floor and the outlier
   lag.

One structural note that applies to all of them: because the daemon accepts on
the first attempt that clears and caches it, any threshold rule is a
"wait long enough and something clears" rule, and its false-accept probability
grows with session length. The repeatability rule does not have that shape —
noise does not become more likely to agree with itself over time.

---

## What this cannot settle

- **One position.** 3 m on axis, mic fixed. Nothing here says anything about
  1 m, off axis, or a further position.
- **The speaker configuration was never recorded**, and may have differed
  between captures. Two speakers on the right and one at the back, stereo
  summed. A single-arrival estimator against an unknown multi-source sum is
  being asked a question with no single right answer, so the ±100-sample spread
  in the positives may be a measurement property or may be different speakers.
  It cannot be told apart here.
- **The positives' true lag is unknown.** Every rule is scored accept/refuse
  only. A rule that accepts the wrong lag scores identically to one that
  accepts the right one.
- **Run 1's eight sessions are not eight independent samples** — same setup,
  minutes apart. The effective sample size for the positive class is closer to
  three conditions than to thirteen.
- **7 negatives**, all of them "one leg carries nothing related". The hard
  negative — two genuinely comparable arrivals, which is what session 2's
  position 1 looked like with `peak_lag` alternating between candidates — is
  **absent from this data entirely**. Both candidate rules would be tested by
  exactly that case, and neither has been.
- `0/7` and `12/13` are small-sample statements. They rank rules; they do not
  bound a false-accept rate.

## What would settle it

- Captures at more than one position, with **one speaker energised at a time**
  and the speaker state recorded as a session variable.
- A capture of the ambiguous case — two comparable arrivals — which is the only
  condition that separates "repeatability works" from "repeatability agrees
  with itself because the room is simple". **This is now constructible on
  demand.** The room has two speakers on the right and one at the back; the
  hard negative is "energise two of them and measure". Session 2's position 1
  produced it by accident, as `peak_lag` alternating between candidates, and it
  was read as noise. Build it deliberately: one speaker alone (the easy
  positive), then two together (the hard case), same position, same evening,
  speaker state recorded both times. Both candidate rules should be scored
  against that pair before either is implemented.
- Attempts spaced at least 2.5 s apart in at least one capture, so the
  repeatability rule can be scored on independent estimates directly rather
  than by subsampling a 1 Hz retry stream.
- The per-speaker geometry measurement (loopback for converter latency, tape
  measure per speaker), which turns accept/refuse scoring into correctness
  scoring by giving each lock an expected arrival time.
