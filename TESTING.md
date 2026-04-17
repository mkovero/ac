# Testing

Run all tests:
```bash
cd ac-rs && cargo test            # Rust: ac-core (43) + ac-cli (50) = 93 tests
python -m pytest tests/ -q        # Python: 149 integration tests
```

No JACK daemon or audio hardware required — Python tests spawn the Rust `ac-daemon --fake-audio` (synthetic sine + 1% 2nd harmonic) on free ports and connect via ZMQ.

## Rust tests

```bash
cd ac-rs
cargo test -p ac-core             # 43 unit tests — analysis, generator, calibration, config, conversions
cargo test -p ac-cli              # 50 parser tests — all commands, abbreviations, defaults, error cases
cargo test                        # all crates
```

## Build

```bash
cd ac-rs
cargo build                       # all crates: ac (CLI), ac-daemon, ac-ui
cargo build --release             # optimized
```

Both the Rust CLI and Python client auto-discover the debug build at `ac-rs/target/debug/ac-daemon`. For production installs:

```bash
cargo build --release
sudo install -m 755 target/release/ac target/release/ac-daemon target/release/ac-ui /usr/local/bin/
```

Use `--fake-audio` to run the daemon without JACK (for integration testing):

```bash
ac-daemon --local --fake-audio
```

## Built-in self-tests

In addition to pytest, `ac` has built-in self-tests runnable without pytest:

```bash
ac test software              # validates analysis pipeline + conversions (no hardware)
ac test hardware              # hardware validation (requires 2 loopback pairs)
ac test hardware dmm          # + cross-check against DMM over SCPI
ac test dut                   # DUT characterization (requires 2 loopback pairs)
ac test dut compare           # A/B comparison (prompts to bypass DUT)
ac test dut -10dbu            # DUT test at specific level
```

Short forms: `ac te so`, `ac te h`, `ac te h dmm`, `ac te du`, `ac te du comp`

## Test files

| File | Tests | What it covers |
|------|-------|----------------|
| `test_analysis.py` | 28 | FFT analysis: THD, THD+N, harmonics, noise floor, fundamental detection, spectrum downsampling |
| `test_parse.py` | 58 | CLI token parser: all commands incl. test/dut, abbreviations, defaults, error cases |
| `test_server_client.py` | 21 | ZMQ integration: command dispatch, sweep/plot/monitor/generate workers, busy guard, stop, software self-tests |
| `test_calibration.py` | 14 | Calibration class: save/load, vrms conversions, uncalibrated None handling |
| `test_conversions.py` | 11 | Unit conversions: dBu/Vrms/dBFS/Vpp, known audio standards |

### Rust unit tests

#### ac-core (43 tests)

| Module | Tests | What it covers |
|--------|-------|----------------|
| `analysis` | 16 | FFT analysis port: THD, THD+N, harmonics, fundamental detection, noise floor, clipping, ac_coupled |
| `generator` | 4 | Sine RMS, phase start, dBFS→amplitude, pink noise length and crest factor |
| `calibration` | 5 | Save/load roundtrip, missing key, load_all, out_vrms computation |
| `config` | 2 | JSON round-trip, missing keys use defaults |
| `conversions` | 2 | dBu↔Vrms, format helpers |
| `cwt` | 6 | Morlet CWT: log-spaced freqs, magnitude peaks, energy conservation |
| `transfer` | 4 | H1 estimator: coherence, delay, magnitude/phase from known signals |
| `gpio` | 4 | Frame parser: button events, LED commands, checksum validation |

#### ac-cli (50 tests)

| Module | Tests | What it covers |
|--------|-------|----------------|
| `parse` | 50 | All commands: sweep, plot, monitor, generate, calibrate, setup, devices, transfer, test, probe, session, stop, server, gpio, dmm, config. Abbreviations, defaults, error cases. |

## What is verified numerically

### THD accuracy (test_analysis.py)

These tests generate synthetic signals with mathematically known distortion and verify the analyzer returns correct values:

- **1% 2nd harmonic** → THD = 1.000% (±0.05%)
- **1% H2 + 0.5% H3** → THD = sqrt(1² + 0.5²) = 1.118% (±0.05%)
- **0.01% 2nd harmonic** → THD = 0.010% (±0.005%)
- **Three equal 1% harmonics** → THD = sqrt(3) ≈ 1.732% (±0.1%)
- **Pure sine** → THD < 0.01%
- **THD+N ≥ THD** always (physical law)
- **THD+N within 0.5x–10x of THD** (guards against np.mean vs np.sum bugs)

### THD across the audio band

- THD measured at 100, 440, 1000, 5000, 10000 Hz — all within ±0.1% of expected
- THD measured at amplitudes 0.01, 0.1, 0.5, 0.9 — level-independent (±0.1%)

### Fundamental & RMS

- **fundamental_dbfs** scales correctly: 10x amplitude = 20 dB, 5x = 13.98 dB
- **linear_rms** = amplitude / sqrt(2) for pure sine (±1% relative)
- **Harmonic amplitudes** (H2/H3 ratios vs fundamental) match injected values (±10% relative)

### Noise floor

- Injecting broadband noise raises the measured noise floor proportionally
- Clean sine noise floor is lower than noisy sine noise floor

### Unit conversions (test_conversions.py)

- 0 dBu = 0.77459667 Vrms (standard definition)
- +4 dBu = 1.228 Vrms (pro audio reference)
- +20 dBu = 7.746 Vrms
- Vrms ↔ dBu roundtrip within 1e-9
- dBFS → Vrms: -20 dBFS with ref 1.0 = 0.1 Vrms
- Full chain: dBFS + calibration ref → Vrms → dBu (verified against manual calculation)
- Vpp = Vrms × 2√2

### Calibration (test_calibration.py)

- `out_vrms(-20 dBFS)` with cal 0.245 → 0.0245 Vrms
- `in_vrms(linear_rms)` = linear_rms × vrms_at_0dbfs_in
- Uncalibrated → returns None (not NaN, not crash)
- Save/load roundtrip preserves values to 1e-9

### Integration: end-to-end THD (test_server_client.py)

The Rust fake audio engine generates amplitude 0.1 with 1% 2nd harmonic. Through the full pipeline (fake engine → analyze → sweep_point_frame → ZMQ → client):

- **THD ≈ 1.0%** (0.8–1.3% tolerance for transport/rounding)
- **fundamental_dbfs ≈ -20 dBFS** (±2 dB)
- **THD+N ≥ THD** verified through the full stack
- **plot_level** produces correct step count and cmd field

### None vs NaN safety (test_server_client.py)

Without calibration, `gain_db`, `out_dbu`, `in_dbu` are `None` in sweep_point frames. Tests verify:
- These fields are indeed `None` (not missing, not NaN)
- The correct pattern (`p["gain_db"] if p.get("gain_db") is not None else np.nan`) produces `float64` arrays
- The buggy pattern (`.get("gain_db", np.nan)`) produces `object` arrays — confirming why the gain line vanished

## Known limitations

### Spectrum downsampling (display only)

`_downsample_spectrum()` uses geomspace point-sampling to reduce ~24000 FFT bins to ~1000 for UI display. Narrow peaks at exact FFT bin frequencies can fall between sampled indices and appear as zero. This does NOT affect measurement values (THD, harmonics, noise floor are computed from the full spectrum). Tested in `test_downsample_structure` and `test_downsample_short_spectrum_passthrough`.

### Noise floor algorithm

The time-domain subtraction method (subtract reconstructed sines from waveform) has a measurement floor of approximately -38 dBFS for a clean synthetic sine due to windowing artifacts. Real-world signals with broadband noise are measured correctly relative to each other.

### Fake audio engine (`--fake-audio`)

Tests use the Rust `FakeEngine` which produces synthetic float32 sine waves, not real audio. It does not simulate:
- Actual latency or jitter
- ADC/DAC nonlinearity
- Real noise floors
- Sample rate drift

Integration tests verify the software pipeline is correct; hardware validation requires real equipment — use `ac test hardware`.

## Hardware validation (`ac test hardware`)

Requires two loopback pairs: `output_channel` → `input_channel` (pair A) and a second output → `reference_channel` (pair B). Configure with `ac setup output N input N reference M`. Stimulus is sent to both output ports simultaneously.

| Test | What it measures | Pass criteria |
|------|-----------------|---------------|
| Noise floor | RMS level with silence on both inputs | < -80 dBFS |
| Level linearity | -42 to -6 dBFS in 6 dB steps, check monotonicity | monotonic, step error < 1 dB (1.5 dB top step) |
| THD floor | THD at 1 kHz across levels (-40 to -3 dBFS) | best THD < 0.05% |
| Frequency response | Tone at 50–20kHz, deviation from 1 kHz ref | < 1.0 dB |
| Channel match | Same stimulus on both inputs, compare levels and THD | level delta < 0.5 dB, THD delta < 0.01% |
| Channel isolation | Disconnect ref output, tone on primary, measure ref input | < -60 dBFS (skipped if same output) |
| Repeatability | Same measurement 5x, check variance | level sigma < 0.05 dB, THD sigma < 0.005% |

### DMM cross-check (`ac test hardware dmm`)

Requires `ac setup dmm <ip>` and calibration (`ac calibrate`).

| Test | What it measures | Pass criteria |
|------|-----------------|---------------|
| Absolute level | -10 dBFS vs DMM Vrms vs calibration prediction | < 1% error |
| Level tracking | Sweep -40 to 0 dBFS, DMM vs predicted at each step | < 2% error |
| Freq response | Same level at 100–20kHz, check DMM reads flat | < 1.0 dB deviation |

## DUT characterization (`ac test dut`)

Requires two loopback pairs (same as hardware test). Signal path: `output_channel` → DUT → `input_channel` (measurement), `reference_channel` output → `reference_channel` input (direct loopback reference). Uses `capture_block_stereo()` for simultaneous measurement + reference capture.

| Test | What it measures | Reports |
|------|-----------------|---------|
| Noise floor | DUT output with no stimulus | dBFS |
| Gain | Level difference between measurement and reference at 1 kHz | dB (+ ref/meas levels) |
| THD vs level | THD, THD+N, and gain at 1 kHz across drive levels (-40 to -3 dBFS) | best THD%, per-level breakdown |
| Frequency response | H1 transfer function (pink noise, 4s capture) | deviation range, coherence, delay |
| Clipping point | Level sweep upward until THD > 1% | onset level in dBFS |

### Compare mode (`ac test dut compare`)

Runs the full 5-measurement suite twice: once with DUT in the signal path, then prompts the user to bypass the DUT and runs again. Results are tagged `[With DUT]` and `[Bypass]` for comparison.

### With direct loopback (no DUT)

Expected results: gain ≈ 0 dB, flat frequency response (±0 dB), coherence = 1.000, delay = 0 ms, very low THD. Useful as a baseline sanity check.

## Adding tests

- **Parser tests**: add to `test_parse.py`. No fixtures needed — pure function input/output.
- **Analysis tests**: add to `test_analysis.py`. Use `make_recording()` to build synthetic signals with known properties. Always assert exact numerical values, not just ranges.
- **Integration tests**: add to `test_server_client.py`. Use the session-scoped `server_client` fixture. Must drain to `done`/`error` before returning so the server is idle for the next test.
- **Calibration/conversion tests**: add to respective files. Pure math, no I/O.
