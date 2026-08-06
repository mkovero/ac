# handoff-rig-findings — issues to file from the 2026-07-28 session

> **Filed 2026-07-28.** A → **#225**, B+E → **#226**, C → **#227**,
> D → **#228**. Follow-up testing after this document was written changed
> three of its conclusions; the affected sections are marked **[revised]**
> below and the corrections are inline. F is not filed — its central claim
> did not survive a repeat.

Source: `work/rig/rig-session-results.md`, plus two operator observations after it:

- Pink noise driven from `ac transfer` does not reach the loopback output
  (output 0) at all.
- LF looks correct, HF is dead. Changing input gain or moving the microphone
  does not change it.

Each issue below separates **established** (measured, with the run that
measured it) from **speculated** (reasoning, marked as such) and states what
would falsify it. File as separate issues — they have different fixes and
different owners.

---

## Issue A — the drive does not feed the reference output **[revised]** → #225

**Priority: highest.** Root cause now identified, and it is a one-line type
confusion rather than a missing feature.

**Root cause (code, `main` @ bd40ed4).** `resolve_ref_output`
(`handlers/mod.rs:263`) resolves the reference **output** port from
`cfg.reference_channel` — which is the reference **input** (capture) channel
everywhere else in the codebase. `Config` has no reference-output field at
all (`config.rs:41-56`). On this rig `reference_channel: 2` means capture_3 =
IN3, the loopback *return*, so the daemon opens **playback_3** as the
reference output while the loopback source is **playback_1**. The leg exists
and is connected — to the wrong port. `drive_out_ports`
(`handlers/transfer.rs:73`) opens both legs whenever they differ, so this is
driven, not merely named.

**Correction to the section below.** "This is a missing leg, not a timing
race — the earlier 'route arrives too late' reading was wrong" is itself
wrong on both halves. The leg is not missing (`main` has `resolve_ref_output`
and connects it); it points at the wrong port. And the timing race is real,
independently reproduced, and is the more damaging of the two — see #226,
where a session with a fully working reference leg still draws a dead top end
because it locked on silence before the stimulus arrived. Fixing A alone does
not fix the operator's symptom.

**The 66.96 ms figure** is the operator's own later run, not from
`work/rig/rig-session-results.md`. Attribution added at their confirmation.

**Established.** The installed daemon has no separate reference-output leg:
the start reply carries no `ref_out_port` and frames carry no `conn_tags`
(rig report, "Wiring as actually used"). The session's own results were only
obtainable by starting a standalone `generate_pink` worker on
`channels: [0, 4]` *before* the transfer session. Driving from within the
session and patching afterwards gave 18.09 / 25.22 / 25.41 ms for one
unchanging position across three sessions.

**Established.** With the stimulus already flowing at sample zero, the same
position repeats to one sample (0.0105 ms spread over 8 sessions).

**Confirmed by the operator.** `ac transfer` does not output to the loopback
output at all. Every session that produced a reference signal did so because
the route was added by hand in JACK. This is a missing leg, not a timing
race — the earlier "route arrives too late" reading was wrong.

**Confirmed quantitatively** (operator's own run, not the session report).
An affected session read **66.96 ms** against a
validated 4.59 ms at that position, leaving 62.4 ms of residual misalignment:

| stage | window | coherence ceiling `((W−D)/W)²` |
|---|---|---|
| 0 | 42.7 ms | **0.000** |
| 1 | 341 ms | 0.668 |
| 2 | 1024 ms | 0.882 |

**[revised] The table is a model, and it fails at HF.** Checked against
measured data: at a 24.5 ms residual it predicts 0.95 / 0.86 / 0.18 where
0.93–0.95 / 0.77–0.87 / **0.054** was measured — the lower two stages match,
the top stage is over-predicted 3×. At a 6 ms residual it predicts ~0.55
against 0.11 measured. Window-overlap loss is not the dominant HF mechanism;
intra-band phase dispersion is (6 ms at 20 kHz is 120 cycles). The
"zero at stage 0" conclusion holds, but the formula must not be used to
predict intermediate HF residuals.

The residual exceeds the top stage's entire window, so its coherence is zero
by construction — gone, not degraded. That is the operator's "LF legit,
nothing on HF", and it is insensitive to gain and microphone position because
the lock is cached (Issue E).

The manual patch landed after the capture rings had filled, so the delay was
estimated against a reference that did not yet exist — the same failure the
session's method 1 produced, with a different random draw.

**Workaround until fixed:** a standalone `generate_pink` worker on
`channels: [0, 4]`, started *before* the transfer session. That gave 4.59 ms
repeatable to one sample across eight sessions.

---

## Issue B — a delay estimated against a silent or uncorrelated reference is
## cached silently for the session

**Established.** The per-pair delay is estimated once when the capture rings
first fill, then cached (rig report; `transfer.rs` `pair_delays`). No validity
check exists — an estimate against silence is accepted and reused.

**Established.** The consequence is frequency-selective. A wrong lock leaves
stage 2 at 0.93 and stage 0 at 0.05 (Run 1's comparison table), because the
top stage's window is 42.7 ms at 96 kHz and the bottom stage's is 2.5 s. The
fault is close to invisible below a few hundred Hz.

**Established.** Pair 1 in Run 5 locked to 494 ms on an input carrying no
correlated content at all — the estimator returned a confident number for a
pair that had nothing to estimate.

**Suggested direction, not prescribed.** Refuse to lock rather than lock on
noise: require the correlation peak to clear a prominence threshold, and
report "no lock" when it does not. A displayed non-answer is recoverable; a
confident wrong answer is what produced this session's worst data.

---

## Issue C — the delay estimator takes the global correlation maximum, so
## reflections win

**Established.** 5 of 8 sessions at one position locked to non-physical
delays — 22.78, 30.34, 30.45, 30.43 ms at a microphone under 1.5 m away, and
4.18 ms after moving *away* from the source. At a more distant, off-axis
position, sessions split into two clusters 14.5 ms apart (Run 1).

**Established.** `estimate_delay_samples` takes the maximum of the
cross-correlation. In a room the direct-sound peak is not always the maximum.

**Established.** Lock reliability tracks *electrical* SNR at fixed geometry:
4/6 → 5/6 → 6/6 valid across 20 dB of input gain, with room, distance and
direct-to-reverberant ratio unchanged (Run 7).

**Established.** Headless tests feed one unambiguous peak, so no existing test
can see this.

**Suggested direction.** Prefer prominence over a plausibility window. A
physical window needs a distance assumption the software does not have, and
Run 7 shows electrical SNR feeds the same failure at fixed geometry — a
prominence threshold covers both causes, a distance window covers neither.
"Earliest prominent peak" rather than "global maximum" also matches what the
direct sound physically is.

**Falsify the fix** against Run 1's data shape: a fix that does not turn the
22.8 / 30.3 / 30.4 ms locks into either a correct lock or a refusal has not
worked.

---

## Issue D — a wrong lock is diagnosable but not surfaced

**Established.** Top-stage coherence separates the two cases cleanly: 0.715 –
0.755 with a correct lock, 0.05 – 0.06 with a wrong one, at the same position
in the same room (Runs 1 and 3).

**Speculated.** That this is a good enough discriminator to drive a UI
warning, given a settled ladder and a plausible drive level.

The operator's report — LF fine, HF dead, unresponsive to gain and position —
is precisely this condition, arrived at without any instrument telling them
so. Whatever the eventual fix to C, a session running on a bad lock should say
so rather than draw a confident dead top end.

Related but distinct from #224, which labels resolution and settling. This is
a fault indicator, not a scale.

---

## Issue E — the delay is never re-estimated within a session

**Established.** Cached at warmup, no re-estimation (rig report; also the
existing note that `reconnect_input` clears the measurement ring only, named
in PR #217 and not folded in).

**Established consequence.** Moving the microphone mid-session leaves a stale
lock, which is why the operator saw no change from moving it. A new session is
required per position — Run 1 had to open a fresh session for every distance.

**Open question, for the architect.** Whether re-estimation should be
automatic (on `reconnect_input`, on a coherence collapse, periodically) or
explicit (an operator action). Automatic re-estimation mid-measurement changes
the alignment under a running average, which is its own hazard.

---

## Issue F — **[revised] the drift did not survive a repeat. Not filed.**

A 65 s stationary repeat (same position, same drive, nothing moving) settles
this, and both the original claim and the proposed explanation were wrong:

- **`n_blocks` is constant at 4 for the whole run.** Blocks averaged per
  stage are held uniform by design (`handlers/transfer.rs:240`), so there is
  no 1/N convergence. The average-accumulation explanation offered in review
  is dead.
- **There is no monotone trend.** Across thirteen 5 s windows the 258 Hz step
  scatters ±0.04 around ~0.10 — 0.130, 0.105, 0.105, 0.079, 0.078, 0.103,
  0.107, 0.057, 0.144, 0.089, 0.102, 0.088, 0.115 — with no direction.
- **The sign is not stable across sessions.** This run reads the 258 Hz step
  **positive** throughout; the run in the original report read it
  **negative** (−0.143 → −0.091) at the same position. A sign flip between
  sessions means the metric is dominated by room response, not by an
  estimator property: three columns either side of 258 Hz spans about 1/16
  octave, which is where modal structure lives.
- **The upper crossover holds in both runs** — +0.05…+0.084 here, +0.091
  stable earlier. Present, right order, no drift.

Conclusion: the documented step is confirmed at the upper crossover. The
lower-crossover "drift" is an artifact of a metric too narrow to measure it.
Anyone revisiting this needs a wider-band metric first — a claim about
estimator bias cannot be made from three columns at 258 Hz.

The original text follows, retained for the measurements it records.

## Issue F (original text) — the lower crossover's coherence step drifts

**Established.** At the 258 Hz crossover the step is *negative* — coherence
higher above the crossover than below — and drifts −0.143 → −0.091 over 25 s,
still moving at the end (Run 4). The upper crossover at 2064 Hz is stationary
to ±0.003 over 40 s.

**Established.** Settling does not explain it. Stage 2 settles at 2.56 s; the
step is still moving between the 10–25 s and 25 s+ windows, ten times past
that.

**Excluded.** Room movement — the operator was stationary throughout.

**Not a defect on its face.** The negative sign is probably acoustic rather
than an analyser property: stage 1 sits at 0.970 and stage 2 at 0.934, and at
258 Hz room modes make low-frequency coherence genuinely worse. The documented
+0.05 was measured where true coherence was uniform across the crossover, and
live γ² near the upper crossover is ~0.97 rather than the 0.5 the documented
figure used — the two are not directly comparable.

**The drift is the open part**, and it now has no candidate explanation.
Worth one repeat before anything is concluded: same position, same drive,
60 s, nothing in the room changing.

---

## Also worth recording

- **`conn_tags` absent** from every frame this daemon publishes, so #205's
  drive-path check was unavailable all session. Per the field's own contract
  that must read as *unknown*, never as healthy. Confirm that is what happens.
- **Clock source is `AutoSync`, not `Internal`.** No drift observed — eight
  sessions agreed to one sample — but set it to Internal before the next
  session to remove it from consideration.
- **Stage 0's 0.755 is reverberation-limited, settled.** Flat to 0.006 across
  20 dB of gain (Run 7). Not a defect, and gain cannot improve it. Worth
  stating somewhere durable so it is not re-investigated.
- **Run 2's A/B had no positive control**, and the reason is a specification
  error in `work/handoff/handoff-rig-session.md`: it asked for a level step rather than a
  finger snap, on the grounds that a count needs a repeatable stimulus. But
  the mechanism needs a *short* transient — the four maxima come from a delta
  traversing the analysis window, and a 6 s step is longer than the window, so
  its edge produces a monotone ramp on either build. A ~50 ms gated burst is
  both repeatable and impulse-like. #208 is adequately closed by other
  evidence (the stale `ac-view` repeated, the current one does not), but not
  by that A/B.
- **Rig state: torn down.** The `~/ac-ctrl` worktree, `~/target-ctrl` and the
  control daemon on 25556/25557 are all removed; the card's mixer is back to
  its session-start values and the system daemon is untouched.
- **#208 is closed** (operator, after Run 2). The proposed ~50 ms burst
  re-test was dropped on that call and not run.
