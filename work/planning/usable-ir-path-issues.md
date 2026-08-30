# The IR path after #276 — execution plan

Running plan for the issues filed **after** epic
[#276](https://github.com/mkovero/ac/issues/276) ("make the non-live IR path a
usable, reportable measurement"). The epic's own sub-issues (#277–#287) are
done or in flight; this is the second wave — 27 issues, almost all discovered
by rig sessions once the path could actually be run on hardware.

**Expires** when #276's definition-of-done is met and every issue listed below
is closed. Delete it then; do not let it become a second epic body. The tracker
owns open/closed — this file owns *order and reasoning only*, and any status
column in it is a snapshot, not authority.

**Snapshot taken 2026-08-25**, against `main` at `3469bcf` (PR #397 merged).
**16 of 27 closed. 11 open, and all desk work on them is done** — what remains
is one design (#378), one rebase (#352), two merges awaiting rig evidence
(#368, #369), and a queue of measurements that need an operator on site.

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

## What changed since this plan was written

Three things re-priced whole phases. Recorded here because the phase text below
reads as history without them.

- **The metre is deprecated** (#391, merged as PR #392, 2026-08-24). Shape 3
  above was resolved by deletion, not by building the surface. #371's premise
  did not survive tracing: `constant_ms` only ever fed the cosmetic `(X m)`
  suffix and never touched `delay_ms`, samples or phase, and the residual it
  was meant to correct is speaker-model-specific geometry that cannot be
  derived from a known reference delay. So #371, #372, #367 and #357 all closed
  *not-planned*, #390 decided **remove**, and #391 took out
  `ir_arrival_distance`, `distance_cal`, `distance_plausible_max_m`,
  `format_delay_readout`'s metres branch and the whole `speed_of_sound_*`
  derivation pipeline in one wire break. `ir_flight_time_ms()`
  (`ac-scene/src/sweep_ir.rs:41`) is what replaced it. **Phase 3 is gone;
  Phase 4's DoD line has to be restated.**
- **The one-period jump did not reproduce on the new audio stack.** 24 captures
  at one fixed position, peak ranges 4 and 7 samples, no cluster structure;
  2026-08-22 saw it in 10 of 24 at period 1024, so 0 in 24 has p ≈ 4×10⁻⁶
  (`work/rig/rig-2026-08-23-onset-353-results.md`). It came from the
  pipewire-jack layer, now removed — it did not scale down with the period.
  **This does not close #359/#363.** The operator's call on both, 2026-08-24:
  latency jumps are to be expected off this rig ("power management to shitty
  drivers/hw"), so the detection has to be **global**, not sized to hardware
  that happens not to jump. See Phase 1.
- **Phase 0 cleared to two items** (2026-08-24 → 2026-08-25). #361 (PR #393),
  #375 (PR #394), #380 (PR #395), #385 (PR #396) and #389 (PR #397) all merged
  in a day. What that leaves is the shape worth noticing: **every remaining
  open issue is blocked on either a design call or hardware, not on
  implementation.** #368 and #369 both have clean, QA-passed, mergeable
  branches held only by evidence that needs the rig.

---

## Inventory

Open unless struck through. Status column is a snapshot; the tracker is
authority.

| issue | title (short) | routing | rig? |
|---|---|---|---|
| [#340](https://github.com/mkovero/ac/issues/340) | τ window ceiling | code half merged (#349); open for AC3/AC4 only → #350 | yes |
| [#346](https://github.com/mkovero/ac/issues/346) | onset estimator, not `argmax` | **PR #352 `blocked`, still 6 conflicts vs `main`** | yes (AC5) |
| [#347](https://github.com/mkovero/ac/issues/347) | τ measured more than once | code merged (#348); open on the zero-sample tolerance | yes |
| [#350](https://github.com/mkovero/ac/issues/350) | derive τ window constants | measured 2026-08-22; `EDGE_MARGIN_FRAC` still not derived | yes |
| [#351](https://github.com/mkovero/ac/issues/351) | `measure_tau` still `argmax` | needs-design, blocked on #346 | yes |
| [#353](https://github.com/mkovero/ac/issues/353) | onset floor / guard coupling | fixed by #377 on `issue-346`; closes when #352 merges | no |
| [#359](https://github.com/mkovero/ac/issues/359) | `plot_ir` inherits the period jump | re-scoped — jump absent here, guard still required | yes |
| [#363](https://github.com/mkovero/ac/issues/363) | τ one period short, "2 readings agree" | re-scoped — agreement rule cannot fail | yes |
| [#368](https://github.com/mkovero/ac/issues/368) | ±2 dB unity gate refuses good loopbacks | **PR #384 MERGEABLE, QA re-review passed** | yes (to merge) |
| [#369](https://github.com/mkovero/ac/issues/369) | xrun counter ignored by τ | **PR #388 MERGEABLE, one `assumed` threshold left** | yes (to merge) |
| [#378](https://github.com/mkovero/ac/issues/378) | onset threshold vs the DRR term | needs-design, no comments yet — **the blocker** | yes |
| ~~#357~~ | ac-view readout rows collide | closed not-planned — rows cannot fire (#391) | — |
| ~~#358~~ | `calibrate` ignores `output_channel` | closed — PR #383 | — |
| ~~#360~~ | `drive_max_dbfs` governs one command | closed — PR #381; surfacing was #380 | — |
| ~~#361~~ | `it_loopback_ir` admits a pinned peak | closed — PR #393, refuses an edge-pinned peak | — |
| ~~#367~~ | no distance on any acoustic path | closed not-planned — superseded by #391 | — |
| ~~#370~~ | spawned daemon caches config | closed — PR #386, per-request re-read + port echo | — |
| ~~#371~~ | no capture path for a distance cal | closed not-planned — premise did not hold | — |
| ~~#372~~ | distance readout unreachable | closed not-planned — no writer, none coming | — |
| ~~#373~~ | self-pair refuses the whole session | closed — PR #382 | — |
| ~~#375~~ | AC5's bar came from a lucky tape draw | closed — PR #394, bar re-derived as 3 × se | — |
| ~~#376~~ | failed deconvolution reported as a result | closed — PR #387 | — |
| ~~#380~~ | clamp warning invisible on 8 commands | closed — PR #395, CLI reads the applied level back | — |
| ~~#385~~ | spawned daemon squats 5556/5557 | closed — PR #396, daemon identity | — |
| ~~#389~~ | `clippy --all-targets` fails | closed — PR #397, build gate green again | — |
| ~~#390~~ | deprecate or keep the distance layer dormant | closed — decided *remove* | — |
| ~~#391~~ | deprecate the metre | closed — PR #392 on `main` | — |

## Dependencies

```
  TRUST THE CAPTURE  (must precede any accuracy measurement)
  #363 ─┐
  #359 ─┴─→ period-jump detection, for hardware that is not this rig
  #369 ────→ label a capture taken across a dropout   (PR #388, needs evidence)
  #368 ────→ a working loopback is not refused        (PR #384, ready)

  ACCURACY
  #378 ─→ (rig re-measure) ─→ #346 AC5 verdict ─→ PR #352 merges ─→ #353 closes
          └─ scored against #375's re-derived 3 × se bar (landed)
  #351 ─→ reconcile τ's rule with the arrival's       (after #346 lands)
  #350 ─→ τ window constants                          (measured; margin undecided)
  #347 ─→ the zero-sample agreement tolerance         (folds into #363)
  #340 ─→ nothing of its own left once #350 closes

  DONE — kept so the absence is legible
  #360 → #380     clamp made real, then made visible
  #385, #389      site-time and build-gate costs, both paid
  #361            test hygiene, independent of everything
  #371 → #372 → #357, #367     dissolved by #391 (Phase 3)
```

---

## Phase 0 — before the next site visit — **cleared to two merges**

Rig time is the scarce resource and it needs an operator physically present.
Everything here was desk work that either makes the visit *legal*, makes it
*cheaper*, or stops it producing numbers that have to be thrown away. All of it
has landed except two branches that are themselves waiting on the rig.

- [x] **#360 — `drive_max_dbfs` on every emitting command** (PR #381). The
      interlock `.agents/rig.md` requires is now satisfiable: `plot_ir` and
      `calibrate` clamp server-side, and `calibrate`'s silent `-10 dBFS`
      default no longer sits 30 dB above the standing cap. Every session before
      this one was recorded as a request-side-only deviation.
- [x] **#380 — the clamp is visible** (PR #395). `ac-cli` reads the applied
      level back from the clamp-echoing responses, so a clamped run and an
      unclamped run no longer print the same thing.
- [x] **#373 — stop refusing a session over the self-pair** (PR #382).
      `[[0,2],[2,2]]` runs in one session again.
- [x] **#358 — route on the requested `output_channel`** (PR #383).
- [x] **#370 — echo the resolved ports** (PR #386) — and the larger half landed
      too: config is re-read per request, so a spawned daemon no longer
      re-measures the first channel for a whole scan.
- [x] **#376 — refuse a deconvolution that is noise** (PR #387). Reported as an
      `IrVerdict` with its reason carried through `SweepIrFault`, not as a
      result. The one that mattered most for a drive sweep: low drive is the
      *safe* choice under the consent rules, so the protocol pushes the operator
      toward exactly the conditions that produced it.
- [x] **#361 — `it_loopback_ir`'s pinned-peak bound** (PR #393). Refuses an
      edge-pinned peak instead of admitting it — same defect class as #340, one
      layer down.
- [x] **#385 — a squatting daemon is identifiable** (PR #396). The 2026-08-23
      session had to work around 5556/5557 by hand with
      `AC_CTRL_PORT`/`AC_DATA_PORT` and a note not to touch the `bin-350`
      daemon; that workaround was site time and a mis-measurement risk (wrong
      daemon answers, right-looking numbers).
- [x] **#389 — the clippy gate is green again** (PR #397). Every "green" claimed
      between the break and this fix was qualified.
- [x] **#375 AC1–AC3 — AC5's bar re-derived** (PR #394). See Phase 2 for the
      number and what it does to the standing verdict.
- [ ] **#368 — replace the ±2 dB unity gate.** PR #384 is **MERGEABLE and
      CLEAN** (the #391 conflict is gone), `claude-approved`, QA re-review
      passed with the hot-off-unity case pinned
      (`calibrate_measures_tau_on_hot_off_unity_fake_loopback`, +3.01 dB via
      `AC_FAKE_TAU_GAIN_OVERRIDE`), and the operator picked option **B** on
      2026-08-23. τ is a timing measurement and the gate was on gain. Held only
      by its `requires-rig` label — decide whether that evidence is a merge gate
      or a follow-up, because the branch itself is finished.
- [ ] **#369 — carry the xrun delta into `TauOutcome`.** PR #388 is
      **MERGEABLE and CLEAN**, both QA passes addressed. One thing blocks it and
      it is **not code-fixable**: the `assumed` "any xrun > 0 is dirty"
      threshold needs either a rig run (timed xrun vs clean lifecycle, same
      acoustic path, comparing raw τ) or explicit human acceptance on #369. The
      queue entry with the falsifying value in both directions is already
      written (`work/rig/rig-verify-queue.md:339-366`) — but a queue entry is
      not evidence. Latent at period 1024; period 64 makes it reachable, and two
      corrupted readings have no reason to disagree.

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

So both issues stay, with their justification changed. Neither has moved since;
both are still `needs-design`-shaped work nobody has started.

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
- [ ] **Verification cannot be by reproduction on this rig.** Nothing here jumps
      any more, so the guard has to be exercised by injection — the fake backend
      already has `AC_FAKE_TAU_DELAY_SAMPLES_OVERRIDE` from #348, which is the
      shape to extend, and #384/#388 both use the same override pattern for
      their own untriggerable cases. State plainly in both issues that a green
      run on 192.168.9.25 is *not* evidence the guard works.
- [ ] **#347's open half folds in here.** The exact-zero tolerance
      (`delta_samples == 0`) is still architect-tagged `assumed`; whatever #363
      concludes about independent windows should set it. The measurement #347
      asks for — ≥10 `calibrate` runs within one client lifetime and across
      restarts, at 48 and 96 kHz — is still unrun.

## Phase 2 — the arrival's accuracy

Now the critical path, and the only phase with unstarted design work: with
Phase 3 dissolved and Phase 0 cleared, **#378 is the single item between the
tree and #276's definition-of-done.** It has no comments on it yet.

- [ ] **#378 — design.** What the onset threshold is taken *relative to*. The
      pre-impulse statistic cannot see the DRR drop that supplies 15.4 of the
      18.2-sample between-position shift; #353's median floor (merged as #377)
      covers the other 2.8. Requires the architect, on the Tier 1 path. **This
      is the item to dispatch first — everything below waits on it.**
- [ ] **#378 — implement**, with the test-against-the-rejected-rule form and a
      pinned, stated breakdown direction (#353's fix degrades to exactly today's
      answer, never earlier or non-causal — hold the replacement to the same
      bar).
- [ ] **#378 — rig re-measurement**, both taped positions, c-free increment
      against `transfer_stream`, n stated per position. **Requires site
      access.** This is the measurement that also settles #346 AC5.
- [ ] **#375 AC4 — restate the AC5 verdict.** The bar itself has landed (PR
      #394): `|Δt_est − Δt_transfer_stream| ≤ 3 × se(Δt_est)`, in units of the
      candidate estimator's own measured standard error rather than a tape
      draw converted to samples. The old 1.3-sample / 4.7 mm derivation is kept
      struck-through with the reason it was wrong, not deleted. Against the
      2026-08-23 n = 12-per-position table (`transfer_stream` 550.00 smp, sd 0
      at n = 3 fresh locks; IR peak 557.42 smp, se 0.60; onset 575.67 smp, se
      3.26) the onset's bar is **3 × 3.26 = 9.78 samples**, and the measured
      increment difference was **25.67 samples** — so AC5 still fails, by ~2.6×
      its own bar rather than ~20× a borrowed one. AC4 was deferred into #378
      when #375 closed, because #378 is expected to move the onset's numbers;
      restating it before then scores code about to change.
      **The 3σ multiplier is settled**: accepted by mkovero on #375,
      2026-08-25, and recorded at `work/rig/rig-test-plan.md`. Fixed before any
      verification run and not to be re-opened once a run's numbers are known.
      **Two riders remain undecided** and were not part of that acceptance —
      both belong to #378's verification, not to `k`: (a) `se` is estimated, so
      the increment's distribution is t at ~22 df, not normal — score with a
      t-multiplier or hold n ≥ 12 per position as a condition of the bar;
      (b) `3 × se` encodes no accuracy requirement and rewards imprecision — it
      gives the onset 9.78 smp (35 mm) and the IR peak 1.80 smp (6.5 mm) from
      one rule. If the tuning path needs a fixed physical tolerance, that is a
      second criterion and its value comes from the use, not from this rig.
- [ ] **PR #352 merges or does not**, on that verdict. Its AC1–AC4 have never
      been in question. Two things to do before it can merge at all, neither
      about accuracy:
      - **rebase onto `main`** — still `CONFLICTING`/`DIRTY`, unchanged since
        2026-08-23, and a test-merge still produces **6 conflicts across 5
        files** (`ac-cli/src/commands/plot.rs`, `ac-cli/tests/it_plot_ir.rs`,
        `ac-core/src/measurement/report.rs`, `ac-core/src/measurement/sweep.rs`,
        `ac-daemon/tests/it_protocol.rs`), mostly where #391 removed the
        distance readout under it. The longer this sits, the more of `main` it
        has to absorb.
      - **restate any distance-facing output in ms.** `ir_arrival_distance()` is
        gone; the arrival's companion number is `ir_flight_time_ms()`.
- [ ] **#353 closes with #352.** Open only because #377's base was `issue-346`
      rather than `main`. Nothing to do on it.
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
      estimator-vs-estimator increment, in the 3 × se form #375 landed.
- [ ] Confirm no `StandardsCitation` moved to `verified: true` without a
      document in `stddocs/`. IEC 60268-21 is still the one missing document,
      and only the loudspeaker/PA case needs it.

---

## What one site visit should cover

Ordered so an early result can re-prioritise the rest of the same session.
Every run at the authorised drive level with the clamp now actually enforced in
the daemon (#360) **and visible in the CLI output** (#380) — the first session
that can be recorded as a clamped run rather than a deviation.

1. **#369's xrun threshold** — timed xrun vs clean lifecycle on the same
   acoustic path, comparing raw τ. First because it is the cheapest thing here
   and it releases a finished, mergeable branch (PR #388); the falsifying value
   in both directions is already written down at
   `work/rig/rig-verify-queue.md:339-366`. If the operator would rather accept
   the "any xrun > 0 is dirty" threshold explicitly on #369, this slot
   disappears and the PR merges today.
2. **#378 verification** — both taped positions, ≥12 captures each, c-free
   increment vs `transfer_stream`, standard error per position recorded, scored
   against `3 × se` (multiplier accepted 2026-08-25, fixed). Only if #378's fix
   has landed; otherwise this slot is a characterisation run, not a verdict.
   Settle the bar's **two open riders** — t-multiplier vs n ≥ 12, and whether a
   fixed physical tolerance sits alongside `3 × se` — before the run, not after.
3. **#347 / #363 τ repeatability** — ≥10 `calibrate` runs within one client
   lifetime and across daemon restarts, at 48 and 96 kHz, `delta_samples`
   recorded per run. Settles the exact-zero tolerance, and is the only way to
   see whether the agreement rule can distinguish anything on a stack that does
   not jump.
4. **#350 confirmation** — τ near the window edge, folded in while the loopback
   is patched; decides whether `TAU_EDGE_MARGIN_FRAC` stays at its conservative
   0.10.
5. **#368's rig evidence**, if its `requires-rig` label is treated as a merge
   gate rather than a follow-up — the branch is otherwise finished.

Dropped since earlier revisions: the period-jump confirmation (done, negative);
#371's capture verification and #357's pixel check (both closed with #391); the
clamped-run record (now a property of every run, not a separate item).

Standing constraints, not restated per item: −40 dBFS ceiling with explicit
per-run consent; pairs `[[0,2],[2,2]]` so the self-pair witnesses common-mode
buffering; hand tape is ±5 cm, so prefer estimator-vs-estimator on identical
captures over anything scored against a taped number; never build on the rig.
