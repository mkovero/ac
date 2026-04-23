# Loudness measurement — ITU-R BS.1770-5 + EBU R128

Tier-1 loudness meter per ITU-R BS.1770-5, with the EBU R128 broadcast
delivery profile layered on top. Delivered as a sidecar on the existing
`ac monitor` stream so every monitored channel gets live LKFS-M / LKFS-S /
LKFS-I / dBTP / LRA readouts and R128 pass/fail colour-coding.

Why ITU-R + EBU: both standards are free downloads (unlike the IEC / AES
specs the rest of the repo cites). The EBU also publishes compliance test
WAVs that become our golden fixtures.

## Status

| Phase | Subject | State |
|---|---|---|
| A | K-weighting filter, 400 ms MS block, per-block LKFS | planned |
| B | Momentary / short-term / integrated + two-pass gating | planned |
| C | Loudness range (LRA) | planned |
| D | True-peak via 4× polyphase FIR | planned |
| E | Wire into `ac monitor` PUB stream + UI overlay + reset key | planned |
| F | EBU Tech 3341 / 3342 compliance validation | planned |

The citation flips to `verified: true` only after Phase F is green.

## Algorithm

### K-weighting (BS.1770-5 §2.1 + §2.2)

Two-stage biquad cascade, applied per channel in time-domain:

1. **Pre-filter** (high-shelf, ≈ 4 dB boost @ 1.68 kHz). Approximates the
   acoustic response of a head at high frequencies.
2. **RLB filter** (2nd-order high-pass, −3 dB @ ≈ 38 Hz). Revised Low-Frequency
   B-curve — rolls off the rumble region.

BS.1770-5 publishes explicit digital coefficients for 48 kHz (Annex 1 Table 1):

```
Pre-filter       RLB filter
b0 =  1.53512486  b0 =  1.0
b1 = -2.69169619  b1 = -2.0
b2 =  1.19839281  b2 =  1.0
a1 = -1.69065929  a1 = -1.99004745
a2 =  0.73248077  a2 =  0.99007225
```

Difference equation (shared convention — matches `weighting.rs`):

```
y[n] = b0·x[n] + b1·x[n-1] + b2·x[n-2] − a1·y[n-1] − a2·y[n-2]
```

Our implementation stores state in Direct-Form-II-Transposed for numerical
stability across long streams.

For non-48 kHz sample rates, coefficients are re-derived by running the
RBJ cookbook formulas on the two analog prototypes that reproduce the
Annex 1 values at 48 kHz:

```
Pre-filter:    high-shelf, f0 = 1681.974450955532 Hz,
               Q = 0.7071752369554193,  gain_db = 3.999843853973347
RLB filter:    high-pass,  f0 = 38.13547087613982 Hz,
               Q = 0.5003270373253953
```

These parameters are the inverted-analog values used by `libebur128` and
`ffmpeg ebur128`, i.e. they're the analog prototypes that, under the
bilinear transform at fs = 48 kHz, reproduce Annex 1 Table 1 exactly.

### Mean-square block (§2.3)

- Block length: 400 ms
- Block overlap: 75 % → new block every 100 ms
- Per block, per channel: arithmetic mean of squared samples
- Channel-weighted sum across channels → single mean-square per block
- Block loudness: `L_k = −0.691 + 10·log10(MS)` [LKFS]

Channel weights (§2.4):

```
L, R, C, mono     1.0
Ls, Rs            1.41
LFE               0.0 (excluded)
```

Initial implementation ships **mono and stereo**. 5.1 / 7.1 needs a
channel-map contract (follow-up issue filed at Phase E).

### Gating (§2.4)

Two passes over the sequence of 400 ms blocks:

1. **Absolute gate**: drop blocks with `L_k < −70 LUFS`.
2. **Relative gate**: compute ungated integrated loudness over surviving
   blocks; drop blocks more than 10 LU below that.

The mean of the doubly-gated blocks is the integrated loudness
(LKFS-I / LUFS-I). For EBU R128, target is −23 LUFS with ±0.5 LU tolerance
(±1 LU for live broadcast).

### Momentary / short-term

- **Momentary (LKFS-M)**: single 400 ms block loudness (ungated),
  re-computed every 100 ms.
- **Short-term (LKFS-S)**: 3 s sliding window, re-computed every 100 ms.
  Implemented as mean of the last 30 mean-squares (100 ms step).

### Loudness range (Tech 3342)

Sliding 3 s short-term windows at 100 ms step → distribution of LKFS-S
values. Two-pass gating:

1. Absolute gate at −70 LUFS.
2. Relative gate at −20 LU below the ungated mean of surviving values.

`LRA = P95 − P10` of the doubly-gated values, in LU.

### True-peak (§3, Annex 2)

- Upsample by 4× using a 48-tap linear-phase FIR, split into 4 polyphase
  sub-filters of 12 taps each. Annex 2 gives the taps.
- `dBTP = 20·log10(max(|upsampled|))`.
- Reported per block (100 ms), with a persistent max across the measured
  program.

## ZMQ frame schema (Phase E)

Added to the monitor PUB stream; one frame per monitored channel per
100 ms, same cadence as `fractional_octave_leq`:

```json
{
  "type": "loudness",
  "ch": 0,
  "momentary_lkfs": -23.0,
  "short_term_lkfs": -22.8,
  "integrated_lkfs": -23.1,
  "lra_lu": 7.3,
  "true_peak_dbtp": -1.1,
  "gated_duration_s": 42.5,
  "profile": "ebu_r128",
  "target_lkfs": -23.0,
  "tolerance_lu": 0.5,
  "pass": true
}
```

Reset from the client side: `{"type": "loudness_reset"}` on the CTRL
socket, analogous to the existing `reset_leq`. Zeroes the integrated
accumulator and the LRA histogram; doesn't flush the K-weighting filter
state (that's per-channel, not per-measurement).

UI: top-right status row under the fractional-octave overlay. Bound to
`Shift+L` (reset) — confirm no collision in `ac-ui/src/app/input.rs`
before wiring.

## Test vectors

EBU Tech 3341 "EBU Mode" compliance vectors and Tech 3342 LRA compliance
vectors are fetched at test time by `scripts/fetch-r128.sh` into
`ac-rs/tests/fixtures/loudness/` (git-ignored). Run:

```
scripts/fetch-r128.sh
cargo test -p ac-core --test loudness_ebu
```

### Tech 3341 cases (integrated + momentary + short-term)

| # | Stimulus | Expected |
|---|---|---|
| 1 | 1 kHz sine, stereo, 20 s @ −23 dBFS | I = −23.0 ±0.1 LU |
| 2 | 1 kHz sine, stereo, 20 s @ −33 dBFS | I = −33.0 ±0.1 LU |
| 3 | segments @ −36 / −23 / −36 dBFS | I = −23.0 ±0.1 LU |
| 4 | segments 20 s / 60 s / 20 s at −72/-36/-72 dBFS | I = −23.0 ±0.1 LU |
| 5 | segment with loudness from −26 to −20 LU (no gating effect) | I per reference |
| 6 | 5.1 program | deferred until surround support lands |
| 7 | Tech 3341 short-term test signal | M / S match reference |
| 8 | Tech 3341 momentary test signal | M matches reference |
| 9 | Tech 3341 fs-variant (44.1/88.2/96 kHz) | I within ±0.1 LU of 48 kHz path |

### Tech 3342 cases (LRA)

| # | Stimulus | Expected LRA |
|---|---|---|
| 1 | Static 1 kHz sine | 0 ±0.1 LU |
| 2 | Ramp −20 LU to −10 LU | 10 ±1 LU |
| 3 | Tech 3342 case 3 WAV | per reference |
| 4 | Tech 3342 case 4 WAV | per reference |
| 5 | Tech 3342 case 5 WAV | per reference |
| 6 | Tech 3342 case 6 WAV | per reference |

### Tech 3341 true-peak cases

| # | Stimulus | Expected dBTP |
|---|---|---|
| 15 | 0 dBFS sine at fs/4, 0° phase | 0.0 ±0.1 dB |
| 16 | 0 dBFS sine at fs/4, 45° phase | +3.0 ±0.1 dB |
| 17 | intersample-peak test case | per reference |
| 18 | intersample-peak test case | per reference |

## References (all free)

- **ITU-R BS.1770-5** — `stddocs/ITU-R BS.1770-5.pdf` in this repo, or
  <https://www.itu.int/rec/R-REC-BS.1770> for the latest revision.
- **EBU R128** — <https://tech.ebu.ch/publications/r128>.
- **EBU Tech 3341 "Loudness Metering"** —
  <https://tech.ebu.ch/publications/tech3341>.
- **EBU Tech 3342 "Loudness Range"** —
  <https://tech.ebu.ch/publications/tech3342>.
- **Compliance test WAVs** — <https://tech.ebu.ch/publications/ebu_loudness_test_set>.
