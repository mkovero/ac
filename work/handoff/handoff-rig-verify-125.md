# handoff-rig-verify-125 — verifying the rig2 fixes at a fixed 3 m position

Rig: 192.168.9.25. Branch under test: `rig2-fixes-125`. Baseline for comparison:
`work/rig/rig-session-2-results.md`, position 3.

**The microphone is fixed at ~3 m on axis and is not to be moved.** That is
position 3 from session 2, and it is the position where the failure this branch
claims to fix actually occurred. The session is sharp without moving anything.

---

## Why this position is the right one

Session 2, position 3: **7 of 7 sessions locked, 2 of them wrong** — 18.43 ms
and 14.00 ms against a true arrival of 11.34 ms.

The true value is not asserted, it is geometry-confirmed: the arrival moved
+5.4 ms from position 1, ≈ 1.85 m of extra flight, consistent with the physical
move. Four of the seven sessions agreed on 11.34 ms to the sample.

So there is a known-correct answer at this position, and a known failure rate
against it.

## What cannot be tested here

Finding 4 — that valid and invalid prominence distributions overlap — needs
1 m for comparison and cannot be settled from one position. Do not attempt to
conclude anything about the gate's correctness from this session.

---

## Before starting

- **Clock stays `AutoSync`** (`numid=320 = 0`). The external master clocks the
  card over ADAT, and ADAT carries playback_5 — the stimulus leg. Setting
  Internal breaks the speaker path. Do not "fix" this.
- **Verify installed binaries by sha256**, not size and mtime. Both passed on a
  stale binary last session.
- `install.sh` does not ship `ac-view`; copy it by hand.
- Wiring: loopback is **playback_2 (AN2) → capture_4 (IN4)**. capture_3 is
  digitally silent. Session config wants `reference_channel: 3`,
  `reference_output_channel: 1`.
- `~/.config/ac/config.json` is wrong for this rig — use an isolated `HOME` as
  session 2 did (`~/rig2-home`, `drive_max_dbfs: -30.0`).
- Take a **contemporaneous silent baseline** before and after. The room floor
  moved ~10 dB across session 2's evening, and every level comparison depends
  on it.

## Emission

−30 dBFS nominal pink, per-run consent from Markus, clamp enforced
server-side. Stop emission between runs.

---

## Run 1 — the wrong-lock fix (the point of this session)

Six to eight fresh sessions. The lock is cached per session, so each attempt
needs a new one. Stimulus on a standalone `generate_pink` worker started
**before** the session, so the rings never fill against silence.

For each session record: locked delay, `delay_locked`, `delay_prominence`,
`peak_lag`, `noncausal_peak_lag`, `noncausal_peak_value`,
`negative_lag_median`, and the full `delay_evidence` list.

**Pass:** every lock that occurs is at 11.34 ms ± a sample or two.

**Refusals are acceptable.** A lower lock count than session 2's 7/7 is an
expected outcome, not a regression — some of session 2's locks were non-causal
peaks at high prominence, and 2 of the 7 were simply wrong. **Judge on
physical plausibility and geometry consistency, never on count.**

**Fail:** any lock at 14.00 ms, 18.43 ms, or any other value inconsistent with
3 m of flight. That is the regression surviving.

## Run 2 — evidence completeness (fix 2)

From the same captures, check the property session 2 could not satisfy: **the
accepted lag is present in its own candidate list.**

In session 2 this failed in *every* position-3 session — the list ran
1815–3335 while the lock was at 1081. If it still fails, offline threshold
tuning remains impossible and that blocks finding 4 permanently.

Also confirm: lags unique, strictly ordered, at most 35 entries.

## Run 3 — the two numbers worth having

Cheap, from the same sessions, no extra setup.

**Recomputed prominence at this position.** Thresholds are now measured
against the strongest *causal* peak, so session 2's 13.6–27.8 range is in a
different definition and is **not comparable**. This gives the first numbers in
the new definition, at the position where wrong locks actually happened.
Record them as a new baseline, not as a delta against session 2.

**`negative_lag_median` alongside the all-lag median.** This is the input to
the open question behind finding 4: whether the noise floor should be measured
over negative lags only, since a causal path puts no signal there and the
all-lag median is contaminated by reverberation. One position cannot settle it,
but 3 m is the reverberant end — where the contamination is worst and the
difference between the two medians should be largest. That makes it the more
informative half of the eventual comparison.

## Run 4 — the fault indicator, if time allows

Findings 3 and 5 from session 2 are **not** fixed by this branch, so:

- `LOST LOCK` / `NO LOCK` remain structurally unreachable. A refusing session
  still renders a blank window with no indicator. Expected. Confirm rather than
  investigate.
- `CHECK ROUTING`'s threshold changed from all-columns to <10% alive. If
  inducing unrelated legs is quick (reference off an independent pink
  generator), confirm it now fires. Session 2 measured 4.4% alive on genuinely
  unrelated legs against near-total when healthy, so 10% should sit between
  them — but that is a desk number.

---

## Not testable here, and still owed

- **The onset case.** The onset guard was dropped from this branch because it
  could not be falsified and its own test — oldest eighth silent — is
  permanently true of a gated stimulus, which would have silently prevented
  #208's 50 ms burst from ever locking. The remaining risk is a confident wrong
  lock at a *positive* lag when the stream starts before the stimulus. It is
  the only case causality cannot catch. Testing it means deliberately starting
  the stream first — worth doing if there is time, and it needs no mic
  movement.
- **Run C positions 1, 2, 4, 5.** Blocked on the mic being fixed. Finding 4
  stays open.
- **Run D** — #208's positive control, 50 ms gated burst against `cda40ef`.
  Needs no mic movement either, if there is time.
- **1 m and 3 m back-to-back with contemporaneous baselines.** Session 2
  measured them hours apart, which confounds room drift with distance and may
  be the whole explanation for the unexplained 1/12-versus-7/7 inversion.

## Recording

Per session: sample rate, drive level, input gain, silent baseline before and
after, and the fields listed in Run 1. Write results to a new
`work/rig/rig-verify-125-results.md` rather than editing session 2's file — the two are
in different threshold definitions and should not be merged into one table.
