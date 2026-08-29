# rig-2026-08-22-tau-window-350-results — 192.168.9.25

Track A2/A3 follow-on, run the same evening as
`rig-2026-08-22-tau-loopback-results.md`. Electrical only — AN2 → IN4 cable,
no microphone, nothing into the room.

**Operator:** Markus, on site. **Session start:** 2026-08-22 22:28 EEST.
**Drive level:** −30 dBFS, authorised by the operator for this session before
the first emitting run. `calibrate` passes `ref_dbfs` through unclamped
(#360), so the requested level is the only ceiling on this path.

## Why this session exists, and what it changed about the method

`work/rig/rig-test-plan.md` § A2 said #350 could not be answered by the period
ladder: the only lever hardware has on edge proximity is τ itself, and τ moves
in period-sized steps (44.5 %, 33.8 %, 12.5 %, then off the end), so the band
between 0 % and the shipped 10 % margin is unsamplable. It asked for
`TAU_MIN_HALF_WINDOW_S` to be reachable from the environment in a test build.

That build now exists: `tau-window-override`, an off-by-default cargo feature
(branch `tau-window-override`, commit `19604fc`) exposing
`AC_TAU_HALF_WINDOW_S` and `AC_TAU_EDGE_MARGIN_FRAC`, plus a per-reading
`peak_abs` / `floor_abs` / `snr_db` probe — nothing on this path had ever
recorded the SNR the peak was located against. **Inverting the experiment**
(hold τ fixed, move the window) samples edge proximity continuously and,
unlike the period ladder, needs no JACK restart between points.

## Build under test

Built on the development VM, copied to the rig, hashed *after* the copy.

| binary | sha256 | ref |
|---|---|---|
| `ac-daemon` (feature `tau-window-override`) | `605bc9f9e6e3c90fd12c3b80802bf15aa193b03e07848f004934537b7337093c` | `19604fc` |
| `ac` | `7fa422d42d57f7c6918038c266b60fe76168f938f4680ffaf3a79822ddbdd902` | `19604fc` |
| `it_loopback_ir-issue-341` (cross-check) | as recorded in the Track A file | `ca41897` |

**`/usr/local/bin/ac-daemon` is on `PATH` and its config routes output to
`playback_5`, the loudspeaker.** `ac`'s `find_binary` searches `PATH` first, so
every run here prefixes `PATH` with the `bin-350` directory and kills any
stray `ac-daemon` before starting its own. Without that the sweep would have
driven the speaker instead of the cable. Recorded because it is a live trap
for the next session, not because it went wrong here.

## Configuration

96 kHz, period 1024, JACK via pipewire, clock `AutoSync` — unchanged from
#277 / #243 / the Track A file, so the numbers compare. Isolated
`HOME=~/rig-2026-08-22/home-350` with sticky `output_port` /`input_port`
naming the cable ports (never the positional `output 1 input 3` form, which
misroutes — #358).

Geometry at 96 kHz, τ = 4200 samples: edge clearance
`f = (half − τ − 1) / half`, so `half = (τ + 1) / (1 − f)`. Every row below is
a half-window chosen to place the *known* arrival at a chosen `f`.

---

## Pass 1 — descending window sweep, edge margin overridden to 0

14 half-windows from 0.06251 s (f = 0.30) down to 0.04200 s (arrival outside
the window), 3 `ac calibrate` runs each, 2 readings per run = 84 readings.

**τ was reported correctly (4200 samples / 43.7500 ms) at every window down to
f = 0.02, and the peak amplitude never moved**: `peak_abs` = 2.9507e-2 to five
significant figures across all 84 readings, at every window size. `snr_db`
ranged 33.8–83.5 dB and tracked **reading order**, not edge proximity — the
first reading after a fresh engine start always shows a floor ~30× higher
(1.5e-4 vs 5e-6) than the second. Edge proximity does not degrade this
measurement at this rig's electrical noise floor.

**But 18 of the 42 runs reported 33.0833 ms** — 3176 samples, exactly one JACK
period (1024 samples) below the true round trip — **and every one of them said
"2 readings agree".** The 33.08 runs clustered in blocks rather than scattering,
and the descending sweep confounded window size with wall-clock time, so pass 1
alone cannot say whether the small window caused it.

## Pass 2 — interleaved control, ascending order

Same windows in the opposite order, each preceded by a control run at
0.06251 s (f = 0.30, a window that can hold a 4200-sample arrival with 30 %
clearance and therefore cannot pin a peak). 33 runs.

**The control reported 33.0833 ms in 8 of its 11 runs**, tracking the test rows
in the same block. Both directions of the sweep produce the same behaviour.

> **The window is exonerated.** The one-period shift is time-varying and has
> nothing to do with edge proximity, window size, or the sweep direction.
> Whatever it is, #350's constants are not it.

## Pass 3 — the refusal path, and what the margin actually buys

Passes 1–2 never pinned a peak: whenever the window was too small to hold a
4200-sample arrival, the rig happened to be in its 3176-sample state, which
still fitted. These three windows (0.03300, 0.03200, 0.03000 s) cannot hold
*either* state, so the arrival is outside the window by construction. Each was
run at margin 0.0 and at the shipped 0.10, with a large-window control first.

| window | margin 0.0 | margin 0.10 |
|---|---|---|
| 0.03300 | refused — "2 readings disagree, not a period multiple" | refused — peak at 6318 of 6336, within 317 of the edge |
| 0.03200 | refused — peak at 6143 of 6144, within 0 of the edge | refused — within 307 of the edge |
| 0.03000 | refused — "2 readings disagree, not a period multiple" | refused — peak at 5690 of 5760, within 288 of the edge |

Controls read 43.7500 ms throughout, so the rig was in its true state and the
refusals are the window's doing, not the shift's.

**#340 AC4's refusal fires on hardware and names the right things** — measured
peak position, half-width, and the fact that the arrival is likely outside the
window. No number was returned in any of the six out-of-window runs.

**With the margin off, the pinned peak was still caught** — by #347's
disagreement rule, because a pinned peak lands in a different place each
lifetime and the delta is not a period multiple. The margin is not the only
net. It is the net that *names the cause*; the disagreement rule only says the
two readings differ.

---

## What this says about #350's two constants

**`TAU_EDGE_MARGIN_FRAC = 0.10` is safe but is not derived from anything
measured here.** The measured acceptance side is good to **f ≈ 0.01** — a τ one
percent of the half-window from the edge was located exactly, twice, with no
amplitude or SNR penalty. So the margin is roughly 10× larger than this rig's
electrical noise floor requires, and it costs 5.01 ms of measurable ceiling at
96 kHz (50.01 ms → 44.99 ms).

**Recommendation: keep 0.10, and record why.** Nothing measured argues for
lowering it, the acceptance side has 10× of headroom over what it costs, and
the margin is what turns an out-of-window arrival into a named refusal rather
than an unexplained disagreement. If the ceiling ever binds, **raise
`TAU_MIN_HALF_WINDOW_S`** — `TAU_TAIL_S = 150 ms` allows a half-window up to
75 ms, and the new runtime guard refuses anything larger rather than gating
past the end of the capture — rather than lowering the margin.

**What is still unmeasured, and in which direction it points:** this was an
electrical loopback with a 46–83 dB peak-to-floor ratio. An acoustic path at
30 dB SNR was not run. The gap is one-directional — a noisier path can only
make edge detection *worse*, never better — so 0.10 remains an upper bound
that is safe for the acoustic case, and the 1 % figure above must not be read
as a licence to shrink the margin for a microphone measurement.

## The finding that outranks #350

**`calibrate` reports a τ one full JACK period short, corroborated, in 42 of
97 runs (43 %) at this rig's normal configuration.** Every one of those 42 runs
printed "2 readings agree" and stored `agreement_count: 2`.

Counts by pass, for windows that could hold either state:

| pass | runs | 43.7500 ms | 33.0833 ms |
|---|---|---|---|
| 1 (descending sweep) | 42 | 24 | 18 |
| 2 (interleaved control) | 33 | 10 | 23 |
| 3 (controls only) | 3 | 3 | 0 |
| discriminator | 15 | 15 | 0 |
| watcher | 4 | 3 | 1 |
| **total** | **97** | **55** | **42** |

**Not one run in those 97 was refused for disagreement.** If the shift were an
independent per-lifetime draw at the observed 43 % rate, roughly half of all
runs would have had one reading of each kind and been refused. Zero were. The
state is **sticky over seconds** — long enough that `measure_tau_twice`'s two
lifetimes, about a second apart, always land in the same state.

That is the exact failure `AGENTS.md`'s "agreement rules need independent
windows" describes: two readings a second apart are not independent samples of
a state that persists for tens of seconds, so their agreement measures the
state's persistence, not the reading's correctness.

**Cross-check.** A watcher ran `ac calibrate` until the 33.08 state appeared
(iteration 3), then immediately ran `it_loopback_ir` on the same ports — an
independent code path with its own daemon and its own client lifetime. It read
`peak_offset +4200 samples = +43.7500 ms, snr 36.71 dB`, and a `calibrate` run
straight after also read 43.7500 ms. The state had flipped back within ~3 s.
So this is **per-client-lifetime**, consistent with the known one-period jump,
and the cross-check neither convicts nor clears `measure_tau`'s own maths: it
says the two tools disagreed because they registered different clients, which
is precisely the hazard.

Filed as its own issue. It blocks trusting any stored τ, including the
`agreement_count: 2` entry recorded in the Track A file earlier the same
evening.

## Rig state left behind

No emission in progress. No `ac-daemon` running. Clock `AutoSync`. Loopback
cable `playback_2` → `capture_4` still patched. Speaker powered, not driven.
Mic still taped at 3.000 m on axis. No build tree on the host — binaries only,
in `~/rig-2026-08-22/bin-350/`, hashes above. Logs and the three sweep scripts
in `~/rig-2026-08-22/logs-350/` and `~/rig-2026-08-22/`.

`~/rig-2026-08-22/home-350/.config/ac/cal.json` accumulated one `tau_history`
entry per run of this session — **it contains both states, all labelled
`agreement_count: 2`**, and is worth keeping as evidence for the issue above.
