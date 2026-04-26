# ac-rs — Rust audio measurement system

Full Rust implementation of the `ac` stack: CLI client, ZMQ daemon, and GPU UI.

When adding a new analysis feature, **first decide its tier** — Tier 1 (reference measurement, `ac-core/src/measurement/`) vs Tier 2 (live analysis, `ac-core/src/visualize/`). See `../ARCHITECTURE.md`.

## Build

```bash
cargo build                       # all crates
cargo build --release             # optimized
cargo test                        # 330 tests (ac-core 119, ac-cli 55, ac-daemon 43 + 11 it, ac-ui 102)
```

## Crate layout

| Crate | Binary | Role |
|-------|--------|------|
| `ac-core` | — | Pure library — analysis, CWT, generator, calibration, config, conversions, IEC 61260-1 filterbank, Farina log-sweep IR, IEC 61672-1 A/C/Z weighting, AES17 idle-channel noise, HTML + PDF report renderers. No sockets, no global state. 119 unit tests. |
| `ac-cli` | `ac` | CLI client — positional parser, ZMQ REQ/SUB, CSV export, daemon/UI auto-spawn. 55 parser tests. |
| `ac-daemon` | `ac-daemon` | ZMQ REP+PUB server. Audio I/O (JACK/CPAL/fake), worker management. Thin shell over `ac-core`. 43 unit + 10 integration tests. |
| `ac-ui` | `ac-ui` | GPU UI — wgpu spectrum/waterfall/CWT, egui transfer/sweep views. Connects via ZMQ SUB + REQ. 102 tests. |

## ac-daemon binary

```
ac-daemon [--local] [--fake-audio] [--ctrl-port N] [--data-port N]
```

| Flag | Default | Effect |
|------|---------|--------|
| `--local` | off | Bind to `127.0.0.1` only (auto-spawned with this flag) |
| `--fake-audio` | off | Use synthetic sine loopback instead of JACK |
| `--ctrl-port N` | 5556 | ZMQ REP port |
| `--data-port N` | 5557 | ZMQ PUB port |

## Audio backends (`ac-daemon/src/audio/`)

| File | When used |
|------|-----------|
| `jack_backend.rs` | Default (JACK must be running) |
| `fake.rs` | `--fake-audio` flag; returns clean sine so `analyze()` gets plausible output |

## Server loop (`ac-daemon/src/server.rs`)

Single-threaded ZMQ REP/PUB loop. Workers run in `std::thread::spawn`.
Main loop drains the `pub_tx` channel (worker → PUB socket) and reaps finished
workers via `JoinHandle::is_finished()` every 10 ms poll interval.

## Handlers (`ac-daemon/src/handlers.rs`)

One function per command. Each audio command (`generate`, `plot`, etc.) checks
the busy guard (`check_busy`), spawns a `WorkerHandle`, inserts it into the
shared `workers` map, and returns the CTRL reply immediately.

## Protocol reference

See `ZMQ.md` — authoritative for both Python and Rust implementations.

## Backend status

| Path | State |
|------|-------|
| `calibrate` | Full state machine: emits `cal_prompt`, blocks on `cal_reply`, writes cal.json via `Calibration::save()` |
| `dmm_read` | SCPI client wired (only used when `[dmm]` section is configured; otherwise `no DMM configured`) |
| GPIO handler | USB2GPIO (Arduino Mega) handler in `gpio.rs`, spawned by `--gpio <port>` |
| CPAL backend | Runs when JACK unavailable. **Note:** CPAL backend inherits the `AudioEngine` default no-op routing methods — commands that rely on port routing (`probe`, `transfer`, `test_hardware`, `test_dut`) currently behave incorrectly. See issue #27. |
| `--fake-audio` | Synthetic sine loopback; bypasses routing (see issue #34) |

## Known limitations

- JACK process callback is not real-time safe today (Mutex + alloc on every
  period). See issue #23 — fix in flight via `ringbuf` SPSC + atomic tone swap.
- `xruns()` counter is always 0 on both JACK and CPAL (issue #24).
- Capture rings grow unbounded on long output-only commands (issue #25).
- `handlers.rs` is 1931 LOC; slated for split into per-concern modules (#29).

## ac-ui keybindings

| Key | Action |
|-----|--------|
| `W` | Cycle view: matrix (grid+spectrum) → single → waterfall (FFT) → waterfall (CWT) → waterfall (CQT) → waterfall (reassigned) |
| `C` | Compare selected channels in one rect (needs ≥ 1 Space-selected channel) |
| Left click (Matrix) | Zoom in: swap to Single layout on the clicked channel |
| `F` | Toggle fullscreen |
| `D` | Toggle timing overlay |
| `Tab` / `Shift+Tab` | Next/prev channel or grid page |
| `Space` | Toggle channel selection |
| `[` / `]` | Shift dB floor ±5 |
| `+` / `-` | Adjust dB span |
| `Left` / `Right` | FFT monitor tick interval ±1 ms (clamped 1–1000 ms, FFT mode only) |
| `Up` / `Down` | FFT monitor N (1024 … 65536, FFT mode only) |
| `Ctrl+R` | Reset all views |
| `S` | Screenshot |
| `P` | Toggle peak hold (Spectrum view) — top-5 local maxima ranked by dB with ≥1/3-octave spacing, auto-decays at 20 dB/s after 1 s idle |
| `M` | Toggle min hold (Spectrum view) — per-bin rolling minimum, same decay as peak |
| `Shift+M` | Toggle daemon-side mic frequency-response correction. Per-channel curves are loaded via `ac calibrate mic-curve` and applied in the daemon before frame emission; the toggle gates application without unloading the curve. Top-right shows `mic-cal` (on) or `mic-cal: off` (loaded but disabled); bottom-left appends `[mic-corrected]` to the readout. |
| `O` | Cycle fractional-octave smoothing: off → 1/24 → 1/12 → 1/6 → 1/3 (default: 1/6; applies to spectrum, waterfall, and transfer |H(f)|; state shown top-right) |
| `Shift+O` | Cycle fractional-octave CWT aggregation (CWT mode only): off → 1/1 → 1/3 → 1/6 → 1/12 → 1/24 → off. Replaces the displayed CWT column with summed-power per band; preserves single-tone dBFS at synthetic isolated scales (kernel-overlap drift on real signals — see `ac-core::fractional_octave`); state shown top-right |
| `A` | Cycle per-band frequency weighting: off → A → C → Z → off. Daemon adds the IEC 61672-1 Annex E dB offset at each band centre before emitting `fractional_octave` / `fractional_octave_leq`. Display-only — same Morlet-CWT caveat. |
| `I` | Cycle per-band time integration: off → fast (τ=125 ms) → slow (τ=1 s) → Leq → off. Daemon emits a `fractional_octave_leq` sidecar frame when non-off. Display-only — not IEC 61672 SPL (upstream aggregation is Morlet CWT, not IEC 61260 filters). |
| `Shift+I` | Reset Leq accumulators on the daemon. Fast/slow modes ignore it — they re-prime from their next input. |
| `Shift+L` | Reset BS.1770-5 loudness accumulators (integrated LKFS, LRA, true-peak) for every monitored channel. |
| `Shift+Up/Down` | CWT sigma ±1 (5–24, only in CWT mode) |
| `Shift+Left/Right` | CWT scales ×2/÷2 (64–8192, only in CWT mode) |
| Scroll | Zoom freq/dB/time axis (context-dependent) |
| `Shift+Scroll` | Cycle waterfall palette (inferno → magma, Waterfall only) |
| `Ctrl+Shift+Scroll` | Pan dB window ±2 dB/tick — analog gain trim (floor and ceiling shift together) |
| Drag | Pan freq/dB axes |
| Right-drag | Box zoom — rubber-band a freq/dB (spectrum) or freq/time (waterfall/CWT) region to fill the cell |
| Right-click | Reset hovered cell view (no drag; drag triggers box zoom) |

For the full backlog see <https://github.com/mkovero/ac/issues?q=is%3Aopen+label%3Abacklog>.
