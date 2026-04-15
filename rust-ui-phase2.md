# ac-ui — Phase 2 Plan

## Status going in

v0.1 (design doc implementation steps 1–9) is landed on `main` and runs
against both synthetic and real daemon sources. The GPU pipeline draws
a log-freq / linear-dB spectrum for up to N channels as two instanced
draw calls (fill + line) via `render/shaders/spectrum.wgsl`. Keyboard
bindings plus mouse zoom / drag-pan / right-click reset are in place.

Deferred from v0.1 and still open:

- Waterfall spectrogram (design doc implementation step 10)
- GPU-timing overlay + 100-channel stress benchmark (step 11)
- Multi-channel real-daemon rendering — blocked on daemon wire format
- `assets/fonts/JetBrainsMono-Regular.ttf` bundle (v0.1 ships with
  egui's default font stack, no custom glyphs yet)
- `ac ui` Python subcommand wrapper

Phase 2 closes all of the above and raises the bar on interaction
ergonomics (crosshair readouts, per-cell view config).

## Status coming out

All five workstreams landed. Summary of what shipped, with pointers to
the actual code so this doc stops being a plan and starts being an
index into the real implementation:

- **WS1 multi-channel real** — `ac-daemon/src/handlers/audio.rs::monitor_spectrum`
  now takes an optional `"channels": [u32, ...]` request array, cycles
  `reconnect_input` per channel within one worker, and stamps each
  published `spectrum` frame with `channel` + `n_channels`.
  `ac-ui/src/data/receiver.rs::route_slot` maps incoming frames onto
  pre-allocated slots first-come-first-served, so `ac-ui --channels N`
  now honours real-daemon data. `ac/ui/spectrum.py` and
  `ac/client/ac.py::cmd_monitor` filter by `channel == input_channel`
  so the Python TUI/monitor behaviour is unchanged.
- **WS2 waterfall** — `render/waterfall.rs` + `render/shaders/waterfall.wgsl`
  implement a per-channel `texture_2d_array<f32>` ring buffer; the
  fragment shader does log-x bin remap when freqs are log-spaced
  (synthetic) and linear remap when freqs are linearly spaced
  (daemon downsample output). The inferno LUT is baked at build time
  by `crates/ac-ui/build.rs`. `w` cycles between `Spectrum` and
  `Waterfall` view modes.
- **WS3 GPU timing + benchmark** — `render/timing.rs` manages a
  `wgpu::QuerySet` with double-buffered readback; `d` toggles the
  overlay (falls back to `gpu n/a (TIMESTAMP_QUERY unsupported)` on
  adapters without the feature). `--benchmark <secs>` in `main.rs`
  runs for N seconds then prints `fps`, `p50/p95/p99 frame ms`, and
  mean CPU/GPU ms via `app.benchmark_report()`. Rolling stats live in
  `ui/stats.rs`.
- **WS4 interaction polish** — `CellView` replaced the shared
  `DisplayConfig` freq/db window (per-cell zoom/pan); hover crosshair
  readout in `ui/overlay.rs`; `Ctrl+R` resets all cells, right-click
  resets the hovered cell only. Scroll modes: plain=zoom freq+db
  together (spectrum) / freq only (waterfall); `Shift+Scroll`=zoom dB
  or colormap; `Ctrl+Scroll`=zoom freq (spectrum) / time window
  (waterfall). Zoom clamps at the data-driven freq ceiling tracked
  live in `App::data_freq_ceiling` so waterfall/spectrum honour 48k
  or 96k daemon output without a CLI flag.
- **WS5 Python wrapper** — `ac/__main__.py::_resolve_rust_bin` finds
  `ac-ui` on `$PATH` then in `ac-rs/target/debug/`, and `ac ui ...`
  replaces the Python process via `os.execvp`. Mirrored in
  `ac/client/ac.py::main` for the direct-`ac` entrypoint.

Late additions not in the original plan, worth knowing about:

- **Linear→dBFS at receiver boundary** — the daemon publishes
  `|FFT|/N/wc` linear amplitude, not dB. `data/receiver.rs` now does
  `20*log10(max(x, 1e-12))` on ingest, matching
  `ac/ui/spectrum.py:131`. Without this, the UI fed raw linear values
  straight into the dB-assuming colormap/axis pipeline and everything
  read as "max loudness".
- **Adaptive grid ticks** — `render/grid.rs::freq_ticks` generates
  decade×{1,2,5} ticks when the visible span is ≥1 decade and
  1-2-5 nice linear ticks otherwise, so zooming into a sub-decade
  window still shows labels instead of going blank. `time_ticks` does
  the same for the waterfall Y axis.
- **Waterfall time axis** — `CellView::rows_visible` (backed in the
  shader via `WaterfallCellMeta::rows_visible`) lets `Ctrl+Scroll`
  shrink the visible history window; row period is tracked as an EMA
  of inter-frame delta in `App` so axis labels read seconds from now,
  not row indices.
- **JetBrainsMono bundle deferred** — egui's default monospace proved
  legible enough for the timing overlay; the font bundle did not
  land. Left in the deferred bucket for Phase 3 if we grow a larger
  HUD.

---

## Workstreams

### 1. Multi-channel real-daemon mode (unblock everything else)

The current daemon-facing path in `data/receiver.rs` routes every frame
to channel slot 0 because `ac-rs/crates/ac-daemon/src/handlers/audio.rs:352`
emits a single `spectrum` JSON blob with no channel identifier. This is
the one piece of Phase 2 that requires a wire-protocol change; land it
first so the rest of the work can target real data.

**Daemon changes** (`ac-rs/crates/ac-daemon/src/handlers/audio.rs`):

- Add `"channel": <u32>` to the emitted `spectrum` frame. Use the
  existing `cfg.input_channel` if only one channel is being monitored;
  when `monitor_spectrum` grows multi-channel analysis, this becomes
  the actual per-frame index.
- Optionally add `"n_channels": <u32>` so the UI can preallocate slots
  on first frame rather than growing on collision.
- Bump the documented protocol in `ac-rs/ZMQ.md` — the Python client
  (`ac/ui/spectrum.py`) ignores unknown fields, so this is additive and
  does not need a protocol version bump.

**UI changes** (`ac-rs/crates/ac-ui/src/data/`):

- `types.rs::SpectrumFrame` — add `pub channel: Option<u32>` (Option so
  old daemons keep working; None falls back to slot 0 as today).
- `receiver.rs` — grow the `ChannelStore` inputs vector lazily when a
  frame arrives for a slot index we have not yet seen. Alternative: take
  a CLI flag `--max-channels N` and preallocate up front, which keeps
  allocation off the hot path. Pick the preallocation route to avoid a
  reallocation hiccup mid-session.
- `store.rs` — no structural change; `ChannelStore::new(n)` already
  accepts arbitrary N. Confirm peak hold + EMA state stays per-slot.

**App wiring** (`app.rs`):

- Drop the `if args.synthetic { args.channels.max(1) } else { 1 }`
  gate in `main.rs`. Real mode also honours `--channels` now.
- Layout modes (`Grid` / `Overlay` / `Single`) already dispatch on
  `store.len()` and need no change.

**Verification**:

```
# Terminal A — daemon with multi-channel capture
cd ac-rs && cargo build -p ac-daemon && \
  ./target/debug/ac-daemon --local --fake-audio

# Terminal B — drive monitor_spectrum (Python)
ac monitor thd 0dbu 1khz

# Terminal C — UI in multi-channel mode
./ac-rs/target/debug/ac-ui --channels 2
```

Expected: two cells in grid mode, each tracking its own input. Kill
the daemon; both cells' connection dots go red in sync.

### 2. Waterfall spectrogram (design step 10)

Second visualization mode alongside the instantaneous spectrum. Reuses
the same ZMQ frames but accumulates them into a 2D texture that scrolls.

**New module**: `ac-rs/crates/ac-ui/src/render/waterfall.rs`

- Ring buffer of `N` historical frames per channel, stored in a single
  `wgpu::Texture` (R32Float, layered per channel via array texture or
  stacked rows — prefer array texture so multi-channel waterfall works
  with one sampler).
- On each new frame: write one row to the current write index with
  `queue.write_texture`; advance the write head modulo N.
- Vertex shader draws a full-screen quad per cell; fragment shader
  samples the texture with a wraparound offset so the oldest row
  appears at the top and the newest at the bottom.
- Colormap: start with viridis; ship the LUT as a 256×1 RGBA texture
  in `render/shaders/colormap_viridis.rgba` generated at build time
  (tiny `build.rs`, no runtime dep).

**Shader**: `render/shaders/waterfall.wgsl` — `vs_quad` + `fs_sample`.

**Toggle**: add a new `ViewMode` enum to `data/types.rs`
(`Spectrum` / `Waterfall` / `Split`) and cycle with `w`. `Split`
divides each cell horizontally: spectrum on top, waterfall on bottom.

**Parameters** (new CLI + keybinds):

- `--history <n>` default 512 rows (~50 s at 10 Hz update)
- `[` / `]` — adjust colormap dB range (linked to the spectrum
  `db_min`/`db_max` by default; decouple with `Shift+[`/`Shift+]`)

**Risk**: `queue.write_texture` per frame per channel × N channels is
where 100-channel mode could fall over. Measure after step 3 lands.

### 3. GPU timing overlay + 100-channel benchmark (design step 11)

**Timestamp queries**:

- Add a `wgpu::QuerySet` with `QueryType::Timestamp` sized for two
  queries per pass (start/end) × passes (grid, fill, line, waterfall,
  egui). Wrap every `RenderPass` with `write_timestamp`.
- Resolve into a buffer per frame; read back on frame N+2 to avoid
  stalling the pipeline (double-buffer the readback).
- Convert to ms using `queue.get_timestamp_period()`.

**Overlay** (new file `src/ui/timing.rs`):

- Displayed bottom-left when `d` is toggled. Current frame wall time,
  CPU prep ms, GPU total ms, per-pass breakdown, fps from a 60-frame
  rolling window.
- Extend `theme::READOUT_PX` consumer — this is the only place that
  needs a monospace font, so this is a natural time to land the
  deferred JetBrainsMono bundle (see below).

**Benchmark harness** (no new crate; a flag on the existing binary):

```
./target/debug/ac-ui --synthetic --channels 100 --bins 1000 --rate 60 \
  --benchmark 30
```

- `--benchmark <seconds>` runs for the given duration then prints:
  `frames, mean fps, p50/p95/p99 frame time, mean GPU ms, mean CPU ms`
  and exits cleanly. No window manipulation tricks — just a timer
  that fires an `AppExit` request.
- Treat any p99 frame time over 16.6 ms as a regression. Record the
  number from the Intel Iris Xe dev machine as the baseline so future
  changes have a reference.

### 4. Interaction polish

Two items deferred from v0.1 that became obvious once the mouse was
wired up:

- **Crosshair readout on hover**: when the cursor is inside a cell and
  no drag is in progress, draw a thin crosshair in the cell's channel
  color and render `"{freq} Hz  {dBFS}"` near the cursor. Lives in
  `ui/overlay.rs`; reuses `App::cell_at` from v0.1.
- **Per-cell independent view config**: today zoom/pan mutates the
  shared `DisplayConfig`, so all cells move together. Move the
  freq/dB window into a `Vec<CellView>` indexed by channel and have
  the shader read from it via a second storage buffer or extend
  `ChannelMeta` (preferred — it already carries `freq_log_min/max` and
  `db_min/max`, so the shader side is free; only the CPU side needs
  to stop broadcasting). `Ctrl+R` resets all cells, right-click
  resets the hovered cell only.

### 5. Deferred v0.1 leftovers

- **JetBrainsMono TTF bundle** (`ac-rs/crates/ac-ui/assets/fonts/`):
  land as part of step 3 when the timing overlay needs consistent
  monospace widths. Loaded via `include_bytes!` + `egui::FontData`.
- **`ac ui` Python subcommand**: add a dispatcher arm in
  `ac/__main__.py` that resolves the `ac-ui` binary the same way
  `ac-daemon` is resolved today (PATH, then `ac-rs/target/debug/`) and
  execs it with the remaining argv. Keeps the "one command line" story
  intact without forcing Python users to remember a second binary name.
  Document in `ac/__main__.py` that this is an execve, not a subprocess
  — the Python process is replaced, so no auto-spawn state leaks.

---

## Order of operations

1. **Step 1 (multi-channel real)** first — unblocks steps 2 and 3 on
   real data, and the wire change is small enough to land before the
   bigger GPU work.
2. **Step 3 (GPU timing + benchmark)** before step 2 — the waterfall
   is the most likely performance regression, and landing the
   benchmark harness first gives us a before/after number.
3. **Step 2 (waterfall)** — the visible feature, measured against the
   step-3 baseline.
4. **Step 4 (interaction polish)** — can interleave with any of the
   above; low risk, no new dependencies.
5. **Step 5 (deferred leftovers)** — JetBrainsMono folds into step 3;
   `ac ui` Python wrapper lands at the very end once the binary is
   considered daily-driver stable.

---

## Out of scope (still)

- Full replacement of `ac/ui/spectrum.py`. The Python UI stays in
  place as the reference implementation until ac-ui Phase 2 ships
  and we have at least one week of daily-driver use.
- Audio playback / capture from the UI process. ac-ui remains a
  viewer; all measurement control goes through the Python client.
- Non-spectrum visualizations (sweep plots, THD vs level). Those
  belong in a separate binary or a later phase.
- Cross-platform packaging. Linux/Vulkan only for Phase 2; Windows /
  macOS / WebGPU are explicitly out until the renderer is stable.

---

## Critical files

**New**:
- `ac-rs/crates/ac-ui/src/render/waterfall.rs`
- `ac-rs/crates/ac-ui/src/render/shaders/waterfall.wgsl`
- `ac-rs/crates/ac-ui/src/render/shaders/colormap_viridis.rgba` (build-time)
- `ac-rs/crates/ac-ui/src/ui/timing.rs`
- `ac-rs/crates/ac-ui/assets/fonts/JetBrainsMono-Regular.ttf`
- `ac-rs/crates/ac-ui/build.rs` (colormap LUT generation only)

**Edit**:
- `ac-rs/crates/ac-daemon/src/handlers/audio.rs:352` — add `channel` field
- `ac-rs/ZMQ.md` — document the additive field
- `ac-rs/crates/ac-ui/src/data/types.rs` — `channel`, `ViewMode`, `CellView`
- `ac-rs/crates/ac-ui/src/data/receiver.rs` — route by channel
- `ac-rs/crates/ac-ui/src/render/context.rs` — QuerySet + readback buffers
- `ac-rs/crates/ac-ui/src/render/spectrum.rs` — per-cell view in `ChannelMeta`
- `ac-rs/crates/ac-ui/src/app.rs` — `w` / `d` / `Ctrl+R` bindings,
  hover crosshair state, per-cell drag routing
- `ac-rs/crates/ac-ui/src/main.rs` — `--history`, `--benchmark`, help text
- `ac-rs/crates/ac-ui/src/ui/overlay.rs` — hover crosshair
- `ac/__main__.py` — `ac ui` dispatcher arm
- `rust-ui-design.md` — fold Phase 2 results back into the canonical
  design doc once landed

---

## Verification checklist

1. `cargo build -p ac-ui && cargo clippy -p ac-ui -- -D warnings`
2. Multi-channel synthetic still passes (`--synthetic --channels 100`).
3. Multi-channel real: two+ cells tracking independent inputs against a
   daemon built from the same branch.
4. Waterfall scrolls without per-frame CPU vertex rebuild; time visible
   in the timing overlay stays under the baseline.
5. `--benchmark 30` on `--synthetic --channels 100 --bins 1000 --rate 60`
   reports p99 frame time < 16.6 ms on the Intel Iris Xe dev machine.
6. `pytest` stays green — Phase 2 adds one Python edit (`ac/__main__.py`)
   that should be covered by the existing dispatcher tests; extend if
   needed.
7. `ac ui --synthetic` from the Python wrapper opens the same window as
   calling `ac-ui` directly.
