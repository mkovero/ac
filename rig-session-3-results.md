# rig-session-3-results — 2026-08-04, 192.168.9.25

Executes `handoff-rig-session-3.md`. Rig: RME Babyface Pro, 96 kHz native both
directions, external master clock over ADAT. Build under test: `main` @
`4659b25` (#237 + #239 + #240 + #241), built on the rig into `~/target-rig3`
and installed after stopping the daemon.

**Drive level: −30 dBFS nominal pink** (≈ −25.8 dBFS instantaneous peak at
~4 dB crest), authorised by the operator for this session — the standing
acoustic exception above the −40 dBFS electrical ceiling. The daemon ran under
`HOME=/home/mui/rig2-home` with `drive_max_dbfs: -30.0`, so the clamp is
server-side and not merely in the request. Emission was stopped between every
session.

**Not merged into either previous results file**, as the handoff instructs:
the threshold definitions differ from session 2's and the speaker
configuration was unrecorded there.

---

## Setup as measured

| | |
|---|---|
| Source A | Genelec 1083 (prototype), **right**, fed from **playback_5** (ADAT), EQ off |
| Source B | Genelec 1083, **left**, fed from **playback_6** (ADAT) |
| Mic | Beyerdynamic measurement mic, **capture_1**, on axis to A, **1.000 m** tweeter→capsule, 90° capsule |
| Reference leg | **playback_2 (AN2) → capture_4 (IN4)**, electrical loopback |
| Config | `reference_channel: 3`, `reference_output_channel: 1`, `output_channel: 4` |
| Mic preamp | `numid=301` = **36** (session baseline), 48 V on, PAD off |
| Clock | `numid=320` = **0 (AutoSync)** — left alone, as required |
| Room | ~24–26 °C, **c = 346 m/s** (not 343) |

The mic was moved twice during the session, with the operator taping each
position. Distances are as given: A's are firm to the stated decimal, B's
carry a "~" and turn out to be the looser of the two (see the geometry table).

| position | A → capsule | B → capsule |
|---|---|---|
| 1 (Run 1, Run 2 as specified) | **1.000 m**, on axis | 3.2 m |
| 2 (equidistant) | ~2.0 m | ~2.0 m |
| 3 (asymmetric, the hard negative) | 1.8 m | ~2.5 m |
| 4 (Run 5, added later) | **3.000 m**, on axis | powered, silent |
| 5 (Run 6, added later) | **2.4 m**, off axis, capsule **28 cm from a wall** | powered, silent |

## Pre-flight

- Daemon stopped **before** installing. All three binaries
  (`ac`, `ac-daemon`, `ac-view`) copied by hand — `install.sh` does not ship
  `ac-view`.
- **All three verified by sha256** against `~/target-rig3/release/`. All three
  differed from what was installed beforehand, so the install was necessary
  and the previous session's binaries were not what `main` now builds:

  | binary | installed now |
  |---|---|
  | `ac` | `614f69de888e384a…` |
  | `ac-daemon` | `7687a7c38fe7c554…` |
  | `ac-view` | `d8126ed263864a7a…` |

- Clock left at `AutoSync`. Nothing in the TotalMix matrix was written.

---

## The zero-flight constant is 0, and it was measured in every session

Every session in this report carried **two pairs at once**:

| pair | meaning |
|---|---|
| `(3, 3)` | `capture_4` against itself — the electrical loopback correlated with itself, zero flight by construction |
| `(0, 3)` | `capture_1` (mic) against `capture_4` — the acoustic path |

`transfer_stream` takes `pairs=[[m,r], …]`, so both ran inside one session and
the constant is contemporaneous with the arrival it is subtracted from rather
than inferred from an earlier evening.

**The electrical pair locked at exactly 0 samples in every frame of every
session** — 2064 locked frames in Run 1 alone, `delay_samples` ∈ {0},
prominence 55–98 driven and ~300 silent. So the estimator and the JACK
buffering contribute **no constant**: playback and capture latency is
common-mode across the two legs and cancels, as
`rig-acoustic-layout` predicted. Any nonzero acoustic figure below is
external-converter latency plus flight, nothing else.

## Silent baseline

| | mic (`capture_1`) | reference (`capture_4`) |
|---|---|---|
| before, no emission | **−44.0 / −43.4 dBFS** (two sessions) | −94.8 dBFS |

The acoustic pair correctly **refused** on silence, prominence 4.5–6.8 against
a gate of 24, 11 attempts per 12 s session. Refusal on silence is the
estimator working.

---

## Run 1 — geometry, single source at 1.000 m: **8 / 8 lock, zero spread**

Source A alone (right 1083 on `playback_5`, EQ off), mic on axis at 1.000 m,
gain 36, −30 dBFS pink on channels `[1, 4]`, 8 fresh sessions, 15 s each.

| | |
|---|---|
| sessions locked | **8 / 8** |
| locked lag | **392 samples = 4.0833 ms**, identical in all 8 |
| spread | **0 samples** |
| per-session median prominence | 25.08 / 27.09 / 32.23 (min / median / max of the 8) |
| attempts to lock | 2 (one session took 4) |
| mic level | −34.5 dBFS median |
| reference level | −22.4 dBFS median |

This is the cleanest result the rig has produced. Every session agrees to the
sample, and every one clears the shipped gate of 24 — unlike session 2's
position 1, which locked once in twelve tries at what was described as the
same geometry.

### What the number means, and what it does not

Flight at 1.000 m and c = 346 m/s is 2.8902 ms. With the zero-flight constant
measured at 0, the residual is

```
converter constant = 4.0833 ms − 2.8902 ms = 1.1931 ms  (114.5 samples)
```

— the external ADAT converter and speaker electronics. **That figure is fitted
to the 1.000 m tape, so quoting "implied distance 1.000 m" back from it is
circular.** It becomes a real number only when a second, independent distance
is measured against it. That is what the rest of the session does.

**Session 2's inferred system latency of ~3.05 ms is wrong.** It predicted
~6 ms total at 1 m; the measurement is 4.08 ms. The inference was made against
an estimator that was itself under test, which is exactly the circularity this
run was written to retire.

### Expected arrival time per speaker — the deliverable

```
arrival(d) = 1.1931 ms + d / 346 m/s          (96 kHz: 114.5 + 277.4·d samples)
```

Checked against every other position measured tonight. Only the first row is
fitted; the rest are predictions made before the capture and compared after.

| source | tape | predicted lag | measured lag | implied distance | error |
|---|---|---|---|---|---|
| A, on axis | **1.000 m** (fitted) | — | 392 (4.0833 ms) | 1.000 m | — |
| B | 3.2 m | 1002 | **988** (10.2917 ms) | 3.148 m | **−5.2 cm** |
| A, equidistant position | ~2.0 m | 669 | **659** (6.8646 ms) | 1.962 m | **−3.8 cm** |
| B, equidistant position | ~2.0 m | 669 | **659** (6.8646 ms) | 1.962 m | **−3.8 cm** |
| A, asymmetric position | 1.8 m | 614 | **628** (6.5417 ms) | 1.851 m | **+5.1 cm** |
| B, asymmetric position | ~2.5 m | 808 | **762** (7.9375 ms) | 2.334 m | **−16.6 cm** |
| A, 3 m on axis | **3.000 m** | 947 | **938** (`peak_lag`, refused) | 2.968 m | **−3.2 cm** |

Six of seven predictions land within 5 cm of a tape figure the operator gave
to one decimal or as "roughly" — including the 3 m position, where the figure
comes from the peak the estimator *refused* to report. The one position where
the model does not describe the measurement at all is the near-wall one
(Run 6), and there the reason is that no candidate corresponds to the direct
arrival. **From here a lock is checkable against
geometry**, and no future session needs to compare itself against a previous
session's estimate.

The exception is worth stating precisely rather than averaging away. **B's
tape figures are internally inconsistent by more than the measurement is.**
Its three positions were tapped as 3.2 m, ~2.0 m and ~2.5 m; the measured lags
are 988, 659 and 762, which imply 3.148, 1.962 and 2.334 m. The measured
spacings (1.19 m and 0.37 m) disagree with the tape spacings (1.2 m and 0.5 m)
by 1 cm and 13 cm respectively. The 1 cm case is where the tape was given as a
firm number; the 13 cm case is where it was "~2.5" and B is the more off-axis
box. So the residual sits in B's tape and its off-axis path, not in the
estimator — every A position, on axis and taped, agrees to ≤5 cm.

Solving the two exact-ish points (A at 1.000 m, B at 3.2 m) for both unknowns
instead gives c = 354.4 m/s and a constant of 1.2612 ms. c that high would
need ~38 °C, so the residual is tape error of a few centimetres, not physics —
which is the expected outcome at these distances and is why the handoff says
temperature uncertainty is below the measurement's own resolution here.

---

## Run 2 — the hard negative

### As the handoff specified it, the case does not arise

A at 1.000 m and B at ~3.2 m, both driven (channels `[1, 4, 5]`), 8 fresh
sessions of 30 s:

| | A alone (Run 1) | A + B together |
|---|---|---|
| sessions locked | 8 / 8 | **8 / 8** |
| locked lag | 392 | **392**, spread 0 |
| per-session median prominence | 25.08 / 27.09 / 32.23 | 24.29 / 25.59 / 29.08 |
| mic level | −34.5 dBFS | −33.8 dBFS |

**Adding the second speaker changed the mic level by 0.7 dB and the answer not
at all.** At these distances A is ~6 dB up on B at the capsule, so summing
them produces one dominant arrival, not two comparable ones. This is a real
result about the geometry, not a null run — but it is not the ambiguous case
the handoff wants, so the mic was moved (below).

### B alone is the session's strongest result

Same evening, same position, B alone (channels `[1, 5]`), 8 sessions of 30 s.
B is ~3.2 m away and off axis, so the mic sees −39.3 dBFS against a −44 dBFS
silent floor:

| | |
|---|---|
| sessions locked | **1 / 8** |
| the one lock | **988 samples, 10.2917 ms**, prominence 24.07 |
| the seven refusals | per-session median prominence 12.73–18.89; best single frame 22.53 |
| `peak_lag` while refusing | **988** in six of seven sessions (1059 in one) |
| attempts per refusing session | 29 |

**The refusals are refusing the correct answer.** The lag the estimator
declines to accept at prominence 15.8 is the same lag it accepts at 24.07, and
that lag is the one geometry predicts for a 3.2 m tape. This is the evidence
the handoff asked for in its strongest form — a single named source, no second
speaker, nothing else to blame — and it says the gate of 24 is too high.

The one competing lag, 1059, is 71 samples later: 25.6 cm of extra path, a
reflection, and the only thing the estimator ever wavers between here.

### The same speaker, 1.2 m closer: 1/8 becomes 8/8

The mic was then moved to a point roughly equidistant from both sources
(~2.0 m each), and B was run again, alone, unchanged in every other respect:

| B alone | ~3.2 m | ~2.0 m |
|---|---|---|
| sessions locked | **1 / 8** | **8 / 8** |
| locked lag | 988 | 659, spread 0 |
| per-session median prominence | 12.73 / 15.92 / 24.07 | 25.45 / 27.05 / 30.15 |
| mic level | −39.3 dBFS | −38.3 dBFS |

**One dB of level, and the lock rate goes from one in eight to eight in
eight.** The gate is not tracking level — it is tracking direct-to-reverberant
ratio, which improves fast as the mic approaches the source. That is the same
conclusion session 2 reached from gain (20 dB of input gain moved prominence
by 0.4), seen from the other side: what gain cannot buy, 1.2 m of distance
can.

A at the same equidistant position gives the same lag and a nearly identical
level (−39.2 dBFS), which is the geometry working: equal distance means equal
arrival time. It also means **the equidistant position cannot produce the
ambiguous case** — two coincident arrivals are one peak, not two competing
ones. The mic was moved again for that.

### The ambiguous case, finally produced — and both rules accept it

Third position: **A at 1.8 m, B at ~2.5 m**. That is 2.9 dB of level
difference by inverse square and 134 samples (1.40 ms) of lag separation —
measured at the capsule as **1.1 dB** (A −38.2, B −39.1 dBFS), which is closer
than session 2's accidental case. Two comparable arrivals, distinct lags,
neither one the right answer.

| condition | sessions locked | locked lag | implied | per-session median prominence (min/med/max) |
|---|---|---|---|---|
| A alone | 6 / 6 | 628 | 1.851 m | 24.08 / 26.75 / 29.59 |
| B alone | 6 / 6 | 762 | 2.334 m | 24.00 / 25.77 / 31.16 |
| **A + B together** | **8 / 8** | **628** | 1.851 m | **24.74 / 28.07 / 31.86** |

**The estimator locks on A, every session, and it is more confident doing it
than in either single-source case.** Median prominence rises from 26.8 and
25.8 to 28.1.

The full evidence from one A+B session shows why:

| lag | value | rel. peak | what it is |
|---|---|---|---|
| 628 | 0.1968 | −2.70 dB | **A's direct arrival — accepted** |
| 701 | 0.1762 | −3.66 dB | |
| 706 | 0.1739 | −3.77 dB | |
| 757 | 0.1626 | −4.36 dB | **B's direct arrival** (762 when measured alone) |
| 770 | 0.1488 | −5.13 dB | |
| 843 | 0.2685 | 0.00 dB | strongest peak — a reflection, and `peak_lag` |

Six candidates in this 550–950 sample window clear `DIRECT_PEAK_FRACTION`'s
6 dB threshold — two of them real sources, the rest reverberation — and 32
clear it across the full lag range. The earliest-peak rule takes the earliest,
which is A, and reports prominence 30.3, the highest number of the evening.

**This is what Run 2 was written to decide, and the answer is that prominence
cannot decide it.** Prominence measures peak-against-median contrast, not
uniqueness. Adding a second comparable source *raises* it, because the room
now has more correlated energy in it and the median does not move as fast as
the peak. So:

- **Neither candidate gate rule refuses the ambiguous case.** Both accept, at
  higher confidence than the unambiguous one. Gate tuning cannot fix this,
  because the quantity being tuned moves the wrong way.
- **The lock it returns is defensible but silently partial.** "Earliest
  arrival within 6 dB" is the nearest source, which is the answer a live-sound
  engineer usually wants — #227's rule is doing exactly its job. What the
  operator never learns is that a second source of comparable strength arrived
  1.4 ms later and was discarded.
- The choice between the two candidate rules therefore **cannot be made on
  Run 2 evidence**, and must be made on the single-source data, where they do
  differ.

The obvious alternative statistic — *how many* candidates sit inside the 6 dB
window — was tested and **does not work as shipped**:

| condition | candidates within 6 dB of the peak |
|---|---|
| A alone, 1.8 m | 23 |
| A + B, 1.8 / 2.5 m | **32** |

It moves the right way, but 32 is `MAX_CANDIDATES`. **The count is censored at
the cap**, so it cannot be read as a measure of anything once the room is
reverberant enough to fill the list — and both figures are already so large
that no threshold separates "two sources" from "one source in a live room".
Detecting a stereo-summed measurement needs structure the candidate list does
not currently carry (arrival clusters, not peak counts). Recorded as a dead
end so the next session does not re-derive it.

---

## Run 5 — A at 3.000 m, on axis: the gate refuses a 3 cm answer, eight times out of eight

Added after the operator returned, because everything above was measured at
≤2.5 m and session 2's two wrong locks happened at 3 m. A alone, on axis,
taped 3.000 m, B powered but silent, 8 sessions × 30 s.

| | |
|---|---|
| sessions locked | **0 / 8** |
| `peak_lag` while refusing | **938** in six sessions, **1031** in two |
| 938 implies | **2.968 m** — 3.2 cm from tape |
| 1031 implies | 3.303 m — a reflection 33.5 cm further out |
| per-session median prominence | 13.29 – 19.12 |
| best single frame | 23.77 — never reaches 24 |
| attempts per session | 29 |
| mic level | −41.2 dBFS, ~7 dB over the room floor |

**The estimator computes the right answer 29 times a session and reports none
of them.** This is the same finding as B alone at 3.2 m, at a taped on-axis
position with a single source, and it is the strongest form the argument can
take: at the distance live sound actually works at, prominence never reaches
24 even though the arrival is resolved to 3 cm.

### Against session 2's 3 m result

| | session 2, ~3 m | tonight, 3.000 m |
|---|---|---|
| sessions locked | 7 / 7 | 0 / 8 |
| wrong locks | **2** (14.00 ms, 18.43 ms) | **0** |
| correct arrival present in evidence | absent from its own candidate list | `peak_lag`, 6/8 sessions |

The wrong locks are gone and the accepted lag is no longer missing from its
own evidence, which is what #232/#237 were for. **But this is not a clean
controlled comparison** — the prominence definition changed between the two
sessions (thresholds are now measured against the strongest *causal* peak),
session 2's speaker configuration was never recorded, and the two "3 m"
positions are not the same spot in the room. Read it as consistent with the
fixes working, not as proof of them.

Repeatability at this position, scored both ways: p90 94 samples (0.98 ms,
0.34 m) on every attempt and on independent windows alike — and the 94 is the
33 cm reflection, not scatter.

---

## Run 6 — near a wall, off axis: the case where refusing is right

Run C position 5, also added after the operator returned. **A taped at 2.4 m,
capsule 28 cm from a wall, off axis at (operator's words) "a really nasty
angle."** A alone, 8 sessions × 30 s. Predicted direct arrival: 780 samples.

| | |
|---|---|
| sessions locked | **1 / 8** |
| the one lock | **925 samples, 9.6354 ms** → 2.921 m, at prominence **24.15** |
| tape says | 2.4 m → **780 samples** |
| the lock is therefore | **52 cm long — the first wrong lock of the session** |
| `peak_lag` in the seven refusals | 990–1006 → 3.17 m, **77 cm long** |
| per-session median prominence | 14.48 – 18.85 |

**Nothing at this position points at the direct arrival.** A full candidate
capture shows the earliest candidate anywhere near the arrival zone is 911
(−7.1 dB), and the whole 6 dB window sits at 987–1020:

| lag | rel. peak | implied |
|---|---|---|
| 911 | −7.10 dB | 2.871 m |
| 925 | −5.53 dB | 2.921 m ← what the one locking session accepted |
| 946 | −3.83 dB | 2.997 m |
| 987 | −0.14 dB | 3.145 m |
| **997** | **0.00 dB** | **3.173 m** ← `peak_lag` |
| 1020 | −0.49 dB | 3.256 m |

780 does not appear in the list at all. The list is 33 entries with the rank
cap binding, so this is "≥12 dB down, or crowded out by 32 stronger peaks" —
not proof the direct arrival is absent from the correlation, but proof it is
**not recoverable by lowering the gate**: everything inside
`DIRECT_PEAK_FRACTION`'s window at this position is half a metre or more long,
so the earliest-peak rule has nothing right to choose.

**So this is the position where refusing is correct**, and the shipped gate
refuses it seven times out of eight. The eighth is the failure: prominence
24.15, barely over the line, and a lock 52 cm from the truth.

### The warning this carries for the repeatability rule

At this position successive estimates agree to **p90 = 9 samples (3.2 cm)** —
tighter than anywhere else tonight, on independent windows as well as
overlapping ones. **The wrong answer is the most repeatable answer in the
session.**

An agreement rule of ±120 samples, or ±72, or ±9, accepts it. Repeatability
measures whether the room is stable, not whether the arrival is the direct
one. Only geometry separates them — which is what Run 1 now makes possible,
and it is the argument for keeping an expected-arrival figure per speaker
rather than leaning on agreement between estimates.

---

## Run 3 — independent attempts

Scored on B alone, which refuses for 30 s and therefore produces 29 attempts
per session instead of the two a locking session gives. `independent()` keeps
the first frame of each attempt and then drops any attempt less than one ring
(2.5 s) after the one before, so the surviving estimates share no samples.

| scoring | pairs | median \|Δlag\| | p90 \|Δlag\| |
|---|---|---|---|
| every attempt (~1.0 s apart, ~60% overlap) | 202 | 0 | **72** |
| independent windows (≥2.5 s apart) | 65 | 0 | **72** |

**The overlap was not inflating the agreement.** Scored honestly on
non-overlapping windows the spread is identical: p90 of 72 samples = 0.75 ms =
0.26 m. The offline scoring's estimate that the repeatability rule would need
±120 samples to hold its hit rate is conservative — ±72 covers p90 here, and
the deviations are not noise but the single 71-sample reflection above.

Silence is the control, and it separates cleanly: on the silent baseline the
same scoring gives a median \|Δlag\| of 15256 samples and p90 of 34797
(±125 m). A ±120-sample agreement rule cannot be fooled by a dead room.

---

## Run 4 — `CHECK ROUTING`: confirmed unreachable, as predicted

Two conditions, 20 s each, scored against `fault.rs` at `4659b25`
(`COHERENCE_THRESHOLD` 0.5, `COHERENCE_ALIVE_FRACTION` 0.10):

| condition | frames | locked | ladder frames | alive fraction | `coherence_dead` |
|---|---|---|---|---|---|
| unrelated legs (pink on `playback_2` only, speakers silent) | 352 | 0 | **0** | — | never evaluated |
| healthy (ref + speaker A) | 352 | 295 | 293 | 0.836 / 0.873 / 0.909 | 0 frames |

**There are no ladder columns at all while the legs are unrelated** — the
daemon builds the ladder only after a lock, and unrelated legs refuse. So
`CHECK ROUTING` has nothing to read, exactly as the handoff says, and this is
the same root cause as finding 3 rather than a new defect. Confirmed in two
minutes and not investigated further.

The healthy condition is the useful half: 504 columns with 87% alive means the
0.10 fraction has enormous margin on a good measurement, so the rule will not
misfire once it can be reached.

---

## #242 — the delay readout over a live trace

`ac-view --transfer` on the rig's own X session (NVIDIA, `DISPLAY=:0`,
`XAUTHORITY=/home/mui/.Xauthority` — the isolated `HOME` hides the X cookie,
which is what made the first two launches fail), A alone at 1.000 m, 96 kHz,
screenshots `~/s3-242-magnitude{,-2}.png`.

**Grey-on-ember is not the problem.** The readout sits at the top of the plot
area at the +20 dB gridline; the trace at this position runs 0 to −20 dB, well
below it. Contrast is white-on-black and the text is entirely legible with the
trace live and settled.

**What it does collide with is the topmost y-axis label.** `4.08 ms (1.40 m)`
is drawn straight through the `20` tick label — the digits overlap. At this
window size the 32 px move traded a trace collision for an axis-label
collision. It is legible enough to read, so this is not a hold on #242, but
the label overlap should be recorded.

**The metres figure is wrong for the operator, and by a lot.** The display
reads `1.40 m` with the mic tape-measured at **1.000 m**. Two things compound:
the converter constant of 1.1931 ms is not subtracted (+0.41 m), and the
conversion uses 343 m/s rather than the room's 346 (+0.4%). An operator who
tapes a metre and reads 1.40 m on screen has no way to tell which part is the
instrument's latency. This is worth its own issue: either subtract a measured
electrical constant before converting to distance, or stop showing metres.

---

## Recording — every capture

−30 dBFS nominal pink throughout, mic preamp `numid=301` = 36, 96 kHz, one
fresh session per point, emission stopped between sessions, both pairs
`(3,3)` and `(0,3)` in every session.

| run | speakers on | mic position | sessions × s | locked | lag | mic dBFS |
|---|---|---|---|---|---|---|
| `baseline-before` | **none** | 1.000 m from A | 2 × 12 | 0/2 (correct) | — | −44.0 |
| `run1-1m-spkA` | **A only** | 1.000 m from A | 8 × 15 | 8/8 | 392 | −34.5 |
| `probe-spkB` | **B only** | 1.000 m from A, ~3.2 m from B | 1 × 10 | 0/1 | (988) | −39.6 |
| `run2-AB` | **A + B** | 1.000 / ~3.2 m | 8 × 30 | 8/8 | 392 | −33.8 |
| `run2-B-alone` | **B only** | ~3.2 m | 8 × 30 | 1/8 | 988 | −39.3 |
| `runC-2m-A` | **A only** | ~2.0 m equidistant | 8 × 15 | 8/8 | 659 | −39.3 |
| `runC-2m-B` | **B only** | ~2.0 m equidistant | 8 × 30 | 8/8 | 659 | −38.3 |
| `runD-A` | **A only** | 1.8 m from A | 6 × 15 | 6/6 | 628 | −38.2 |
| `runD-B` | **B only** | ~2.5 m from B | 6 × 30 | 6/6 | 762 | −39.1 |
| `runD-AB` | **A + B** | 1.8 / ~2.5 m | 8 × 30 | 8/8 | 628 | −36.3 |
| `run4` | ref leg only, then A | 1.8 m | 2 × 20 | 0, then 295/352 | — | — |
| `baseline-after` | **none** | 1.8 / ~2.5 m | 2 × 12 | 0/2 (correct) | — | −48.1 |
| `runE-3m-A` | **A only** | 3.000 m on axis | 8 × 30 | **0/8** | (938) | −41.2 |
| `runF-wall` | **A only** | 2.4 m, 28 cm off wall, off axis | 8 × 30 | **1/8, wrong** | 925 | −39.9 |
| `baseline-final` | **none** | wall position | 2 × 12 | 0/2 (correct) | — | −45.5 |

Reference leg (`capture_4`) read −22.3 to −22.4 dBFS in every driven session
and −94.8 dBFS silent. `delay_locked`, `delay_attempts`, and from
`delay_evidence` the `prominence`, `peak_lag`, `noncausal_peak_lag` and
`negative_lag_median`, are in `audit/rig-session-3/*.json.gz`, one record per
frame; two captures (`runD-A-evidence.pkl.gz`, `runD-AB-evidence.pkl.gz`) keep
the full candidate lists as well. `s3.py` runs a set, `analyse3.py` scores one
(`python3 analyse3.py <tag> 1.1931`).

That is **88 transfer sessions** in total, counting the three evidence
captures and Run 4's two conditions.

**Silent baseline moved 4.4 dB across the evening** (−44.0 dBFS at the start,
−48.1 dBFS at the end; −45.5 dBFS at the wall position, where the boundary
raises it again). Smaller than
session 2's 10 dB, but still large enough that no level comparison should
cross runs without its own contemporaneous baseline. Both baselines refused
correctly, at per-frame prominence 2.5–6.8.

## Cross-cutting notes

- **Capture discontinuity warnings, continuously.** The daemon log fills with
  `transfer_stream: capture discontinuity — 1024 samples (0.01 s) discarded by
  the pre-wait ring clear; the analysis window is not contiguous` for the whole
  life of every session. It did not visibly disturb any result here — the
  locks are sample-identical across sessions — but it is one period per
  occurrence and it is constant. See `handoff-capture-contiguity.md`.
- **Frame rate ~17.5/s**, matching session 2's ~18/s and not `ZMQ.md`'s
  documented one frame per iteration ≈ 2.5 s. Unchanged and still unexplained.
- **`transfer_stream` accepts `pairs`, and that is what made this session
  possible.** Carrying the electrical loopback alongside the acoustic path in
  one session turns the converter constant from an assumption into a
  measurement. Any future rig session should do the same.
- **The isolated `HOME` hides the X cookie.** `ac-view` under
  `HOME=/home/mui/rig2-home` dies with `XOpenDisplayFailed` unless
  `XAUTHORITY=/home/mui/.Xauthority` is passed explicitly. Two launches were
  lost to this before the log was read.
- `~/.config/ac/config.json` was again left alone, and is still wrong for this
  rig (`reference_channel: 2` → the silent `capture_3`).

## Rig state left behind

- **No emission in progress.** All workers stopped, `ac-view` closed,
  `status` clean.
- Clock `numid=320` = **0 (AutoSync)**. Mic preamp `numid=301` = **36**, found
  at 36 and left at 36. 48 V on, PAD off. No mixer route written.
- `/usr/local/bin/{ac,ac-daemon,ac-view}` = a build of `4659b25`, sha256
  verified.
- Daemon running on the default ports under `HOME=/home/mui/rig2-home`
  (`drive_max_dbfs: -30.0`, `reference_channel: 3`,
  `reference_output_channel: 1`).
- Build directory `~/target-rig3` (447 MB). Screenshots
  `~/s3-242-magnitude{,-2}.png`.
- **Mic left at the near-wall position** — 2.4 m from A, 28 cm off the wall,
  off axis. Both 1083s powered, neither driven.

## What this session says should happen next

1. **Set the gate from this data — and it now has both sides.** The evidence
   is no longer one-directional:

   | position | what prominence should do | what it did |
   |---|---|---|
   | A, 3.000 m on axis | **accept** — `peak_lag` is right to 3 cm | refused 8/8, never reached 24 |
   | B, 3.2 m | **accept** — `peak_lag` is the lag it locks to when it locks | refused 7/8 |
   | A, 2.4 m near a wall | **refuse** — nothing in the 6 dB window is within 0.5 m | refused 7/8, **accepted once at 24.15, 52 cm wrong** |

   So 24 is too high for the clean distant case and, at the same time, is
   *only just* high enough to keep the near-wall case out — one session got
   past it. A single prominence threshold is being asked to separate two
   situations that differ in where the peaks are, not in how prominent they
   are. Any new number must be checked against both rows, not just the first.

   Nothing tonight locked wrongly except that one near-wall session: 87 of 88
   sessions either locked to a geometrically correct lag or refused.
2. **Do not expect a gate to refuse the stereo-summed case.** It accepts, at
   *higher* prominence than the single-source case. If refusing that case is
   wanted, it needs a different statistic, and the candidate count is not it
   (censored at `MAX_CANDIDATES`).
3. **Subtract the electrical constant before showing metres**, or stop showing
   metres. `1.40 m` on screen at a taped 1.000 m is the instrument reporting
   its own latency as distance. The constant is measurable in-session — this
   report measured it in every session at zero cost.
4. **#242 can ship**; log the axis-label overlap separately.
5. **#226's automatic-refresh half is not needed for lock stability.**
   Eighty-eight sessions produced zero unstable locks; every repeat at a fixed
   position agreed to the sample, including the one wrong lock. What #226 is
   still needed for is the stimulus-before-session ordering, which remains a
   real trap.
6. **Do not use repeatability as a correctness test.** At the near-wall
   position successive independent estimates agreed to **9 samples (3.2 cm)**
   — the tightest agreement of the session — around an answer 52 cm from the
   truth. Agreement measures whether the room is stable. Only the geometry
   figure from Run 1 separates a right answer from a repeatable wrong one.
