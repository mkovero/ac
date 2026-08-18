# rig-loopback-ir-277-results — 2026-08-18, 192.168.9.25

Executes #277 ("Run `it_loopback_ir` on the rig"). Rig: RME Babyface Pro on
pipewire-jack, 96 kHz, external master clock over ADAT (`numid=320` = 0,
AutoSync, not touched).

**Build under test:** `origin/main` @ `edc298a` plus the test edit on branch
`issue-277-loopback`. Source rsynced to `~/ac-277`, built into
`~/target-277`. **The rig has no `mold`**, and `ac-rs/.cargo/config.toml`
sets `-C link-arg=-fuse-ld=mold`, so the build ran with
`RUSTFLAGS="-C target-cpu=native"`, which overrides the file's `rustflags`
wholesale. That drops the linker choice and keeps `target-cpu=native`. A
linker does not change arithmetic; it is recorded because it means these
binaries are not what a stock `cargo test` on this host would produce.

**Drive level: −40 dBFS**, the standing electrical ceiling, authorised by the
operator for this session. No exception was needed and none was taken.
**Nothing was emitted acoustically**: the speaker leg (`playback_5`, ADAT1 →
Genelec 1083) was never driven. Note for the next session that Spotify was
connected to `playback_3`, `playback_4` and `playback_5` throughout; it was
silent, and it shares no port with either leg measured here.

`plot_ir` does **not** apply the config's `drive_max_dbfs` ceiling — only
`set_drive` does (`handlers/transfer.rs:138`; `handlers/audio/plot.rs:571`
takes `level_dbfs` straight through). So on this path the level in the
request is the only limit. The test now refuses to run against real ports
unless `AC_LOOPBACK_LEVEL_DBFS` is set explicitly, rather than inheriting a
default.

## Conditions

| | |
|---|---|
| Sample rate | 96000 Hz (`clock.rate`, and `hw_params` both directions) |
| Period | 1024 (`clock.quantum`, and `hw_params period_size`) |
| Device buffer | 4096 frames = 42.67 ms (`hw_params buffer_size`, both directions) |
| Backend | pipewire-jack |
| Analogue leg | `playback_2` (AN2) → `capture_4` (IN4) |
| Converter leg | `playback_7` (ADAT3) → external converter → analogue out → `capture_3` (IN3) |

The test's self-loop default (`ac-daemon:out → ac-daemon:in`) routes through
no converter at all, so #277's "not `jackd -d dummy`" criterion needed the
port names to become settable. They are now `AC_LOOPBACK_OUT` /
`AC_LOOPBACK_IN`, defaulting to the old self-loop.

## Results

Sweep 50 Hz–16 kHz, `tail_s` 0.2, `n_harmonics` 3, `window_len` requested
16384. `peak_offset` is the peak's distance from the window centre, which is
the round-trip latency of that chain.

| run | chain | sweep | window used | peak idx | peak abs | floor abs | SNR dB | peak offset |
|---|---|---|---|---|---|---|---|---|
| 1 | `playback_2` → `capture_4` | 2.0 s | 16384 | 12392 | 1.564617 | 2.2845e−2 | 36.71 | +4200 = **43.750 ms** |
| 1r | `playback_2` → `capture_4` | 2.0 s | 16384 | 12392 | 1.564609 | 2.2842e−2 | 36.71 | +4200 = **43.750 ms** |
| 2 | `playback_7` → `capture_3` | 2.0 s | 16384 | 12404 | 1.410347 | 2.0279e−2 | 36.85 | +4212 = **43.875 ms** |
| 2r | `playback_7` → `capture_3` | 2.0 s | 16384 | 12404 | 1.410307 | 2.0185e−2 | 36.89 | +4212 = **43.875 ms** |
| 1s | `playback_2` → `capture_4` | 0.5 s | 5768 | 5709 | 2.1673e−1 | 1.3295e−2 | 24.24 | +2825 = 29.427 ms |
| 2s | `playback_7` → `capture_3` | 0.5 s | 5768 | 5717 | 1.9404e−1 | 1.2167e−2 | 24.05 | +2833 = 29.510 ms |

Cross-checks, each output against the *other* leg's input:

| chain | peak abs | SNR dB | reading |
|---|---|---|---|
| `playback_7` → `capture_4` | 1.109e−5 | 6.3 | no signal |
| `playback_2` → `capture_3` | 3.368e−4 | 3.8 | no signal |

Five orders of magnitude between a patched leg and an unpatched one. The
legs are independent; neither number below is crosstalk.

Both legs repeat to the **exact sample index** across runs. Peak magnitudes
agree to six figures. Whatever else is wrong here, the measurement is not
noisy.

## Finding 1 — the test cannot pass, and could not have passed before it reached hardware

**Filed as #341.**

`it_loopback_ir` is red on every configuration tried, including the
`jackd -d dummy` self-loop its own runbook prescribes. Verified on a pristine
worktree at `origin/main` `edc298a` with no edit of mine, so this is the
state the issue found, not a state it caused:

| config | result |
|---|---|
| dummy, 48 kHz, period 1024 | peak at 2466, bound [721, 2163] — **fails the peak-position assertion** |
| dummy, 96 kHz, period 1024 | peak in bounds; SNR 28.90 dB — **fails the ≥ 40 dB assertion** |
| Babyface, either leg, 2.0 s | peak at 12392/12404, bound [4096, 12288] — **fails the peak-position assertion** |

Both failure modes are properties of the test's expectations, not of the
audio path. The 96 kHz dummy case is the sharper one: the dummy driver's
loopback is bit-exact, so 28.90 dB is what the deconvolution produces with a
*perfect* signal chain. The 40 dB gate is unreachable at these sweep
parameters no matter what hardware is attached — the best number measured
anywhere in this session is 36.89 dB, on a clean electrical loopback.

The threshold was never run, so it was never wrong out loud.

## Finding 2 — `window_len` is a request, and the clamp is what the assertions actually divide

**Filed as #342.**

`per_order_window_lens` clamps each order's window to the gap between it and
the next order (`sweep.rs:318`, `:335`):

```
window_len_used[0] = min(window_len_requested, offsets[1] - offsets[0])
```

and that gap is `duration · ln 2 / ln(f2/f1)` seconds. For the test's 0.5 s
sweep over 50 Hz–16 kHz that is 60.1 ms — 2884 samples at 48 kHz, 5768 at
96 kHz — against a requested 16384. The gate is centred at the sweep
endpoint (`sweep.rs:381`), so the window reaches only **half** that gap past
the centre: 30.0 ms.

The test's `len/4 … 3·len/4` bound was written for a 16384-sample window,
where one JACK period of latency is comfortably inside. Against the window
that is actually returned, one period is already at the edge. Nothing in the
frame contradicts this — `window_len_used` is published, and is simply not
read by anything.

## Finding 3 — the interface round trip is 43.75 ms, and the short sweep reports 29.4 ms instead

τ for the analogue leg is **43.750 ms** (4200 samples), for the converter leg
**43.875 ms** (4212 samples), at 96 kHz / period 1024 / buffer 4096.

43.75 ms against a 42.67 ms device buffer is the expected shape: the round
trip is dominated by the ALSA buffer, plus about a quarter-period of
converter and graph latency. It is not a rounding of anything smaller.

The 0.5 s runs report **29.4 ms** for the same two chains. That number is
not a latency. The window only extends 30.04 ms past its centre, so a 43.75
ms arrival cannot appear in it; the peak lands 59 and 51 samples from the
window's end, which is where the true impulse's leading edge is cut off.
Both legs converge on ~29.5 ms because both are pinned against the same
edge, not because they measure the same thing.

`jack_iodelay` measures 43.754 ms on the same leg by an unrelated method, so
the 29.4 ms figure has been falsified by a second instrument, not merely
argued against.

**This is the failure mode to carry into #281, and it is live rather than
hypothetical — filed as #340.** `measure_tau` sweeps 0.2 s from 100 Hz
(`calibrate.rs:56-64`), which clamps its window to 26.2 ms and so reaches
13.1 ms past centre, at every sample rate. This rig's τ is 43.75 ms. `ac
calibrate` cannot measure it, has no edge guard, and would return a pinned
peak. A τ measured with too short
a sweep does not error, does not warn, and does not look wrong: it produces a
stable, repeatable, physically plausible number that is off by 14 ms. Runs 1s
and 2s reproduce to within 8 samples of each other and would pass any
repeatability check written against them. The condition tuple #281 keys τ on
— device, backend, sample rate, period size, port pair — does **not** include
the sweep duration that decides whether the window can hold the arrival at
all. Two τ values measured under an identical tuple, one correct and one 14
ms short, are indistinguishable to that layer.

## Verification against `jack_iodelay`

Both τ figures were re-measured with `jack_iodelay` (jack-example-tools 4-2),
which resolves round-trip latency by a phase method on a pair of sinusoids —
a different stimulus, a different estimator, and no code of ours in the path.
Its output level is not adjustable and is above the −40 dBFS session ceiling;
both legs are electrical with no loudspeaker patched, and the operator asked
for this check.

| leg | Farina IR peak | `jack_iodelay` | delta |
|---|---|---|---|
| `playback_2` → `capture_4` | 4200 samples, 43.750 ms | 4200.379 frames, 43.754 ms | 0.38 samples |
| `playback_7` → `capture_3` | 4212 samples, 43.875 ms | 4212.099 frames, 43.876 ms | 0.10 samples |
| converter leg − analogue leg | 12 samples, 0.125 ms | 11.72 frames, 0.122 ms | 0.28 samples |

The IR method reports integer samples — no sub-sample interpolation on the
peak — so this is agreement as tight as it can express. It confirms three
things independently: τ ≈ 43.75 ms, the converter's 0.125 ms cost, and that
the 0.5 s sweep's 29.4 ms is an artefact rather than a second opinion.

`jack_iodelay` also reports "extra loopback latency" of 2152 frames
(analogue) and 2164 (converter) against a 4200/4212-frame round trip. The
difference is 2048 frames = two periods — the latency JACK itself reports
through its port-latency API. **Slightly under half the true round trip is
visible to that API**; the remaining 22.4 ms is device buffering JACK does
not account for. Anything deriving τ from `jack_port_get_latency_range`
rather than measuring it would be wrong by that margin, in the direction of
reporting the interface as faster than it is.

## Finding 4 — the converter leg costs 0.125 ms, which is not the 1.1931 ms constant

**Filed as #343.**

The converter leg is **12 samples** longer than the pure analogue leg:
0.125 ms, stable across repeats, at 96 kHz.

`rig-session-3-results.md` established 1.1931 ms as the constant term in
`arrival(d) = 1.1931 ms + d/346`, measured acoustically against an electrical
reference that did not traverse the converter. If the converter's DAC path
were the whole of that term, this session would have measured about 1.19 ms
of difference. It measured a tenth of that.

Both legs here return through a Babyface ADC, so the ADC cancels in the
difference. What 0.125 ms measures is the converter's DAC path, ADAT hop
included, *minus* the Babyface's own AN2 DAC path — small because both ends
of the subtraction are DACs of comparable design.

So roughly 1.07 ms of the acoustic constant lives somewhere other than
conversion. **Not in digital processing downstream of the converter:** the
1083 is an analogue loudspeaker and the EQ ahead of it is analogue too, so
there is no DSP after the converter to hold a millisecond (operator,
2026-08-18). Analogue filters at these frequencies cost tens of microseconds
of group delay, not a millisecond.

That leaves two candidates this session cannot separate:

- **The loudspeaker's own acoustic group delay.** An analogue crossover plus
  the drivers themselves carry excess group delay concentrated at LF. How
  much of it lands in an arrival estimate depends on which bands that
  estimate weights.
- **The estimator.** Session 3 derived the constant from `transfer_stream`'s
  cross-correlation delay over the analysis band, not from a Farina IR peak.
  A band-limited system biases a broadband correlation, and the two methods
  need not agree on what "arrival" means.

It is not a mis-measured distance: 1.07 ms is 37 cm at 346 m/s, far outside
any plausible error in a taped 1.000 m.

What this session does establish is that "external-converter latency" is the
wrong name for that constant, and that anything applying it as a converter
correction is applying about ten times too much.

The clean way to separate the two remaining candidates is one session that
measures the same speaker leg both ways at a fixed taped distance — Farina IR
peak and `transfer_stream` delay, same patch, same evening. Disagreement of
about a millisecond puts it on the estimator; agreement puts it on the
loudspeaker. That needs the mic and acoustic emission, so it is its own run
and its own issue, not a fix for this one.

## What this session does not say

- **Nothing about `sweep_ir`'s numerics.** Eight tests already cover the
  Farina maths. Everything red here is a window, a threshold, or a bound.
- **Nothing about whether 43.75 ms is the right τ for a measurement.** It is
  the round trip for these two port pairs at this buffer size. It is not the
  τ of the stimulus leg used for acoustic work (`playback_5`), which was
  deliberately not driven.
- **Nothing about the acoustic path**, the mic, or the speakers. No acoustic
  emission occurred.
- **Nothing about the CPAL backend** (issue #27), untouched here.

## Expiry

Supersede when `it_loopback_ir`'s thresholds are re-derived, or when τ is
next measured at a different period size or buffer size. The numbers in the
results table are valid only for 96 kHz / period 1024 / buffer 4096 on
pipewire-jack, with the two port pairs named.
