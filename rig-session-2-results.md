# rig-session-2-results — 2026-08-03, 192.168.9.25

Executes `handoff-rig-session-2.md`. Rig: RME Babyface Pro, 96 kHz native both
directions, PipeWire `clock.rate 96000` / `clock.quantum 1024`. Build under
test: `main` @ `7f0dd5e` (#233 + #232 + #234), built on the rig and installed
20:14, daemon restarted against it.

**Drive level: −30 dBFS nominal pink**, authorised by the operator for this
session, above the standing −40 dBFS electrical ceiling because the acoustic
path needs the mic SNR. The daemon ran under an isolated `HOME`
(`~/rig2-home`) with `drive_max_dbfs: -30.0` so the clamp was enforced
server-side, not just in the request. Emission was stopped between runs.

---

## What the handoff got wrong before anything was measured

Three of the pre-flight instructions did not survive contact with the rig.
Recorded first because each one would have produced a false reading.

### The clock must stay `AutoSync` — do not set `Internal`

The handoff says "Set the interface clock to `Internal`… it costs nothing to
remove." It costs the session. **An external master clock provides timing over
ADAT, and ADAT is the link carrying playback_5 — the stimulus leg.** Setting
the card to Internal makes it master and the speaker path stops working.
Operator, during this session, after the change was made and reverted:

> "amixer -c0 cset numid=320 0 needs to be like this, Autosync, Internal clock
> will not work as master clock provides timing through ADAT which is used for
> playback_5."

The no-drift evidence from 2026-07-28 (eight sessions agreeing to one sample)
was obtained under AutoSync and needs no change. `numid=320` was left at `0`.

### The reference leg has moved — AN1→IN3 is gone

The handoff's wiring (`playback_1 → capture_3`) is not what is on the rig. A
routing probe at −40 dBFS, one playback channel at a time, reading the raw
capture peaks:

| capture | silent | pb_1 | pb_5 | pb_6 | pb_2 |
|---|---|---|---|---|---|
| capture_1 (mic) | −37.4 | −38.3 | −36.2 | −35.7 | −37.9 |
| capture_2 | −40.6 | −32.3 | −41.8 | −37.3 | −37.7 |
| capture_3 (**config's ref**) | −92.5 | −93.4 | −92.9 | −93.2 | −93.3 |
| capture_4 | −92.7 | −92.3 | −92.8 | −92.8 | **−30.6** |

capture_3 is digitally silent under every output. The electrical loopback is
now **playback_2 (AN2) → capture_4 (IN4)**: −93.0 dBFS silent, −19.2 dBFS
driven at −30. `PCM-AN1→AN1` and `Mic-AN1→AN1` are both muted at the mixer,
which is why the old leg is dead.

The speaker leg is unchanged (playback_5), confirmed by a 20 s median A/B
rather than max-of-peaks, because the room floor moved 10 dB between probes:

| | silent | pink pb_5 | pink pb_6 | silent again |
|---|---|---|---|---|
| mic peak, median | −41.45 | **−32.82** | −35.15 | −42.26 |

Session config was repointed to `reference_channel: 3`,
`reference_output_channel: 1`.

### `install.sh` had not shipped what was on `main` — and size+mtime says it had

The handoff says to check timestamps. Timestamps pass and are wrong. The
installed `ac-daemon` and `ac-view` were **byte-identical in size** to a fresh
build of `7f0dd5e` and dated the same day, but differed by sha256. Only `ac`
matched. Verify by hash; size and mtime cannot see this.

---

## Run A — #233, the reference leg: **pass**

Drivable session, no `generate_pink` worker, no hand-patching, drive started
through `set_drive` with a 250 ms keepalive.

- `ref_peak_dbfs` median **−22.17 dBFS** — off the −96 floor.
- The start reply carries no warning, and the JACK graph shows
  `ac-daemon:out → Babyface Pro Pro:playback_5` **and** `playback_2`.

The reference stimulus leaves on its own playback index without the operator
patching anything. #225 is fixed on hardware.

As the handoff predicted, this did not fix the top end: the session refused to
lock (prominence 21.1 against a gate of 24) and therefore never built a
ladder. That is #226 plus the finding below, not a failure of #233.

---

## Run B — #228, the six states: three pass, two cannot fire

Scored on the real display (`ac-view --transfer` on the rig's X session,
NVIDIA, screenshots attached), with the ZMQ frame that produced each picture
captured alongside it so no screenshot is scored on its own.

| induce | expect | result |
|---|---|---|
| session idle, drive off | *(nothing)* | *(nothing)* ✓ |
| disconnect the reference edge, drive on | `NO REFERENCE` | ✓ "reference leg silent — check the output patch" |
| unplug the mic, drive on | `NO SIGNAL` | ✓ "measurement leg silent — check the mic, the DUT, and the input" |
| feed the two legs from different sources | `CHECK ROUTING` | **✗ did not fire** |
| force a bad lock | `LOST LOCK` | **✗ structurally unreachable** |
| re-lock | `LOCK ACQUIRED` | ✓ — and it confirmed a wrong lock |

The `conn_tags` row was correctly dropped from the plan by #236 before the
session; it has zero occurrences in `ac-rs/` and nothing on screen answers to
it.

### `LOST LOCK` / `NO LOCK` cannot fire — the states exist for a case they cannot observe

`FaultState::update` gates every lock-derived state on
`refusing = frame.settled && frame.delay_locked == Some(false)`, and
`settled` is `frame.mtw.is_some()`. But the daemon builds the ladder only
once a lock exists (`handlers/transfer.rs`: `let Some(delay) = pair_delays[i]
else { continue }`), and `delay_locked` is `delay_opt.is_some()` where
`pair_delays[i]` is never cleared once set.

So for the whole life of a session:

- **before a lock** — no `mtw`, so `settled` is false, so `refusing` is false;
- **after a lock** — `delay_locked` is true forever, so `refusing` is false.

There is no reachable state in which a live session reports a refusal to the
indicator. Observed directly: a session driving at −30 dBFS with both legs
live and the estimator refusing for 14+ s renders a **completely blank
window** — no trace, no indicator, delay reading `0.00 ms` (`b2-driving-refusal.png`).
This is the exact failure #228 was written to end, and it is the failure the
handoff predicted would be *invisible without* #228.

### `CHECK ROUTING` needs every column dead, and never gets it

`coherence_dead` requires **all** ladder columns below 0.5. With the
measurement leg hearing only room noise and the reference carrying electrical
pink — genuinely unrelated sources — **22 of 504 columns still cleared 0.5**
(max 0.844, concentrated at 37–71 Hz). The rule is an all-or-nothing test over
hundreds of columns; a handful of low-frequency columns always survive.

Two weaker inductions (reference off an unrelated analog input; reference off
an independent pink generator) also failed to trip it, for the same reason.

### `LOCK ACQUIRED` fires, and it confirmed a −826 ms lock

The transient renders correctly once a session actually transitions
false→true. Forcing that transition exposed something worse than a missing
indicator (`b11-01.png`):

```
t=0.05  locked False  prom  3.40  peak_lag -7457
t=0.28  locked True   prom 31.83  peak_lag   434   delay_ms -826.35
```

At t=0.28 s — 0.14 s after drive started — the estimator accepted a lock of
**−826.35 ms**, displayed as `-826.35 ms (-283.44 m)`. Its own evidence puts
the strongest peak at lag 434 = **+4.52 ms**, the physically correct arrival.
The "earliest peak within `DIRECT_PEAK_FRACTION` of the strongest" rule
scanned from −1 s upward and took an early ripple thrown up by the stimulus
**onset transient**, since the reference leg had only just come alive.

A negative delay means the microphone led the electrical reference, which this
rig cannot do. The value was then cached for the session, and the indicator
painted `LOCK ACQUIRED` over it — a confirmation, in the register reserved for
things going right.

---

## Run C — #227, the prominence threshold

One fresh session per measurement point (the lock is cached per session), with
the stimulus on a standalone `generate_pink` worker on channels `[1, 4]`
started **before** the session, so the rings never fill against silence.

### The captures

`delay_evidence` is published on every frame as designed, and repeating it
every frame is what let a subscriber that attached late read it at all.

### Position 1 — ~1 m on axis, the "known-good baseline": 1 lock in 12 sessions

Six sessions at input gain 36 and six at 56, −30 dBFS throughout.

| gain | mic peak | sessions locked | prominence min / median / max |
|---|---|---|---|
| 36 | −32.4 dBFS | **1 / 6** | 7.13 / 18.14 / 25.80 |
| 56 | −12.6 dBFS | **0 / 6** | 10.47 / 18.56 / 23.95 |

**The shipped gate of 24 refuses the baseline position.** The handoff asked
whether 12 refuses valid locks in a live room; the answer at the *best*
position is that the derived gate refuses almost all of them, and the
noise-derived floor of 12 sits inside the range a real 1 m acoustic path
produces (7.1–25.8), not below it.

**Gain buys nothing.** 20 dB of input gain moved the prominence median by
0.4. Run 7's finding that lock reliability tracked electrical SNR at fixed
geometry is gone, as the handoff expected — but it is gone because nothing
locks, not because everything does.

The one session that did lock (prominence 25.80) locked to 4.146 ms and
measured cleanly — coherence 0.887 LF / 0.979 MF / **0.877 HF** — so the
eleven refusals were refusing a measurement that would have been good.

The candidate lists explain the low prominence. At this position the direct
arrival at 5.8–6.3 ms competes with a reflection cluster at ~30 ms sitting
only **1.3 dB below it**, and `peak_lag` alternates between the two from one
attempt to the next:

| lag | time | value | rel. peak |
|---|---|---|---|
| 609 | 6.34 ms | 0.1995 | 0.00 dB |
| 558 | 5.81 ms | 0.1923 | −0.32 dB |
| 2917 | **30.39 ms** | 0.1716 | **−1.31 dB** |

That is the case `DIRECT_PEAK_FRACTION`'s 6 dB window is built for, and the
earliest-peak rule would resolve it correctly — if the prominence gate let it
run at all.

### Position 3 — ~3 m on axis: 7 of 7 lock, 2 of them wrong

Geometry confirmed by the measurement itself: the direct arrival moved from
~6 ms to 11.3 ms, +5.4 ms ≈ 1.85 m of extra flight.

| session | prominence | locked delay | verdict |
|---|---|---|---|
| check | 24.19 | 11.34 ms | correct |
| 1 | 24.07 | 11.26 ms | correct |
| 2 | 24.99 | 11.34 ms | correct |
| 3 | 27.81 | 11.34 ms | correct |
| 4 | 24.20 | **18.43 ms** | **wrong** |
| 5 | 24.03 | 11.34 ms | correct |
| 6 | 25.68 | **14.00 ms** | **wrong** |

Prominence 13.6–27.8, median 21.8 — sitting *on* the gate, so which side a
session lands on is close to a coin toss. **A refusal here is acceptable; two
wrong locks in seven are not**, and the handoff names that as the one
unacceptable outcome.

Where it locks correctly, #227's earliest-peak rule is doing real work: the
global maximum sat at 19.4–26.9 ms and the accepted lock was 11.34 ms. The
rule is right; the gate in front of it is not, and its rejection of late
arrivals is incomplete.

Ladder coherence at this position, settled: stage 0 **0.601**, stage 1 0.861,
stage 2 0.840 — lower than the 0.755 at 1 m, as distance predicts.

### The captures cannot answer the question they were taken for

**In every position-3 session, the lag the estimator locked to is absent from
its own candidate list.**

| session | locked lag | candidate span |
|---|---|---|
| 1 | 1081 (11.26 ms) | 1815–3335 (18.91–34.74 ms) |
| 4 | 1769 (18.43 ms) | 1815–2628 (18.91–27.38 ms) |
| 6 | 1344 (14.00 ms) | 1814–2629 (18.90–27.39 ms) |

`MAX_CANDIDATES = 32` keeps the 32 **strongest** peaks. At 3 m the direct
arrival is weaker than 32 peaks of the 19–35 ms reverberant cluster, all
within the 12 dB capture window, so the capture keeps the cluster and discards
the arrival. Its doc comment reasons that ranking by strength "keeps the
arrivals, which outrank the ripple by construction" — on a real path at 3 m,
the direct arrival does not outrank the reverberation.

The consequence is the one that matters for the plan: **`DIRECT_PEAK_FRACTION`
cannot be set offline from these captures**, because they cannot reproduce the
decision the estimator made. Replaying the accept rule over the recorded
candidates returns a *different* lag than the daemon chose, at every constant
value. Offline tuning was the entire reason Run C exists.

At position 1 the truncation bites less often but still bites: the one
locking session there locked to lag 398, also absent from its own list.

### What the replay can still say

The lock/refuse *rate* does not depend on the candidate list, only on
prominence, so this part of the replay stands. Over 114 attempts at position 1
(gain 56):

| floor | fraction | gate | attempts locking |
|---|---|---|---|
| 12.0 | 0.500 | **24.0** | **0.0%** ← shipped |
| 12.0 | 0.707 | 17.0 | 72.8% |
| 12.0 | 0.800 | 15.0 | 93.0% |
| 12.0 | 0.900 | 13.3 | 97.4% |

The handoff's own suggestion — tighten the fraction rather than lower the
floor, since it lowers the gate proportionally without moving anything closer
to the noise — is the right lever and the numbers support it. But the fraction
cannot be *settled* from this data for the reason above, and at 0.9 the
window starts excluding a direct arrival that sits 1.2–1.5 dB below the
strongest peak, which is where this room puts it.

### Positions 2, 4 and 5 — not run

The session stopped after position 3. Positions 2 (1 m off axis), 4 (3 m off
axis) and 5 (near a wall) still have no data, so `NOISE_FLOOR_PROMINENCE` has
no measurement from the marginal end. Given the truncation defect above, they
should not be run until the capture can carry the arrival it locked to —
otherwise they produce the same unusable evidence at greater cost.

---

## Run D — criterion 10's positive control: not run

Optional in the handoff, and dropped for time in favour of finishing Run C.
The gap it addresses stays open and is unchanged.

---

## Cross-cutting notes

- **The frame rate is not what `ZMQ.md` documents.** Sessions published ~18
  frames/s (737 frames in 40 s), against the documented "one frame per
  iteration, ≈ 2.5 s at 48 kHz". Not investigated; recorded because every
  per-frame cost in the protocol was reasoned against the slower figure.
- **`delay_evidence` repeated every frame earns its keep.** Every capture in
  this session came from a subscriber that attached after the lock attempt.
- **The room floor moves by ~10 dB.** Silent-baseline mic peaks ranged −41.5
  to −27.7 dBFS median across the evening. Any level comparison here needs a
  contemporaneous baseline, not one from earlier in the session.
- **Stage 0 coherence:** 0.877 (legacy band, 2 kHz+) at 1 m on the one good
  lock; 0.601 (ladder stage 0) at 3 m. Both consistent with a
  reverberation-limited path, as recorded last session.

## Rig state left behind

- **Clock left at `AutoSync`** (`numid=320 = 0`) — required, see above.
- **Mic preamp `numid=301` left at 36**, the session baseline. It was found at
  **0** at the start of this session, not 36.
- 48 V left on, PAD left off. No mixer route was written by this session.
- **No emission in progress.** All workers stopped, `ac-view` closed.
- `/usr/local/bin/{ac,ac-daemon,ac-view}` now match a build of `7f0dd5e`,
  verified by sha256.
- A daemon runs on the default ports under `HOME=/home/mui/rig2-home` with
  `drive_max_dbfs: -30.0`, `reference_channel: 3`,
  `reference_output_channel: 1`. `~/rig2b-home` is a variant with
  `input_channel: 3`, used only to force a lock for Run B.
- **`~/.config/ac/config.json` was not touched, and it is now wrong for this
  rig**: `reference_channel: 2` points at capture_3, which is silent, and
  `reference_output_channel` is unset. Anything run as the operator's own user
  will report `NO REFERENCE`. Fixing it is a one-line change but it is the
  operator's file, so it was left alone.
- Build directory `~/target-rig2` (~1 GB), screenshots in `~/runB/`.

## What this session says should happen next

Ordered by what blocks what.

1. **The candidate capture must keep the lag it locked to.** Everything else
   in Run C is unmeasurable until it does. Keeping the strongest 32 is the
   wrong rule on a reverberant path; at minimum the accepted lag and the
   global peak belong in the list unconditionally.
2. **`LOST LOCK`/`NO LOCK` need a reachable path.** Either the daemon
   publishes ladder columns (or another settling signal) while refusing, or
   `settled` stops being the gate. As shipped, a refusing session is a blank
   screen with no indicator — the state #228 exists to prevent.
3. **The estimator must not accept a negative lag**, and should not attempt a
   lock across a stimulus onset. A −826 ms lock was accepted at prominence
   31.8 and confirmed on screen as `LOCK ACQUIRED`.
4. **The gate needs setting from measured data, not noise statistics.** 24
   refuses the baseline position; 12 sits inside the range a real 1 m path
   produces. This is blocked on (1).
5. **`CHECK ROUTING`'s all-columns rule needs a fraction instead.** 22 of 504
   columns cleared 0.5 on genuinely unrelated legs.

