# rig-243-343-results — 2026-08-18, 192.168.9.25

Executes the #243 verification and the #343 attribution in one session, and
takes #251's captures. Rig: RME Babyface Pro on pipewire-jack, 96 kHz,
period 1024, buffer 4096, external master clock over ADAT
(`numid=320` = 0, AutoSync, untouched). Mic preamp `numid=301` = 36, the
session-3 baseline. Room 25–27 °C by the operator's estimate, taken as
**26 °C → c = 347.06 m/s**; the ±1 °C uncertainty is worth ±0.5 sample at
1 m and is not a term in anything below.

**Build:** `origin/main` @ `edc298a` plus branch `issue-243-rig-session`,
built on the rig into `~/target-243` with `RUSTFLAGS="-C target-cpu=native"`
(the rig has no `mold` — see `rig-loopback-ir-277-results.md`). Measurements
were taken with two new example binaries, `transfer_probe` and `ir_probe`,
because `ac transfer` launches `ac-view` and `ac plot ir` discards its result
(the gap epic #276 exists to close).

**Drive: −30 dBFS**, authorised by the operator for this session against the
standing −40 dBFS electrical ceiling, enforced server-side by
`drive_max_dbfs: -30.0` under `HOME=/home/mui/rig243-home`. Emission stopped
between runs. `jack_iodelay` was also used on electrical legs only, at its
own uncontrolled level, with no loudspeaker patched.

## Wiring

| leg | path |
|---|---|
| stimulus / acoustic | `playback_5` (ADAT1) → converter → Genelec 1083 → air → mic → `capture_1` |
| reference | `playback_7` (ADAT3) → converter → analogue out → `capture_3` |
| stimulus, electrical (temporary) | `playback_5` → converter → `capture_2`, with the 1083's cable moved |

Config: `output_channel: 4`, `input_channel: 0`, `reference_channel: 2`,
`reference_output_channel: 6`, `temperature_c: 26.0`.

---

## Result 1 — #243 passes, at the restated criterion

The reference now traverses the same external converter as the stimulus.

```
session 3 constant     114.5 samples   1.1931 ms   reference via Babyface DAC (playback_2)
this session           101.9 samples   1.0615 ms   reference via converter (playback_7)
drop                    12.6 samples   0.1316 ms
```

**Predicted drop was 12 samples**, from #277's direct measurement of the
converter's 0.125 ms cost (`rig-loopback-ir-277-results.md`). The rig
delivered 12.6.

The residual is `transfer_stream`'s locked delay minus flight:
378.5 − 276.6 = 101.9 samples at 1.000 m.

**Under the criterion this block carried until this morning — "the 1.1931 ms
residual collapses to ~0" — this run would have been written up as a
failure**, and the queue's own diagnostic would have sent the operator to
re-check wiring that is correct. The criterion was restated in PR #344 before
the run, on the strength of #277's number.

## Result 2 — #343 answered: the residue is not the estimator

The 1.07 ms residue decomposes, with every term measured rather than
inferred:

```
residual                 101.9 samples   (378.5 locked delay − 276.6 flight at 1.000 m)
  converter asymmetry     46.0 samples   playback_5 leg − playback_7 leg, measured directly
  speaker + mic           55.9 samples   = 0.58 ms = 20 cm
```

The asymmetry came from moving the 1083's own cable to `capture_2` and
measuring `playback_5` → converter → `capture_2` (4262.064 frames) against
`playback_7` → converter → `capture_3` (4216.096) in the same graph state.
20 cm of acoustic-centre-plus-capsule offset on a multi-way box at a taped
1.000 m is unremarkable. **Nothing is left over for an estimator bias.**

`transfer_stream` is exonerated. Its delay was 378 samples in three separate
sessions this morning and 378/378/379/379/379 in five repeats this evening —
a one-sample spread across sessions.

## Result 3 — the IR *peak* is not the arrival, and that was the confusion

Peak-picking a deconvolved IR reads late on a multi-way loudspeaker driven by
a 50 Hz–16 kHz sweep. Onset thresholds relative to the peak, at 1.000 m:

| point | offset | implied speaker+air+mic | as path |
|---|---|---|---|
| IR peak | 4741 | 479 samples | 1.66 m |
| 50% of peak | 4656 | 394 | 1.37 m |
| 25% of peak | 4634 | 372 | 1.29 m |
| **`transfer_stream` implies** | **4594.5** | **332** | **1.20 m** |
| pure flight, zero speaker latency | 4538.6 | 277 | 1.00 m |
| 10% of peak | 4508 | 246 | 0.89 m — **impossible** |
| 5% of peak | 4442 | 180 | 0.65 m — **impossible** |

The 10% and 5% crossings sit *before* sound could have arrived, so they are
pre-ringing of the bandlimited deconvolution rather than the wavefront. The
physically admissible band opens at 4538.6. `transfer_stream` lands inside
it; the IR peak lands 200 samples past it.

**Consequence for #276:** an IR-derived arrival needs an onset estimator, not
`argmax |h|`. Any distance or gate boundary taken from the peak carries this
loudspeaker's group delay as if it were path.

## Result 4 — the two-distance check closes it

The mic was moved to ~3 m. The increment is pure flight and cancels every
constant — τ, channel asymmetry, speaker, mic, and any fixed estimator bias.

Both positions are taped on axis, 1.000 m and 3.000 m, so the move is a
**taped 2.000 m** and this is a check against an external truth rather than
an internal consistency test. The operator's "roughly" refers to tape
accuracy over 3 m, not to an unmeasured position.

| | 1.000 m | 3.000 m | increment | implied distance |
|---|---|---|---|---|
| `transfer_stream` | 378.5 | 933 | **554.5 samples** | **2.0047 m** |
| IR peak | 4741 | 5300 | **559 samples** | **2.0210 m** |
| taped | | | 555 samples | 2.000 m |

**`transfer_stream` agrees with the tape to 4.7 mm. The IR peak agrees to
21 mm.**

Uncertainty on that comparison, so the two are not over-read: the room
temperature was estimated at 25–27 °C rather than measured, and that ±1 °C
is ±0.17% on `c`, or **±3.5 mm** over 2.000 m. Tape accuracy over 3 m to a
mic capsule is realistically no better than **±5–10 mm**. So the
`transfer_stream` figure sits inside the combined uncertainty and the IR
figure sits just outside it — consistent with the IR's much lower SNR at the
far position (11.25 dB against 28.66 dB at 1 m).

The two methods differ by **4.5 samples — 1.6 cm** on the increment while
differing by ~148 samples on the absolute (145 at 1 m, 151 at 3 m). A
constant that cancels in the increment and persists in the absolute is
exactly the signature of a fixed group-delay bias, and it confirms the
decomposition above from an independent direction.

---

## Result 5 — round-trip latency varies by exactly one period, per client

This one was not on the agenda and matters more than most of what was.

`jack_iodelay` on an unchanged leg, with no configuration change of any kind:

```
4262.064 frames
5286.064 frames     ← +1024.000
5286.063
5286.064
```

The **fractional part survives the jump untouched**, which is what proves it:
an integer number of periods added in software, not an analogue or converter
effect. The same thing happened earlier at +1019 and was cleared by a USB
replug of the Babyface — which cannot undo state inside a separate converter.

It is **per client, not per graph**. At one point `jack_iodelay` measured its
own clients at +1024 while the daemon's workers, running minutes apart on the
same machine, sat at the unshifted value: the daemon's acoustic IR read 4739
in both states.

**Consequences:**

1. **Absolute latencies are only comparable within one client's lifetime.**
   Comparing an IR taken by one process against a τ taken by another is
   unsound, and produced a wrong intermediate conclusion during this session
   before the mechanism was found.
2. **`transfer_stream` is immune**, because both legs live in one client for
   one session and a period added to the graph is common-mode. This is why
   its differential is stable to one sample across sessions, and why session
   3's electrical pair locked at exactly 0 in every frame of every run.
3. **#281's τ layer has a hole underneath the one filed as #340.** A τ
   measured twice under an *identical* condition tuple — same device,
   backend, sample rate, period size, port pair — can differ by exactly one
   period, because nothing about the configuration changed. No field in
   `TauConditions` can see it. A stored τ is one client restart away from
   being 10.67 ms wrong at 96 kHz, with the refuse-on-mismatch logic
   satisfied throughout.
4. **#276's settled decision 3 needs revisiting.** It reads: *"No live
   loopback reference for the Farina path. The stimulus is analytic … so the
   reference is a property of the signal chain, not of the run. It belongs in
   `cal.json`."* The signal chain's latency is measurably **not** a property
   that survives between runs. Either the Farina path captures a reference
   simultaneously, or its absolute arrivals are unreliable at the period
   level.

## Result 6 — the converter routing change is worth 2 samples

The operator's routing change on the reference leg, measured either side of a
revert, in one graph state:

| leg | poked | reverted | Δ |
|---|---|---|---|
| `playback_5` → converter → `capture_2` | 4264.064 | 4262.064 | **−2.000** |
| `playback_7` → converter → `capture_3` | 4216.096 | 4216.096 | 0.000 |

Two samples, exactly as the operator predicted before it was measured.
Within-state repeatability is 0.001 frames, so this is real by three orders
of magnitude. It was invisible for most of the session because a
period-sized graph artefact appeared at the same moment and was initially
attributed to the converter.

**The Babyface fell off ALSA entirely** partway through, while remaining
visible to `lsusb`: `/proc/asound/cards` lost it and `jack_lsp` showed only
`Dummy Output`. Recovered by a USB replug, after which rate, period, buffer,
clock and preamp gain were re-verified before measuring again. Any run in
that window would have fallen through to the dummy device and produced
plausible numbers from nothing.

---

## Captures

`audit/rig-243-2026-08-18/` — seven `transfer_probe` JSON Lines captures,
gzipped, every frame with the full `delay_evidence` object including the
uncapped `candidates` list. These are #251's data.

| file | position | state |
|---|---|---|
| `ac243-preflight.jsonl.gz` | 1.000 m | passive, no drive — silent floors |
| `ac243-run1-1m.jsonl.gz` | 1.000 m | volume 1, preamp 21 |
| `ac243-run2-1m-vol2.jsonl.gz` | 1.000 m | volume 2, preamp 21 |
| `ac243-run3-1m-gain36.jsonl.gz` | 1.000 m | volume 2, preamp 36 — **the clean 1 m reference** |
| `ac243-run4-1m-routed.jsonl.gz` | 1.000 m | **degraded graph state**, +1024 |
| `ac243-run5-1m-routed-repeat.jsonl.gz` | 1.000 m | **degraded graph state**, +1024 |
| `ac243-run6-3m.jsonl.gz` | 3.000 m taped | volume 2, preamp 36 — **the clean 3 m reference** |

Runs 4 and 5 are retained deliberately and must not be pooled with the
others: they were taken while the graph carried an extra period, and they
show a **bimodal** delay (381 / 414 / 415 / 418 across sessions) that does
not reproduce in the healthy state. That bimodality was initially read as an
estimator defect. It is not — it is what this instrument looks like when the
graph is sick, and it is worth having on disk as the signature of that.

## Preamp gain does not buy SNR here

| | preamp 21 | preamp 36 | change |
|---|---|---|---|
| mic floor, silent | −54.01 dBFS | −40.13 dBFS | +13.88 dB |
| mic driven | −45.66 dBFS | −30.44 dBFS | +15.22 dB |
| **SNR** | 8.35 dB | **9.69 dB** | **+1.34 dB** |

15 units of gain bought 1.3 dB. The floor rose almost as much as the signal,
so what limits this measurement is room noise at the capsule, not preamp or
converter noise. More gain only uses more of the ADC's range. Improving mic
SNR on this rig requires acoustic level or a quieter room.

## What this session does not say

- **Nothing about absolute distance accuracy**, only about the *increment*.
  Both positions are taped, but the constant term — converter asymmetry plus
  acoustic centre plus capsule — is derived from these same measurements, so
  the absolute is not an independent check. The 2.000 m increment is.
- **Nothing measured about room temperature.** 26 °C is the operator's
  estimate of a 25–27 °C range, and it enters every distance through `c`.
  A thermometer would remove ±3.5 mm from the 2.000 m comparison and is the
  cheapest accuracy improvement available to this rig.
- **Nothing about the loudspeaker's response**, only its arrival timing.
- **Nothing about whether 46 samples of converter channel asymmetry is
  constant** across other channel pairs, sample rates or period sizes. It was
  measured once, for one pair, at 96 kHz / 1024.
- **Nothing about `ac calibrate`'s τ path**, which was not exercised. #340
  and Result 5 above are both reasoned from measurements taken outside it.

## Expiry

Supersede when the mic moves, when the converter routing changes, or at any
change of sample rate or period size. Result 5 outlives the rest: it is a
property of the audio stack, not of this patch.
