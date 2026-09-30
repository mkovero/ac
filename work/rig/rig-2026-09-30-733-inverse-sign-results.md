# Rig record — #733 Farina inverse envelope sign (and #504), 2026-09-30

- **Rig:** pupu, FF400 @ 96 kHz, period 256, `jackd -S`. Preflight passed: port order analog-first, ALSA baseline, 0 xruns.
- **Builds (portable, `scripts/rig/build-portable.sh`):**
  - `main` = `cb8c7ae5b9e8` (origin/main); `ac-daemon` sha256 `d74acc31…`.
  - `fix` = `2cae9485c536` (branch `tail-decay`, WIP commit: sign fix plus the envelope-based tail-decay check); `ac-daemon` sha256 `2683616b…`.
- **Consent:** the operator said "do the loopback run now", then "I'm not in the room, you can emit also".
- **Procedure:** default `ac plot ir` (20 Hz–20 kHz, 4 s, 0.4 s window, 0.5 s tail), each build under its own isolated HOME and daemon ports.
  - `loop`: output 1 (AN2) → cable → input 1 (IN2), no reference, −40 dBFS typed, 3 runs per build.
  - `acou`: output 0 (AN1) → Genelec 1083 → mic IN1 at ~1 m, reference AN2→IN2, −50 dBFS typed (standing speaker ceiling), 2 runs per build.
- **Script:** `rig733.sh`, run as a file over ssh (not `bash -s`).

## Printed read-out

| run | pre-imp SNR dB | arrival (smp) | broadband Δ | tail-decay line | exit |
|---|---|---|---|---|---|
| main-loop-1..3 | 22.0 | +1711 | +0 | FAILED: 40 Hz band only decayed 22.0 dB | 0 |
| fix-loop-1..3 | 76.0 | +1711 | +0 | adequate: worst 25 Hz band 32.6 dB | 0 |
| main-acou-1 | 22.5 | +2032 | +141 | FAILED: 15849 Hz band 0.4 dB | 0 |
| main-acou-2 | 21.8 | +2032 | +139 | FAILED: 15849 Hz band 0.1 dB | 0 |
| fix-acou-1 | 74.8 | +2031 | −3 | FAILED: 25 Hz band 4.6 dB, levelled off | 0 |
| fix-acou-2 | 74.9 | +2031 | −4 | FAILED: 25 Hz band 8.3 dB, levelled off | 0 |

On `main`, the acoustic broadband peak (argmax) sat at sample 23548: 2317 samples after the 2 kHz high-passed arrival. That is the "low-frequency room mode" #669 designed around. On `fix` it is at 21231, 0 samples from the arrival.

## Frequency response from the report's `linear_ir` (FFT, re 1 kHz)

| run | p-p 50 Hz–18 kHz | 31.5 | 100 | 250 | 4 k | 10 k | 18 k |
|---|---|---|---|---|---|---|---|
| main-loop | 107.6 dB | +61.6 | +41.0 | +25.3 | −14.7 | −24.0 | −39.2 |
| fix-loop | 0.5 dB | +0.1 | −0.0 | −0.0 | −0.0 | −0.0 | −0.3 |
| main-acou | 93.2 dB | +46.4 | +36.3 | +28.3 | −24.6 | −31.9 | −38.5 |
| fix-acou | 32.7 dB | −15.8 | −5.1 | +4.2 | −8.1 | −10.0 | −5.3 |

The lost commit `d6c8fd85` recorded 100.0 → 0.6 dB p-p on the Babyface loopback on 2026-08-28. This run reproduces that on the FF400: 107.6 → 0.5 dB.

## Reading

- The sign fix flattens the loopback to 0.5 dB p-p. It raises the pre-impulse figure 54 dB on both paths, and it removes the late broadband peak on the acoustic path. The 2 kHz high-passed arrival did not move (±1 sample).
- The loopback tail-decay FAILED of #504 is gone at the default tail.
- The acoustic 25 Hz band rises only 4.6–8.3 dB over its floor. The 1083 produces little at 25 Hz at −50 dBFS. That is a property of the path and level, not of the tail length ("levelled off").
