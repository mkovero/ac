# Rig verification queue — what still needs 192.168.9.25

Written 2026-08-03 alongside branch `rig2-fixes-125` (findings 1, 2 and 5 of
`work/handoff/handover.md`). **Updated 2026-08-06: session 3 ran on 2026-08-04 and executed
blocks 1, 2 and 3.** Their results are in `work/rig/rig-session-3-results.md`, which
supersedes the expectations written here — where this file and that one
disagree, the session is right. What survives is one block, promoted below.

**Updated 2026-08-18: the #243 block's pass criterion is restated.** #277 ran
on the rig and measured the converter's contribution directly; the old
criterion ("the residual collapses to ~0") would have read a correct cable
change as a failure. The wiring for that block is also already in place. See
the block below and `work/rig/rig-loopback-ir-277-results.md`.

Each block states what it verifies and what a pass looks like, so a run that
produces a surprise can be told apart from a run that produces a failure.

---

## Still to run

**Run D — #208's positive control, and now block 1's control too.** 50 ms gated
burst against `cda40ef`. Dropped for time in session 2, dropped again in
session 3; the gap is unchanged. **It is no longer independent** — it is the
only planned run producing a legitimately gated ring, which is the case the
dropped onset guard must not suppress, so it carries per-frame
`median_value` / `negative_lag_median` as well. Full statement in block 4.

Two things session 3 raised that no block here covers yet:

- **The cable change, and the one measurement that verifies it — #243.** Move
  the reference out through the same external converter as the stimulus,
  analogue output looped back to a Babyface input. Both legs then traverse
  Babyface → ADAT → external converter DAC → analogue, and everything up to
  that point is common-mode.

  **The cable change is already made.** As of 2026-08-18 the rig is patched
  `playback_7` (ADAT3) → converter → analogue out → `capture_3` (IN3), which
  is the reference leg this block asks for. This entry named `playback_6`;
  the channel is incidental, the path is the point. What remains is the
  measurement, the mic, and the speaker on `playback_5`.

  > **The old pass criterion was "the 1.1931 ms residual collapses to ~0".
  > That is now known to be wrong, and running against it would read a
  > correct cable change as a failure.**
  >
  > #277 measured the two legs directly at 96 kHz — Farina IR peak and
  > `jack_iodelay`, agreeing to 0.4 samples. The converter leg costs
  > **0.125 ms** more than the analogue leg (`work/rig/rig-loopback-ir-277-results.md`,
  > landing in PR #339). Both legs return through a Babyface ADC, so the ADC
  > cancels; 0.125 ms is the converter's DAC path minus the Babyface's own.
  >
  > The residual decomposes as
  > `(converter DAC − Babyface DAC) + speaker + mic + estimator bias = 1.1931 ms`.
  > Moving the reference through the converter cancels the first term and
  > nothing else.
  >
  > **Pass: the residual falls by 0.125 ms — 12 samples at 96 kHz — from
  > 1.1931 ms to ≈ 1.068 ms.** Measure it the same way session 3 did, with
  > the pair indices updated for the new reference capture: `pairs=[[2,2],[0,2]]`
  > if the reference is `capture_3`, at zero extra cost.
  >
  > **A collapse to ~0 is now the surprising outcome, not the expected one.**
  > It would mean either the 0.125 ms electrical measurement is wrong or the
  > two legs are not traversing what we believe. Informative either way, but
  > it is no longer the result to hope for.
  >
  > **What this will not fix:** roughly 1.07 ms of phantom distance — 37 cm —
  > survives the cable change, because it was never conversion latency. The
  > metres readout will still over-read at a taped distance afterwards. That
  > residue is #343's question, and this session can answer it (below).

- **#251 — rides along with the cable change, deliberately.** 20 s captures at
  3.000 m and 1.000 m with full `delay_evidence.candidates`, to score the
  **selection** half of #246. It is the last unscored half of a change three
  sessions went into, and it was queued to ride along so it does not get lost
  behind the wiring work. Do not let the cable change consume the session
  without it.

- **The electrical constant.** `arrival(d) = 1.1931 ms + d/346 m/s`. Measure it
  in-session at zero cost with the two-pair call — the same call as the #243
  verification above, so one measurement serves both. Note the pair indices
  follow whichever capture carries the reference; session 3's `[[3,3],[0,3]]`
  was `capture_4`.

- **Attribute the constant — #343, rides along with the #243 run and costs one
  extra measurement.** With the reference through the converter, conversion is
  out of the residual and what remains is speaker + mic + estimator bias. Take
  a Farina IR peak on the same leg at the same taped distance, in the same
  session, and compare it against the `transfer_stream` delay.

  > **Say which before running:** disagreement of about a millisecond puts the
  > residue on the estimator — `transfer_stream`'s broadband cross-correlation
  > against a band-limited system — and it then travels with every distance
  > readout on every rig, not just this one. Agreement puts it on the
  > loudspeaker's own acoustic group delay, which is a property of this DUT.
  > Two taped distances, so flight separates from the constant term rather
  > than being assumed.
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

  **What the regeneration actually restores, which is a stronger argument than
  tidiness.** These five are the *only* coverage of whether a correctly
  computed trace is actually visible — `work/qa/qa-ignore-audit-2026-08-10.md`
  finding 2. The headless suite covers the invariants: `it_banner_clearance`
  asserts nothing overlaps the banner, `it_trace_distinction` asserts meas and
  ref differ in colour and that snapshot traces paint dashed against live
  solid, `it_transfer_geometry` covers tick and axis placement. None of them
  can see a trace that is painted at the right coordinates in the right colour
  and then not visible — clipped, alpha-zero, occluded, or a font fallback
  substituting a glyph.

  **While the references are stale that coverage is at zero, not
  intermittent.** All five fail on the pixel diff before they can fail on
  anything real, so a genuine rendering regression arriving today would be
  indistinguishable from the known 7 px shift. Regenerating is what puts the
  check back, not what tidies it up.

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

**The distinction that keeps these two off the cut list.** Run D still competes
for session time — it needs the emission path, drive-level consent, and an A/B
against `cda40ef` — so it is a real trade in a way the two blocks above are
not. Snapshot regeneration and the `install.sh` question compete with nothing;
cutting them for time is a category error, because there is no acoustic
measurement they are taking time away from.

**What has changed is the price of Run D's side of that trade.** It was cut
twice as the self-contained block, and it is not self-contained any more: block
1 needs a legitimately gated ring to score the onset guard, and this is the only
run that produces one. Weigh it as two answers competing for the time, not one.

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

One thing to know before a session, not a block: **#254**, the three-channel
stall. A `transfer_stream` over three or more distinct channels used to reply
`ok: true` and then publish nothing, indefinitely, under `--fake-audio`.
`pairs=[[3,3],[0,3]]`, the converter-constant measurement, is two channels and
was never affected; a second measurement position — `[[0,3],[1,3]]` — is three.

**A fix is in the tree** (`audio/fake.rs` returns one buffer per registered
port; `handlers/transfer.rs` errors instead of warming up forever when capture
returns fewer buffers than the session has channels), so three-channel sessions
are rehearsable off the rig — build first and check `gh issue view 254` for
where the issue itself stands rather than trusting this line. **Ring mode is
not the way to rehearse it**: `fake_ring` still points every ref ring at one
channel, which is #204.

- **#243's own acceptance criterion 7 — a corrected metres readout against a
  taped move.** QA on PR #356 (2026-08-20) flagged this as unverified: nothing
  in the tree runs the built `distance_cal` subtraction against a physical
  distance change. Tape a **2.000 m** reference (a new position, distinct from
  the existing 1.000 m calibration fixture and the #251/electrical-constant
  captures above), name the capture's `distance_setup_id`, lock, and read
  `format_delay_readout`'s metres figure back against the tape.

  > **Pass: the corrected metres figure agrees with 2.000 m to within 5 mm.**
  > Use the same reference leg and wiring the #243 block above already has in
  > place — no new patching, just a second taped distance under the same
  > `setup_id` discipline `distance_cal_for` enforces (a constant captured at
  > 1.000 m must not be silently applied at 2.000 m; this run is what proves
  > the corrected number, not just the refusal-on-mismatch path already
  > covered by unit tests).

  **Ran 2026-08-23 against PR #356 at `647d115`. Result: FAIL, 23 mm.** Full
  record in `work/rig/rig-243-criterion7-results.md`. Constant derived at a
  taped 3.000 m, verified at a taped 1.000 m: readout 1.023 m, |X| = 23 mm,
  outside `rig-test-plan.md`'s `> 8.5 mm → fail` band — no temperature in the
  plausible range rescues it. The back-solve agrees: `c = 2.000 m / Δt` gives
  351.0 m/s → 32.5 °C, outside both the operator's stated 25–27 °C and the
  plan's 20–30 °C sanity band, so temperature alone cannot account for it.

  > **The bar stays at 5 mm.** It is grounded — `rig-243-343-results.md`
  > records `transfer_stream` agreeing with the tape to 4.7 mm on this same
  > 2.000 m increment. An earlier revision of the results file restated the
  > criterion at 5 cm on the claim that 5 mm was an unmeasured slip; that
  > claim was wrong and is corrected in place. Moving this bar is the
  > operator's call with the 4.7 mm precedent in hand, not a QA convenience.

  **What the run did establish.** The measurements are clean: both positions
  locked to a single sample value across every frame of two independent
  sessions each (925 smp at 3.000 m, 378 smp at 1.000 m), and the zero-flight
  control pair locked at exactly 0 samples throughout. Three implementation
  gaps were filed — #371 (nothing in the tree creates a `DistanceCalEntry`),
  #372 (the calibrated readout and its plausibility warning are unreachable
  from any shipped UI), #373 (`distance_setup_id` refuses the whole session
  over a self-pair). None depend on the tolerance.

  **Open, and it reaches #346/#352.** The 4.7 mm precedent and this session's
  23 mm are the same estimator, same two taped positions, same rig, 5× apart.
  The visible difference is the stack — this ran on the 2026-08-23
  jackd-direct configuration at period 64 / 2, and the 4.7 mm predates it —
  and each position is necessarily its own session (#226: the lock is cached),
  so the increment spans two JACK client lifetimes and per-client offsets do
  not cancel in it. **#346 AC5 is defined relative to `transfer_stream`'s
  4.7 mm, so that reference needs re-baselining before PR #352 can be scored
  against it.** Settling this needs a third taped distance and a thermometer.

- **`ac-view` transfer snapshots regenerated for #356 — done 2026-08-20,
  one open finding.** Ran on 192.168.9.25 (RTX 2070) at `issue-243`
  `e0e9341` (built with `RUSTFLAGS="-C target-cpu=native"`, no `mold` on
  that box — see rig-build-rustflags note if repeating this):
  `UPDATE_SNAPSHOTS=1 cargo test -p ac-view --test it_transfer_snapshots --
  --ignored --test-threads=1`, then a plain rerun, green. Box + date +
  commit recorded in `it_transfer_snapshots.rs`'s module doc.

  Correcting the prior entry's premise: **4 of the 5 references moved, not
  1.** None of the five fixtures set `distance_cal` before this pass —
  `transfer_armed_banner.png`, `transfer_driving_banner.png`, and
  `transfer_stored_comparison_no_live.png` were already stale from #243's
  ms-only wording change alone (confirmed empirically: `git diff --stat`
  before vs. after regen). `transfer_ir_panel.png` is the only byte-stable
  one, since its readout is replaced by the IR panel. Separately,
  `snapshot_transfer_live_masked_gap`'s fixture was extended (this pass) to
  actually set `distance_cal` + an exceeded `distance_plausible_max_m`, so
  its reference now paints both new rows — closing the gap that no
  existing fixture exercised them at all.

  > **Finding, not yet resolved: the two new rows visibly collide with the
  > pane's own content.** Cropped and inspected the regenerated
  > `transfer_live_masked_gap.png` directly (960×420, top-left 400×150
  > region). Row 2 (delay readout) sits on the 0 dB gridline; row 3
  > (`delay_calibration`) has the live magnitude trace drawn across its
  > baseline; row 4 (`delay_warning`) sits on the −20 dB gridline. All
  > three rows are still legible — painted after the trace/grid so they're
  > on top, not erased — but visually busy in a way rows 0–2 alone were
  > not. This matches the *risk* QA #356 named (rows 3/4 use the same
  > pane-top overlay origin as rows 0–2, extended twice as deep) and
  > confirms it actually fires for a plausible mid-slope magnitude curve,
  > not just as a theoretical edge case. Root cause is the overlay's
  > shared-origin-with-the-plot convention (pre-existing since row 0),
  > not a bug introduced fresh by #356's two-row addition — but two more
  > rows measurably raises how often it's hit. Whether that's acceptable
  > (legible-on-top is the existing house style) or needs a layout change
  > (background chip, reserved margin, or clipping the trace under the
  > text) is a display-truth design call, not a mechanical one — flagged
  > for the architect/QA rather than guessed at here. Referenced PNG is
  > the ground truth; look at it directly rather than trusting this
  > description.

---

## Before anything: the rig's own defects

Not caused by any branch. Each cost a session's time.

0. **Do not build on 192.168.9.25.** It is the hypervisor host the development
   VM runs on. Four `cargo build --release` runs there on 2026-08-22 caused the
   host's OOM killer to take a running 24 GB guest; the audio stack survived by
   luck. Build on the VM, copy binaries over, and take the sha256 **after** the
   copy. Integration tests travel the same way (`cargo test --no-run` produces a
   self-contained binary), so the rig needs no toolchain at all. Two build traps
   found the same day: a *shared* `CARGO_TARGET_DIR` across worktrees goes
   false-fresh (three of four refs reported `Finished in 0.3s` without
   compiling, and one binary was copied out under four names with four identical
   hashes), while *separate* target dirs make binaries differ by sha256 even
   when the code is identical, because absolute paths are baked into panic
   messages. Identical hashes across refs that should differ is the alarming
   direction; differing hashes across refs that should match is benign.
4. **`ac plot` with no arguments runs a measurement.** It is not a usage query:
   it auto-spawns a daemon and sweeps 20 Hz–20 kHz out `cfg.output_channel` at
   the CLI default of **−20 dBFS**, unclamped. `ac --help` is the only form that
   prints usage without emitting. Read the parser source instead of probing the
   CLI on a live rig. (`ac-daemon` likewise does not reject unknown flags —
   `--help` starts a server.)
5. **`ac calibrate`'s `output_channel` argument does not route** — #358. It
   selects the storage key only; the tone goes to `cfg.output_channel`. Set the
   channel in the config, or pass sticky `output_port`/`input_port` names. The
   symptom is "loopback not detected this run" on a cable that is fine.
6. **`/home/mui/rig2-home/.config/ac/cal.json` carries a mislabeled entry** as
   of 2026-08-22: key `out1_in3` written while `playback_5` was driven (#358).
   No `tau_history`, so nothing reads a wrong τ from it, but it claims a pair
   that was never measured. Left in place — it is the operator's file.

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

> **Record per-frame `median_value` and `negative_lag_median` on this run, not
> frame counters.** A capture requirement, not a preference: it is the one
> thing that makes the run re-scorable if it produces a surprise. Session 3
> ran this case as `run4` and kept counters, so the capture that could have
> carried the evidence does not — a run structurally unable to observe the
> thing it was there to see. It cost the offline scoring in
> `audit/rig-session-3/negative-lag-rule.md` §5 the only question it could not
> answer.
>
> **What the two numbers are for.** `R = median_value / negative_lag_median` is
> the measured signature of an onset-straddling ring: **0.364** on the one such
> capture (`audit/rig-verify-125/gate-rules-offline.md` §2), against **0.720**
> as the lowest of 843 steady-state attempts across all of session 3. A factor
> of two, on the one condition that produced the only false-confidence lock
> either session has recorded — the all-lag prominence there was 30.93, on the
> *weakest* arrival in its set.
>
> That reaches the guard, not just the diagnosis. The guard's second defect was
> that it would suppress a legitimately gated stimulus, and `R` is a
> discriminator it never had: a gated burst puts the same noise into the
> negative lags as into the positive ones between bursts, so its `R` should
> *not* collapse the way an onset-straddling ring's does. **Run D is the
> control for exactly that** — 50 ms bursts, already queued. Capture both runs
> with per-frame floors and one session scores the guard's shape as well as the
> fault it guards against. Neither is scoreable from counters.

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

> **Executed, session 3. Scored offline 2026-08-10 — the answer is "no", and
> this block is closed.** `audit/rig-session-3/negative-lag-rule.md`
> (`negative_lag_rule.py`, 843 attempts): the contamination the proposal exists
> to remove is **3.5%**, against **±17.5%** per-attempt noise on the statistic
> that would remove it; separation against the pooled silence ceiling gets
> *worse* (1.37× → 1.04×); and the near-wall 52 cm-wrong lock is **promoted**,
> 1/8 wall sessions admitted at 24 becoming 3/8. Do not raise the reverberation
> argument again — this is its second measured refutation, after
> `audit/rig-verify-125/gate-rules-offline.md` §2.
>
> **One narrower property survives, and it has moved to block 1 rather than
> staying here as a remainder**: an onset-straddling ring collapses the all-lag
> floor while the negative-lag floor holds (`R = median_value /
> negative_lag_median` = 0.364 observed, against 0.720 lowest in 843
> steady-state attempts). That is a ring-composition diagnostic, not a floor —
> and it is the discriminator the dropped onset guard lacked. Block 1's onset
> case and Run D now both carry the capture requirement it needs: **per-frame
> floors, not counters.** Session 3's `run4` kept counters, which is why this
> could not be re-scored here.
>
> The 1 m / 3 m back-to-back comparison asked for below was also run — and it
> inverted the session-2 result: 8/8 at 1.000 m, 0/8 at 3.000 m.

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

50 ms gated burst against `cda40ef`. **It is no longer independent of the rest
of this queue, and that changes what cutting it costs.** It was cut twice for
being self-contained, and that reasoning was right at the time. Block 1 now
makes it the control for the onset guard as well as #208's positive control,
because it is the **only planned run that produces a legitimately gated ring** —
the case the guard must not suppress. Cutting it now drops two answers.

If it is cut a third time, that is a decision to close #208's verification
unproven, and it should be recorded as one rather than deferred again.

**The trap in this run, recorded before it is set up.** Feeding the digital
loopback to *both* legs makes H1 ≡ 1 with a flat magnitude, so the check
returns a clean null **that looks exactly like a pass**. The measurement leg
must be analog out→in and the reference leg the digital loopback, connected by
`jack_connect` within ~1 s of session start. Use a repeatable level step, not a
finger snap: the criterion is a *count* of episodes, and comparing counts
across two different transients proves nothing — which is also why the A/B
against `cda40ef` is not optional. One episode on the new build only means
something if the old build shows four.

**It now carries a second passenger, at no extra cost: per-frame
`median_value` and `negative_lag_median`.** A 50 ms burst is the legitimately
gated stimulus that the dropped onset guard would have suppressed, so this run
is the control for block 1's onset capture — the case where `R` must *not*
collapse. Same capture, no extra rig time, and it is the difference between
being able to score the guard's shape and arguing about it for a fourth
session. Do not record counters here.

**Those floors survive an inconclusive #208 result.** The level-step A/B can
come back unreadable — a transient that does not excite the symptom on either
build is exactly what session 3's control did, and it is a real possibility
again. That outcome says nothing about the guard: `R` on a gated ring is a
different measurement on the same capture, and it is still answered. **Do not
discard the run's per-frame floors along with an inconclusive episode count**,
and do not let the risk of an inconclusive A/B argue for cutting the run —
those are now two questions riding one setup, and only one of them can come
back empty.

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
