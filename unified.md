# unified — planning document

A unified measurement instrument view for `ac`. Multiple synchronized
views of the same underlying signals and transfer functions, sharing a
single rendering substrate based on CRT phosphor physics ("ember"
renderer).

This document is the living plan. Sections marked **[STATUS]** are
append-only logs updated as work proceeds across sessions.

---

## 1. Goals and non-goals

### Goals

- A single GPU-rendered instrument that exposes the data already
  computed by `ac-core` in multiple complementary views: time-domain
  scope, complex-plane FRF (Nyquist), pole-zero, group delay, impulse
  response, coherence, goniometer, 3D phase scope, Takens delay
  embedding.
- Synchronized cursors across views: hovering frequency f₀ in one view
  highlights the corresponding locus point on every other frequency-
  parameterized view; hovering time t₀ highlights the corresponding
  trajectory point on every time-parameterized view.
- A shared rendering substrate (persistent luminance buffer +
  exponential decay + additive deposition + tone-mapped through a
  blackbody color ramp) so that adding a view collapses to specifying
  its (x,y) function, not designing a new rendering pipeline.
- Free before/after diff for static-plot views (Bode/coherence/group
  delay/IR) by setting τ_p large: the previous measurement decays as
  the new one is deposited, producing fade transitions automatically.
- Tier 2 throughout. Live, exploratory, technique-labeled. Not a
  Tier 1 replacement.

### Non-goals

- Not a replacement for `ac plot`, `ac sweep`, `ac noise`, or any
  Tier 1 archival measurement. Does not produce `MeasurementReport`s.
- Not a DAW plugin, mastering meter, or production-grade VU/PPM.
- Not an RF VNA. Smith chart projection is deferred until an
  impedance-domain consumer exists.
- Not real-time audio effects. The renderer reads audio; it does not
  process audio in the signal path.
- Not a replacement for the existing `SpectrumRenderer` or
  `WaterfallRenderer`. The unified instrument is a sibling that
  reuses their data sources.

---

## 2. Current state

This section inventories what exists in `ac-rs` today that the unified
instrument will consume, extend, or reuse. File references are to the
workspace root.

### Architecture and crates

The workspace splits cleanly:

- **`ac-core`** — pure library, no sockets, no global state.
  - `measurement/` — Tier 1 (filterbank, weighting, THD, sweep,
    noise, report).
  - `visualize/` — Tier 2 (spectrum, CWT, CQT, reassigned, transfer,
    aggregate, fractional_octave, weighting_curves,
    time_integration).
  - `shared/` — Tier 0 primitives used by both tiers (calibration,
    conversions, constants, generator, types, fft_cache,
    mic_curve_filter, reference_levels).
- **`ac-daemon`** — JACK/CPAL/fake audio backends, ZMQ REP+PUB
  server, worker threads. Real-time-safe JACK callback.
- **`ac-cli`** — REQ/SUB client. 28+ commands.
- **`ac-ui`** — wgpu + egui GPU UI. Existing renderers:
  - `render/spectrum.rs` (275 lines, custom WGSL).
  - `render/waterfall.rs` (484 lines, custom WGSL with ring-buffer
    history texture and palette LUT).
  - `render/sweep.rs`, `render/grid.rs`, `render/virtual_overlay.rs`
    (egui-based).

The `WaterfallRenderer` is the most architecturally relevant precedent
for the ember substrate: it already maintains a persistent 2D history
texture, instances multiple cells, samples a palette LUT in the
fragment shader, and supports per-cell zoom/pan via a uniform struct.
The ember substrate replaces "ring of FFT rows" with "additive
deposition into a persistent luminance buffer with per-frame decay" —
the *shape* of the rendering pipeline is already established.

### Transfer function pipeline (most critical for unified)

`ac-core/src/visualize/transfer.rs` implements a Welch-averaged H1
estimator:

- `welch_all(x, y, ...)` computes Gxx, Gyy, Gxy from one set of
  per-segment FFTs (no redundant work).
- `estimate_delay()` does FFT cross-correlation with ±1 s lag bound.
- `h1_estimate_with_delay()` skips redundant delay estimation across
  ticks (used by the streaming worker; the ref↔meas path is
  physically constant during a session).
- Returns `TransferResult { freqs, magnitude_db, phase_deg,
  coherence, delay_samples, delay_ms }`.

**Critical gap**: `TransferResult` does *not* preserve the complex
H(ω). Magnitude and phase are computed from H1 internally
(`h1.norm().log10()`, `h1.arg().to_degrees()`) and the complex
intermediate is discarded. Every Nyquist, pole-zero, group-delay-
from-complex, and IR-from-IFFT view needs `Vec<Complex<f64>>`. This
is a data-model change in `ac-core`, not a UI-layer workaround.
**Decision required** in §10.

The handler `ac-daemon/src/handlers/transfer.rs` runs the streaming
worker, downsamples to ≤2000 points for wire economy, applies
mic-curve correction, and emits `transfer_stream` frames over the
DATA PUB socket every ~50 ms (20 Hz target).

### Wire protocol (constraint, not flexible)

ZMQ REP+PUB on tcp/5556 (CTRL) and tcp/5557 (DATA). Topics: `data`,
`done`, `error`, `cal_prompt`, `cal_done`. Per `ARCHITECTURE.md` §
"Wire message conventions", every published frame carries a tier
marker in `type`, e.g. `visualize/spectrum`, `visualize/cwt`,
`measurement/frequency_response/point`. The legacy flat names
(`type: "spectrum"`, `type: "transfer_stream"`) are aliased during
migration.

The unified instrument introduces **view-state synchronization**
(cursors, view selection, layout) which is conceptually different
from measurement state. View state is high-frequency (every mouse
move) and ephemeral (not archived). Putting it on the same DATA
socket would either flood subscribers or starve measurements at
HWM; putting it on CTRL would serialize it through REQ/REP latency.

**Decision required** in §10: separate ZMQ topic, separate socket,
or in-process channel only (no cross-process cursor sync in v1).

### UI layout primitives (already present)

`ac-ui/src/data/types.rs`:

- `LayoutMode { Grid, Single, Compare, Sweep }` — existing per-channel
  layout switcher.
- `ViewMode { Spectrum, Waterfall }` — existing per-cell view switcher.
- `CellView { freq_min, freq_max, db_min, db_max, rows_visible, ... }` —
  per-cell zoom/pan state, kept independent so mouse interactions
  target only the hovered cell.
- `DisplayConfig { averaging_alpha, frozen, layout, view_mode,
  active_channel }` — global display settings.

The unified instrument extends these: `ViewMode` gains variants
(`Scope`, `Goniometer`, `Nyquist`, `PoleZero`, `Takens`, …), each
configured by a per-cell parameter struct, all rendered through the
ember substrate. `LayoutMode::Grid` is the natural unified mode
(N×M cells of arbitrary view types).

### Data flow already in place

`ac-ui/src/data/`:

- `receiver.rs` — ZMQ SUB consumer. Decodes frames into typed structs.
- `store.rs` — `ChannelStore`, `LoudnessStore`, `SweepStore`,
  `TransferStore`, `VirtualChannelStore`. Indexed by channel /
  transfer pair.
- `smoothing.rs` — EMA averaging.
- `types.rs` — `SpectrumFrame`, `TransferFrame`, `LoudnessReadout`,
  `SweepPoint`, `SweepDone`.
- `triple_buffer` crate is used between data thread and render
  thread (see `app.rs::Input<...>`).

The triple-buffer pattern is the existing answer to "how does audio-
rate data reach the GPU thread without locking" and applies directly
to ember deposition: audio samples (or sample batches) cross the
thread boundary via triple buffer, the render thread drains and
deposits.

### Tier 2 calibration awareness (constraint)

Per `ARCHITECTURE.md` § "Calibration", every visible dB value carries
voltage / SPL / mic-curve corrections from `shared/calibration.rs`.
Ember-substrate views that display calibrated quantities (Bode,
coherence, IR, scope-dBFS-axis) must respect this. Trajectory views
(goniometer, Takens, Nyquist) are dimensionless or relative and
don't need SPL correction; they may benefit from voltage
calibration for axis labels.

### Out of scope

`ds/` (Python diagnostics session manager) is explicitly out of scope
per current design direction. The unified instrument is `ac-ui`.

---

## 3. Data model

### What a "measurement" is

For Tier 2 streaming (the unified instrument's domain), a measurement
is a continuous frame stream, not an archived report. The existing
`TransferFrame`, `SpectrumFrame`, etc. in `ac-ui/src/data/types.rs`
are the unit. The unified instrument adds:

- **`ScopeFrame`**: time-domain capture (raw f32 samples, channel id,
  sr, timestamp). Already implicitly available from the JACK ring;
  not currently exposed as a frame. Decision in §10.
- **`ComplexTransferFrame`** (or extension of `TransferFrame`):
  preserves complex H(ω) plus the existing magnitude/phase/coherence.
  Required for Nyquist, pole-zero, group-delay-from-complex, IR-via-
  IFFT.

### What a "session" is for the unified view

For v1: implicit. The unified instrument is *live*. A session is "the
window is open, frames arrive, views render." No on-disk session
artifact.

For v2 (deferred): a captured session would record the inbound frame
stream + timestamps, allowing scrubbing/replay. Out of scope for the
phasing plan below.

### Per-view runtime state

Each cell holds:

- View type tag (`Scope`, `Spectrum`, `Bode`, `Coherence`, `Nyquist`,
  `PoleZero`, `IR`, `GroupDelay`, `Goniometer`, `PhaseScope3D`,
  `Takens`).
- Source spec (which channel, or which transfer pair).
- Substrate parameters: `tau_p` (decay constant, seconds), `bloom`
  (0..1), `palette` (BlackBody | Viridis | Phosphor), `intensity_gain`.
- View-specific parameters (e.g. Takens delay τ in samples; Nyquist
  axis bounds; scope sweep mode).

### Persistence

Per-session state (window layout, per-cell config) persists to
`~/.config/ac/ui.json` alongside the existing config. Schema
versioned per the existing `schema_version` convention. Loading
stale schemas is non-fatal: unknown view types fall back to
`Spectrum`.

---

## 4. View-state and cursor protocol

### What needs to be synchronized

- **Frequency cursor** (`f_cursor: Option<f64>`, Hz): from any
  frequency-axis view, shared with all frequency-parameterized
  views (Bode, coherence, group delay, Nyquist, pole-zero) and
  hinted on time views as the period 1/f.
- **Time cursor** (`t_cursor: Option<f64>`, seconds): from scope/IR,
  shared with embedding and phase-portrait views (highlights the
  trajectory point at that t).
- **Selection** (which cell is "active" for keyboard shortcuts).
- **Layout** (cell grid, view assignments).

### Recommended protocol (decision in §10)

In-process only for v1. Cursor state lives in `ac-ui` as a single
`SharedViewState` struct, mutated on the UI thread, read by the
render pipeline each frame. No ZMQ traffic for cursor sync.

Rationale: cursor sync is a UI concern, not a measurement concern.
The daemon does not need to know where the user's mouse is. Adding
a ZMQ topic for it would couple two layers that currently don't
need coupling, and the existing `triple_buffer` data path is for
*frames*, not view-state.

For v2 (deferred): if a remote/headless inspection mode is ever
wanted (e.g. follow-mode where one workstation drives another), a
new PUB topic `view/cursor` on the existing DATA socket is the
natural extension. Frames would be `{f_cursor, t_cursor,
timestamp_ms}` at ≤30 Hz.

### Frame schema additions

For frames where complex H(ω) is needed, extend `TransferFrame` with
optional `re: Vec<f32>`, `im: Vec<f32>` arrays (parallel to
existing `magnitude_db` / `phase_deg`). Old subscribers ignore the
new fields; the unified instrument requires them for Nyquist /
pole-zero / IR. The downsampled point set (≤2000 indices) used by
the streaming worker remains the same — re/im are downsampled in
parallel with mag/phase.

---

## 5. Rendering substrate — the ember pipeline

This is the spine of the unified instrument. Every view differs only
in its (x,y) function per sample (or per spectral bin). Rendering
treatment is universal.

### Substrate model

A persistent 2D **luminance buffer** L(x,y) lives across frames.
Two operations act on it:

1. **Deposition**: per audio sample (or per spectral bin), draw a
   point at (x,y) into L with additive blending. Constant
   per-sample intensity. The natural 1/|v| brightness behavior
   (slow segments glow brighter than fast ones) emerges
   automatically because slow segments have more samples per
   pixel.
2. **Decay**: once per rendered frame, multiply the entire buffer
   by `exp(-Δt / τ_p)` where Δt is wall-clock time since last
   frame and τ_p is the configured phosphor time constant for the
   cell.

Display is L mapped through a tone curve to RGB. Optional
bloom: separable Gaussian blur of L, added back additively at
reduced intensity.

### Buffer format

R16Float per cell (single luminance channel). Color is added at
display time via the tone-mapped LUT — the buffer itself is
monochrome luminance. RGBA16Float is rejected as wasteful unless
per-deposition color becomes a feature, which it is not in v1.

For multiple cells sharing the framebuffer: one R16F texture array
indexed by cell, mirroring the `WaterfallRenderer`'s existing
`texture_2d_array<f32>` pattern in
`render/shaders/waterfall.wgsl`.

### Tone mapping (color ramp)

The default palette is **blackbody radiation**: deep red → orange →
yellow → white-hot, mapped from the Planck locus. Palette is
implemented as a 1D LUT texture (one row per palette, like the
existing waterfall palette table).

**Open question** in §10: blackbody is the physics-honest choice and
matches the "ember in the dark" aesthetic, but it has poor
perceptual uniformity at the low end (deep red is hard to
distinguish from black). Alternative palettes to ship: a perceptually
tuned warm ramp (interpolated through Lab space), classic P31
phosphor green, and amber. All are LUT swaps; the substrate is
indifferent.

Tone curve before LUT lookup: `t = clamp(L^γ * gain, 0, 1)`, with γ
≈ 0.6 by default (matches CRT phosphor response shoulder), gain
configurable per cell.

### Sweep modes for time-domain views

- **Strip-chart scroll** (default): horizontal axis is wall time,
  buffer translates leftward each frame. Cheap, signal-agnostic.
- **Triggered sawtooth**: classic scope sweep with retrace blanking
  and a trigger condition (level + slope or zero-crossing). Useful
  for periodic signals; only meaningful when the user has selected
  a trigger.

Both are deposition-pattern variations on the same substrate.

### Audio-thread vs render-thread deposition

**Decision** (recorded in §10): deposition runs on the **render
thread**, batching samples consumed from a triple-buffered ring
each frame. The audio thread only writes to the existing JACK
capture ring (already real-time-safe; `audio/jack_backend.rs`
guarantees no allocations and no locks in the process callback).
The render thread drains the ring, deposits points/lines into the
GPU texture via `queue.write_texture` or a vertex buffer of
points.

Rationale:

- Audio-thread GPU access is not real-time safe and would couple
  the JACK callback's worst case to the GPU's worst case.
- Triple-buffer hand-off is the existing pattern in `ac-ui`.
- At 48 kHz with 60 Hz render, 800 samples/frame per channel is
  trivially manageable as a vertex stream.
- 192 kHz with 30 Hz render is 6400 samples/frame — still fine; if
  this ever becomes a bottleneck, decimation by N is a one-line
  fix at the cost of dropping fine detail.

### Routing static-plot views through the substrate

**Decision** (recorded in §10): Bode magnitude, Bode phase,
coherence, group delay, and impulse response views render through
the same substrate, with τ_p set very long (5–10 s).

Consequence — the diff workflow becomes free: when a new
`TransferFrame` arrives, the previous trace is still glowing, the
new one deposits over it, and the eye sees a fade transition that
visually answers "what changed?" without explicit before/after
overlay logic.

Risk to validate during the substrate prototype: small-amplitude
detail (e.g. −60 dB bins) may be invisible against bloom. Mitigation
levers if it bites: per-view bloom-strength override; reduced bloom
on Bode/coherence specifically; switching those views to a
non-bloom render path while keeping the trajectory views glowing.
This is a tunable, not an architectural fork.

### Implementation footprint

Rough sketch (not committed code; just sizing):

- One WGSL shader for deposition (vertex stream → texture, additive
  blend). ~50 lines.
- One WGSL shader for decay pass (fullscreen quad, multiply
  texture in place via render-to-self with a copy ping-pong, since
  wgpu textures aren't read+write in one pass). ~30 lines.
- One WGSL shader for display (sample L, tone curve, palette LUT).
  ~50 lines.
- Optional: separable Gaussian blur for bloom. Two passes. ~80
  lines total (existing wgpu examples cover this).
- Rust side: `EmberRenderer` struct holding the texture array,
  decay/display pipelines, palette LUTs. Models on
  `WaterfallRenderer`. ~400 lines.

Total: ~600 lines of new code for the substrate. Each view on top
adds ~50–150 lines (the (x,y) mapping plus per-view parameter
struct).

---

## 6. View catalog

Each view collapses to: source data → (x,y) per sample/bin → per-cell
substrate parameters. Per-view rendering details are absent because
they are universal.

### Time-domain views

#### Scope

- **Source**: `ScopeFrame` (raw audio samples for one channel).
- **(x,y)**: x = (sample_index % sweep_window) / sweep_window, y =
  sample / max_amplitude. Strip-chart scroll: x = wall_time
  modulo window length.
- **Cursor**: emits time cursor on hover (sample index → seconds).
- **Per-view params**: sweep mode (strip / triggered), trigger
  level + slope, sweep window length.
- **Notes**: the simplest possible deposition — one point per
  sample. Validates the substrate.

#### Goniometer (2D phase scope)

- **Source**: stereo `ScopeFrame` (two channels).
- **(x,y)**: x = (L−R)/√2, y = (L+R)/√2 (M/S rotation by 45°).
  Optionally swap to (L, R) raw.
- **Cursor**: time cursor on hover (closest trajectory point).
- **Per-view params**: rotation mode (M/S vs LR), gain.
- **Notes**: stereo signal correlation visualized.

#### 3D phase scope

- **Source**: stereo `ScopeFrame`.
- **(x,y)**: 3D position projected to 2D via cell-local camera.
  z = time (recent samples scrolled forward) or z = M/S derived axis.
- **Cursor**: time cursor.
- **Per-view params**: camera azimuth, elevation, zoom; z-axis
  meaning (time vs M/S).
- **Notes**: 3D camera is a matrix multiplication in the deposition
  vertex shader. The substrate is unchanged.

#### Takens delay embedding

- **Source**: mono `ScopeFrame`.
- **(x,y)**: (x(t), x(t−τ)) for 2D, or (x(t), x(t−τ), x(t−2τ))
  projected to 2D for 3D mode. τ in samples.
- **Cursor**: time cursor (highlights the orbit point at t).
- **Per-view params**: τ (samples or ms); dimension (2 or 3);
  3D camera if dim=3.
- **Notes**: τ chosen by user via a knob, or auto-set to first
  zero of autocorrelation (cheap, recomputed once per second on
  a rolling 2 s window). First-minimum-of-mutual-information is
  a future refinement; not v1.

### Frequency-domain views

#### Bode magnitude

- **Source**: `TransferFrame` with magnitude_db.
- **(x,y)**: x = log10(freq / freq_min) / log10(freq_max / freq_min),
  y = (mag_db − db_min) / (db_max − db_min).
- **Cursor**: emits frequency cursor on hover.
- **Per-view params**: long τ_p (5–10 s) for fade transitions.
- **Notes**: connect adjacent points with line primitives, deposited
  with anti-aliased lines. The line-density (per pixel) scales
  with the spectrum's slope, so resonances naturally glow brighter.

#### Bode phase

- **Source**: `TransferFrame` with phase_deg.
- **(x,y)**: y = phase_deg, unwrapped if requested.
- **Cursor**: same frequency cursor.
- **Per-view params**: unwrap toggle, wrap range (±180° or 0..360°).

#### Coherence γ²(f)

- **Source**: `TransferFrame.coherence`.
- **(x,y)**: y in [0, 1].
- **Cursor**: frequency cursor.
- **Notes**: visually obvious where the FRF is trustworthy (γ² ≈ 1)
  vs unreliable (γ² < 0.8). Free; coherence is already computed.

#### Nyquist / complex-plane locus

- **Source**: `ComplexTransferFrame` (re, im arrays).
  *Requires extending `TransferResult` and the wire frame.*
- **(x,y)**: x = Re(H(f)), y = Im(H(f)), parameterized by f (color-
  graded along the curve, optionally with cursor highlight at
  current f_cursor).
- **Cursor**: frequency cursor (highlights the locus point).
- **Per-view params**: axis bounds (auto-fit by default), unit
  circle overlay toggle.
- **Notes**: the view that directly motivates §10's "extend
  `TransferResult` with complex H" decision.

#### Group delay

- **Source**: `ComplexTransferFrame` or unwrapped Bode phase.
- **(x,y)**: τ_g(f) = −dφ/dω, computed from differenced unwrapped
  phase. y in seconds (or ms).
- **Cursor**: frequency cursor.
- **Notes**: cheap derivative once we have phase. Allpass /
  minimum-phase deviation becomes obvious.

#### Impulse response

- **Source**: `ComplexTransferFrame` IFFT'd to time domain.
- **(x,y)**: x = time, y = h(t).
- **Cursor**: time cursor (and freq cursor → hint period).
- **Per-view params**: window length, axis bounds.
- **Notes**: the bridge view between frequency and time. Embedding a
  measured IR in Takens space is conceptually the same operation
  applied to a derived signal — same renderer, different (x,y)
  source.

#### Spectrum (legacy)

The existing `SpectrumFrame` view continues to be served by the
existing `SpectrumRenderer`. Migrating it onto the ember substrate
is *possible* (deposit one point per bin with intensity proportional
to magnitude) but is **not** in the v1 scope. The substrate proves
itself on the new views; the existing spectrum view stays as is
until and unless we choose to retire `SpectrumRenderer`.

### Pole-zero

- **Source**: `ComplexTransferFrame` → vector-fit rational
  approximation → poles and zeros in s-plane (or z-plane).
- **(x,y)**: x = Re(p) (rad/s), y = Im(p) (rad/s). Distinct
  glyphs for poles (×) and zeros (○).
- **Cursor**: hovering a pole highlights the corresponding peak on
  Bode and Nyquist.
- **Per-view params**: rational fit order, fit refresh rate
  (vector fitting is iterative and not free; once per second is
  plenty).
- **Notes**: the only view that requires a non-trivial new
  algorithm. Vector fitting (Gustavsen 1999) has Rust
  implementations; if not, a port from `scikit-rf` or `vectfit3` is
  modest. **Phased to last** because everything else is cheaper
  and the substrate validates without it.

---

## 7. Architectural sketch

```
                                              ┌────────────────────────┐
                                              │   ZMQ DATA (existing)  │
                                              │  visualize/spectrum    │
                                              │  visualize/transfer    │
                                              │  measurement/...       │
                                              └───────────┬────────────┘
                                                          │
                                                  receiver (existing)
                                                          │
                                                          ▼
┌──────────────────┐    triple_buffer    ┌─────────────────────────────┐
│ JACK capture ring│ ─────────────────▶ │   ac-ui data store           │
│ (existing,       │  (samples)         │   (existing: ChannelStore,   │
│  RT-safe)        │                    │    TransferStore, ...)       │
└──────────────────┘                    │                              │
                                        │   + new: ScopeStore          │
                                        │   + new: ComplexTransferStore│
                                        └─────────────┬────────────────┘
                                                      │
                                                      ▼
                                        ┌─────────────────────────────┐
                                        │   render pipeline           │
                                        │                             │
                                        │   ┌───────────────────────┐ │
                                        │   │ EmberRenderer (new)   │ │
                                        │   │  - texture array      │ │
                                        │   │  - deposition shader  │ │
                                        │   │  - decay shader       │ │
                                        │   │  - display shader     │ │
                                        │   │  - palette LUT        │ │
                                        │   └───────────────────────┘ │
                                        │                             │
                                        │   per-cell View enum:       │
                                        │     - Scope, Goniometer,    │
                                        │       Takens, Bode, ...     │
                                        │   computes (x,y) per sample │
                                        │   or per bin, feeds Ember   │
                                        │                             │
                                        │   SharedViewState           │
                                        │     - f_cursor, t_cursor    │
                                        │     - active_cell           │
                                        └─────────────────────────────┘
```

The diagram emphasizes that the unified instrument adds **one
renderer** (`EmberRenderer`), one shared cursor struct, two new
store types, and a new `View` enum dispatched in a single render-
pipeline function. Everything else — JACK, ZMQ, triple-buffer,
existing data flow — is unchanged.

---

## 8. Phasing plan

### Phase 0 — Substrate prototype (highest priority)

Single view (Scope or Goniometer). Full ember pipeline:
deposition + decay + tone-mapped display + bloom. Pluggable
palette. Validates that the approach is viable at audio rates
on the target hardware (NVIDIA open DKMS on Linux X11).

**"This works" criterion**: a sustained sine wave at 1 kHz,
44.1 kHz audio, renders as a clean glowing horizontal trace
(scope) or a clean elliptical orbit (goniometer) at 60 Hz with
no perceptible lag, no buffer overflow, no GPU stalls. Bloom
strength configurable. Blackbody palette readable.

**"This doesn't work" failure modes to watch for**:
- R16F precision banding at low luminance (mitigate by
  switching to R32F if confirmed; cost: 2× memory bandwidth).
- Decay-pass cost dominates frame budget (mitigate by
  combining decay into display pass: read with `* exp(-Δt/τ)`,
  write back; cost: one extra texture write).
- Vulkan/X11 present-mode pathology specific to NVIDIA Linux
  (existing code already supports `WGPU_BACKEND=gl` fallback;
  see `render/context.rs` comment on issue #109).
- Per-sample point primitives not actually fast enough (in
  practice they should be; if not, switch to line strips with
  alpha-by-velocity weighting).

### Phase 1 — Trajectory views on the substrate

Goniometer (if not Phase 0), 3D phase scope, Takens embedding.
Each is a (x,y) function plus per-cell parameters; ~50–150
lines per view. Validates that adding a view is genuinely
cheap.

### Phase 2 — Frequency-domain views: easy ones

Bode magnitude, Bode phase, coherence, group delay. All consume
the existing `TransferFrame`; no new wire schema. Long τ_p
gives free fade transitions for diff workflow.

### Phase 3 — Complex H plumbing

Extend `TransferResult` in `ac-core/visualize/transfer.rs` with
`re`, `im` (or `complex: Vec<Complex<f64>>`). Extend the
streaming worker in `ac-daemon/handlers/transfer.rs` to publish
re/im arrays alongside magnitude/phase/coherence (downsampled
in lockstep). Extend `TransferFrame` in `ac-ui/data/types.rs`.
Backwards-compatible: omit re/im on legacy clients.

### Phase 4 — Nyquist + IR views

Nyquist locus (direct from re/im). IR from IFFT of complex H.
Both straightforward once Phase 3 lands.

### Phase 5 — Pole-zero (last)

Vector fitting integration. Requires either a Rust port of
`vectfit3` or wrapping an existing crate. Lowest leverage; do
last.

### Phase 6 — Persistence

Per-cell config + grid layout saved to `~/.config/ac/ui.json`.
Schema-versioned. Defer until the view set is stable.

### Justification for putting the substrate first

The substrate is the highest-risk, highest-leverage component.
If it works, every view is cheap and the design is correct. If
it doesn't, the view designs are partially invalidated — the
fade-transition diff workflow specifically depends on long-τ_p
deposition, and per-view rendering would need to be redesigned
without that affordance. Knowing this in week 1 vs month 3 is
the difference between a small course-correction and a costly
rewrite.

---

## 9. Open questions

Each is tagged with the section(s) it gates.

- **OQ1 [§3, §4, §6]** — Should `TransferResult` be extended in
  place with `complex: Vec<Complex<f64>>`, or should there be a
  separate `ComplexTransferResult` returned by a new entry point
  (`h1_estimate_complex`)? In-place extension is simpler but
  affects every consumer of `TransferResult`. Separate type is
  cleaner but duplicates plumbing.

- **OQ2 [§4]** — Cursor sync: in-process only (current
  recommendation), or via a new ZMQ topic `view/cursor` for
  multi-process / remote follow-mode? V1 says in-process; v2 is
  trivial to add later.

- **OQ3 [§5]** — Color palette: blackbody (physics-honest, may
  hide low-amplitude detail) vs perceptually tuned warm
  (readable, less aesthetic), vs both selectable. Recommendation:
  ship both, blackbody default for trajectory views, perceptual
  default for measurement views (Bode, coherence). Validate with
  prototype.

- **OQ4 [§5]** — Buffer format R16F vs R32F. R16F is plausibly
  enough; banding at low luminance is the failure mode to watch
  for. Decide after Phase 0 prototype.

- **OQ5 [§5]** — Decay pass: separate fullscreen pass (cleaner)
  vs folded into display pass via read-modify-write (one less
  texture round-trip). Decide based on profiler in Phase 0.

- **OQ6 [§6]** — Takens τ selection: user knob (simple), auto
  via first zero of autocorrelation (adaptive, cheap), or auto
  via first minimum of average mutual information (theoretical
  best, more compute). Recommendation: ship the knob plus
  autocorrelation auto-mode in v1; AMI is a v2 refinement.

- **OQ7 [§6]** — `ScopeFrame` source: should the daemon publish
  raw scope frames as a new `visualize/scope` wire type, or
  should `ac-ui` access the JACK ring directly when running on
  the same host? Direct access is cheaper but ties scope view
  to local-host operation. Wire frames are slower but
  consistent. Recommendation: wire frames at low rate (e.g. 60
  Hz batched packets, ~800 samples/packet at 48 kHz) to keep the
  architecture uniform; revisit if bandwidth becomes a problem.

- **OQ8 [§6]** — Vector-fitting library choice for pole-zero:
  hand-port of `vectfit3`, wrap a C library via FFI, or pure
  Rust if a maintained crate exists. Decide at start of Phase 5.

- **OQ9 [§5]** — Rendering of legacy spectrum view: keep as is
  (current decision) vs migrate to ember substrate. If migrated,
  do existing keyboard shortcuts (averaging α, freeze, etc.)
  still apply meaningfully? Defer.

- **OQ10 [§7]** — Should the unified instrument be a separate
  binary (e.g. `ac unified`) or a mode of the existing `ac-ui`
  (toggled via CLI flag or runtime keybind)? Recommendation:
  mode of existing UI; cell-by-cell view selection naturally
  encompasses both old and new views in one window.

---

## 10. [STATUS] Decisions made

Append-only. Each entry: `(YYYY-MM-DD) Decision — Rationale.`

- `(2026-04-30) Tier classification: unified instrument is Tier 2.` —
  It is live, exploratory, technique-labeled, does not produce
  `MeasurementReport`s. New view code lives under
  `ac-ui/src/render/` and `ac-core/src/visualize/`.

- `(2026-04-30) Rendering substrate is persistence-based phosphor
  model with blackbody (or perceptually tuned warm) tone mapping.` —
  Adopted because (a) it matches the natural temporal structure of
  streaming audio better than per-frame redraw, (b) it collapses
  per-view rendering to (x,y) function specification, (c) it
  handles sustained / transient / dense content gracefully via
  accumulation and decay, (d) it makes before/after diffs free
  for static-plot views via long τ_p.

- `(2026-04-30) Bode/coherence/group-delay/IR views routed through
  the same substrate with long τ_p (5–10 s) rather than
  conventional plotting.` — Enables measurement-to-measurement
  fade transitions and free diff workflow. Risk (small-amplitude
  detail readability under bloom) is a tunable, not an
  architectural fork; mitigations listed in §5.

- `(2026-04-30) Ember deposition runs on the render thread, fed by
  the existing triple_buffer hand-off from the audio thread.` —
  Audio-thread GPU access is not RT-safe; the existing data path
  is the right reuse point.

- `(2026-04-30) Cursor sync is in-process only in v1; ZMQ cursor
  topic deferred to v2.` — Cursor state is a UI concern, not a
  measurement concern. No need to couple `ac-daemon` to UI events.

- `(2026-04-30) Phasing plan: substrate prototype first, then
  trajectory views, then easy frequency-domain views, then
  complex-H plumbing, then Nyquist+IR, then pole-zero, then
  persistence.` — Substrate is highest-risk, highest-leverage;
  validate it before building view logic against unverified
  assumptions about its viability.

- `(2026-04-30) Buffer format default: R16F single channel.
  Color comes from tone-mapped LUT at display time.` — RGBA16F is
  rejected as wasteful; per-deposition color is not a v1 feature.
  Re-evaluate if low-luminance banding becomes visible
  (recorded in §9 OQ4).

- `(2026-04-30) The unified instrument is a mode of the existing
  ac-ui binary, not a separate executable.` — Cell-by-cell view
  selection naturally encompasses old and new views; introducing
  a second binary fragments the UX without benefit.

---

## 11. [STATUS] Progress log

Append-only. Each entry: `(YYYY-MM-DD) — Summary.`

- `(2026-04-30) Initial draft of unified.md.` — Document created,
  current state inventoried against ac-rs codebase, view catalog
  drafted, rendering substrate spec drafted, phasing plan
  established, open questions enumerated. No code written.

---

## Appendix A — Per-view (x,y) reference card

Quick reference. `s` is a sample, `t` is sample index (or wall time
for strip-chart), `H` is complex transfer function, `f` is freq.

| View          | (x, y) per element              | Source                |
|---------------|----------------------------------|----------------------|
| Scope         | (t mod W, s/A)                   | ScopeFrame            |
| Goniometer    | ((L−R)/√2, (L+R)/√2)             | stereo ScopeFrame     |
| 3D phase      | proj₃→₂(L, R, axis_z)            | stereo ScopeFrame     |
| Takens (2D)   | (s(t), s(t−τ))                   | mono ScopeFrame       |
| Takens (3D)   | proj(s(t), s(t−τ), s(t−2τ))      | mono ScopeFrame       |
| Bode mag      | (log f, mag_db)                  | TransferFrame         |
| Bode phase    | (log f, φ)                       | TransferFrame         |
| Coherence     | (log f, γ²)                      | TransferFrame         |
| Group delay   | (log f, −dφ/dω)                  | TransferFrame         |
| Nyquist       | (Re H, Im H)                     | ComplexTransferFrame  |
| IR            | (t, IFFT(H))                     | ComplexTransferFrame  |
| Pole-zero     | (Re p, Im p)                     | rational fit of H     |

Every row uses the same renderer. The whole instrument is "pick a
row, draw."
