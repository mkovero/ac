# Rig verification queue — what still needs 192.168.9.25

Written 2026-08-03 alongside branch `rig2-fixes-125` (findings 1, 2 and 5 of
`work/handoff/handover.md`). **Updated 2026-08-06: session 3 ran on 2026-08-04 and executed
blocks 1, 2 and 3.** Their results are in `work/rig/rig-session-3-results.md`, which
supersedes the expectations written here — where this file and that one
disagree, the session is right. What survives is one block, promoted below.

Each block states what it verifies and what a pass looks like, so a run that
produces a surprise can be told apart from a run that produces a failure.

---

## Still to run

**Run D — #208's positive control.** 50 ms gated burst against `cda40ef`.
Dropped for time in session 2, dropped again in session 3; the gap is
unchanged. Independent of everything else here. Full statement in block 4.

Two things session 3 raised that no block here covers yet:

- **The cable change, and the one measurement that verifies it — #243.** Move
  the reference out to **playback_6**, through the same external converter as
  the stimulus, analogue output looped back to a Babyface input. Both legs then
  traverse Babyface → ADAT → external converter DAC → analogue, and everything
  up to that point is common-mode.

  > **Pass: the 1.1931 ms residual collapses to ~0.** Measure it the same way
  > it was measured in session 3 — `pairs=[[3,3],[0,3]]`, zero extra cost.
  > **Fail is informative:** a residual that does not collapse means the
  > reference is not traversing what it should, and **its size says which
  > stage was missed.** Until this runs, the metres readout shows the
  > instrument's own latency as distance — 1.40 m at a taped 1.000 m.

- **#251 — rides along with the cable change, deliberately.** 20 s captures at
  3.000 m and 1.000 m with full `delay_evidence.candidates`, to score the
  **selection** half of #246. It is the last unscored half of a change three
  sessions went into, and it was queued to ride along so it does not get lost
  behind the wiring work. Do not let the cable change consume the session
  without it.

- **The electrical constant.** `arrival(d) = 1.1931 ms + d/346 m/s`. Measure it
  in-session at zero cost with `pairs=[[3,3],[0,3]]` — the same call as the
  #243 verification above, so one measurement serves both.
- **The discarded second arrival — #255.** No rig time needed to decide it;
  session 3's captures are in `audit/rig-session-3/` and the ambiguous case is
  reproducible on demand (two of the room's three speakers energised).

- **Regenerate the `ac-view` reference snapshots — do this before any other
  block, and do not read the first failure as a regression.** The five PNGs in
  `ac-rs/crates/ac-view/tests/snapshots/` were last regenerated at `de4b658`
  (#194). #245's fix (`d569907`, merged as PR #252) reserves half a text line
  at the top of every view, which shifts the layout by roughly 7 px, so all
  five references are stale. Every one of these tests is `#[ignore]`d as
  *"real-adapter only (wgpu); run on 192.168.9.25 per A3 policy"*
  (`it_transfer_snapshots.rs`), so nothing offline catches it and the next run
  here pixel-diff-fails all five at once — a real defect the tests could not
  see, not a regression in the build under test.

  ```
  UPDATE_SNAPSHOTS=1 cargo test -p ac-view --test it_transfer_snapshots \
      -- --ignored --test-threads=1
  ```

  > **Pass: five regenerated PNGs committed, and a second run without
  > `UPDATE_SNAPSHOTS` green.** No emission, no wiring change, no drive —
  > it costs a build and a minute.

  **This block needs no microphone and must not compete with acoustic work for
  session time.** It is a wgpu render check on the box: the mic can stay where
  session 3 left it (near-wall, 2.4 m from A, 28 cm off the wall), and the
  block runs while the machine warms up or after the last position change,
  when position no longer matters. Its independence is exactly why it would
  otherwise be first to be cut — schedule it *outside* the acoustic budget,
  not at the end of it.

- **`CHECK ROUTING`, post-lock — the one path that has never fired on
  hardware.** Session 3's Run 4 settled the *pre-lock* case and confirmed it
  unreachable: unrelated legs refuse, no lock means no ladder,
  `FaultInput::coherence` is empty, and `coherence_dead` returns `false` on an
  empty slice. The reachable route is the other one — a pair that **locked**,
  kept its cached delay (`handlers/transfer.rs`, `pair_delays[i].is_some()`, so
  `delay_locked` stays true and `mtw` keeps publishing), and then lost coherence
  **with both legs still above the floor**. That branch is written and covered
  in pure code, and has never been exercised on real signal. It is the only way
  the state can occur at all.

  Procedure, in order — the ordering is what makes it a test rather than a
  description:

  1. **Lock at a normal position and confirm the ladder has settled** before
     touching anything. A pair that has not locked yet reproduces the *pre-lock*
     case Run 4 already tested, and the run then tells you nothing new. Settled
     ladder is the precondition, not a nicety.
  2. **Start capturing before you block anything.** The informative frames are
     the ones where `mtw` is still present *and* coherence has collapsed. Begin
     capture once the display is already blank and the discriminator — ladder
     present versus absent — is gone with it.
  3. **Block the mic capsule by hand, drive still running**, and keep capturing
     through the transition and past it.

  > **Pass:** a capture showing `mtw` present, `coherence_dead` true, and
  > whatever the banner did alongside it. Either outcome is worth having — the
  > banner firing exercises the path for the first time; the banner staying
  > dark with `mtw` present and coherence collapsed is a defect with the
  > evidence already attached. **File on the capture, not on the recollection.**
  >
  > **Do not let the drive stop.** Drive off plus both legs at the floor is a
  > deliberate `None` (`ac-scene/src/fault.rs:652-665`) and produces a blank
  > pane that looks like the same symptom while testing nothing. That
  > combination is what the 2026-08 phone clip turned out to be.

  **This one is not free.** It needs the acoustic path and the drive, unlike
  the two blocks below — but it wants the mic **where session 3 left it**, so it
  costs no position change. A minute, inside the acoustic budget.

- **Does `install.sh` still hit `Text file busy`?** One line, zero cost
  alongside any other block. `install -m 755` over a running `ac-daemon` may
  fail on the daemon binary; it has never been established either way, and the
  script now runs under `set -e`, so a failure aborts before the sha256 lines
  print. Stop the daemon, install, and note which happened.
  > **Pass:** a stated answer — either "installs cleanly with the daemon up",
  > or "fails `Text file busy`, stop the daemon first", added to the rig
  > defects list above.

  **No microphone, no emission — same scheduling rule as the block above.** It
  is a file copy over a running daemon and happens during the install every
  session already begins with.

**The distinction that keeps these two off the cut list.** Run D has been
dropped twice for being independent of everything else, and it *is* the right
thing to cut when time runs short: it needs the emission path, drive-level
consent, and an A/B against `cda40ef`, so it genuinely competes with the
acoustic blocks for the session. Snapshot regeneration and the `install.sh`
question compete with nothing. Cutting them for time is a category error —
there is no acoustic measurement they are taking time away from.

**Rides along with block 1, does not justify a trip:** what actually produced
session 2's `LOCK ACQUIRED`. The capture bounds it but cannot identify it —
`delay_attempts` did not exist on `7f0dd5e` (it arrived with #239), and
`pair_prominence` is cached and republished every frame, so frame timestamps
never dated the attempts. On a current build `delay_attempts` dates them, and
one session settles whether the transition came from the 1 Hz `RELOCK_RETRY`
or from the first attempt that saw a live reference.

Pair it with **block 1's unresolved onset case** — start the stream before the
stimulus, deliberately. Same session shape, and `delay_attempts` now dates the
attempts in both, so one run answers two questions that were previously
separate trips.

One thing to know before a session, not a block: **#254.** A `transfer_stream`
over three or more distinct channels stalls silently under `--fake-audio` —
`ok: true`, then no frames, indefinitely. `pairs=[[3,3],[0,3]]`, the
converter-constant measurement, is two channels and is unaffected. Adding a
second measurement position — `[[0,3],[1,3]]` — is three, and cannot be
rehearsed off the rig until #254 lands.

---

## Before anything: the rig's own defects

Not caused by any branch. Each cost a session's time.

1. **`~/.config/ac/config.json` has `reference_channel: 2`**, which points at
   `capture_3` — digitally silent on this wiring. Anything run as the
   operator's own user reports `NO REFERENCE`. Correct values for the current
   loopback (playback_2/AN2 → capture_4/IN4): `reference_channel: 3`,
   `reference_output_channel: 1`. Left to Markus deliberately; it is his file.
2. ~~**`install.sh` does not ship `ac-view`.**~~ **Fixed 2026-08-06** (`fa6ee27`):
   the script installs all three binaries and prints their sha256. Read that
   output — it is now the hash check. The instruction it replaces existed
   because size *and* mtime both matched on a stale binary; neither is
   evidence of which build is installed.
3. **Clock stays `AutoSync`** (`numid=320 = 0`). The external master clocks the
   card over ADAT and ADAT carries playback_5, the stimulus leg. Any older
   instruction to set Internal is wrong.

---

## 1. Fixes 1, 2 and 5 — does the branch do what it claims

> **Executed, session 3 (2026-08-04).** Fix 1 and fix 2 pass. Fix 5 does not:
> `CHECK ROUTING` is **confirmed unreachable** (Run 4) — no lock means no
> `mtw`, so the routing check and `LOST LOCK`/`NO LOCK` cannot be reached
> together. The onset case was run and did not reproduce a wrong positive
> lock. What the session changed about the *gate* is in
> `work/rig/rig-session-3-results.md` "What this session says should happen next": 24
> is simultaneously too high for the clean 3 m case and only just high enough
> to exclude the near-wall case, so it is not a threshold problem. Read the
> rest of this block as the expectation that was tested, not as work to do.

Build `rig2-fixes-125`, install, confirm by sha256. One session at **3 m on
axis** and one at **1 m on axis**, both at the drive level session 2 used
(−30 dBFS; drive-level consent still applies, −40 dBFS is the standing cap for
anything larger).

**Fix 1 — no negative lock.** Pass: `delay_locked` is never true with a
negative `delay_ms`. A −826 ms lock painted `LOCK ACQUIRED` last session; that
must now be a refusal or a correct positive lock. If the same session refuses
*everywhere* where it previously locked at 3 m, that is a **failure**, not a
pass — capture `delay_evidence` and stop.

Note what the fix does **not** do: a non-causal peak is not itself a refusal.
The estimator now searches the causal half only and measures every threshold
against the strongest peak in it, so the −826 ms capture would return the
+4.52 ms arrival its own evidence contained, not a refusal. The negative peak
is still published, as `noncausal_peak_lag` / `noncausal_peak_value`.

**Fix 1, the onset case — UNRESOLVED, and the one thing here that needs the
rig to answer.** The −826 ms lock happened when `transfer_stream` was started
before the stimulus, so the correlation ring straddled the silence→signal
transition. A daemon-side guard that skipped the lock attempt while a ring
straddled an onset was written and then **dropped before this branch shipped**,
for two reasons: no synthetic onset ring could be built where the causal-only
search still returns a wrong answer (the guard had nothing left to prevent),
and the guard would fire indefinitely on a legitimately gated stimulus — Run D
below is a 50 ms burst, whose ring is silent for most of its length — which
would suppress locking outright on that session.

So: **start the stream before the stimulus, deliberately, and see what
happens.** Pass: the first lock lands at a plausible positive lag once the
stimulus is running. Fail: a confident wrong lock at a positive lag, which is
the one case causality cannot catch. If it fails, the guard is the fix and its
shape is in this branch's history — but it must then also handle gated
stimuli.

**Fix 2 — the capture can reproduce its own decision.** Pass: for every frame
where `delay_locked` is true, `delay_samples` appears in
`delay_evidence.candidates`, and so do `peak_lag` and `noncausal_peak_lag`,
each exactly once. This is the one that unblocks offline tuning; it failed in
**every** position-3 session last round.

**Fix 5 — `CHECK ROUTING` fires.** Point the two legs at genuinely unrelated
sources, as in session 2 (that run put 22 of 504 columns over the mask and the
banner stayed dark). Pass: `CHECK ROUTING` appears. Then confirm the other
side: a healthy 3 m measurement must **never** show it.

The gate is now "fewer than 10% of columns clear the mask", which at 48 points
per octave is **about one octave** of coherent band. Worth one deliberate
check while the rig is set up: measure something coherent over less than an
octave — a driver well outside its passband is the easy version — and see
whether it reads `CHECK ROUTING`. If it does, the discriminator to reach for
is contiguity of the coherent columns, not a smaller fraction; a fraction
cannot tell one passband from scattered accidents.

### What this branch does *not* fix — do not read these as regressions

- **`LOST LOCK` / `NO LOCK` are still unreachable.** Finding 3 of `work/handoff/handover.md`
  is untouched here. A session that refuses for 14 s still renders a blank
  window with no indicator. That is the known state, not a new one.
- **The prominence gate still refuses valid measurements at 1 m.** Finding 4 is
  a data question, not a constant, and nothing here moves it. Expect roughly
  the session-2 hit rate (1/12 at 1 m).

  **A lower hit rate than session 2 is an expected outcome of this branch, not
  a regression.** Some of what session 2 counted as a lock was a non-causal
  peak accepted at high prominence, and those are now either refused or moved
  onto the causal arrival. A refusal that replaces a −826 ms "lock" is the fix
  working. Judge the branch on whether the locks it *does* return are
  physically plausible — positive, and consistent with the geometry as the
  microphone moves — not on how many it returns. Session 2's 3 m position
  locked 7/7 and **2 of those 7 were wrong**; a count is not the measure.

---

## 2. The negative-lag floor — the experiment finding 4 waits on

> **Executed, session 3.** `negative_lag_median` was collected at every
> position, locking or refusing, and the captures are in
> `audit/rig-session-3/*.json.gz`, one record per session. The 1 m / 3 m
> back-to-back comparison asked for below was also run — and it inverted the
> session-2 result: 8/8 at 1.000 m, 0/8 at 3.000 m. The offline question is
> answerable from those files without further rig time.

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

> **Partly executed, session 3.** Position 5 (near a wall) was run as Run 6
> and is the session's most valuable negative: at 2.4 m with the capsule 28 cm
> from a wall it refused 7/8 and **accepted once at prominence 24.15, 52 cm
> wrong** — while successive estimates there agreed to 9 samples (3.2 cm), the
> tightest agreement of the session, around that wrong answer. Positions 2 and
> 4 as written (1 m and 3 m *off axis*) were not run; the session ran 3.000 m
> **on** axis instead, plus two two-source geometries. Whether the off-axis
> pair is still worth a trip is a live question, not a scheduled one.

1 m off axis, 3 m off axis, near a wall. `NOISE_FLOOR_PROMINENCE` has no data
from the marginal end, which is where it is decided.

These were explicitly blocked on fix 2: before it, the captures could not
reproduce their own accepted lag, so the runs would have cost position changes
and produced unusable evidence. That block is lifted by this branch — but
**only if block 1 above passes fix 2 first**. Do not run these on an unverified
build.

## 4. Run D — #208's positive control

> **Not run.** Dropped for time in session 2 and again in session 3. This is
> the one block here that is still work.

50 ms gated burst against `cda40ef`. Independent of everything above, so it is
the first thing to cut if the session runs short — which is how it has now
survived two sessions unrun. If it is cut a third time, that is a decision to
close #208's verification unproven, and it should be recorded as one rather
than deferred again.

**The trap in this run, recorded before it is set up.** Feeding the digital
loopback to *both* legs makes H1 ≡ 1 with a flat magnitude, so the check
returns a clean null **that looks exactly like a pass**. The measurement leg
must be analog out→in and the reference leg the digital loopback, connected by
`jack_connect` within ~1 s of session start. Use a repeatable level step, not a
finger snap: the criterion is a *count* of episodes, and comparing counts
across two different transients proves nothing — which is also why the A/B
against `cda40ef` is not optional. One episode on the new build only means
something if the old build shows four.

---

## Rig state left behind

**After session 3 (2026-08-04) — this is the current state.** No emission in
progress, all workers stopped, `ac-view` closed. Clock `AutoSync`
(`numid=320` = 0); mic preamp `numid=301` = 36, found at 36 and left there;
48 V on, PAD off; no mixer route written. `/usr/local/bin/{ac,ac-daemon,
ac-view}` are a sha256-verified build of `4659b25` — **older than `main`**, so
reinstall and re-read the hashes `install.sh` now prints before any block. A
daemon runs under `HOME=/home/mui/rig2-home` (`drive_max_dbfs: -30.0`,
`reference_channel: 3`, `reference_output_channel: 1`). Build dir
`~/target-rig3` (447 MB). **The mic is at the near-wall position** — 2.4 m
from A, 28 cm off the wall, off axis — so anything that assumes 1 m on axis
must move it first.

<details><summary>After session 2 — historical</summary>

Clock `AutoSync`, mic preamp `numid=301` at 36 (found at 0), 48 V on, PAD off,
no mixer route written. No emission in progress. `/usr/local/bin/{ac,ac-daemon,
ac-view}` match a build of `7f0dd5e` by sha256. A daemon runs under
`HOME=/home/mui/rig2-home` with `drive_max_dbfs: -30.0`. Build dir
`~/target-rig2` (~1 GB), screenshots in `~/runB/`.

</details>
