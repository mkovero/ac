<!-- agent: architect -->

# design-mtw-alignment — reference alignment and retention for the MTW ladder

Scope: deliverable 9 of `$AC_HOME/handoff/handoff-mtw-live-spectrum.md` — per-band reference
alignment and reference-retention sizing. Routed to architect as "the decision
that makes or breaks HF usability on a delayed DUT, settled before the ladder
is fixed".

---

## design decision

### core question

**At what rate is the reference alignment applied — once at full rate before
decimation, or per band at each stage's decimated rate?**

Everything else in deliverable 9 follows from this: whether offsets are exact
or rounded, whether there is one offset per pair or one per band, what has to
be retained, and what the snapshot stores.

### option A — align once at full rate, then decimate

The reference is read at a signed integer sample offset `D` at the capture
rate. Both channels then enter *identical* decimation chains, one per band.
Alignment is a single integer per pair; no stage applies an offset of its own.

*tradeoffs:* exact by construction — an integer offset at the capture rate has
no rounding error at any stage. Costs one aligned read per band from a shared
history rather than one shared decimated stream per band (the decimation work
is per band either way, since each stage needs both channels).

### option B — per-band offset at each decimated rate

Each band converts the common delay into its own rate: `offset_b =
round((D + latency_b) / M_b)` decimated samples.

*tradeoffs:* offsets are small integers and the alignment lives next to the
segmentation that consumes it. Costs rounding of up to `M_b/2` capture-rate
samples per band, which is not benign — see below.

### recommendation

**Option A**, on two grounds, the second of which also simplifies the
deliverable.

**1. Option B's rounding error is a constant ~12° of phase at every band's top
edge, by construction.** Worst-case rounding is `M_b/(2·sr)` seconds, and the
ladder assigns band top edges proportional to `1/M_b`, so the product is
scale-invariant. At 48 kHz, NFFT 4096:

| dec | max rounding | band top | phase error at top edge |
|---|---|---|---|
| 4 | 0.042 ms | 811 Hz | 12.2° |
| 16 | 0.167 ms | 203 Hz | 12.2° |
| 64 | 0.667 ms | 50.7 Hz | 12.3° |

Each band lands a different rounding, so the errors do not cancel across a
crossover — they appear as a phase step of up to ~24° where two bands meet.
The slice exists to stop the display claiming things the data does not
support; a fabricated phase step at every crossover is the same defect in a
new place. Option A's error is exactly zero: `D` is already an integer at the
capture rate.

**2. Decimation latency is common-mode and cancels — so there is nothing
per-band left to align.** If meas and ref traverse identical decimators,
write `Xdec = Hdec·X`. Then

    Gxy_dec = E[conj(Rdec)·Mdec] = |Hdec|²·Gxy
    Gxx_dec = |Hdec|²·Gxx
    H1 = Gxy_dec / Gxx_dec = Gxy / Gxx

The decimator's phase cancels in the conjugate product and its magnitude
cancels in the ratio. Within its passband the stage is transparent to H1 —
including its group delay, and including a non-linear-phase design. What must
not happen is the two channels seeing *different* filters.

The handoff's premise that "alignment is per band, because each stage has its
own decimation latency on top of the common `D`" is therefore true only under
option B, where the per-band arithmetic is the implementer's own rounding, not
physics. **Flagging rather than silently narrowing scope:** under option A,
deliverable 9's per-band alignment reduces to one signed integer offset per
pair plus the shared ladder. This is a simplification of the deliverable, not
a reduction of what it achieves — coherence is still delay-invariant in every
band, which is what criterion 8 tests.

Consequence for criterion 13, which reads "the stored *per-band* alignment
offsets": under option A there is one offset per pair. The criterion remains
satisfiable and its mutation test still bites (perturb the stored offset,
parity fails), but the wording should become per-pair when this is adopted. It
is a spec edit, so it is named here rather than assumed.

---

## retention sizing

Retention is bounded by the deepest band's window plus the largest offset the
alignment may ask for, in **either direction**:

    retain ≥ W_deepest + |offset|_max + tick + decimator transient

`W_deepest` is set by the ladder, not by `sr`: NFFT·M/sr is 5.461 s at 48 kHz
(dec 64) and 5.461 s at 96 kHz (dec 128). It does not shrink at higher rates.

|offset|_max is `D_max` (criterion 8 fixes 100 ms) **plus the #216 skew while
that is live** — 0.2 s at 96 kHz, and `estimate_delay` returns `D − skew`, so
the offset is *negative* on today's hardware.

| mode | W_deepest | offset budget | + tick/transient | retain |
|---|---|---|---|---|
| live | 1.365 s | 0.3 s | ~0.15 s | **2 s** |
| bench | 5.461 s | 0.3 s | ~0.15 s | **8 s** |

Memory, f32, per channel: 2 s at 192 kHz = 1.5 MB; 8 s at 192 kHz = 6.1 MB.
An eight-unique-channel bench session at 192 kHz is ~49 MB. Allocate per mode
— bench pays for bench.

**Retention lives worker-side, in the analysis history, not in the RT rings.**
`REF_RING_CAPACITY` (4 × 192_000, i.e. 4.0 s at 192 kHz) stays as it is. It
only has to cover tick jitter; growing it would put the ladder's retention
policy inside the RT allocation and next to issue #25's unbounded-growth
territory for no benefit.

**The EMA does not extend retention.** Accumulator state carries the history;
audio is needed only for the current window. Retention is `W + offset`, not
`n·τ`.

---

## affected modules

- `ac-core/src/visualize/mtw/` — new. `ladder.rs` (stage layout derived from
  `sr`), `align.rs` (signed offset reader over a two-channel history),
  `decimate.rs` (polyphase FIR stages), `ema.rs` (per-band cross-spectrum
  accumulators). Pure crate, no daemon dependency, per deliverable 1.
- `ac-core/src/visualize/pair_derivation.rs` — gains the MTW entry point,
  driven from stored state. Shares `mtw/` with the live path; no second
  implementation.
- `ac-core/src/snapshot/mod.rs` — `SnapshotMeta` gains ladder params, mode, τ,
  PPO, and the alignment offset. `derive_pair` already reads a stored delay
  (`mod.rs:131`, errors when absent) and never re-estimates, which is what
  makes criterion 15's cross-boundary identity hold today.
- `ac-daemon/src/handlers/transfer.rs` — the worker feeds the ladder instead
  of assembling a `target_total` Welch window.

No decimation infrastructure exists in the tree today — this is greenfield,
not a refactor.

## interface changes

Public `ac-core` API gains the `mtw` module and an MTW variant on the
derivation entry point. No existing signature changes. `meta.json` gains
fields (additive; missing fields must fail loudly per criterion 14).

## ZMQ protocol impact

No removals or renames. Additive only, as the handoff's wire contract
specifies: the per-column Δf/window array, the mode tag, N_eff. `ds` consumes
session state, not transfer frames, so it is unaffected.

## implementation notes

- Model the stage layout test on the existing `sr`-derived helpers rather than
  tabulating 48 kHz — criterion 2 tests 44.1/48/96/192 kHz.
- Decimators: linear-phase FIR, cascaded by 2 or 4 per stage, polyphase.
  Stopband ≥ 90 dB — the magnitude floor in `h1_estimate_core` is 1e-6
  (−120 dB), and aliasing above the stopband would land inside the displayed
  range. Both channels **must** use the same filter instance parameters; the
  cancellation argument above is the whole reason per-band alignment is not
  needed, and it fails if the chains differ.
- Assign each band strictly inside its decimator's passband. At the band edge
  `|Hdec| → 0` and `H1 = Gxy/Gxx` divides two vanishing quantities — the
  cancellation is algebraically exact but numerically worthless there.
- The alignment offset is signed. A design that assumes non-negative will
  break on today's hardware, where the offset is ≈ −19200 at 96 kHz.
- Alignment is estimated once per session, as the current worker already does
  (`pair_delays`, cached at warmup). Keep that; the snapshot stores it.

## risks

- **Signed offset overlooked.** #216's skew makes the live offset negative
  today; post-fix it is positive. Mitigation: the offset type is `i64` and the
  history is retained on both sides of the read point. A test at both signs.
- **Alignment goes stale if the DUT path changes mid-session** (repatching,
  `reconnect_input`). Pre-existing — the delay is already cached per session —
  but the ladder makes it visible as HF coherence collapse rather than a phase
  tilt. Mitigation: re-estimate on `reconnect_input`; named here, not scoped
  here.
- **Criterion 8 cannot detect #216.** The alignment absorbs the skew and
  aligns correctly by accident. Mitigation is already written into the
  criterion: verify the skew from per-ring occupancy (`AC_DRAIN_TELEMETRY`,
  PR #215), never from coherence, magnitude, or delay.
- **Bench retention at 192 kHz** is ~49 MB across eight channels. Mitigation:
  allocate per mode, and size from the ladder rather than a constant.
