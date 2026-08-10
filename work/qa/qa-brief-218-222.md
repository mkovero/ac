# qa-brief-218-222 — live transfer display, engine + display switch

Review #218 and #222 as **one unit**. #222 is stacked on #218 and cannot be
exercised without it; #218 alone changes nothing on screen.

## Independence rule

Re-derive every expected value from the specification — `work/handoff/handoff-mtw-live-spectrum.md`
(revision 3), `docs/design/design-mtw-alignment.md`, `docs/design/design-mtw-ladder.md`,
`work/handoff/handoff-live-display-switch.md`. **Do not read expected values from
implementation comments, test constants, or the PR bodies.** Where a test
asserts a number, derive it independently and compare. A disagreement is a
finding either way — the test may be wrong, or the spec may be.

Spec inputs you will need (these are design decisions, not implementation
outputs): NFFT 4096 at every stage; stage rates full / 12000 / 4000 Hz;
N = 4 blocks, uniform; 50% block overlap, Hann; adjacent-block correlation
ρ = 1/6; crossovers anchored to P_REF = 48; D_max = 100 ms.

## #218 — engine

1. **No synthesised columns on the transfer display.** Every emitted
   magnitude, phase and coherence column maps to ≥ 1 source bin.
   Mutation-verify.
2. **Ladder derives from `sr`.** Check at 44.1 / 48 / 96 / 192 kHz. Confirm
   no rate-specific constants in the layout. Derive the decimation factors
   yourself; 44.1 kHz is the one that does not divide evenly.
3. **Splice continuity in magnitude *and* coherence.** The coherence half
   needs a **partially coherent** stimulus — correlated source plus
   uncorrelated noise. **You choose the stimulus.** A flat reference is fully
   coherent, so γ² = 1 everywhere and a coherence step cannot appear; a test
   using one passes vacuously.
4. **Averaging is upstream of the division.** Structural: no averaging state
   on `|H1|` or any dB quantity.
5. **N present and the coherence floor matches it.** N reports blocks
   actually averaged. The floor on uncorrelated inputs is **1/N per column at
   one bin** — measure per column, not per stage.

   **CORRECTED.** An earlier revision of this brief instructed that the floor
   is "not 1/N, because the blocks overlap." That was wrong: ρ = 1/6 is the
   Welch correction for power-spectrum *variance*, and MSC bias is a different
   functional that 50% overlap costs far less. The instruction reached the
   code and shipped a figure further from truth than the uncorrected one.
   Stage averages run below 1/N because of bin count, not overlap.
6. **Each block of audio is analysed exactly once.** Block boundaries do not
   move with the drain. The existing test delivers the same audio in ragged
   chunks and demands bit-identical columns — verify it would actually fail
   under head-relative segmentation rather than trusting that it would.
7. **Coherence delay-invariant across all stages** up to D_max = 100 ms,
   including the top stage. **You choose the delay sweep.** Mutation-verify by
   disabling the alignment offset — the top stage must collapse toward
   `((W − D)/W)²`. A test exercising only the bottom stage cannot distinguish
   alignment from phase rotation.

   *This criterion cannot verify #216.* The offset derives from
   `estimate_delay`, so alignment absorbs a ring skew and passes by accident.
   Ring skew is verified from per-ring occupancy (`AC_DRAIN_TELEMETRY`), never
   from coherence, magnitude or delay.
8. **`spl` bit-identical** before and after. This is the conformance guard.
9. **Tier 1 bit-identical** — `ac plot`, RTA, SPL. Verify by comparison
   against pre-change output, not by inspection.
10. **Uniform N at every warmup stage.** *(New — postdates the criteria list,
    so nothing else covers it.)* Stages settle at roughly 0.11 / 0.85 / 2.56 s
    at 96 kHz. At each stage, assert every emitted column reports the same N,
    and that no column spans a crossover whose lower stage is unsettled.
    Mutation-verify: emit a blend column from the settled side alone and the
    test must fail. The failure this guards against is a coherence step that
    *moves* during warmup, which reads as a wandering DUT feature rather than
    an artifact.

## #222 — display switch

1. **No interpolated column reaches the screen.** Mutation-verify against a
   frame whose bottom-stage columns are sparse.
2. **Display-truth discipline holds.** `ac-scene` computes all values, strings
   and normalised coordinates; `ac-view` performs the affine map only. No
   `log10`, no measurement formatting in the renderer.
3. **Variable column count renders correctly** at all four rates, including
   the density change at each stage boundary.
4. **The old display path is gone**, not disabled. Grep-verify. Two paths left
   wired is how the original defect survived the `ac-ui` → `ac-view` rewrite.

## Do not accept

- **Tolerances chosen to make a test pass.** Any tolerance must be justified
  against a stated source of error.
- **Vacuous assertions.** Two instances have already been found in this work
  — `FakeEngine::last_drain_occupancy` (a regression test passing against an
  unfixed daemon) and `cycling_derot_changes_the_built_transfer_scene_phase`'s
  sibling (passing on two empty panes). Both were green for reasons unrelated
  to their claim. Specifically check any `assert_eq!` over a collection that a
  source switch could have emptied.
- **Expected values read from the implementation.**

## Known and already filed — do not re-report

- ~~Frame reports nominal N while the display corrects it → #223.~~
  **Withdrawn.** The correction was wrong; it has been removed from both
  crates and #223 is closed. The frame ships blocks-held and bins-per-column
  uncombined, and no derived depth figure should be added until a model is
  fitted and validated — two have now been wrong.
- Snapshots will not match the live view once #222 lands → **#221**.
- `aggregate.rs`'s interpolation branch is untouched **by rule** — the
  peak-picker tests depend on it, and the per-channel level curves it still
  affects are out of scope by decision.
- Criterion 10 (repeats absent, on hardware) is **post-merge, owner Markus**.
  It is not a merge gate and cannot run before #222.
- `reconnect_input` clears the measurement ring only — same defect class as
  #216, named in PR #217, deliberately not folded in.

## Order

QA on both → UX on #222 (density change is visible in a value display, so QA
sign-off precedes `ux-approved`) → merge → criterion 10 on the rig.
