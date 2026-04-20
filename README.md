# ac — audio measurement CLI

Command-line toolkit for audio bench measurements.

THD, THD+N, level 'n frequency sweeps, transfer functions, live spectrum.

The `ip` of audio — terse, positional, unit-tagged arguments.

> **Alpha** — only tested on Linux with JACK (native and PipeWire).
> sounddevice/PortAudio backend exists but is not exercised yet.
> macOS and Windows are untested.

## Architecture

The entire stack is implemented in Rust:

| Crate | Binary | Role |
|-------|--------|------|
| `ac-cli` | `ac` | CLI client — parser, ZMQ client, CSV export, UI launch |
| `ac-daemon` | `ac-daemon` | ZMQ server — audio I/O, analysis, worker management |
| `ac-ui` | `ac-ui` | GPU UI — wgpu spectrum/waterfall/transfer/sweep views |
| `ac-core` | (library) | Pure DSP — FFT, THD, generator, calibration, config |

## Install

```bash
cd ac-rs && cargo build --release
# Binaries: target/release/ac, target/release/ac-daemon, target/release/ac-ui
```

## Audio backend

`ac` auto-detects the audio backend at startup:

| Backend | When used | Platforms |
|---------|-----------|-----------|
| **JACK** (`jack-client`) | Preferred when a JACK server is running | Linux (native or PipeWire) |
| **sounddevice** (PortAudio) | Fallback when JACK is unavailable | Linux, macOS, Windows |

To force a backend, set `"backend"` in `~/.config/ac/config.json`:

```json
{ "backend": "sounddevice" }
```

When using JACK, the server must be running before any measurement:

```bash
jackd -d alsa -d hw:0 -r 48000 -p 1024 -n 2
```

## Quick start

```bash
ac devices                          # list available audio ports
ac setup output 11 input 0          # tell ac which channels to use
ac calibrate                        # interactive level cal (enables dBu)
ac plot 20hz 20khz 0dbu 20ppd show  # measure THD vs frequency, open plot
ac s f 20hz 20khz 0dbu              # fast output-only chirp
ac m sh                             # live spectrum, GPU UI window
```

## Commands

| Command | What it does |
|---------|-------------|
| `devices` | List audio ports |
| `setup` | Configure hardware — output, input, reference, range, dmm, gpio |
| `calibrate` | Interface calibration |
| `generate` | Play a sine or pink noise tone |
| `sweep` | Level ramp or frequency chirp |
| `plot` | Point-by-point THD measurement with PNG output |
| `transfer` | H1 transfer function (magnitude, phase, coherence) |
| `monitor` | Live spectrum with TUI bar chart |
| `probe` | Auto-detect analog ports and loopback pairs (DMM + capture scan) |
| `dmm` | One-off AC Vrms reading from SCPI multimeter |
| `server` | Enable/disable server, show connections, connect to remote |
| `stop` | Stop active generator/measurement |

## Units

Everything is positional. The suffix tells `ac` what it is:

| Suffix | Meaning | Examples |
|--------|---------|---------|
| `hz` `khz` | Frequency | `20hz` `1khz` `20000hz` |
| `dbu` `dbfs` `vrms` `mvrms` `vpp` | Level | `0dbu` `-12dbfs` `775mvrms` `1vrms` `2vpp` |
| `s` | Duration / interval | `1s` `0.5s` |
| `ppd` | Points per decade | `10ppd` `20ppd` |

Append `show` to any command to open a live view (`ac-ui`).

## Abbreviations

Everything has a short form:

```
s(weep)  m(onitor)  g(enerate)  c(alibrate)  p(lot)  tf/tr(ansfer)  pr(obe)
l(evel)  f(requency)  si(ne)  pk(ink)  sh(ow)
se(tup)  d(evices)  st(op)  ref(erence)
```

## Sessions

Group measurements into named sessions:

```bash
ac new myamp        # create + activate
ac sessions         # list all
ac use myamp        # switch
ac diff amp1 amp2   # compare
```

## GPIO — physical button control

Optional hardware interface for hands-free operation. A [usb2gpio](https://github.com/mkovero/usb2gpio) board (Arduino Mega2560) connects via USB serial and provides physical buttons for starting/stopping tone generation, with LED feedback for active state.

Buttons trigger ZMQ commands to the server — press SINE to generate a 1 kHz tone at the calibrated level, press STOP to silence it. LEDs reflect what's playing.

```bash
ac setup gpio /dev/ttyUSB0   # enable
ac setup gpio none           # disable
ac gpio                      # show status
ac gpio log                  # stream button events
```

The server auto-starts the GPIO handler on launch if `gpio_port` is configured.

## DMM — automated meter readings

Optional SCPI integration for reading a bench multimeter (e.g. Keysight 34461A) over TCP. During calibration, `ac` queries the DMM for AC Vrms readings instead of requiring manual entry — it connects to port 5025, sends `MEAS:VOLT:AC?`, and averages three readings.

The DMM value is presented as a suggestion; you can accept it or type an override.

```bash
ac setup dmm 192.168.1.100   # enable (IP or hostname of meter)
ac setup dmm disable          # disable
ac dmm                        # take a one-off reading
ac calibrate                  # uses DMM automatically if configured
```

## Server

`ac` is client/server — the daemon manages audio I/O and runs analysis.
It auto-spawns locally. For remote use:

```bash
ac server enable          # bind to all interfaces on a server
ac server 192.168.1.5     # connect to remote server
```

The daemon is `ac-daemon` (Rust). The Rust CLI auto-discovers it in
`$PATH` or `ac-rs/target/debug/ac-daemon`. See `ac-rs/ZMQ.md` for the wire
protocol.

## Build

```bash
cd ac-rs
cargo build                   # all crates (ac, ac-daemon, ac-ui)
cargo test                    # 227 tests (ac-core 43, ac-cli 50, ac-daemon 43 + 10 it, ac-ui 81)
```

## Dependencies

Rust: libzmq, libjack, Rust toolchain (≥ 1.75).
