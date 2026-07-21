# handoff-ac-view — M3: the shell

Parent plan: `ui-plan.md` (D6, D10, D12, D13, D16; invariant I-A
geometry half). Base: `main` post-M2 (d954148). One PR. This is the
first milestone where anything is drawn — and by design the *last*
place a numeric bug can hide, so the fence around what this crate may
compute is the core of the spec.

## Goal

A new workspace crate `ac-view`: a keyboard-driven egui application
that launches a transfer session against a (possibly remote) daemon,
draws the live calibrated meas-channel spectrum from `ac-scene`
scenes, shows the SPL readout, and can freeze the session into a
snapshot, fetch it, open it, and re-derive it per-channel under a
different weighting/integration. Plain egui painting — no wgpu, no
Ember, no persistence (D13; those arrive in M4+ on top of a proven
shell).

## The one structural rule

**`ac-view` computes nothing.** Every number, string, coordinate, tick,
and label on screen comes from `ac-scene` (live path) or
`ac-core::snapshot` → `ac-scene` (snapshot path). The renderer maps
normalized scene coordinates to the viewport and draws finished
strings. Concretely enforced (AC1): no `log10`/`ln`/`powf`/dB
arithmetic anywhere in `ac-view` sources; no numeric formatting of
measurement values (`format!` on a level/frequency is a review-
rejectable offense — if a string is missing, it gets added to
`ac-scene` behind its gates).

The renderer's whole numeric job is one affine map, and it contains the
project's oldest bug class: screen y grows *downward*, scene y grows
*upward*. The geometry test exists for exactly this line of code.

## Deliverables

1. **Crate `ac-view`** (deps: `ac-scene`, `ac-core`, egui/eframe, a ZMQ
   client lib; **no wgpu**).
2. **ZMQ client**: CTRL REQ + DATA SUB against configurable endpoints
   (`host:port`, no localhost hardcode — D6 remote is first-class).
   Existing commands only; zero wire changes.
3. **Session lifecycle**: launch `transfer_stream` with per-channel
   weighting/integration params (D10: set at start, not live-toggled),
   clean stop on quit, sane behavior on daemon disconnect (show state,
   don't crash, don't spin).
4. **Live spectrum view**: polyline(s) from scene traces, axis ticks
   and labels as delivered, SPL readout string verbatim, keyboard-
   movable cursor with the whole-Hz readout.
5. **Range adjustment** (freq span / dB span) via keyboard, with
   **clamped semantics — a degenerate range (min ≥ max, zero span) must
   be unrepresentable in UI state**, plus the defensive tick tests QA
   named: additive tests in `ac-scene::ticks` for degenerate inputs
   (this is the sanctioned cross-crate edit; additive only).
6. **Snapshot flow**: trigger (`snapshot`), chunked fetch with sha256
   verification (M1 client side), open a local `.acsnap` (no daemon
   needed — D8), per-channel weighting/integration re-derivation with
   the readout updating accordingly. Snapshot traces visibly
   distinguished from live ones via their provenance (D15) — how is
   UX's call, that it happens is not.
7. **Keyboard scheme**: every function reachable by keyboard; no `[`,
   `]`, `+`, `-` bindings (D16, Finnish layout); a help overlay (single
   key) listing all bindings — the only always-available chrome.
   Otherwise: no toolbars, no menus, nothing on screen that isn't
   measurement or invoked (D16).
8. **The geometry test** (I-A's render half): a headless, shape-level
   test — via egui's test harness, asserting on painted shapes, not
   pixels — that the scene→viewport map preserves orientation: larger
   scene `y` (higher level) → smaller screen `y` (higher on screen),
   larger scene `x` → larger screen `x`. This is the anti-Y-mirror
   test at the exact line where the old bug lived. No GPU adapter
   involved, CI-runnable.

## A3 gate resolution (addressed to QA, per qa-signoff-m2)

Pixel-level truth remains without a CI harness — **not yet attempted in
this repo** (architect review, decision 2: no prior mention of
lavapipe or a CI software-rendering attempt exists anywhere in this
codebase's history; this is a proposal made on the assumption that
CI-side software rendering is unreliable for a GPU-adjacent egui app,
not a documented fact). Proposed discharge for M3: the shape-level
geometry test is the blocking CI gate; one manual real-adapter run with
a screenshot attached to the PR is the pixel-level evidence,
documented, not CI-blocking. QA judges adequacy; if rejected, the
alternative is investigating egui_kittest's software renderer, as a
follow-up, not a blocker to drawing a correct polyline.

## Acceptance criteria (falsifiable)

1. **Computes-nothing rule**: an asserting test (or CI grep) that
   `ac-view` sources contain no `log10`/`ln`/dB arithmetic and no
   `format!` on measurement values; `cargo tree -p ac-view` free of
   wgpu.
2. **Geometry test** as specified — and mutation-verified at birth
   (standing policy since M2): flip the sign in the y-map, watch it
   fail, revert.
3. **Live end-to-end** under `--fake-audio` + correlated stimulus:
   session launches, frames flow, and the on-screen SPL string equals
   `ac-scene`'s output for the same captured frame — asserted at the
   harness level, not eyeballed.
4. **Snapshot end-to-end**: trigger → fetch (sha256 asserted) → open →
   re-derive under a different weighting → readout changes by the
   already-verified cross-weighting expectation (reuse M1.5's
   hand-derived offset; no new derivation needed).
5. **Keyboard**: binding table asserted against the forbidden-key list;
   every deliverable-6/7 function reachable without a mouse (checked
   as a harness script or a review checklist in the PR — implementer
   proposes, UX gate disposes).
6. **Degenerate ranges**: UI state clamps (test), and the additive
   `ac-scene::ticks` degenerate-input tests pass.
7. **Remote**: client connects to endpoints on a separate `HOME`/tmp
   daemon instance (M1's crash-test pattern proves the isolation
   tooling exists); no filesystem sharing assumed anywhere.
8. Workspace green ×2, clippy `-D warnings`, fmt, zero edits to
   pre-existing assertions (the additive ticks tests are new
   functions, not edits).

## Out of scope (hard fence)

- Waterfall, |H|/phase/coherence views (M4+ — but view dispatch must
  not structurally assume "exactly one view forever").
- Ember, wgpu, persistence, any aesthetic beyond egui defaults + trace
  distinction (M4+; UX gate may constrain, not expand).
- Mid-session parameter toggling (D10), config-file editing UI,
  multi-window, mouse-first interactions.
- Wire/schema changes, daemon changes, `ac-scene` changes beyond the
  sanctioned additive ticks tests.
- `--shot --verify` screenshot tooling (still deferred, unchanged).

## Routing

Value-display class: **QA before ux-approved**. QA: AC1–AC4, AC6–AC7,
and the A3 adequacy ruling. UX: keyboard scheme, help overlay, trace
distinction, layout minimalism (D16 is the brief). Architect: light —
one pass confirming no wire changes and that view dispatch doesn't
paint M4 into a corner.

---

## Architect review (approved, one wording fix required, two design notes)

Grounded against `ui-plan.md` D6/D10/D12/D13/D16, `ZMQ.md`, and
`attic/ac-ui`'s (the pre-Rust-rewrite, now-detached crate's) dependency
choices — not taken on the handoff's own claims.

**1. Zero-wire-changes claim: confirmed, not just asserted.**
`ZMQ.md` already defines all six commands this crate needs:
`transfer_stream` (:1510), `stop` (:703), `snapshot` (:1719),
`snapshot_fetch` (:1759), `snapshot_list` (:1798), `snapshot_delete`
(:1819). Every deliverable (session launch, snapshot fetch/open,
per-channel re-derivation) maps onto an existing command; re-derivation
itself is client-side (D8, no new command). No objection.

**2. Required fix before this ships: the A3 gate resolution's
"known environment limitation, unchanged" framing is not accurate as
written.** Checked the whole repo for `lavapipe`: it appears exactly
once, right here (line 76 of this handoff). It is not in
`qa-signoff-m2.md`'s own A3 discussion (which raised the same gate and
never mentions lavapipe), not in `ARCHITECTURE.md`, not in any prior
handoff or QA sign-off, not in `attic/ac-ui`'s history. "Known...
unchanged" reads as citing established precedent; there is none to
cite. This is exactly the class of claim M1's "verified: false"
discipline exists for — a documentation-honesty issue, not a design
flaw, so it doesn't block developer work, but the wording must change
before merge: either someone actually runs a CI-style headless
lavapipe render once and records the actual failure (making the claim
true), or the sentence gets rewritten as "not yet attempted in this
repo — the manual-screenshot fallback is proposed on the assumption
that CI-side software rendering is unreliable for a GPU-adjacent egui
app, not because it's been tried and failed here." QA should hold the
PR to whichever version actually ships, not wave this through as
"unchanged" when it's a first mention.

**3. Design note: `attic/ac-ui` is real prior art for the dependency
choice, and this handoff should say so rather than pick versions cold.**
The detached crate pinned `egui = "0.31"`, `eframe`/`egui-winit` at the
same line, plus `wgpu = "24"`/`egui-wgpu = "0.31"` — the pairing this
handoff deliberately drops (D13: wgpu deferred to Ember). Recommend
`ac-view` pin the same `egui`/`eframe` minor (`0.31`) rather than
picking independently — it's the version this codebase's own prior GUI
work already exercised, and matching it means M4's eventual wgpu
re-introduction (Ember) isn't also an egui major-version bump. Since
`eframe` without wgpu still needs a real backend to actually paint a
window, use `eframe`'s default `glow` backend (not a new decision to
make — it's `eframe`'s non-wgpu default) for the app the human runs;
the geometry test (deliverable 8) is a separate concern and doesn't
touch this backend at all — `egui_kittest` operates on egui's paint
output directly, no adapter, which is exactly why AC2 can be CI-
blocking while pixel truth (A3) can't.

**4. Design note: view dispatch, so M4 doesn't require restructuring.**
One concrete shape, so "must not structurally assume exactly one view
forever" isn't left to the implementer to interpret: a `ViewKind` enum
(single variant today, `ViewKind::Spectrum(SpectrumViewState)`) drawn
through one dispatch function, `fn draw_view(kind: &ViewKind, ui: &mut
egui::Ui, scene: &Scene)`, called from the app shell rather than the
shell inlining spectrum-specific drawing calls directly. Session
management, keyboard routing, and snapshot flow stay view-agnostic (a
"redraw with whatever the current view is" call, not a spectrum-shaped
one). Adding `ViewKind::Transfer(...)` in M4 then touches the dispatch
match arm and nothing in the shell. Not over-engineering for a single-
variant enum today — it's the one seam the handoff's own out-of-scope
section already requires exist.

**5. Confirmed consistent, no changes needed:** D10's
parameter-static-at-launch rule matches deliverable 3 exactly; D6's
"no filesystem coupling" matches AC7's remote-isolation requirement
exactly; the computes-nothing rule (AC1) should be enforced the same
way M2's AC6 enforced its own dependency-freedom claim — a `#[test]`
in `ac-view` itself scanning its own `src/` for the forbidden tokens,
not a CI-only grep step nobody runs locally.

No objection to the crate boundary, the ZMQ client choice (`zmq =
"0.10"`, already the workspace's pinned version in `ac-daemon`/
`ac-cli`), or any of the 8 deliverables' own scoping. Ready for
developer, conditional only on decision 2's wording fix landing in the
same PR (trivial, but not optional — QA should check for it under
AC7's "zero edits to pre-existing assertions" spirit: a doc claim is
an assertion too).

---

## UX review (keyboard scheme, help overlay, trace distinction, layout)

<!-- agent: ux -->

Reviewed `ac-view/src/view.rs`, `app.rs`, `keys.rs` as implemented
(`qa-signoff-m3.md`: approve). Scope per routing: keyboard scheme, help
overlay, trace distinction, layout minimalism — not the numeric/
network correctness QA already covered.

### what this output must communicate

1. Which polyline is which — meas (the calibrated signal that matters)
   vs. ref, and whether what's on screen is live or a frozen snapshot
   (D15's whole point: a trace is data-with-provenance, and the
   provenance has to be *visible*, not just present in the struct).
2. Every keyboard function, discoverable without memorising this
   document, in the one sanctioned always-available surface (the help
   overlay).

### what to remove

Nothing structurally — no toolbar, no menu, no persistent chrome beyond
the one status line and the invoked help overlay currently exist. The
status line (`"live — 127.0.0.1:5556"` / `"disconnected — ... not
responding"` / `"no session"`) is measurement-adjacent state, not
decoration — same register as the CLI baseline's own always-shown
signal-conditions line. Keep it.

### real finding 1 — the trace color violates this doc's own palette
rule, not a style nitpick

`view.rs:87`: `Stroke::new(1.5, Color32::LIGHT_GREEN)` for the meas
trace. This document's own palette section says, in as many words:
*"Never use blue or green as primary signal colours — they recede in
dark environments and carry strong semantic baggage (status, success)
that conflicts with their use as neutral signal indicators."* Green is
exactly what's in the code. This isn't a preference call the developer
guessed wrong on — it's already-decided doctrine this PR didn't follow.
Fix: warm amber, `#d7875f` (term 173) — "the ember" — for the signal
that matters (meas). Value text (`Color32::WHITE` at `view.rs:120,134`)
should be near-white `#e4e4e4`, not pure white — pure white reads
harsher/higher-contrast than the palette calls for and competes with
the ember trace rather than receding behind it.

### real finding 2 — no live/snapshot or meas/ref trace distinction
exists at all

The handoff is explicit: *"Snapshot traces visibly distinguished from
live ones via their provenance (D15) — how is UX's call, that it
happens is not."* Checked `view.rs`'s trace-drawing loop (lines 76-89):
every trace in `scene.traces` is painted with the *same* stroke,
regardless of `trace.provenance.source` (`Source::Live` vs.
`Source::Snapshot`) or `trace.provenance.channel_role` (`meas_0` vs.
`ref`). This isn't a partial implementation with a rough edge — the
distinction doesn't exist in the code at all. Concrete proposal:

```
meas, live:      ember (#d7875f), full weight, solid
meas, snapshot:  ember (#d7875f), full weight, dashed
ref, live:       dark grey (#626262), thinner, solid
ref, snapshot:   dark grey (#626262), thinner, dashed
```

Ref recedes (D16/ember principle: the reference isn't what the user
came to look at) via *weight*, not a second competing hue — this
codebase already has one signal colour, it doesn't need two. Snapshot
vs. live distinguishes by *stroke style* (solid/dashed), not colour, so
the same "is this the calibrated signal" cue (colour) still answers
that question regardless of source, and provenance answers a different
question (is this live or frozen) without colliding with it. `Trace`
already carries everything needed (`provenance.source`,
`provenance.channel_role` starts with `"meas"`/`"ref"`) — this is a
`match`/`if` in the existing drawing loop, not a new field anywhere.

### real finding 3 — the help overlay shows Rust enum debug names, not
keys a user pressed

`keys.rs:130`: `format!("{:?}  {}", b.key, b.description)`. `egui::Key`'s
`Debug` impl prints variant names — `"Slash"`, `"ArrowLeft"`,
`"ArrowRight"` — not the character on the keycap. A user reads "Slash
Toggle help overlay" and has to translate that back to "oh, `/`" in
their head; "ArrowLeft"/"ArrowRight" happen to be readable but verbose
next to a terse instrument-panel register. Fix: a small explicit label
per key (`fn key_label(k: Key) -> &'static str`, e.g. `Slash -> "/"`,
`ArrowLeft -> "←"`), not `{:?}`. Every bound key is already known and
fixed (`BINDINGS` is a `const` array) — this is a finite match, not an
open-ended formatter.

### field justifications

- Status line: measurement-adjacent state (connection health), always
  relevant, matches the CLI baseline's "always show signal conditions."
- Trace colour (ember, meas only): the one thing on screen that should
  glow — everything else, including ref, recedes.
- Stroke weight/style (not a second hue) for ref and for snapshot-vs-
  live: two independent facts, two independent *non-colour* channels,
  so they compose instead of colliding.
- Help overlay, actual key characters: the overlay's only job is
  "what do I press" — a debug-formatted enum name partially fails that
  job.

### before / after

```
before: LIGHT_GREEN meas trace, LIGHT_GREEN ref trace (indistinguishable),
        no live/snapshot distinction, help overlay reads "Slash  Toggle help overlay"

after:  ember (#d7875f) meas trace / dark-grey thinner ref trace,
        dashed stroke for snapshot-sourced traces of either channel,
        help overlay reads "/  Toggle help overlay"

removed: green as a primary signal colour (violates this doc's own rule).
added:   ref/meas and live/snapshot visual distinction (deliverable 6,
         previously unimplemented).
changed: help-overlay key labels from enum debug names to the actual
         keycap character.
```

### open questions

None requiring a human decision — all three findings have a concrete,
already-specified resolution (this document's own palette section
supplies the colour; D15 supplies what needs distinguishing; the fix
for the key-label issue is mechanical). Layout minimalism itself
(no toolbar, no menu, single status line, invoked-only help overlay) is
already correct as built — no changes requested there.

### verdict

Not approved as-is: finding 1 is a documented-rule violation, finding 2
is a missing deliverable (not a rough edge), finding 3 is a legibility
defect in the one piece of chrome this milestone is allowed to have.
All three are small, mechanical, and scoped entirely to `view.rs`/
`keys.rs` — no numeric or network code touched, nothing QA verified
needs re-verification once these land. Keyboard scheme itself
(bindings, forbidden-key compliance, help-overlay existence,
reachability) is otherwise approved as implemented.
