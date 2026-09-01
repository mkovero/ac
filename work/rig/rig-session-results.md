# rig-session-results — 2026-07-28, 192.168.9.25

Executes `$AC_HOME/handoff/handoff-rig-session.md`. Rig: RME Babyface Pro, 96 kHz native both
directions (`hw_params` rate 96000, period 1024, buffer 4096 — **no
resampling anywhere in the path**), PipeWire graph `clock.rate 96000`,
`clock.quantum 1024`. Build under test: `main` @ `bd40ed4` (#218 + #222),
binaries installed 21:34, daemon restarted 21:34.

**Drive level: −30 dBFS nominal pink** (≈ −25.8 dBFS instantaneous peak),
authorised for this session by the operator, above the standing −40 dBFS
electrical ceiling because the acoustic path needs the mic SNR. Emission was
stopped between every run.

## Wiring as actually used

The handoff's wiring is AN1→IN3 with mics on IN1/IN2. What is physically
there:

- **playback_1 (AN1) → capture_3 (IN3)** — electrical loopback, the reference.
- **playback_5 (ADAT left) → external converter → loudspeaker** — the
  measurement stimulus.
- **capture_1 (IN1)** — measurement microphone, <1 m from the speaker.
- **capture_2 (IN2)** — *not* a measurement mic (operator). Used only as a
  second pair to exercise fan-out.

The installed daemon has **no separate reference-output leg** — the start
reply carries no `ref_out_port` and frames carry no `conn_tags`, both of which
postdate it. So the reference leg was fed by wiring the stimulus to both
outputs. Two ways were used, and the difference matters:

1. `jack_connect` the transfer worker's own `out` port to playback_1 after
   session start. Lands at ~0.29 s — **too late**, see below.
2. A standalone `generate_pink` worker on `channels: [0, 4]`, started before
   the transfer session. This is what all reported results use.

**The per-pair delay is estimated once when the capture rings first fill and
cached for the session.** With method 1 the rings fill before the reference
leg exists, so the cached delay is measured against silence and is garbage
for the rest of the session — the same position gave 18.09 / 25.22 / 25.41 ms
across three sessions. With method 2 the stimulus is already flowing at
sample zero and the same position repeats to **one sample**. Any future rig
work should drive the stimulus from a separate worker for this reason.

---

## Run 1 — delay tracks distance

Mic on IN1, four to eight sessions per position, each a fresh transfer
session (the delay is cached per session, so a new session per position is
required).

| position | delay (ms) | sessions | spread |
|---|---|---|---|
| D (baseline, <1 m) | **4.5938** | 8 | 0.0105 ms — one sample |
| E (+34 cm, axial) | **5.44 / 5.92** (physical locks) | 8 | see below |

**Δ = +1.08 to +1.16 ms for 34 cm, against a 1.00 ms prediction.** Hand
placement of ±5 cm is ±0.15 ms, which covers the difference. **Delay tracks
distance — the physics gate passes.**

The absolute value is small because both legs are *captured*: playback and
capture buffer latency is common-mode and cancels in the correlation, leaving
the external converter's latency plus flight time. This is also why the
earlier ~25 ms readings could be recognised as wrong.

### The finding this run actually produced

At position E, **5 of 8 sessions locked to a non-physical delay** — 22.78,
30.34, 30.45, 30.43 ms (30 ms is ~10 m at a mic under 1.5 m away) and 4.18 ms
(below the baseline, after moving *away*). Earlier, at a more distant and
off-axis position, sessions split cleanly into two clusters 14.5 ms apart.

`estimate_delay_samples` takes the **maximum** of the cross-correlation.
In a room the direct-sound peak is not always the maximum — a reflection
wins whenever the direct-to-reverberant ratio is poor, which is exactly what
distance and off-axis placement produce. Headless tests feed one unambiguous
peak, so nothing upstream can see this.

**Top-stage coherence separates the two cases perfectly:**

| lock at position E | stage 0 (2–48 kHz) | stage 1 | stage 2 |
|---|---|---|---|
| 4.18 / 5.44 / 5.92 ms (physical) | **0.67 / 0.74 / 0.75** | 0.96–0.97 | 0.92–0.93 |
| 22.8 / 30.3 / 30.4 ms (spurious) | **0.05–0.06** | 0.77–0.87 | 0.93–0.95 |

Suggested fixes, in order of cheapness: reject a candidate whose top-stage
coherence sits at the 1/N floor and re-estimate; restrict the search to a
physically plausible window; prefer the earliest prominent peak over the
global maximum. Note the low stage is nearly blind to the error (0.93 either
way) — a delay fault is only visible at HF.

The gain sweep below shows electrical SNR feeds this too, not only the
direct-to-reverberant ratio: at fixed geometry, valid locks went 4/6 → 5/6 →
6/6 across 20 dB of input gain. A prominence threshold on the correlation
peak would cover both causes.

## Run 2 — criterion 10, the #208 recurrence check

Stimulus: a repeatable level step, 6 s on / 15 s off, three cycles, gated two
ways. Watched transfer magnitude.

**First form — gating the whole generator** (both legs die):

| phase | mic peak | stage 2 magnitude | coherence |
|---|---|---|---|
| on | −30 dBFS | −16 dB | 0.93 |
| off | −44 dBFS | **+64 dB** | 0.23 |
| on again | −30 dBFS | −16 dB | 0.93 within ~1 s |

The +64 dB is H1 dividing by a silent reference, not a response. Recorded
because it is a trap: it is a large, repeatable magnitude excursion driven
entirely by the reference leg going away.

**Second form — gating only the speaker leg**, reference left hot
(`jack_disconnect` on the playback_5 edge). This is the well-conditioned
test and models a DUT going quiet:

- Stage 2 magnitude decays **monotonically** −15.9 → −37 dB over ~1.8 s and
  stays flat for the full 15 s.
- Coherence 0.94 → 0.15, monotone.
- Recovery on re-connect is monotone, ~2 s.
- Identical on every cycle. **No repeats, no episodes at 3–5 s spacing.**

**A/B against the control.** Control = `cda40ef`, `main` immediately before
the #218 merge, built in an isolated worktree (`~/ac-ctrl`,
`CARGO_TARGET_DIR=~/target-ctrl`) and run on ports 25556/25557 alongside the
system daemon. Frames confirm it is the old path (no `mtw` object at all).
Same stimulus source, same gate:

- Decay −15.5 → −37 dB by 8 s, coherence → 0.2, then flat noise.
- **Also no recurrence.**

**So the A/B has no positive control.** The new build shows no repeats, but
neither does the old one, so this run cannot say the ladder removed them.
`cda40ef` is post-#207 (splice), post-#215 and post-#220 (ac-view drain) —
the handoff asked for `main` before the #218 merge, which is what this is,
but that baseline already contains every repair that plausibly caused the
symptom.

What this does establish: **the triple recurrence is not present in the
daemon's transfer frames on either build.** If it still reproduces, it is in
the view layer, which this measurement does not cover — frames were read
directly off ZMQ, not off the screen.

## Run 3 — alignment on a real delay

Mic at 5.917 ms (real acoustic delay, not synthesised), settled frames only:

| stage | band | coherence (median / min) | magnitude |
|---|---|---|---|
| 0 | 2064–47869 Hz | **0.755 / 0.715** | −25.45 dB |
| 1 | 258–2035 Hz | 0.970 / 0.965 | −14.01 dB |
| 2 | 20–254 Hz | 0.934 / 0.923 | −15.81 dB |

The top stage's window is 42.7 ms at 96 kHz; without working alignment its
coherence collapses. It does not — 0.755 with a floor of 0.715 over 539
frames. That 0.755 is room reverberation, not an alignment failure, and the
comparison in Run 1 proves the point: the *same* stage reads 0.05 when the
delay lock is wrong. **#216's general half holds on hardware.**

## Run 4 — the documented coherence step at the crossovers

`docs/design/design-mtw-ladder.md` records ~0.05 at the crossovers, measured headlessly
at γ² = 0.5. Measured live, median over frames in each window:

| window | xo 2064.53 Hz (stage 1→0) | xo 258.05 Hz (stage 2→1) |
|---|---|---|
| 0–3 s | +0.0909 | −0.1427 |
| 3–10 s | +0.0880 | −0.1139 |
| 10–25 s | +0.0913 | −0.1014 |
| 25 s+ | +0.0907 | −0.0914 |

- **Upper crossover: present, same order as documented, and stationary** —
  ±0.003 over 40 s. It does not move as the ladder warms.
- **Lower crossover: negative** (coherence is *higher* above it) **and it
  drifts**, −0.143 → −0.091 over 25 s, still creeping at the end.

The drift is consistent with stage 2's own settling — it has the longest
window (2.5 s) and the fewest averages early on, so a difference taken
against stage 1 keeps moving while stage 2 converges. Worth confirming
against the design's intent before treating it as a defect. Note also that
the live γ² is ~0.97 near the upper crossover, not the 0.5 the documented
0.05 was measured at, so the two numbers are not directly comparable.

## Run 5 — two pairs

Both pairs against the same reference (capture_3), one session:

| pair | delay | stage 0 coh | stage 1 coh | stage 2 coh | stage 2 magnitude |
|---|---|---|---|---|---|
| meas 0 (mic) | 5.917 ms | 0.755 | 0.970 | 0.934 | −15.81 dB |
| meas 1 (not a mic) | 494.167 ms | 0.050 | 0.093 | 0.159 | −60.91 dB |

Different delays, different coherences, magnitudes 45 dB apart. **No
coupling and no leakage between pairs** — the failure this run looks for does
not occur. Input 2 is not a measurement microphone, so its numbers carry no
acoustic meaning; as a fan-out test it still does the job, and its 494 ms
delay is another instance of the Run 1 peak-picking problem on an
uncorrelated input.

## Run 6 — settling on the acoustic path

Time from the first frame until each rung reports settled, pair 0:

| stage | acoustic (this run) | electrical (prior) | analytic |
|---|---|---|---|
| 0 | **0.079 s** | 0.070 | 0.107 |
| 1 | **0.828 s** | 0.824 | 0.853 |
| 2 | **2.532 s** | 2.541 | 2.560 |

Within 9 ms / 4 ms / 9 ms of the electrical measurements. **Settling behaves
identically on the acoustic path.** The curve fills top-down: stage 0 alone
at first (219 columns, 2.06–47.9 kHz), reaching all 504 columns and 20.5 Hz
once stage 2 lands.

## Run 7 — input gain sweep (added after the six runs)

Not in the handoff. Run to settle whether stage 0's 0.755 is limited by the
room or by preamp noise, which decides whether Run 3's number is as good as
the rig can give. `1-AN1 Capture Volume` (numid 301) is software-settable,
0–65, so the sweep needed no hardware handling. Six sessions per setting,
filtered to valid delay locks (4–8 ms), −30 dBFS drive throughout.

| input gain | mic peak | stage 0 | stage 1 | stage 2 | valid locks |
|---|---|---|---|---|---|
| 36 | −35.2 dBFS | **0.710** | 0.939 | 0.847 | 4/6 |
| 46 | −25.1 dBFS | **0.714** | 0.938 | 0.834 | 5/6 |
| 56 | −15.5 dBFS | **0.716** | 0.939 | 0.834 | 6/6 |

**Stage 0 coherence is flat across 20 dB of gain** — 0.710 → 0.716, a spread
smaller than the frame-to-frame noise. The HF deficit is **reverberation-
limited, not preamp-noise-limited**. Run 3's 0.755 is the room, and gain
cannot buy it back; only a closer mic or a deader path would. Stages 1 and 2
are likewise flat.

The delay locks are the surprise. Geometry, room and direct-to-reverberant
ratio were identical across the three settings — only electrical SNR changed
— yet lock reliability tracked it monotonically. See Run 1.

Two measurement caveats: the gain scale is not linear in dB at the bottom of
its range (36→46 measured +10.1 dB, 46→56 +10.4 dB, but a single earlier run
at 36 read −30.5 dBFS where this batch read −35.2), and those peaks are
measuring room noise, which varies run to run by a few dB.

**Mixer state.** Only `numid=301` was written. `PCM-AN1-AN1 Playback`
(numid 145, 65535) and `01-AN1 Playback` (numid 289, 37449) were read before
and after and were unchanged — the output path was never touched. Input gain
was restored to its session-start value of 36; 48V left on, PAD left off.

## Run 8 — silent-start lock, the field symptom reproduced (→ #226)

Not in the handoff. Run to test whether the cached delay explains the
operator's "LF legit, HF dead, gain and mic position change nothing".

A `drivable` session — what `ac transfer` / `ac-view` opens: output ports
connected, silent until `set_drive` — started with no stimulus. Pink noise at
−30 dBFS arrived 4 s later. Reference leg present and working throughout.
Validated delay at this position: 5.92 ms.

Locked delay: **−0.156 ms**, estimated against silence.

| t | mic peak | stage 0 | stage 1 | stage 2 |
|---|---|---|---|---|
| 0–2 s (silent) | −44 dBFS | 0.05 | 0.10 | 0.24 |
| 4 s (stimulus on) | −27 dBFS | **0.112** | 0.938 | 0.921 |
| 24 s | −27 dBFS | **0.119** | 0.937 | 0.960 |

Stages 1 and 2 recover the moment stimulus arrives; **stage 0 never does**,
for the remaining 20 s. Same position with stimulus present at session start:
0.755.

The symptom is fully explained, reproduces in 25 s, and does **not** require
the reference-output bug (#225) to occur — the reference was live here.

## Run 9 — Issue F repeat, 65 s stationary (→ nothing filed)

`n_blocks` is **constant at 4** for the whole run — blocks per stage are held
uniform by design (`handlers/transfer.rs:240`), so there is no 1/N
convergence and the average-accumulation explanation for Run 4's drift is
wrong.

The drift itself does not reproduce. Thirteen 5 s windows, 258 Hz step:
0.130, 0.105, 0.105, 0.079, 0.078, 0.103, 0.107, 0.057, 0.144, 0.089, 0.102,
0.088, 0.115 — scatter of ±0.04 with no trend. And the sign is opposite to
Run 4's at the same position (positive here, negative there), which means the
metric is dominated by room response: three columns either side of 258 Hz is
about 1/16 octave, where modal structure lives.

Upper crossover in this run: +0.05…+0.084, consistent with Run 4's +0.091.

**Revised conclusion for Run 4:** the documented step is confirmed at the
upper crossover and is stationary. The lower-crossover drift is a metric
artifact, not a finding.

---

## Cross-cutting notes

- **`conn_tags` is absent** from every frame this daemon publishes, so the
  #205 drive-path check was unavailable all session. Per the field's own
  contract that must read as *unknown*, never as healthy.
- **No interface monitoring path exists**, checked two ways after the
  operator raised it as a possible cause of a static delay component.
  Driving AN1 only (speaker silent) leaves the mic at coherence 0.05/0.12/0.18
  — no electrical copy. Driving ADAT only leaves capture_3 at −96.2 dBFS —
  digital silence. The card's mixer matrix agrees: the only non-zero routes
  are `PCM-AN1→AN1`, `IN3→PH3`, `IN4→PH4`, `AS1/AS2→PH3/PH4`; no input is
  routed to any output in use.
- **Clock source is `AutoSync`, not `Internal`.** No drift was observed —
  eight sessions over a minute agreed to one sample, which a free-running
  converter would not do. ~~Setting it to Internal would make that
  airtight.~~ **It would not — Internal breaks the stimulus path** (session 2,
  2026-08-03): the external master clocks the card over ADAT, and ADAT carries
  `playback_5`. The clock stays `AutoSync` (`numid=320` = 0), which is what
  this observation was made under. See `work/rig/rig-session-2-results.md:21`.
- Mic level −30 dBFS peak against a reference at −14.5 dBFS. The mismatch is
  harmless: H1 and coherence are ratios — confirmed empirically by Run 7,
  where 20 dB of input gain moved stage 0 coherence by 0.006.
- **The #208 recurrence is agreed closed** (operator, after Run 2): not
  present in the frames on either build, and not reproducing on the rig.

## Rig state left behind

Left in place, delete when no longer wanted:

- `~/ac-ctrl` — git worktree at `cda40ef`.
- `~/target-ctrl` — its build directory (~870 MB).
- A control `ac-daemon` on ports 25556/25557 (`~/ctrl-daemon.log`).

The system daemon, the system binaries and `~/.config/ac/config.json` were
not touched. The card's mixer is back to its session-start state (Run 7). No
emission is in progress.
