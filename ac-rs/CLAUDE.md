# ac-rs — Rust audio measurement system

Full Rust `ac` stack: measurement library, ZMQ daemon, CLI client, native egui view over pure scene layer. (Old GPU UI `ac-ui` deprecated + detached — see `attic/ac-ui`. `ac-view` is replacement, different crate.)

New analysis feature: **pick tier first** — Tier 1 (reference measurement, `ac-core/src/measurement/`) vs Tier 2 (live analysis, `ac-core/src/visualize/`). See `../ARCHITECTURE.md`.

## Build

```bash
cargo build                       # all crates
cargo build --release             # optimized
cargo test --workspace            # ~900 tests, 14 #[ignore]'d
```

Count = order of magnitude, not check target. Measured 908 passed / 14 ignored on 2026-08-10 (`main` at `06e8ccc`). Need number? Run command. Don't cite this line — and don't read it through a pipe, see TESTING.md.

**`cargo test -p <crate>` not enough before merge.** Two branches merge clean, still break build together: `it_banner_clearance.rs` (#252) did not compile against `TransferInput` fields added by #243 branch, both landed on `main`. No CI here — `--workspace` is check.

## Crate layout

| Crate | Binary | Role |
|-------|--------|------|
| `ac-core` | — | Pure library — Tier 1 (`measurement/*`): IEC 61260-1 filterbank, IEC 61672-1 A/C/Z weighting, AES17 idle-channel noise, IEC 60268-3 THD, ITU-R BS.468-4 CCIR weighting, BS.1770-5 / EBU R128 loudness, Farina log-sweep IR, HTML + PDF report renderers. Tier 2 (`visualize/*`): live FFT spectrum, Morlet CWT, constant-Q transform, Auger-Flandrin reassigned STFT, fractional-octave aggregator, time integration. Plus `shared/`: 3-layer calibration (voltage / SPL / mic-curve), conversions, generator, config. |
| `ac-cli` | `ac` | CLI client — positional parser, ZMQ REQ/SUB, CSV export, daemon auto-spawn. |
| `ac-daemon` | `ac-daemon` | ZMQ REP+PUB server. Audio I/O (JACK/CPAL/fake), worker management. Thin shell over `ac-core`. Has `#[ignore]`'d JACK-loopback runbook (`tests/it_loopback_ir.rs`). |
| `ac-scene` | — | Pure scene/data layer for views. Turns `transfer_stream` v2 wire frame or snapshot `PairDerivation` into trace geometry, axis ticks, readout strings — plain data, zero rendering. No egui, no wgpu, no ZMQ — enforced by dependency list, not convention. |
| `ac-view` | `ac-view` | Keyboard-driven egui/eframe shell. Draws `ac-scene` scenes, does **no numeric computation of its own** — that split is display-truth boundary. Snapshot tests via `egui_kittest`. |

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
| `jack_backend.rs` | Default (JACK must run). **Required on Linux** — see issue #27. |
| `cpal_backend.rs` | macOS/Windows fallback when no JACK. Disabled on Linux at runtime (`#[cfg(not(target_os = "linux"))]` in `make_engine`). |
| `fake.rs` | `--fake-audio` flag; returns clean sine so `analyze()` gets plausible output. Also Linux fallback when JACK not running, so missing-JACK fails loud instead of silently grabbing ALSA. |

## Server loop (`ac-daemon/src/server.rs`)

Single-threaded ZMQ REP/PUB loop. Workers run in `std::thread::spawn`. Main loop drains `pub_tx` channel (worker → PUB socket), reaps finished workers via `JoinHandle::is_finished()` every 10 ms poll.

## Handlers (`ac-daemon/src/handlers/`)

One function per command. Each audio command (`generate`, `plot`, etc.) checks busy guard (`check_busy`), spawns `WorkerHandle`, inserts into shared `workers` map, returns CTRL reply immediately.

## Protocol reference

See `ac-rs/ZMQ.md` — authoritative for both Python and Rust.

## Backend status

| Path | State |
|------|-------|
| `calibrate` | Full state machine: emits `cal_prompt`, blocks on `cal_reply`, writes cal.json via `Calibration::save()` |
| `dmm_read` | SCPI client wired (only used when `[dmm]` section configured; else `no DMM configured`) |
| GPIO handler | USB2GPIO (Arduino Mega) handler in `gpio.rs`, spawned by `--gpio <port>` |
| CPAL backend | Runs when no JACK. **Note:** CPAL backend inherits `AudioEngine` default no-op routing methods — commands needing port routing (`probe`, `transfer`, `test_hardware`, `test_dut`) behave wrong now. See issue #27. |
| `--fake-audio` | Synthetic sine loopback; bypasses routing (see issue #34) |

## Known limitations

- JACK process callback not real-time safe today (Mutex + alloc every period). See issue #23 — fix in flight via `ringbuf` SPSC + atomic tone swap.
- `xruns()` counter always 0 on JACK and CPAL (issue #24).
- Capture rings grow unbounded on long output-only commands (issue #25).
- ~~`handlers.rs` is 1931 LOC; slated for split (#29).~~ Done — now `handlers/`, one module per concern (`transfer`, `snapshot`, `calibrate`, `test_hw`, `test_dut`, `test_software`, `mic`, `admin`, `mod`).

Full backlog: <https://github.com/mkovero/ac/issues?q=is%3Aopen+label%3Abacklog>.

Note: detached `ac-ui`'s keybinding daemon toggles (mic correction, per-band weighting, time integration, Leq/loudness reset, fractional-octave smoothing) had no ac-cli equivalent at detach time. Whether ac-cli needs flags = B1 command-matrix question (handoff.md), unresolved here — and `ac-view` took over keyboard-driven role, so check what it already exposes before calling this open gap.