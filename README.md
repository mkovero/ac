# ac — audio measurement

Bench measurement stack for audio: a positional CLI, a ZMQ measurement
daemon, and a keyboard-driven native GUI.

THD, THD+N, level 'n frequency sweeps, transfer functions, live spectrum,
raw-capture snapshots.

The `ip` of audio — terse, positional, unit-tagged arguments.

> **Alpha** — developed and tested on Linux with JACK (native and PipeWire).
> On Linux, CPAL/ALSA fallback is **deliberately disabled**: no JACK means
> the daemon falls back to fake audio and says so, rather than silently
> grabbing a device. macOS and Windows are untested.

## Architecture

The analysis stack is split into two tiers — **reference measurement**
(reproducible, standards-aligned, archivable) and **live analysis**
(responsive, calibration-aware, frame-streamed). Both tiers produce
trustworthy numbers; the split is about which *technique* is in use,
not about numeric rigor. Full model and placement rules:
[`ARCHITECTURE.md`](ARCHITECTURE.md).

Every Tier 1 module carries a `StandardsCitation` audited against the
published text (IEC 60268-3:2018, IEC 61260-1:2014, IEC 61672-1:2013,
AES17-2020, ITU-R BS.468-4, ITU-R BS.1770-5, Farina AES 108 #5093).
All shipped citations are `verified: true`; see the audit table in
`ARCHITECTURE.md`.

The entire stack is Rust:

| Crate | Binary | Role |
|-------|--------|------|
| `ac-cli` | `ac` | CLI client — positional parser, ZMQ REQ/SUB, CSV export, daemon/view auto-spawn |
| `ac-daemon` | `ac-daemon` | ZMQ server — audio I/O (JACK/CPAL/fake), analysis workers, snapshot ring |
| `ac-core` | (library) | Pure DSP — FFT, THD, sweep IR, fractional-octave filterbank, A/C/Z weighting, BS.1770-5 loudness, generator, 3-layer calibration, `.acsnap` read/write, HTML+PDF reports |
| `ac-scene` | (library) | Pure scene layer — traces, axes, ticks, readouts as plain data. Computes every number and string the GUI shows; no GPU dependency |
| `ac-view` | `ac-view` | egui/eframe shell — draws `ac-scene` scenes. Performs an affine viewport map and nothing else: no `log10`, no measurement formatting |

The `ac-scene` / `ac-view` split is load-bearing, not stylistic. Display
truth is computed in a testable pure crate; the renderer is forbidden
from computing measurement values. The former `ac-ui` GPU crate is gone.

## Build

```bash
cd ac-rs
cargo build --release
# Binaries: target/release/{ac, ac-daemon, ac-view}
```

Toolchain is pinned to Rust 1.95.0 via `rust-toolchain.toml`.

`ac monitor` and `ac transfer` launch `ac-view`, which must be on `$PATH`
or in the same directory as `ac`. Note that `install.sh` currently
installs only `ac` and `ac-daemon` — install `ac-view` alongside them if
you want the GUI outside a dev tree.

## Audio backend

`ac-daemon` picks a backend at startup:

| Backend | When used | Platforms |
|---------|-----------|-----------|
| **JACK** (`jack-audio`) | Preferred whenever a JACK server is running | Linux (native or PipeWire) |
| **CPAL** (`cpal-audio`) | Only when JACK is unavailable **and not on Linux** | macOS, Windows |
| **fake** (`--fake-audio`) | Explicit flag, or Linux with no JACK — synthetic sine + 1% H2 | all |

CPAL inherits no-op port routing, so `probe`, `transfer`,
`test hardware` and `test dut` do not behave correctly on it.

Start JACK before measuring:

```bash
jackd -d alsa -d hw:0 -r 48000 -p 1024 -n 2
```

## Quick start

```bash
ac devices                          # list available audio ports
ac setup output 11 input 0          # tell ac which channels to use
ac setup reference 1                # loopback reference leg — required by ac-view
ac setup refout 6                   # reference stimulus leg — same converter as the main output
ac setup temp 24                    # room °C — sets the delay readout's speed of sound
ac calibrate                        # interactive level cal (enables dBu)
ac calibrate spl input 0            # pistonphone SPL cal — readouts in dB SPL
ac calibrate mic-curve mic.frd input 0   # attach mic frequency-response curve
ac plot 20hz 20khz 0dbu 20ppd       # THD vs frequency → CSV
ac s f 20hz 20khz 0dbu              # fast output-only chirp
ac m                                # live spectrum (ac-view window)
ac monitor --tui                    # same measurement, terminal readout
ac transfer                         # transfer view — arm/fire stimulus, H1 + phase
ac monitor cwt                      # live Morlet-CWT waterfall (terminal)
```

`ac setup reference <N>` is not optional if you want the GUI: both
`ac-view` views run on a measurement/reference pair and exit with an
explicit error if the reference channel is unset.

## Reference wiring

**Send the reference out through the same converter as the stimulus, and
loop its analogue output back into an interface input.**

The transfer view locks a delay by correlating the measurement leg against
the reference leg, so what it reports is the *difference* between the two
paths. Anything the acoustic path passes through that the reference does not
survives in that difference and is indistinguishable from distance.

```
              ┌─ stimulus ─→ converter ─→ amp ─→ speaker ─→ air ─→ mic ─┐
   interface ─┤                                                          ├─→ correlate
              └─ reference ─→ same converter ─→ loopback cable ─────────┘
```

Wired this way, everything up to the converter's analogue output is
common-mode and cancels: interface, transport (ADAT/MADI/USB), converter
DAC, sample rate, buffer size. What is left in the delay is only what the
acoustic branch genuinely adds — amplifier, speaker DSP, driver origin,
flight through air, microphone and preamp — so the delay *is* the arrival
time and the metres figure means what it says. This is also how REW, Open
Sound Meter and Smaart expect to be wired.

Take the reference from a *different* converter than the stimulus and the
transport and DAC of the acoustic leg stay in the residual. On the rig
behind #243 — reference out the interface's own DAC, stimulus out over ADAT
through an external converter — that residual was **1.1931 ms**, which the
readout paints as **41 cm of room that is not there**, with a mic taped at
1.000 m reading 1.40 m. Nothing on screen says so: the number is plausible,
stable, and wrong. Only the wiring fixes it.

Deliberately *not* subtracted, under any wiring: **speaker DSP latency
belongs to the device under test, not to the instrument.** Correct wiring
leaves it in the measurement, which is what an operator aligning a system
wants to see. A stored instrument constant would have removed it silently
along with the transport delay.

The metres figure appears only once the pair has a measured lock. Before
that the readout shows milliseconds alone rather than converting the
placeholder `0.00 ms` into a distance; an unlocked pair is named by the
view's `NO LOCK` indicator.

Set the room temperature so the conversion uses the right speed of sound:

```bash
ac setup temp 24          # °C; c = 331.3 + 0.606·T = 345.8 m/s
ac setup temp none        # clear — falls back to 343 m/s (the 20 °C figure)
```

At 24–26 °C the speed of sound is ~346 m/s against the 343 m/s default, a
1 % error — about 25 µs at 1 m, which is 2.4 samples at 96 kHz and therefore
larger than the delay estimate's own resolution.

## Commands

| Command | What it does |
|---------|-------------|
| `devices` | List audio ports |
| `setup` | Configure hardware — device, output, input, reference, refout, dburef, range, temp, dmm, gpio, server-timeout |
| `calibrate` | Voltage cal (sine + DMM); `calibrate spl` adds 94 dB SPL pistonphone reference; `calibrate mic-curve <path>` attaches a mic response correction; `calibrate show` lists stored entries |
| `generate` | Play a sine or pink noise tone |
| `sweep` | Level ramp, frequency chirp, or `sweep ir` (Farina log-sweep impulse response) |
| `plot` | Point-by-point THD vs frequency; `plot level` for THD vs level. Writes CSV to the session directory |
| `transfer` | Launch the `ac-view` transfer view — H1 magnitude, phase, coherence |
| `monitor` | Live spectrum in `ac-view`; `--tui` for the terminal readout. `monitor cwt` / `cqt` / `reassigned` switch the daemon analysis mode and use the terminal readout |
| `test` | Built-in self-tests — `test software`, `test hardware [dmm]`, `test dut [compare] [level]` |
| `report` | Render a `MeasurementReport` JSON to HTML or PDF |
| `probe` | Auto-detect analog ports and loopback pairs (DMM + capture scan) |
| `dmm` | One-off AC Vrms reading from SCPI multimeter |
| `gpio` | GPIO status; `gpio log` streams button events |
| `server` | Enable/disable server, show connections, connect to remote |
| `stop` | Stop active generator/measurement |

Sweeps and plots print rows to stdout and save CSV; there is no plot
image output. `ac plot ... show` only notes that no visual display is
available — the numbers are already in the CSV.

## Views — `ac-view`

Two views, one shell. Every function is reachable by keyboard; there are
no toolbars or menus, and `/` opens the only always-available chrome.
`[`, `]`, `+`, `-` are never bound (Finnish layout).

| Key | Scope | Action |
|-----|-------|--------|
| `/` | both | Toggle help overlay |
| `Q` | both | Quit (drives off on the way out) |
| `S` | both | Trigger snapshot |
| `F` | both | Open local `.acsnap` |
| `←` `→` | both | Move cursor to previous/next column |
| `I` `O` | both | Zoom frequency axis in/out |
| `K` `L` | both | Zoom level axis in/out |
| `A` `D` | both | Pan frequency axis |
| `W` | spectrum | Cycle SPL weighting |
| `T` | spectrum | Cycle SPL integration |
| `V` | spectrum | Toggle reference trace |
| `P` | transfer | Raw (measured) phase vs de-rotated |
| `R` | transfer | Cycle de-rotation reference — session / snapshot / raw |
| `G` | transfer | Settings overlay (channels, start level) |
| `Space` | transfer | Arm stimulus; stop if armed or driving |
| `Enter` | transfer | Fire (start driving); stop if driving |
| `Esc` | transfer | Cancel / stop |
| `↑` `↓` | transfer | Drive level (Shift: 3 dB step) |

Drive only ever starts through the in-app arm→fire machine. Neither
`ac monitor` nor `ac transfer` can launch a view already driving — the
CLI carries no drive argument at all.

The GUI can also be run directly:

```bash
ac-view <host> <ctrl-port> <data-port> [--transfer] [--meas <N>]
# defaults: 127.0.0.1 5556 5557
```

## Snapshots — `.acsnap`

The daemon keeps a rolling ring of **raw pre-processing samples** for
every session channel (30 s by default, `snapshot_ring_s`). A snapshot
freezes that ring into a `.acsnap` — a zip of `meta.json` (full
provenance) plus `audio.flac` (24-bit multichannel).

Snapshots are self-contained by design: reprocessing one needs no daemon,
no audio backend, and no external config. Every calibrated quantity the
live path streams — H1, calibrated spectra, SPL — is re-derivable
offline through the same `ac-core` functions the daemon calls live.
Weighting and integration are session properties, editable after capture.

Format spec: [`ac-rs/SNAPSHOT.md`](ac-rs/SNAPSHOT.md). Wire commands
(`snapshot`, `snapshot_fetch`, `snapshot_list`, `snapshot_delete`):
[`ac-rs/ZMQ.md`](ac-rs/ZMQ.md).

> The format, the daemon side, and offline derivation are complete and
> tested. In `ac-view`, `S` triggers and fetches a snapshot but does not
> yet display it, and `F` is a stub pending the file-picker UX.

## Units

Everything is positional. The suffix tells `ac` what it is:

| Suffix | Meaning | Examples |
|--------|---------|---------|
| `hz` `khz` | Frequency | `20hz` `1khz` `20000hz` |
| `dbu` `dbfs` `vrms` `v` `mvrms` `mv` `vpp` `mvpp` | Level | `0dbu` `-12dbfs` `775mvrms` `1vrms` `2vpp` |
| `s` | Duration / interval | `1s` `0.5s` |
| `ppd` | Points per decade | `10ppd` `20ppd` |
| `step` `steps` | Step count (level plots) | `26steps` |
| `bands` `bpo` | Bands per octave | `3bands` `12bpo` |

A bare number is read as dBFS.

## Abbreviations

Everything has a short form:

```
s|sw(eep)   m|mon(itor)   tr|trans(fer)   g|gen(erate)   c|cal(ibrate)
p|pl(ot)    pr(obe)       te|tst(est)     ser(ver)       st(op)
l|lev(el)   f|freq(uency) si(ne)          pk(ink)        sh(ow)
so|soft(ware)  h|hw(ardware)  du|dut  comp(are)
se|set(up)  d|dev|devs(ices)  o|out(put)  i|in(put)  r|ra(nge)  ref(erence)
n(ew)       ses|sess|ls (sessions)  u(se)  df (diff)
```

## Sessions

Group measurements into named sessions. CSV and report output lands in
the active session's directory.

```bash
ac new myamp        # create + activate
ac sessions         # list all
ac use myamp        # switch
ac rm myamp         # delete
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
ac setup dmm disable         # disable
ac dmm                       # take a one-off reading
ac calibrate                 # uses DMM automatically if configured
```

## Server

`ac` is client/server — the daemon manages audio I/O and runs analysis.
It auto-spawns locally, and restarts itself if the binary is newer than
the running instance. For remote use:

```bash
ac server enable          # bind to all interfaces on a server
ac server 192.168.1.5     # connect to remote server
```

```
ac-daemon [--local] [--fake-audio] [--ctrl-port N] [--data-port N]
```

CTRL is a REP socket on 5556, DATA a PUB socket on 5557. Wire protocol:
[`ac-rs/ZMQ.md`](ac-rs/ZMQ.md).

## Testing

```bash
cd ac-rs && cargo test --workspace   # ~900 tests; 14 are #[ignore]'d
pytest tests/ -q          # black-box ZMQ protocol tests (spawns --fake-audio daemon)
```

Roughly, measured 2026-08-06: `ac-core` 396, `ac-daemon` 214, `ac-scene` 115,
`ac-cli` 97, `ac-view` the remainder. These drift — run the command rather
than citing them. The `#[ignore]`d ones need real hardware (JACK loopback,
stimulus-emitting contiguity checks), a live daemon, a real GPU adapter
for the display-truth snapshot harness, or exist to regenerate fixtures.
See [`TESTING.md`](TESTING.md).

## Dependencies

libzmq, libjack, and the pinned Rust toolchain (1.95.0). `ac-view`
additionally needs a working X11 or Wayland session.
