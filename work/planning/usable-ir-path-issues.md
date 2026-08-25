# The IR path after #276 — execution plan

Running plan for the issues filed **after** epic
[#276](https://github.com/mkovero/ac/issues/276) ("make the non-live IR path a
usable, reportable measurement"). The epic's own sub-issues (#277–#287) are
done or in flight; this is the second wave — 27 issues now, almost all
discovered by rig sessions once the path could actually be run on hardware.

**Expires** when #276's definition-of-done is met and every issue listed below
is closed. Delete it then; do not let it become a second epic body. The tracker
owns open/closed — this file owns *order and reasoning only*, and any status
column in it is a snapshot, not authority.

**Snapshot taken 2026-08-24**, against `main` at `fd444c5` (PR #392 merged).

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

## What changed since this plan was written (2026-08-23 → 2026-08-24)

Two things re-priced whole phases. Both are recorded here because the phase
text below now reads as history without them.

- **The metre is deprecated (#391, merged as PR #392).** Shape 3 above was
  resolved by deletion, not by building the surface. #371's premise did not
  survive tracing: `constant_ms` only ever fed the cosmetic `(X m)` suffix and
  never touched `delay_ms`, samples or phase, and the residual it was meant to
  correct is speaker-model-specific geometry that cannot be derived from a
  known reference delay. So #371, #372, #367 and #357 all closed *not-planned*,
  #390 decided **remove**, and #391 took out `ir_arrival_distance`,
  `distance_cal`, `distance_plausible_max_m`, `format_delay_readout`'s metres
  branch and the whole `speed_of_sound_*` derivation pipeline in one wire
  break. `ir_flight_time_ms()` (`ac-scene/src/sweep_ir.rs:41`) is what replaced
  it. **Phase 3 is gone; Phase 4's DoD line has to be restated.**
- **The one-period jump did not reproduce on the new audio stack.** 24 captures
  at one fixed position, peak ranges 4 and 7 samples, no cluster structure;
  2026-08-22 saw it in 10 of 24 at period 1024, so 0 in 24 has p ≈ 4×10⁻⁶
  (`work/rig/rig-2026-08-23-onset-353-results.md`). It came from the
  pipewire-jack layer, now removed — it did not scale down with the period.
  **This does not close #359/#363.** The operator's call on both, 2026-08-24:
  latency jumps are to be expected off this rig ("power management to shitty
  drivers/hw"), so the detection has to be **global**, not sized to hardware
  that happens not to jump. See Phase 1.

Nine of the original 22 issues are closed. Three new ones arrived that belong
in this plan's ordering (#380, #385, #389) plus the two that carried the metre
decision (#390, #391, both closed).

---

## Inventory

Open unless marked. Status column is a snapshot; the tracker is authority.

| issue | title (short) | routing | rig? |
|---|---|---|---|
| [#340](https://github.com/mkovero/ac/issues/340) | τ window ceiling | code half merged (#349); open for AC3/AC4 only → #350 | yes |
| [#346](https://github.com/mkovero/ac/issues/346) | onset estimator, not `argmax` | **PR #352 open, `blocked`, 6 conflicts vs `main`** | yes (AC5) |
| [#347](https://github.com/mkovero/ac/issues/347) | τ measured more than once | code merged (#348); open on the zero-sample tolerance | yes |
| [#350](https://github.com/mkovero/ac/issues/350) | derive τ window constants | measured 2026-08-22; `EDGE_MARGIN_FRAC` still not derived | yes |
| [#351](https://github.com/mkovero/ac/issues/351) | `measure_tau` still `argmax` | needs-design, blocked on #346 | yes |
| [#353](https://github.com/mkovero/ac/issues/353) | onset floor / guard coupling | fixed by #377 on `issue-346`; `in-review`, closes when #352 merges | no |
| [#359](https://github.com/mkovero/ac/issues/359) | `plot_ir` inherits the period jump | **re-scoped** — jump absent here, guard still required | yes |
| [#361](https://github.com/mkovero/ac/issues/361) | `it_loopback_ir` admits a pinned peak | ready-to-implement, untouched | no |
| [#363](https://github.com/mkovero/ac/issues/363) | τ one period short, "2 readings agree" | **re-scoped** — agreement rule cannot fail | yes |
| [#368](https://github.com/mkovero/ac/issues/368) | ±2 dB unity gate refuses good loopbacks | **PR #384 open, `claude-approved`, CONFLICTING** | no |
| [#369](https://github.com/mkovero/ac/issues/369) | xrun counter ignored by τ | **PR #388 open, `in-review`** | no |
| [#375](https://github.com/mkovero/ac/issues/375) | AC5's bar came from a lucky tape draw | needs-design; AC1–AC3 are desk work, unstarted | no |
| [#378](https://github.com/mkovero/ac/issues/378) | onset threshold vs the DRR term | needs-design, no comments yet — **the blocker** | yes |
| [#380](https://github.com/mkovero/ac/issues/380) | surface the clamp warning on 8 commands | untriaged; #360's follow-on | no |
| [#385](https://github.com/mkovero/ac/issues/385) | spawned daemon squats 5556/5557 | untriaged; costs rig time | no |
| [#389](https://github.com/mkovero/ac/issues/389) | `clippy --all-targets` fails | untriaged; the build gate is red | no |
| ~~#357~~ | ac-view readout rows collide | **closed** not-planned — rows cannot fire (#391) | — |
| ~~#358~~ | `calibrate` ignores `output_channel` | **closed** — PR #383 | — |
| ~~#360~~ | `drive_max_dbfs` governs one command | **closed** — PR #381; surfacing is #380 | — |
| ~~#367~~ | no distance on any acoustic path | **closed** not-planned — superseded by #391 | — |
| ~~#370~~ | spawned daemon caches config | **closed** — PR #386, per-request re-read + port echo | — |
| ~~#371~~ | no capture path for a distance cal | **closed** not-planned — premise did not hold | — |
| ~~#372~~ | distance readout unreachable | **closed** not-planned — no writer, none coming | — |
| ~~#373~~ | self-pair refuses the whole session | **closed** — PR #382 | — |
| ~~#376~~ | failed deconvolution reported as a result | **closed** — PR #387 | — |
| ~~#390~~ | deprecate or keep the distance layer dormant | **closed** — decided *remove* | — |
| ~~#391~~ | deprecate the metre | **closed** — PR #392 on `main` | — |

## Dependencies

```
  RUN THE RIG AT ALL  (cheap, unblocks operator time)
  #385 ────→ a squatting daemon is discoverable        ← new
  #380 ────→ the clamp that #360 added is visible      ← new
  #389 ────→ the build gate goes green again           ← new

  TRUST THE CAPTURE  (must precede any accuracy measurement)
  #363 ─┐
  #359 ─┴─→ period-jump detection, for hardware that is not this rig
  #369 ────→ label a capture taken across a dropout    (PR #388)
  #368 ────→ a working loopback is not refused         (PR #384)

  ACCURACY
  #378 ─→ (rig re-measure) ─→ #346 AC5 verdict ─→ PR #352 merges ─→ #353 closes
  #375 ─→ the bar that verdict is scored against   (AC1–3 desk, AC4 after #378)
  #351 ─→ reconcile τ's rule with the arrival's    (after #346 lands)
  #350 ─→ τ window constants                       (measured; margin undecided)
  #347 ─→ the zero-sample agreement tolerance      (folds into #363)

  TEST HYGIENE
  #361 ────────────────────  independent of everything

  OPERATOR SURFACE — dissolved by #391, kept here so the absence is legible
  #371 ─→ #372 ─→ #357      all closed not-planned
  #367 ─────────────────    closed, superseded
```

---

## Phase 0 — before the next site visit — **mostly landed**

Rig time is the scarce resource and it needs an operator physically present.
Everything here is desk work that either makes the visit *legal*, makes it
*cheaper*, or stops it producing numbers that have to be thrown away.

- [x] **#360 — `drive_max_dbfs` on every emitting command** (PR #381). The
      interlock `.agents/rig.md` requires is now satisfiable: `plot_ir` and
      `calibrate` clamp server-side, and `calibrate`'s silent `-10 dBFS`
      default no longer sits 30 dB above the standing cap. Every session before
      this one was recorded as a request-side-only deviation.
- [x] **#373 — stop refusing a session over the self-pair** (PR #382).
      `[[0,2],[2,2]]` runs in one session again.
- [x] **#358 — route on the requested `output_channel`** (PR #383).
- [x] **#370 — echo the resolved ports** (PR #386) — and the larger half
      landed too: config is re-read per request, so a spawned daemon no longer
      re-measures the first channel for a whole scan.
- [x] **#376 — refuse a deconvolution that is noise** (PR #387). Reported as an
      `IrVerdict` with its reason carried through `SweepIrFault`, not as a
      result. This is the one that mattered most for a drive sweep: low drive
      is the *safe* choice under the consent rules, so the protocol pushes the
      operator toward exactly the conditions that produced it.
- [ ] **#368 — replace the ±2 dB unity gate.** PR #384 is open and
      `claude-approved`, operator picked option **B** on 2026-08-23, and the
      branch is **CONFLICTING against `main`** — #391 moved code under it.
      Rebase, re-run, merge. τ is a timing measurement and the gate is on gain;
      the message must also report the captured level and the window it missed,
      not "loopback not detected", which asserts a cause the instrument cannot
      know.
- [ ] **#369 — carry the xrun delta into `TauOutcome`.** PR #388 open,
      architect design posted 2026-08-24, `in-review`. Latent at period 1024;
      period 64 makes it reachable, and two corrupted readings have no reason
      to disagree. Merge before the next session, not after.
- [ ] **#361 — `it_loopback_ir`'s pinned-peak bound.** Untouched, still
      `ready-to-implement`, no design call needed. Same defect class as #340 one
      layer down.
- [ ] **#375 AC1–AC3 — re-derive AC5's bar** from measured estimator
      repeatability, state the n it assumes, and mark every rig criterion as
      tape-scored or c-free. Desk work against data already recorded — the
      n = 12-per-position scatter is in
      `work/rig/rig-2026-08-23-onset-353-results.md`. AC4 waits (Phase 2).
- [ ] **#389 — the clippy gate is red.** `cargo clippy --workspace
      --all-targets -- -D warnings` fails on `field_reassign_with_default` in
      `handlers/mod.rs` tests. `ac-rs/CLAUDE.md` treats that as a build-breaking
      state, so every "green" claimed since is qualified. Cheapest item here.
- [ ] **#385 — a squatting daemon must be identifiable.** 5556/5557 are
      hardcoded with no way to see whose daemon owns them; the 2026-08-23
      session worked around it by hand with `AC_CTRL_PORT`/`AC_DATA_PORT` and a
      note not to touch the `bin-350` daemon. That workaround is a site-time
      cost and a mis-measurement risk (wrong daemon answers, right-looking
      numbers).
- [ ] **#380 — surface the clamp warning on the 8 stimulus commands.** #360
      made the clamp real; this makes it visible. Without it a clamped run and
      an unclamped run print the same thing, which is the silent-config defect
      class.

## Phase 1 — the period jump: answered, then re-scoped

**The question this plan called its one genuinely open question is settled, and
the answer did not shrink the work.**

Measured 2026-08-23 on the new stack (`jackd` → ALSA direct, period 64, ports
`system:*`): 24 `plot_ir` captures at one fixed position, peak ranges 4 and 7
samples, **no cluster structure of any kind**, versus 10 of 24 split by 1023
samples on 2026-08-22 under pipewire-jack. The jump was PipeWire's; it did not
scale down to 64 samples, it disappeared.

The operator's 2026-08-24 call on #363 and #359 overrides the "re-scope down"
branch this plan originally wrote:

> it is very much to be expected that latency can jump, not everyone has rig
> setup similar than I have here […] several factors that can cause funky
> latency spikes from power management to shitty drivers/hw.

So both issues stay, with their justification changed:

- [ ] **#363 — the agreement rule is structurally unable to fail.**
      `agreement_count: 2` means "the state persisted for one second", not "two
      independent readings agree" — overlapping windows cannot witness a shift
      that both share. This is now the primary defect in #363, independent of
      whether any given host jumps. Keep the independent-window requirement;
      drop the framing that treats the 42-of-97 rig population as the evidence,
      since that population came from a stack that no longer exists.
- [ ] **#359 — same guard, `plot_ir`'s side.** Design once, for the layer that
      composes an absolute arrival, so `calibrate` and `plot_ir` inherit one
      guard rather than two (#359's own last AC — the third consumer,
      `ir_arrival_distance()`, was deleted by #391).
- [ ] **Verification cannot be by reproduction on this rig.** Nothing here
      jumps any more, so the guard has to be exercised by injection — the fake
      backend already has `AC_FAKE_TAU_DELAY_SAMPLES_OVERRIDE` from #348, which
      is the shape to extend. State plainly in both issues that a green run on
      192.168.9.25 is *not* evidence the guard works.
- [ ] **#347's open half folds in here.** The exact-zero tolerance
      (`delta_samples == 0`) is still architect-tagged `assumed`; whatever #363
      concludes about independent windows should set it. The measurement #347
      asks for — ≥10 `calibrate` runs within one client lifetime and across
      restarts, at 48 and 96 kHz — is still unrun.

## Phase 2 — the arrival's accuracy

Unchanged in substance and now the critical path: with Phase 3 dissolved and
Phase 0 nearly clear, #378 is the only thing standing between the tree and
#276's definition-of-done.

- [ ] **#378 — design.** What the onset threshold is taken *relative to*. The
      pre-impulse statistic cannot see the DRR drop that supplies 15.4 of the
      18.2-sample between-position shift; #353's median floor (merged as #377)
      covers the other 2.8. Requires the architect, on the Tier 1 path. No
      comments on the issue yet — this is the item to dispatch first.
- [ ] **#378 — implement**, with the test-against-the-rejected-rule form and a
      pinned, stated breakdown direction (#353's fix degrades to exactly
      today's answer, never earlier or non-causal — hold the replacement to
      the same bar).
- [ ] **#378 — rig re-measurement**, both taped positions, c-free increment
      against `transfer_stream`, n stated per position. **Requires site
      access.** This is the measurement that also settles #346 AC5.
- [ ] **#375 AC4 — restate the AC5 verdict** against the re-derived bar and the
      new number. AC5 as executed on 2026-08-23 read **25.67 samples (92.8 mm)**
      against a registered bar of 1.3 samples — outside the bar, outside the
      5 cm physical floor, and outside the onset's own 3.26-sample standard
      error. Whether the bar or the estimator moves is the operator's and
      architect's call, and it is not answerable until the two items above land.
- [ ] **PR #352 merges or does not**, on that verdict. Its AC1–AC4 have never
      been in question. Two things to do before it can merge at all, neither of
      them about accuracy:
      - **rebase onto `main`** — the branch is behind and a test-merge produces
        **6 conflicts across 5 files** (`ac-cli/src/commands/plot.rs`,
        `ac-cli/tests/it_plot_ir.rs`, `ac-core/src/measurement/report.rs`,
        `ac-core/src/measurement/sweep.rs`, `ac-daemon/tests/it_protocol.rs`),
        mostly where #391 removed the distance readout under it;
      - **restate any distance-facing output in ms.** `ir_arrival_distance()`
        is gone; the arrival's companion number is `ir_flight_time_ms()`.
- [ ] **#353 closes with #352.** It is only open because #377's base was
      `issue-346` rather than `main`. Nothing to do on it.
- [ ] **#351 — reconcile `measure_tau`'s rule with the arrival's.** Only after
      #346 lands, because what τ must match is whatever #378 leaves behind.
      Note the trap named in the issue: switching `measure_tau` to
      `estimate_onset` does *not* guarantee cancellation — the two captures have
      different sweep bandwidths, so their bandlimited skirts must be shown to
      match, not assumed to.
- [ ] **#350 / #347 / #340 closeout.** #350 was measured on 2026-08-22
      (`work/rig/rig-2026-08-22-tau-window-350-results.md`, branch
      `tau-window-override` merged as #364): `TAU_EDGE_MARGIN_FRAC = 0.10` is
      **safe but still not derived**, and about 10× larger than this rig's floor
      requires. Either derive it or record it as deliberately conservative with
      the measurement behind it — a value nobody has scored should not ship
      silently. #347's open half is Phase 1's. #340 then has nothing of its own
      left.

## Phase 3 — the operator surface — **dissolved, do not rebuild**

This phase asked for a capture path (#371), a readout (#372), a derived
`InterfaceLatency` state (#367) and a layout fix (#357). All four are closed,
and #391 removed the layer they were built on. Kept here as the record of why,
because the argument is easy to re-derive wrongly:

- **The metre was never in the tuning path.** `constant_ms` fed `distance_m()`,
  called only by `format_delay_readout*` for a cosmetic `(X m)` suffix. ms,
  samples and phase never depended on it.
- **The residual it corrected is not derivable.** 46.0 samples of converter
  asymmetry plus 55.9 samples of acoustic-centre geometry at 96 kHz; the
  geometry term is speaker-model-specific and correlation already cancels pure
  interface latency before `delay_ms` is formed. Taping only ever improved the
  displayed metre.
- **#367's error budget did not survive review.** The 0.22 m τ spread it
  proposed to bound is the *smallest* of three same-signed terms, the largest
  being ~1.5 m from `argmax |h|` vs true onset. Bounding the smallest while the
  largest sits unsubtracted next to it is a fourth mechanism built to make a
  metre trustworthy.
- **The operator confirmed the archive break**: no `#[serde(skip)]`, old
  `.acsnap` files carrying `distance_cal` are not readable afterward.

If a metre is ever wanted again, it starts from #391's commit message and the
#390 decision record, not from these four issues.

## Phase 4 — recheck the epic

- [ ] Walk #276's definition-of-done line by line against the tree. **One line
      is now unsatisfiable as written**: "prints an arrival time and a distance
      that agree with the tape measure". #391 removed the distance, and a taped
      distance is ±5 cm, which cannot score any of the quantities above. Restate
      it in flight-time terms — `ir_flight_time_ms()` against a c-free
      estimator-vs-estimator increment, per the form #375 lands.
- [ ] Confirm no `StandardsCitation` moved to `verified: true` without a
      document in `stddocs/`. IEC 60268-21 is still the one missing document,
      and only the loudspeaker/PA case needs it.

---

## What one site visit should cover

Ordered so an early result can re-prioritise the rest of the same session.
Assumes Phase 0 has landed; every run at the authorised drive level with the
clamp now actually enforced in the daemon (#360 merged — the first session
that can be recorded as a clamped run rather than a deviation).

1. **#378 verification** — both taped positions, ≥12 captures each, c-free
   increment vs `transfer_stream`, standard error per position recorded. Now
   the first item, since the period-jump confirmation it used to follow is
   done. Only if #378's fix has landed; otherwise this slot is a
   characterisation run, not a verdict.
2. **#347 / #363 τ repeatability** — ≥10 `calibrate` runs within one client
   lifetime and across daemon restarts, at 48 and 96 kHz, `delta_samples`
   recorded per run. Settles the exact-zero tolerance, and is the only way to
   see whether the agreement rule can distinguish anything on a stack that does
   not jump.
3. **#350 confirmation** — τ near the window edge, folded in while the loopback
   is patched; decides whether `TAU_EDGE_MARGIN_FRAC` stays at its conservative
   0.10.
4. **A clamped-run record** — one emitting command per path with
   `drive_max_dbfs` doing the clamping, to close out the interlock deviation
   that every prior session had to declare.

Dropped from this list since the last revision: the period-jump confirmation
(done, negative), #371's capture verification and #357's pixel check (both
closed with #391).

Standing constraints, not restated per item: −40 dBFS ceiling with explicit
per-run consent; pairs `[[0,2],[2,2]]` so the self-pair witnesses common-mode
buffering; hand tape is ±5 cm, so prefer estimator-vs-estimator on identical
captures over anything scored against a taped number; never build on the rig.
