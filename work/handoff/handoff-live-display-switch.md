# handoff-live-display-switch — make ac-view draw the three-stage columns

**This is the step that was missing.** `work/handoff/handoff-mtw-live-spectrum.md` scoped
the daemon side and specified the wire contract as additive, so #218 lands the
engine and changes nothing on screen. This slice switches the display over. It
is the one that delivers a usable live transfer view.

Base: `main` after #218 merges. Scope: `ac-view` and `ac-scene` only. No
daemon changes.

---

## What this changes

Today `ac-view` draws the Welch-derived arrays from the transfer frame. After
#218 the frame also carries the three-stage columns. This slice stops drawing
the old arrays and draws the new ones.

That is the whole change. It is small. It is also the only reason any of the
preceding work is visible.

## What lands with it

| | before | after |
|---|---|---|
| source | single 1 s Welch, sliding re-segmentation | three stages, fixed blocks |
| resolution | 1 Hz flat | 23.44 / 2.93 / 0.977 Hz by stage |
| HF settling | ~2.5 s | ~0.11 s |
| LF settling | 2.5 s | 2.56 s |
| transients | repeats, `n_averages` maxima | one episode |
| columns | uniform density, ~86 interpolated below 69 Hz | density follows resolution, none interpolated |

---

## Deliverables

1. **`ac-scene` reads the three-stage columns.** Transfer magnitude, phase and
   coherence come from the new arrays. The Welch-derived arrays are no longer
   read for display.
2. **Variable column density.** Column count is no longer fixed — density
   drops where resolution does not support it. The axis mapping must handle a
   column list that is not uniformly spaced in log frequency, since that is
   the point of the honest-density rule.
3. **Per-column Δf and window carried into the scene** so a reading can be
   interpreted. How this surfaces is UX's call — a hover readout, a shaded
   region below the bottom stage's validity, or nothing visible in v1 — but
   the data reaches the scene either way.
4. **N reaches the scene** for the same reason: coherence cannot be judged
   without it. **Use the variance-equivalent value (3.2), not nominal 4.** The
   coherence floor on uncorrelated inputs is 0.312, and a reader working from
   4 would treat 0.28 as signal when it is floor. If #218 still reports
   nominal, this slice is where that gets corrected — flag it rather than
   silently converting.
5. **Remove the dead path.** Once nothing reads the Welch-derived display
   arrays, delete the scene-side code that consumed them. Do not leave both
   wired with a toggle; two paths drifting apart is how the previous display
   bug survived a rewrite.

## Explicitly not in this slice

- Daemon changes of any kind.
- Per-channel level curves. Out of scope by decision, not deferral.
- SPL. Stays on its existing full-rate path, undrawn.
- Snapshot parity (#221). This slice is what makes the divergence visible;
  it does not fix it.
- Bench mode.

---

## Acceptance criteria

1. **No interpolated column reaches the screen.** Mutation-verified against a
   frame whose bottom-stage columns are sparse.
2. **Display-truth discipline holds.** `ac-scene` computes all values, strings
   and normalised coordinates; `ac-view` performs the affine map only. No
   `log10`, no measurement formatting in the renderer.
3. **Variable column count renders correctly** at 44.1, 48, 96 and 192 kHz,
   including the density change at each stage boundary. A fixture per rate.
4. **The old display path is gone**, not disabled. Grep-verified.
5. Workspace green, zero edits to pre-existing assertions.

---

## Then, and only then

**Criterion 10 becomes checkable.** It is the finger-snap-equivalent check
that the repeats are gone, and it cannot run before this slice because until
now the screen shows the old path. Setup is already recorded: analog out→in
on the measurement leg, digital loopback on the reference via `jack_connect`
within ~1 s of session start, transient by gating drive on/off, A/B'd against
`main`. Use a repeatable level step, not a finger snap — the check is a count,
and comparing counts across two different transients proves nothing.

The trap is recorded too: feeding the digital loopback to *both* legs gives
H1 ≡ 1 and a flat magnitude, so the check returns a clean null that looks
exactly like a pass.

**#221 becomes real** at the moment this merges — the live view runs the three
stages while snapshots derive by the old method. Latent until now, actual
from here.

---

## Order

1. Merge #218 (QA criteria 1–8, then UX).
2. This slice.
3. Criterion 10 on the rig.
4. #221 when it matters.

## Routing

- **UX:** deliverable 3's surfacing, and the density change is visible in a
  value display, so QA sign-off precedes `ux-approved`.
- **QA:** criteria 1–4.
