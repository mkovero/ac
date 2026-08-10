# handoff-rig-session — everything that was blocked on the loopback

Rig: 192.168.9.25, Babyface Pro, 96 kHz, JACK.

**Wiring as built:** outputs 1 and 2 looped to inputs 3 and 4 (AN1→IN3,
AN2→IN4). Microphones on inputs 1 and 2.

That gives measurement ≠ reference with a real acoustic path for the first
time: reference is the electrical loopback on input 3, measurement is a mic on
input 1. Two mics means two pairs against one reference, which also exercises
the multi-pair fan-out on real hardware.

Everything below was blocked on exactly that and nothing else.

---

## Before starting

- `./install.sh` must actually ship the new `ac-view` binary to the system
  location. It silently did not once, and the symptom was the old display with
  all of its faults, which reads as "the fix didn't work". Verify the binary
  timestamp, not the build output.
- Restart the daemon. It is long-running, and an old one publishes frames
  without the ladder — the transfer view then correctly draws nothing.
- Use `ac transfer`, not `ac monitor`. `commands/monitor.rs:42` passes
  `false`, which opens the descoped spectrum view. That is a separate one-line
  fix and not part of this session.

## Emission

Acoustic now, not electrical. −40 dBFS into a loopback and −40 dBFS into a
loudspeaker are different propositions, and the level that gives usable
microphone SNR may be higher than the level used for the electrical runs.
That call is yours per run — the rule is that the level is chosen
deliberately and recorded with each result, not inherited from the earlier
sessions.

---

## Run 1 — delay tracks distance

**Do this first.** It is the cheapest end-to-end validation available and it
uses physics as the reference rather than the software's own numbers.

Note the delay reading. Move the microphone 34 cm further from the source.
The reading should increase by 1.0 ms.

- Tracks → the whole chain is sound: alignment, decimation, the ladder, and
  the delay estimate all agree with the speed of sound.
- Doesn't track → stop. Nothing further in this session means anything until
  it does, and the fault is upstream of everything else being checked.

Record two or three distances rather than one, so a constant offset is
distinguishable from a scale error.

## Run 2 — criterion 10, the #208 recurrence check

The one that has been owner-held since the beginning.

**Stimulus: a repeatable level step** — gate the drive on and off. Not a
finger snap. The check is a *count*, and comparing counts across two
transients that were never identical proves nothing.

**A/B against `main` before the merge.** Same stimulus, same setup, both
builds. One episode on the new build only means something if the old build
shows four.

Watch **transfer magnitude**. That is where the repeats live.

You have already observed informally that `ac transfer` does not repeat. This
run makes that a recorded result with a control, which is what the criterion
asks for.

## Run 3 — alignment on a real delay (#216's general half)

The mic sits several milliseconds from the source, so this is the first time
the alignment path faces a delay it did not synthesise.

QA verified coherence stays at 1.0000 across 0/10/20/40/100 ms headlessly. On
hardware, check the **top stage specifically** — its window is 42.7 ms at
96 kHz, so without working alignment its coherence collapses to zero by
50 ms of delay. High-frequency coherence near 1 with the mic at a normal
working distance is the result that matters.

This is the half of #216 that lives inside #218. If it holds, #216 closes
entirely.

## Run 4 — the documented coherence step

`docs/design/design-mtw-ladder.md` records a step of about 0.05 at the crossovers,
measured headlessly. Confirm it is present and of that order on real data,
and that it does not move as the ladder warms.

Do not treat its presence as a failure — it is accepted and documented, and
the reason is structural. Its *absence* would be the surprise, and would mean
the documented figure needs revisiting.

## Run 5 — two pairs

Both microphones against the same reference. Different positions, so the two
pairs should show **different delays** and different responses.

The failure this looks for is the pairs being coupled — identical delays,
identical curves, or one pair's alignment leaking into the other.

## Run 6 — settling on the acoustic path

Warmup was measured electrically at 0.070 / 0.824 / 2.541 s against analytic
0.107 / 0.853 / 2.560. Confirm the same behaviour on the acoustic path, and
that the curve fills top-down against a stationary axis rather than the axis
rescaling.

---

## Recording

For each run: sample rate, drive level, mic distance, and the raw numbers.
`AC_DRAIN_TELEMETRY=1` for anything where realtime behaviour is in question.

Runs 1 and 2 are the ones that decide something. Runs 3–6 are confirmations
of results that already exist headlessly; if one disagrees, the headless
result is what gets re-examined, not the hardware.

## Not in this session

- **#224** (per-band Δf and settling labels). Not rig-dependent. UX flagged it
  should land before the ladder is used to tune a real system, because
  resolution and settling vary 24× across one screen with nothing saying so,
  and the first bad report about "the bottom lagging" is the kind that sticks.
- **`ac monitor`'s default view.** One line in `commands/monitor.rs`.
- **#221** snapshot parity. Becomes real now that the live view runs the
  ladder; still deferred.
