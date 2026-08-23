# The IR path after #276 — execution plan

Running plan for the issues filed **after** epic
[#276](https://github.com/mkovero/ac/issues/276) ("make the non-live IR path a
usable, reportable measurement"). The epic's own sub-issues (#277–#287) are
done or in flight; this is the second wave — 21 issues, almost all discovered
by rig sessions once the path could actually be run on hardware.

**Expires** when #276's definition-of-done is met and every issue listed below
is closed. Delete it then; do not let it become a second epic body. The tracker
owns open/closed — this file owns *order and reasoning only*, and any status
column in it is a snapshot, not authority.

## What changed since the epic was written

#276 assumed the remaining work was integration: wire two working producers to
a consumer. Every issue below is instead a **trust** defect — the path runs end
to end and produces numbers that are stable, repeatable and wrong, or correct
and unreachable. Three shapes recur:

1. **A plausible number where there should be a refusal** — a peak pinned at a
   window edge (#340, #361), a τ one period short labelled "2 readings agree"
   (#363), a deconvolution that is noise reported as an arrival (#376), a scan
   that measures the same channel ten times (#370).
2. **A guard aimed at the wrong quantity** — the onset floor referenced to a
   pre-impulse statistic that cannot see the term that dominates (#378), a
   ±2 dB gain window gating a *timing* measurement (#368), an exact-match τ rule
   refusing 0.22 m of error by reporting nothing (#367).
3. **Correct code with no operator surface** — #243's capture half (#371), its
   readout (#372), the control pair it refuses to carry (#373).

---

## Inventory

| issue | title (short) | routing | rig? |
|---|---|---|---|
| [#340](https://github.com/mkovero/ac/issues/340) | τ window ceiling | code half merged (#349); open for AC3/AC4 only → #350 | yes |
| [#346](https://github.com/mkovero/ac/issues/346) | onset estimator, not `argmax` | **PR #352 open, `blocked`** | yes (AC5) |
| [#347](https://github.com/mkovero/ac/issues/347) | τ measured more than once | code merged (#348); open on the zero-sample tolerance | yes |
| [#350](https://github.com/mkovero/ac/issues/350) | derive τ window constants | needs rig data | yes |
| [#351](https://github.com/mkovero/ac/issues/351) | `measure_tau` still `argmax` | needs-design, blocked on #346 | yes |
| [#353](https://github.com/mkovero/ac/issues/353) | onset floor / guard coupling | **fixed, PR #377 merged into `issue-346`** | no |
| [#357](https://github.com/mkovero/ac/issues/357) | ac-view readout rows collide | needs-design + needs-ux, blocked | yes (pixels) |
| [#358](https://github.com/mkovero/ac/issues/358) | `calibrate` ignores `output_channel` | ready-to-implement | no |
| [#359](https://github.com/mkovero/ac/issues/359) | `plot_ir` inherits the period jump | needs-design | yes |
| [#360](https://github.com/mkovero/ac/issues/360) | `drive_max_dbfs` governs one command | needs-design, drive-path | no |
| [#361](https://github.com/mkovero/ac/issues/361) | `it_loopback_ir` admits a pinned peak | ready-to-implement | no |
| [#363](https://github.com/mkovero/ac/issues/363) | τ one period short, "2 readings agree" | needs a design call | yes |
| [#367](https://github.com/mkovero/ac/issues/367) | no distance on any acoustic path | needs triage/design | no |
| [#368](https://github.com/mkovero/ac/issues/368) | ±2 dB unity gate refuses good loopbacks | needs triage | no |
| [#369](https://github.com/mkovero/ac/issues/369) | xrun counter ignored by τ | small, latent | no |
| [#370](https://github.com/mkovero/ac/issues/370) | spawned daemon caches config | needs triage | no |
| [#371](https://github.com/mkovero/ac/issues/371) | no capture path for a distance cal | needs-design | yes (to verify) |
| [#372](https://github.com/mkovero/ac/issues/372) | distance readout unreachable from any UI | needs-ux | no |
| [#373](https://github.com/mkovero/ac/issues/373) | self-pair refuses the whole session | small bug | no |
| [#375](https://github.com/mkovero/ac/issues/375) | AC5's bar came from a lucky tape draw | needs-design | no |
| [#376](https://github.com/mkovero/ac/issues/376) | failed deconvolution reported as a result | needs-design | no |
| [#378](https://github.com/mkovero/ac/issues/378) | onset threshold vs the DRR term | needs-design | yes |

## Dependencies

```
  SAFETY / INTERLOCK
  #360 ─────────────────────────────────→ gates every further rig session

  TRUST THE CAPTURE  (must precede any accuracy measurement)
  #363 ─┐
  #359 ─┴─→ period-jump detection on both paths
  #376 ────→ refuse a deconvolution that is noise
  #369 ────→ label a capture taken across a dropout

  RUN THE RIG AT ALL  (cheap, unblocks operator time)
  #373 ─┐   self-pair carried alongside the acoustic pair
  #358 ─┤   the requested output channel is the one driven
  #368 ─┤   a working loopback is not refused for gain structure
  #370 ─┘   the config in use is visible in the output

  ACCURACY
  #378 ─→ (rig re-measure) ─→ #346 AC5 verdict ─→ PR #352 merges
  #375 ─→ the bar that verdict is scored against   (AC1–3 desk, AC4 after #378)
  #351 ─→ reconcile τ's rule with the arrival's    (after #346 lands)
  #350 ─→ τ window constants                       (largely answered by #363)
  #347 ─→ the zero-sample agreement tolerance      (folds into #363)

  OPERATOR SURFACE
  #371 ─→ #372 ─→ #357      capture → readout → layout
  #367 ─────────────────    third InterfaceLatency state (derived/approximate)

  TEST HYGIENE
  #361 ────────────────────  independent of everything
```

---

## Phase 0 — before the next site visit

Rig time is the scarce resource and it needs an operator physically present.
Everything here is desk work that either makes the visit *legal*, makes it
*cheaper*, or stops it producing numbers that have to be thrown away.

- [ ] **#360 — `drive_max_dbfs` on every emitting command.** First, and not
      only for ordering reasons: `.agents/rig.md` makes a server-side clamp an
      **interlock**, and `plot_ir` and `calibrate` are the two commands that
      actually emit. The interlock is unsatisfiable on both today, so every
      session has to be recorded as a request-side-only deviation.
      `calibrate`'s silent `-10 dBFS` default is 30 dB above the standing cap.
- [ ] **#373 — stop refusing a session over the self-pair.** `[[0,2],[2,2]]` is
      the rig's standing shape; the self-pair is the evidence that buffering is
      common-mode. Today the operator must choose between the distance readout
      and the session's own sanity check, and #243's verification had to split
      every point into two sessions. Small fix, direct saving in site time.
- [ ] **#358 — route on the requested `output_channel`.** The override is
      parsed, used only as the storage key, and never reaches `resolve_output`.
      Cost a whole run to a loudspeaker while listening on an idle loopback.
- [ ] **#368 — replace the ±2 dB unity gate.** τ is a timing measurement; the
      gate is on gain. Three correct loopbacks refused in one session,
      including the master-section pair whose τ is the one that matters — and
      whose level is a fader position that moves between sessions. Also fix the
      message: report the captured level and the window it missed, not
      "loopback not detected", which asserts a cause the instrument cannot know.
- [ ] **#370 — echo the resolved ports** in `cal_done` and `ac calibrate`'s
      output (the cheap half; per-request config re-read is the larger fix).
      `plot ir` already prints `Output: system:playback_5`, which is why the
      acoustic runs of the same session were never in doubt.
- [ ] **#376 — refuse a deconvolution that is noise.** Below ~16 dB pre-impulse
      SNR the peak lands wherever the noise is largest and the read-out looks
      normal. Low drive is the *safe* choice under the consent rules, so the
      conditions that produce this are the ones the protocol pushes an operator
      toward. Land it before a session that will be sweeping drive.
- [ ] **#361 — `it_loopback_ir`'s pinned-peak bound.** Test-side, no design
      call, and it is the same defect class one layer down from #340. Do it
      while the reasoning is loaded.
- [ ] **#375 AC1–AC3 — re-derive AC5's bar** from measured estimator
      repeatability, state the n it assumes, and mark every rig criterion as
      tape-scored or c-free. Desk work against data already recorded; AC4 waits
      (below).

## Phase 1 — decide whether the period jump still exists

**This is the plan's one genuinely open question, and it re-prices two issues.**

#359 (5 of 12 captures per position, a 1023-sample split) and #363 (42 of 97 τ
runs exactly one period short, *every one* labelled "2 readings agree") were
both measured at period 1024 under **pipewire-jack**. That stack is gone —
`jackd` now drives ALSA directly at period 64. One period is now 64 samples
(0.67 ms, 0.23 m) rather than 1024 (10.67 ms, 3.70 m), and the 2026-08-23
session's `transfer_stream` re-locked with **zero spread** at both positions,
which is not what a live coin-flip shift looks like.

- [ ] **Confirm or refute, first, and cheaply**: ~20 `plot_ir` captures at one
      fixed position and ~20 `calibrate` runs, cluster the peak indices, look
      for a 64-sample split. Thirty minutes of a visit that is happening anyway.
- [ ] If the jump is **gone**: #363 and #359 become "the agreement rule is
      structurally unable to fail" rather than "the readings are wrong" — still
      real (`agreement_count: 2` means "the state persisted for one second"),
      but no longer a gate on the accuracy work. Re-scope both, keep #363's
      independent-window requirement.
- [ ] If it **persists**: #363 and #359 move ahead of #378. An accuracy term of
      25 samples cannot be measured through a defect that moves the same number
      by 64 at random. Design once, for the layer that composes an absolute
      arrival, so `calibrate`, `plot_ir` and `ir_arrival_distance()` inherit one
      guard rather than three (#359's own last AC).
- [ ] **#369** rides along either way — read `AudioEngine::xruns()` around each
      `measure_tau` and carry the delta into `TauOutcome`. Latent today (zero
      xruns in 130 `ac` client lifetimes) but period 64 makes it reachable, and
      two corrupted readings have no reason to disagree.

## Phase 2 — the arrival's accuracy

- [ ] **#378 — design.** What the onset threshold is taken *relative to*. The
      pre-impulse statistic cannot see the DRR drop that supplies 15.4 of the
      18.2-sample between-position shift; #353's median floor (merged as #377)
      covers the other 2.8. Requires the architect, on the Tier 1 path.
- [ ] **#378 — implement**, with the test-against-the-rejected-rule form and a
      pinned, stated breakdown direction (#353's fix degrades to exactly
      today's answer, never earlier or non-causal — hold the replacement to
      the same bar).
- [ ] **#378 — rig re-measurement**, both taped positions, c-free increment
      against `transfer_stream`, n stated per position. **Requires site
      access.** This is the measurement that also settles #346 AC5.
- [ ] **#375 AC4 — restate the AC5 verdict** against the re-derived bar and the
      new number. Only meaningful after the two above.
- [ ] **PR #352 merges or does not**, on that verdict. It stays `blocked` until
      then; its AC1–AC4 have never been in question.
- [ ] **#351 — reconcile `measure_tau`'s rule with the arrival's.** Only after
      #346 lands, because what τ must match is whatever #378 leaves behind.
      Note the trap named in the issue: switching `measure_tau` to
      `estimate_onset` does *not* guarantee cancellation — the two captures have
      different sweep bandwidths, so their bandlimited skirts must be shown to
      match, not assumed to.
- [ ] **#350 / #347 / #340 closeout.** #363's controls already showed the window
      is not the mechanism (a 30 %-clearance control reproduced the shift), so
      #350 is mostly a confirmation run to fold into the same session. #347's
      open half is the *tolerance* — `delta_samples == 0`, still architect-tagged
      `assumed`; whatever #363 concludes about independent windows should set it.
      #340 then has nothing of its own left.

## Phase 3 — the operator surface

Correct code that no operator can reach. Independent of Phase 2 — do it in
parallel if there is capacity, since none of it needs the rig until verification.

- [ ] **#371 — a capture path for a distance calibration.** #243 asked for "a
      calibration procedure, not a constant in the source"; the read half
      shipped and the capture half did not. Hand-editing `cal.json` also forces
      the operator to reproduce the daemon's own `c` (`331.3 + 0.606·T`) or the
      stored constant and the readout disagree by a term nobody can see.
- [ ] **#372 — reach the readout from a shipped UI.** `distance_setup_id`
      appears nowhere in `ac-cli` or `ac-view`, so the plausibility warning —
      the thing #243 was actually filed about — is reachable only from a raw ZMQ
      client. Needs #371 to be worth anything in practice.
- [ ] **#367 — a third `InterfaceLatency` state.** Three physically different
      paths on one interface span 60 samples (0.22 m), and to avoid that the
      tool reports nothing on every acoustic path. The error has a known sign
      (applying a τ that omits part of the output chain reads long), and
      `tau_for` already computes `nearest`/`differing_fields` purely to phrase
      the refusal. Bound it and report it instead.
- [ ] **#357 — the ac-view row collision**, last: it is a layout call on rows
      whose content #371/#372 are still deciding, and it needs pixel
      verification on the real adapter.

## Phase 4 — recheck the epic

- [ ] Walk #276's definition-of-done line by line against the tree. In
      particular: "prints an arrival time and a distance that agree with the
      tape measure" needs restating in the c-free form #375 lands — a taped
      distance is ±5 cm, which cannot score any of the quantities above.
- [ ] Confirm no `StandardsCitation` moved to `verified: true` without a
      document in `stddocs/`. IEC 60268-21 is still the one missing document,
      and only the loudspeaker/PA case needs it.

---

## What one site visit should cover

Ordered so an early result can re-prioritise the rest of the same session.
Assumes Phase 0 has landed; every run at the authorised drive level with the
clamp actually enforced in the daemon running (#360).

1. **Period-jump confirmation** (Phase 1) — 20 `plot_ir` + 20 `calibrate`, one
   fixed position, cluster the peak indices. Cheapest, and re-prices #359/#363.
2. **#378 verification** — both taped positions, ≥12 captures each, c-free
   increment vs `transfer_stream`, standard error per position recorded. Only
   if #378's fix has landed; otherwise this slot is a characterisation run, not
   a verdict.
3. **#350 confirmation** — τ near the window edge, folded in while the loopback
   is patched.
4. **#371 verification** — capture a distance constant through whatever path
   ships, at a taped position, and check the stored constant against the
   readout using the daemon's own `c`.
5. **#357 pixels** — regenerate the transfer snapshots on the real adapter.

Standing constraints, not restated per item: −40 dBFS ceiling with explicit
per-run consent; pairs `[[0,2],[2,2]]` so the self-pair witnesses common-mode
buffering; hand tape is ±5 cm, so prefer estimator-vs-estimator on identical
captures over anything scored against a taped number; never build on the rig.
