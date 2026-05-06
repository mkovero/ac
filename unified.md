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

- **OQ1 [§3, §4, §6]** — `[RESOLVED 2026-05-05 — see §10]`
  TransferResult extended in place with `re: Vec<f64>` + `im:
  Vec<f64>` (rather than a separate `ComplexTransferResult` type).
  Existing consumers untouched (added fields are pure additions);
  re/im are computed inside the existing H₁ loop from the same
  complex value as mag/phase, so all four representations stay
  consistent. Wire-frame extension is similarly additive, so old
  subscribers ignoring re/im keep working.

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

- **OQ7 [§6]** — `[RESOLVED 2026-05-04 — see §10]` `ScopeFrame`
  source: wire frames via a new `visualize/scope` topic. Daemon
  emits per-channel f32 samples per `monitor_spectrum` tick,
  capped at 2048 samples per frame; UI consumes via
  `data/receiver.rs` → `data/store.rs::ScopeStore`. Resolved in
  favour of wire frames (over direct JACK-ring access) because
  the daemon already runs `monitor_spectrum` worker — adding a
  sidecar emit there is cheaper than a second consumer of the
  JACK ring, and it keeps the architecture uniform across
  same-host vs remote operation.

- **OQ11 [§6]** — Takens auto-τ via autocorrelation-first-zero.
  Defer until at least one real-audio path exists (now: yes,
  via Phase 0b — but Takens is mono-only and the synthetic
  carrier still applies; revisit when Takens consumes
  `active_channel` real audio).

- **OQ12 [§6]** — Resolved 2026-05-04 by Phase 0b: Goniometer
  M/S↔raw rotation toggle is bound to plain `R` (Goniometer
  view only). Replaces the broken-on-trackpad scroll-toggle that
  flipped on every micro-event.

- **OQ13 [§6]** — PhaseScope3D camera nudge keys vs scroll-only:
  scroll for v1 (zoom plain, az on Ctrl+scroll, el on
  Shift+scroll). Add keyboard nudge keys later if useful.

- **OQ14 [§6]** — Auto-expand monitor channel set when
  Goniometer is the initial view (`--channels 0` →
  `[0, 1]`)? V1: no, user opts in via `--channels 0,1`. The
  overlay says "synthetic — no stereo (ch 1 not present)" so
  the user knows what to do. Revisit if it proves an annoying
  trip-up in daily use.

- **OQ15 [§6]** — Mic-curve correction on `visualize/scope`
  samples? V1: no; trajectory views are dimensionless. Calibrated
  quantities live on the `visualize/spectrum` (or `cwt` / etc.)
  frame the user already has.

- **OQ16 [§6]** — Adaptive scope cap at high SR (192 kHz × 200
  ms tick = ~38 k samples >> 2048 cap). V1: truncate to newest
  2048 (~10 ms @ 192 kHz); revisit if visible aliasing appears
  on the Goniometer figure.

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

- `(2026-04-30) Phase 0a substrate runs the synthetic sine source
  internally; ScopeFrame wire schema (OQ7) deferred to 0b.` —
  Lets the substrate's deposit/decay/display pipeline be validated
  in isolation without committing to a wire-format decision that
  is independent of substrate viability.

- `(2026-04-30) Phase 0a uses three separate render passes
  (decay → deposit → display) over an R16Float ping-pong texture
  pair, no compute shaders.` — wgpu disallows reading and writing
  the same texture in one pass; ping-pong avoids the problem
  without needing storage-binding feature gates that vary by
  backend. Compute approach deferred — only worth the complexity
  if the profiler shows the decay-pass cost dominating the frame
  budget (OQ5).

- `(2026-04-30) Initial palette pair shipped: blackbody (Tanner
  Helland Planck approximation) and a hand-tuned warm ramp.` —
  Resolves OQ3 for v1 by shipping both behind a single `set_palette`
  toggle. Default to blackbody; perceptual readability comparison
  on real measurement views (Bode, coherence) waits until those
  views are wired in Phase 2.

- `(2026-05-04) Phase 0b ScopeFrame wire schema: JSON
  visualize/scope, one frame per channel per monitor_spectrum tick,
  samples capped at 2048, frame_idx synchronizes L+R.` — Resolves
  OQ7. JSON for parity with every other visualize/* topic; cap
  matches the largest plausible UI render-frame at 60 Hz (~16 ms ×
  48 kHz ≈ 800 samples + headroom). Bandwidth ≤8 KB/frame ×
  ~100 frames/s = 800 KB/s worst case, well below ZMQ inproc
  throughput.

- `(2026-05-04) Phase 0b stereo pairing happens at the dispatch
  site, not the builder.` — Builder takes
  `Option<(&[f32], &[f32])>`; the Goniometer / PhaseScope3D
  dispatch arms validate frame_idx within ±1 tick + length
  equality before passing Some. Keeps the builder pure /
  unit-testable; the synthetic-fallback path remains identical.

- `(2026-05-04) ScopeStore is Arc<Mutex<HashMap<u32, ring>>>
  mirroring LoudnessStore — per-physical-channel, no preallocated
  slots.` — `ChannelStore`'s triple-buffer scheme keys by slot index
  and preallocates at start; the late-arriving per-channel scope
  stream doesn't fit that, especially for users running
  `--channels 10,11`. Mutex contention is irrelevant at
  ~100 ops/s × ~8 KB.

- `(2026-05-04) Phase 0b stereo only fires when both channels are
  in the monitor set.` — No automatic monitor-channel expansion;
  the user opts in via `--channels N,N+1`. Overlay reads
  "synthetic — no stereo (ch Y not present)" otherwise so the
  user knows what to do. (See OQ14 — revisit if this trips users
  up regularly.)

- `(2026-05-04) Goniometer M/S↔raw rotation: bound to plain R
  key (Goniometer view only); preceding scroll-toggle removed.` —
  Trackpad scroll deltas flipped the binary toggle on every
  micro-event, which made the figure thrash between rotations.
  Resolves OQ12. Ctrl+R global reset retained behind its
  modifier guard.

- `(2026-05-04) PhaseScope3D dropped.` — The history-iteration
  approach (re-project all samples each frame at z based on
  deque-index) fights the substrate's deposit-then-fade contract:
  every frame redeposits samples at slightly-shifted positions,
  producing visible venetian-blind / lattice artefacts that
  obscure the actual 3D structure. Fixing it cleanly would need
  wallclock-anchored z + auto-rotation (parallax) + reduced
  history density — significant scope for a view that's lower-
  leverage than Goniometer / Takens. Removed end-to-end (variant,
  builder, App fields, --view flag, W-cycle slot, tests). The
  ember substrate slot it occupied collapses to: Scope →
  SpectrumEmber → Goniometer → Takens.

- `(2026-05-04) Takens delay embedding wired to real audio
  (active_channel mono via scope_store).` — `MonoStatus` enum
  (Real { ch } / NotStreamingYet { ch } / NoAudio) drives a
  state-aware caption; auto-gain (separate `ember_takens_peak`
  tracker) so quiet content fills the cell; synthetic AM 800 Hz
  stays as fallback when no scope frames have arrived. Status
  caption shows current τ in samples so the user can read the
  scroll-knob value without looking at the notification line.

- `(2026-05-04) Z key wipes ember substrate to black + drops
  per-view history rings + resets stereo/takens auto-gain peaks.`
  — Lets the user A/B test signals cleanly without τ_p decay
  bleeding old content into the new figure.

- `(2026-05-04) JACK port enumeration filters to IS_PHYSICAL.`
  — Bug: 'ac generate sine 10 -10' was resolving channel 10 to
  the daemon's own 'ac-daemon:in' port (the daemon's own audio
  sink, IS_INPUT-flagged from JACK's POV). Adding IS_PHYSICAL to
  both playback_ports() and capture_ports() filters out the
  daemon's own ports, PipeWire/PulseAudio bridges, and other
  client buses. resolve_output_by_channel also changed from
  silent-fallback to a synthesised port name → loud Result error
  when the channel is out of range.

- `(2026-05-05) Takens dropped — academic origin (dynamical-
  systems phase-space), 1 % THD orbit deformation barely visible,
  no audio analyser ships a Takens view (Audio Precision, Prism,
  Studer test sets, etc.). Replaced with IoTransfer for the same
  diagnostic question.` — Honest re-evaluation: Takens is a
  phase-space-reconstruction tool from chaos research, not
  standard bench practice for distortion-shape diagnosis.

- `(2026-05-05) IoTransfer view added — input-vs-output transfer
  Lissajous on the existing substrate.` — Channel semantics:
  `active = ref input`, `active + 1 = DUT output`. Linear
  pass-through DUT → diagonal at slope = gain; soft compression
  → S-curve; hard clipping → flat tops on the diagonal;
  asymmetric class-A → asymmetric line about origin; crossover
  distortion → kink near zero; slew-limiting → hysteresis loop.
  This is the textbook analog-bench distortion-shape view (Audio
  Precision, Studer, Prism — every distortion analyser has it).
  Implementation reuses Goniometer's resolve_stereo_pair,
  update_stereo_peak, StereoStatus, scope_store path; the
  `build_iotransfer_polyline` body is Goniometer's raw-rotation
  path with relabelled axes.

- `(2026-05-05) Phase 2 — BodeMag + Coherence on the substrate.`
  — Reuses the existing `transfer_stream` daemon worker (no new
  wire schema); auto-registers `(active, active+1)` as a
  TransferPair on view-entry via new
  `App::ensure_transfer_pair_for_active` so the user thinks
  "I want bode of ch N → ch N+1" and gets it without manually
  registering pairs first. Builders `build_bodemag_polyline`
  (signed dB, single-trace, max-aggregation per log-spaced
  column — same anti-moiré pattern as `build_spectrum_polyline`)
  and `build_coherence_polyline` (γ²(f), min-aggregation per
  column for pessimistic γ²). Long τ_p (~4 s) gives the free
  fade-diff workflow promised in §5: tweak a console knob,
  watch the new curve fade in over the previous one.
  BodeMag default dB window changed to (-40, +40) so unity gain
  lands at mid-cell — typical Bode work centres around 0 dB,
  not the spectrum default of (-120, 0).

- `(2026-05-05) Phase 2.5 — BodePhase + GroupDelay on the
  substrate.` — Round out Phase 2's frequency-domain set with the
  remaining pair from `unified.md` §6. Same `transfer_stream`
  data, same auto-pair convention. BodePhase: wrapped phase in
  [-180°, +180°] (matches the daemon's TransferFrame; no unwrap
  applied at view time so the trace stays on a familiar axis).
  GroupDelay: τ_g = -dφ/dω in milliseconds, computed from a
  forward-difference derivative of the *unwrapped* phase. New
  `unwrap_phase_deg` helper handles the ±360° jumps the daemon's
  wrapped phase introduces (without unwrap, finite-difference
  produces ±360°/Δf spikes). Default y windows: (-180, +180)
  for phase (natural range), (-5, +20) ms for group delay
  (covers most realistic audio DUTs; tunable via [/]). Phase
  aggregation: first-valid-per-column (signed quantity, no
  meaningful "peak" or "floor" to bias toward).

- `(2026-05-05) Phase 3 — complex H plumbing.` — `TransferResult`
  in `ac-core/visualize/transfer.rs` gains `re: Vec<f64>` and
  `im: Vec<f64>` parallel to magnitude_db / phase_deg, computed
  inside the existing H₁ loop from the same complex value so all
  four representations are mutually consistent (round-trip:
  `|H| = √(re² + im²)`, `arg(H) = atan2(im, re)`). Daemon's
  transfer_stream worker downsamples re/im in lockstep with the
  existing fields; mic-curve correction extends to (re, im) by
  scaling by `10^(-curve_db/20)` (preserves arg(H) while shrinking
  |H| consistently with the dB correction). Wire frame is
  backwards-compatible: `re` / `im` are pure additions and the
  ac-ui `TransferFrame` defaults them to empty when older daemons
  omit them. Resolves §9 OQ1 — option chosen: extend
  TransferResult in place (single struct, no separate
  ComplexTransferResult type, no plumbing duplication).

- `(2026-05-05) Phase 4b — IR view (impulse response).` —
  Daemon-side IFFT of the full-resolution `H₁(ω)` from
  `TransferResult` (NOT the downsampled wire re/im — IFFT needs
  the raw `nperseg/2 + 1 = sr/2 + 1` bins to recover h(t)
  correctly), shipped as a new `visualize/ir` sidecar to
  `transfer_stream`. UI gets a centred (`fftshift`-style) time-
  domain h(t) downsampled to ≤ 2000 samples for wire economy plus
  metadata (`dt_ms`, `t_origin_ms`, `stride`) so the view can
  label the time axis. Per-pair `IrStore` mirrors `LoudnessStore`'s
  shape but keyed by `TransferPair` since IR is per-(meas, ref).
  Builder `build_ir_polyline` drops a faint y=0.5 baseline first
  (visible in flat regions of the IR), then the trace; auto-gain
  shares the existing `ember_stereo_peak` (Goniometer / IoTransfer
  / Nyquist all share — never simultaneously visible). Reference
  IR vs Tier 1: this is Tier 2 visualisation only — no mic-curve
  correction, no calibration. For measurement-grade IR use the
  Tier 1 sweep path. New `unwrap_phase_deg` co-resident defensive
  bin-0 / bin-Nyquist imaginary-part zeroing in
  `impulse_response_from_h` handles the float-noise residue
  realfft inverse refuses.

- `(2026-05-05) Phase 6 — UI state persistence.` — v1 persists
  the small set of user-tunable knobs that change per-session
  (`view_mode`, `ember_intensity_scale`, `ember_tau_p_scale`,
  `ember_gonio_rotation_ms`) to `~/.config/ac/ui.json`.
  Per-cell dB windows / freq zooms intentionally NOT persisted
  in v1 — they get tweaked dozens of times per session in normal
  use, and persisting them means a stale window from yesterday
  clamps today's measurements off-screen until the user resets.
  ViewMode persisted as a string token (not the enum directly)
  so renaming a variant doesn't bork old configs — unknown
  tokens silently fall back. Schema versioned; missing /
  corrupt / version-mismatch all fall back to defaults via
  `log::warn`. Save is debounced ~500 ms after the last
  mutator (W cycle, ,/. for intensity, Shift+,/. for τ_p, R
  for goniometer rotation) so rapid keypress sequences don't
  write the file every frame; force-flushed on
  `ApplicationHandler::exiting`. CLI override: `--view`
  always wins for the launch session (single-launch override
  pattern); next save captures the new view as the persisted
  default. New `--no-persist` flag and matching test fixture
  `disable_persist: true` keep test runs from polluting the
  developer's real on-disk state.

- `(2026-05-05) Phase 4 — Nyquist locus on the substrate.` —
  Plot (Re H, Im H) parametrically as a curve in the complex
  plane, parameterised by frequency. Consumes the re/im fields
  from Phase 3 directly. Auto-gain (shared with Goniometer /
  IoTransfer's `ember_stereo_peak`) scales the curve so the
  largest |H| sits at ~0.85 of cell radius; a faint 64-vertex
  unit-circle reference is deposited at low intensity so the
  user has a |H| = 1 boundary to read against. Off-cell vertices
  are skipped (no clamp artefacts at substrate edges when DUT
  gain spikes faster than auto-gain can decay). Same auto-pair
  convention as the Bode views (active = meas, active+1 = ref);
  caption shows "nyquist (ember) │ meas ch X → ref ch Y │
  unit circle = |H|=1". IR view (Phase 4b) deferred — meaningful
  IR-via-IFFT needs higher bin density than the current 2000-bin
  downsample; will land alongside an "extended bins" toggle on
  the daemon worker.

- `(2026-05-06) Per-vertex coherence weighting for transfer
  views.` — Resolves the "fuzzy spiky sun" problem first observed
  on Nyquist with stereo mics on uncorrelated music: γ² is near
  zero across most bins in that scenario, so H is meaningless
  but the substrate dutifully accumulates a noise-dominated
  scatter into a saturated core with random radial spikes. The
  fix: each polyline vertex now carries a per-bin confidence
  weight `w = γ²^k` (clamped to [0,1]); deposit shader multiplies
  the global intensity by `w`. Bright = trustworthy, dim = noisy
  — no more equal-weight rendering of garbage and signal. Soft
  weighting rather than a hard threshold so there's no
  discontinuity to tune and the substrate's natural decay
  finishes the job. Applies to BodeMag, BodePhase, GroupDelay,
  Nyquist; Coherence view excluded (gating γ² on γ² is
  circular), IR view excluded (no per-sample γ² — IFFT collapses
  the bin axis). Static reference geometry (Nyquist unit
  circle, IR baseline) always weight = 1.0.

---

## 11. [STATUS] Progress log

Append-only. Each entry: `(YYYY-MM-DD) — Summary.`

- `(2026-05-06) — Bridge empty log columns in transfer-view
  polylines.` — Pre-existing bug, surfaced by side-by-side
  inspection: BodeMag/BodePhase/GroupDelay/Coherence appeared
  to "ignore" everything below ~1 kHz. Cause was an axis
  mismatch — the daemon downsamples transfer frames to 2000
  *linear-spaced* bins (~12 Hz/bin at sr=48 kHz), but the
  polyline builder aggregates into 512 *log-spaced* columns,
  so columns at low freq are wider in Hz than the bin spacing.
  Result: most leftmost columns get no bin, the builder reset
  `prev = None` on every empty column, and the polyline broke
  apart into invisible single-vertex fragments.
  Fix is one-liner per builder: skip empty columns *without*
  resetting `prev`, so the trace bridges the gap (one segment
  spanning the empty range — geometrically a straight line
  in (log f, dB) space, which is exactly what a Bode plot
  draws anyway). The intentional "below dB floor → break
  polyline" behaviour in the legacy spectrum builder is
  unaffected; that filter happens before col-aggregation.
  Tests had been masking this because `dense_freqs` (the
  shared test helper) generates *log-spaced* bins, where
  every column gets coverage. Added `linear_daemon_freqs(n,
  sr)` and two regression tests asserting the leftmost
  emitted vertex sits at xn < 0.10 (~30 Hz, well below the
  1 kHz cutoff the user observed). 663 → 665 workspace.

- `(2026-05-06) — Goniometer + IoTransfer move to TransferPair
  selection.` — The two trajectory views were the last holdouts on
  the active+1 stereo convention. Now they go through the same
  `resolve_transfer_pair_for_active` resolver as Bode/Nyquist/IR;
  the resolved pair feeds a refactored
  `resolve_stereo_pair(pair, scope_store, want)` that maps
  `(meas, ref_ch)` → `(L = ref_ch, R = meas)` (matches IoTransfer's
  X/Y convention; Goniometer is symmetric so the labelling is
  arbitrary but stays uniform). Pair registration via Space + T
  exactly as for transfer views — Tab cycles between registered
  pairs once on a virtual cell.
  StereoStatus enum: `NoSecondChannel { l }` (active+1 not in
  monitor set) → `NoTransferPair` (no T-registered pair). Captions:
  Goniometer "synthetic — Space-select L + R, then T"; IoTransfer
  "synthetic — Space-select REF + DUT, then T". Synthetic carrier
  fallback unchanged — first-launch users still see the rotating
  ellipse without having to register anything.
  +5 tests covering the new resolver: NoAudio/NoTransferPair/
  NotStreamingYet/Real/partial-frames. 658 → 663 workspace
  passing.

- `(2026-05-06) — Explicit transfer-pair selection for Bode/
  Coherence/Nyquist/IR.` — Drop the active+1 auto-register that
  fired whenever the user entered a transfer view. Replaced by a
  read-only resolver that reads `virtual_channels.pairs()` and
  the current `active_channel`:
  - No pair registered → `None` (overlay shows "no transfer
    pair — Space-select MEAS + REF, then T").
  - `active >= n_real` (Tab'd onto a virtual channel slot) →
    `pairs[active - n_real]`. Tab now cycles which pair the
    transfer view is rendering — the natural multichannel UX.
  - `active < n_real` (real channel) → `pairs[0]`. Lets W →
    BodeMag show *something* without forcing the user to Tab
    onto a virtual cell first.
  Pair registration stays on `T` (Space-select MEAS + REF, then
  T): the active+1 heuristic only worked for two-channel mic
  setups and was creating unwanted virtual channels for users
  with more than two inputs. `ensure_transfer_pair_for_active`
  removed; `resolve_transfer_pair_for_active` is read-only
  (`&self`), so the render hot path no longer needs to thread
  through `&mut self` to look up which pair to draw. Resolver
  factored into a free `resolve_transfer_pair(pairs, active,
  n_real)` so the resolution rule can be tested without
  standing up an App. 5 new tests covering empty / virtual-slot
  / real-channel-fallback / past-the-end / no-real-channels
  cases. Overlay captions updated for all six transfer views.
  653 → 658 workspace passing.

- `(2026-05-06) — Coherence-weighted ember deposition.` — Soft
  per-vertex confidence weighting via γ²^k. Vertex format
  `[f32; 2] → [f32; 3]` (x, y, w); deposit shader multiplies
  intensity by per-vertex weight. New `coherence_weight(γ², k)`
  helper short-circuits to 1.0 when k=0 (off) and clamps γ² to
  [0,1] before exponentiation; non-finite γ² → 0 (defensive).
  Per-builder coherence handling:
  - **BodeMag**: column-aggregator now tracks the γ² of the bin
    that won the max (parallel to the value).
  - **BodePhase**: same parallel tracking through the
    first-valid aggregator.
  - **GroupDelay**: per-derivative coherence is `min(γ²[i],
    γ²[i+1])` — the derivative is only as trustworthy as its
    weakest input phase.
  - **Nyquist**: per-bin γ² direct; unit-circle reference
    always w = 1.0.
  - **Coherence** + **IR**: w = 1.0 always (no meaningful γ²
    to gate against).
  - **Scope / SpectrumEmber / Goniometer / IoTransfer**:
    w = 1.0 always (no per-bin coherence in their data).
  New App field `ember_coherence_k` (default 2.0); persisted in
  ui.json (additive serde default — old configs migrate
  silently); `K` keybind cycles {0, 1, 2, 4} with notify
  feedback. 8 new tests: `coherence_weight` truth table,
  BodeMag weights track γ², BodeMag k=0 disables weighting,
  Coherence/IR views never weight, Nyquist weights track γ²,
  Nyquist unit-circle reference always full weight, persist
  round-trip for k, missing-field migration default. 645 →
  653 workspace tests passing (+8 ac-ui).
  Visual: open Nyquist view with two-mic music input. Before:
  saturated white core + radial spike haze (everything equal-
  weight). After (default k=2): trace concentrates on
  high-coherence bins (typically room modes, narrow tonal
  features); low-γ² bins fade to invisibility through the
  weighted deposition + substrate decay. Press `K` to cycle
  off/k=1/k=2/k=4 and watch the noise floor lift or drop.

- `(2026-04-30) Initial draft of unified.md.` — Document created,
  current state inventoried against ac-rs codebase, view catalog
  drafted, rendering substrate spec drafted, phasing plan
  established, open questions enumerated. No code written.

- `(2026-04-30) Phase 0a — substrate scaffolded.` — Added
  `ac-ui/src/render/ember.rs` (~580 lines) plus three WGSL files
  (`ember_decay.wgsl`, `ember_deposit.wgsl`, `ember_display.wgsl`).
  EmberRenderer holds the R16Float ping-pong pair, three render
  pipelines, palette LUT (blackbody + warm baked at startup), a
  vertex buffer for deposit points, and a synthetic-sine driver
  that emits one (x,y) point per audio sample at 48 kHz / 1 kHz.
  Wired through `ViewMode::Scope` with a new `WSlot::Scope`
  variant in the W-key cycle and a `--view scope` CLI flag.
  All 141 ac-ui tests pass; full workspace 580 pass + 1 ignored.
  Visual validation (the "clean glowing trace" criterion in §8)
  is left to the user — needs a display.

- `(2026-05-04) Phase 0a follow-ups + Phase 1 trajectory views +
  Phase 0b real-stereo plumbing — backfilled log entry.` — The
  log was stale between 2026-04-30 (Phase 0a scaffold) and
  2026-05-04. This entry covers everything that landed in that
  window:
  - **Phase 0a follow-ups** (April 30 → May 3, ~10 commits):
    `SpectrumEmber` view consuming real `SpectrumFrame`s through
    the substrate; per-column FFT-bin aggregation to kill log-axis
    moiré; mirrored-envelope polyline; faster fade; sample rate
    pulled live from frames instead of hardcoded; post-smoothing
    feed (so `O`/`A`/`I` keys apply); strip-chart scroll and
    deposit-density tuning on Scope; `,` / `.` keys for live
    intensity tuning across all ember views; black-background
    cleanup; continuous-trace smoothing.
  - **Phase 1 — trajectory views** (commit `899df16`, May 3):
    `ViewMode::{Goniometer, PhaseScope3D, Takens}` on the substrate.
    Goniometer + PhaseScope3D originally drove a 1.0 / 1.3 kHz
    incommensurate Lissajous; replaced with a same-1 kHz carrier
    plus 0.3 Hz phase drift on R so the figure cycles through every
    phase state in ~3 s — the demo a phase scope is *for*. Takens
    uses an AM-modulated 800 Hz mono carrier with a τ knob (scroll
    sweeps 1..=4096 samples geometrically). PhaseScope3D camera:
    plain scroll = zoom, Ctrl+scroll = az, Shift+scroll = el.
    8 new ac-ui tests (builder connectivity, substrate-box
    invariants, history caps, orthographic-centre projection).
  - **Phase 1 saturation tuning** (commit `22ca57c`, May 4): the
    initial τ_p / intensity values were tuned like Scope's
    strip-chart but trajectory views revisit the same Lissajous
    pixels ~50× per second — the substrate saturated to white.
    Dropped τ_p (0.4–0.6 → 0.08–0.15) and intensity (0.0025 →
    0.0008 / 0.00025) to restore visible fade. PhaseScope3D
    history cap dropped 4800 → 1800.
  - **Phase 0b — real stereo audio** (May 4): new
    `visualize/scope` ZMQ topic in the daemon emitting raw f32
    samples per channel per `monitor_spectrum` tick (capped 2048;
    `frame_idx` synchronises L+R within a tick). UI plumbed via
    `ScopeFrame` type, `ScopeStore` (`Arc<Mutex<HashMap<u32,
    VecDeque<f32>>>>`) populated by the receiver, and a
    `resolve_stereo_pair` dispatch helper that takes
    `active_channel` as L and `active_channel + 1` as R. Builders
    `build_goniometer_polyline` / `build_phase3d_polyline` gain
    `Option<(&[f32], &[f32])>` argument; real-audio branch
    bypasses synthetic phase advancement so cold-start fallback
    is seamless. Overlay shows source state via `StereoStatus`
    enum: `ch X + Y` / `synthetic — no stereo (ch Y not present)`
    / `synthetic — daemon not streaming scope yet (ch X+Y)` /
    `synthetic 1 kHz + 0.3 Hz phase walk`. R-key (Goniometer
    only) toggles M/S↔raw rotation; the preceding broken-on-
    trackpad scroll-toggle was removed. Resolves §9 OQ7 and
    OQ12.
  - **Tests**: ac-ui from 141 → 166 passing; ac-daemon adds
    `monitor_spectrum_emits_scope_frames` integration test
    asserting non-empty samples in [-1, 1], shared `frame_idx`
    across L+R within a tick, and monotonic per-tick increment.
  - Visual validation (the "real Lissajous tracks input phase"
    criterion) left to the user — needs JACK + at least 2
    channels in the monitor set: `ac-ui --view goniometer
    --channels 0,1`.

- `(2026-05-05) — Phase 4b IR view + ac-core IFFT helper.` —
  ac-core `impulse_response_from_h(re, im)` does the
  `realfft::plan_fft_inverse` + fftshift; daemon's transfer worker
  emits a `visualize/ir` sidecar (~2000 sample h(t) +
  dt_ms / t_origin_ms / stride) per pair per tick alongside the
  existing `transfer_stream`. UI: new IrFrame + IrStore (per
  TransferPair, mirrors LoudnessStore), receiver branch,
  ViewMode::Ir, build_ir_polyline (baseline + trace + auto-gain
  sharing the ember_stereo_peak with Nyquist/Gonio/IoTransfer).
  Tests: 4 new ac-core IFFT tests (round-trip recovers Dirac
  from flat H, defensive empty-input guard), 1 new daemon
  integration test (IR sidecar shape + cap + centred t_origin),
  5 new UI builder tests (empty / too-short / flat-zero /
  substrate-box / centred-impulse-peak-at-mid-cell). 645
  workspace passing.
  W-cycle: scope → spectrum (ember) → goniometer → iotransfer →
  bode mag → coherence → bode phase → group delay → nyquist →
  ir → matrix. Ten ember slots; only pole-zero (Phase 5) remains
  from §8's substrate views.
  Visual: `ac-ui --view ir --channels 0,1`. Linear pass-through
  → tight peak at mid-cell. Long room reverb tail → energy
  decays from t=0 to the right. Pre-ringing (bandlimited
  filters) → energy in the left half (pre-causal taps).

- `(2026-05-05) — Phase 6 UI state persistence.` — Survives
  restarts. New `data/persist.rs` module (UiState struct,
  schema_version=1, JSON I/O, ViewMode ↔ string-token
  conversion). App::new loads on startup; mutators
  (`mark_ui_dirty`) flag for debounced disk write in `redraw`
  (500 ms quiet window so key-mashing doesn't hammer the
  filesystem); `--view` overrides persistence per-launch;
  `--no-persist` disables both directions for benchmark / test
  use. 7 new persist unit tests (default round-trip,
  missing-file fallback, corrupt-file fallback, schema-
  mismatch fallback, full save/load round-trip, every
  ViewMode round-trips through the string token, unknown
  token returns None). 637 workspace tests passing.

- `(2026-05-05) — Phase 4 Nyquist locus on the substrate.` —
  First view to consume Phase 3's re/im fields. Parametric (Re,
  Im) curve in the complex plane with auto-gain (shared with
  Goniometer/IoTransfer's stereo peak — single auto-gain knob
  the user can clear via Z) and a faint unit-circle reference at
  4× lower deposit density. Off-cell bins skipped to avoid
  clamp-edge artefacts. 5 new tests: empty re/im guard,
  mismatched-length defensive empty, unity-real lands on +x ray,
  unit quarter-circle traces arc at radius 0.45, off-cell bins
  skipped. unknown_view_errors_helpfully test probe changed from
  "nyquist" → "polezero" (was becoming a moving target as Phases
  rolled forward). 624 workspace tests passing.
  W-cycle: scope → spectrum (ember) → goniometer → iotransfer →
  bode mag → coherence → bode phase → group delay → nyquist →
  matrix. Nine ember slots; only IR (Phase 4b) and pole-zero
  (Phase 5) remain from §8's substrate views.
  Visual: `ac-ui --view nyquist --channels 0,1`, route a known
  test signal through your DUT (ref ch 0, DUT out ch 1). Linear
  pass-through DUT: trace concentrates near (1, 0) (re=1, im=0).
  Resonant DUT: full Nyquist loop encircling origin. Phase
  inversion: trace flips to (-1, 0) region.

- `(2026-05-05) — Phase 3 complex H plumbing.` — TransferResult
  in ac-core now carries re/im parallel to mag/phase, computed
  from the same H₁ in the existing Welch loop (single struct, no
  duplicated entry point — resolves §9 OQ1). Daemon's
  transfer_stream worker downsamples re/im in lockstep + applies
  the mic-curve correction multiplicatively to (re, im) to keep
  arg(H) untouched while the dB correction shrinks |H|. Wire
  frame extension is back-compatible (legacy subscribers ignore
  the new fields). Tests: `unity_loopback_re_im_consistent` in
  ac-core (round-trip mag/phase ↔ re/im consistency, unity-gain
  Re ≈ 1 + Im ≈ 0); `transfer_stream_emits_data_and_done`
  extended to assert re/im presence and bin-by-bin
  `|H| ≈ √(re² + im²)` consistency. 619 workspace tests passing.
  Invisible to the UI in this commit — Phase 4 (Nyquist) lands
  next and consumes these fields directly.

- `(2026-05-05) — Phase 2.5 BodePhase + GroupDelay on the
  substrate.` — Rounds out the Bode quartet (mag/phase + coherence
  + group delay), all on the existing `transfer_stream` data.
  Adds `unwrap_phase_deg` helper for jump-free derivatives
  (group delay needs unwrapped phase), defaults to wrapped
  phase for the Bode-phase view (matches what every other tool
  shows). 7 new tests: unwrap-recovers-linear-ramp,
  unwrap-handles-jitter-near-wrap, BodePhase substrate-box
  invariant, GroupDelay sign vs. positive phase slope,
  GroupDelay flat-phase-traces-mid-cell, and 2 empty-frame
  guards. 177 ac-ui tests passing (+7).
  W-cycle: scope → spectrum (ember) → goniometer → iotransfer
  → bode mag → coherence → bode phase → group delay → matrix.
  Eight ember slots covering the entire Phase 2 plan from §8.

- `(2026-05-05) — Phase 2 frequency-domain views: BodeMag +
  Coherence on the substrate.` — Both consume the existing
  `transfer_stream` wire frame (no new daemon work). Auto-pair
  registration: when the user enters BodeMag or Coherence,
  `App::ensure_transfer_pair_for_active` registers
  `(monitor[active], monitor[active]+1)` as a TransferPair if
  not already there + (re)starts the daemon worker. Substrate
  parameters: τ_p ≈ 4 s, intensity 0.005, γ 0.6 / gain 1.5
  (single-trace at the transfer worker's ~10 Hz tick — much
  sparser deposit than spectrum so intensity is bumped vs
  SpectrumEmber). 5 new builder unit tests + the existing
  parser test extended for `bode_mag` / `bodemag` /
  `coherence` / `coh` aliases. 170 ac-ui tests passing.
  W-cycle is now Scope → SpectrumEmber → Goniometer →
  IoTransfer → BodeMag → Coherence — six ember slots covering
  signal-time, signal-freq, stereo-phase, distortion-shape, and
  the diff-friendly Bode/coherence pair.
  Visual: `ac-ui --view bode_mag --channels 0,1`, then
  externally route a known signal through your DUT (ref ch 0,
  DUT out ch 1). Watch the magnitude curve fade-trail under
  itself as you change the DUT.

- `(2026-05-05) — Phase 1.5 trajectory cleanup: dropped Takens,
  added IoTransfer.` — Takens removed end-to-end (ViewMode +
  WSlot variants, App fields, build_takens_polyline + helpers +
  3 unit tests, --view CLI flag, W-cycle slot, MonoStatus enum,
  format_takens_status_line, OverlayInput fields). IoTransfer
  added: new `build_iotransfer_polyline` (Goniometer's raw-
  rotation path with axes relabelled X = ref input, Y = DUT
  output), reuses every Phase 0b helper (resolve_stereo_pair,
  update_stereo_peak, StereoStatus, scope_store), state-aware
  overlay caption ("iotransfer (ember) │ ref ch L → dut ch R"
  / synthetic fallbacks). Tests: 4 new IoTransfer (unity-
  diagonal, clipped flat-tops, synthetic fallback in unit box,
  phase-state freeze), 3 dropped (takens history /  τ skip /
  real-audio freeze). 165 ac-ui tests passing.
  W-cycle is now Scope → SpectrumEmber → Goniometer →
  IoTransfer — every slot earning its place. Visual:
  `ac-ui --view iotransfer --channels 0,1`, send a known sine
  to ref-in, route through DUT into +1; clean line = linear,
  flat tops = clipping, S-curve = soft compression, ellipse
  opening = phase shift.

---

## Appendix A — Per-view (x,y) reference card

Quick reference. `s` is a sample, `t` is sample index (or wall time
for strip-chart), `H` is complex transfer function, `f` is freq.

| View          | (x, y) per element              | Source                |
|---------------|----------------------------------|----------------------|
| Scope         | (t mod W, s/A)                   | ScopeFrame            |
| Goniometer    | ((L−R)/√2, (L+R)/√2)             | stereo ScopeFrame     |
| IoTransfer    | (ref, dut_out)                   | scope pair (ref, dut+1) |
| BodeMag       | (log f, mag_db)                  | TransferFrame         |
| Coherence     | (log f, γ²)                      | TransferFrame         |
| BodePhase     | (log f, φ)                       | TransferFrame         |
| GroupDelay    | (log f, −dφ/dω)                  | TransferFrame         |
| Nyquist       | (Re H, Im H)                     | TransferFrame (re/im) |
| IR            | (t, IFFT(H))                     | IrFrame (Phase 4b)    |
| Pole-zero     | (Re p, Im p)                     | rational fit of H (Phase 5) |

Every row uses the same renderer. The whole instrument is "pick a
row, draw."
