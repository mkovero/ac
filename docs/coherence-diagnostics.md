# Coherence diagnostics — why γ² is low, and what actually fixes it

Operator-facing reference. The question this answers is the most common one
this instrument will ever be asked: *coherence is low, what is wrong?*

The short version is that coherence answers one question — **how much of the
measured signal is linearly explained by the reference** — and there are only
a few reasons it says "less than all of it". Most of them are not fixable by
touching the instrument, and the two rules below tell you which case you are
in before you change anything.

**Do not reach for the gain knob.** §2 is the algebra for why it cannot work.

---

## 1. The two rules

Both are answerable from the screen, in seconds, without changing the setup.

### Rule 1 — is the loss *absolute* or *relative*?

Change the level of the quieter leg and watch γ².

| what the loss tracks | what it is | what helps |
|---|---|---|
| the **absolute** level of the quiet leg | noise floor — the signal is near the floor of that input | more signal, closer mic, quieter room |
| the **ratio** between the legs | nothing — this is not a real effect | see §2; the ratio cannot matter |

Absolute means noise, and **no implementation could do better**: coherence is
reporting a true property of the capture. Relative cannot happen, and if it
appears to, the cause is somewhere other than coherence.

### Rule 2 — is the loss *HF-first* or *broadband*?

Look at where on the frequency axis γ² falls away.

| shape | mechanism | where it is treated |
|---|---|---|
| **HF-first** — good at LF, collapsing upward | residual delay rotating phase across a column's bandwidth | §4 |
| **broadband** — flat-ish loss everywhere | SNR, or window overlap | §2, §3 |

HF-first is the common one on a real acoustic path and it is a *delay*
problem, not a level problem. That is the whole reason §4's tolerance table
exists.

---

## 2. Gain cannot reduce coherence — algebraically, not empirically

Scale the measurement leg by `a` and the reference leg by `b`. Then

```
|Gxy|²  scales as (ab)²
Gxx     scales as a²
Gyy     scales as b²

γ² = |Gxy|² / (Gxx · Gyy)  →  (ab)² / (a²·b²)  =  1
```

**The scale factors cancel exactly.** γ² is a ratio of quantities that carry
the same gain, so it is invariant to the level of either leg. This is an
identity, not a property of this implementation, and it holds for any correct
coherence estimator.

Measured, so it is not left as an assertion:

- **20 dB of input gain moved stage 0 coherence by 0.006** — 0.710 → 0.716
  across preamp settings 36/46/56, with geometry, room and
  direct-to-reverberant ratio held constant (rig session 1, Run 7,
  `work/rig/rig-session-results.md`). The spread is smaller than the
  frame-to-frame noise.
- A microphone reading **15 dB below the reference** returns γ² = 1.0 on a
  digital loopback.

### What looks like a gain effect is SNR

The thing that *does* move coherence is how much of the captured signal is
noise, and that has a closed form:

```
γ² = SNR / (1 + SNR)
```

| SNR | γ² |
|---|---|
| 20 dB | 0.990 |
| 10 dB | 0.909 |
| 6 dB | 0.799 |
| 0 dB | 0.500 |

Turning up a preamp raises signal and noise together, so SNR — and therefore
γ² — does not move. Moving the microphone closer, or reducing the noise,
changes SNR and does. This is the mechanism behind Rule 1: the loss tracks the
*absolute* level of the quiet leg because that is what sets its SNR, never the
ratio between the legs.

### Stage 0 at 0.755 is not a defect

On a live acoustic path the top stage reads around 0.75, and it is
**reverberation-limited**: the microphone hears the direct sound plus a
reverberant field that the reference leg does not contain. That energy is real,
it is not linearly related to the reference, and coherence correctly declines
to count it.

Run 7 is what settles this rather than assuming it — flat across 20 dB of gain
means the deficit is the room, not the preamp. Only a closer microphone or a
deader path improves it. **This is the first number an operator misreads.**

---

## 3. Broadband loss: window overlap

The other broadband mechanism is analysis-window overlap: when the delay
between legs eats into the window, the two records share less data than the
window length implies. This dominates at low frequency, where §4's phase
rotation is negligible.

It is a real term and it is not the HF story. Attributing high-frequency loss
to overlap under-estimates delay sensitivity badly.

---

## 4. HF-first loss: residual delay, and how much is too much

**Moved here from `work/handoff/handoff-lock-and-smoothing.md` decision 5**,
which now points at this section. It lives here because a handoff carries a
delete condition and this material does not.

### The mechanism

High-frequency coherence loss is dominated by **phase rotation across a
column's bandwidth**, not by loss of window overlap. A column at 20 kHz spans
289 Hz at 1/48 octave. `Sxy` is summed across the bins in that column, and a
residual delay `τ` rotates phase across the span, so the coherent sum collapses
by roughly `|sinc(τ·BW)|` while `Sxx` and `Syy` — which carry no phase
difference — do not.

### The tolerance table

| highest frequency needed coherent | delay tolerance |
|---|---|
| 200 Hz | 62 ms |
| 2 kHz | 6.2 ms |
| 10 kHz | 1.2 ms |
| 20 kHz | **616 µs** |

**Sub-millisecond is a 20 kHz requirement, not a general one.** Subwoofer work
is trivially tolerant of delay, and an operator working below 2 kHz has
milliseconds of headroom.

### Derived, then measured

The 616 µs above is derived from `sinc(τ·BW)`. It was then simulated against
the real stage-0 path — sr 96000, nperseg 4096, 8 blocks, bins summed per
column before coherence, 1/48 octave at 20 kHz, BW 288.8 Hz:

> γ² tracks `sinc²(τ·BW)` within ±6% out to `τ·BW ≈ 0.45`, with the γ² = 0.9
> crossing at **625 µs = 12.5 cycles at 20 kHz**.

Derivation and measurement agree to 1.5%. Both numbers are kept because the
agreement is the point: this is one of the few places in this instrument where
a derived figure was checked against the thing it described.

### Precision is not the constraint — drift and discontinuity are

One sample is 10.4 µs at 96 kHz, 22.7 µs at 44.1 kHz, so sample resolution is
not what threatens a 616 µs budget. The real threats are discrete or drifting:

- **Two clock domains.** Both legs on one interface means one crystal and no
  drift. Separate devices means two crystals, and consumer crystals run
  ±50–100 ppm:

  | crystal error | time to drift 600 µs |
  |---|---|
  | 20 ppm | 30 s |
  | 50 ppm | 12 s |
  | 100 ppm | 6 s |

  **This is what sets the automatic re-lock interval — seconds to tens of
  seconds, not minutes.** A measurement that was aligned when it started is not
  aligned a minute later on two clocks.
- **Xruns.** A dropped buffer shifts alignment permanently by its own length.
  Nothing recovers it except re-locking.
- **The wrong peak.** A delay lock on a reflection rather than the direct
  arrival is a millisecond-scale error, not a microsecond one — far outside
  every row of the table above. See #227 and the delay-gate work.

### Display density and delay tolerance run opposite to intuition

**A coarser display is *less* delay-tolerant, not more.** This reads like a
typo and it is not — do not "fix" it.

Points per octave sets a column's bandwidth: fewer points per octave means
each column spans *more* frequency. The loss term is `sinc(τ·BW)`, so widening
BW gives the phase rotation more span to collapse the coherent sum, at the same
delay. Halving the display density therefore roughly halves the delay a
measurement tolerates at the top of the band.

ac fixes points-per-octave at 1/48 rather than exposing it, and this coupling
is part of why: a display-density control would silently be a delay-tolerance
control. The reasoning is recorded in `docs/design/design-mtw-ladder.md`.

If you have used a tool that demanded a tighter loopback than ac does, display
density is the first thing to compare — the tolerance above is a property of
the aggregation bandwidth, not of how coherence itself is computed.
