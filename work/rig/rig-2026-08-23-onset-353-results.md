# rig-2026-08-23-onset-353-results — 192.168.9.25

Characterisation session for **#353**, run on PR **#352**'s build. Acoustic —
speaker A on `system:playback_5` through the external converter, microphone on
`system:capture_1`.

**Operator:** Markus, on site. **Status: complete**, both positions plus a
drive ladder. Raw logs in `audit/rig-353-2026-08-23/`.

**This session was not run as an AC5 acceptance gate.** #352's AC5 already
failed on 2026-08-22 and the blocking defect (#353) has no fix, so re-scoring
the same binary could only reproduce that verdict. It was run to answer the
question #353's design needs answered: *what does the onset threshold actually
couple to?* The registered AC5 rule was executed along the way because it costs
nothing extra once both positions are captured, and it had never been run.

## Build under test

**No build was made for this session.** The binaries already on the rig from
2026-08-22 were re-hashed and matched the record exactly, so this is the same
code as the failing session and the audio stack is the only variable.

| binary | sha256 | ref |
|---|---|---|
| `ac-issue-346` | `82410a74206c23a32b1c8c070d233750087e4a40cc40f4403d286db12fe97c24` | `b1ac5a7` (PR #352 HEAD) |
| `ac-daemon-issue-346` | `098a7f5ee0db213725d7efd76e3cbbb8f3f00181346a5ecd3378cedee92f1a40` | `b1ac5a7` |

`b1ac5a7` is still the tip of `issue-346`; nothing has been committed to the
branch since the 2026-08-22 failure.

Daemon run as `HOME=/home/mui/rig2-home`, `--ctrl-port 25910 --data-port 25911`.
A pre-existing `bin-350` daemon owned 5556/5557 throughout and was left alone;
the CLI was pointed at the test daemon via `AC_CTRL_PORT` / `AC_DATA_PORT`.

## What changed since 2026-08-22 — read this before comparing numbers

The audio stack was replaced between the two sessions. **pipewire-jack is out**;
`jackd` drives ALSA directly, **period 64 / 2 periods** (was 1024 / 4096), and
JACK ports are now `system:playback_N` / `system:capture_N` (was
`Babyface Pro Pro:...`). Confirmed this session by `jack_lsp` and
`jack_bufsize`.

Consequence: **every absolute delay in the 2026-08-22 file is superseded.** The
arrival at 1.000 m went from ~4604 samples to ~851, because the old figure
carried PipeWire's buffering. Differences and increments from that session
remain comparable; absolutes do not.

## Drive level

**−30 dBFS ceiling, authorised by the operator for this session**, passed
explicitly on every command. The drive ladder ran at −30 / −36 / −42 dBFS, all
at or below that ceiling.

**Enforcement was request-side only, and that remains a deviation from the
interlock** — `drive_max_dbfs: -30.0` is in the running daemon's config but
`plot_ir` has no clamp (#360), so the argument is the level that reaches the
speaker. Recorded as agreed with the operator, not claimed as a clamped run.

Per-run consent was taken before each emitting block: B1a (IR at 1 m), B1b
(pink at 1 m), the transfer repeats, B2 (3 m), and the ladder.

**One parse failure, no emission.** The first invocation used `16384window`;
the token is `16384win`, so the parser printed usage and nothing was driven.
Noted because it is the same class of mistake as 2026-08-22's `ac plot` with no
arguments — but the opposite outcome: a *bad token* fails safe, a *missing*
argument list does not. That asymmetry is the real hazard, and it is #360's.

## Wiring, clock, gain — confirmed this session

- Stimulus: `system:playback_5` (ADAT) → external converter → speaker A.
- Measurement: microphone → `system:capture_1`.
- Reference: electrical loopback `system:playback_2` → `system:capture_4`,
  patched throughout, never re-patched.
- Clock `AutoSync` (`numid=320` = 0), read at session start and again at end.
- Mic preamp `numid=301` = 36, the session baseline, unchanged throughout.

## Configuration

96 kHz, **period 64**. Sweep: 50 Hz–16 kHz, 2.0 s, 5 harmonics, 16384-sample
window, 0.8 s tail. Twelve captures per position, geometry untouched within a
position.

## Temperature — not measured

No thermometer. Operator states 25–27 °C. Every scoring number below is c-free,
so this does not enter; it bounds only the sanity checks, which are labelled as
such.

## Distance — hand-taped, ±5 cm

Operator moved the capsule 1.000 m → 3.000 m mid-session and re-read the tape as
3.000 m. Per the standing error budget the tape is ±5 cm and dominates every
other term by more than 10×. **No criterion below is scored against it.**

---

## Run 1 — 1.000 m, 12 IR captures

| run | arrival | peak | SNR dB | | run | arrival | peak | SNR dB |
|---|---|---|---|---|---|---|---|---|
| 1 | 818 | 9152 | 26.1 | | 7 | 850 | 9153 | 23.4 |
| 2 | 858 | 9153 | 22.2 | | 8 | 853 | 9153 | 25.1 |
| 3 | 854 | 9153 | 23.1 | | 9 | 854 | 9154 | 23.2 |
| 4 | 857 | 9155 | 23.6 | | 10 | 853 | 9153 | 23.8 |
| 5 | 859 | 9156 | 22.4 | | 11 | 853 | 9154 | 23.9 |
| 6 | 853 | 9153 | 22.5 | | 12 | 851 | 9153 | 22.9 |

`arrival` is re gate centre; `peak` is an absolute IR index, so
`peak − 8192` is the peak on the same axis as `arrival`.

- onset mean **851.08**, sd **10.76**, range 41 samples (148 mm)
- peak mean **9153.50**, sd **1.09**, range 4 samples (14 mm)
- pre-impulse SNR 22.2–26.1 dB, mean 23.52
- **r(arrival, SNR) = −0.765**
- onset lands **110.4 samples** before the peak on average
- rule reported every capture: `onset: backward threshold from floor, no causal bound`

**The floor coupling reproduces.** 2026-08-22 measured r = −0.767 at this
position under pipewire-jack; this session gets −0.765 under jackd-direct. Same
code, different stack, same number — the coupling is a property of
`estimate_onset`, not of the audio graph.

**Caveat that belongs with that r.** It is leveraged by run 1 (SNR 26.1,
arrival 818, the only point far from the cluster). Drop it and r = −0.400 with
sd 2.81. The correlation is real and reproducible in sign, but its magnitude
here rests on one capture, and the *slope* differs from 2026-08-22 by 3×
(−7.27 vs −21 samples per dB). Do not carry the slope across sessions.

## Run 2 — 1.000 m, transfer_stream, 3 independent sessions

`pairs=[[0,3],[3,3]]`, standalone `generate_pink` on `[1,4]` started before each
session so the correlation rings never fill against silence (#226). One fresh
session per lock — the per-pair delay is estimated once at warmup and cached, so
repeats within a session measure nothing.

| pair | delay | locked |
|---|---|---|
| (0,3) acoustic | **392 samples**, all three sessions | 100% |
| (3,3) loopback vs itself | **0 samples**, all three sessions | 100% |

**Pair (3,3) reads exactly 0 in every frame of every session.** That is the
standing proof that playback and capture buffering are common-mode and cancel,
and it still holds on the jackd-direct stack.

## Run 3 — 3.000 m, 12 IR captures

| run | arrival | peak | SNR dB | | run | arrival | peak | SNR dB |
|---|---|---|---|---|---|---|---|---|
| 1 | 1424 | 9712 | 22.7 | | 7 | 1428 | 9716 | 22.6 |
| 2 | 1423 | 9710 | 23.0 | | 8 | 1421 | 9710 | 24.6 |
| 3 | 1432 | 9710 | 22.7 | | 9 | 1426 | 9709 | 22.4 |
| 4 | 1427 | 9710 | 21.9 | | 10 | 1432 | 9711 | 20.3 |
| 5 | 1429 | 9711 | 21.3 | | 11 | 1425 | 9711 | 25.6 |
| 6 | 1425 | 9710 | 22.8 | | 12 | 1429 | 9711 | 22.3 |

- onset mean **1426.75**, sd **3.41**, range 11 samples (40 mm)
- peak mean **9710.92**, sd **1.78**, range 7 samples (25 mm)
- pre-impulse SNR 20.3–25.6 dB, mean 22.68
- **r(arrival, SNR) = −0.660**
- onset lands **92.2 samples** before the peak

## Run 4 — 3.000 m, transfer_stream, 3 independent sessions

| pair | delay | locked |
|---|---|---|
| (0,3) acoustic | **942 samples**, all three sessions | 100% |
| (3,3) loopback | **0 samples**, all three sessions | 100% |

---

## The registered AC5 rule, executed

`work/rig/rig-test-plan.md:399` converts #346 AC5 into a c-free form, because
the tape cannot certify a millimetre criterion here:

> **Pass: |Δt_onset − Δt_transfer_stream| ≤ 1.3 samples**, each Δt being that
> estimator's own increment between the two taped positions.

Both estimators ran at the same two positions in the same session, so geometry,
temperature and tape are common-mode and cancel exactly.

| quantity | increment | as distance | vs transfer_stream |
|---|---|---|---|
| `transfer_stream` | **550.00** smp (sd 0, 3 fresh locks each end) | 1.9884 m | — reference |
| IR peak | 557.42 smp (se 0.60) | 2.0152 m | 7.42 smp / 26.8 mm |
| **onset (PR #352)** | **575.67** smp (se 3.26) | 2.0812 m | **25.67 smp / 92.8 mm** |

### The criterion has a provenance problem — read this before quoting a multiple

The registered 1.3 samples is "4.7 mm over 2.000 m" converted. **4.7 mm was
`transfer_stream` agreeing with the *tape* in one session** — a single draw
inside a ±5 cm hand-measured distance, which the operator has since stated
plainly is the realistic floor. So a c-free criterion inherited its magnitude
from a tape comparison that could not have resolved it. Quoting "20× the
criterion" lends that number an authority it has not got.

Three bars, one measurement:

| bar | provenance | 25.67 smp / 92.8 mm against it |
|---|---|---|
| 1.3 smp (4.7 mm) | registered; inherited from a lucky tape draw | 20× over |
| 13.8 smp (5 cm) | the rig's physical-measurement floor — though it is a *tape* bar applied to a tape-free quantity, so it is conservative here | **1.9× over** |
| 3.26 smp | the onset's own standard error on this increment, measured this session | **7.9σ over** |

**The third bar depends on nothing external.** Two estimators ran on identical
captures and disagree by 7.9× the noisier one's own scatter, while
`transfer_stream` re-locked in three separate sessions at each position with
zero spread. That is the estimators disagreeing, not a resolution limit — and it
is the only form of the statement that survives both the tape's ±5 cm and the
criterion's weak provenance.

**Verdict: the onset's increment is outside every bar on the table, and the bar
itself needs re-deriving.** Reported as the number plus its provenance rather
than as a bare FAIL, because restating an acceptance criterion off one session
is exactly what went wrong on #243 AC7. The bar is the operator's to set.

The peak is outside the registered rule too, at 7.42 samples — but it is **3.5×
closer** to `transfer_stream` than the onset is, and it sits inside the 5 cm
floor while the onset does not. Same ordering 2026-08-22 found: on AC5's own
metric, at this rig's SNR, the estimator #352 introduces is the worse-behaved of
the two.

**Sanity check, not a criterion.** Back-solving c from `transfer_stream`'s
increment against a taped 2.000 m gives 349.1 m/s → 29.4 °C, above the stated
25–27. The tape is ±5 cm and 23 mm of tape error produces exactly this, so the
back-solve diagnoses nothing here. Recorded because it is free, not because it
decides anything.

---

## The finding this session was run for

**2026-08-22 attributed the AC5 failure to distance-dependent pre-impulse SNR.
That attribution does not survive this session.**

That session saw pre-impulse SNR fall 2.52 dB from 1 m to 3 m and used the
within-position slope to predict the increment error. Here the SNR fell only
**0.83 dB** — and the onset still moved **18.2 samples** closer to the peak.

| | 1.000 m | 3.000 m | change |
|---|---|---|---|
| onset before peak | 110.4 smp | 92.2 smp | **−18.2** |
| pre-impulse SNR | 23.52 dB | 22.68 dB | −0.83 |
| r(arrival, SNR) within position | −0.765 | −0.660 | both strong |
| **r pooled, SNR centred per position** | | | **−0.016** |

Within a position the coupling is strong and reproducible. Across positions,
with the position mean removed, it vanishes. **Pre-impulse SNR does not predict
the between-position bias.**

### The drive ladder separates the two terms

18 further captures at 3.000 m, geometry fixed, drive stepped −30 / −36 / −42
dBFS — so SNR varies while distance does not.

First, a result that is not about the onset at all:

| level | peak index range | pre-impulse SNR |
|---|---|---|
| −30 dBFS | 7 smp | 20.5–26.3 dB |
| −36 dBFS | 8 smp | 14.5–24.1 dB |
| −42 dBFS | **6205 smp** | 8.3–16.5 dB |

**At −42 dBFS the deconvolution fails outright** — the peak itself lands on
noise, 5 of 6 captures scattered across 6205 samples. Those captures are not an
onset measurement and are excluded below. `plot_ir` reports them with no
indication that the result is garbage beyond the ISO tail-decay warning, which
also fires on perfectly good captures. Worth an issue of its own.

On the 13 captures whose peak stayed in the stable cluster:

| level | SNR | onset before peak |
|---|---|---|
| −36 | 14.5 | 51 |
| −36 | 15.8 | 68 |
| −36 | 15.9 | 60 |
| −42 | 16.5 | 74 |
| −36 | 17.2 | 71 |
| −36 | 17.8 | 79 |
| −30 | 20.5 | 86 |
| −30 | 20.7 | 88 |
| −30 | 21.6 | 91 |
| −30 | 21.9 | 90 |
| −30 | 23.7 | 90 |
| −36 | 24.1 | 94 |
| −30 | 26.3 | 93 |

**r = +0.910, n = 13. Slope +3.38 samples per dB, at fixed geometry.**

The points collapse onto **one curve regardless of drive level** — the −36 dBFS
capture at SNR 24.1 gives 94 samples, indistinguishable from the −30 dBFS
captures at the same SNR. So the governing variable is measured SNR, not the
level commanded.

### Decomposition

| term | size |
|---|---|
| observed onset shift, 1 m → 3 m | −18.2 smp |
| predicted from pre-impulse SNR (0.83 dB × 3.38 smp/dB) | −2.8 smp |
| **residual, unexplained by pre-impulse SNR** | **−15.4 smp** |

**There are two terms, not one.** One tracks pre-impulse SNR and is what #353
describes. The other is distance-dependent, roughly 5× larger over this move,
and is invisible to pre-impulse SNR.

The mechanism consistent with both: the backward search thresholds against
`floor_rms`, a *pre-impulse* statistic, but what it must actually discriminate
is the wavefront's leading skirt against everything else present *at that point
in the IR*. Moving from 1 m to 3 m drops the direct-to-reverberant ratio while
leaving the pre-impulse noise floor nearly untouched — the room contribution
arrives *after* the pre-impulse window, so `floor_rms` never sees it. The
threshold stays where the pre-impulse floor puts it while the signal it is meant
to find gets relatively weaker, and the search stops later.

**What this means for #353.** The issue is filed as a guard band sized off
`window_len` rather than lobe width. That is real and this session's
within-position correlation confirms it. But a fix that only re-sizes the guard
band against the pre-impulse floor addresses the −2.8 sample term and leaves the
−15.4 sample term standing. **#353's design needs to decide what the threshold
is taken relative to, not merely how wide the band is.** A pre-impulse statistic
is structurally unable to see the term that dominates.

---

## Verdicts

- **#346 AC5 — the onset's increment is 25.67 samples (92.8 mm) from
  `transfer_stream`'s on the same captures**, by the registered c-free rule,
  executed for the first time. That is outside the registered 1.3 samples, and
  also outside the 5 cm physical floor and the onset's own 3.26-sample standard
  error. **But the registered bar's magnitude came from a lucky tape draw and
  needs re-deriving** — see the provenance section. Reported as a number with
  its bar in question, not as a bare FAIL. #352 should not merge on AC5 as it
  stands; whether the bar or the estimator moves is the operator's and
  architect's call.
- **#353 — confirmed and re-scoped.** The floor coupling is real,
  stack-independent (r = −0.765 vs −0.767) and reproduces exactly. But it is
  the smaller of two terms, and the issue as written does not cover the larger
  one.
- **#359 — the one-period jump did not occur.** 24 captures, peak ranges 4 and 7
  samples, no cluster structure of any kind. 2026-08-22 saw it in 10 of 24 at
  period 1024. At that rate, 0 in 24 has p ≈ 4×10⁻⁶. The jump came from the
  pipewire-jack layer, now removed — it did not merely scale down with the
  period. **#359 should be re-tested before any work is done on it**; the defect
  it describes may no longer be reachable on this stack.

## Confounds

- **Temperature not measured.** Does not touch any scored number — all are
  c-free — but it does bound the back-solve, which is reported as a sanity check
  only.
- **Tape is ±5 cm.** No criterion here is scored against it.
- **Pre-impulse SNR 20.3–26.3 dB**, room-noise-limited at the capsule per
  2026-08-18, so gain cannot improve it. Whether either coupling term persists
  at materially higher SNR remains **unresolved and unanswerable on this rig.**
- **The 1 m r is leveraged by one capture.** Stated with the number above.
- **`transfer_stream`'s sd = 0** comes from three fresh locks, not from frames
  within one session. Three is a small n for a scatter estimate; it establishes
  that the lock is reproducible, not that its variance is zero.

## Issues filed from this session

- **#376** — `plot_ir` reports a failed deconvolution as a normal result. At −42 dBFS
  the peak scattered over 6205 samples with no error, no warning specific to the
  failure, and a plausible-looking arrival printed each time.
- **#375** — AC5's bar was derived by converting a lucky tape draw into samples.
- **#359 needs re-testing before work**, per the verdict above. Posted there.

## Rig state left behind

**No emission in progress. Test daemon stopped**; the pre-existing `bin-350`
daemon on 5556/5557 was never touched and is still running. Clock `AutoSync`,
verified after the last run. Mic preamp back at baseline 36. **Mic taped at
3.000 m on axis.** Loopback cable still patched. Speaker powered. SSH tunnel
closed.

## Expiry

Supersede when the microphone moves, when the speaker or converter routing
changes, or at any change of sample rate or period size. What outlives the
geometry: the **floor-coupling correlation** (r = −0.765, reproduced across two
audio stacks), the **fixed-geometry SNR slope** (+3.38 smp/dB, r = +0.910), the
**two-term decomposition**, and the **absence of the period jump** — none depend
on the absolute distance being right.
