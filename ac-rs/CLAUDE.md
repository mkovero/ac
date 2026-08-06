# ac-rs — Rust audio measurement system

Full Rust implementation of the `ac` stack: measurement library, ZMQ daemon,
CLI client, and a native egui view over a pure scene layer. (The older GPU UI,
`ac-ui`, was deprecated and detached — see `attic/ac-ui`. `ac-view` is its
replacement and is not the same crate.)

When adding a new analysis feature, **first decide its tier** — Tier 1 (reference measurement, `ac-core/src/measurement/`) vs Tier 2 (live analysis, `ac-core/src/visualize/`). See `../ARCHITECTURE.md`.

## Build

```bash
cargo build                       # all crates
cargo build --release             # optimized
cargo test --workspace            # ~900 tests, 14 #[ignore]'d
```

The count above is an order of magnitude, not a figure to check against.
Measured at 899 passed / 14 ignored on 2026-08-06 (`main`, first commit at
which `cargo test --workspace` compiled again — see the note below). If you
need the number, run the command; do not cite this line.

**`cargo test -p <crate>` is not enough before merging.** Two branches that
merge cleanly can still break the build together: `it_banner_clearance.rs`
(from #252) did not compile against the `TransferInput` fields added by #243's
branch, and both landed on `main`. There is no CI here, so `--workspace` is
the check.

## Crate layout

| Crate | Binary | Role |
|-------|--------|------|
| `ac-core` | — | Pure library — Tier 1 (`measurement/*`): IEC 61260-1 filterbank, IEC 61672-1 A/C/Z weighting, AES17 idle-channel noise, IEC 60268-3 THD, ITU-R BS.468-4 CCIR weighting, BS.1770-5 / EBU R128 loudness, Farina log-sweep IR, HTML + PDF report renderers. Tier 2 (`visualize/*`): live FFT spectrum, Morlet CWT, constant-Q transform, Auger-Flandrin reassigned STFT, fractional-octave aggregator, time integration. Plus `shared/`: 3-layer calibration (voltage / SPL / mic-curve), conversions, generator, config. |
| `ac-cli` | `ac` | CLI client — positional parser, ZMQ REQ/SUB, CSV export, daemon auto-spawn. |
| `ac-daemon` | `ac-daemon` | ZMQ REP+PUB server. Audio I/O (JACK/CPAL/fake), worker management. Thin shell over `ac-core`. Includes an `#[ignore]`'d JACK-loopback runbook (`tests/it_loopback_ir.rs`). |
| `ac-scene` | — | Pure scene/data layer for the views. Turns a `transfer_stream` v2 wire frame or a snapshot `PairDerivation` into trace geometry, axis ticks and readout strings — plain data, zero rendering. No egui, no wgpu, no ZMQ, enforced by its dependency list rather than by convention. |
| `ac-view` | `ac-view` | Keyboard-driven egui/eframe shell. Draws `ac-scene` scenes and does **no numeric computation of its own** — that split is the display-truth boundary. Snapshot tests via `egui_kittest`. |

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
| `jack_backend.rs` | Default (JACK must be running). **Required on Linux** — see issue #27. |
| `cpal_backend.rs` | macOS/Windows fallback when JACK is unavailable. Disabled on Linux at runtime (`#[cfg(not(target_os = "linux"))]` in `make_engine`). |
| `fake.rs` | `--fake-audio` flag; returns clean sine so `analyze()` gets plausible output. Also the Linux fallback when JACK isn't running, so missing-JACK fails loudly instead of silently grabbing ALSA. |

## Server loop (`ac-daemon/src/server.rs`)

Single-threaded ZMQ REP/PUB loop. Workers run in `std::thread::spawn`.
Main loop drains the `pub_tx` channel (worker → PUB socket) and reaps finished
workers via `JoinHandle::is_finished()` every 10 ms poll interval.

## Handlers (`ac-daemon/src/handlers/`)

One function per command. Each audio command (`generate`, `plot`, etc.) checks
the busy guard (`check_busy`), spawns a `WorkerHandle`, inserts it into the
shared `workers` map, and returns the CTRL reply immediately.

## Protocol reference

See `ac-rs/ZMQ.md` — authoritative for both Python and Rust implementations.

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
- ~~`handlers.rs` is 1931 LOC; slated for split (#29).~~ Done — it is now
  `handlers/` with one module per concern (`transfer`, `snapshot`, `calibrate`,
  `test_hw`, `test_dut`, `test_software`, `mic`, `admin`, `mod`).

For the full backlog see <https://github.com/mkovero/ac/issues?q=is%3Aopen+label%3Abacklog>.

Note: the detached `ac-ui`'s keybinding-driven daemon toggles (mic correction,
per-band weighting, time integration, Leq/loudness reset, fractional-octave
smoothing) had no ac-cli equivalent at the time of the detach. Whether ac-cli
needs flags for these is a B1 command-matrix question (handoff.md), not
resolved here — and `ac-view` has since taken over the keyboard-driven role,
so check what it already exposes before treating this as an open gap.
