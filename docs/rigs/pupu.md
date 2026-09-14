# Rig profile: `pupu` — dedicated audio HIL rig

Facts about this machine only. **How to test on it** is
`docs/runbooks/rig-testing.md`; the machine-readable form of this page is
`scripts/rig/hosts/pupu.env` — change both together.

Last verified: 2026-09-14.

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

- Mic static at **1 m** on the speaker axis; tape marks on the floor at
  **2 m** and **3 m** for manual moves. Put it back at 1 m after a move and
  state the position in the run record.
- The 1083 has intermittent distortion, obvious by ear when present. A run
  nobody listened to needs a THD check before it is trusted.
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
  line in *Low*, phantom off, gains 0); JACK restarts and rate changes do
  not. Restore — gains before phantom:

  ```sh
  C=Fireface400
  amixer -c $C cset numid=93 2; amixer -c $C cset numid=94 2; amixer -c $C cset numid=89 2
  amixer -c $C cset numid=81 0,0; amixer -c $C cset numid=90 on,off; sleep 2
  amixer -c $C cset numid=81 20,0; amixer -c $C cset numid=102 on
  ```

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
  `reference_output_channel 1`, `reference_channel 1`, `drive_max_dbfs
  -40.0`. No calibration; levels are dBFS only.
- **Ceilings (nominal dBFS): −40 standing, −50 on anything that drives the
  speaker** (operator, 2026-09-14). The daemon's `drive_max_dbfs` is −40 and
  cannot tell outputs apart, so the −50 speaker ceiling is enforced by
  `scripts/rig/` (`RIG_SPEAKER_CEILING_DBFS`) and, for manual commands, by
  whoever runs them. Emission rules: `.agents/rig.md` hard constraints.
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
