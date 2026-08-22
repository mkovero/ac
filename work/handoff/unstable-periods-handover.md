# Unstable round-trip latency on the RME Babyface Pro rig — handover

**Written:** 2026-08-22, from the `ac` measurement project.
**For:** whoever picks this up in another repo (`rme-re` was the suspected
starting point — see "Where I would look first", which argues against it).
**Self-contained:** nothing here requires the `ac` codebase to understand.
Reproduction is given both with and without `ac` binaries.
**Expires:** when the mechanism is identified. Nothing in this file is a
conclusion about a fix.

---

## The claim, in one paragraph

On this machine the **measured** audio round trip through a physical cable
loopback takes one of exactly two values, differing by exactly one JACK period
(1024 samples at 96 kHz = 10.67 ms), and which one you get is decided per
client lifetime. It is not a reported or computed latency figure — it is the
position of the impulse peak in a deconvolved sweep, i.e. where the sound
actually came back. The state is **sticky over seconds**: consecutive client
registrations a second apart nearly always land in the same state, and it flips
on a timescale of roughly 5–60 s of repeated client churn.

Modelled: `round_trip = N × 1024 + 2152 samples`, with **N ∈ {1, 2}** and the
2152-sample remainder (22.4 ms — converter + USB) constant across both states.

## Why it matters, in the project this came from

`ac` measures acoustic distance and transfer functions. It subtracts a stored
round-trip constant (τ) from every arrival time. One period at 96 kHz is
10.67 ms = **3.7 m of apparent distance**, which is larger than most of the
geometry being measured. Worse, `ac`'s own guard against this takes two
readings in two separate client lifetimes and refuses when they disagree — and
because the state is sticky, **both readings agree while both are wrong**, 43 %
of the time. Tracked as `ac` issue #363.

---

## Evidence

Session of 2026-08-22, 22:28–22:42 EEST. Electrical loopback only — a physical
cable from `Babyface Pro Pro:playback_2` (analogue out 2) to
`Babyface Pro Pro:capture_4` (analogue in 4). No microphone, no speaker in the
path. Stimulus: 0.2 s log sweep at −30 dBFS, deconvolved (Farina), peak of the
linear impulse response taken as the round trip.

**Ground truth for this cable is 4200 samples / 43.7500 ms**, established
independently on 2026-08-14 by two methods that agreed to 0.4 samples (Farina
peak and `jack_iodelay`), and reproduced twice on 2026-08-22 by a separate code
path (`peak_index 12392`, `+4200 samples`, `snr 36.71 dB`).

**97 measurement runs, each taking two readings in two client lifetimes:**

| reported round trip | runs |
|---|---|
| 4200 samples / 43.7500 ms (true) | 55 |
| 3176 samples / 33.0833 ms (one period short) | 42 |

- **Zero runs** out of 97 had their two readings disagree. If the state were an
  independent per-lifetime draw at the observed 43 % rate, roughly half of all
  runs would have drawn one of each. That is what pins the stickiness.
- The peak amplitude is **identical in both states** — `peak_abs` = 2.9507e-2
  to five significant figures — as is the noise floor. Nothing about the
  signal degrades; it simply arrives one period earlier or later.
- Peak-to-floor ratio was 46–83 dB throughout. This is not a detection
  problem.

**Ruled out by controlled experiment, not by argument:**

- **Not an analysis-window artefact.** The measurement window size was swept
  continuously across 14 values in both directions, with a control run at a
  window 30 % larger than needed interleaved before each test point. The
  control shows the same two states at the same times as the test rows.
- **Not the JACK period changing.** Every run reported `period_size 1024`, and
  PipeWire's `clock.quantum` stayed 1024 throughout.
- **Not a stuck or drifting value.** A watcher caught the 33.08 ms state, ran
  an independent tool on the same ports ~2 s later (43.75 ms), and a third
  reading ~3 s after that (43.75 ms). It flips back on its own.
- **Not sample-rate or clock reconfiguration.** 96 kHz and `AutoSync` (external
  ADAT master) throughout, verified before and after.

**Prior observation, same rig, 2026-08-18:** the same one-period jump was seen
with the **fractional part of the delay preserved exactly** across the jump
(4262.064 → 5286.064 samples). That is the strongest single clue in this file:
whatever adds the period does not resample, does not interpolate, and does not
touch sub-sample alignment. It adds a whole buffer.

---

## System under test — collected 2026-08-22, after the runs

| item | value |
|---|---|
| host | Arch Linux, kernel `7.1.3-1-rt-mui` (`PREEMPT_RT`) |
| device | RME Babyface Pro, USB `2a39:3fc0`, high speed, at `usb-0000:23:00.3-2` |
| driver | in-kernel `snd_usb_audio` — **this is where the RME reverse-engineering work lives**, including the Babyface-Pro-specific paths below |
| ALSA card | `hw:0` `Pro71990237`, 10 ch in / 10 ch out, `S32_LE`, MMAP_INTERLEAVED |
| ALSA params | `rate 96000`, `period_size 1024`, `buffer_size 4096` (**4 periods**), identical on playback and capture |
| server | PipeWire 1.6.8 (no `jackd`; `libjack.so.0.3.1608` is PipeWire's) |
| PipeWire clock | `clock.rate 96000`, `clock.quantum 1024`, `min-quantum 512`, `max-quantum 2048`, `force-quantum 0` |
| ALSA node props | `api.alsa.period-size 1024`, `api.alsa.period-num 4`, `api.alsa.headroom 0`, `clock.quantum-limit 8192` |
| node lifecycle | `session.suspend-timeout-seconds = 0`, `node.pause-on-idle = false` — **suspend is disabled**; with no client running, both device nodes sit in state `idle`, not `suspended` |
| clock source | `AutoSync` (external master over ADAT), `numid=320` = 0 |
| `snd_usb_audio` params | `lowlatency = Y`, `implicit_fb = N` (all cards), `autoclock = Y`, **`bbfpro_pc_nurbs = 8`**, **`bbfpro_pc_npkts = 16`** |
| PCM state with **no client running** | both `pcm0p` and `pcm0c` read `state: RUNNING`, same `trigger_time`, same `hw_ptr` — the device stream is continuous across client lifetimes |
| PCM delay, idle | playback `delay: 1536`, capture `delay: 512` |

## Where the evidence does and does not point

**Correction to an earlier draft of this file:** it argued that the driver was
not in the path because no third-party RME module is loaded. That is wrong. The
RME reverse-engineering work *is* `snd_usb_audio` — the Babyface Pro's
device-specific paths are in the mainline driver, and this rig exposes them
directly as the `bbfpro_pc_nurbs = 8` / `bbfpro_pc_npkts = 16` module
parameters. The driver is very much in the path.

What the measurements *do* bound:

1. **The stream is never restarted.** Verified directly, not inferred: with no
   client running, both PCM substreams read `state: RUNNING`, sharing a
   `trigger_time` and an `hw_ptr`. Suspend is disabled
   (`session.suspend-timeout-seconds = 0`, `node.pause-on-idle = false`). So
   whatever adds the period does so **without** an alt-setting change, a
   re-negotiated format, or a fresh `snd_pcm_start` — any explanation has to
   work on a stream that is already running.
2. **A whole buffer appears, nothing else changes.** The 2152-sample remainder
   — the part carrying converter and USB transport delay — is identical in both
   states, the peak amplitude is identical to five significant figures, and the
   2026-08-18 observation has the **fractional** part surviving the jump
   untouched (4262.064 → 5286.064). Nothing resamples, interpolates, or
   re-aligns. One buffer of exactly one graph quantum is added or removed.
3. **The quantum itself never moves.** `clock.quantum` stayed 1024 and every
   run reported `period_size 1024`.

Two candidate layers survive that, and the evidence does not currently separate
them:

**(a) PipeWire graph scheduling.** A newly registered client's node may be
scheduled before or after the always-running ALSA driver node depending on when
in the cycle it registers, which costs exactly one quantum and nothing else.
Fits the stickiness (ordering is fixed for the client's life) and the flips (a
new registration can land on the other side).

**(b) `snd_usb_audio` playback start alignment, with `lowlatency = Y`.** In
low-latency mode the driver defers URB submission until the application has
written data, so the achieved pipeline depth depends on *when* the first write
lands relative to the URB schedule — and it can settle a buffer deeper or
shallower **without restarting the stream**, which is exactly the constraint
point 1 imposes. The playback pipeline here is `8 URBs × 16 packets`; at 96 kHz
high speed a packet is one 125 µs microframe = 12 samples, so 8 × 16 × 12 =
**1536 samples**, which is precisely the playback `delay` read while idle. That
geometry is Babyface-specific and driver-configured, so if (b) is the mechanism,
this is squarely `rme-re` territory.

The single most useful next measurement is the one that separates (a) from (b),
and it is cheap — see experiment 1 below.

---

## Reproduction

### Without the `ac` project (preferred for a driver/server investigation)

`jack_iodelay` measures round trip through whatever it is patched to and prints
continuously. Each invocation is a **new client**, which is the unit that
varies here.

```bash
# Physical loopback cable must be patched: analogue out 2 -> analogue in 4.
for i in $(seq 1 30); do
  jack_iodelay > /tmp/iod-$i.txt 2>&1 &
  sleep 0.4
  jack_connect  jack_delay:out "Babyface Pro Pro:playback_2"
  jack_connect  "Babyface Pro Pro:capture_4" jack_delay:in
  sleep 3
  kill %1; wait 2>/dev/null
  echo "run $i: $(tail -2 /tmp/iod-$i.txt | head -1)"
done
```

Expect the reported total round trip to take two values 1024 samples apart,
in blocks rather than alternating. **This emits a test signal at
`jack_iodelay`'s own fixed level into the cable** — confirm with the operator
before running; on this rig the standing consent covers a cable, not a
loudspeaker, and the speaker leg must not be patched while this runs.

### With the `ac` project

Binaries were left on the rig at `~/rig-2026-08-22/bin-350/`:

| binary | sha256 |
|---|---|
| `ac-daemon` | `605bc9f9e6e3c90fd12c3b80802bf15aa193b03e07848f004934537b7337093c` |
| `ac` | `7fa422d42d57f7c6918038c266b60fe76168f938f4680ffaf3a79822ddbdd902` |

```bash
export PATH="$HOME/rig-2026-08-22/bin-350:$PATH"   # required: another
# ac-daemon on the default PATH routes output to the loudspeaker
pkill -x ac-daemon
HOME=$HOME/rig-2026-08-22/home-350 ac-daemon --local &
printf 'skip\nskip\n' | HOME=$HOME/rig-2026-08-22/home-350 ac calibrate -30dbfs
```

The `Delay:` line reports the round trip. Repeat with a fresh daemon each time.
Raw logs from the session, ~110 runs: `~/rig-2026-08-22/logs-350/`.

---

## Experiments that would discriminate, in the order I would run them

1. **Read the ALSA pipeline depth in each state.** While the measurement
   reports 43.75 ms, and again while it reports 33.08 ms, dump
   `/proc/asound/card0/pcm0p/sub0/status` and `.../pcm0c/sub0/status` and
   compare `delay`, `avail`, `hw_ptr` and `appl_ptr`. **If `delay` differs by
   1024 between the two states, the extra buffer is visible below PipeWire and
   candidate (b) is live; if `delay` is identical in both states, the period is
   being added above ALSA and (a) is the answer.** No emission beyond the
   measurement already being run, no privileges, no reconfiguration. This is
   the experiment that decides whether this handover belongs in a driver
   project at all.
2. **Does it happen without a client restart?** Register one long-lived JACK
   client and measure repeatedly for several minutes without re-registering. If
   the round trip never changes, the trigger is client registration.
3. **`snd_usb_audio.lowlatency=0`.** Reload the module with low-latency mode
   off and repeat the churn loop. If the two states collapse to one, the
   mechanism is the driver's deferred URB submission and the investigation is
   entirely inside `snd_usb_audio`. Requires a module reload, so schedule it —
   nothing on this rig may be mid-measurement.
4. **Vary the Babyface URB geometry.** `bbfpro_pc_nurbs` (8) and
   `bbfpro_pc_npkts` (16) set the 1536-sample playback pipeline. If changing
   them moves the *constant* 2152-sample part but leaves the 1024-sample jump
   alone, the jump is not in the URB pipeline; if the jump follows them, it is.
5. **Pin the graph.** Set `clock.force-quantum 1024` (and `node.lock-quantum`
   on the client) so the graph cannot renegotiate, then repeat the churn loop.
   Exonerates or implicates the renegotiation path.
6. **Compare measured against reported latency per lifetime.** Record
   `jack_client_get_latency_range` in both directions alongside the measured
   round trip. If the reported figure tracks the measurement, the server knows
   it is adding a period; if it stays constant while the measurement moves by
   1024, the server is wrong about its own graph — a more serious finding, and
   one worth reporting upstream.
7. **Same experiment on a different interface on the same host.** The machine
   has an `HDA NVidia` card. If it shows the same two-state behaviour, nothing
   Babyface-specific is involved. Note this is weaker evidence than it looks:
   a shared scheduling cause would show on both cards, but a *driver* cause
   would not, so a negative result here narrows the search while a positive one
   does not close it.
8. **Only if 1–7 point at the wire:** `usbmon` on bus 3 across a flip, comparing
   the first packets of a lifetime that lands in the N=1 state against one that
   lands in N=2 — the number of frames buffered before the first data packet,
   and the feedback endpoint's initial reports. Note `implicit_fb = N` here, so
   the implicit-feedback path is not the one in use.

## What to report back

For `ac` issue #363 to be closed properly, this investigation needs to answer
one question: **is there a condition a measurement client can establish, or
check, that guarantees which state it is in?** Anything that makes the state
observable from inside a client — a latency figure that is finally correct, a
setting that pins it, or a documented condition under which the jump cannot
occur — lets the measurement layer stop guessing. A fix that merely makes the
jump rarer is not enough: `ac` stores τ and reuses it for later sessions, so a
rare wrong value is worse than a frequent one, because nothing will catch it.

If the answer turns out to be "no such condition exists", say so plainly — that
is also actionable. It means the measurement layer must corroborate τ against
something that does not share a client lifetime, and `ac` will design for that
instead.

## Source material

- `work/rig/rig-2026-08-22-tau-window-350-results.md` in the `ac` repo — the
  full session record, including the control design and the run-by-run tables.
- `ac` issue #363 — the measurement-layer consequence.
- `ac` issue #347 — the two-reading agreement rule this defeats.
- Rig logs: `~/rig-2026-08-22/logs-350/` on 192.168.9.25 (`cal-m0.log`,
  `cal-ctl.log`, `cal-pin.log`, `watch.log`, and a `daemon-*.log` per point
  carrying per-reading peak/floor/SNR).
