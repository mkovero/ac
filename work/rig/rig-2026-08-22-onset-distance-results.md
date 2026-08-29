# rig-2026-08-22-onset-distance-results — 192.168.9.25

Track B of `work/rig/rig-test-plan.md`. Acoustic — speaker A on `playback_5`
through the external converter, microphone on `capture_1`.

**Operator:** Markus, on site. **Status: incomplete** — 1.000 m done, 3.000 m
pending at time of writing.

## Build under test

Built on the development VM, copied to the rig, hashed after the copy (see
`rig-2026-08-22-tau-loopback-results.md` for why nothing is built on this host).

| binary | sha256 | ref |
|---|---|---|
| `ac-issue-346` | `82410a74206c23a32b1c8c070d233750087e4a40cc40f4403d286db12fe97c24` | `b1ac5a7` (PR #352) |
| `ac-daemon-issue-346` | `098a7f5ee0db213725d7efd76e3cbbb8f3f00181346a5ecd3378cedee92f1a40` | `b1ac5a7` |

Daemon run as `HOME=/home/mui/rig2-home`, `--ctrl-port 25910 --data-port 25911`,
with the client pointed at it via `AC_CTRL_PORT` / `AC_DATA_PORT` so the CLI
could not auto-spawn the stale `/usr/local/bin/ac-daemon` (2026-08-04 build).

## Drive level

**−30 dBFS, authorised by the operator for this session**, passed explicitly on
every command.

**Enforcement was request-side only, and that is a deviation from the
interlock.** `drive_max_dbfs` is applied in `set_drive` alone
(`handlers/transfer.rs:133`); `plot_ir` has no clamp. Recorded as agreed with
the operator rather than claimed as a clamped run.

**One emission exceeded consent, and is recorded rather than omitted.** Early in
the block, `ac plot` was invoked with no arguments in the belief it would print
usage. It does not — it ran a 20 Hz–20 kHz THD sweep out `playback_5` into the
speaker at the CLI default of **−20.0 dBFS**, 10 dB above the authorised
ceiling, via the stale auto-spawned daemon. It was stopped within ~30 s. No data
from it was retained and none is used here. Cause: probing a CLI for usage on a
live rig; the correct move is reading the parser source, or `ac --help`, which
is the only form that prints usage without executing.

## Wiring — confirmed this session

- Stimulus: `Babyface Pro Pro:playback_5` (ADAT) → external converter → speaker A.
- Measurement: microphone → `Babyface Pro Pro:capture_1`.
- Electrical loopback `playback_2` → `capture_4` left patched throughout; on
  different channels, so nothing was re-patched between tracks.

## Clock

`AutoSync` (`numid=320` = 0), read this session.

## Configuration

96 kHz, period 1024. Sweep: 50 Hz–16 kHz, 2.0 s, 5 harmonics, 16384-sample
window requested, 0.8 s tail (a 2.5 s tail control was also run — see below).

## Temperature — not measured

No thermometer available. Operator states 25–27 °C. Per the test plan this
bounds the metres-domain criterion (#356 AC7) at ±3.5 mm and leaves the c-free
comparison (#346 AC5) unaffected. **It turns out not to be the limiting factor
for AC5** — the estimator's own scatter is an order of magnitude larger, below.

---

## Run B1 — #346 AC5, position 1.000 m on axis — **12 captures + 6 control**

**What was being verified:** the onset estimator's arrival (PR #352), toward
AC5's requirement that its increment over the taped 2.000 m match at least as
well as `transfer_stream`'s 4.7 mm.

**What a pass looked like, stated before running:** arrival ≈ τ (43.75 ms,
measured this session in `rig-2026-08-22-tau-loopback-results.md` Run A1b) plus
~2.9 ms flight, with the onset landing before the peak and the rule named.

### The estimator does what #346 says it should

Arrival lands **130–154 samples (1.35–1.60 ms) before the peak**, against
#346's recorded 145 samples at 1.000 m. The onset rule is reported —
`onset: backward threshold from floor, no causal bound` — satisfying #346's
requirement that a number can be told apart from a peak a year later. First
capture:

```
arrival       +4604 samples  (+47.958 ms re gate centre @ 96000 Hz)
              onset: backward threshold from floor, no causal bound
peak          0.4637 FS  at sample 12950  (diagnostic — not arrival)
pre-imp SNR   25.4 dB
```

### Two findings that decide the block

Twelve consecutive captures, **fixed geometry, same daemon, nothing touched
between runs** (arrival / peak index / pre-impulse SNR):

| run | arrival | peak | SNR dB | | run | arrival | peak | SNR dB |
|---|---|---|---|---|---|---|---|---|
| 1 | 4604 | 12950 | 25.4 | | 7 | 3585 | 11925 | 24.3 |
| 2 | 4606 | 12950 | 25.3 | | 8 | 3602 | 11927 | 23.6 |
| 3 | 4626 | 12949 | 23.9 | | 9 | 4608 | 12951 | 25.5 |
| 4 | 3606 | 11928 | 24.7 | | 10 | 4628 | 12950 | 23.9 |
| 5 | 3608 | 11926 | 23.6 | | 11 | 4625 | 12949 | 23.8 |
| 6 | 3586 | 11926 | 24.7 | | 12 | 4605 | 12949 | 24.5 |

**Finding 1 — the one-period jump reaches `plot_ir`, not just `calibrate`.**
The captures fall into two clusters whose peaks differ by **1023.3 samples** —
one JACK period at 1024. Seven runs in one cluster, five in the other, with no
configuration change of any kind. The peak index inside each cluster is stable
to **±1.5 samples** (12949–12951 and 11925–11928), which is what identifies the
gap as a discrete graph-buffering shift rather than drift.

#347 fixed this for `calibrate` by taking two readings in separate client
lifetimes and refusing on disagreement. **`plot_ir` has no such corroboration**,
so a single IR capture's arrival — and any distance derived from it — can be
10.67 ms, or 3.70 m, wrong with nothing to detect it. Agreed with the operator
to file as a new issue against `plot_ir` / #346, distinct from #347.

**Finding 2 — the onset is bimodal, and it tracks the noise floor.**
Normalising the period jump out (adding 1024 to the low cluster) gives twelve
arrivals spanning **28 samples = 0.292 ms = 101 mm**:

```
4604 4605 4606 4608 4609 4610 | 4625 4626 4626 4628 4630 4632
```

Six early (mean 4607.0), six late (mean 4627.8) — a **20.8-sample gap = 75 mm**,
split 6/6. Not a spread; two states.

The discriminator is the noise floor:

- early group: mean pre-impulse SNR **24.95 dB**
- late group: mean pre-impulse SNR **23.92 dB**
- **correlation of arrival against pre-impulse SNR: r = −0.767 (n = 12)**

About **1 dB of floor moves the onset 21 samples later**. That is #353's
mechanism measured on hardware: `estimate_onset`'s threshold is taken relative
to `floor_rms`, so a higher floor raises the threshold and the backward search
stops later — nearer the peak. #353 was filed against a wide-lobe synthetic
case with a 256-sample window; **this is the same coupling firing at ordinary
SNR on an ordinary capture**, which is a stronger claim than the issue makes for
itself.

**Control — the sweep tail is not the cause.** Six further captures with the
tail extended 0.8 s → 2.5 s (the ISO 18233 §6.3.2 tail-decay check had warned on
the 0.8 s capture):

```
arrival  4622 4608 4597 4596 4611 4617     peak 12947–12951     SNR 23.8–25.5
```

Range **26 samples**, peak still stable to ±2, no period jump in this set. The
scatter is unchanged, so it is not a tail artefact.

### What this means for AC5

| | single-capture scatter at 1.000 m |
|---|---|
| onset estimator (PR #352) | 26–28 samples ≈ **95–101 mm** |
| IR peak (what it replaces) | ±1.5 samples ≈ **5 mm** |
| **AC5 criterion** | **4.7 mm** |

Averaging all twelve leaves a standard error of roughly 3–4 samples ≈ 11–14 mm,
still 2–3× outside the criterion — and because the distribution is bimodal
rather than Gaussian, the mean depends on the mix of the two states, which is
not a stable quantity.

**The onset is more accurate and less repeatable than the peak.** It correctly
lands ~150 samples before the peak, which is the bias #346 exists to remove; but
AC5 scores an *increment*, and a constant bias cancels in an increment. On the
metric AC5 actually uses, the estimator this PR replaces is currently the
better-behaved one at this rig's SNR.

**Caveat that belongs with the number:** `transfer_stream`'s 4.7 mm came from
20 s of continuous correlation; this is a single 2.0 s sweep. They are not
like-for-like measurements, and AC5's wording does not distinguish them. The
comparison to make is `transfer_stream` against the onset **on this session's
own captures**, which requires the 3.000 m position.

**Confound:** pre-impulse SNR here is 23.6–25.5 dB. The 2026-08-18 session
measured mic SNR of 9.69 dB at −30 dBFS and established that this rig is limited
by room noise at the capsule, not by preamp or converter, so more gain will not
improve it. Whether the bimodality persists at materially higher SNR is
**unresolved and not answerable on this rig.**

**Verdict: decline to conclude on AC5 pending the 3.000 m position** — and note
that the scatter measured here is large enough that the increment will carry
wide error bars whatever it comes out at.

---

## Run B1b — position 3.000 m on axis — 12 captures

Identical parameters to the 1.000 m set. Mic moved by the operator; same daemon,
never restarted, so client lifetimes are continuous across the move.

| run | arrival | peak | SNR dB | | run | arrival | peak | SNR dB |
|---|---|---|---|---|---|---|---|---|
| 1 | 5219 | 13519 | 21.9 | | 7 | 4194 | 12500 | 22.7 |
| 2 | 5216 | 13518 | 21.6 | | 8 | 4187 | 12490 | 23.4 |
| 3 | 5217 | 13523 | 22.0 | | 9 | 4211 | 12502 | 17.8 |
| 4 | 5221 | 13516 | 19.7 | | 10 | 4191 | 12500 | 24.0 |
| 5 | 5212 | 13524 | 23.2 | | 11 | 5214 | 13522 | 23.8 |
| 6 | 4187 | 12496 | 21.7 | | 12 | 5217 | 13518 | 21.2 |

The period jump appears again — 7 runs high, 5 low, peaks 1023 apart. Same
normalisation applied.

The single largest onset outlier in the whole session is run 9 (4211 → 5235
normalised, 18 samples later than its neighbours) and it carries **the lowest
pre-impulse SNR of any capture, 17.8 dB**. That is the floor coupling again,
visible in a single point.

### The increment — what AC5 actually asks for

| | 1.000 m | 3.000 m |
|---|---|---|
| onset mean / sd | 4617.42 / **11.15** | 5217.17 / **6.44** |
| peak mean / sd | 12950.00 / **0.95** | 13520.67 / **3.73** |
| pre-impulse SNR | 24.43 dB | 21.92 dB |

Expected for a taped 2.000 m at c = 347.06 m/s (26 °C): **553.2 samples**.

| quantity | measured | as distance | error vs tape |
|---|---|---|---|
| onset increment | 599.75 samples | 2.168 m | **+168.2 mm** |
| peak increment | 570.67 samples | 2.063 m | **+63.1 mm** |
| **onset − peak** | **+29.08 samples** | — | **+105.1 mm** |

**The differential is the estimator-attributable number.** It contains no tape
and no speed of sound: both estimators ran on the same captures, so geometry and
temperature are common-mode and cancel exactly. **105 mm, against a 4.7 mm
criterion — 22×.**

### Verdict on #346 AC5 — **FAIL**, with a mechanism

Pre-impulse SNR fell **2.52 dB** from 1.000 m to 3.000 m, as it must when the
source is further away. The 1.000 m correlation says roughly 21 samples of extra
delay per dB of floor; 2.52 dB predicts ~53 samples, and the observed onset-minus-peak
differential is 29 samples — same sign, same order, consistent mechanism.

**So the onset estimator's error is distance-dependent, and therefore does not
cancel in an increment.** That is the specific property AC5 relies on. The IR
peak carries a large *constant* bias (~150 samples, ~1.5 ms) which cancels
exactly in an increment; the onset replaces it with a smaller but *variable*
bias which does not. On AC5's own metric, at this rig's SNR, the estimator this
PR introduces is worse than the one it replaces — while being, as #346 argued,
more accurate in the absolute.

Two independent measures both land at 22× the criterion: single-capture scatter
(101 mm at 1 m) and the increment differential (105 mm).

**This is #353 deciding a merge.** #353 was filed as a synthetic wide-lobe case
at a 256-sample window and flagged as needing a check against rig data before
choosing a fix. This session is that check, and the answer is that the coupling
is active at ordinary SNR on ordinary captures, and is large enough to dominate
the measurement PR #352 exists to improve. **#352 should not merge on the
assumption that AC5 is a formality.**

### The unresolved common-mode term — do not attribute it to either estimator

Both estimators over-read the taped increment: peak by **+63 mm**, onset by
**+168 mm**. The 2026-08-18 session got the peak agreeing with a taped 2.000 m
increment to 1.6 cm, so +63 mm is out of family with the prior record.

For the peak increment to be exactly 2.000 m, `c` would have to be 336.5 m/s,
i.e. **8.6 °C** — implausible against the operator's stated 25–27 °C, and
temperature was not measured this session (no thermometer). The remaining
candidates are the tape marks not being 2.000 m apart, a capsule height or axis
difference between the two positions, or the acoustic reference point of the
loudspeaker. **Unresolved, and it does not touch the +105 mm differential**,
which is common-mode-free by construction.

**Confound (both positions):** temperature not measured; pre-impulse SNR
21.9–24.4 dB, room-noise-limited per 2026-08-18, so it cannot be improved by
gain on this rig. Whether the coupling persists at materially higher SNR is
**unresolved and not answerable here.**

## Issues filed from this file's runs

- **#359** — `plot_ir`'s arrival inherits the one-period jump with no
  corroboration. #347 guards `calibrate` only.
- Result posted to PR #352. **AC5 fails**; recommendation there is that the
  architect look at #353 and #352 together rather than in sequence, because a
  fix to the floor coupling changes what AC5 should be measured against.

## Not run, and why

- **B2 (#356 AC7)** — not attempted. It needs a stored per-pair distance
  calibration, and the τ measured this session is keyed `out1_in3` (the
  loopback pair) while B2's transfer measures the acoustic pair. Whether
  #356's readout can consume a loopback-derived τ for a different pair is a
  question about that PR's design, not a measurement, and it was not worth
  starting late in a long session. **Read `#356`'s per-pair calibration path
  before the next attempt** — the answer decides whether B2 needs its own τ
  route or a config that exposes the existing one.
- **A2 period ladder / A3 ten-run corroboration** — not run. A2a measured τ
  once at period 1024 and it corroborated; the ladder across 256/512/2048 and
  the repeat statistics remain open. Note that #359's evidence already shows
  the jump is frequent (5 in 12, twice), which raises A3's value.
- **`CHECK ROUTING` post-lock** — not run.

## Still to run

- **3.000 m position** — B1 repeats for the AC5 increment, then B2 (#356 AC7).
- **B2 (#356 AC7)** needs a stored τ calibration before the metres readout will
  populate; the 1.000 m runs reported `distance unavailable — no calibration
  stored for this channel pair`. That is Track A2's `ac calibrate`, on the
  loopback that is already patched.
- **`CHECK ROUTING` post-lock** — rides along at whichever position the mic ends
  at.

## Rig state left behind

**No emission in progress, no daemon running** — all stopped at session end.
Clock `AutoSync`, verified after the last run. **Mic taped at 3.000 m on axis**
(moved from 1.000 m mid-session). Loopback cable still patched. Speaker powered.

Full state, including the two `cal.json` files written and the one that carries
a mislabeled entry, is in `rig-2026-08-22-tau-loopback-results.md` — the two
files share a rig, so state is recorded once, there.

## Expiry

Supersede when the microphone moves, when the speaker or converter routing
changes, or at any change of sample rate or period size. The two findings that
outlive the geometry are the **floor-coupling correlation** (r = −0.767, a
property of the estimator) and the **period-jump frequency** (5 in 12 at two
positions, a property of the audio stack) — both survive a re-taping, since
neither depends on the absolute distance being right.
