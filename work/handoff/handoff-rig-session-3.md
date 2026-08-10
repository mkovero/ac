# handoff-rig-session-3 — geometry, and the case that has never been measured

Rig: 192.168.9.25. Build: `main` (contains #237 and #239).

Two sessions of delay work have been compared against each other rather than
against anything physical, and the acoustic setup was never recorded. This
session fixes both. Nothing here needs new software.

---

## Pre-flight — all of these have bitten before

- **Stop the daemon before installing.** `sudo cp` partially succeeded last
  time: `ac-daemon` failed `Text file busy` under the running daemon while the
  other two went through. "Did the install command run" passes in that case.
- **Verify all three binaries by sha256.** Size and mtime both passed on a
  differing binary in an earlier session.
- `install.sh` does not ship `ac-view`. Copy it by hand.
- **Clock stays `AutoSync`** (`numid=320 = 0`). The external master clocks the
  card over ADAT, and ADAT carries playback_5 — the stimulus leg. Setting
  Internal breaks the speaker path. Do not "fix" this.
- **Wiring:** loopback is **playback_2 (AN2) → capture_4 (IN4)**. capture_3 is
  digitally silent. Session config wants `reference_channel: 3`,
  `reference_output_channel: 1`. Use an isolated `HOME` — `~/.config/ac/
  config.json` still points `reference_channel` at the dead capture_3.
- **Write down which speakers are energised, for every single capture.** The
  room has two on the right side and one at the back, stereo-summed. Which were
  on has never been recorded, and it confounds the comparison between the two
  previous sessions.
- Contemporaneous silent baseline before and after. The floor moved 10 dB
  across session 2's evening and 0.1 dB across session 3's.
- **One glance at the magnitude pane with a live trace**, for #242: the delay
  readout has moved down 32 px into the trace region. It sits over stage 0 at
  96 kHz, which settles in 0.11 s, so it will be populated the moment you look.
  Grey-on-ember legibility there is a screenshot judgement that painted rects
  cannot make. #242 is rebased and green but held for this check.

## Emission

−30 dBFS nominal pink was the working figure. Per-run consent, clamped
server-side. Stop emission between runs.

---

## Setup as actually built (2026-08-04)

- **Single source:** one Genelec 1083 (prototype), right side, fed from
  **playback_5**. **EQ off.** All other speakers powered down.
- **Microphone:** Beyerdynamic measurement mic, **on axis, 1.000 m from the
  tweeter**, 90° capsule orientation. Calibration file
  `beyer/449350_34804_90Grad.txt`.
- **Room temperature ~24–26 °C.**

**The calibration file is not needed for this session.** It corrects the
microphone's own magnitude response, which matters for a reported transfer
curve and not at all for delay estimation — the correlation is looking for
timing, not level accuracy. Wire it in later; do not open it tonight.

**This is the cleanest positive case the rig can produce.** A single named
source, on axis, at short range, with no ambiguity to blame. That makes Run 1
straightforward and gives Run 2's hard negative something unambiguous to be
compared against.

### Speed of sound at this temperature

Do **not** use 343 m/s. That is the 20 °C figure and this room is warmer:

| T | c | flight per 1.000 m |
|---|---|---|
| 20 °C | 343.21 m/s | 2.9136 ms |
| 24 °C | 345.55 m/s | 2.8940 ms |
| **25 °C** | **346.13 m/s** | **2.8891 ms** |
| 26 °C | 346.71 m/s | 2.8843 ms |

Use **c = 346 m/s**. The 24–26 °C uncertainty is 9.7 µs at 1 m — about
3.4 mm, or roughly one sample at 96 kHz (one sample = 3.6 mm). So temperature
uncertainty is *below* the measurement's own resolution at this distance and
can be ignored. It will matter at 3 m, where the same spread is ~10 mm; record
the temperature whenever the distance grows.

Expected total at this position: electrical latency plus ~2.89 ms of flight.
Session 2's inferred system latency was ~3.05 ms, so a total near 6 ms — which
is where its position 1 landed.

## Run 1 — per-speaker geometry (do this first)

**This is the measurement that retires the circular-tolerance problem.** Every
delay figure so far has been checked against a previous session's estimate,
produced by an estimator that was itself under test. This replaces that with
physics.

For **each speaker, one at a time, named**:

1. Electrical loopback (playback_2 → capture_4) with the same stimulus →
   converter and buffer latency at **zero flight**. This is the constant to
   subtract.
2. Tape-measure that speaker's driver to the microphone capsule. Record the
   number, not an estimate. **Tonight: 1.000 m, tweeter to capsule.** Note
   which driver was measured — on a two-way box the woofer sits a few
   centimetres from the tweeter, which is inside tape accuracy at 1 m but not
   at 3 m.
3. Acoustic delay minus electrical delay, **× 346 m/s** (see above), against
   the tape.

**Deliverable: an expected arrival time per speaker.** After this, every future
lock is checkable against geometry rather than against history — which is what
made session 2's 11.34 ms anchor circular and unusable.

Note the ambiguity this resolves. Three candidate lags from the last two
sessions sit at 2.84 / 2.69 / 2.53 m implied distance, evenly spaced 15.5 cm
apart. That was read as a reflection structure; with the speaker configuration
unrecorded, different sources explain it just as well. The tape settles which.

Speed of sound is temperature-dependent — about 0.6 m/s per °C, and the
constant for this room is 346 m/s, not 343. See the table above before
treating any disagreement as a defect.

**The strongest single result available tonight:** if prominence is *still*
below the gate of 24 in this configuration — single source, on axis, 1 m, no
ambiguity — that is the clearest possible evidence the gate is simply too
high. Better evidence than the 3 m data, because there is nothing left to
blame. Record the prominence for every session whether it locks or not.

## Run 2 — the hard negative

**Neither candidate gate rule has ever been tested against the case that would
actually test it.** Session 2 produced it by accident at position 1 and it was
read as noise.

Same position, same evening, speaker state recorded for both:

- **One speaker alone** — the easy positive. A single dominant arrival.
- **Two speakers together** — the hard case. Two comparable arrivals, no single
  right answer.

Six to eight fresh sessions each (the lock caches per session). Record for
every session: locked delay, `delay_locked`, `delay_attempts`,
`delay_prominence`, `peak_lag`, `noncausal_peak_lag`, `negative_lag_median`,
and the full `delay_evidence`.

**What this decides.** In the two-speaker case, low prominence and refusal may
be *correct* — the measurement genuinely is ambiguous. If both candidate rules
accept it, they are accepting an answer that has no meaning. If both refuse it
and accept the single-speaker case, both are viable and the choice is made on
other grounds.

This is also a live-sound procedural point, not only a test: a single-arrival
estimator against a stereo-summed system is being asked a question with no
single right answer. Isolating the source is how the measurement is actually
done.

## Run 3 — independent attempts

One capture with **attempts ≥2.5 s apart**.

The offline scoring found that successive estimates share about 60% of their
samples — the ring is 2.5 s and retries land ~1.0 s apart — so
"successive estimates agree" partly measures buffer overlap rather than
reproducibility. Scored honestly on non-overlapping windows, the repeatability
rule needs about ±120 samples (±1.25 ms, ±0.43 m) to hold its hit rate.

This run scores it on genuinely independent estimates instead of by
subsampling. It needs no code change — just a longer gap between attempts,
however that is arranged.

## Run 4 — `CHECK ROUTING`, if convenient

Reference fed from an independent pink generator, so the two legs carry
unrelated content.

**Expect it not to fire.** `CHECK ROUTING` reads ladder coherence columns, the
ladder needs a lock, and unrelated legs refuse — so there is nothing to
evaluate. That is a known consequence of the same root cause as finding 3, and
it is not fixed. Confirm and move on; do not investigate.

Worth doing only because it costs two minutes and the assumption has never been
checked on hardware.

---

## What to expect, so nothing reads as a failure

- **Refusals are the system working.** The gate is 24 in the causal definition
  and the last session got zero locks in eight sessions at 3 m with prominence
  14.7–17.0. Judge on physical plausibility and geometry consistency, never on
  lock count.
- **`LOST LOCK` will not appear.** The delay is estimated once and cached, so
  `delay_locked` never returns to false. That row is dormant until #226. A
  never-locked session correctly shows `NO LOCK`, and shows it within a second
  or two of starting — ratified, not a defect.
- **Stage 0 coherence will not exceed ~0.75** at a metre. Reverberation-limited,
  flat across 20 dB of gain. Gain cannot improve it.
- **Session 2's prominence numbers are in a different definition** — thresholds
  are now measured against the strongest *causal* peak. Do not compare the two
  sets.

## Recording

Per capture: sample rate, drive level, input gain, **which speakers are on**,
mic position and distance, silent baseline before and after, and the fields
listed in Run 2.

Write to a new `work/rig/rig-session-3-results.md`. Do not merge into either previous
results file — the threshold definitions differ and the speaker configuration
is unknown for both.

## Still out of reach this session

- **Run C positions 1, 2, 4, 5** — need the mic movable. Worth doing if it is,
  now that geometry per speaker makes positions comparable.
- **Run D** (#208's positive control) — the daemon has no burst primitive, so
  `set_drive` over ZMQ cannot approach 50 ms, and the recurrence lives in a
  response that mostly does not exist while sessions refuse.
- **#226's design** — waits on this session. If the gate turns out to have been
  refusing valid measurements, locks are not unstable and the automatic-refresh
  half may not be worth building. That decision is downstream of Run 2.
