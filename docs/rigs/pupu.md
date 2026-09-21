# Rig profile: `pupu` — dedicated audio HIL rig

Facts about this machine only. **How to test on it** is
`docs/runbooks/rig-testing.md`; the machine-readable form of this page is
`scripts/rig/hosts/pupu.env` — change both together.

Last verified: 2026-09-21 (physical setup and arrival behaviour; the audio
system and JACK sections below still carry their own 2026-09-14 dates).

## Access

- Address, user and SSH key are private and not in this repo. The scripts
  read them from `$AC_HOME/rig-hosts/pupu.access.env` (`RIG_HOST`,
  `RIG_USER`, `RIG_SSH_KEY`).
- Never build here; ship binaries built on the development host.

## Physical setup

| FF400 port | JACK port | `ac` index | connected to |
|---|---|---|---|
| AN1 out | `system:playback_1` | output 0 | Genelec 1083 |
| AN2 out | `system:playback_2` | output 1 | cable → IN2 (reference loopback) |
| IN1 (mic pre, phantom **on**) | `system:capture_1` | input 0 | measurement mic |
| IN2 (mic pre, phantom **off**) | `system:capture_2` | input 1 | loopback from AN2 |

JACK port names and `ac` indices here assume the analog-first order — see
"JACK port order"; it moves.

- Mic on the speaker axis; tape marks on the floor at **1 m**, **2 m** and
  **3 m** for manual moves. The mic does **not** sit at a taped mark by
  default: on 2026-09-21 it stood at ≈0.5 m, untaped, left there by an
  earlier session, while this page still claimed 1 m. Measure the position
  every session rather than assuming it, and state it in the run record.
  Measured flight times with τ removed, 96 kHz: ≈+199 samples at ≈0.5 m,
  +596…+597 at 2 m.
- The 1083 has intermittent distortion, obvious by ear when present. A run
  nobody listened to needs a THD check before it is trusted.
- τ is **1711 samples at 96 kHz for both analog pairs**, AN1 → IN1 and
  AN2 → IN2 alike, measured by cable on 2026-09-18 after an FF400 power
  cycle. τ stays per channel pair as a rule — these two simply agree here,
  and neither applies to an acoustic path.
- The FF400 reports `fw2.0` since the 2026-09-18 power cycle.
- Never plug a line output into IN1 while its phantom is on.
- Verified by emission 2026-09-14 (1 kHz, −60 dBFS, 3 s per output): AN1 →
  mic −64.7 dBFS, IN2 −139.0 dBFS; AN2 → IN2 −58.3 dBFS, mic −101.6 dBFS
  (leakage). The operator's stated layout was the reverse of the cabling
  once already — probe after any cable work.

## Audio system

- RME Fireface 400 over FireWire (TI XIO2213, shared IRQ 18), ALSA card
  `Fireface400`, driver `snd_fireface`, clock Internal.
- JACK: system unit `jack-ac.service`, drop-in
  `/etc/systemd/system/jack-ac.service.d/override.conf`:

  ```
  ExecStart=/usr/bin/jackd -R -P 80 -S -n default -t 2000 -d alsa -d hw:Fireface400 -r 96000 -p 256 -n 3
  ```

  **96 kHz, 256 frames, 3 periods, synchronous mode.** Keep `-S` (see JACK
  stability). jack2 1.9.22, kernel 7.2.4-rt3 (PREEMPT_RT), i5-2415M, no CPU
  governor / C-state / IRQ tuning needed.
- FF400 mixer/control service: user unit `snd-fireface-ctl.service`
  (enabled), bundle `~/ac-test/ff400-control-v0.2.0`.
- **Baseline:** +4 dBu line out / line in / headphone level; phantom IN1 on,
  IN2 off; mic gain IN1 20 dB, IN2 0 dB; line gains 0 dB; unity output
  volume; identity stream routing; every analog/S/PDIF/ADAT source gain 0 (no
  hardware monitor path); metering on.
- **A power cycle resets the FF400 to driver defaults** (line out *High*,
  line in *Low*, phantom off, gains 0). **So did a `jack-ac.service` restart**
  on 2026-09-15: a −60 dBFS tone on output 1 read IN2 −51.5 dBFS (baseline
  −57.5) and the mic channel went to a dead −106 dBFS floor, while
  `amixer cget` still returned the baseline values, so preflight's ALSA rows
  passed. One observation: treat every JACK restart as a possible reset
  until shown otherwise.
- **Restore with toggle writes.** Writing a value the driver already caches
  does not reach the device, so set a different value first, then the
  baseline — gains before phantom:

  ```sh
  C=Fireface400
  amixer -c $C cset numid=93 1; amixer -c $C cset numid=94 1; amixer -c $C cset numid=89 1
  amixer -c $C cset numid=93 2; amixer -c $C cset numid=94 2; amixer -c $C cset numid=89 2
  amixer -c $C cset numid=81 0,0; amixer -c $C cset numid=90 off,off; sleep 1
  amixer -c $C cset numid=90 on,off; sleep 2
  amixer -c $C cset numid=81 20,0; amixer -c $C cset numid=102 on
  ```

  Verify by emission, not by readback: `probe-outputs.sh pupu --level -60
  --outputs 1` must read IN2 ≈ −57.5 dBFS and IN1 room noise (≈ −74 dBFS rms).

  Do **not** use `ff400-card1.sh` / `scripts/ff400.sh`: it forces phantom
  off, and its `jack_alias` table is ADAT-first, which is wrong for this
  driver.

### JACK port order

**Not stable.** `snd_fireface` sometimes puts the ADAT/S/PDIF block before
the analog block and sometimes after it; the operator treats this as expected
for now (issue #444). The table above and `scripts/rig/hosts/pupu.env` assume
**analog first** — `capture_1..8` / `playback_1..8` = AN1..AN8 — which is what
2026-09-14 measured (silent 18-channel `jack_rec` at 48 kHz, and the FF400
`meter:analog-output` / `meter:stream-input` controls during a tone). The
port count depends on the rate — 18 capture ports at 48 kHz, 14 at 96 kHz —
so a moved block does not land at a fixed offset.

Silent check: with nothing on ADAT/S/PDIF, those captures read **exact
digital zero** and analog inputs never do, so the live channels show where
the analog block sits. `preflight.sh` reports it and every emitting script
refuses to run when the block is not at `capture_1..8`; the playback order has
matched the capture order whenever both were checked, which is an
observation, not a guarantee. If the block has moved, do not quietly edit the
profile to chase it — record it and settle the ports with the operator. JACK
aliases are not evidence — the old `FF400:capture_ADAT1` aliases were written
by the helper script itself.

## `ac` configuration

- `~/.config/ac/config.json`: `output_channel 0`, `input_channel 0`,
  `reference_output_channel 1`, `reference_channel 1`, and no
  `drive_max_dbfs` key — #459 retired it, and while it is present every
  emitting command refuses (`preflight.sh` FAILs on it). No calibration;
  levels are dBFS only.
- **Ceilings (nominal dBFS): −40 standing, −50 on anything that drives the
  speaker** (operator, 2026-09-14). The daemon has no configurable ceiling:
  since #459 it refuses only above full scale and never clamps. Both limits
  are enforced only by `scripts/rig/` (`RIG_DRIVE_CEILING_DBFS`,
  `RIG_SPEAKER_CEILING_DBFS`) and, for manual commands, by whoever runs
  them. Emission rules: `.agents/rig.md` hard constraints.
- Level expectation (1 kHz, mic at 1 m, IN1 gain 20 dB): mic reads about
  4 dB under the drive level, ~1:1 from −54 to −40 dBFS (measured at
  48 kHz); a −60 dBFS check at 96 kHz read −63.7 dBFS.
- **A −40 dBFS Farina sweep through the 1083 is loud** — louder than a 1 kHz
  tone at the same nominal level, because the sweep dwells in the low
  octaves (woofer, room modes) and in 2–5 kHz. The nominal level is the sweep
  amplitude (RMS 3 dB lower). Hence the −50 dBFS speaker ceiling —
  expect ~10 dB less acoustic SNR than the 25.6 dB measured at −40 (below),
  marginal on a noisy day.

## Measured on this setup (2026-09-14, build 0891cf92, 96 kHz / 256 / 3 / `-S`)

Taken while verifying `scripts/rig/`; one run each, so a first reading, not a
repeatability claim. Absolute offsets only compare within one JACK client's
lifetime — a restarted client can land a period away.

| run | chain | level | peak offset from window centre | SNR |
|---|---|---|---|---|
| `run-loopback-ir.sh --route ref` | AN2 → IN2 cable | −40 dBFS | +1727 smp = **+17.99 ms** (round trip) | 32.8 dB |
| `acoustic-ir.sh` | AN1 → 1083 → mic at 1 m | −40 dBFS (before the −50 speaker ceiling) | +2200 smp = +22.92 ms | 25.6 dB |

Acoustic onset at 50 / 25 / 10 / 5 % of peak: +21.98 / +21.69 / +19.66 /
+18.79 ms. Do not subtract the loopback's 17.99 ms from these: τ is per
channel pair, and the loopback (AN2 → IN2) is not the acoustic pair
(AN1 → IN1). The 5–10 % onsets sit earlier than 1 m of flight allows, so
they are reading pre-ringing or noise, not the arrival.

Wiring probe the same session (1 kHz, −60 dBFS): AN1 → mic −65.7 dBFS, IN2
−133.1 dBFS; AN2 → IN2 −57.5 dBFS, mic −106.0 dBFS.

## Background noise

Varies a lot between sessions — snapshot at the start of each
(`scripts/rig/noise-snapshot.sh pupu`). Reference points:

- 2026-09-14 18:44, operator present, 96 kHz: −52.6 dBFS broadband, loudest
  band 63 Hz at −55.3 dBFS, 1 kHz −72.4 dBFS.
- 2026-09-14 18:53, 96 kHz: −67.1 dBFS broadband, loudest band 63 Hz at
  −71.3 dBFS, 1 kHz −80.3 dBFS — 15 dB quieter within ten minutes, which is
  why a snapshot belongs to every session.
- Broadband low-frequency (peak 63–250 Hz), not mains hum; the FF400's own
  analog floor is ~−108 dBFS, so the mic floor is acoustic. Below ~250 Hz a
  −40 dBFS sweep has little margin.

## What the low-frequency floor does to arrival estimates (2026-09-21)

Measured while running the #537 rig check at ≈0.5 m, mic on IN1, speaker on
AN1, default band. Both facts are properties of this room's floor, not of
one build:

- **`BandLimitedSnrLow` is unreachable from the CLI here.** The arrival
  gate refuses below 35 dB of arrival SNR, but the older pre-impulse SNR
  gate (18 dB, #501) refuses the whole deconvolution first — about 50 dB of
  arrival SNR earlier. `ac plot ir` prints `DECONVOLUTION FAILED` and
  `ac-view` shows a low-SNR fault, so no arrival row is ever reached. Drive
  level cannot separate the two: at −85…−98 dBFS typed, arrival SNR landed
  at 32–45 dB while the CLI still refused every capture. Scoring the
  band-limited gate on this rig means reading the saved report, not the
  terminal.
- **The broadband cross-check disagrees on captures the CLI has already
  refused.** With the unfiltered IR only 5.5–12.6 dB above the pre-impulse
  floor, its peak lands on low-frequency noise: of 13 captures at ≥35 dB
  arrival SNR, 2 read `Agrees`, 6 `BroadbandLater` and 5 `BroadbandEarlier`,
  while every band-limited flight time produced was +199 exactly. Treat a
  broadband standing as meaningless below the 18 dB pre-impulse gate.

Unmeasured, and the reason this is not a defect report: the cross-check's
behaviour between 18 and ~22 dB of broadband pre-impulse SNR, the band a
user does see. There it could withhold a correct number, but it cannot
change a printed one.

## JACK stability (2026-09-14)

Loaded 60 s runs, capture-only `ac monitor 0-7 --tui`:

| rate | period | n | mode | tuning | steady xruns |
|---|---|---|---|---|---|
| 48000 | 256 | 3 | async | none | 1726 |
| 48000 | 256 | 3 | async | none, load = plain `jack_rec` | 284 |
| 96000 | 1024 | 3 | async | none | 1419 |
| 96000 | 1024 | 3 | async | IRQ 90, `performance`, C3+ off | 905 |
| 96000 | 1024 / 256 | 3 | `-S` | IRQ 90, `performance`, C3+ off | 0 |
| 96000 | 256 | 3 | `-S` | none | 0 (also over a 300 s soak) |

Async mode fails regardless of period length, so it is not a CPU budget:
the log is `ProcessGraphAsyncMaster: Process error` plus `client … was not
finished`, even for trivial `jack_rec`, with no ALSA overruns — consistent
with irregular wake-ups from the FireWire stack (isochronous work runs in the
non-RT `firewire-isoc-card0` workqueue). The `ac-daemon` RT callback is lock-
and allocation-free.

## Open items

- Quiet-period noise snapshot (operator absent) not yet taken.
- Level ramp not redone at 96 kHz; no long play-and-capture soak yet.
- Mic model, the speaker reference point the 1/2/3 m marks are measured
  from, and mic height are not recorded.
