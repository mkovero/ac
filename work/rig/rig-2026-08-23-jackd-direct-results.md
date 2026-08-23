# rig-2026-08-23-jackd-direct-results — 192.168.9.25

The one-period τ jump (#363) was **PipeWire**. The operator removed
pipewire-jack from the audio path between the 2026-08-22 session and this one;
`jackd` now drives ALSA directly. This file records what that changed, and the
measurements taken to confirm the jump is gone rather than merely unobserved.

**Operator:** Markus, on site, reported the change and authorised this session.
**Session start:** 2026-08-23 01:26 EEST.
**Drive level:** −30 dBFS throughout, authorised by the operator for this
session. Two paths, authorised separately: the τ work is electrical only
(AN2 → IN4 cable, nothing into the room), and the 3 m acoustic cross-check at
the end drives the loudspeaker, authorised by the operator after the electrical
results were reported. `jack_iodelay` is the one signal in this session whose
level `ac` did not set; it goes into the cable only.
**Expires:** superseded when the rig's audio stack changes again. The stack
description is the perishable part; the numbers are dated and stay valid as a
record of this stack.

## Stack change

| | before (through 2026-08-22) | now |
|---|---|---|
| JACK server | pipewire-jack | `jackd` on ALSA `hw:Pro71990237` |
| period / buffer | 1024 / 4096 | **64 / 128** (`-p 64 -n 2`) |
| port names | `Babyface Pro Pro:capture_N` | **`system:capture_N`** |
| `monitor_N` loopback sources | present | **gone** |
| sample rate, clock | 96 kHz, AutoSync | unchanged |

Full command line:

```
jackd --realtime --realtime-priority 95 -d alsa -d hw:Pro71990237 \
      -r 96000 -p 64 -n 2 -i 10 -o 10 -I 116 -O 116
```

PipeWire still runs, demoted to an ordinary JACK client
(`PipeWire:playback_FL/FR`) carrying desktop audio.

**Two consequences for anything written before this date.** Port names in
stored configs, scripts and handovers are stale — the sticky
`output_port`/`input_port` form this session used had to be rewritten to
`system:playback_2` / `system:capture_4`. And the no-cable digital loopback
(`monitor_N` → worker input) was a pipewire-jack feature; a reference leg now
needs a real cable. Channel *indices* are unchanged, so everything in `ac`,
which resolves by index, points at the same physical connectors as before.

## τ under the new stack

| method | round trip | samples @ 96 kHz |
|---|---|---|
| `ac calibrate` (Farina peak, 2 lifetimes) | **4.4167 ms** | 424 |
| `jack_iodelay` (independent client) | **4.415 ms** | 423.817 |

Agreement: **0.18 samples**. Two separate client registrations, two unrelated
code paths.

**The old 43.7500 ms was ~90 % PipeWire.** 4200 − 424 = 3776 samples =
39.33 ms of buffering that no longer exists. This retires more than the jump:
the handover's model `round_trip = N × 1024 + 2152` described the constant
2152-sample remainder (22.4 ms) as "converter + USB". It was not. True
converter + USB here is at most 424 samples minus JACK's own 128-sample
buffering, ≈ 3.1 ms. Any figure derived from 43.75 ms — #277's τ, the
`it_loopback_ir` expectation, the geometry in the τ handover — is now wrong by
39 ms and must be re-derived, not rescaled.

## Discriminator — is the jump gone?

One JACK period is now 64 samples = 0.667 ms = 0.23 m of apparent distance,
against 1024 samples / 10.67 ms / 3.7 m before. So even a surviving jump does
16× less harm; the question is whether it survives at all.

### Pass 1 — 30 back-to-back runs (01:26:10–01:26:52, 42 s)

30 runs × 2 readings = **60 client lifetimes. Every reading 4.4167 ms.** Zero
jumps, zero refusals, `period 64` reported on every run. Zero xruns logged by
jackd across the window (count 8 before, 8 after).

Back-to-back alone does **not** clear this. The 2026-08-22 state was sticky
over seconds and flipped on a 5–60 s timescale of client churn; the old
session's own 15-run back-to-back block read 43.75 ms 15 times and proved
nothing. Pass 2 exists because of that.

### Pass 2 — spread soak, 35 runs at 8 s spacing (01:28:31 onward, ~5.5 min)

35 runs × 2 readings = **70 client lifetimes, spread across ~5.5 minutes** so
the samples straddle the 5–60 s timescale the old state flipped on. **Every
reading 4.4167 ms.** No jump, no refusal, no drift.

Combined with pass 1: **65 runs, 130 client lifetimes, one value.** Against the
old stack's 43 % per-run rate, the jump is gone, not merely unobserved.

### xruns during the session

17 xrun events appear in jackd's journal over the session window, and **none of
them are `ac`**:

| time | client | events |
|---|---|---|
| 01:15:03 | `jack_delay` | 5 (operator's own run, before this session) |
| 01:22:04 | `Carla` | 3 |
| 01:27:39 | `jack_delay` | 5 (this session's cross-check) |
| 01:28:03 | `jack_delay` | 4 (this session's cross-check) |

`ac-daemon`'s workers ran 130 lifetimes at period 64 without a single xrun;
`jack_iodelay` xruns on essentially every start. Recorded because it inverts
the expectation the lower latency created — the tool used to *check* the rig is
the one that cannot keep up with it, and the hazard in the next section is
therefore latent here, not active.

## jack_iodelay reports a bad "extra loopback latency"

`jack_iodelay` prints `extra loopback latency: 4294967295 frames` — an
unsigned underflow, not a measurement. jackd is told `-I 116 -O 116`, so it
claims 116 + 116 = 232 frames of external latency on top of its own 2 × 64 =
128, i.e. 488 frames claimed against 423.817 measured. The claim overstates by
≈ 64 frames.

This does not affect anything `ac` measures — `ac` reads the position of the
impulse peak, not JACK's reported latency — but it makes JACK's own latency
figures wrong for any other client that trusts them. To correct it, drop each
by ~32: `-I 84 -O 84` puts the claim at the measured 424.

## What `calibrate` still does not check

`handlers/calibrate.rs` never reads the xrun counter. `plot`, `test` and
`monitor_tui` all print one; the JACK backend maintains it
(`jack_backend.rs:229`, exposed at `:549`); τ ignores it. At period 1024 that
was tolerable. At period 64 xruns are expected — jackd logged them for
`jack_delay` on this very rig at 01:15 — and an xrun inside the 0.35 s
τ sweep corrupts the IR the peak is taken from. The result is a bare number
with no indication anything was dropped. Filed separately; not a regression,
newly reachable.

## Acoustic cross-check at 3 m

Operator-authorised, −30 dBFS, loudspeaker on `system:playback_5` (ADAT →
external converter), microphone on `system:capture_1`, stated distance 3 m.

**`ac calibrate` cannot do this measurement, and the reason is structural.**
`handlers/calibrate.rs:428` gates the τ path on a near-unity-gain loopback:

```rust
let loopback_dbfs = ref_dbfs - 20.0 * 2f64.sqrt().log10();   // drive − 3.01 dB
let is_loopback = (captured_dbfs - loopback_dbfs).abs() <= 2.0;
```

An acoustic path is tens of dB down, so `is_loopback` is false and τ is skipped
with `not measured (loopback not detected this run)` — at any mic gain. This is
correct for what τ means, but it does mean `calibrate` is the wrong instrument
for an arrival and the message does not say which one is right.

`ac plot ir 100hz 20khz 1s -30dbfs`, seven runs. numid=301 read 12 at setup and
36 by the end — the operator raised it mid-block, so the two rows below are
*run order*, not a controlled gain comparison (see the caveat after the
decomposition):

| block | arrival, samples | ms |
|---|---|---|
| first four | 1504, 1510, 1510, 1504 | 15.667–15.729 |
| last three | 1511, 1505, 1506 | 15.677–15.740 |

Mean ≈ 1507 samples = **15.70 ms**, spread 7 samples = 0.073 ms = **2.5 cm**.
The arrival held across the whole block, including across a preamp change of
unknown timing within it — which is what makes the figure worth quoting.

### What the number confirms

Decomposing, with the two subtracted terms flagged as assumptions. **This
table is superseded** — "Third pair" below redoes it with the interface term
measured on the pair that actually shares the acoustic path's output side, and
gets 3.17 m. It is kept because the reasoning it rests on, and the size of the
correction, are the point:

| term | ms | source |
|---|---|---|
| measured peak arrival | 15.70 | this session, 7 runs |
| − interface round trip | 4.4167 | measured on the *analogue* pair AN2→IN4; assumed equal for the ADAT-out/mic-in legs |
| − external ADAT converter | 1.1931 | prior rig sessions, derived from `transfer_stream` differentials under the old stack |
| = flight, peak at face value | 10.09 | → **3.49 m** at 346 m·s⁻¹ |
| − known peak-late bias | ≈1.5 | `argmax|h|` reads late on a multi-way speaker |
| = corrected flight | 8.59 | → **2.97 m** |

Against an operator-stated 3 m. The agreement is closer than the method
deserves — the bias term is itself ~0.5 m — so read this as **3.0 m ± ~0.5 m,
dominated by the peak-late bias**, not as a centimetre-accurate result.

**The decisive part is not the agreement, it is the sign.** Measured arrival
15.70 ms is *less* than the old stored τ of 43.75 ms. Under the old constant
this measurement yields a negative flight time — the speaker would sit at
negative distance. So the acoustic path independently rejects the pre-PipeWire
τ, rather than merely preferring the new one.

### `plot ir` cannot report distance on this path

Every acoustic run printed:

```
distance  unavailable — no τ history recorded for device 0 / jack backend yet
          — run `ac calibrate` with loopback patched to measure one
```

This is not a missing-calibration accident. `cal.json` is keyed per channel
pair at the top level (`out1_in3` for the loopback, `out4_in0` for the
acoustic path), and `tau_history` lives *inside* that entry; on top of that,
`Calibration::tau_for` requires an exact `TauConditions` match including
`output_port` and `input_port`. So a τ measured on the loopback pair is
invisible to a measurement on the acoustic pair, twice over.

The instruction the message gives — "run `ac calibrate` with loopback patched"
— cannot be followed on the pair it is asking about, since nothing patches a
loudspeaker-fed ADAT output back into the microphone input. The operator can
still measure the leg that is missing: this rig has a converter loopback on
`playback_7 → capture_3`, which carries the ADAT-out and converter delay the
acoustic path shares. But a τ stored under `out6_in2` does not reach
`out4_in0` either, so measuring it does not make the distance readout appear —
it only gives a human a number to subtract by hand. That is the gap worth an
issue: the delay is measurable, and the tool that measured it will not report
a distance with it.

### The mic gain split above is not attributable — do not read it as a result

The table splits the runs by mic preamp setting, but the split is unsound. The
operator raised numid=301 from 12 to 36 *during* the run block and said so
afterwards, so the boundary between the two sets is not where the table puts
it. numid=301 read 12 at setup, before the first `plot ir`; at most that first
run (peak 0.0660 FS, `pre-imp SNR` 13.7 dB — the lowest of the seven by about
1.4 dB) was taken at 12, and possibly none were.

An earlier draft of this file read the near-identical peak levels across the
two sets as "24 dB of preamp gain does nothing", and speculated that the
microphone must be on the external ADAT converter rather than the Babyface's
AN1. **That is withdrawn**: both sets were probably at 36, so there is no
measured gain change to explain, and nothing here bears on where the mic is
plugged in.

What the seven runs *do* support stands unaffected, because it never depended
on the split: the arrival held at 1504–1511 samples across all of them, and
across a preamp change that landed somewhere inside the block. If the gain
question matters later, it needs its own run — read numid=301 immediately
before and after each measurement, or compare ambient noise floors at the two
settings, which needs no emission at all.

## τ on a second port pair — the converter loopback

The operator pointed out a loopback the earlier sections missed: `playback_7 →
capture_3`, out through ADAT to the external converter and back. Two false
starts before it measured, both worth recording:

1. **The matrix route was muted.** `07-ADAT3 Playback Volume` (numid=295) read
   0, so capture_3 sat at −83.8 dBFS and every channel looked equally dead.
2. **Then the unity gate refused it.** With the route at 65535 the path
   captured −30.0 dBFS for a −30 dBFS drive — exactly 3.01 dB (×√2) hot
   relative to the analogue leg's −33.1 — and `is_loopback` wants
   drive − 3.01 ± 2 dB, i.e. [−35.01, −31.01]. Refused, five runs, reported as
   `loopback not detected this run`. The operator set numid=295 to 46341
   (65535/√2); it then captured −33.0 and measured immediately.

| pair | ports | τ | samples |
|---|---|---|---|
| analogue | `playback_2 → capture_4` | 4.4167 ms | 424 |
| via converter | `playback_7 → capture_3` | **4.5625 ms** | **438** |

Five runs each, no spread within either.

**The delta between two physically different port pairs is 14 samples —
0.1458 ms, 5 cm of apparent distance.** That is the number the exact-match rule
in `tau_for` is protecting against on this interface.

**Operator corrections, in order, and what they cost.** This section
originally claimed `playback_7 → capture_3` was a digital ADAT return. It is
not. The operator then had to correct a second version of the same mistake:
**there is no ADAT return on this rig at all — nothing comes back digitally to
the Babyface.** `playback_5` and `playback_7` both feed the same external
converter; every return path is analogue, into a Babyface analogue input.

Both errors came from the same move — inferring topology from a small delta.
438 − 424 = 14 samples looked "too short for a converter DAC+ADC", but that is
a difference between two paths that each contain a converter pair, so they
largely cancel and 14 samples is an unremarkable residue. Comparing a
difference against the scale of an absolute. The second version then invented a
digital return to explain the first. Numbers this close cannot identify a
signal path; only the person holding the cables can.

### What the three pairs are, with no inference

All three are `converter or on-board DAC → analogue → Babyface analogue input`.
Writing `C` for the common interface term (USB in and out plus JACK buffering,
cancels in every difference), `Bd` for the Babyface DAC, `Ao + Cd` for reaching
the converter's analogue output, and `Ba(x)` for the Babyface ADC on input `x`:

The two converter-fed paths are **not** equivalent, which is the fact that
makes the numbers mean something (operator, 2026-08-23):

- **`playback_5` is the master output.** After the converter it passes the
  entire analogue master section — Studer 900 channel strip, limiter, volume
  fader — and that is the chain the loudspeakers hang off.
- **`playback_7` is a direct path**: ADAT through the converter to an analogue
  output and straight back to the Babyface, with no master section in it.

| pair | path | legs | samples |
|---|---|---|---|
| `playback_2 → capture_4` | Babyface DAC → IN4 (line) | `C + Bd + Ba(IN4)` | 424 |
| `playback_7 → capture_3` | converter, direct → IN3 (line) | `C + Cx + Ba(IN3)` | 438 |
| `playback_5 → capture_2` | converter → **master section** → AN2 | `C + Cx + M + Ba(AN2)` | 484 |

- `484 − 438 = 46 samples (0.479 ms)` = **`M + [Ba(AN2) − Ba(IN3)]`** — the
  analogue master section plus whatever separates an AN input from a line
  input. Both loops share the converter, so nothing else remains.
- `438 − 424 = 14 samples (0.146 ms)` = `Cx − Bd + [Ba(IN3) − Ba(IN4)]`.

**Neither difference isolates a term, and the 46 samples should not be assumed
to be the master section.** Half a millisecond is a long time for passive
analogue electronics — a channel strip, a limiter and a fader are microsecond
devices unless something in that chain is not analogue. So `M` and the
input-stage difference are both live candidates and this measurement cannot
separate them. Recorded as an open number, not an explained one.

**The test is one cable move:** the existing AN2-out loopback, currently
landing on IN4, moved to IN3. That gives `C + Bd + Ba(IN3)` directly, so
`Ba(IN3) − Ba(IN4)` falls out against the 424 already in hand, and the
input-stage question separates from the master-section question.

### Why this is the right τ for the acoustic path anyway

The loudspeakers are fed from the master section, and so is
`playback_5 → capture_2`. That pair therefore carries the converter *and* the
Studer chain — the same output chain the acoustic measurement goes through,
diverging only where the speaker takes over from the patch cable. Using it for
the 3 m decomposition was correct, and for a better reason than the one given
when it was chosen: it was picked because AN2 matched the microphone's AN1
input family, before the master section was known to be in the path at all.

It also explains the level. `playback_5 → capture_2` captured −37.2 dBFS where
the direct paths sat near −33: **the master volume fader is in that path**, so
that leg's level is an operator-set analogue position, not a fixed property of
the rig. The +4 dB of AN2 input gain added to clear `calibrate`'s unity gate is
compensating for a fader position that can move between sessions — and if the
fader moves, that gate will refuse the pair again.

## Third pair — the converter DAC leg, measured

The operator patched the converter's analogue output (the loudspeaker feed)
into `capture_2` / AN2, making the speaker leg's output side measurable as a
loopback for the first time.

It was refused twice more before it measured, both times by the same gate: at
AN2 gain 12 the path captured **−37.2 dBFS**, 4.19 dB below the gate's target
of drive − 3.01 dB and so 2.19 dB outside its ±2 dB window. Raising
`2-AN2 Capture Volume` (numid=304) from 12 to 16 put it at −33.3 dBFS and it
measured immediately. AN2's 48 V and PAD were both off, so the line-level
converter output was never at risk on that input.

| pair | path | τ | samples |
|---|---|---|---|
| `playback_2 → capture_4` | Babyface DAC → IN4 (line) | 4.4167 ms | 424 |
| `playback_7 → capture_3` | converter, direct → IN3 (line) | 4.5625 ms | 438 |
| `playback_5 → capture_2` | converter → master section → AN2 | **5.0417 ms** | **484** |

Five runs each, no spread within any of them.

**Three physically different paths on one interface span 60 samples — 0.625 ms,
0.22 m.** That is the entire error budget `tau_for`'s exact-match rule is
protecting against on this rig, and it refuses to report anything rather than
accept it.

### The acoustic decomposition, now measured rather than assumed

Using the pair that shares the acoustic path's output side:

| term | ms |
|---|---|
| measured arrival, 7 runs | 15.70 |
| − τ (`playback_5 → capture_2`, measured) | 5.0417 |
| = flight + speaker + mic + peak bias | **10.66** → 3.69 m at face value |
| − documented peak-late bias, ≈1.5 ms | 9.16 | → **3.17 m** |

Against an operator-stated 3 m. The two earlier estimates — 2.97 m from the
assumed 1.1931 ms constant, 3.33 m from the `playback_7` pair — bracketed this
one. Only the first was ever a real candidate: `playback_7` bypasses the
analogue master section that the loudspeakers are fed through, so its τ is
short by that whole chain — 46 samples, which is most of why that estimate
missed high.

**What the residual does not do is decompose.** The 1.99 ms left after
subtracting 3 m of air at 346 m·s⁻¹ lumps together loudspeaker group delay,
the microphone leg, and the peak-late bias, and it does not cleanly match the
1.1931 ms intercept measured under the old stack by the differential
`transfer_stream` method — which used a different arrival estimator and so
carries a different share of the bias. Do not treat 1.99 ms and 1.1931 ms as
the same quantity measured twice. Separating them needs an arrival estimator
whose bias is known, not another loopback.

### The gate refused three correct loopbacks in one session

Every one of the three pairs above was refused at first contact, and none of
the refusals was about timing:

| pair | captured | why refused |
|---|---|---|
| `playback_7 → capture_3` | −83.8 dBFS | matrix route muted (numid=295 = 0) |
| `playback_7 → capture_3` | −30.0 dBFS | 3.01 dB hot — route at full scale |
| `playback_5 → capture_2` | −37.2 dBFS | 4.19 dB low — input gain at 12 |

τ is a *timing* measurement: the position of a deconvolved peak does not depend
on the path being at unity gain. The ±2 dB window is there to distinguish "a
loopback is patched" from "nothing is patched", and for that it is far too
tight — a working cable with 3 dB in it reads identically to no cable at all,
because both produce `loopback not detected this run`. The operator's recourse
is to tune analogue levels to ±2 dB before the instrument will report a number
that does not depend on level.
