# Rig verification queue — everything that needs 192.168.9.25 and cannot be run yet

Written 2026-08-03 alongside branch `rig2-fixes-125` (findings 1, 2 and 5 of
`handover.md`). Nothing here has been run: the operator is off site. This file
exists so the next session at the rig does one trip, not three.

Order matters below. Each block states what it verifies and what a pass looks
like, so a run that produces a surprise can be told apart from a run that
produces a failure.

---

## Before anything: the rig's own defects

Neither is caused by this branch. Both cost a session's time last round.

1. **`~/.config/ac/config.json` has `reference_channel: 2`**, which points at
   `capture_3` — digitally silent on this wiring. Anything run as the
   operator's own user reports `NO REFERENCE`. Correct values for the current
   loopback (playback_2/AN2 → capture_4/IN4): `reference_channel: 3`,
   `reference_output_channel: 1`. Left to Markus deliberately; it is his file.
2. **`install.sh` does not ship `ac-view`.** Copied by hand twice now. Verify
   every installed binary by **sha256**, not size and mtime — both matched on
   a stale binary last session.
3. **Clock stays `AutoSync`** (`numid=320 = 0`). The external master clocks the
   card over ADAT and ADAT carries playback_5, the stimulus leg. Any older
   instruction to set Internal is wrong.

---

## 1. Fixes 1, 2 and 5 — does the branch do what it claims

Build `rig2-fixes-125`, install, confirm by sha256. One session at **3 m on
axis** and one at **1 m on axis**, both at the drive level session 2 used
(−30 dBFS; drive-level consent still applies, −40 dBFS is the standing cap for
anything larger).

**Fix 1 — no negative lock.** Pass: `delay_locked` is never true with a
negative `delay_ms`. A −826 ms lock painted `LOCK ACQUIRED` last session; that
must now be a refusal. If the same session refuses *everywhere* where it
previously locked at 3 m, that is a **failure**, not a pass — capture
`delay_evidence` and stop.

**Fix 1, daemon half — no lock across the stimulus onset.** Start
`transfer_stream` first, then start the stimulus, which is the order that
produced the −826 ms lock. Pass: the first lock lands after the stimulus has
been running, at a plausible positive lag, with no refusal record published
for the ring that straddled the onset.

**Fix 2 — the capture can reproduce its own decision.** Pass: for every frame
where `delay_locked` is true, `delay_samples` appears in
`delay_evidence.candidates`, and so does `peak_lag`. This is the one that
unblocks offline tuning; it failed in **every** position-3 session last round.

**Fix 5 — `CHECK ROUTING` fires.** Point the two legs at genuinely unrelated
sources, as in session 2 (that run put 22 of 504 columns over the mask and the
banner stayed dark). Pass: `CHECK ROUTING` appears. Then confirm the other
side: a healthy 3 m measurement must **never** show it.

### What this branch does *not* fix — do not read these as regressions

- **`LOST LOCK` / `NO LOCK` are still unreachable.** Finding 3 of `handover.md`
  is untouched here. A session that refuses for 14 s still renders a blank
  window with no indicator. That is the known state, not a new one.
- **The prominence gate still refuses valid measurements at 1 m.** Finding 4 is
  a data question, not a constant, and nothing here moves it. Expect roughly
  the session-2 hit rate (1/12 at 1 m). Fix 1 may lower it further, since a
  negative lag that used to be accepted is now a refusal — that is the fix
  working.

---

## 2. The negative-lag floor — the experiment finding 4 waits on

`delay_evidence.negative_lag_median` is new on this branch, published every
frame, and **decides nothing**. It is the noise floor measured over lags a
causal path puts no signal into, as against `median_value`, which is taken
over all lags and so contains the reverberation it is meant to discriminate
against.

Collect it at every position that gets measured, locking or refusing. The
question it settles, offline and with no rig time:

> Does `peak_value / negative_lag_median` separate the valid 1 m locks
> (prominence 7.1–25.8 on the all-lag statistic) from the synthetic noise
> ceiling (7.73 median, p99.9 = 8.5), where `peak_value / median_value` does
> not?

This is reasoning, not measurement, and this class of inference has been wrong
repeatedly in this project. A capture set that answers "no" is as valuable as
one that answers "yes" — it closes the proposal instead of leaving it open.

**Also unexplained and possibly decisive:** 1/12 locks at 1 m against 7/7 at
3 m, when geometry predicts the opposite. The two positions were measured
hours apart and the room floor moved ~10 dB across that evening, so it may be
drift rather than distance. Measure the two positions **back to back this
time**, with a contemporaneous silent baseline at each.

---

## 3. Run C positions 2, 4 and 5 — now unblocked

1 m off axis, 3 m off axis, near a wall. `NOISE_FLOOR_PROMINENCE` has no data
from the marginal end, which is where it is decided.

These were explicitly blocked on fix 2: before it, the captures could not
reproduce their own accepted lag, so the runs would have cost position changes
and produced unusable evidence. That block is lifted by this branch — but
**only if block 1 above passes fix 2 first**. Do not run these on an unverified
build.

## 4. Run D — #208's positive control

50 ms gated burst against `cda40ef`. Dropped for time last session; the gap is
unchanged. Independent of everything above, so it is the first thing to cut if
the session runs short.

---

## Rig state left behind after session 2

Clock `AutoSync`, mic preamp `numid=301` at 36 (found at 0), 48 V on, PAD off,
no mixer route written. No emission in progress. `/usr/local/bin/{ac,ac-daemon,
ac-view}` match a build of `7f0dd5e` by sha256 — **these predate this branch**;
reinstall and re-verify before block 1. A daemon runs under
`HOME=/home/mui/rig2-home` with `drive_max_dbfs: -30.0`. Build dir
`~/target-rig2` (~1 GB), screenshots in `~/runB/`.
