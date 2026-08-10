# ui-plan — Tier 2 live instrument, second attempt

Status: **binding plan**. Supersedes nothing (docs/superseded/torc.md / docs/superseded/unified.md already
superseded at ac-ui detach). Historical lessons from the old UI are folded
in as constraints, not carried as features.

Scope of this document: the decisions and milestone order for the new UI.
Each milestone gets its own `handoff-*.md` when it starts; do not pick up
future milestones from here.

---

## 0. Decisions (binding)

| # | Q | Decision |
|---|---|----------|
| D1 | Session model | **Transfer-only at the session level.** One session shape: N measurement channels + one loopback reference. No ref-less mode, no mode switch. Views requiring H (Bode, coherence, IR) simply exist within the same session; ambient/no-stimulus use is the same session with coherence honestly at noise. |
| D2 | Wire contract | UI subscribes to **`transfer_stream` frames only**, extended (additively) with per-channel calibrated spectra + per-channel SPL scalar + processing tags. `monitor_spectrum` is untouched and remains the CLI's path. UI never subscribes to it. |
| D3 | Static references | Voltage cal / SPL cal / mic curve are **per-input-channel properties** (3-layer `Calibration`, unchanged). Applied at derivation time, **never baked into stored samples**. |
| D4 | Snapshot | A snapshot is **raw pre-processing capture + full provenance**, not a saved display. Daemon-side ring buffer of raw samples (all session channels), dumped on command. Everything (weighting, integration, Leq, frac-octave, FFT params, H1, coherence) is re-derivable offline. |
| D5 | Snapshot format | Single `.acsnap` file = zip of `meta.json` + `audio.flac`. FLAC 24-bit, **one multichannel stream** (alignment structural, not promised). `meta.json`: format version, sr, channel map (stream ch → session role), per-channel processing params at capture, full 3-layer calibration snapshot, session config, timestamp, daemon version. Must reprocess identically on another machine years later with zero external state. |
| D6 | Remote | First-class, unchanged: live = ZMQ only (CTRL 5556 / DATA 5557). Snapshot travels over CTRL via chunked fetch (offset/length REQ-REP). DATA socket stays pure frames. No filesystem coupling between UI and daemon. |
| D7 | Ring | 30 s default, config-overridable. Daemon-side. Raw f32 as captured (24-bit provenance from hardware ⇒ FLAC-24 lossless). |
| D8 | Reprocessing site | UI links `ac-core` directly and calls the **identical tested functions** the daemon calls live. One math truth. Snapshot viewing needs no daemon and no audio hardware. |
| D9 | Live SPL | Computed **daemon-side** (weighting + F/S integration + cal chain), shipped in-frame as a scalar with labelled tags. Snapshot SPL recomputed UI-side via ac-core. Reconciled by invariant I-B below. |
| D10 | Per-channel params | Weighting / time integration are per-channel session properties, carried as frame tags, inherited by snapshots at capture, editable per-channel post-hoc in the snapshot viewer. Live view is parameter-static in V1 (set at session start); interactive parameter play happens on frozen data. |
| D11 | FFT/Welch params | Capture-time fixtures for the live stream; **edit-time choices** on snapshots (raw samples support any segmentation). |
| D12 | Crates | In-workspace: `ac-scene` (pure presentation math — frame/snapshot → polylines in data coords, tick values, readout strings; **no GPU, no egui dep**) and `ac-view` (shell: input, session launch, rendering). `ac-core` stays pure DSP. |
| D13 | Renderer | Plain egui painting for V1 (polylines). wgpu deferred to the Ember milestone. Ember = **renderer persistence mode only**; it never touches data values and needs no display-truth tests beyond geometry. |
| D14 | View roster | V1: **spectrum only** (calibrated meas-channel spectrum + SPL readout). Then waterfall (same stream). Then |H|/phase/coherence. Nothing else until proven needed. Old nine-view roster stays dead. |
| D15 | `ac plot` | Tier 1, no UI participation in V1. Snapshots do **not** subsume `plot` (a ring mid-sweep is not a sweep). Future intersection is static overlays only; `ac-scene` therefore treats a trace as data-with-provenance, not "the live stream". |
| D16 | Keyboard | Keyboard-driven, minimal chrome, only-what's-necessary. Finnish layout constraint stands: no `[` `]` `+` `-` bindings. |
| D17 | CLI × snapshots | No CLI manipulation of snapshots. (CLI *trigger* of a snapshot is a possible 5-line follow-up, not V1.) |
| D18 | Spectrum on the wire | Per-channel spectra ship as **band-power aggregated log-spaced columns** computed daemon-side via `ac-core::visualize::aggregate` (IEC 61260-1 band-power semantics — the "dual trace"/N-dependence lesson). Linear amplitude on the wire; the single dB conversion lives in `ac-scene`. Fine-structure inspection is what snapshots are for. |

## 1. Test invariants (blocking gates)

- **I-A Display truth.** Every number that appears on screen — cursor
  readouts, axis ticks, peak/SPL labels, bin→column mapping — is computed
  in `ac-scene` and tested in CI against **checked-in `.acsnap` fixtures**
  (deterministic, no GPU, no adapter). The renderer is covered by one
  geometry test for the Y-orientation bug class.
- **I-B Live/snapshot parity.** A snapshot taken during a live session and
  reprocessed with unchanged parameters reproduces the daemon's frame
  values (spectra, SPL, H, coherence) within tolerance. This single
  invariant covers the live path, the snapshot path, FLAC losslessness,
  and calibration application end-to-end.
- **I-C Cross-tier parity.** #99 extends to the new frame fields: a mic'd
  channel reports the same physical level in `plot`, `monitor_spectrum`,
  and `transfer_stream` spectra.
- **I-D Cross-technique.** H1 magnitude from a transfer session agrees with
  a swept `plot` response on a linear DUT within tolerance (bench runbook,
  `#[ignore]`'d like the JACK loopback test). Strongest external check —
  the two paths share no code above ac-core.

## 2. Milestones (one handoff each, in order)

| M | Slice | Crates touched | Gate |
|---|-------|----------------|------|
| M0 | `transfer_stream` frame v2: per-channel calibrated spectra (D18), SPL scalar (D9), tags. Shared cal-chain helper lift from `monitor.rs`. | ac-core, ac-daemon | I-C; additive-only wire check (ac-cli tests green) |
| M1 | Snapshot backend: ring (D7), `snapshot` CTRL command, chunked fetch (D6), `.acsnap` write/read (D5), ac-core offline derivation (D8). | ac-core, ac-daemon | I-B |
| M2 | `ac-scene`: frame/snapshot → scene. Display-truth tests on checked-in fixtures. | ac-scene | I-A (scene half) |
| M3 | `ac-view` shell: egui polyline spectrum, SPL readout, session launch, snapshot open + per-channel weighting/integration editing, keyboard. | ac-view | I-A (geometry half) |
| M4+ | Waterfall → |H|/phase/coherence views → Ember persistence (wgpu) → static overlays. | later | per-slice |

## 3. Routing gates

- Value-display PRs (M2 onward): **QA sign-off before ux-approved** — QA
  owns whether shown values are true, UX owns how they are shown.
- Wire-frame changes (M0, M1): **architect** review — additive-only,
  contract discipline (linear amplitude, one dB conversion site).
- Ember / aesthetic work (M4+): **UX + architect**, per the old rule.

## 4. Carried lessons (context for agents, not tasks)

- Truth boundary moved: the old display-truth harness needed a real GPU
  adapter; the Ember Y-mirror bug lived where tests couldn't see it. Hence
  D12/D13 — pure scene, dumb renderer.
- One conversion site, band-power aggregation, no max-per-bucket, no
  index-pick decimation for noisy spectra (D18).
- Single stream ⇒ the "two streams with different cadence" bug class
  (the live LF investigation) is impossible by construction (D2).
- `verified: false` honesty default and labelled-tag rules apply to every
  new frame field.
