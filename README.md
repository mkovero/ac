# ac — audio measurement CLI

Command-line toolkit for audio bench measurements.

THD, THD+N, level 'n frequency sweeps, live spectrum.

The `ip` of audio — terse, positional, unit-tagged arguments.

## Install

```bash
pip install -e .
```

This gives you the `ac` command.

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
ac m sh                             # live spectrum, pyqtgraph window
```

## Commands

| Command | What it does |
|---------|-------------|
| `devices` | List audio ports |
| `setup` | Configure hardware — output, input, range, dmm (SCPI), gpio |
| `calibrate` | Interface calibration |
| `generate` | Play a sine or pink noise tone |
| `sweep` | Ramp or frequency chirp |
| `plot` | Point-by-point THD measurement |
| `monitor` | Live spectrum |
| `stop` | Stop activities |

## Units

Everything is positional. The suffix tells `ac` what it is:

| Suffix | Meaning | Examples |
|--------|---------|---------|
| `hz` `khz` | Frequency | `20hz` `1khz` `20000hz` |
| `dbu` `dbfs` `vrms` `mvrms` `vpp` | Level | `0dbu` `-12dbfs` `775mvrms` `1vrms` `2vpp` |
| `s` | Duration / interval | `1s` `0.5s` |
| `ppd` | Points per decade | `10ppd` `20ppd` |

Append `show` to any command to open a pyqtgraph window.

## Abbreviations

Everything has a short form:

```
s(weep)  m(onitor)  g(enerate)  c(alibrate)  p(lot)
l(evel)  f(requency)  si(ne)  pk(ink)  sh(ow)
se(tup)  d(evices)  st(op)
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

`ac` is client/server — the server manages audio I/O and runs analysis.
It auto-spawns locally. For remote use:

```bash
ac server enable          # bind to all interfaces on a server
ac server 192.168.1.5     # connect to remote server
```

## Dependencies

numpy, scipy, matplotlib, sounddevice, pyzmq.
Optional: `jack-client` (for JACK backend), `pyqtgraph` (for live GUI views).
