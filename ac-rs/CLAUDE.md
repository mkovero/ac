# ac-rs — Rust audio measurement system

Full Rust implementation of the `ac` stack: CLI client and ZMQ daemon. (The
former GPU UI, `ac-ui`, was deprecated and detached — see `attic/ac-ui`.)

When adding a new analysis feature, **first decide its tier** — Tier 1 (reference measurement, `ac-core/src/measurement/`) vs Tier 2 (live analysis, `ac-core/src/visualize/`). See `../ARCHITECTURE.md`.

## Build

```bash
cargo build            # all crates
cargo test             # 1 #[ignore]'d (JACK loopback runbook) — run it deliberately
```

## Audio backends (`ac-daemon/src/audio/`)

| File | When used |
|------|-----------|
| `jack_backend.rs` | Default (JACK must be running). **Required on Linux** — see issue #27. |
| `cpal_backend.rs` | macOS/Windows fallback when JACK is unavailable. Disabled on Linux at runtime (`#[cfg(not(target_os = "linux"))]` in `make_engine`). |
| `fake.rs` | `--fake-audio` flag; returns clean sine so `analyze()` gets plausible output. Also the Linux fallback when JACK isn't running, so missing-JACK fails loudly instead of silently grabbing ALSA. |

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

For the full backlog see <https://github.com/mkovero/ac/issues?q=is%3Aopen+label%3Abacklog>.

Note: the GPU UI's keybinding-driven daemon toggles (mic correction, per-band
weighting, time integration, Leq/loudness reset, fractional-octave smoothing)
had no ac-cli equivalent as of the ac-ui detach. Whether ac-cli needs flags
for these is a B1 command-matrix question (handoff.md), not resolved here.
