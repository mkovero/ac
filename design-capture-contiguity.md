<!-- agent: architect -->

# design decision — capture contiguity (handoff-capture-contiguity.md, D1/D2)

Scope: D1's mode-selection surface, the D1↔#192 shared-seam question, and
D2's counter placement. Markus has answered both Open Questions: **shared
synthetic-clock seam** (Q2), **partial D4 now, transfer path only** (Q1).

Nothing here changes the handoff's acceptance criteria or its hard fence.
Two scope notes are flagged explicitly at the end.

## verified against the tree first

The handoff's code claims are current, not stale:

- `jack_backend.rs:480–487` — `capture_multi` clears `ring_cons` and every
  `ring_ref_cons` *before* `wait_ring`. Identical shape at `capture_block:416`
  and `capture_stereo:446`.
- `jack_backend.rs:432` — `capture_available` does **not** clear. H1's
  single-vs-multi prediction is real and testable.
- `transfer.rs:427,496,546` — `chunk_secs = 0.05` → `capture_multi(0.05)` →
  `rings[i].extend_from_slice(buf)` → `h1_estimate_with_delay:620`.
- `fake.rs:296,321` — `capture_block`/`capture_stereo` `thread::sleep(duration)`
  then synthesise exactly `n` samples on demand. No ring, no clear, no overrun.
  D1's premise confirmed.

One correction to the handoff's framing: **the transfer worker's tick loop has
no clock of its own.** There is no `Instant`-based pacing and no sleep in the
loop body — the loop blocks in `eng.capture_multi(chunk_secs)` and that call
*is* the clock (`wait_ring` on JACK, `thread::sleep` on fake). This is what
makes a single injected time source able to serve both consumers, and it is
the load-bearing fact under the recommendation below.

---

## core question

Where do the ring-drain semantics live, so that the fake backend is
*structurally capable* of reproducing a splice — and provably exercises the
same code path JACK does, rather than a hand-written imitation of it?

This is the whole design. The mode knob is a consequence of it.

## option A — reimplement ring semantics inside `FakeEngine`

Give `FakeEngine` its own `HeapRb`, and override `capture_block` /
`capture_multi` / `capture_available` to repeat the clear → wait → `pop_slice`
sequence that `jack_backend` performs.

*tradeoffs:* Touches nothing outside `fake.rs`; zero risk to the JACK path.
But the reproducer then proves only that *the copy* splices. Any future drift
between the two implementations silently voids the evidence, and this
deliverable is evidence — acceptance criterion 4 turns on the reproducer being
load-bearing. A skeptic's objection ("your fake splices, that says nothing
about JACK") has no answer under this option.

## option B — extract the consumer-side drain into a shared type

Lift the clear → wait → `pop_slice` sequence out of `jack_backend` into a
shared `audio::rings::CaptureRings` owning `ring_cons` + `ring_ref_cons` and
exposing the four `capture_*` bodies. JACK feeds it from the process callback
(unchanged); the fake feeds it from a synthetic clock. Both backends then run
the *same* drain code.

The RT objection does not apply here, and this is the key point: the process
callback is the **producer**. `capture_block` / `wait_ring` / `capture_multi`
are `&mut self` calls on the worker thread — consumer side, not real-time.
Extracting them touches no real-time code at all.

*tradeoffs:* Costs one mechanical extraction in `jack_backend.rs` with no
semantic change (the `clear()`-before-`wait_ring` ordering is preserved
exactly — see scope note 1). Buys a reproducer that is evidence rather than
analogy, and single-sources the discarded-sample counter for free (D2 below).

## recommendation

**Option B.** The deliverable is instrumented evidence, and under option A the
instrument measures a replica of the thing under test. Option B is available
cheaply and safely only because the drain is consumer-side — that is the fact
that decides it.

---

## the synthetic clock (Q2: shared seam — approved)

`FakeEngine` gains a virtual sample cursor. `wait_ring`'s fake implementation
does not block: it advances the cursor by the requested duration, synthesises
exactly that many samples into the ring from the existing phase-continuous
generator, and returns. Deterministic, and it drops `thread::sleep` from the
tick loop, so a 50-tick session runs in milliseconds instead of 2.5 s.

**The subtle part, which the implementer must not miss.** A virtual clock that
advances *only* inside the wait reproduces nothing: `clear()` would find an
empty ring and discard zero samples. The splice exists because the ring
accrues samples during the consumer's **processing** time, and those are what
the next tick's `clear()` throws away. So the fake must charge a per-tick
processing interval as well.

Make that explicit rather than incidental: a `process_secs` knob. It is not a
simulated delay — it is the **independent variable of the experiment**. Gap
length sets replica spacing, so D3 can sweep `process_secs` and check the
measured spacing against `sr/L` directly. Acceptance criterion 5 asks for that
number reconciled; this is what makes it measurable rather than argued.

**Honest boundary on the shared seam.** It gives #192 a deterministic,
core-count-independent tick driver — that is #192's acceptance criterion 4
("a regression test that reproduces the high-parallelism condition rather than
relying on core count"), and it is the expensive half of that issue. It does
**not** make rayon's completion ordering deterministic, so it does not by
itself root-cause #192. Claim the harness, not the fix.

## mode-selection surface (D1)

Follows the established fake-only knob pattern exactly — request param on the
command, guarded by the `fake` flag, no config key. Model:
`fake_correlated_pair` at `transfer.rs:170` (parse) and `:461` (guard).

```json
{"cmd": "transfer_stream", "fake_ring": {"process_secs": 0.01}}
```

Key present ⇒ ring-backed mode. Key absent ⇒ current on-demand generator,
byte-identical. Ignored unless `state.fake_audio`. No wire-protocol change:
this is a request param on an existing command, in the same class as the two
knobs already there.

## D2 — counter placement

**Recommendation: internal counter, as the handoff defaults.** Add
`discarded_samples(&self) -> u64` to the `AudioEngine` trait with a `0`
default. Under option B it lives in `CaptureRings` and is incremented at the
single `clear()` site, so both backends get it from one place.

Two cautions:

- The handoff says "mirroring the existing `xruns()` shape". Mirror the
  *signature*, not the implementation — `xruns()` returns a hardcoded 0 on
  both real backends (issue #24). A discarded-sample counter that does the
  same is worse than none, because it reads as evidence of no splice.
- `reconnect_input:520` also clears the ring. That clear is a routing switch,
  not a per-tick discard, and must be counted separately or not at all —
  folding it into the same counter contaminates the measurement on any session
  that switches inputs.

## D2 — the wire-tap may already exist

`transfer.rs:540` already does `snapshot_ring.lock().unwrap().push_tick(&bufs)`
on the same `bufs` the `rings` concat consumes one statement later, and the
code comment states they are the same raw pre-processing blocks. `.acsnap`
output (`ac_core::snapshot::write`, FLAC + meta + zip) therefore already
contains the spliced stream, in order, today.

`rings[i]` is that same content with the head `drain`ed — so the snapshot is a
**superset** of the exact estimator window, with ordering intact. For seeing
and locating splice boundaries a superset is strictly better than the exact
slice.

**Instruction to the implementer: take a snapshot of a splicing session and
look at it before building anything for D2.** If the boundaries are visible
there, D2's WAV/FLAC dump is already delivered and the only remaining work is
the counter. Do not build a second audio sink to rediscover this.

## affected modules

- `ac-daemon/src/audio/rings.rs` — new. `CaptureRings`: owns the consumers,
  the four drain bodies, the discard counter.
- `ac-daemon/src/audio/jack_backend.rs` — mechanical: `capture_*` delegate to
  `CaptureRings`. Producer side and the process callback untouched.
- `ac-daemon/src/audio/fake.rs` — opt-in ring-backed mode, virtual cursor,
  `process_secs` charge. Default path unchanged.
- `ac-daemon/src/handlers/transfer.rs` — parse `fake_ring`, guard on `fake`.
- `ac-daemon/tests/` — D3 guard test.

## interface changes

CLI: none. Trait: one defaulted method (`discarded_samples`). Request schema:
one fake-only optional param, guarded, on an existing command.

## ZMQ protocol impact

**No.** No frame-schema change; `ds` is unaffected. The counter stays internal
per the hard fence.

## implementation notes for developer

- Extract from `jack_backend.rs:244` (`wait_ring`) and `:413–509` (the four
  `capture_*`). Preserve statement order verbatim — this extraction is
  behaviour-preserving by construction or it is wrong.
- `fake.rs:149` `make_samples_for` becomes the ring's producer instead of the
  caller's direct source. Per-channel phase continuity already lives in
  `noise_state` / `correlated_ref_pos` / `correlated_meas_pos` — feeding a ring
  must not reset those, or `correlated_pair_delay_relationship_holds_across_
  call_boundary` will catch it (and should).
- Guard pattern to copy: `transfer.rs:461`, `if fake && fake_correlated_pair.is_some()`.
- D3's mutation verification: swap `clear()` and `wait_ring` at the extracted
  site, confirm the test flips. That mutation is now a one-line change in one
  file — another dividend of option B.

## risks

- **Extraction drifts semantics silently.** Mitigation: no logic edits in the
  same commit as the move; run the `#[ignore]`d JACK loopback runbook
  (`tests/it_loopback_ir.rs`) on the rig before the PR, since the headless
  suite cannot cover the JACK drain.
- **`process_secs` picked arbitrarily, spacing never reconciled.** Mitigation:
  D3 sweeps at least two values and asserts spacing tracks `sr/L`. Criterion 5
  is not satisfied by a single measured number with nothing to compare it to.
- **The fake reproduces a splice and the issue gets closed on that alone.**
  The handoff is explicit that confirmed splicing does not explain *three*
  discrete copies. QA should hold criterion 5 hard.

## scope notes

1. Option B moves the `clear()`-before-`wait_ring` code without changing it.
   The hard fence forbids *fixing* that ordering in this slice; it does not
   forbid relocating it unchanged. Flagging explicitly rather than assuming —
   if Markus reads the fence as "do not touch that code at all", fall back to
   option A and accept the weaker evidence.
2. #204 (`FakeEngine cannot model routing`, `needs-design`) is the same
   structural complaint about the same file: the fake models the generator,
   not the plumbing. Option B is the routing-side precedent for #204's option
   1. Worth deciding them together; not merged into this slice.
