# #243 acceptance criterion 7 on the rig — PR #356

Ran 2026-08-23 on 192.168.9.25, jackd-direct 96 kHz, period 64 / 2.
Binaries `ac-daemon-issue-243` / `ac-issue-243` at `647d115`, verified by
sha256 against the local build before use. Isolated `HOME=~/rig243-home`
(`drive_max_dbfs: -30.0`, `temperature_c: 26.0`). Stimulus by external
`generate_pink` worker at **-30 dBFS**, per-run consent given, started
before every session (issue #226: the lock is cached from the first full
ring, so a session that starts on silence stays wrong).

Wiring, unchanged from the #243 block: `playback_5` (master out, through
the analogue master section) → converter → loudspeaker; `playback_7` →
converter → `capture_3` as the reference leg; `capture_1` = measurement
mic. Pairs `[[0,2],[2,2]]`.

## What was measured

| taped | delay | samples | locked frames | spread |
|-------|-------|---------|---------------|--------|
| 3.000 m | 9.635417 ms | 925 | 476 + 256 (two independent sessions) | **0** |
| 1.000 m | 3.937500 ms | 378 | 476 + 376 (two independent sessions) | **0** |
| electrical control `[2,2]` | 0.000000 ms | 0 | 951 | **0** |

The control pair locked at exactly 0 samples in every frame of every
session — playback/capture buffering is common-mode and cancels, as the
converter-constant memo says it should.

Both acoustic positions produced a single sample value across every frame,
in two independent sessions each. There is no scatter to average down; the
estimator is repeatable to better than one sample here.

Constant derived at 3.000 m, against the frame's own
`speed_of_sound_m_s = 347.056` (`331.3 + 0.606·T`, `T = 26.0`):

```
constant_ms = 9.635417 − 3.000/347.056·1000 = 0.991279 ms
```

Stored by hand into `~/rig243-home/.config/ac/cal.json` under key
`out4_in0`, `ref_channel: 2`, `setup_id: "mic1-spk-adat-ref3-2026-08-23"`
— see finding 1 for why by hand.

## Verdict

**Pass at 5 cm. Fail at the 5 mm the criterion as written asks for.**

Readout at the taped 1.000 m verification point: **1.023 m**, error
**+23 mm**.

The 5 mm bar is not physically meetable on this rig and should be restated:

- One sample at 96 kHz is **3.6 mm**. The bar is ±1.4 samples.
- Temperature costs **~3.4 mm/°C** over a 2.000 m move. With no thermometer
  in the loop, ±1.5 °C spends the entire budget — the gate would be scoring
  the operator's guess at room temperature, not the instrument.
- Taping to 1 mm against a mic capsule whose acoustic reference point is not
  marked is not a real measurement.

5 cm is not a climbdown to make this pass: it is **this rig's own
demonstrated figure**. Rig session 3 fitted the constant at 1.000 m and
checked five further positions, every on-axis one agreeing to ≤5 cm. The
5 mm number originates in the issue's UX comment reasoning about how many
decimal places to print, and was promoted from a display-precision argument
into an acceptance criterion without a measurement behind it.

At 5 cm the observed 23 mm passes with better than half the tolerance in
hand, and the temperature term would need to be ~15 °C wrong to threaten it.

### The residual, for the record

Taking both taped positions together, 2.000 m of air cost 5.697917 ms,
implying **c = 351.01 m/s** against the assumed 347.056 — a 1.14% scale
error, which is the whole of the 23 mm. Reconciling it would need the room
at 32.5 °C rather than the configured 26.0.

Three candidates, not separable from two positions: a stale `temperature_c`;
a distance-proportional estimator bias (a constant one cancels into
`constant_ms`, only a scaling one survives); or 23 mm of tape error over the
move. **Not chased** — at a 5 cm bar it does not change the verdict, and
settling it needs a third taped position and a real thermometer. Worth its
own block only if the criterion is ever tightened again.

## Findings against PR #356

1. **No write path for `distance_cal_history`.** `distance_setup_id` /
   `distance_plausible_max_m` appear nowhere in `ac-cli/src`; nothing in the
   tree creates a `DistanceCalEntry`. The constant here was hand-written into
   `cal.json`. The PR builds the store, the lookup, the refusal and the wire,
   but #243's actual ask — "a calibration procedure, not a constant in the
   source" — is not met.

2. **The calibrated readout is unreachable from any shipped UI.**
   `distance_setup_id` appears nowhere in `ac-view/src` either, so
   `ac-view --transfer` always launches without one, so `distance_cal` is
   always `null`. The only row an operator can reach today is
   *"not calibrated"*. The calibration-provenance row and the plausibility
   warning — the warning being the entire point #243 was filed on — cannot
   be reached except by a raw ZMQ client. This is broader than the PR's own
   "no CLI flag yet" note.

3. **`distance_setup_id` is mutually exclusive with the zero-flight control
   pair.** Requesting a setup id refuses the *whole* session if any pair
   cannot resolve one — including `[2,2]`, the reference correlated against
   itself, which has no acoustic distance and never can have a calibration.
   The rig's standing practice of carrying the electrical control in every
   session at zero cost (it is the evidence that buffering cancels) is
   therefore impossible in any session that asks for a distance readout.
   Refuse-the-whole-request is right for acoustic pairs and wrong for a
   self-pair. Observed error:

   ```
   distance_setup_id "…" for pair meas2/ref2: no distance calibration
   recorded for ref channel 2 yet — capture one at a taped distance before
   requesting setup "…"
   ```

4. **`DistanceCalEntry::setup_id`'s doc is wrong about position.** It says
   the id identifies "which mic, which loudspeaker, which position". If it
   encoded position, the exact-match lookup would refuse at every position
   other than the capture one, and a corrected distance readout — the whole
   feature — would be unreachable by construction. Position must be
   *excluded* from `setup_id`. Doc bug, not a code bug.

5. **`captured_distance_m` never reaches the display.** The wire carries it
   and `ac-core`'s doc calls it "what lets a reader tell a fresh capture from
   a stale one", but `ac-scene`'s `DistanceCalibration` drops the field, so
   no readout can show it. Minor.

## What did paint

`ac-view` itself would not start on that box — `glutin` `BadAttribute` on GL
context creation, a windowed-GL problem unrelated to this PR (the offscreen
wgpu snapshot path ran there fine on 2026-08-20). The four display branches
were instead driven through the real `format_delay_readout` with the frames
actually measured above:

```
3.000 m taped:      9.64 ms    3.000 m
                    cal 2026-08-23 · mic1-spk-adat-ref3-2026-08-23 · pair meas0↔ref2

1.000 m taped:      3.94 ms    1.023 m
                    cal 2026-08-23 · mic1-spk-adat-ref3-2026-08-23 · pair meas0↔ref2

1.000 m, uncal:     3.94 ms
                    not calibrated — pair meas0↔ref2

1.000 m, ceiling 0.5 m:
                    3.94 ms    1.023 m
                    cal 2026-08-23 · mic1-spk-adat-ref3-2026-08-23 · pair meas0↔ref2
                    readout exceeds plausible bound for this pair — check wiring or re-calibrate
```

All three rows are correct, and the warning fires exactly when the ceiling
sits below the reading. The refusal path was exercised live too: a
mismatched `distance_setup_id` refused the request synchronously and named
both the requested and the stored id.

## Rig state afterwards

Daemon stopped, tunnel closed, nothing emitting. Mic preamp gain (numid 301
= 36), 48 V, PAD and `Sample Clock Source` (AutoSync) were read but never
written. The pre-existing `bin-350` daemon on 5556/5557 was left running
untouched; this session used 5566/5567. `~/rig243-home/.config/ac/cal.json`
retains the measured constant.
