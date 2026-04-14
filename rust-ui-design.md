# ac-ui — GPU-Accelerated Spectrum Monitor

Design document for a standalone Rust binary that renders real-time audio
spectra via wgpu/Vulkan. Connects to `ac-daemon` over ZMQ or runs with
synthetic data for rendering performance evaluation.

---

## Goals

1. Investigate whether a wgpu/Rust renderer can handle 10–100 simultaneous
   spectrum displays at 60 fps.
2. Provide a minimal, keyboard-driven spectrum monitor for daily use.
3. Establish the rendering architecture for an eventual full replacement of
   the pyqtgraph UI.

---

## Architecture Overview

```
ac-daemon (ZMQ DATA:5557, PUB)
    │
    │  topic-prefixed JSON frames
    │  "data {\"type\":\"spectrum\", ...}\n"
    │
    ▼
ZMQ SUB thread (receiver.rs)
    │
    │  parse JSON → SpectrumFrame
    │
    ▼
per-channel triple buffers (store.rs)
    │
    │  lock-free read of latest frame
    │
    ▼
render loop
    │
    ├─ upload spectrum data → GPU storage buffers
    ├─ custom wgpu pipeline: line + gradient fill
    ├─ egui overlay: grid labels, readouts
    │
    ▼
wgpu surface present (Vulkan backend)
```

No mutex on the hot path. The ZMQ thread writes one side of a triple
buffer; the render loop reads the other. Intermediate frames are silently
dropped — the UI always shows the freshest data.

---

## Rendering Stack

| Layer | Crate | Role |
|-------|-------|------|
| GPU abstraction | `wgpu` | Vulkan primary, Metal/DX12 fallback |
| Windowing | `winit` | Event loop, keyboard input |
| UI overlay | `egui` + `egui-wgpu` + `egui-winit` | Grid labels, readouts, notifications |
| Spectrum plot | Custom wgpu pipeline | Instanced line+fill via storage buffers |
| Waterfall | Custom wgpu pipeline | Scrolling texture + colormap shader |

egui is used only for text rendering and lightweight overlays — the
spectrum itself is a custom render pass for maximum performance.

---

## Data Format

The UI consumes `spectrum` frames from `monitor_spectrum` on the DATA
socket. Wire format (topic-prefixed JSON):

```
data {"type":"spectrum","cmd":"monitor_spectrum","freq_hz":1000.0,"sr":48000,
      "freqs":[20.0, 21.5, ...],"spectrum":[-80.0, -78.3, ...],
      "fundamental_dbfs":-3.0,"thd_pct":0.01,"thdn_pct":0.02,
      "in_dbu":null,"clipping":false,"xruns":0}
```

Key fields for rendering:

| Field | Type | Use |
|-------|------|-----|
| `freqs` | `[f32]` | X-axis, log-spaced, DC removed, ≤1000 points |
| `spectrum` | `[f32]` | Y-axis, dBFS magnitudes, matching length |
| `freq_hz` | `f32` | Auto-detected fundamental |
| `fundamental_dbfs` | `f32` | Level of fundamental |
| `thd_pct` | `f32` | THD percentage |
| `thdn_pct` | `f32` | THD+N percentage |
| `sr` | `u32` | Sample rate |
| `clipping` | `bool` | Clip indicator |
| `xruns` | `u32` | Cumulative xrun count |
| `in_dbu` | `f32?` | Calibrated input level (null if uncalibrated) |

The `freqs` array is stable across frames for a given SR/FFT size.
The receiver detects changes and only reallocates then.

---

## Interaction

Keyboard-first. Mouse supports zoom/pan within the hovered cell.

| Key | Action |
|-----|--------|
| `Esc` / `q` | Quit |
| `Enter` | Freeze / unfreeze display (data keeps flowing, rendering pauses update) |
| `s` | Save screenshot (PNG) + spectrum data (CSV) |
| `Space` | Toggle peak hold |
| `+` / `-` | Adjust dB range (vertical zoom) |
| `Ctrl+Tab` | Next channel (single-view mode) |
| `Ctrl+Shift+Tab` | Previous channel (single-view mode) |
| `l` | Cycle layout: grid → overlay → single → grid |
| `f` | Toggle fullscreen |

| Mouse | Action |
|-------|--------|
| Scroll wheel | Zoom both axes around cursor |
| `Shift` + scroll | Zoom dB axis only |
| `Ctrl` + scroll | Zoom frequency axis only |
| Left click + drag | Pan frequency and dB window |
| Right click | Reset view (defaults from `theme.rs`) |

Zoom and pan mutate the shared `DisplayConfig` (freq_min/max, db_min/max)
so both the GPU shader and the egui grid labels track the new window
without any CPU-side vertex rebuild.

### Screenshot & Export

`s` writes two files to `~/ac-screenshots/` (created on first save):

- `spectrum_YYYYMMDD_HHMMSS.png` — GPU surface readback, PNG encoded
- `spectrum_YYYYMMDD_HHMMSS.csv` — columns: `freq_hz,ch0_dbfs,ch1_dbfs,...`

File I/O runs in a spawned thread so the render loop does not stall.
A brief "saved" notification appears in the overlay for ~1 second.

---

## Visual Design

Dark, minimal, instrument-like. The data is the interface.

### Palette

```
bg              #0A0A0F     near-black, not pure black
grid_line       #FFFFFF 8%  barely visible
grid_label      #5A5A6A     muted gray
text            #B0B0BA     light gray
clip_led        #DD3333     red, only shown when clipping
cursor_line     #FFFFFF 30% subtle crosshair on hover
```

### Channel Colors

Muted, desaturated. Distinct but not neon.

```
ch0   #4A9EAA   teal
ch1   #AA7A4A   amber
ch2   #AA4A6A   coral
ch3   #6AAA4A   olive
ch4   #7A5AAA   lavender
ch5   #AA944A   gold
ch6   #4AAA7A   mint
ch7   #AA4A4A   warm red
ch8   #4A7AAA   steel blue
ch9   #8AAA4A   lime
```

For channels beyond 10, cycle with shifted lightness.

### Spectrum Rendering

- **Line:** 1.5 px ribbon (quad strip generated in vertex shader),
  channel color at full opacity, slight alpha falloff at edges for AA
- **Fill:** triangle strip from spectrum line to bottom edge, channel
  color at 15% alpha, gradient to 0% at bottom
- **Peak hold:** same hue at 40% alpha, thin line (1 px), decays over time

### Grid

- Frequency gridlines at decades: 20, 50, 100, 200, 500, 1k, 2k, 5k, 10k, 20k Hz
- dB gridlines every 20 dB (adjustable with +/-)
- Labels in `grid_label` color, small monospace, positioned at axis edges
- Grid recedes; data comes forward

### Text Overlay

Floating monospace text, no boxes or panels.

```
Top-right:      48000 Hz │ CH1
Bottom-left:    1000.0 Hz   -3.2 dBFS   THD 0.01%
Bottom-right:   ● connected              (or ● disconnected in red)
```

Only visible when data is flowing (except connection status).

### Font

JetBrains Mono Regular, bundled as TTF in `assets/fonts/`. Loaded via
egui `FontDefinitions` at startup. Three sizes:

- Grid labels: 10 px
- Status text: 11 px
- Readouts: 12 px

### Layout Modes

**Grid:** auto-arranged N channels.
`cols = ceil(sqrt(n))`, `rows = ceil(n / cols)`.
Each cell is an independent wgpu viewport with its own spectrum draw.
At high channel counts (>8), per-channel text is suppressed — only
channel number label. Hover or click to see detail.

**Overlay:** all channels in one plot, different colors. Practical up to
~8 channels. Beyond that, switch to grid.

**Single:** one channel fullscreen. Cycle with `Ctrl+Tab` /
`Ctrl+Shift+Tab`.

---

## Module Structure

```
ac-ui/
├── Cargo.toml
├── assets/
│   └── fonts/
│       └── JetBrainsMono-Regular.ttf
├── src/
│   ├── main.rs                 — winit event loop, wgpu + egui bootstrap
│   ├── app.rs                  — App state, keyboard dispatch, frame logic
│   │
│   ├── data/
│   │   ├── mod.rs
│   │   ├── types.rs            — SpectrumFrame, DisplayFrame, DisplayConfig
│   │   ├── receiver.rs         — ZMQ SUB thread, JSON parse, topic filter
│   │   ├── synthetic.rs        — fake data generator for benchmarking
│   │   └── store.rs            — per-channel triple buffers, peak hold, EMA
│   │
│   ├── render/
│   │   ├── mod.rs
│   │   ├── context.rs          — shared wgpu device, queue, surface format
│   │   ├── spectrum.rs         — instanced line+fill pipeline
│   │   ├── grid.rs             — grid lines + labels via egui painter
│   │   ├── waterfall.rs        — scrolling spectrogram texture (phase 2)
│   │   └── shaders/
│   │       ├── spectrum.wgsl   — vertex + fragment for spectrum trace
│   │       └── waterfall.wgsl  — texture scroll + colormap (phase 2)
│   │
│   ├── ui/
│   │   ├── mod.rs
│   │   ├── layout.rs           — viewport rect computation for N channels
│   │   ├── overlay.rs          — floating text: readouts, notifications
│   │   └── export.rs           — PNG screenshot + CSV dump
│   │
│   └── theme.rs                — all color, size, and style constants
```

---

## Key Types

### `data/types.rs`

```rust
/// Raw frame from ZMQ or synthetic source.
pub struct SpectrumFrame {
    pub freqs: Vec<f32>,
    pub spectrum: Vec<f32>,
    pub freq_hz: f32,
    pub fundamental_dbfs: f32,
    pub thd_pct: f32,
    pub thdn_pct: f32,
    pub in_dbu: Option<f32>,
    pub sr: u32,
    pub clipping: bool,
    pub xruns: u32,
}

/// Processed frame ready for GPU upload.
/// Produced by store.rs after applying peak hold and averaging.
pub struct DisplayFrame {
    pub spectrum: Vec<f32>,       // current (or averaged)
    pub peak_hold: Vec<f32>,      // decaying per-bin max
    pub freqs: Vec<f32>,          // frequency axis
    pub meta: FrameMeta,
}

pub struct FrameMeta {
    pub freq_hz: f32,
    pub fundamental_dbfs: f32,
    pub thd_pct: f32,
    pub thdn_pct: f32,
    pub in_dbu: Option<f32>,
    pub sr: u32,
    pub clipping: bool,
    pub xruns: u32,
}

pub struct DisplayConfig {
    pub db_min: f32,              // default -140
    pub db_max: f32,              // default 0
    pub freq_min: f32,            // default 20
    pub freq_max: f32,            // default sr/2
    pub peak_hold: bool,
    pub averaging_alpha: f32,     // EMA: 0 = off, 1 = instant
    pub frozen: bool,
}
```

### `data/store.rs`

```rust
pub struct ChannelStore {
    channels: Vec<ChannelSlot>,
}

struct ChannelSlot {
    buffer: triple_buffer::Output<SpectrumFrame>,
    peak_hold: Vec<f32>,
    averaged: Vec<f32>,
    last_update: Instant,
    frame_count: u64,
}

impl ChannelStore {
    /// Read latest frames for all channels.
    /// Applies peak decay and EMA averaging.
    /// Returns empty vec for channels with no data yet.
    pub fn read_all(&mut self, config: &DisplayConfig) -> Vec<Option<DisplayFrame>>;
}
```

---

## Spectrum Render Pipeline

### Why Custom wgpu Instead of egui_plot

egui_plot resubmits all vertices via CPU every frame. For 100 channels ×
1000 bins at 60 fps, that is 6M vertices/sec of CPU-side geometry
generation. The custom pipeline uploads bin data as a storage buffer;
the vertex shader generates screen positions. CPU work per frame: one
buffer write per channel.

### GPU Data Layout

All channels packed into contiguous storage buffers:

```
spectrum_data: [ch0_bin0, ch0_bin1, ..., ch0_binN, ch1_bin0, ..., chM_binN]
freq_data:     [ch0_f0,   ch0_f1,   ..., ch0_fN,   ch1_f0,   ..., chM_fN  ]
```

Per-channel metadata in a third storage buffer:

```rust
#[repr(C)]
struct ChannelMeta {
    color: [f32; 4],
    viewport: [f32; 4],     // x, y, w, h in NDC
    db_min: f32,
    db_max: f32,
    freq_log_min: f32,      // log10(freq_min)
    freq_log_max: f32,      // log10(freq_max)
    n_bins: u32,
    _pad: [u32; 3],
}
```

### Shader Sketch (`spectrum.wgsl`)

```wgsl
@group(0) @binding(0) var<storage, read> spectrum_data: array<f32>;
@group(0) @binding(1) var<storage, read> freq_data: array<f32>;
@group(0) @binding(2) var<storage, read> channels: array<ChannelMeta>;

struct ChannelMeta {
    color: vec4<f32>,
    viewport: vec4<f32>,
    db_min: f32,
    db_max: f32,
    freq_log_min: f32,
    freq_log_max: f32,
    n_bins: u32,
    _pad: vec3<u32>,
}

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) edge: f32,
}

const LN10: f32 = 2.302585;
const LINE_HALF_W: f32 = 0.0015;   // NDC units, ~1.5px at 1080p

@vertex
fn vs_line(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) ch: u32,
) -> VsOut {
    let meta = channels[ch];
    let bin = vid / 2u;
    let side = f32(vid % 2u) * 2.0 - 1.0;

    let offset = ch * meta.n_bins;
    let freq = freq_data[offset + bin];
    let mag  = spectrum_data[offset + bin];

    // log freq → normalized x
    let x_n = (log(freq) / LN10 - meta.freq_log_min)
            / (meta.freq_log_max - meta.freq_log_min);
    // dB → normalized y
    let y_n = (mag - meta.db_min)
            / (meta.db_max - meta.db_min);

    // map to viewport in NDC [-1, 1]
    let x = meta.viewport.x + x_n * meta.viewport.z;
    let y = meta.viewport.y + y_n * meta.viewport.w;

    // perpendicular offset for line thickness
    let y_off = side * LINE_HALF_W;

    var out: VsOut;
    out.pos = vec4(x * 2.0 - 1.0, (y + y_off) * 2.0 - 1.0, 0.0, 1.0);
    out.color = meta.color;
    out.edge = side;
    return out;
}

@fragment
fn fs_line(in: VsOut) -> @location(0) vec4<f32> {
    // soft edge AA
    let alpha = 1.0 - smoothstep(0.7, 1.0, abs(in.edge));
    return vec4(in.color.rgb, in.color.a * alpha);
}
```

Fill pass uses a separate vertex function that generates triangles from
the spectrum line down to the bottom of the viewport, with alpha
gradient from `FILL_ALPHA` at the line to 0 at the bottom edge.

### Draw Calls Per Frame

Two instanced draw calls total (not per channel):

1. **Fill:** triangle strip, `n_bins * 2` vertices, `n_channels` instances
2. **Line:** triangle strip, `n_bins * 2` vertices, `n_channels` instances

For 100 channels × 1000 bins: 200K vertices fill + 200K line = 400K
total. Single-digit ms on integrated GPU.

---

## Waterfall Spectrogram (Phase 2)

One `wgpu::Texture` per channel:
- Size: `n_bins × history_depth` (e.g. 1000 × 256)
- Format: `R32Float`
- Each new frame: `queue.write_texture` one row, advance ring pointer

Fragment shader samples texture with scroll offset and applies colormap
(viridis computed in shader, or as a 256×1 lookup texture).

Memory: 100 channels × 1000 × 256 × 4 bytes = ~100 MB VRAM.
Acceptable for a dedicated measurement workstation.

Split view per channel: spectrum top half, waterfall bottom half.

---

## Synthetic Data Mode

For benchmarking rendering without a running daemon.

```rust
pub struct SyntheticSource {
    pub n_channels: usize,
    pub n_bins: usize,
    pub update_hz: f32,
}
```

Generates realistic-looking spectra: pink noise slope (−3 dB/octave),
random harmonic peaks, slow amplitude drift. Writes into the same
`ChannelStore` as the real receiver.

```
ac-ui --synthetic --channels 10  --bins 1000 --rate 10
ac-ui --synthetic --channels 100 --bins 1000 --rate 10
ac-ui --synthetic --channels 100 --bins 4000 --rate 30
```

---

## Performance Targets

| Scenario | Channels | Bins | Data Rate | Target FPS |
|----------|----------|------|-----------|------------|
| Daily use | 1–4 | 1000 | 5 Hz | 60 |
| Multi-channel | 10 | 1000 | 5 Hz | 60 |
| Stress test | 100 | 1000 | 10 Hz | 60 |
| Extreme | 100 | 4000 | 30 Hz | 30+ |

### Metrics (displayed in status bar, toggled with `d`)

- GPU frame time (wgpu timestamp queries)
- CPU frame time (Instant-based)
- Data throughput: frames/sec × bins × channels
- Buffer sizes (computed)

---

## Dependencies

```toml
[package]
name = "ac-ui"
version = "0.1.0"
edition = "2021"

[dependencies]
wgpu = "24"
winit = { version = "0.30", features = ["rwh_06"] }
egui = "0.31"
egui-wgpu = "0.31"
egui-winit = "0.31"
pollster = "0.4"
env_logger = "0.11"
log = "0.4"

serde = { version = "1", features = ["derive"] }
serde_json = "1"
zmq = "0.10"
triple_buffer = "8"
crossbeam-channel = "0.5"
bytemuck = { version = "1", features = ["derive"] }

image = { version = "0.25", features = ["png"] }
chrono = "0.4"
```

---

## CLI

```
ac-ui [OPTIONS]

Options:
  --connect <addr>       ZMQ DATA endpoint [default: tcp://127.0.0.1:5557]
  --ctrl <addr>          ZMQ CTRL endpoint [default: tcp://127.0.0.1:5556]
  --synthetic            Use fake data instead of daemon
  --channels <n>         Synthetic channel count [default: 1]
  --bins <n>             Synthetic bins per channel [default: 1000]
  --rate <hz>            Synthetic update rate [default: 10]
  --output-dir <path>    Screenshot/CSV dir [default: ~/ac-screenshots]
```

---

## Implementation Order

1. **Window + render loop** — `main.rs`, `app.rs`: winit window, wgpu
   surface, egui context, dark clear color, `Esc`/`q` quit.

2. **Theme** — `theme.rs`: all color and size constants.

3. **Synthetic data** — `types.rs`, `synthetic.rs`, `store.rs`: fake
   spectra flowing through triple buffers, single channel.

4. **Spectrum pipeline** — `spectrum.rs`, `spectrum.wgsl`: single
   channel line + gradient fill, log-freq x-axis, linear-dB y-axis.

5. **Grid** — `grid.rs`: decade frequency lines, dB lines, small labels
   via egui painter.

6. **Overlay** — `overlay.rs`: corner readouts (freq, level, THD,
   connection dot). Freeze indicator when `Enter` is pressed.

7. **Export** — `export.rs`: `s` key → PNG readback + CSV dump.

8. **Multi-channel** — `layout.rs`: N-channel grid viewports, instanced
   rendering, layout cycling with `l`.

9. **ZMQ receiver** — `receiver.rs`: real daemon connection, topic
   parsing, channel routing.

10. **Waterfall** — `waterfall.rs`, `waterfall.wgsl`: scrolling
    spectrogram texture per channel.

11. **Performance pass** — stress test with synthetic 100-channel mode,
    optimize bottlenecks, add GPU timing display (`d` key).

---

## Design Decisions & Rationale

**Triple buffer over channel:** We only care about the latest frame. A
channel accumulates backpressure if the UI stutters; a triple buffer
always yields the freshest data and silently drops intermediate frames.

**Instanced draw over per-channel draw calls:** One instanced draw call
for all channels keeps driver overhead constant regardless of channel
count. Per-channel state (color, viewport, data offset) lives in a
storage buffer indexed by `instance_index`.

**Storage buffer over vertex buffer:** Spectrum data is uploaded as raw
floats. The vertex shader generates quad strip geometry from the data —
no CPU-side vertex generation. This moves the work to the GPU where it
belongs.

**egui for text, custom pipeline for plot:** egui_plot would work for a
prototype but does not scale. It regenerates all vertices CPU-side every
frame. The custom pipeline's CPU cost per frame is a single
`queue.write_buffer` regardless of bin count.

**JSON over msgpack:** The daemon already publishes JSON on the DATA
socket. At ≤1000 bins per frame and ≤10 frames/sec per channel,
`serde_json` parsing is microsecond-scale. No need to change the wire
format.

**`zmq` crate (libzmq binding) over `zeromq` (pure Rust):** Matches
what ac-daemon uses. Synchronous API is simpler for the dedicated
receiver thread — no tokio runtime needed.
