# handoff-243-redirect — stop building the calibration constant

**Read this before continuing on #243.** The issue's premise has changed. If
you have started building per-pair constant storage, a calibration command, or
config plumbing for a zero-flight offset, **stop and read this first** — most
of that work is no longer wanted.

Nothing you have done is wasted if it is small; the reasoning below is what
changed, not the quality of the work.

---

## What #243 said

The distance readout shows 1.88 m at a taped 1.5 m, because it converts raw
delay to metres without subtracting the instrument's own latency, and uses a
hardcoded 343 m/s. The proposed fix was a calibration procedure: patch the real
stimulus output to the real measurement input, measure the zero-flight
constant, store it per (measurement, reference) pair, show metres only when a
constant exists.

## Why that was wrong

The constant only exists because the reference and the stimulus travel
different paths on this rig.

Today the reference goes out **playback_2**, through the Babyface's own DAC,
down a cable, back into a Babyface input. The stimulus goes out **playback_5**,
over ADAT, through an *external* converter, then amp, speaker, air, mic. The
correlation measures the difference between those, so everything the acoustic
path has that the reference lacks — ADAT transport, the external converter's
DAC — survives as a residual. That residual is the 1.1931 ms measured in rig
session 3.

**The fix is the wiring, not a stored number.** Send the reference out
**playback_6** — the ADAT pair alongside playback_5 — through the *same*
external converter, and loop its analogue output back into a Babyface input.
Both legs then traverse Babyface → ADAT → external converter DAC → analogue.
Everything up to the converter's analogue output is common-mode and cancels.
What remains in the residual is only what the acoustic branch genuinely adds:
amplifier, speaker DSP, driver origin, air, microphone and preamp.

That is also how REW, Open Sound Meter and Smaart expect to be wired, so the
workflow becomes the standard one rather than a rig-specific arrangement.

**And it self-tracks.** Change interface, sample rate, buffer size, or
converter and both legs move together. A stored constant would rot silently
across exactly those changes, which is the failure mode worth avoiding — this
project has already been bitten twice by numbers that were correct when
measured and wrong later.

Markus is changing the cabling on the next rig visit. Assume correct wiring is
the supported topology.

---

## What #243 becomes

### 1. Document the supported topology

Reference out the same converter as the stimulus, looped back to an interface
input. State it where a user configuring a rig will find it, not only in a
design doc. Say plainly that the distance readout is only meaningful under this
wiring, and why.

### 2. Fix the arithmetic that is wrong regardless of wiring

- **Speed of sound is hardcoded at 343 m/s.** That is the 20 °C figure. The
  rig runs 24–26 °C, where c is 346 m/s — a 1% error, about 25 µs at 1 m, which
  is 2.4 samples at 96 kHz and therefore above the measurement's own
  resolution. Make it a parameter with a temperature input or a configurable
  constant. Do not leave a literal.
- **The readout presents raw delay as distance.** Under correct wiring that is
  right. Under any other wiring it is not, and today it silently is not. See
  the next item.

### 3. Add a plausibility check, not a calibration

Under correct wiring an acoustic measurement cannot produce a negative
distance, and a very small one is implausible for a microphone in a room. A
floor check is cheap and catches the misrouted case — the current rig wiring
would show roughly 41 cm of phantom distance, and nothing says so.

This replaces the calibration machinery: it detects the condition instead of
compensating for it.

### 4. What is explicitly out of scope now

- Per-pair constant storage.
- A calibration command.
- Config plumbing for a zero-flight offset.
- Showing metres conditionally on a stored constant existing.

If a rig genuinely cannot loop the reference through the same converter, a
calibration fallback has a case — **file it as a separate issue** rather than
building it here. It is a fallback for an unsupported topology, not the primary
mechanism.

---

## What does not change

The readout's other properties, the display placement (#242 is merged), and the
`ac-scene` computes-everything / `ac-view` renders-only discipline.

## Context worth having

- The 1.1931 ms constant and the geometry model behind it are in
  `work/rig/rig-session-3-results.md` and PR #244. The model predicted eight taped
  positions to ≤5 cm and caught a 10 cm placement error from a lag alone. That
  is what makes the wiring argument checkable rather than plausible.
- Session 3's electrical loopback on `[3,3]` locked at exactly 0 samples. That
  is capture_4 against itself — a useful ring-alignment check, and it measures
  nothing about path latency. Do not treat a zero there as evidence the paths
  match.
- One judgement is buried in all of this and worth stating in whatever you
  document: speaker DSP latency belongs to the *device under test*, not the
  instrument. Correct wiring leaves it in the measurement, which is almost
  certainly what an operator wants to see. A calibration constant would have
  quietly removed it.
