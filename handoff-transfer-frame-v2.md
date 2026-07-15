# handoff-transfer-frame-v2 — M0: extend `transfer_stream` for the new UI

Parent plan: `ui-plan.md` (decisions D2, D3, D9, D18; invariant I-C).
One PR. No UI code exists yet; this milestone is daemon + core only.

## Goal

Make one `transfer_stream` frame per pair carry everything the future UI
draws: per-channel calibrated spectra, per-meas-channel SPL scalar, and
the labelled processing tags — all derived from the **same capture blocks**
as H, so everything on screen shares one time origin.

## Deliverables

### 1. Shared calibration/SPL helper (ac-core or daemon `handlers/mic.rs`-adjacent)

Lift the per-channel chain currently embedded in
`handlers/audio/monitor.rs` (voltage cal → SPL offset → mic curve →
weighting → F/S time integration) into a shared helper both `monitor.rs`
and `transfer.rs` call. First-reuse lift per the tier-framing precedent
(#97/#98). No behavior change on the monitor path — its existing tests
must stay green untouched.

### 2. New frame fields (additive only)

Extend the per-pair `transfer_msg` in
`handlers/transfer.rs` (`json!` at ~line 371):

| Field | Shape | Semantics |
|-------|-------|-----------|
| `spec_freqs` | `[f64; K]` | Log-spaced column centre frequencies, fixed grid (proposal: 48 columns/octave over 20 Hz–Nyquist; K ≈ 480 at 48 k). Same grid every frame. |
| `meas_spectrum` | `[f64; K]` | **Linear amplitude**, band-power aggregated (`ac-core::visualize::aggregate`, √(Σ amp²) per column) from the meas channel's Welch segments — the same segments feeding `h1_estimate_with_delay`. Calibrated: voltage cal + mic curve applied (linear domain); **no dB conversion daemon-side**. |
| `ref_spectrum` | `[f64; K]` | Same, reference channel (no mic curve — ref leg is guarded already). |
| `spl` | `f64 \| null` | Per meas channel. Weighted, time-integrated, cal-chain SPL via the shared helper. `null` when no SPL cal layer exists for the channel. |
| `spl_weighting` | tag | `"A" \| "C" \| "Z"` — whatever the session param is. |
| `spl_integration` | tag | `"fast" \| "slow"` (+ future `"leq"`). |
| `cal_tags` | object | Per-channel provenance per the tier-framing labelled-tag rules (voltage / SPL / mic-curve layers present-and-applied or absent), same vocabulary `monitor_spectrum` frames use. |

Existing fields (`freqs`, `magnitude_db`, `phase_deg`, `coherence`, `re`,
`im`, `delay_*`, `mic_correction`, IR sidecar) are **untouched** — names,
shapes, semantics, decimation. Additive only.

### 3. Session params

`transfer_stream` CTRL command accepts optional per-meas-channel
`weighting` and `integration` params (defaults: `Z`, `fast`). Rejected
values → `{"ok": false, ...}` before worker spawn, matching existing
validation style.

## Acceptance criteria (falsifiable)

1. **Presence:** under `--fake-audio`, every `transfer_stream` data frame
   contains all new fields; `spec_freqs` identical across frames.
2. **Amplitude truth:** fake-audio sine at a known dBFS → the column
   containing f₀ in `meas_spectrum` reads the correct linear amplitude
   within tolerance after windowing/aggregation accounting (state the
   tolerance and its derivation in the test).
3. **Band-power semantics:** broadband (pink/white) fake stimulus → the
   level of a given fractional-octave region computed from `meas_spectrum`
   is invariant to FFT size / segment count within tolerance
   (N-independence — the anti-"dual trace" test).
4. **Cross-path parity (I-C extension):** same fake signal, same channel,
   same cal: level derived from `meas_spectrum` matches the
   `monitor_spectrum` path's calibrated level within tolerance; `spl`
   matches the monitor path's SPL for identical weighting/integration
   config. Extends the #99 parity test, does not fork it.
5. **Additive-only:** entire existing test suite green with zero edits to
   existing assertions (`cargo test --workspace`). Any existing-test edit
   is a red flag, not a fixup.
6. **Wire economy:** frame size measured and recorded in the PR
   description (K=480 f64 × 2 channels ≈ 8 KB/pair/frame — confirm).
7. `ZMQ.md` updated with the new fields, marked additive, with the
   linear-amplitude contract stated explicitly ("dB conversion happens in
   the receiver, nowhere else").

## Out of scope (hard fence)

- Any UI / scene / rendering code, any new crate.
- Snapshot, ring buffer, chunked fetch (M1).
- Changes to `monitor_spectrum` behavior beyond the pure helper lift.
- Renaming, removing, re-decimating, or "improving" existing frame fields.
- Live toggling of weighting/integration mid-session (session-start params
  only, per D10).
- `StandardsCitation.verified` flips of any kind.

## Routing

Architect review required (wire-frame change). QA sign-off on acceptance
tests 2–4 (measurement invariants). No UX gate — nothing is displayed yet.

---

## Architect addendum (design-approved)

`.agents/architect.md`'s module map is stale (pre-`ac-rs` workspace);
review done against actual current code.

### decision 0 — Welch normalization (blocking, found during review, not in original spec)

`welch_all()` (`ac-core/src/visualize/transfer.rs:63-101`) accumulates raw
`|FFT|²` into `gxx`/`gyy` with **no window-compensation** (`÷((nperseg/2)·wc)²`).
`spectrum_only()` (monitor path, `spectrum.rs:111`) *does* apply that
normalization — a full-scale on-bin sine reads amplitude ≈1.0 there. Exposing
raw `sqrt(gxx)`/`sqrt(gyy)` as `meas_spectrum`/`ref_spectrum` would silently
fail AC #2 (amplitude truth) and AC #4 (cross-path parity) by construction.
`TransferResult` must gain `gxx`/`gyy` (or pre-normalized amplitude
equivalents) with the same `((nperseg/2)·wc)²` normalization `spectrum_only`
uses, applied inside `h1_estimate_core` before storage. This is new
normalization logic, not a rename — name it and test it explicitly, don't
let it hide inside a generic "apply calibration" pass.

### decision 1 — wire aggregation (D18): daemon-side fixed grid, confirmed

Transfer's Welch freq axis is already uniform-linear-from-sr
(`freqs[k] = k·sr/nperseg`, 1 Hz spacing, `transfer.rs:220`), so
`spectrum_to_columns_wire` — the same function + tests (`t1`-`t6` in
`aggregate.rs`) the monitor path already uses — applies **unmodified**.
Zero new aggregation code. (Ruled out: client-side aggregation in `ac-scene`
— that crate doesn't exist until M2, and D12 keeps it pure-presentation
anyway.)

### decision 2 — linear amplitude / one dB-conversion site: confirmed

`spectrum_to_columns_wire` is already linear-in/linear-out. Only remaining
work is decision 0 (getting a correctly-normalized linear amplitude out of
`h1_estimate_core` at all) plus stating the contract in `ZMQ.md` per AC #7.

### decision 3 — helper location + scope correction

The spec's "lift the per-channel chain currently embedded in `monitor.rs`"
overstates what exists. The cal-lookup chain (voltage cal → SPL offset → mic
curve) **is** already shared identically between `monitor.rs:388-405` and
`transfer.rs`'s per-pair cal lookup — nothing to lift there. What does **not**
exist anywhere is a broadband weighted + F/S-integrated **scalar** SPL number
— `monitor.rs` only has BS.1770 LKFS (different weighting/standard) and
per-band `fractional_octave_leq` (weighted+integrated, but per-band). The
`spl` field is new code for the weighting+integration half, not a lift.

**Decision:** new pure function in `ac-core::visualize` (e.g.
`spl_level.rs`), not daemon-side `handlers/mic.rs`-adjacent as originally
suggested. Reuses `WeightingCurve::db_offset` and
`EmaIntegrator`/`LeqIntegrator` (both already in `ac-core::visualize`,
already tested) unchanged. Rationale: D8 requires this exact function to run
outside the daemon by M1 (offline snapshot reprocessing) and M3 (`ac-view`
linking `ac-core` directly) — building it in ac-core now avoids relocating it
later. Daemon keeps owning calibration *lookup* (`Calibration::load`,
already shared); passes already-calibrated linear amplitude + weighting/
integration mode into the new function.

**Scope clarification:** `monitor.rs`'s wire output gains **no new field** in
M0 (per "no behavior change" + the out-of-scope fence). "Both call it" means
both call the calibration-lookup pieces (already true today) — the new
weighting+integration scalar function is exercised by `transfer.rs` only in
M0. Do not go looking for where `monitor.rs` emits a new SPL field; it
doesn't, in this milestone.

### decision 4 — `spl` field shape: flat, confirmed

Flat scalar + sibling tags (`spl`, `spl_weighting`, `spl_integration`), as
drafted above — not a nested object. House style in this wire protocol is
uniformly flat (`measurement/loudness`, `visualize/spectrum`,
`fractional_octave` all use sibling scalar+tag fields); zero precedent for
nesting a value with its tags. Not a close call.

### affected modules

- `ac-core/src/visualize/transfer.rs` — `TransferResult` gains `gxx`/`gyy`
  (normalized per decision 0); `h1_estimate_core` applies the missing
  window-compensation before storing them.
- `ac-core/src/visualize/aggregate.rs` — no changes; `spectrum_to_columns_wire`
  reused as-is.
- `ac-core/src/visualize/` — new file for the weighting+integration scalar
  function.
- `ac-daemon/src/handlers/transfer.rs` — per-pair closure (~line 371) gains
  the 7 new fields; calls the new ac-core function + `spectrum_to_columns_wire`
  on `sqrt(gxx)`/`sqrt(gyy)`.
- `ac-daemon/src/handlers/audio/monitor.rs` — no field changes.
- `ac-rs/ZMQ.md` — new fields documented per AC #7, linear-amplitude
  contract stated explicitly.

### implementation notes for developer

- Start from `transfer.rs:302-386` (the `par_iter` closure) — same place
  `mag`/`phase`/`coh` already get mic-curve-corrected; build the new fields
  alongside.
- `spectrum_to_columns_wire(&sqrt_gxx_or_gyy, sr, 20.0, sr/2.0, K)` — model
  on `monitor.rs:1330-1338`'s existing usage.
- Voltage-cal application to `gxx`/`gyy`: same `cal.in_vrms(...)`-style
  pattern already used for `in_dbu` in `monitor.rs` — apply before
  aggregation, in linear domain, per D3.
- For the `spl` scalar's test model: `time_integration.rs`'s `EmaIntegrator`
  tests + `weighting_curves.rs`'s `db_offset` tests — the new function is a
  thin composition of both, not new DSP.
- `aggregate.rs`'s `t1`-`t6` tests are the right shape to imitate for AC
  #2/#3 (amplitude truth, N-independence) on the new spectra.

### risks

- Welch normalization (decision 0): make it a named, tested step.
- `gxx`/`gyy` at 1 Hz resolution over up to 24 kHz is a lot of data
  pre-aggregation (nperseg/2+1 ≈ 24000 f64 per channel) computed every
  ~2.5s tick on top of existing H1 work — confirm no CPU regression on the
  transfer worker before merge; not blocking, note in PR description.
- AC #5 (no monitor.rs behavior change) is a real constraint if any refactor
  touches `monitor.rs` — CI catches it, but flag in PR description which
  lines moved vs changed.

**Status: design-approved.** Ready for developer.
