# handoff-rig-session-2 — everything that needs the operator present

One session. Rig: 192.168.9.25, Babyface Pro, 96 kHz. Wiring as before —
playback_1 → capture_3 (electrical reference), playback_5 → external converter
→ loudspeaker, measurement mic on capture_1.

---

## Do not go until — **satisfied, 2026-08-03**

All three merged, in the order the interlock required:

- **#233 merged** (#225 — reference output leg). Supersedes #231, which was
  closed unmerged.
- **#232 merged** (#227 — earliest prominent peak) — `5887621`.
- **#234 merged** (#228 — six-state indicator) — `ab3d236`.

The order mattered because they interlock: #227's refusals are *invisible*
without #228 — `h1_estimate` falls back to unaligned zero, and an unaligned
measurement collapses HF exactly like a bad lock does. Testing #227 without
#228 means staring at a dead top end unable to tell refusal from failure.

Note the issue-versus-PR numbering, since this document was written against
the issues and the merge order was carried out against the PRs: #225/#227/#228
are *issues* (now closed by merge); #233/#232/#234 are the PRs that closed
them. #228 in particular never existed as a PR.

**These branches carry no CI.** `gh pr checks` reported no checks on either
branch — the merge was gated on a local `cargo fmt --check` + `cargo clippy
--all-targets -- -D warnings` + full `cargo test`, run twice: once on #232
merged with main, and again on #234 merged with the *post-#232* main, because
the two share `handlers/transfer.rs` and `ZMQ.md` and GitHub's `MERGEABLE`
only means textually clean. Both passes were clean, 0 failed. If the rig
session finds a regression, that is the verification depth it got — not a
green pipeline.

## Before the drive is armed

- **Set the interface clock to `Internal`.** No drift was observed last time
  (eight sessions to one sample) but it costs nothing to remove.
- **Verify `install.sh` actually shipped the binaries.** Check timestamps, not
  build output. This silently failed once and the symptom was the old display
  with all its faults, which reads as "the fix didn't work."
- **Restart the daemon.** A long-running one from before the merge publishes
  frames without the new fields.

## Emission

Acoustic. −30 dBFS nominal was authorised last session and is the working
figure; anything different is a deliberate per-run call, recorded with the
result. Stop emission between runs.

---

## Run A — #231, the reference leg (2 minutes)

Start `ac transfer` alone. No `generate_pink` worker, no hand-patching.

**Pass:** `ref_peak_dbfs` comes off the −96 dBFS floor.

**Expect the top end to still be dead.** #231 fixes routing, not the lock —
`ac transfer` comes up silent and the lock is taken at warmup before you press
drive. That is #226, not a failure of this fix. Run 8 established it.

## Run B — #228, the six states (20 minutes)

Induce each fault deliberately and confirm the indicator names it. This is a
state-machine test, not a measurement.

| induce | expect |
|---|---|
| session idle, drive off | *(nothing)* |
| disconnect the reference edge, drive on | `NO REFERENCE` |
| unplug or mute the mic, drive on | `NO SIGNAL` |
| feed the two legs from different sources | `CHECK ROUTING` |
| force a bad lock (start silent, then drive) | `LOST LOCK` |
| re-lock by key | `LOCK ACQUIRED`, then normal |

**The one most likely to be wrong is the first.** Drive state is now published
from `engine_on`/`engine_level` — observed, post-dead-man, post-clamp — so
check the idle case both ways: drive genuinely off, and drive commanded on but
dead-manned. They must read differently.

**Do not look for the `conn_tags` check — it is not in this build.** The
instruction that stood here ("confirm `conn_tags` absent reads as *unknown*,
never healthy") cannot be carried out against the merged tree, and following
it would cost bench time hunting for a display element that does not exist.

`conn_tags` has **zero occurrences anywhere in `ac-rs/` on main.** It lives
only on #214 (issue #205), still open. The six-state indicator does not read
it and never did: `ac-rs/crates/ac-scene/src/fault.rs` derives every state
from exactly five inputs — `frame.drive`, `meas_peak_dbfs`, `ref_peak_dbfs`,
`delay_locked`, and `mtw` presence (as `settled`). The absent-reads-as-unknown
mapping is #214's `drive_path_state_from_tag` in `ac-scene/src/readout.rs`,
feeding a *separate* drive-path health line with its own vocabulary
(`NOT CONNECTED` in caps, `unknown` lowercase).

So Run B's six-state table is unaffected by whether #214 lands, and the table
above stands as written. Two things to know if #214 lands **before** the
session rather than after:

- A second operator-facing element appears alongside the first. Row 2
  (`disconnect the reference edge`) would then produce both `NO REFERENCE` and
  `drive path   REF OUT 3 (…)   NOT CONNECTED`. That is corroboration, not
  contradiction — but "confirm the indicator names it" becomes ambiguous about
  *which* indicator is being scored. Decide that before the run, not during.
- The unknown path still cannot be exercised here. The rig is real JACK, so
  `conn_tags` is populated and the observable values are `on`/`off`. Omission
  is the `--fake-audio` path only — see
  `ac-daemon/tests/it_protocol.rs::fake_backend_omits_conn_tags_rather_than_claiming_connected`.

## Run C — #227, the prominence threshold (the long one)

**This is the run that needs your physical presence most, and it is a data
capture, not a pass/fail.**

`MIN_PROMINENCE = 12` was derived from noise statistics, not measured. Against
pure noise it has good margin — p99.9 of 8.5 over 96 000 lags, worst of 2000
trials 9.0 — so uncorrelated legs will not slip through. The untested
direction is whether **12 refuses valid locks in a live room.** Run 3's mic was
under a metre with HF coherence 0.755; at three metres off-axis the direct
peak's prominence is much lower and nothing in the fixtures covers it.

**Capture the correlation function itself, not just the resulting lock.** Per
session, record: the full normalised correlation (or at minimum peak lag,
peak value, median |ρ|, and the lag/value of every candidate within 12 dB of
the peak), plus position, angle and input gain.

That way the threshold is tuned **offline from recorded data**, not by
returning to the rig for each candidate value. Without it, every threshold
revision costs another physical session.

Positions — five, four to six sessions each (the lock is cached per session,
so each position needs fresh sessions):

| # | position |
|---|---|
| 1 | ~1 m, on axis — the known-good baseline |
| 2 | ~1 m, off axis |
| 3 | ~3 m, on axis |
| 4 | ~3 m, off axis — worst realistic case |
| 5 | near a wall or corner — strong early reflection |

Repeat position 1 at two input gains (the 36/56 settings from last session) —
Run 7 showed lock reliability tracking electrical SNR at fixed geometry, and
that behaviour should now be gone.

**What the data has to answer:** at each position, what fraction of sessions
lock correctly, what fraction refuse, and what fraction still lock wrong. A
refusal at position 4 is acceptable; a wrong lock is not.

**Which position sets which constant.** There are two dials, not one, and
they are measured at opposite ends of this position list — so record what
each position is *for* rather than pooling all five:

| dial | set from | why |
|---|---|---|
| `DIRECT_PEAK_FRACTION` (6 dB) | positions **1–2** | needs the direct arrival's own value against the reflection's. Recoverable only where both are well above the noise, i.e. where sessions lock. |
| `NOISE_FLOOR_PROMINENCE` (12) | positions **3–5** | needs the ripple ceiling on a real path, which only the marginal and refusing positions show. |

The accept gate is `NOISE_FLOOR_PROMINENCE / DIRECT_PEAK_FRACTION` = 24, so
it follows from the two and is not measured separately. Note the fraction is
the cheaper dial: tightening it lowers the gate proportionally (0.707 gives
17, 0.8 gives 15) without moving anything closer to the noise. If the
positions that matter show direct arrivals are never a full 6 dB below the
reflection, that is the lever to reach for first.

**Second question, same data:** the 6 dB candidate window fixes
reflection-comparable-to-direct. It does not fix reflection-well-above-direct,
which is what positions 4 and 5 produce. The recorded candidate list shows
directly how far above direct those reflections sit. If any exceed 6 dB, the
fix is incomplete for that case and should say so rather than appear to cover
it.

## Run D — criterion 10's positive control (10 minutes, optional)

#208 is closed and this changes nothing, but the gap is recorded and this is
the cheap moment to close it.

The previous A/B used a 6 s level step, which is longer than the analysis
window — its edge produces a monotone ramp on both builds, so the stimulus
could not excite the symptom. **Use a ~50 ms gated burst**: repeatable *and*
impulse-like.

Run it on the current build and on `cda40ef` (pre-#218). Expect four episodes
on the old, one on the new. If the old build shows one as well, the stimulus
still is not exciting it and the gap stays open — which is a fine outcome to
record.

---

## Recording

Per run: sample rate, drive level, mic position and angle, input gain, and the
raw numbers. `AC_DRAIN_TELEMETRY=1` for anything where realtime behaviour is
in question.

Run C's correlation captures are the artifacts that matter most — they are the
only thing in this session that cannot be reconstructed later, and they set a
constant that currently rests on theory.

## Expectations, so nothing reads as a failure

- **Run A will not fix the dead top end.** That is #226.
- **Stage 0 coherence will not exceed ~0.75 at a metre.** It is
  reverberation-limited — flat to 0.006 across 20 dB of gain (Run 7). Not a
  defect, and gain cannot improve it.
- **Refusals are a success, not a failure.** #227 is meant to refuse rather
  than lock wrong. A `LOST LOCK` at position 4 is the system working.
- **There will be more refusals than the last session would suggest.** The
  accept gate is now 24, not 12 — it is derived from the other two constants
  so that reflection rejection cannot silently switch itself off at low SNR.
  That is deliberately more conservative, and positions 3–5 may refuse
  routinely. It is the correct failure direction, and it is why those
  positions are the ones that set the noise floor.
- **The direct-to-reflection ratio will read ~9% high at refusing SNR.**
  Systematic, not a measurement problem: an uncorrelated floor lifts the
  weaker peak proportionally more than the stronger one. Verified against a
  synthesised 0.625 ratio, which recovers to within 2% on a clean capture and
  to 0.682 on one that refuses. The bias **overstates** the direct arrival,
  so a fraction set from noisy captures errs strict rather than permissive —
  the safe direction, but the reason the fraction is set from positions 1–2
  and only sanity-checked against 3–5.
