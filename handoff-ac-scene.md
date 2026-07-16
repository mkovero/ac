# handoff-ac-scene — M2: the pure scene layer

Parent plan: `ui-plan.md` (D12, D13, D14, D15, D18; invariant I-A).
Base: `main` post M0+M1+M1.5, fixture frozen (`a10688c7…`). One PR.
First UI-side milestone — and still **zero rendering**: no egui, no wgpu,
no window. If it can't run headless in CI, it doesn't belong in this
crate.

## Goal

A new workspace crate `ac-scene` that turns a transfer frame or a
snapshot derivation into everything the screen will ever show — trace
geometry, axis ticks, readout strings — as plain data, fully tested
against the frozen fixture. After M2, the renderer (M3) is left with
nothing numeric to get wrong.

## The Scene contract (the deliverable that outlives this PR)

`Scene` is plain data. For the V1 spectrum view it contains:

- **Traces**: per channel, a polyline as `Vec<(x, y)>` in **normalized
  [0,1]² coordinates** with the orientation **defined in this crate**:
  `x=0` = low frequency, `y=0` = *bottom* = low level. Each trace carries
  provenance (session channel role, source = live | snapshot, capture
  params) per D15 — a trace is data-with-provenance, never "the live
  stream".
- **Axes**: tick positions (normalized) **plus tick label strings**
  (e.g. `"100"`, `"1k"`, `"10k"`; `"-60"`, `"-40"` dB) generated here,
  for a given freq range and dB range.
- **Readouts**: formatted strings, generated here —
  - SPL: value + weighting + integration tags **verbatim from the frame
    tags** (labelled-tag discipline; scene never invents or renames a
    tag);
  - cursor: given a cursor frequency, the nearest column's frequency and
    level as a formatted string, with the correct reference label
    (`dBFS` vs `dB SPL` decided by the cal tags, nothing else).

Two structural rules with teeth:

1. **`ac-scene` is the single dB-conversion site.** The wire and the
   snapshot derivation deliver linear amplitude (M0 contract); the one
   linear→dB conversion in the entire UI stack lives here. M3's renderer
   receives normalized coordinates and finished strings — it must never
   contain a `log10`.
2. **Orientation is a scene property, tested in pure code.** The old
   Ember Y-mirror bug lived in the render layer where tests couldn't
   see it. Normalizing here, with `y=0`=bottom asserted by test, moves
   that bug class into CI. M3's renderer then only maps [0,1]→viewport,
   guarded by its single geometry test.

## Deliverables

1. Crate `ac-scene` (deps: `ac-core`, serde; **no egui, no wgpu, no
   ZMQ socket code**). Frame *deserialization types* matching ZMQ.md's
   `transfer_stream` v2 schema live here, so the tested surface starts
   at the wire bytes. Socket handling stays out (M3).
2. Scene construction from both inputs: a parsed live frame, and a
   snapshot `PairDerivation`. Same underlying data must yield the same
   scene (see AC4).
3. Axis/tick generation for log-frequency and dB axes over caller-given
   ranges.
4. Cursor readout and SPL readout formatting, with formatting rules
   (significant digits, unit labels) written down in the crate docs —
   they are part of the contract, not incidental.
5. Checked-in **captured frame fixture**: one real `transfer_stream` v2
   JSON frame (from `--fake-audio` under the correlated stimulus)
   committed next to the `.acsnap` fixture, so wire-side tests need no
   daemon.

## Acceptance criteria (falsifiable)

1. **Amplitude/readout truth**: for the frozen fixture's known tone, the
   cursor readout string at f₀ is asserted **character-for-character**
   against a hand-derived expectation (the −6.75 dB class of check,
   re-derived for the display path: linear→dB conversion + formatting).
   Same for the SPL readout including its tags.
2. **Orientation invariant**: a test asserts that a higher-level column
   yields a strictly larger `y` than a lower-level one, and that a
   higher frequency yields a larger `x` — the anti-mirror test, in pure
   code.
3. **Tick truth**: for at least two (range → ticks) cases, positions
   *and label strings* asserted against hand-enumerated expectations,
   including the normalized position of a known frequency (log-mapping
   correctness, not just label text).
4. **Wire/snapshot scene equivalence**: scene built from the captured
   frame fixture equals (within float tolerance on coordinates,
   exactly on strings) the scene built from the `.acsnap` fixture's
   derivation over the same data — the two input paths cannot drift.
5. **Reference-label correctness**: cal tags present → `dB SPL`; SPL cal
   layer absent → `dBFS`; asserted both ways.
6. **No forbidden deps**: `cargo tree -p ac-scene` free of egui/wgpu/zmq
   (checked in CI or by an asserting test).
7. All of the above runs headless in CI — no daemon, no audio backend,
   no GPU adapter. Workspace green, clippy `-D warnings`, fmt, zero
   edits to pre-existing assertions.

## Out of scope (hard fence)

- Rendering of any kind, egui/wgpu, windows, keyboard input (M3).
- ZMQ transport, session lifecycle, snapshot fetch client (M3).
- Waterfall, |H|/phase/coherence scenes (M4+) — but the `Scene`/trace
  types must not structurally preclude them (multiple traces,
  provenance already generic).
- Any change to ac-core, the daemon, the wire schema, or the fixtures
  (the captured-frame fixture is a new file, not a regeneration).
- Ember, persistence, styling, colors — scenes carry no aesthetics.

## Routing

Value-display class: **QA sign-off before ux-approved** (ui-plan §3).
QA owns AC1–AC5 (every hand-derived expectation independently
re-derived — the M1.5 lesson is now standing policy). UX gate applies to
the formatting rules only (how values are shown: digits, labels, tick
style), not whether they are true. Architect: one pass on the `Scene`
type structure — it is the contract every future view builds on.

---

## Architect review (design-approved, one gap flagged)

Grounded against current `main` post-M1.5: `ac-rs/Cargo.toml` (`members
= ["crates/ac-core", "crates/ac-daemon", "crates/ac-cli"]`),
`ac-core::visualize::pair_derivation::PairDerivation`, `WeightingCurve`,
`ZMQ.md`'s `transfer_stream` v2 frame (lines ~1572-1627). `.agents/
architect.md`'s module map is stale, same caveat as every prior review
in this stack.

**0. Workspace registration.** Add `"crates/ac-scene"` to `ac-rs/
Cargo.toml`'s `members`. Trivial, noted only so the developer doesn't
have to guess the workspace root convention.

**1. Wire-frame struct is a new, minimal type — not `TransferResult`
reused.** `ac-core::visualize::transfer::TransferResult` has no
`spl`/`spl_weighting`/`spl_integration`/`cal_tags` fields and carries
`h1`-specific data (`re`/`im`/`magnitude_db` etc.) ac-scene's V1
spectrum view doesn't need (H1/phase/coherence traces are M4+, out of
scope here per the handoff's own fence). Define a dedicated
`#[derive(Deserialize)]` struct in `ac-scene::wire` scoped to exactly
the fields deliverable 1-5 use: `spec_freqs`, `meas_spectrum`,
`ref_spectrum`, `spl`, `spl_weighting`, `spl_integration`. Skip
`cal_tags` entirely (see decision 3) — deserializing a field the crate
never reads is dead weight, and `serde`'s default behavior already
ignores unknown JSON fields, so this struct doesn't need `deny_unknown_
fields` to stay narrow.

**2. Canonical intermediate type, so AC4 holds structurally.** Both
construction paths (`Scene::from_wire_frame(&WireFrame)` and
`Scene::from_pair_derivation(&PairDerivation, ...)`) must funnel
through one shared `Scene::from_input(SceneInput)`, where `SceneInput`
holds exactly the fields both paths can populate (decision 3 covers the
one field they can't both populate). Two independently-written
conversion functions that each duplicate trace-building, tick-building,
and formatting logic would make AC4 pass by coincidence today and drift
silently the first time either path is touched in isolation. One
`from_input` makes AC4 a tautology for everything except the two
`From`/mapping functions themselves, which is exactly the surface a
dedicated test should target.

**3. Real gap: `PairDerivation` has no integration-tag equivalent —
AC4 must be scoped around it, not asserted through it.** Checked
`pair_derivation.rs:28-53` directly: `PairDerivation` carries `spl:
Option<f64>` and `spl_weighting: WeightingCurve`, but nothing
corresponding to the wire's `spl_integration: "fast"|"slow"`. This
isn't an oversight in that module — its own doc comment says why:
snapshot derivation is a single-window static value, not an F/S
time-integrated one, so "integration constant" is a category error for
that path, not a missing field. Recommendation: `SceneInput.spl_
integration: Option<&'static str>` — `Some("fast"|"slow")` for
wire-built scenes, `None` for snapshot-built ones. The SPL readout
formatter must render both cases (e.g. omit the integration clause
entirely when `None`, don't fabricate one) — deliverable 4's "written
down in the crate docs" formatting-rules requirement should say this
explicitly. Consequence for AC4: the equivalence test cannot assert
byte-identical SPL readout strings between a wire scene and a
snapshot-derived scene of the same underlying window, because one
legitimately carries an integration clause and the other doesn't by
construction — this is a difference in what the two inputs *are*, not
a bug either path should hide. AC4's test should assert equivalence on
trace coordinates, axis ticks, and the SPL *value*/*weighting* portion
of the readout, and explicitly document why the integration clause is
excluded rather than silently only testing scenes where both happen to
agree.

**4. Single dB-conversion site: name it, and reuse the existing floor
constant.** Recommend `ac_scene::dbfs::linear_to_dbfs(amp: f64) -> f64`
as the one `log10` in the crate (trace y-coordinates for `meas_
spectrum`/`ref_spectrum`, and the cursor readout that samples a point
off those same linear traces). Floor it at `ac_core::shared::
reference_levels::MIN_DBFS` (`-200.0`, `reference_levels.rs:31`) rather
than inventing a new constant — that's the crate's own canonical floor
already reused by `measurement::ccir468`; note `visualize::spl_level`
currently redeclares a private local copy of the same value
(`spl_level.rs:20`) instead of importing it, which is a small existing
inconsistency, not this PR's to fix. The `spl` field itself is never
re-converted — it's already a dB SPL scalar on both the wire and
`PairDerivation`, so `linear_to_dbfs` applies only to spectrum traces
and cursor readouts, never to `spl`.

**5. Axis ranges are caller-supplied, not inferred.** Confirms
deliverable 3's "for a given freq range and dB range" phrasing —
tick/axis generation takes `(f_min, f_max)` / `(db_min, db_max)` as
explicit arguments, not derived from the data's own min/max. Matters
for AC3 (log-mapping correctness is range-dependent) and for later
milestones' zoom/pan, which this PR doesn't implement but shouldn't
have to fight the type signature to add.

**6. A3 gate — applies to M3, not this PR, and the reason is worth
stating precisely rather than the usual one-liner.** `.agents/qa.md`'s
`[PENDING A3]` clauses exist because the old GPU-dependent display-
truth harness (`ac-ui --headless-test`) was removed and never re-homed
— the blocker was literally "there is no way to test this without a
real GPU adapter." D12/D13's whole point in splitting `ac-scene` out
from `ac-view` is that this crate *is* the re-homing: orientation (AC2),
tick truth (AC3), and readout truth (AC1/AC5) are asserted in pure
code against checked-in fixtures, no adapter, no window, CI-headless
by construction (AC7). That's not "the gate doesn't apply because no
UI exists yet" (the M0/M1/M1.5 reasoning, routine) — UI-adjacent
numeric truth genuinely exists in this PR, and A3's structural blocker
is answered by this crate's own existence rather than sidestepped.
QA should evaluate AC1-AC7 directly, now, without waiting for a
separate "A3 lands" event. The gate stays live for M3: once `ac-view`
renders `Scene` to a GPU surface, *that* PR is back under `[PENDING
A3]` for real, because pixel-level truth still has no harness.

**7. `WeightingCurve::tag()` reuse.** `tag()` returns `"Z"`/`"A"`/`"C"`
(`weighting_curves.rs:54-60`) — exact match to the wire's `spl_
weighting` string and to what `SceneInput.spl_weighting` should carry
for the readout's weighting label. No re-mapping table needed in
ac-scene; deserialize the wire's string directly into `WeightingCurve`
via `WeightingCurve::from_tag` (already `pub`, already round-trip
tested per `weighting_curves.rs:182`), and pass `PairDerivation.spl_
weighting` straight through on the snapshot path.

No objection to the five deliverables' own scoping. Ready for
developer, with decision 3's `SceneInput.spl_integration: Option<&'
static str>` and its AC4-scoping consequence treated as part of the
approved design, not a follow-up.

**Addendum to decision 3 — two consequences the developer must carry
through, not just the type signature:**

**3a. The `None` case must be tested, not just handled.** A
snapshot-derived readout omitting the integration tag is a formatting
*rule*, not a fallback — if it's only implemented and never asserted,
it's untested code sitting in a "value-display, character-for-character"
milestone, which is exactly the standard this stack holds itself to.
AC1 must hand-derive **two** expected strings: one with the tag (wire
path, e.g. `"... (A, fast)"`) and one without (snapshot path — UX gate
picks the exact form, omitted clause vs. an explicit single-window
label, but either way it's a literal string in the test, not an
assumption). Whichever label the UX gate picks for the `None` case,
AC1 doesn't pass until both variants are in the fixture-backed test.

**3b. Only the tag string is excluded from AC4 — the SPL number stays
in.** Decision 3 scopes AC4 around the *integration-tag* mismatch
specifically because that field is structurally absent on one path, not
because SPL comparison generally is unreliable across paths. M1.5's
`full_ib_parity_under_correlated_stimulus` already established that a
converged time-integrated estimator and a single-window derivation
agree numerically on stationary content, within a stated tolerance —
that's the whole point of I-B. AC4 must still assert `spl` (the f64,
not the label) matches within that same tolerance class across the
wire-built and snapshot-built scenes. If the developer's AC4
implementation excludes the whole SPL readout because the tag differs,
it silently drops the one comparison this milestone can reuse M1.5's
proof for — the exclusion is `spl_integration: Option<&'static str>`
only.

---

## UX review (formatting rules only, per routing)

<!-- agent: ux -->

Reviewed `ac-scene/src/readout.rs` and `ac-scene/src/ticks.rs` as
implemented (`qa-signoff-m2.md`: approved). Scope per the handoff's own
routing note: digits, labels, tick style — not whether the numbers are
true, which QA already covered.

### what this output must communicate

1. The value at the cursor's nearest column, and whether it's a
   physically-referenced quantity (dB SPL) or a full-scale-referenced
   one (dBFS) — the unit *is* the fact here, per "relevant units,
   mandatory context."
2. The context that makes an SPL number auditable: which weighting,
   which integration (when the input has one to give).

### what to remove

Nothing in the SPL readout — `"{value:.2} dB SPL (A, fast)"` already
carries exactly what's needed and nothing else: no repeated unit, no
hedging, tags verbatim per this project's own labelled-tag discipline
(correctly *not* my call to loosen — expanding `"A"` to `"A-weighted"`
would violate the "never invent or rename a tag" rule the architect
review already locked in).

One thing to remove from the **cursor** readout: the frequency field's
false precision. `format_cursor_readout` renders
`"{freq_hz:.2} Hz: ..."` — two decimal places on a frequency that
names a *column*, not a bin. At 1 kHz with 48 cols/octave the column is
~15 Hz wide; at 15 kHz it's several hundred Hz wide (log-spaced, D18).
Printing `"994.04 Hz"` claims 0.01 Hz resolution the measurement
doesn't have and the column's own geometry contradicts — this is the
one place in the crate that currently reads like it was generated, not
designed, and it's an easy miss precisely because the *value* is
correct (QA verified `994.0351...` to 5 sig figs) while the *display*
of it is what's dishonest.

### proposed output

```
current:  994.04 Hz: -6.75 dB SPL
proposed: 994 Hz: -6.75 dB SPL
```

For a high-frequency example where the gap is more visible:

```
current:  15803.27 Hz: -41.20 dBFS
proposed: 15803 Hz: -41.20 dBFS
```

Whole Hz, no decimals — matches what a column-centre frequency can
actually claim, and matches the axis ticks' own register (`"100"`,
`"1k"`, never `"1000.00"`) so the cursor readout doesn't look like a
different instrument than the axis it's reading off of.

### field justifications

- frequency (whole Hz): identifies which column the cursor snapped to;
  precision capped at what the column geometry supports.
- level (`.2` dB): kept as-is — this project's established
  `-6.75 dB`-class precision, and unlike frequency, a level *is* a
  single scalar reading, not a band label, so two decimals isn't false
  precision.
- unit label (`dBFS`/`dB SPL`): the fact of what's being measured
  against — mandatory per AC5, already correct.
- weighting/integration tags: audit trail for the level number,
  verbatim from frame tags, correctly not reformatted.

### before / after

```
before:
994.04 Hz: -6.75 dB SPL

after:
994 Hz: -6.75 dB SPL

removed: two decimal places of frequency precision the source column
         geometry never had.
added:   nothing.
changed: frequency formatting only — level, unit, and SPL-readout
         formatting are unchanged, already correct.
```

### open questions

- Whether `format_cursor_readout` should report the column's Hz range
  (e.g. `"987-1002 Hz"`) instead of a single centre value is a real M3
  question — showing the true resolution rather than a false point
  value would fit "tolerance for the minute" even better — but it's a
  bigger interface change than this pass warrants and isn't blocking:
  a whole-Hz centre value is honest, just not maximally informative.
  Flagging for M3/M4, not this milestone.

### verdict

One concrete change requested, everything else in `readout.rs`/
`ticks.rs` approved as-is. Not blocking merge on its own terms, but
cheap enough (`format!("{:.0} Hz: ...", freq_hz.round(), ...)` plus one
test-string update) that it should land before M3 builds a cursor
tooltip against this contract — the format is the contract, and a
contract shouldn't ship a known dishonesty on day one.
