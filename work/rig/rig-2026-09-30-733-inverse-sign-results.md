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

## Final build `860a16352816` (all re-derivations in), same setup, same night

| run | pre-imp SNR dB | arrival | broadband Δ | tail-decay line | STI | exit |
|---|---|---|---|---|---|---|
| loop-1, loop-2 | 76.0 | +1711 | +0 | adequate, 25 Hz 33.3 dB | — | 0 |
| loop-sti (1.6 s tail) | 76.0 | +1711 | +0 | adequate, 1585 Hz 75.0 dB | 1.00 | 0 |
| acou-1 | 74.9 | +2031 | −3 | "tail long enough; 25 Hz band rises only 7.3 dB above its floor — check the drive level and the background noise in that band" | — | 0 |
| acou-2 | 73.1 | +2031 | −3 | same, 9.7 dB | — | 0 |
| acou-sti (1.6 s tail) | 74.0 | +2031 | −3 | same, 7.6 dB | 0.99 | 0 |

- **Acoustic room figures, octave bands 500 Hz–4 kHz:** T30 0.11–0.16 s, C50 +29 to +34 dB. On `main` the same bands read T30 0.12–0.15 s and C50 +28 to +33 dB. The octave-band room figures are nearly unchanged by the fix: the band filters remove a smooth tilt, so per-band decay did not depend on it.
- **STI:** 0.99 at 1 m in this dead room is consistent with those figures.
- **Reference leg:** `ref latency` measured 1711 samples on every acoustic run, with the reference gate now capped at 24 dB.

## Last commit `6635d0a44b01` (tail trend measured inside the tail, after the Codex review and recheck)

| run | pre-imp SNR dB | arrival | tail-decay line | STI | exit |
|---|---|---|---|---|---|
| loop-1 | 76.0 | +1711 | adequate, 25 Hz 32.6 dB | — | 0 |
| loop-sti (1.6 s) | 76.0 | +1711 | adequate, 1585 Hz 75.1 dB | 1.00 | 0 |
| acou-1 (0.5 s) | 75.6 | +2031 | FAILED: 79 Hz band 25.2 dB, still falling — longer tail_s | — | 0 |
| acou-sti (1.6 s) | 74.3 | +2031 | 25 Hz band 5.5 dB, "tail too short to tell whether it was still falling" | 0.99 | 0 |

The acoustic 79 Hz verdict is a real one, not a false alarm:
- The rule needs a fall of more than 12 dB between the tail's second and fourth quarters (3σ at B ≈ 18 Hz, 0.125 s): a decay faster than ≈ 48 dB/s that is still going at 0.5 s. That reads as a low room mode ringing past the default tail.
- With the 1.6 s tail of the STI run the band no longer fails; only 25 Hz is left, where the 1083 has little output.
- On `main` the same capture printed FAILED on the 15849 Hz band with 0.4 dB of decay: the tilted kernel.
